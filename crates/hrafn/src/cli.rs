//! The `hrafn` command line client.

use crate::paths;
use base64::Engine;
use hrafn_proto::{HOST_NAME, Request, Response};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

const HELP: &str = "\
hrafn: drive your real Brave (or Chrome) from the terminal

USAGE: hrafn [--tab ID] [--json] <command> [args]

Tabs and pages
  status                       Is the browser connected? (extension + browser version)
  tabs                         List open tabs (* = current target)
  open <url>                   Open a new tab, wait for it to load, make it current
  navigate <url>               Load a URL in the current tab
  back | forward | reload
  tab new [url] | tab switch <id> | tab close [id]

Reading
  read [ref] [--tree-only|--text-only] [--max-chars N]
                               Accessibility tree with refs (e1, e2...) plus page text
  tree | text                  Just the tree / just the visible text
  find <words> [--role ROLE]   Elements whose name contains the words
  html <target>                Outer HTML of an element

Acting (target = a ref like e12, or a CSS selector)
  click <target> | hover <target> | focus <target>
  type <target> <text> [--append]
  select <target> <value>
  keys <combo>                 e.g. Enter, Tab, Escape, Control+A, Shift+ArrowDown
  scroll <up|down|top|bottom>

Waiting and capture
  wait <ms>                    Pause
  wait-stable [--ms N] [--timeout N]
                               Wait until the page is loaded and the network is quiet
  screenshot [file.png] [--full]
  eval <javascript>            Run JS in the page and print the result

Logs and cookies
  console [--limit N] [--clear]
  net log [--filter TEXT] [--limit N] | net clear
  cookies <domain> | cookies set '<json>' | cookies delete <url> <name>

Other
  detach                       Stop debugging the current tab (removes Brave's banner)
  doctor                       Check the install and the browser connection
  help
";

fn fail(msg: impl AsRef<str>) -> i32 {
    eprintln!("hrafn: {}", msg.as_ref());
    1
}

pub fn run(mut args: Vec<String>) -> i32 {
    let mut tab = None;
    let mut raw_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                raw_json = true;
                args.remove(i);
            }
            "--tab" if i + 1 < args.len() => match args[i + 1].parse::<i64>() {
                Ok(t) => {
                    tab = Some(t);
                    args.drain(i..i + 2);
                }
                Err(_) => return fail("--tab needs a number (see `hrafn tabs`)"),
            },
            _ => i += 1,
        }
    }
    let Some(cmd) = args.first().cloned() else {
        print!("{HELP}");
        return 0;
    };
    let rest = &args[1..];
    match cmd.as_str() {
        "help" | "-h" | "--help" => {
            print!("{HELP}");
            0
        }
        "--version" | "version" => {
            println!("hrafn {}", env!("CARGO_PKG_VERSION"));
            0
        }
        "doctor" => doctor(),
        "wait" => match rest.first().and_then(|m| m.parse::<u64>().ok()) {
            Some(ms) => {
                std::thread::sleep(Duration::from_millis(ms));
                0
            }
            None => fail("usage: hrafn wait <milliseconds>"),
        },
        _ => match build(&cmd, rest) {
            Ok((name, a)) => send_and_print(&name, a, tab, raw_json, rest),
            Err(e) => fail(e),
        },
    }
}

/// Take `--flag VALUE` out of `rest`.
fn opt(rest: &mut Vec<String>, flag: &str) -> Option<String> {
    let i = rest.iter().position(|a| a == flag)?;
    if i + 1 < rest.len() {
        let v = rest.remove(i + 1);
        rest.remove(i);
        Some(v)
    } else {
        rest.remove(i);
        None
    }
}

/// Take a bare `--flag` out of `rest`.
fn flag(rest: &mut Vec<String>, f: &str) -> bool {
    match rest.iter().position(|a| a == f) {
        Some(i) => {
            rest.remove(i);
            true
        }
        None => false,
    }
}

fn num(v: Option<String>, name: &str) -> Result<Option<u64>, String> {
    v.map(|s| s.parse::<u64>().map_err(|_| format!("{name} needs a number"))).transpose()
}

fn need(rest: &[String], n: usize, usage: &str) -> Result<(), String> {
    if rest.len() < n { Err(format!("usage: hrafn {usage}")) } else { Ok(()) }
}

/// Turn a command line into the extension command name and its arguments.
fn build(cmd: &str, rest: &[String]) -> Result<(String, Value), String> {
    let mut r: Vec<String> = rest.to_vec();
    let out = |name: &str, a: Value| Ok((name.to_string(), a));
    match cmd {
        "status" | "tabs" | "back" | "forward" | "reload" | "detach" => out(cmd, json!({})),
        "open" | "navigate" => {
            need(&r, 1, &format!("{cmd} <url>"))?;
            out(cmd, json!({ "url": r[0] }))
        }
        "tab" => match r.first().map(String::as_str) {
            Some("new") => out("open", json!({ "url": r.get(1).cloned().unwrap_or_else(|| "about:blank".into()) })),
            Some("switch") => {
                let id: i64 = r.get(1).and_then(|s| s.parse().ok()).ok_or("usage: hrafn tab switch <id>")?;
                out("switch", json!({ "id": id }))
            }
            Some("close") => {
                let id = r.get(1).and_then(|s| s.parse::<i64>().ok());
                out("close", json!({ "id": id }))
            }
            _ => Err("usage: hrafn tab new [url] | tab switch <id> | tab close [id]".into()),
        },
        "read" | "tree" | "text" => {
            let max = num(opt(&mut r, "--max-chars"), "--max-chars")?;
            let mode = if cmd != "read" {
                cmd
            } else if flag(&mut r, "--tree-only") {
                "tree"
            } else if flag(&mut r, "--text-only") {
                "text"
            } else {
                "both"
            };
            out("read", json!({ "mode": mode, "ref": r.first(), "max_chars": max }))
        }
        "find" => {
            let role = opt(&mut r, "--role");
            need(&r, 1, "find <words> [--role ROLE]")?;
            out("find", json!({ "query": r.join(" "), "role": role }))
        }
        "click" | "hover" | "focus" | "html" => {
            need(&r, 1, &format!("{cmd} <target>"))?;
            out(cmd, json!({ "target": r[0] }))
        }
        "type" => {
            let append = flag(&mut r, "--append");
            need(&r, 2, "type <target> <text> [--append]")?;
            out("type", json!({ "target": r[0], "text": r[1..].join(" "), "append": append }))
        }
        "select" => {
            need(&r, 2, "select <target> <value>")?;
            out("select", json!({ "target": r[0], "value": r[1] }))
        }
        "keys" => {
            need(&r, 1, "keys <combo>")?;
            out("keys", json!({ "combo": r[0] }))
        }
        "scroll" => {
            let dir = r.first().map(String::as_str).unwrap_or("down");
            if !["up", "down", "top", "bottom"].contains(&dir) {
                return Err("usage: hrafn scroll <up|down|top|bottom>".into());
            }
            out("scroll", json!({ "dir": dir }))
        }
        "wait-stable" => {
            let quiet = num(opt(&mut r, "--ms"), "--ms")?;
            let timeout = num(opt(&mut r, "--timeout"), "--timeout")?;
            out("wait_stable", json!({ "quiet_ms": quiet, "timeout_ms": timeout }))
        }
        "screenshot" => {
            let full = flag(&mut r, "--full");
            out("screenshot", json!({ "full": full }))
        }
        "eval" => {
            need(&r, 1, "eval <javascript>")?;
            out("eval", json!({ "code": r.join(" ") }))
        }
        "console" => {
            let limit = num(opt(&mut r, "--limit"), "--limit")?;
            out("console", json!({ "limit": limit, "clear": flag(&mut r, "--clear") }))
        }
        "net" => {
            let action = r.first().cloned().unwrap_or_else(|| "log".into());
            if action != "log" && action != "clear" {
                return Err("usage: hrafn net log [--filter TEXT] [--limit N] | net clear".into());
            }
            let filter = opt(&mut r, "--filter");
            let limit = num(opt(&mut r, "--limit"), "--limit")?;
            out("net", json!({ "action": action, "filter": filter, "limit": limit }))
        }
        "cookies" => match r.first().map(String::as_str) {
            Some("set") => {
                need(&r, 2, "cookies set '<json>'")?;
                let details: Value = serde_json::from_str(&r[1]).map_err(|e| format!("cookie JSON: {e}"))?;
                out("cookies", json!({ "action": "set", "details": details }))
            }
            Some("delete") => {
                need(&r, 3, "cookies delete <url> <name>")?;
                out("cookies", json!({ "action": "delete", "url": r[1], "name": r[2] }))
            }
            Some(domain) => out("cookies", json!({ "action": "get", "domain": domain })),
            None => Err("usage: hrafn cookies <domain> | cookies set '<json>' | cookies delete <url> <name>".into()),
        },
        other => Err(format!("unknown command `{other}` (see `hrafn help`)")),
    }
}

const NOT_CONNECTED: &str = "Brave isn't connected to Hrafn.\n\
  Check that the Hrafn extension is loaded and enabled in brave://extensions\n\
  (click its reload arrow), then try again. `hrafn doctor` shows details.";

/// First socket with a live host behind it.
fn connect() -> Option<UnixStream> {
    paths::client_sockets().iter().find_map(|s| UnixStream::connect(s).ok())
}

fn call(cmd: &str, args: Value, tab: Option<i64>) -> Result<Response, String> {
    let stream = connect().ok_or_else(|| NOT_CONNECTED.to_string())?;
    stream.set_read_timeout(Some(Duration::from_secs(160))).ok();
    let req = Request { id: 1, cmd: cmd.into(), args, tab };
    let mut line = serde_json::to_vec(&req).map_err(|e| e.to_string())?;
    line.push(b'\n');
    (&stream).write_all(&line).map_err(|e| format!("could not send: {e}"))?;
    let mut reply = String::new();
    BufReader::new(&stream).read_line(&mut reply).map_err(|e| format!("no reply: {e}"))?;
    if reply.is_empty() {
        return Err(NOT_CONNECTED.into());
    }
    serde_json::from_str(&reply).map_err(|e| format!("unreadable reply: {e}"))
}

fn send_and_print(cmd: &str, args: Value, tab: Option<i64>, raw_json: bool, rest: &[String]) -> i32 {
    let resp = match call(cmd, args, tab) {
        Ok(r) => r,
        Err(e) => return fail(e),
    };
    if !resp.ok {
        return fail(resp.error.unwrap_or_else(|| "failed".into()));
    }
    let res = resp.result;
    if cmd == "screenshot" {
        return save_screenshot(&res, rest);
    }
    if raw_json {
        println!("{}", serde_json::to_string_pretty(&res).unwrap_or_default());
        return 0;
    }
    match cmd {
        "tabs" => {
            for t in res.as_array().into_iter().flatten() {
                println!(
                    "{} {:>10}  {}  {}",
                    if t["current"].as_bool() == Some(true) { "*" } else { " " },
                    t["id"],
                    t["title"].as_str().unwrap_or(""),
                    t["url"].as_str().unwrap_or("")
                );
            }
        }
        "read" => {
            for key in ["tree", "text"] {
                if let Some(s) = res[key].as_str() {
                    if key == "text" && res["tree"].is_string() {
                        println!("\n--- text ---");
                    }
                    println!("{s}");
                }
            }
        }
        "find" => {
            for m in res.as_array().into_iter().flatten() {
                println!("{}  {} \"{}\"", m["ref"].as_str().unwrap_or(""), m["role"].as_str().unwrap_or(""), m["name"].as_str().unwrap_or(""));
            }
        }
        "html" => println!("{}", res.as_str().unwrap_or_default()),
        "console" => {
            for e in res.as_array().into_iter().flatten() {
                println!("[{}] {}", e["level"].as_str().unwrap_or("log"), e["text"].as_str().unwrap_or(""));
            }
        }
        "net" => {
            for e in res.as_array().into_iter().flatten() {
                println!(
                    "{:>4} {:6} {:10} {}",
                    e["status"].as_i64().map(|s| s.to_string()).unwrap_or_else(|| if e["failed"].is_string() { "ERR".into() } else { "…".into() }),
                    e["method"].as_str().unwrap_or(""),
                    e["type"].as_str().unwrap_or(""),
                    e["url"].as_str().unwrap_or("")
                );
            }
        }
        _ => match &res {
            Value::String(s) => println!("{s}"),
            Value::Null => println!("ok"),
            other => println!("{}", serde_json::to_string_pretty(other).unwrap_or_default()),
        },
    }
    0
}

fn save_screenshot(res: &Value, rest: &[String]) -> i32 {
    let Some(data) = res["data"].as_str() else { return fail("the browser sent no image") };
    let bytes = match base64::engine::general_purpose::STANDARD.decode(data) {
        Ok(b) => b,
        Err(e) => return fail(format!("bad image data: {e}")),
    };
    let path = rest.iter().find(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        format!("hrafn-{s}.png")
    });
    match std::fs::write(&path, bytes) {
        Ok(()) => {
            println!("{path}");
            0
        }
        Err(e) => fail(format!("could not write {path}: {e}")),
    }
}

/// Report what is installed and whether the browser is connected.
fn doctor() -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let mut problems = 0;
    println!("hrafn {}", env!("CARGO_PKG_VERSION"));
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    println!("program:  {exe}");
    for (browser, dir) in [
        ("Brave", ".config/BraveSoftware/Brave-Browser/NativeMessagingHosts"),
        ("Chrome", ".config/google-chrome/NativeMessagingHosts"),
    ] {
        let path = format!("{home}/{dir}/{HOST_NAME}.json");
        match std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            Some(m) => {
                let target = m["path"].as_str().unwrap_or("");
                let ok = std::path::Path::new(target).is_file();
                println!(
                    "{browser:7} host file ok, runs {target}{}, allows {}",
                    if ok { "" } else { "  (MISSING PROGRAM)" },
                    m["allowed_origins"]
                );
                if !ok {
                    problems += 1;
                }
            }
            None => println!("{browser:7} host file not installed ({path})"),
        }
    }
    let host = std::path::Path::new(paths::SYSTEM_HOST);
    if host.is_file() {
        println!("host:     {} present", host.display());
    } else {
        problems += 1;
        println!("host:     {} NOT installed (run `cargo xtask install` in the source folder for the command)", host.display());
    }
    match call("status", json!({}), None) {
        Ok(r) if r.ok => println!("browser:  connected ({})", r.result),
        Ok(r) => {
            problems += 1;
            println!("browser:  answered with an error: {}", r.error.unwrap_or_default())
        }
        Err(_) => {
            problems += 1;
            println!("browser:  NOT connected. Load/enable the Hrafn extension and click its reload arrow.");
        }
    }
    for sock in paths::client_sockets() {
        let log = paths::log_for(&sock);
        if let Ok(text) = std::fs::read_to_string(&log) {
            println!("host log (last lines, {}):", log.display());
            for l in text.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev() {
                println!("  {l}");
            }
        }
    }
    if problems == 0 { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(cmd: &str, rest: &[&str]) -> Result<(String, Value), String> {
        build(cmd, &rest.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn builds_commands() {
        assert_eq!(b("open", &["https://example.com"]).unwrap(), ("open".into(), json!({"url": "https://example.com"})));
        assert_eq!(b("tab", &["new"]).unwrap().1["url"], "about:blank");
        assert_eq!(b("tab", &["switch", "42"]).unwrap(), ("switch".into(), json!({"id": 42})));
        assert_eq!(b("type", &["e3", "hello", "world", "--append"]).unwrap().1, json!({"target":"e3","text":"hello world","append":true}));
        assert_eq!(b("read", &["--text-only"]).unwrap().1["mode"], "text");
        assert_eq!(b("read", &["--max-chars", "500"]).unwrap().1["max_chars"], 500);
        assert_eq!(b("net", &["log", "--filter", "api", "--limit", "5"]).unwrap().1, json!({"action":"log","filter":"api","limit":5}));
        assert_eq!(b("cookies", &["example.com"]).unwrap().1, json!({"action":"get","domain":"example.com"}));
    }

    #[test]
    fn rejects_bad_input() {
        assert!(b("open", &[]).is_err());
        assert!(b("scroll", &["sideways"]).is_err());
        assert!(b("read", &["--max-chars", "lots"]).is_err());
        assert!(b("cookies", &["set", "{not json"]).is_err());
        assert!(b("frobnicate", &[]).is_err());
    }
}
