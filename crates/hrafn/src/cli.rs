//! The `hrafn` command line client.

use hrafn::paths;
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
  click <target> [--double|--triple|--right] | click --at X,Y
  dblclick <target> | rightclick <target> | hover <target> | focus <target>
  drag <from> <to> [--steps N]   (or --from-at X,Y / --to-at X,Y)
  type <target> <text> [--append]
  select <target> <value>
  upload <file-input> <file>... Attach files to an <input type=file> (files must
                               be in a folder the browser can see, e.g. ~/Downloads)
  keys <combo>                 e.g. Enter, Tab, Escape, Control+A, Shift+ArrowDown
  scroll <up|down|top|bottom>

Waiting and capture
  wait <ms>                    Pause
  wait-stable [--ms N] [--timeout N]
                               Wait until the page is loaded and the network is quiet
  wait-for <selector> | wait-for --text TEXT   [--gone] [--timeout MS]
  diff                         What changed on the page since the last diff
  screenshot [file.png] [--full]
  eval <javascript>            Run JS in the page and print the result

Recording and replay
  record start                 Record your clicks, typing, choices and Enter presses
  record stop [plan-file]      Stop and save (or print) the steps as a plan
  record status
  run <plan-file | -> [--keep-going] [--quiet]
                               Replay a plan: one command per line, # comments
                               --quiet prints only line numbers and the summary

Logs, network and cookies
  console [--limit N] [--clear]
  net log [--filter TEXT] [--limit N] | net body <id> [--out FILE] | net clear
  sse [--filter TEXT] [--limit N] | sse clear        Server-sent event messages
  headers [list] | headers add <name> <value> | headers remove <name> | headers clear
  override [list] | override add <url-pattern> [--status N] [--type MIME]
                                 [--body TEXT | --file PATH] | override clear
                               Answer matching requests with a fake response
                               (pattern: * = anything, e.g. '*/api/items*')
  cookies <domain> | cookies set '<json>' | cookies delete <url> <name>

Reading text from pixels (OCR, offline; models download once, ~12 MB)
  ocr [--target T] [--full]    Text visible on the page (or one element)
  ocr <image-file>             Text in an image file (no browser needed)
  ocr --boxes                  Each line with its centre as X,Y (for click --at)
  ocr --find TEXT              Where TEXT is on screen (X,Y)
  ocr --click TEXT             Click the visible TEXT (canvas, images, anything)

Sites (use your own signed-in session in the browser)
  chatgpt status | send <prompt> [--stream] [--timeout S] | read [--all]
  chatgpt conversations | switch <id> | new | stop | model [name]
                               Prompt `-` reads it from stdin
  linkedin job|event|post|profile [url]   Read the LinkedIn page you have open
  linkedin attendees [url] [--max N]      People listed for an event (default 100)
  gdocs info | text | export <format> <file>
                               Doc/Sheet/Slides text or a download (txt, md, pdf,
                               docx, csv, xlsx, pptx, ...) via Google's own export
  gdocs insert <text> [--end]  Type at the cursor (--end: at the end of the doc)
  gdocs slides | slide <n> | notes        Slides: list, go to, speaker notes

Page styling
  style add '<css>' | style remove <id> | style clear

Other
  detach                       Stop debugging the current tab (removes Brave's banner)
  doctor                       Check the install and the browser connection
  help
";

fn fail(msg: impl AsRef<str>) -> i32 {
    eprintln!("hrafn: {}", msg.as_ref());
    1
}

pub fn run(args: Vec<String>) -> i32 {
    let mut tab = None;
    let mut raw_json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => {
                raw_json = true;
                i += 1;
            }
            "--tab" => match args.get(i + 1).and_then(|id| id.parse::<i64>().ok()) {
                Some(t) => {
                    tab = Some(t);
                    i += 2;
                }
                None => return fail("--tab needs a number (see `hrafn tabs`)"),
            },
            _ => break,
        }
    }
    let Some((cmd, rest)) = args[i..].split_first() else {
        print!("{HELP}");
        return 0;
    };
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
        "run" | "replay" => run_plan(rest, tab),
        "ocr" => ocr_cmd(rest.to_vec(), tab, raw_json),
        "chatgpt" | "linkedin" | "gdocs" => {
            let rpc = |c: &str, a: Value, t: Option<i64>| -> Result<Value, String> {
                let r = call(c, a, t)?;
                if r.ok { Ok(r.result) } else { Err(r.error.unwrap_or_else(|| "failed".into())) }
            };
            let res = match cmd.as_str() {
                "chatgpt" => crate::sites::chatgpt(&rpc, tab, rest, raw_json),
                "linkedin" => crate::sites::linkedin(&rpc, tab, rest, raw_json),
                _ => crate::sites::gdocs(&rpc, tab, rest, raw_json),
            };
            match res {
                Ok(()) => 0,
                Err(e) => fail(e),
            }
        }
        "wait" => match rest.first().and_then(|m| m.parse::<u64>().ok()) {
            Some(ms) => {
                std::thread::sleep(Duration::from_millis(ms));
                0
            }
            None => fail("usage: hrafn wait <milliseconds>"),
        },
        _ => match build(cmd, rest) {
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

/// "X,Y" -> [x, y]
fn coords(s: &str) -> Result<[f64; 2], String> {
    let (x, y) = s.split_once(',').ok_or_else(|| format!("expected X,Y but got `{s}`"))?;
    let p = |v: &str| v.trim().parse::<f64>().ok().filter(|n| n.is_finite()).ok_or_else(|| format!("expected finite X,Y coordinates but got `{s}`"));
    Ok([p(x)?, p(y)?])
}

fn need(rest: &[String], n: usize, usage: &str) -> Result<(), String> {
    if rest.len() < n { Err(format!("usage: hrafn {usage}")) } else { Ok(()) }
}

/// Turn a command line into the extension command name and its arguments.
#[cfg(test)]
pub fn build_for_tests(cmd: &str, rest: &[String]) -> Result<(String, Value), String> {
    build(cmd, rest)
}

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
                let id = r.get(1).map(|s| s.parse::<i64>().map_err(|_| "usage: hrafn tab close [id] (id must be a number)")).transpose()?;
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
        "click" | "dblclick" | "rightclick" | "hover" => {
            let at = opt(&mut r, "--at").map(|s| coords(&s)).transpose()?;
            let count = if cmd == "dblclick" || flag(&mut r, "--double") {
                2
            } else if flag(&mut r, "--triple") {
                3
            } else {
                1
            };
            let button = if cmd == "rightclick" || flag(&mut r, "--right") { "right" } else { "left" };
            if at.is_none() {
                need(&r, 1, &format!("{cmd} <target> | {cmd} --at X,Y"))?;
            }
            let name = if cmd == "hover" { "hover" } else { "click" };
            out(name, json!({ "target": r.first(), "at": at, "count": count, "button": button }))
        }
        "drag" => {
            let from_at = opt(&mut r, "--from-at").map(|s| coords(&s)).transpose()?;
            let to_at = opt(&mut r, "--to-at").map(|s| coords(&s)).transpose()?;
            let steps = num(opt(&mut r, "--steps"), "--steps")?;
            let mut words = r.iter();
            let from = if from_at.is_none() { words.next().cloned() } else { None };
            let to = if to_at.is_none() { words.next().cloned() } else { None };
            if (from.is_none() && from_at.is_none()) || (to.is_none() && to_at.is_none()) {
                return Err("usage: hrafn drag <from> <to> (or --from-at X,Y / --to-at X,Y) [--steps N]".into());
            }
            out("drag", json!({ "from": from, "to": to, "from_at": from_at, "to_at": to_at, "steps": steps }))
        }
        "upload" => {
            need(&r, 2, "upload <file-input target> <file>...")?;
            let mut files = Vec::new();
            for f in &r[1..] {
                let p = std::fs::canonicalize(f).map_err(|e| format!("{f}: {e}"))?;
                files.push(p.display().to_string());
            }
            out("upload", json!({ "target": r[0], "files": files }))
        }
        "focus" | "html" => {
            need(&r, 1, &format!("{cmd} <target>"))?;
            out(cmd, json!({ "target": r[0] }))
        }
        "wait-for" => {
            let text = opt(&mut r, "--text");
            let timeout = num(opt(&mut r, "--timeout"), "--timeout")?;
            let gone = flag(&mut r, "--gone");
            if text.is_none() {
                need(&r, 1, "wait-for <selector> | wait-for --text TEXT [--gone] [--timeout MS]")?;
            }
            out("wait_for", json!({ "selector": if text.is_none() { r.first().cloned() } else { None }, "text": text, "gone": gone, "timeout_ms": timeout }))
        }
        "diff" => out("diff", json!({})),
        "style" => match r.first().map(String::as_str) {
            Some("add") => {
                need(&r, 2, "style add '<css>'")?;
                out("style", json!({ "action": "add", "css": r[1..].join(" ") }))
            }
            Some("remove") => {
                need(&r, 2, "style remove <id>")?;
                out("style", json!({ "action": "remove", "id": r[1] }))
            }
            Some("clear") => out("style", json!({ "action": "clear" })),
            _ => Err("usage: hrafn style add '<css>' | style remove <id> | style clear".into()),
        },
        "headers" => match r.first().map(String::as_str) {
            None | Some("list") => out("headers", json!({ "action": "list" })),
            Some("add") => {
                need(&r, 3, "headers add <name> <value>")?;
                out("headers", json!({ "action": "add", "name": r[1], "value": r[2..].join(" ") }))
            }
            Some("remove") => {
                need(&r, 2, "headers remove <name>")?;
                out("headers", json!({ "action": "remove", "name": r[1] }))
            }
            Some("clear") => out("headers", json!({ "action": "clear" })),
            _ => Err("usage: hrafn headers [list] | headers add <name> <value> | headers remove <name> | headers clear".into()),
        },
        "override" => match r.first().map(String::as_str) {
            None | Some("list") => out("override", json!({ "action": "list" })),
            Some("clear") => out("override", json!({ "action": "clear" })),
            Some("add") => {
                let status = num(opt(&mut r, "--status"), "--status")?;
                if status.is_some_and(|status| !(100..=599).contains(&status)) {
                    return Err("--status must be an HTTP status code from 100 to 599".into());
                }
                let ctype = opt(&mut r, "--type");
                let body = match (opt(&mut r, "--body"), opt(&mut r, "--file")) {
                    (Some(b), _) => b,
                    (None, Some(f)) => std::fs::read_to_string(&f).map_err(|e| format!("could not read {f}: {e}"))?,
                    (None, None) => String::new(),
                };
                if body.len() > 900_000 {
                    return Err("override bodies are limited to about 900 KB".into());
                }
                need(&r, 2, "override add <url-pattern> [--status N] [--type MIME] [--body TEXT | --file PATH]")?;
                out("override", json!({ "action": "add", "pattern": r[1], "status": status, "content_type": ctype, "body": body }))
            }
            _ => Err("usage: hrafn override [list] | override add <url-pattern> ... | override clear".into()),
        },
        "sse" => {
            let action = r.first().filter(|a| !a.starts_with("--")).cloned().unwrap_or_else(|| "log".into());
            let filter = opt(&mut r, "--filter");
            let limit = num(opt(&mut r, "--limit"), "--limit")?;
            out("sse", json!({ "action": action, "filter": filter, "limit": limit, "clear": action == "clear" }))
        }
        "record" => match r.first().map(String::as_str) {
            Some(a @ ("start" | "stop" | "status")) => out("record", json!({ "action": a })),
            _ => Err("usage: hrafn record start | record stop [plan-file] | record status".into()),
        },
        "type" => {
            need(&r, 2, "type <target> <text> [--append]")?;
            let append = r.len() > 2 && r.last().is_some_and(|a| a == "--append");
            if append {
                r.pop();
            }
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
            if action == "body" {
                need(&r, 2, "net body <request-id> [--out FILE]")?;
                return out("net", json!({ "action": "body", "id": r[1] }));
            }
            if action != "log" && action != "clear" {
                return Err("usage: hrafn net log [--filter TEXT] [--limit N] | net body <id> [--out FILE] | net clear".into());
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
    if cmd == "record" && res.get("steps").is_some() {
        let plan = crate::plan::from_steps(res["start_url"].as_str().unwrap_or(""), res["steps"].as_array().map(Vec::as_slice).unwrap_or(&[]));
        return match rest.get(1) {
            Some(file) => match std::fs::write(file, &plan) {
                Ok(()) => {
                    println!("saved {} step(s) to {file}; replay with: hrafn run {file}", plan.lines().filter(|l| !l.starts_with('#')).count());
                    0
                }
                Err(e) => fail(format!("could not write {file}: {e}")),
            },
            None => {
                print!("{plan}");
                0
            }
        };
    }
    if cmd == "net" && res.get("body").is_some() {
        return net_body(&res, rest);
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
            if let Some(notice) = res["notice"].as_str() {
                eprintln!("hrafn: {notice}");
            }
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
        "sse" => {
            for e in res.as_array().into_iter().flatten() {
                let ev = e["event"].as_str().filter(|s| !s.is_empty()).unwrap_or("message");
                println!("[{ev}] {}", e["data"].as_str().unwrap_or(""));
            }
        }
        "net" => {
            for e in res.as_array().into_iter().flatten() {
                println!(
                    "{:<14} {:>4} {:6} {:10} {}",
                    e["id"].as_str().unwrap_or(""),
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

fn net_body(res: &Value, rest: &[String]) -> i32 {
    let body = res["body"].as_str().unwrap_or("");
    let bytes = if res["base64"].as_bool() == Some(true) {
        match base64::engine::general_purpose::STANDARD.decode(body) {
            Ok(b) => b,
            Err(e) => return fail(format!("bad body data: {e}")),
        }
    } else {
        body.as_bytes().to_vec()
    };
    let out = rest.iter().position(|a| a == "--out").and_then(|i| rest.get(i + 1));
    match out {
        Some(path) => match std::fs::write(path, &bytes) {
            Ok(()) => {
                println!("{path} ({} bytes)", bytes.len());
                0
            }
            Err(e) => fail(format!("could not write {path}: {e}")),
        },
        None => match String::from_utf8(bytes) {
            Ok(text) => {
                println!("{text}");
                0
            }
            Err(e) => fail(format!("binary body ({} bytes); save it with --out FILE", e.as_bytes().len())),
        },
    }
}

/// Commands after which a replay waits for the page to settle.
const SETTLE_AFTER: [&str; 11] = ["open", "navigate", "back", "forward", "reload", "click", "type", "select", "keys", "drag", "switch"];

/// Run a plan file (or `-` for stdin): one command per line.
fn run_plan(rest: &[String], mut tab: Option<i64>) -> i32 {
    let keep_going = rest.iter().any(|a| a == "--keep-going");
    let quiet = rest.iter().any(|a| a == "--quiet");
    let Some(src) = rest.iter().find(|a| !a.starts_with("--")) else {
        return fail("usage: hrafn run <plan-file | -> [--keep-going] [--quiet]");
    };
    let text = if src == "-" {
        let mut s = String::new();
        if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s) {
            return fail(format!("could not read stdin: {e}"));
        }
        s
    } else {
        match std::fs::read_to_string(src) {
            Ok(s) => s,
            Err(e) => return fail(format!("could not read {src}: {e}")),
        }
    };
    let (mut passed, mut failed) = (0, 0);
    for (n, line) in text.lines().enumerate() {
        let words = match crate::plan::split(line) {
            Ok(Some(w)) => w,
            Ok(None) => continue,
            Err(e) => {
                if quiet {
                    println!("FAIL line {}", n + 1);
                } else {
                    println!("FAIL line {}: {e}", n + 1);
                }
                failed += 1;
                if keep_going { continue } else { break }
            }
        };
        let (cmd, args) = (&words[0], &words[1..]);
        let result: Result<String, String> = if cmd == "wait" {
            match args.first().and_then(|m| m.parse::<u64>().ok()) {
                Some(ms) => {
                    std::thread::sleep(Duration::from_millis(ms));
                    Ok(format!("waited {ms} ms"))
                }
                None => Err("usage: wait <ms>".into()),
            }
        } else if ["run", "doctor", "help", "screenshot"].contains(&cmd.as_str()) {
            Err(format!("`{cmd}` can't be used inside a plan"))
        } else {
            build(cmd, args).and_then(|(name, a)| {
                let closed_tab = a["id"].as_i64().or(tab);
                let resp = call(&name, a, tab)?;
                if !resp.ok {
                    return Err(resp.error.unwrap_or_else(|| "failed".into()));
                }
                match name.as_str() {
                    "open" | "switch" => tab = resp.result["id"].as_i64(),
                    "close" if closed_tab == tab => tab = None,
                    _ => {}
                }
                if SETTLE_AFTER.contains(&name.as_str()) {
                    let settled = call("wait_stable", json!({ "quiet_ms": 300, "timeout_ms": 8000 }), tab)
                        .and_then(|response| if response.ok { Ok(()) } else { Err(response.error.unwrap_or_else(|| "wait failed".into())) });
                    settled.map_err(|e| format!("action completed, but the page did not settle: {e}"))?;
                }
                Ok(match &resp.result {
                    Value::String(s) => s.clone(),
                    Value::Object(o) if o.contains_key("title") => o["title"].as_str().unwrap_or("").to_string(),
                    _ => "ok".into(),
                })
            })
        };
        match result {
            Ok(msg) => {
                passed += 1;
                if !quiet {
                    println!("ok   line {}: {}  ({})", n + 1, line.trim(), msg.lines().next().unwrap_or(""));
                }
            }
            Err(e) => {
                failed += 1;
                if quiet {
                    println!("FAIL line {}", n + 1);
                } else {
                    println!("FAIL line {}: {}  ({e})", n + 1, line.trim());
                }
                if !keep_going {
                    break;
                }
            }
        }
    }
    println!("{passed} passed, {failed} failed");
    if failed == 0 { 0 } else { 1 }
}

/// `hrafn ocr`: read text from the page (or an image file) with the pure-Rust OCR engine.
fn ocr_cmd(mut r: Vec<String>, tab: Option<i64>, raw_json: bool) -> i32 {
    use crate::ocr;
    let target = opt(&mut r, "--target");
    let find_text = opt(&mut r, "--find");
    let click_text = opt(&mut r, "--click");
    let boxes = flag(&mut r, "--boxes");
    let full = flag(&mut r, "--full");
    if let Some(bad) = r.iter().find(|a| a.starts_with("--")) {
        return fail(format!("unknown option {bad} (see `hrafn help`)"));
    }
    // An image file: no browser needed, positions are image pixels.
    let (bytes, dpr, origin, from_file) = if let Some(file) = r.first() {
        match std::fs::read(file) {
            Ok(b) => (b, 1.0, (0.0, 0.0), true),
            Err(e) => return fail(format!("could not read {file}: {e}")),
        }
    } else {
        let resp = match call("screenshot", json!({ "target": target, "full": full }), tab) {
            Ok(r) if r.ok => r.result,
            Ok(r) => return fail(r.error.unwrap_or_default()),
            Err(e) => return fail(e),
        };
        let data = resp["data"].as_str().unwrap_or("");
        match base64::engine::general_purpose::STANDARD.decode(data) {
            Ok(b) => (
                b,
                resp["dpr"].as_f64().unwrap_or(1.0),
                (resp["origin"][0].as_f64().unwrap_or(0.0), resp["origin"][1].as_f64().unwrap_or(0.0)),
                false,
            ),
            Err(e) => return fail(format!("bad image data: {e}")),
        }
    };
    if click_text.is_some() && (from_file || full) {
        return fail("--click works on the visible page only (not with an image file or --full)");
    }
    let lines = match ocr::recognize(&bytes) {
        Ok(l) => l,
        Err(e) => return fail(e),
    };
    let place = |b: [i32; 4]| {
        let (cx, cy) = ocr::center(b);
        let (x, y) = ocr::to_viewport(cx, cy, dpr, origin);
        (x.round(), y.round())
    };
    if let Some(q) = find_text.as_ref().or(click_text.as_ref()) {
        let Some((i, b)) = ocr::find(&lines, q) else {
            let near = ocr::closest(&lines, q).map(|t| format!("; closest line read: \"{t}\"")).unwrap_or_default();
            return fail(format!("\"{q}\" is not visible (OCR read {} line(s){near})", lines.len()));
        };
        let (x, y) = place(b);
        if click_text.is_some() {
            return match call("click", json!({ "at": [x, y], "count": 1, "button": "left" }), tab) {
                Ok(r) if r.ok => {
                    println!("clicked \"{q}\" at {x},{y} (line: {})", lines[i].text);
                    0
                }
                Ok(r) => fail(r.error.unwrap_or_default()),
                Err(e) => fail(e),
            };
        }
        if raw_json {
            println!("{}", json!({ "x": x, "y": y, "line": lines[i].text }));
        } else {
            println!("{x},{y}  {}", lines[i].text);
        }
        return 0;
    }
    if raw_json {
        let out: Vec<Value> = lines
            .iter()
            .map(|l| {
                let b = l.bounds();
                let (x, y) = place(b);
                json!({ "text": l.text, "x": x, "y": y, "box": b })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&out).unwrap_or_default());
    } else if boxes {
        for l in &lines {
            let (x, y) = place(l.bounds());
            println!("{x},{y}  {}", l.text);
        }
    } else {
        for l in &lines {
            println!("{}", l.text);
        }
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
    fn builds_extra_commands() {
        assert_eq!(b("dblclick", &["#a"]).unwrap().1, json!({"target":"#a","at":null,"count":2,"button":"left"}));
        assert_eq!(b("click", &["--at", "10,20.5", "--right"]).unwrap().1, json!({"target":null,"at":[10.0,20.5],"count":1,"button":"right"}));
        assert_eq!(b("drag", &["#a", "#b", "--steps", "5"]).unwrap().1, json!({"from":"#a","to":"#b","from_at":null,"to_at":null,"steps":5}));
        assert_eq!(b("drag", &["--from-at", "1,2", "#b"]).unwrap().1["to"], "#b");
        assert_eq!(b("wait-for", &["--text", "Done", "--gone"]).unwrap().1, json!({"selector":null,"text":"Done","gone":true,"timeout_ms":null}));
        assert_eq!(b("style", &["add", "body", "{color:red}"]).unwrap().1["css"], "body {color:red}");
        assert_eq!(b("headers", &["add", "X-Test", "yes"]).unwrap().1, json!({"action":"add","name":"X-Test","value":"yes"}));
        assert_eq!(
            b("override", &["add", "*/api/*", "--status", "503", "--body", "{}"]).unwrap().1,
            json!({"action":"add","pattern":"*/api/*","status":503,"content_type":null,"body":"{}"})
        );
        assert_eq!(b("net", &["body", "123.4"]).unwrap().1, json!({"action":"body","id":"123.4"}));
        assert_eq!(b("sse", &["--limit", "3"]).unwrap().1["action"], "log");
        assert_eq!(b("record", &["start"]).unwrap().1, json!({"action":"start"}));
    }

    #[test]
    fn rejects_bad_extra_input() {
        assert!(b("click", &["--at", "10"]).is_err());
        assert!(b("drag", &["#a"]).is_err());
        assert!(b("wait-for", &[]).is_err());
        assert!(b("record", &["pause"]).is_err());
        assert!(b("override", &["add"]).is_err());
        assert!(b("override", &["add", "*", "--file", "/no/such/file"]).is_err());
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
