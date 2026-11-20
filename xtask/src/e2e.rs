//! End-to-end test: a throwaway headless Brave with its own temporary profile loads
//! the built extension, starts the real `hrafn` host, and every CLI command is run
//! against a small local test site. The user's own browser and profile are untouched.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const PAGE_ONE: &str = r#"<!doctype html><html lang="en"><head><title>Hrafn test page</title></head><body>
<h1>Hrafn test</h1>
<button id="go" onclick="document.getElementById('out').textContent='clicked'">Press me</button>
<p id="out">waiting</p>
<label for="name">Your name</label> <input id="name" value="old">
<select id="color"><option value="red">Red</option><option value="blue">Blue</option></select>
<form id="f" onsubmit="event.preventDefault(); document.title='submitted:' + document.getElementById('q').value">
  <input id="q" aria-label="Search box"></form>
<a href="/two">Second page</a>
<div id="dbl" ondblclick="this.textContent='double'">double me</div>
<div id="ctx" oncontextmenu="event.preventDefault(); this.textContent='right'">right me</div>
<div id="src" style="width:80px;height:30px;background:#ccc">drag me</div>
<div id="dst" style="width:120px;height:40px;background:#eee;margin-top:20px">drop here</div>
<button id="later-btn" onclick="setTimeout(() => { const p = document.createElement('p'); p.id = 'later'; p.textContent = 'Arrived late'; document.body.appendChild(p); }, 700)">Load later</button>
<div style="height:3000px"></div><p id="bottom">Bottom of page</p>
<script>
console.log("hello from page");
fetch("/api/data").then(r => r.json()).then(d => console.log("data", d.n));
let dragging = false;
document.getElementById("src").addEventListener("mousedown", () => { dragging = true; });
document.getElementById("dst").addEventListener("mouseup", () => { if (dragging) document.getElementById("dst").textContent = "dropped"; });
document.addEventListener("mouseup", () => { setTimeout(() => { dragging = false; }, 0); });
</script></body></html>"#;

/// Mock of ChatGPT's late-2026 page structure, as seen on the real site: messages
/// tagged `data-content-search-unit-key="<conv>:<n>:<role>"` with "… said:" labels,
/// hidden duplicate model/Stop buttons, a visible Stop button only while
/// answering, and no data-testid attributes.
const FAKE_CHATGPT: &str = r#"<!doctype html><html><head><title>ChatGPT</title></head><body>
<nav><a href="/c/conv-one">First chat</a> <a href="/c/conv-two">Second chat</a></nav>
<button aria-label="Select ChatGPT model" aria-haspopup="menu" style="display:none">Select effortTest-4HighLow</button>
<button aria-label="Stop" style="display:none"></button>
<button id="model" aria-label="Select ChatGPT model" aria-haspopup="menu" onclick="document.getElementById('menu').hidden = false">Test-4<br>High</button>
<div id="menu" role="menu" hidden><div role="menuitem" onclick="pick(this)">Test-5 mini</div><div role="menuitem" onclick="pick(this)">Test-5 pro</div></div>
<main id="thread"></main>
<form id="f"><div id="prompt-textarea" contenteditable="true" style="min-height:2em;border:1px solid #888"></div></form>
<script>
let n = 0;
function pick(el) { document.getElementById('model').innerHTML = el.textContent + '<br>High'; document.getElementById('menu').hidden = true; }
function unit(role, label) { const d = document.createElement('div'); d.setAttribute('data-content-search-unit-key', 'conv-x:' + (n++) + ':' + role);
  const h = document.createElement('h5'); h.textContent = label; d.appendChild(h); const body = document.createElement('div'); d.appendChild(body);
  document.getElementById('thread').appendChild(d); return body; }
document.getElementById('prompt-textarea').addEventListener('keydown', (e) => {
  if (e.key !== 'Enter') return;
  e.preventDefault();
  const box = document.getElementById('prompt-textarea');
  const prompt = box.textContent.trim(); box.textContent = '';
  unit('user', 'You said:').textContent = prompt;
  const a = unit('assistant', 'ChatGPT said:');
  const stop = document.createElement('button'); stop.setAttribute('aria-label', 'Stop'); stop.textContent = '■'; document.body.appendChild(stop);
  const words = ('You said: ' + prompt + '. Ravens fly at dawn.').split(' ');
  let i = 0;
  const t = setInterval(() => { a.textContent += (i ? ' ' : '') + words[i++]; if (i >= words.length) { clearInterval(t); stop.remove(); } }, 80);
  stop.onclick = () => { clearInterval(t); stop.remove(); };
});
</script></body></html>"#;

const FAKE_LINKEDIN_JOB: &str = r#"<!doctype html><html><head><title>Warehouse Lead | Example Freight | LinkedIn</title></head><body>
<h1 class="job-details-jobs-unified-top-card__job-title">Warehouse Lead</h1>
<div class="job-details-jobs-unified-top-card__company-name"><a>Example Freight</a></div>
<div class="job-details-jobs-unified-top-card__primary-description-container"><span class="tvm__text">Springfield, WA</span> · 3 days ago · 25 applicants</div>
<button class="job-details-fit-level-preferences"><span>Full-time</span></button>
<div class="jobs-description__content"><p>Lead a small warehouse team.</p><p>Forklift experience helps.</p></div>
</body></html>"#;

/// Late-2026 LinkedIn job search page: scrambled class names, facts joined with
/// " · ", an upsell line, and a long paragraph mentioning "Applicants".
const FAKE_LINKEDIN_JOB_2026: &str = r#"<!doctype html><html><head><title>Parcel Courier | Example Parcel Co | LinkedIn</title></head><body>
<h1 class="x9f2">0 notifications</h1>
<ul class="a81kq"><li class="b2xx"><span>Selected, Parcel Courier</span><span class="zz1">Parcel Courier</span><div>Example Parcel Co</div><div>Springfield, WA</div></li></ul>
<div class="p0q"><div class="r7t"><div><span class="k3j">Parcel Courier</span></div><div>Example Parcel Co</div>
<div>Springfield, WA · 2 weeks ago · 19 people clicked apply</div><div>Full-time</div><div>Pay: $25.42/hr-$35.96/hr</div><div>Try Premium for $0</div></div>
<section><h2>About the job</h2><p>Deliver parcels on a set route.</p><p>Applicants who need accommodations can ask at any time during the process, which is described in detail here for people who want it.</p></section>
<section><h2>About the company</h2><p>Example Parcel Co moves boxes.</p></section></div>
</body></html>"#;

const FAKE_LINKEDIN_EVENT: &str =r#"<!doctype html><html><head><title>Raven Meetup | LinkedIn</title>
<script type="application/ld+json">{"@context":"https://schema.org","@type":"Event","name":"Raven Meetup","startDate":"2026-12-01T18:00","organizer":{"@type":"Organization","name":"Example Birders"},"location":{"@type":"VirtualLocation","url":"https://example.com/live"},"description":"An evening about corvids."}</script></head><body>
<h1>Raven Meetup</h1><a id="att" href="javascript:void 0">12 attendees</a>
<div id="dlg" role="dialog" hidden><div class="artdeco-modal__content" style="height:120px;overflow:auto"><ul id="people"></ul></div></div>
<script>
let n = 0;
function more() { const ul = document.getElementById('people'); for (let k = 0; k < 4 && n < 12; k++, n++) {
  const li = document.createElement('li'); li.innerHTML = '<a href="https://www.linkedin.com/in/person-' + n + '?mini=1">Person ' + n + '</a><div>Bird watcher ' + n + '</div><button>Connect</button>'; li.style.height = '40px'; ul.appendChild(li); } }
document.getElementById('att').onclick = () => { document.getElementById('dlg').hidden = false; more(); };
document.querySelector('.artdeco-modal__content').addEventListener('scroll', (e) => { const s = e.target; if (s.scrollTop + s.clientHeight >= s.scrollHeight - 5) more(); });
</script></body></html>"#;

const FAKE_LINKEDIN_POST: &str = r#"<!doctype html><html><head><title>Post | LinkedIn</title></head><body>
<div class="feed-shared-update-v2"><div class="update-components-actor__title"><span aria-hidden="true">Example Person</span></div>
<div class="update-components-actor__description"><span aria-hidden="true">Ornithologist</span></div>
<div class="update-components-text">Ravens can count to seven.</div>
<span class="social-details-social-counts__reactions-count">42</span><button class="social-details-social-counts__comments">5 comments</button></div>
</body></html>"#;

const FAKE_GOOGLE_DOC: &str = r#"<!doctype html><html><head><title>Test Doc - Google Docs</title></head><body>
<div class="kix-appview-editor" contenteditable="true" style="min-height:4em;border:1px solid #888">Existing text.</div>
</body></html>"#;

const FAKE_GOOGLE_SHEET: &str = r#"<!doctype html><html><head><title>Test Sheet - Google Sheets</title></head><body><div>grid</div></body></html>"#;

const FAKE_GOOGLE_SLIDES: &str = r##"<!doctype html><html><head><title>Test Deck - Google Slides</title></head><body>
<svg width="200" height="260">
  <g onclick="go(1)"><rect class="punch-filmstrip-thumbnail" x="0" y="0" width="200" height="120" fill="#ddd"></rect><text x="10" y="60">Opening slide</text></g>
  <g onclick="go(2)"><rect class="punch-filmstrip-thumbnail" x="0" y="130" width="200" height="120" fill="#ddd"></rect><text x="10" y="190">Second slide</text></g>
</svg>
<div id="speakernotes">Notes for slide 1</div>
<script>function go(n) { document.getElementById('speakernotes').textContent = 'Notes for slide ' + n; }</script>
</body></html>"##;

const PAGE_TWO: &str =r#"<!doctype html><html><head><title>Page two</title></head><body><h1>Page two</h1></body></html>"#;

fn serve() -> String {
    let server = tiny_http::Server::http("127.0.0.1:0").expect("test server");
    let addr = format!("http://{}", server.server_addr().to_ip().unwrap());
    std::thread::spawn(move || {
        for req in server.incoming_requests() {
            let html = |b: &str| {
                tiny_http::Response::from_string(b)
                    .with_header("Content-Type: text/html; charset=utf-8".parse::<tiny_http::Header>().unwrap())
            };
            let echo = req
                .headers()
                .iter()
                .find(|h| h.field.equiv("X-Hrafn-Test"))
                .map(|h| h.value.as_str().to_string())
                .unwrap_or_default();
            let full = req.url().to_string();
            let (path, query) = full.split_once('?').unwrap_or((full.as_str(), ""));
            let text = |b: &str, t: &str| {
                tiny_http::Response::from_string(b).with_header(format!("Content-Type: {t}").parse::<tiny_http::Header>().unwrap())
            };
            let resp = match path {
                "/" => html(PAGE_ONE),
                "/two" => html(PAGE_TWO),
                p if p == "/chatgpt" || p.starts_with("/c/") => html(FAKE_CHATGPT),
                "/li/job" => html(FAKE_LINKEDIN_JOB),
                "/li/job2026" => html(FAKE_LINKEDIN_JOB_2026),
                "/li/event" => html(FAKE_LINKEDIN_EVENT),
                "/li/post" => html(FAKE_LINKEDIN_POST),
                "/document/d/TESTDOC/edit" => html(FAKE_GOOGLE_DOC),
                "/spreadsheets/d/TESTSHEET/edit" => html(FAKE_GOOGLE_SHEET),
                "/presentation/d/TESTDECK/edit" => html(FAKE_GOOGLE_SLIDES),
                "/document/d/TESTDOC/export" if query == "format=txt" => text("\u{feff}Hello from the test doc\nSecond line\n", "text/plain"),
                "/document/d/TESTDOC/export" if query == "format=pdf" => text("%PDF-1.4 test file", "application/pdf"),
                "/spreadsheets/d/TESTSHEET/export" if query == "format=csv&gid=5" => text("a,b\n1,2\n", "text/csv"),
                "/presentation/d/TESTDECK/export/txt" => text("Opening slide\nSecond slide\n", "text/plain"),
                "/api/echo" => tiny_http::Response::from_string(format!(r#"{{"header":"{echo}"}}"#))
                    .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap()),
                "/events" => tiny_http::Response::from_string("event: tick\ndata: one\n\ndata: two\n\n")
                    .with_header("Content-Type: text/event-stream".parse::<tiny_http::Header>().unwrap())
                    .with_header("Cache-Control: no-cache".parse::<tiny_http::Header>().unwrap()),
                "/api/data" => tiny_http::Response::from_string(r#"{"n":42}"#)
                    .with_header("Content-Type: application/json".parse::<tiny_http::Header>().unwrap())
                    .with_header("Set-Cookie: hrafn_test=yes; Path=/".parse::<tiny_http::Header>().unwrap()),
                _ => html("not found").with_status_code(404),
            };
            let _ = req.respond(resp);
        }
    });
    addr
}

struct Ctx {
    sock: PathBuf,
    failures: Vec<String>,
}

impl Ctx {
    fn h(&self, args: &[&str]) -> (bool, String, String) {
        let out = Command::new(crate::cli_binary())
            .args(args)
            .env("HRAFN_SOCKET", &self.sock)
            .output()
            .expect("run hrafn");
        (out.status.success(), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    /// Run a command and check its output with `ok`.
    fn check(&mut self, label: &str, args: &[&str], ok: impl Fn(&str) -> bool) -> String {
        let (success, out, err) = self.h(args);
        if success && ok(&out) {
            eprintln!("PASS {label}");
        } else {
            eprintln!("FAIL {label}\n     hrafn {}\n     stdout: {}\n     stderr: {}", args.join(" "), out.trim(), err.trim());
            self.failures.push(label.to_string());
        }
        out
    }
}

fn launch(brave: &str, profile: &Path, ext: &Path, sock: &Path) -> Child {
    Command::new(brave)
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg(format!("--load-extension={}", ext.display()))
        .arg(format!("--disable-extensions-except={}", ext.display()))
        .args([
            "--headless=new",
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-features=DisableLoadExtensionCommandLineSwitch",
            "--window-size=1280,900",
            "about:blank",
        ])
        .env("HRAFN_SOCKET", sock)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start Brave (set BRAVE_BIN if it is not on PATH)")
}

/// Brave reads native host files from its standard folder even when started with
/// `--user-data-dir`, so the test registers its host there and this guard puts
/// back whatever was there before (also when the test panics).
struct HostRegistration {
    file: PathBuf,
    previous: Option<Vec<u8>>,
}

impl HostRegistration {
    fn install(home: &Path, program: &Path) -> Self {
        let dir = home.join(".config/BraveSoftware/Brave-Browser/NativeMessagingHosts");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("com.hrafn.host.json");
        let previous = std::fs::read(&file).ok();
        std::fs::write(&file, crate::host_manifest(program)).unwrap();
        Self { file, previous }
    }
}

impl Drop for HostRegistration {
    fn drop(&mut self) {
        match &self.previous {
            Some(bytes) => {
                let _ = std::fs::write(&self.file, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&self.file);
            }
        }
    }
}

pub fn run(unsandboxed: bool) {
    crate::build();
    // Sandboxed mode runs Brave through its firejail wrapper, exactly like the user's
    // browser, so the host must be the installed system copy. Unsandboxed mode runs
    // the Brave binary directly with a host copy in the test folder.
    let (brave, program) = if unsandboxed {
        let b = std::env::var("BRAVE_BIN").unwrap_or_else(|_| "/opt/brave-bin/brave".into());
        (b, None)
    } else {
        if !crate::system_host_current() {
            eprintln!(
                "The sandboxed test needs the current host installed at {}.\nRun:\n\n    {}\n\nor use `cargo xtask e2e --unsandboxed`.",
                crate::SYSTEM_HOST,
                crate::sudo_install_command()
            );
            std::process::exit(2);
        }
        (std::env::var("BRAVE_BIN").unwrap_or_else(|_| "brave".into()), Some(PathBuf::from(crate::SYSTEM_HOST)))
    };
    eprintln!("browser: {brave} ({})", if unsandboxed { "unsandboxed" } else { "sandboxed like your Brave" });
    // Profile, extension copy and socket live under Brave's config folder, the place
    // a sandboxed Brave can see and write. Removed afterwards.
    let home = PathBuf::from(std::env::var("HOME").unwrap());
    let work = home.join(".config/BraveSoftware/hrafn-e2e");
    let _ = std::fs::remove_dir_all(&work);
    let profile = work.join("profile");
    std::fs::create_dir_all(&profile).unwrap();
    let program = program.unwrap_or_else(|| {
        let p = work.join("hrafn-host");
        crate::install_program(&crate::host_binary(), &p);
        p
    });
    let _registration = HostRegistration::install(&home, &program);
    let ext = work.join("ext");
    std::fs::create_dir_all(&ext).unwrap();
    for entry in std::fs::read_dir(crate::dist()).unwrap() {
        let p = entry.unwrap().path();
        std::fs::copy(&p, ext.join(p.file_name().unwrap())).unwrap();
    }
    let sock = work.join("hrafn.sock");
    let site = serve();

    let mut brave = launch(&brave, &profile, &ext, &sock);
    let start = Instant::now();
    while !sock.exists() && start.elapsed() < Duration::from_secs(30) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let mut c = Ctx { sock: sock.clone(), failures: Vec::new() };
    if !sock.exists() {
        let _ = brave.kill();
        let mut err = String::new();
        if let Some(mut e) = brave.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut err);
        }
        let log = std::fs::read_to_string(sock.with_extension("log")).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&work);
        panic!("the browser never started the host.\nhost log:\n{log}\nbrowser stderr (tail):\n{}", tail(&err, 30));
    }
    eprintln!("host connected after {:?}", start.elapsed());

    c.check("status", &["status"], |o| o.contains("\"extension\"") && o.contains("Chrome/"));
    let url = format!("{site}/");
    let opened = c.check("open", &["open", &url], |o| o.contains("Hrafn test page"));
    let test_tab = serde_json::from_str::<serde_json::Value>(&opened).ok().and_then(|v| v["id"].as_i64()).unwrap_or(0).to_string();
    c.check("tabs lists the current tab", &["tabs"], |o| o.lines().any(|l| l.starts_with('*') && l.contains("Hrafn test page")));
    c.check("network log covers the first load", &["net", "log"], |o| o.contains("/api/data") && o.contains(&format!("{site}/")));
    c.check("reload", &["reload"], |o| o.contains("Hrafn test page"));
    c.check("wait-stable", &["wait-stable"], |o| o.contains("stable"));
    let tree = c.check("read tree", &["read", "--tree-only"], |o| o.contains("heading \"Hrafn test\"") && o.contains("button \"Press me\""));
    let button_ref = tree
        .lines()
        .find(|l| l.contains("button \"Press me\""))
        .and_then(|l| l.rsplit('[').next())
        .map(|r| r.trim_end_matches(']').to_string())
        .unwrap_or_default();
    c.check("read text", &["text"], |o| o.contains("Bottom of page") && o.contains("waiting"));
    c.check("find", &["find", "press", "--role", "button"], |o| o.contains("button \"Press me\""));
    c.check("click by ref", &["click", &button_ref], |o| o.contains("clicked"));
    c.check("click changed the page", &["eval", "document.getElementById('out').textContent"], |o| o.trim() == "clicked");
    c.check("type replaces", &["type", "#name", "Test User"], |o| o.contains("typed 9"));
    c.check("typed value", &["eval", "document.getElementById('name').value"], |o| o.trim() == "Test User");
    c.check("type --append", &["type", "#name", "!", "--append"], |_| true);
    c.check("appended value", &["eval", "document.getElementById('name').value"], |o| o.trim() == "Test User!");
    c.check("select", &["select", "#color", "blue"], |o| o.contains("blue"));
    c.check("type into form", &["type", "#q", "ravens"], |_| true);
    c.check("keys Enter submits", &["keys", "Enter"], |_| true);
    c.check("form submitted", &["eval", "document.title"], |o| o.trim() == "submitted:ravens");
    c.check("keys Control+A", &["keys", "Control+a"], |_| true);
    c.check("scroll bottom", &["scroll", "bottom"], |o| !o.contains("y=0)"));
    c.check("scroll up (wheel)", &["scroll", "up"], |o| o.contains("scrolled up"));
    c.check("scroll top", &["scroll", "top"], |o| o.contains("y=0)"));
    c.check("html", &["html", "#out"], |o| o.contains("<p id=\"out\">clicked</p>"));
    c.check("hover", &["hover", "a"], |o| o.contains("hovering"));
    c.check("console log", &["console"], |o| o.contains("hello from page") && o.contains("data 42"));
    c.check("network log", &["net", "log", "--filter", "api"], |o| o.contains("/api/data") && o.contains("200"));
    c.check("cookies", &["cookies", "127.0.0.1"], |o| o.contains("hrafn_test"));

    // ---- extras
    c.check("double-click", &["dblclick", "#dbl"], |o| o.contains("double-clicked"));
    c.check("double-click worked", &["eval", "document.getElementById('dbl').textContent"], |o| o.trim() == "double");
    c.check("right-click", &["rightclick", "#ctx"], |o| o.contains("right-clicked"));
    c.check("right-click worked", &["eval", "document.getElementById('ctx').textContent"], |o| o.trim() == "right");
    c.check("drag", &["drag", "#src", "#dst"], |o| o.contains("dragged"));
    c.check("drag worked", &["eval", "document.getElementById('dst').textContent"], |o| o.trim() == "dropped");
    c.check("click --at coordinates", &["eval", "(() => { const r = document.getElementById('go').getBoundingClientRect(); document.getElementById('out').textContent = 'reset'; return Math.round(r.x + r.width / 2) + ',' + Math.round(r.y + r.height / 2); })()"], |_| true);
    let (_, at, _) = c.h(&["eval", "(() => { const r = document.getElementById('go').getBoundingClientRect(); return Math.round(r.x + r.width / 2) + ',' + Math.round(r.y + r.height / 2); })()"]);
    c.check("click at X,Y", &["click", "--at", at.trim()], |o| o.contains("clicked"));
    c.check("click at X,Y worked", &["eval", "document.getElementById('out').textContent"], |o| o.trim() == "clicked");
    c.check("diff baseline", &["diff"], |o| o.contains("first read"));
    c.check("start a late element", &["click", "#later-btn"], |_| true);
    c.check("wait-for selector", &["wait-for", "#later", "--timeout", "5000"], |o| o.contains("is there"));
    c.check("wait-for text", &["wait-for", "--text", "Arrived late"], |o| o.contains("is there"));
    c.check("diff shows the new element", &["diff"], |o| o.contains("+ ") && o.contains("Arrived late"));
    c.check("diff with no change", &["diff"], |o| o.trim() == "no changes");
    let (ok, _, err) = c.h(&["wait-for", "#never", "--timeout", "600"]);
    if !ok && err.contains("timed out") {
        eprintln!("PASS wait-for times out clearly");
    } else {
        eprintln!("FAIL wait-for timeout: {err}");
        c.failures.push("wait-for timeout".into());
    }
    let style = c.check("style add", &["style", "add", "#out { color: rgb(255, 0, 0) !important }"], |o| o.contains("added style s"));
    let style_id = style.split_whitespace().nth(2).unwrap_or("s1").to_string();
    c.check("style applied", &["eval", "getComputedStyle(document.getElementById('out')).color"], |o| o.trim() == "rgb(255, 0, 0)");
    c.check("style remove", &["style", "remove", &style_id], |o| o.contains("removed"));
    c.check("style gone", &["eval", "getComputedStyle(document.getElementById('out')).color"], |o| o.trim() != "rgb(255, 0, 0)");
    c.check("headers add", &["headers", "add", "X-Hrafn-Test", "raven"], |o| o.contains("X-Hrafn-Test"));
    c.check("header is sent", &["eval", "fetch('/api/echo').then(r => r.json()).then(j => j.header)"], |o| o.trim() == "raven");
    c.check("headers clear", &["headers", "clear"], |_| true);
    c.check("header no longer sent", &["eval", "fetch('/api/echo').then(r => r.json()).then(j => j.header)"], |o| o.trim().is_empty());
    c.check("override add", &["override", "add", "*/api/data", "--status", "201", "--body", r#"{"n":7}"#], |o| o.contains("fake response"));
    c.check("override answers", &["eval", "fetch('/api/data').then(async r => r.status + ' ' + await r.text())"], |o| o.trim() == r#"201 {"n":7}"#);
    c.check("override leaves other URLs alone", &["eval", "fetch('/api/echo').then(r => r.status)"], |o| o.trim() == "200");
    c.check("override list", &["override"], |o| o.contains("*/api/data") && o.contains("201"));
    c.check("override clear", &["override", "clear"], |_| true);
    c.check("real response again", &["eval", "fetch('/api/data').then(r => r.text())"], |o| o.trim() == r#"{"n":42}"#);
    let net = c.check("net log has ids", &["net", "log", "--filter", "/api/data", "--limit", "1"], |o| o.contains("/api/data"));
    let req_id = net.split_whitespace().next().unwrap_or("").to_string();
    c.check("net body", &["net", "body", &req_id], |o| o.contains(r#""n":42"#));
    c.check("open an event stream", &["eval", "window.__es = new EventSource('/events'); 'ok'"], |_| true);
    c.check("wait for stream", &["wait", "1500"], |_| true);
    c.check("sse log", &["sse"], |o| o.contains("[tick] one") && o.contains("[message] two"));
    c.check("close the stream", &["eval", "window.__es.close(); 'closed'"], |_| true);

    // ---- record and replay
    c.check("reload before recording", &["reload"], |_| true);
    c.check("record start", &["record", "start"], |o| o.contains("recording tab"));
    c.check("rec: type", &["type", "#q", "ravens"], |_| true);
    c.check("rec: Enter", &["keys", "Enter"], |_| true);
    c.check("rec: select", &["select", "#color", "blue"], |_| true);
    c.check("rec: click", &["click", "#go"], |_| true);
    c.check("record status", &["record", "status"], |o| o.contains("step(s)"));
    let plan = work.join("plan.txt");
    let plan_s = plan.display().to_string();
    c.check("record stop", &["record", "stop", &plan_s], |o| o.contains("saved"));
    let plan_text = std::fs::read_to_string(&plan).unwrap_or_default();
    let steps: Vec<&str> = plan_text.lines().filter(|l| !l.starts_with('#')).collect();
    let want = [
        format!("open {site}/"),
        "type #q ravens".to_string(),
        "keys Enter".to_string(),
        "select #color blue".to_string(),
        "click #go".to_string(),
    ];
    if steps == want.iter().map(String::as_str).collect::<Vec<_>>() {
        eprintln!("PASS recorded plan is exact");
    } else {
        eprintln!("FAIL recorded plan:\n{plan_text}");
        c.failures.push("recorded plan".into());
    }
    let tabs_before_replay = tab_ids(&c);
    c.check("replay plan", &["run", &plan_s], |o| o.contains("5 passed, 0 failed"));
    c.check("replay reached the same end state", &["eval", "document.title + ' / ' + document.getElementById('out').textContent + ' / ' + document.getElementById('color').value"], |o| o.trim() == "submitted:ravens / clicked / blue");
    std::fs::write(&plan, "open about:blank\nclick \"#does-not-exist\"\nkeys Enter\n").unwrap();
    let (ok, out, _) = c.h(&["run", &plan_s]);
    if !ok && out.contains("FAIL line 2") && out.contains("1 passed, 1 failed") {
        eprintln!("PASS replay stops at the first failing step");
    } else {
        eprintln!("FAIL replay failure handling:\n{out}");
        c.failures.push("replay failure".into());
    }
    for id in tab_ids(&c).into_iter().filter(|id| !tabs_before_replay.contains(id)) {
        c.check("close a replay tab", &["tab", "close", &id], |o| o.contains("closed tab"));
    }
    c.check("switch back to the test tab", &["tab", "switch", &test_tab], |o| o.contains("submitted:ravens"));
    let shot = work.join("shot.png");
    let shot_s = shot.display().to_string();
    c.check("screenshot", &["screenshot", &shot_s], |_| true);
    let png = std::fs::read(&shot).unwrap_or_default();
    if png.starts_with(b"\x89PNG") && png.len() > 1000 {
        eprintln!("PASS screenshot is a PNG ({} bytes)", png.len());
    } else {
        eprintln!("FAIL screenshot file is not a PNG");
        c.failures.push("screenshot png".into());
    }
    let full = work.join("full.png");
    let full_s = full.display().to_string();
    c.check("full-page screenshot", &["screenshot", &full_s, "--full"], |_| true);
    let full_len = std::fs::metadata(&full).map(|m| m.len()).unwrap_or(0);
    if full_len > png.len() as u64 {
        eprintln!("PASS full-page screenshot is larger");
    } else {
        eprintln!("FAIL full-page screenshot is not larger than the viewport one");
        c.failures.push("full screenshot".into());
    }
    // ---- OCR (pure-Rust engine; models are downloaded once if missing)
    c.check("scroll to top for OCR", &["scroll", "top"], |_| true);
    c.check("ocr page", &["ocr"], |o| o.contains("Hrafn test") && o.contains("Press me"));
    c.check("ocr --find", &["ocr", "--find", "Press me"], |o| o.split_whitespace().next().is_some_and(|xy| xy.contains(',')));
    c.check("reset output", &["eval", "document.getElementById('out').textContent = 'waiting'"], |_| true);
    c.check("ocr --click", &["ocr", "--click", "Press me"], |o| o.contains("clicked \"Press me\""));
    c.check("ocr --click hit the button", &["eval", "document.getElementById('out').textContent"], |o| o.trim() == "clicked");
    c.check("ocr one element", &["ocr", "--target", "h1"], |o| o.trim() == "Hrafn test");
    c.check("scroll down for OCR", &["scroll", "bottom"], |_| true);
    c.check("ocr element after scrolling", &["ocr", "--target", "#bottom"], |o| o.trim() == "Bottom of page");
    c.check("ocr image file", &["ocr", &shot_s], |o| o.contains("Hrafn test"));
    let (ok, _, err) = c.h(&["ocr", "--find", "no such words here"]);
    if !ok && err.contains("is not visible") {
        eprintln!("PASS ocr --find reports missing text");
    } else {
        eprintln!("FAIL ocr --find missing text: {err}");
        c.failures.push("ocr find missing".into());
    }

    c.check("navigate", &["navigate", &format!("{site}/two")], |o| o.contains("Page two"));
    let (ok, _, err) = c.h(&["click", &button_ref]);
    if !ok && err.contains("unknown ref") {
        eprintln!("PASS refs reset after navigation");
    } else {
        eprintln!("FAIL refs should reset after navigation: {err}");
        c.failures.push("refs reset".into());
    }
    // The title was changed by the form test, so check the address.
    c.check("back", &["back"], |o| o.contains(&format!("\"url\": \"{site}/\"")));
    let (ok, _, err) = c.h(&["eval", "throw new Error('boom')"]);
    if !ok && err.contains("boom") {
        eprintln!("PASS eval errors fail with the message");
    } else {
        eprintln!("FAIL eval errors");
        c.failures.push("eval error".into());
    }
    let (ok, _, err) = c.h(&["click", "#does-not-exist"]);
    if !ok && err.contains("nothing matches") {
        eprintln!("PASS missing element is a clear error");
    } else {
        eprintln!("FAIL missing element error: {err}");
        c.failures.push("missing element".into());
    }
    c.check("tab new", &["tab", "new", &format!("{site}/two")], |o| o.contains("Page two"));
    c.check("tab close", &["tab", "close"], |o| o.contains("closed tab"));
    c.check("detach", &["detach"], |o| o.contains("stopped debugging"));

    // ---- site adapters, against local mock-ups of each site's page structure
    c.check("open mock ChatGPT", &["navigate", &format!("{site}/chatgpt")], |o| o.contains("ChatGPT"));
    c.check("chatgpt status", &["chatgpt", "status"], |o| o.contains("signed in: yes") && o.contains("model: Test-4"));
    c.check("chatgpt send", &["chatgpt", "send", "Hello", "raven"], |o| o.trim() == "You said: Hello raven. Ravens fly at dawn.");
    c.check("chatgpt send --stream", &["chatgpt", "send", "--stream", "Second", "question"], |o| o.trim() == "You said: Second question. Ravens fly at dawn.");
    c.check("chatgpt read", &["chatgpt", "read"], |o| o.trim() == "You said: Second question. Ravens fly at dawn.");
    c.check("chatgpt read --all", &["chatgpt", "read", "--all"], |o| o.matches("--- user").count() == 2 && o.matches("--- assistant").count() == 2);
    c.check("chatgpt conversations", &["chatgpt", "conversations"], |o| o.contains("conv-one  First chat") && o.contains("conv-two  Second chat"));
    c.check("chatgpt model switch", &["chatgpt", "model", "pro"], |o| o.contains("model: Test-5 pro"));
    let (ok, _, err) = c.h(&["chatgpt", "model", "turbo-ultra"]);
    if !ok && err.contains("Test-5 mini") {
        eprintln!("PASS chatgpt model lists choices when there is no match");
    } else {
        eprintln!("FAIL chatgpt model no-match: {err}");
        c.failures.push("chatgpt model no match".into());
    }
    c.check("chatgpt switch", &["chatgpt", "switch", "conv-two"], |o| o.contains("conv-two"));
    c.check("chatgpt switch landed", &["eval", "location.pathname"], |o| o.trim() == "/c/conv-two");

    c.check("linkedin job", &["linkedin", "job", &format!("{site}/li/job")], |o| {
        o.contains("title: Warehouse Lead") && o.contains("company: Example Freight") && o.contains("Springfield, WA") && o.contains("Lead a small warehouse team")
    });
    c.check("linkedin job --json", &["--json", "linkedin", "job"], |o| o.contains("\"title\": \"Warehouse Lead\""));
    c.check("linkedin job (2026 scrambled markup)", &["--json", "linkedin", "job", &format!("{site}/li/job2026")], |o| {
        let j: serde_json::Value = serde_json::from_str(o).unwrap_or_default();
        j["title"] == "Parcel Courier"
            && j["company"] == "Example Parcel Co"
            && j["location"] == "Springfield, WA"
            && j["summary"] == "Springfield, WA · 2 weeks ago · 19 people clicked apply"
            && j["details"] == serde_json::json!(["Full-time", "Pay: $25.42/hr-$35.96/hr"])
            && j["description"].as_str().is_some_and(|d| d.starts_with("Deliver parcels on a set route.") && !d.contains("Example Parcel Co moves"))
    });
    c.check("linkedin event", &["linkedin", "event", &format!("{site}/li/event")], |o| {
        o.contains("title: Raven Meetup") && o.contains("organizer: Example Birders") && o.contains("attendees: 12 attendees") && o.contains("2026-12-01")
    });
    c.check("linkedin attendees --max", &["linkedin", "attendees", &format!("{site}/li/event"), "--max", "10"], |o| {
        o.contains("(10 people)") && o.contains("Person 0 — Bird watcher 0  https://www.linkedin.com/in/person-0")
    });
    c.check("linkedin attendees (loads more by scrolling)", &["linkedin", "attendees", &format!("{site}/li/event")], |o| o.contains("(12 people)") && o.contains("Person 11"));
    c.check("linkedin post", &["linkedin", "post", &format!("{site}/li/post")], |o| {
        o.contains("author: Example Person") && o.contains("Ravens can count to seven") && o.contains("reactions: 42")
    });

    c.check("open mock Google Doc", &["navigate", &format!("{site}/document/d/TESTDOC/edit")], |o| o.contains("Test Doc"));
    c.check("gdocs info", &["gdocs", "info"], |o| o.contains("document: Test Doc") && o.contains("id: TESTDOC"));
    c.check("gdocs text", &["gdocs", "text"], |o| o.starts_with("Hello from the test doc\nSecond line"));
    let pdf = work.join("doc.pdf");
    let pdf_s = pdf.display().to_string();
    c.check("gdocs export pdf", &["gdocs", "export", "pdf", &pdf_s], |o| o.contains("bytes"));
    if std::fs::read(&pdf).unwrap_or_default().starts_with(b"%PDF") {
        eprintln!("PASS exported file is the PDF");
    } else {
        eprintln!("FAIL exported PDF content");
        c.failures.push("gdocs pdf".into());
    }
    let (ok, _, err) = c.h(&["gdocs", "export", "exe", &pdf_s]);
    if !ok && err.contains("docx") {
        eprintln!("PASS gdocs export lists formats for a bad one");
    } else {
        eprintln!("FAIL gdocs bad format: {err}");
        c.failures.push("gdocs bad format".into());
    }
    c.check("gdocs insert --end", &["gdocs", "insert", " Added by hrafn.", "--end"], |o| o.contains("inserted"));
    c.check("insert landed at the end", &["eval", "document.querySelector('.kix-appview-editor').textContent"], |o| o.trim() == "Existing text. Added by hrafn.");
    c.check("open mock Sheet", &["navigate", &format!("{site}/spreadsheets/d/TESTSHEET/edit#gid=5")], |_| true);
    c.check("gdocs text (sheet as CSV)", &["gdocs", "text"], |o| o.trim() == "a,b\n1,2");
    c.check("open mock Slides", &["navigate", &format!("{site}/presentation/d/TESTDECK/edit")], |_| true);
    c.check("gdocs slides", &["gdocs", "slides"], |o| o.contains("1  Opening slide") && o.contains("2  Second slide"));
    c.check("gdocs notes", &["gdocs", "notes"], |o| o.trim() == "Notes for slide 1");
    c.check("gdocs slide 2", &["gdocs", "slide", "2"], |o| o.contains("on slide 2"));
    c.check("gdocs notes follow the slide", &["gdocs", "notes"], |o| o.trim() == "Notes for slide 2");
    c.check("gdocs text (slides)", &["gdocs", "text"], |o| o.contains("Opening slide") && o.contains("Second slide"));
    c.check("leave the Google pages", &["navigate", &format!("{site}/two")], |_| true);
    let (_, tabs_before, _) = c.h(&["tabs"]);
    let (ok, _, err) = c.h(&["gdocs", "info"]);
    let (_, tabs_after, _) = c.h(&["tabs"]);
    if !ok && err.contains("no docs.google.com page is open") && tabs_before.lines().count() == tabs_after.lines().count() {
        eprintln!("PASS gdocs without a Google file says so and opens nothing");
    } else {
        eprintln!("FAIL gdocs without a file: {err}");
        c.failures.push("gdocs no page".into());
    }
    let (_, tabs_before, _) = c.h(&["tabs"]);
    let (ok, _, err) = c.h(&["linkedin", "job"]);
    let (_, tabs_after, _) = c.h(&["tabs"]);
    if !ok && err.contains("no linkedin.com page is open") && tabs_before.lines().count() == tabs_after.lines().count() {
        eprintln!("PASS linkedin without a LinkedIn page says so and opens nothing");
    } else {
        eprintln!("FAIL linkedin without a page: {err}");
        c.failures.push("linkedin no page".into());
    }

    let _ = brave.kill();
    let _ = brave.wait();
    // Give the host a moment to notice the browser is gone and remove its socket.
    std::thread::sleep(Duration::from_millis(500));
    let (ok, _, err) = c.h(&["status"]);
    if !ok && err.contains("isn't connected") {
        eprintln!("PASS clear message when the browser is gone");
    } else {
        eprintln!("FAIL message when the browser is gone: {err}");
        c.failures.push("disconnected message".into());
    }
    let _ = std::fs::remove_dir_all(&work);
    // process::exit skips destructors, so restore the registration first.
    drop(_registration);

    if c.failures.is_empty() {
        eprintln!("\nALL E2E CHECKS PASSED");
    } else {
        eprintln!("\n{} FAILED: {}", c.failures.len(), c.failures.join(", "));
        std::process::exit(1);
    }
}

/// Tab ids from `hrafn tabs` (lines look like "* 1234567  Title  URL").
fn tab_ids(c: &Ctx) -> Vec<String> {
    let (_, out, _) = c.h(&["tabs"]);
    out.lines()
        .filter_map(|l| l.trim_start_matches('*').split_whitespace().next().map(String::from))
        .filter(|w| w.chars().all(|ch| ch.is_ascii_digit()))
        .collect()
}

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
