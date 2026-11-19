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
<div style="height:3000px"></div><p id="bottom">Bottom of page</p>
<script>
console.log("hello from page");
fetch("/api/data").then(r => r.json()).then(d => console.log("data", d.n));
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
            let resp = match req.url() {
                "/" => html(PAGE_ONE),
                "/two" => html(PAGE_TWO),
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
        let out = Command::new(crate::host_binary())
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
        let p = work.join("hrafn");
        crate::install_program(&p);
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
    c.check("open", &["open", &url], |o| o.contains("Hrafn test page"));
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

fn tail(s: &str, n: usize) -> String {
    let lines: Vec<&str> = s.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}
