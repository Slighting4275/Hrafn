//! Service-worker side: native port, debugger sessions, command handlers.

use crate::js::{self, call, from_js, get, now, sleep, to_js};
use crate::{ax, keys};
use hrafn_proto::{HOST_NAME, Request, Response};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

const LOG_CAP: usize = 2000;
const DEFAULT_MAX_CHARS: usize = 30_000;

#[derive(Default)]
struct TabLog {
    console: VecDeque<Value>,
    net: VecDeque<Value>,
    inflight: i64,
    last_net: f64,
}

#[derive(Default)]
struct State {
    port: Option<JsValue>,
    reconnect_delay: i32,
    current_tab: Option<i64>,
    attached: HashSet<i64>,
    refs: HashMap<i64, HashMap<String, i64>>,
    logs: HashMap<i64, TabLog>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::new(State { reconnect_delay: 1000, ..Default::default() });
}

fn with<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(&mut s.borrow_mut()))
}

fn closure1(f: impl FnMut(JsValue) + 'static) -> JsValue {
    Closure::<dyn FnMut(JsValue)>::new(f).into_js_value()
}

// ------------------------------------------------------------------ startup

#[wasm_bindgen]
pub fn start() {
    // Debugger events: console and network logging.
    js::listen(
        &["chrome", "debugger", "onEvent"],
        Closure::<dyn FnMut(JsValue, JsValue, JsValue)>::new(|source: JsValue, method: JsValue, params: JsValue| {
            if let (Some(tab), Some(m)) = (get(&source, "tabId").as_f64(), method.as_string()) {
                on_cdp_event(tab as i64, &m, from_js(params));
            }
        })
        .into_js_value(),
    );
    js::listen(
        &["chrome", "debugger", "onDetach"],
        Closure::<dyn FnMut(JsValue, JsValue)>::new(|source: JsValue, _reason: JsValue| {
            if let Some(tab) = get(&source, "tabId").as_f64() {
                with(|s| s.attached.remove(&(tab as i64)));
            }
        })
        .into_js_value(),
    );
    js::listen(
        &["chrome", "tabs", "onRemoved"],
        closure1(|tab: JsValue| {
            if let Some(t) = tab.as_f64().map(|t| t as i64) {
                with(|s| {
                    s.attached.remove(&t);
                    s.refs.remove(&t);
                    s.logs.remove(&t);
                    if s.current_tab == Some(t) {
                        s.current_tab = None;
                    }
                });
            }
        }),
    );
    // A periodic alarm wakes the worker and reconnects if the host went away.
    js::listen(&["chrome", "alarms", "onAlarm"], closure1(|_| ensure_connected()));
    spawn_local(async {
        let _ = call(&["chrome", "alarms"], "create", vec!["hrafn-keepalive".into(), to_js(&json!({"periodInMinutes": 1}))]).await;
    });
    ensure_connected();
}

fn ensure_connected() {
    if with(|s| s.port.is_some()) {
        return;
    }
    let Some(port) = js::call_sync(&js::path(&["chrome", "runtime"]), "connectNative", &HOST_NAME.into()) else {
        return;
    };
    js::listen_on(&get(&port, "onMessage"), closure1(|msg: JsValue| {
        if let Ok(req) = serde_json::from_value::<Request>(from_js(msg)) {
            spawn_local(handle(req));
        }
    }));
    js::listen_on(&get(&port, "onDisconnect"), closure1(|_| {
        let delay = with(|s| {
            s.port = None;
            let d = s.reconnect_delay;
            s.reconnect_delay = (d * 2).min(30_000);
            d
        });
        spawn_local(async move {
            sleep(delay).await;
            ensure_connected();
        });
    }));
    with(|s| s.port = Some(port));
    // The port only counts as working once a message round-trips; reset backoff
    // after it has stayed up for a while.
    spawn_local(async {
        sleep(5000).await;
        with(|s| {
            if s.port.is_some() {
                s.reconnect_delay = 1000;
            }
        });
    });
}

async fn handle(req: Request) {
    let id = req.id;
    let resp = match dispatch(&req).await {
        Ok(v) => Response::ok(id, v),
        Err(e) => Response::err(id, e),
    };
    let msg = serde_json::to_value(&resp).unwrap_or(Value::Null);
    if let Some(port) = with(|s| s.port.clone()) {
        let _ = js::call_sync(&port, "postMessage", &to_js(&msg));
    }
}

// ------------------------------------------------------------------ events

fn on_cdp_event(tab: i64, method: &str, p: Value) {
    let t = now();
    with(|s| {
        let log = s.logs.entry(tab).or_default();
        let push = |q: &mut VecDeque<Value>, v: Value| {
            if q.len() >= LOG_CAP {
                q.pop_front();
            }
            q.push_back(v);
        };
        match method {
            "Runtime.consoleAPICalled" => {
                let text = p["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|a| match &a["value"] {
                        Value::String(s) => s.clone(),
                        Value::Null => a["description"].as_str().unwrap_or(a["type"].as_str().unwrap_or("")).to_string(),
                        v => v.to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                push(&mut log.console, json!({"ts": t, "level": p["type"], "text": text}));
            }
            "Runtime.exceptionThrown" => {
                let d = &p["exceptionDetails"];
                let text = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("exception");
                push(&mut log.console, json!({"ts": t, "level": "error", "text": text}));
            }
            "Log.entryAdded" => {
                let e = &p["entry"];
                push(&mut log.console, json!({"ts": t, "level": e["level"], "text": e["text"], "url": e["url"]}));
            }
            "Network.requestWillBeSent" => {
                let r = &p["request"];
                push(&mut log.net, json!({"ts": t, "id": p["requestId"], "method": r["method"], "url": r["url"], "type": p["type"], "status": null}));
                log.inflight += 1;
                log.last_net = t;
            }
            "Network.responseReceived" => {
                if let Some(e) = log.net.iter_mut().rev().find(|e| e["id"] == p["requestId"]) {
                    e["status"] = p["response"]["status"].clone();
                    e["mime"] = p["response"]["mimeType"].clone();
                }
                log.last_net = t;
            }
            "Network.loadingFinished" | "Network.loadingFailed" => {
                if method == "Network.loadingFailed"
                    && let Some(e) = log.net.iter_mut().rev().find(|e| e["id"] == p["requestId"])
                {
                    e["failed"] = p["errorText"].clone();
                }
                log.inflight = (log.inflight - 1).max(0);
                log.last_net = t;
            }
            "Page.frameNavigated" if p["frame"].get("parentId").is_none() => {
                s.refs.remove(&tab);
            }
            _ => {}
        }
    });
}

// ------------------------------------------------------------------ helpers

fn arg<'a>(r: &'a Request, k: &str) -> &'a Value {
    &r.args[k]
}

fn arg_str(r: &Request, k: &str) -> Result<String, String> {
    arg(r, k).as_str().map(String::from).ok_or_else(|| format!("missing `{k}`"))
}

async fn tab_info(tab: i64) -> Result<Value, String> {
    let t = from_js(call(&["chrome", "tabs"], "get", vec![JsValue::from(tab as f64)]).await?);
    Ok(json!({"id": t["id"], "title": t["title"], "url": t["url"], "status": t["status"], "windowId": t["windowId"]}))
}

/// The tab a command acts on: explicit, else current, else the focused window's active tab.
async fn target_tab(req: &Request) -> Result<i64, String> {
    if let Some(t) = req.tab {
        tab_info(t).await.map_err(|_| format!("there is no tab {t} (see `hrafn tabs`)"))?;
        return Ok(t);
    }
    if let Some(t) = with(|s| s.current_tab) {
        if tab_info(t).await.is_ok() {
            return Ok(t);
        }
        with(|s| s.current_tab = None);
    }
    let tabs = from_js(call(&["chrome", "tabs"], "query", vec![to_js(&json!({"active": true, "lastFocusedWindow": true}))]).await?);
    tabs[0]["id"].as_i64().ok_or_else(|| "no tab is open".to_string())
}

async fn attach(tab: i64) -> Result<(), String> {
    if with(|s| s.attached.contains(&tab)) {
        return Ok(());
    }
    let url = tab_info(tab).await?["url"].as_str().unwrap_or("").to_string();
    if !(url.starts_with("http://") || url.starts_with("https://") || url.starts_with("file://") || url == "about:blank" || url.is_empty()) {
        return Err(format!("the browser does not allow extensions to control this page ({url})"));
    }
    call(&["chrome", "debugger"], "attach", vec![to_js(&json!({"tabId": tab})), "1.3".into()])
        .await
        .map_err(|e| format!("could not attach to tab {tab}: {e} (close DevTools on that tab if it is open)"))?;
    with(|s| {
        s.attached.insert(tab);
        s.logs.entry(tab).or_default();
    });
    for domain in ["Page.enable", "Runtime.enable", "Network.enable", "Log.enable", "DOM.enable"] {
        cdp_raw(tab, domain, json!({})).await?;
    }
    Ok(())
}

async fn cdp_raw(tab: i64, method: &str, params: Value) -> Result<Value, String> {
    let r = call(&["chrome", "debugger"], "sendCommand", vec![to_js(&json!({"tabId": tab})), method.into(), to_js(&params)])
        .await
        .map_err(|e| format!("{method}: {e}"))?;
    Ok(from_js(r))
}

async fn cdp(tab: i64, method: &str, params: Value) -> Result<Value, String> {
    attach(tab).await?;
    match cdp_raw(tab, method, params.clone()).await {
        // The browser can drop the debugger mid-command (e.g. while it first shows
        // its "debugging" bar). Reattach once and retry.
        Err(e) if e.contains("Detached") || e.contains("not attached") => {
            with(|s| s.attached.remove(&tab));
            sleep(200).await;
            attach(tab).await?;
            cdp_raw(tab, method, params).await
        }
        other => other,
    }
}

async fn wait_load(tab: i64, timeout_ms: f64) -> Result<(), String> {
    sleep(150).await;
    let end = now() + timeout_ms;
    while now() < end {
        if tab_info(tab).await?["status"] == "complete" {
            return Ok(());
        }
        sleep(150).await;
    }
    Err("the page did not finish loading in time".into())
}

async fn eval(tab: i64, expr: &str) -> Result<Value, String> {
    let r = cdp(tab, "Runtime.evaluate", json!({"expression": expr, "returnByValue": true, "awaitPromise": true, "userGesture": true})).await?;
    if let Some(d) = r.get("exceptionDetails") {
        let msg = d["exception"]["description"].as_str().or(d["text"].as_str()).unwrap_or("exception").to_string();
        return Err(msg);
    }
    let res = &r["result"];
    Ok(match res.get("value") {
        Some(v) => v.clone(),
        None => res["description"].clone().as_str().map(|s| json!(s)).unwrap_or_else(|| json!(res["type"])),
    })
}

/// Fresh accessibility tree for a tab; also stores the refs it hands out.
async fn read_tree(tab: i64, root: Option<i64>, max: usize) -> Result<ax::Rendered, String> {
    let r = cdp(tab, "Accessibility.getFullAXTree", json!({})).await?;
    let nodes = r["nodes"].as_array().cloned().unwrap_or_default();
    let rendered = ax::render(&nodes, root, max);
    with(|s| s.refs.insert(tab, rendered.refs.clone()));
    Ok(rendered)
}

/// A ref (`e12`) or CSS selector -> backend DOM node id.
async fn resolve(tab: i64, target: &str) -> Result<i64, String> {
    let is_ref = target.len() > 1 && target.starts_with('e') && target[1..].chars().all(|c| c.is_ascii_digit());
    if is_ref {
        return with(|s| s.refs.get(&tab).and_then(|m| m.get(target)).copied())
            .ok_or_else(|| format!("unknown ref {target}: run `hrafn read` first (refs reset when the page changes)"));
    }
    let doc = cdp(tab, "DOM.getDocument", json!({"depth": 0})).await?;
    let root = doc["root"]["nodeId"].as_i64().ok_or("no document")?;
    let q = cdp(tab, "DOM.querySelector", json!({"nodeId": root, "selector": target})).await?;
    let node = q["nodeId"].as_i64().filter(|n| *n != 0).ok_or_else(|| format!("nothing matches `{target}`"))?;
    let d = cdp(tab, "DOM.describeNode", json!({"nodeId": node})).await?;
    d["node"]["backendNodeId"].as_i64().ok_or_else(|| "element has no id".into())
}

async fn object_id(tab: i64, backend: i64) -> Result<String, String> {
    let r = cdp(tab, "DOM.resolveNode", json!({"backendNodeId": backend})).await?;
    r["object"]["objectId"].as_str().map(String::from).ok_or_else(|| "element is gone".into())
}

async fn call_on(tab: i64, backend: i64, func: &str, args: Vec<Value>) -> Result<Value, String> {
    let oid = object_id(tab, backend).await?;
    let a: Vec<Value> = args.into_iter().map(|v| json!({"value": v})).collect();
    let r = cdp(tab, "Runtime.callFunctionOn", json!({"objectId": oid, "functionDeclaration": func, "arguments": a, "returnByValue": true})).await?;
    if let Some(d) = r.get("exceptionDetails") {
        return Err(d["exception"]["description"].as_str().unwrap_or("error").to_string());
    }
    Ok(r["result"]["value"].clone())
}

async fn center(tab: i64, backend: i64) -> Result<(f64, f64), String> {
    let _ = cdp(tab, "DOM.scrollIntoViewIfNeeded", json!({"backendNodeId": backend})).await;
    let q = cdp(tab, "DOM.getContentQuads", json!({"backendNodeId": backend})).await?;
    let quad: Vec<f64> = q["quads"][0].as_array().ok_or("element is not visible")?.iter().filter_map(Value::as_f64).collect();
    if quad.len() != 8 {
        return Err("element is not visible".into());
    }
    let x = (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0;
    let y = (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0;
    Ok((x, y))
}

async fn mouse(tab: i64, kind: &str, x: f64, y: f64) -> Result<(), String> {
    let mut p = json!({"type": kind, "x": x, "y": y});
    if kind != "mouseMoved" {
        p["button"] = json!("left");
        p["clickCount"] = json!(1);
    }
    cdp(tab, "Input.dispatchMouseEvent", p).await.map(|_| ())
}

// ------------------------------------------------------------------ commands

async fn dispatch(r: &Request) -> Result<Value, String> {
    match r.cmd.as_str() {
        "status" => {
            let version = from_js(call(&["chrome", "runtime"], "getManifest", vec![]).await?)["version"].clone();
            let ua = get(&js::path(&["navigator"]), "userAgent").as_string().unwrap_or_default();
            Ok(json!({"extension": version, "browser": ua}))
        }
        "tabs" => {
            let cur = with(|s| s.current_tab);
            let tabs = from_js(call(&["chrome", "tabs"], "query", vec![to_js(&json!({}))]).await?);
            Ok(Value::Array(
                tabs.as_array()
                    .into_iter()
                    .flatten()
                    .map(|t| json!({"id": t["id"], "title": t["title"], "url": t["url"], "active": t["active"], "current": t["id"].as_i64() == cur}))
                    .collect(),
            ))
        }
        "open" => {
            let url = arg_str(r, "url")?;
            // Open blank, attach, then load: console and network logs then cover the
            // page from its very first request.
            let t = from_js(call(&["chrome", "tabs"], "create", vec![to_js(&json!({"url": "about:blank", "active": true}))]).await?);
            let id = t["id"].as_i64().ok_or("could not open a tab")?;
            with(|s| s.current_tab = Some(id));
            if url != "about:blank" {
                let _ = attach(id).await;
                call(&["chrome", "tabs"], "update", vec![JsValue::from(id as f64), to_js(&json!({"url": url}))]).await?;
            }
            wait_load(id, 60_000.0).await?;
            tab_info(id).await
        }
        "switch" => {
            let id = arg(r, "id").as_i64().ok_or("missing tab id")?;
            let t = from_js(call(&["chrome", "tabs"], "update", vec![JsValue::from(id as f64), to_js(&json!({"active": true}))]).await?);
            let _ = call(&["chrome", "windows"], "update", vec![to_js(&t["windowId"]), to_js(&json!({"focused": true}))]).await;
            with(|s| s.current_tab = Some(id));
            tab_info(id).await
        }
        "close" => {
            let id = match arg(r, "id").as_i64() {
                Some(i) => i,
                None => target_tab(r).await?,
            };
            call(&["chrome", "tabs"], "remove", vec![JsValue::from(id as f64)]).await?;
            with(|s| {
                if s.current_tab == Some(id) {
                    s.current_tab = None;
                }
            });
            Ok(json!(format!("closed tab {id}")))
        }
        "detach" => {
            let tab = target_tab(r).await?;
            if with(|s| s.attached.contains(&tab)) {
                call(&["chrome", "debugger"], "detach", vec![to_js(&json!({"tabId": tab}))]).await?;
                with(|s| s.attached.remove(&tab));
            }
            Ok(json!(format!("stopped debugging tab {tab}")))
        }
        "cookies" => cookies(r).await,
        _ => {
            let tab = target_tab(r).await?;
            page_command(r, tab).await
        }
    }
}

async fn page_command(r: &Request, tab: i64) -> Result<Value, String> {
    if matches!(r.cmd.as_str(), "navigate" | "back" | "forward" | "reload") {
        // Attach before loading so the logs cover the whole load. Pages the browser
        // won't let extensions debug (settings, the web store) still navigate.
        let _ = attach(tab).await;
    }
    match r.cmd.as_str() {
        "navigate" => {
            let url = arg_str(r, "url")?;
            call(&["chrome", "tabs"], "update", vec![JsValue::from(tab as f64), to_js(&json!({"url": url}))]).await?;
            wait_load(tab, 60_000.0).await?;
            tab_info(tab).await
        }
        "back" | "forward" | "reload" => {
            let m = match r.cmd.as_str() {
                "back" => "goBack",
                "forward" => "goForward",
                _ => "reload",
            };
            call(&["chrome", "tabs"], m, vec![JsValue::from(tab as f64)]).await?;
            wait_load(tab, 60_000.0).await?;
            tab_info(tab).await
        }
        "read" => {
            let mode = arg(r, "mode").as_str().unwrap_or("both");
            let max = arg(r, "max_chars").as_u64().map(|m| m as usize).unwrap_or(DEFAULT_MAX_CHARS);
            let root = match arg(r, "ref").as_str() {
                Some(t) => Some(resolve(tab, t).await?),
                None => None,
            };
            let mut out = json!({});
            if mode != "text" {
                out["tree"] = json!(read_tree(tab, root, max).await?.tree);
            }
            if mode != "tree" {
                let text = match root {
                    Some(b) => call_on(tab, b, "function(){return this.innerText||this.textContent||''}", vec![]).await?,
                    None => eval(tab, "document.body ? document.body.innerText : ''").await?,
                };
                let mut text = text.as_str().unwrap_or("").to_string();
                if text.len() > max {
                    let mut cut = max;
                    while !text.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    text.truncate(cut);
                    text.push_str("\n… (truncated)");
                }
                out["text"] = json!(text);
            }
            Ok(out)
        }
        "find" => {
            let q = arg_str(r, "query")?.to_lowercase();
            let role = arg(r, "role").as_str().map(str::to_lowercase);
            let tree = read_tree(tab, None, usize::MAX).await?;
            Ok(Value::Array(
                tree.items
                    .into_iter()
                    .filter(|(_, ro, name)| name.to_lowercase().contains(&q) && role.as_ref().is_none_or(|w| &ro.to_lowercase() == w))
                    .map(|(rf, ro, name)| json!({"ref": rf, "role": ro, "name": name}))
                    .collect(),
            ))
        }
        "html" => {
            let b = resolve(tab, &arg_str(r, "target")?).await?;
            Ok(cdp(tab, "DOM.getOuterHTML", json!({"backendNodeId": b})).await?["outerHTML"].clone())
        }
        "click" | "hover" => {
            let target = arg_str(r, "target")?;
            let b = resolve(tab, &target).await?;
            let (x, y) = center(tab, b).await?;
            mouse(tab, "mouseMoved", x, y).await?;
            if r.cmd == "click" {
                mouse(tab, "mousePressed", x, y).await?;
                mouse(tab, "mouseReleased", x, y).await?;
                sleep(300).await;
            }
            let info = tab_info(tab).await?;
            Ok(json!(format!("{} {target} — now at {}", if r.cmd == "click" { "clicked" } else { "hovering" }, info["url"].as_str().unwrap_or(""))))
        }
        "focus" => {
            let b = resolve(tab, &arg_str(r, "target")?).await?;
            cdp(tab, "DOM.focus", json!({"backendNodeId": b})).await?;
            Ok(json!("focused"))
        }
        "type" => {
            let target = arg_str(r, "target")?;
            let text = arg_str(r, "text")?;
            let b = resolve(tab, &target).await?;
            cdp(tab, "DOM.focus", json!({"backendNodeId": b})).await?;
            if !arg(r, "append").as_bool().unwrap_or(false) {
                call_on(
                    tab,
                    b,
                    "function(){if('value' in this){this.value='';this.dispatchEvent(new Event('input',{bubbles:true}));}else if(this.isContentEditable){this.textContent='';}}",
                    vec![],
                )
                .await?;
            }
            cdp(tab, "Input.insertText", json!({"text": text})).await?;
            Ok(json!(format!("typed {} characters into {target}", text.chars().count())))
        }
        "select" => {
            let b = resolve(tab, &arg_str(r, "target")?).await?;
            let v = call_on(
                tab,
                b,
                "function(v){this.value=v;this.dispatchEvent(new Event('input',{bubbles:true}));this.dispatchEvent(new Event('change',{bubbles:true}));return this.value}",
                vec![json!(arg_str(r, "value")?)],
            )
            .await?;
            Ok(json!(format!("selected {}", v.as_str().unwrap_or(""))))
        }
        "keys" => {
            let k = keys::parse(&arg_str(r, "combo")?)?;
            let mut down = json!({"type": if k.text.is_some() { "keyDown" } else { "rawKeyDown" }, "key": k.key, "code": k.code,
                "windowsVirtualKeyCode": k.vk, "nativeVirtualKeyCode": k.vk, "modifiers": k.modifiers});
            if let Some(t) = &k.text {
                down["text"] = json!(t);
                down["unmodifiedText"] = json!(t);
            }
            cdp(tab, "Input.dispatchKeyEvent", down).await?;
            cdp(tab, "Input.dispatchKeyEvent", json!({"type": "keyUp", "key": k.key, "code": k.code,
                "windowsVirtualKeyCode": k.vk, "nativeVirtualKeyCode": k.vk, "modifiers": k.modifiers})).await?;
            sleep(100).await;
            Ok(json!(format!("pressed {}", arg_str(r, "combo")?)))
        }
        "scroll" => {
            let dir = arg(r, "dir").as_str().unwrap_or("down");
            match dir {
                "top" => eval(tab, "window.scrollTo(0,0)").await?,
                "bottom" => eval(tab, "window.scrollTo(0,(document.scrollingElement||document.body).scrollHeight)").await?,
                _ => {
                    let m = cdp(tab, "Page.getLayoutMetrics", json!({})).await?;
                    let w = m["cssLayoutViewport"]["clientWidth"].as_f64().unwrap_or(800.0);
                    let h = m["cssLayoutViewport"]["clientHeight"].as_f64().unwrap_or(600.0);
                    let dy = if dir == "up" { -0.8 * h } else { 0.8 * h };
                    cdp(tab, "Input.dispatchMouseEvent", json!({"type": "mouseWheel", "x": w / 2.0, "y": h / 2.0, "deltaX": 0, "deltaY": dy})).await?
                }
            };
            sleep(250).await;
            let y = eval(tab, "Math.round(window.scrollY)").await?;
            Ok(json!(format!("scrolled {dir} (y={y})")))
        }
        "wait_stable" => {
            attach(tab).await?;
            let quiet = arg(r, "quiet_ms").as_f64().unwrap_or(500.0);
            let end = now() + arg(r, "timeout_ms").as_f64().unwrap_or(15_000.0);
            loop {
                let ready = eval(tab, "document.readyState").await? == "complete";
                let (inflight, last) = with(|s| s.logs.get(&tab).map(|l| (l.inflight, l.last_net)).unwrap_or((0, 0.0)));
                if ready && inflight == 0 && now() - last >= quiet {
                    return Ok(json!("page is stable"));
                }
                if now() > end {
                    return Err(format!("still busy after the timeout ({inflight} requests in flight)"));
                }
                sleep(100).await;
            }
        }
        "screenshot" => {
            // A hidden tab never paints, so bring it to the front first.
            call(&["chrome", "tabs"], "update", vec![JsValue::from(tab as f64), to_js(&json!({"active": true}))]).await?;
            let mut p = json!({"format": "png"});
            if arg(r, "full").as_bool().unwrap_or(false) {
                let m = cdp(tab, "Page.getLayoutMetrics", json!({})).await?;
                let size = &m["cssContentSize"];
                p["captureBeyondViewport"] = json!(true);
                p["clip"] = json!({"x": 0, "y": 0, "width": size["width"], "height": size["height"], "scale": 1});
            }
            let shot = cdp(tab, "Page.captureScreenshot", p).await?;
            Ok(json!({"data": shot["data"]}))
        }
        "eval" => eval(tab, &arg_str(r, "code")?).await,
        "console" | "net" => {
            attach(tab).await?;
            let limit = arg(r, "limit").as_u64().unwrap_or(50) as usize;
            let filter = arg(r, "filter").as_str().map(str::to_lowercase);
            let clear = arg(r, "clear").as_bool().unwrap_or(false) || arg(r, "action") == "clear";
            Ok(with(|s| {
                let log = s.logs.entry(tab).or_default();
                let q = if r.cmd == "console" { &mut log.console } else { &mut log.net };
                if clear {
                    q.clear();
                    return json!("cleared");
                }
                let hits: Vec<Value> = q
                    .iter()
                    .filter(|e| filter.as_ref().is_none_or(|f| e["url"].as_str().unwrap_or("").to_lowercase().contains(f)))
                    .cloned()
                    .collect();
                Value::Array(hits[hits.len().saturating_sub(limit)..].to_vec())
            }))
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

async fn cookies(r: &Request) -> Result<Value, String> {
    match arg(r, "action").as_str().unwrap_or("get") {
        "set" => Ok(from_js(call(&["chrome", "cookies"], "set", vec![to_js(arg(r, "details"))]).await?)),
        "delete" => {
            let d = json!({"url": arg_str(r, "url")?, "name": arg_str(r, "name")?});
            call(&["chrome", "cookies"], "remove", vec![to_js(&d)]).await?;
            Ok(json!("deleted"))
        }
        _ => Ok(from_js(call(&["chrome", "cookies"], "getAll", vec![to_js(&json!({"domain": arg_str(r, "domain")?}))]).await?)),
    }
}
