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

const PAGE_TWO: &str = r#"<!doctype html><html><head><title>Page two</title></head><body><h1>Page two</h1></body></html>"#;

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
            let resp = match req.url() {
                "/" => html(PAGE_ONE),
                "/two" => html(PAGE_TWO),
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
