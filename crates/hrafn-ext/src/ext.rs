//! Service-worker side: native port, debugger sessions, command handlers.

use crate::js::{self, call, from_js, get, now, sleep, to_js};
use crate::{ax, keys};
use hrafn_proto::{HOST_NAME, Request, Response};
use js_sys::{Function, Reflect};
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet, VecDeque};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

const LOG_CAP: usize = 2000;
const DEFAULT_MAX_CHARS: usize = 30_000;
const EXTENSION_FRAME_DENIED: &str = "Cannot access a chrome-extension:// URL of different extension";
const PAGE_FALLBACK_NOTICE: &str = "Another extension's frame blocks the debugger. Use page text and CSS selectors for main-page controls.";

#[derive(Default)]
struct TabLog {
    console: VecDeque<Value>,
    net: VecDeque<Value>,
    sse: VecDeque<Value>,
    inflight: i64,
    last_net: f64,
}

/// A recording in progress on one tab.
#[derive(Default)]
struct Recording {
    start_url: String,
    steps: Vec<Value>,
    script_id: Option<String>,
}

/// A fake response for requests whose URL matches `pattern`.
#[derive(Clone)]
struct Override {
    pattern: String,
    status: u16,
    content_type: String,
    body: String,
}

#[derive(Default)]
struct State {
    port: Option<JsValue>,
    page_action: Option<Function>,
    reconnect_delay: i32,
    current_tab: Option<i64>,
    attached: HashSet<i64>,
    refs: HashMap<i64, HashMap<String, i64>>,
    logs: HashMap<i64, TabLog>,
    recordings: HashMap<i64, Recording>,
    overrides: HashMap<i64, Vec<Override>>,
    headers: HashMap<i64, serde_json::Map<String, Value>>,
    last_tree: HashMap<i64, String>,
    style_seq: u32,
}

/// Page-side recorder. It has to be JavaScript because it runs inside the page; it
/// only reports events to Rust (through the `__hrafnRecord` binding), which decides
/// what becomes a step.
const RECORDER_JS: &str = r#"(() => {
  if (window.__hrafnRecorder) return; window.__hrafnRecorder = true;
  const esc = (s) => (window.CSS && CSS.escape) ? CSS.escape(s) : s.replace(/"/g, '\\"');
  const sel = (e) => {
    if (!(e instanceof Element)) return null;
    if (e.id) return '#' + esc(e.id);
    for (const a of ['data-testid', 'name', 'aria-label']) {
      const v = e.getAttribute(a);
      if (v) return e.tagName.toLowerCase() + '[' + a + '="' + v.replace(/"/g, '\\"') + '"]';
    }
    const path = []; let n = e;
    while (n && n.nodeType === 1 && path.length < 6) {
      if (n.id) { path.unshift('#' + esc(n.id)); break; }
      let s = n.tagName.toLowerCase(); const p = n.parentElement;
      if (p) { const same = [...p.children].filter((c) => c.tagName === n.tagName);
        if (same.length > 1) s += ':nth-of-type(' + (same.indexOf(n) + 1) + ')'; }
      path.unshift(s); n = p;
    }
    return path.join(' > ');
  };
  const send = (o) => { try { __hrafnRecord(JSON.stringify(o)); } catch (_) {} };
  const textLike = (t) => t && (t.tagName === 'TEXTAREA' || t.isContentEditable ||
    (t.tagName === 'INPUT' && !['checkbox', 'radio', 'button', 'submit', 'reset', 'file', 'image'].includes(t.type)));
  addEventListener('click', (e) => { if (e.detail <= 1) send({ type: 'click', target: sel(e.target) }); }, true);
  addEventListener('dblclick', (e) => send({ type: 'dblclick', target: sel(e.target) }), true);
  addEventListener('input', (e) => { const t = e.target;
    if (textLike(t)) send({ type: 'type', target: sel(t), text: t.isContentEditable ? t.textContent : t.value }); }, true);
  addEventListener('change', (e) => { const t = e.target;
    if (t.tagName === 'SELECT') send({ type: 'select', target: sel(t), value: t.value }); }, true);
  addEventListener('keydown', (e) => {
    if (['Enter', 'Escape', 'Tab'].includes(e.key) && !e.ctrlKey && !e.altKey && !e.metaKey) send({ type: 'keys', combo: e.key });
  }, true);
})()"#;

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
pub fn start(page_action: Function) {
    with(|s| s.page_action = Some(page_action));
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
                    s.recordings.remove(&t);
                    s.overrides.remove(&t);
                    s.headers.remove(&t);
                    s.last_tree.remove(&t);
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
            "Network.eventSourceMessageReceived" => {
                let url = log.net.iter().rev().find(|e| e["id"] == p["requestId"]).map(|e| e["url"].clone()).unwrap_or(Value::Null);
                push(&mut log.sse, json!({"ts": t, "id": p["requestId"], "url": url, "event": p["eventName"], "data": p["data"]}));
            }
            "Page.frameNavigated" if p["frame"].get("parentId").is_none() => {
                s.refs.remove(&tab);
                if let Some(rec) = s.recordings.get_mut(&tab) {
                    rec.steps.push(json!({"type": "navigate", "url": p["frame"]["url"]}));
                }
            }
            "Runtime.bindingCalled" if p["name"] == "__hrafnRecord" => {
                if let (Some(rec), Some(step)) = (
                    s.recordings.get_mut(&tab),
                    p["payload"].as_str().and_then(|j| serde_json::from_str::<Value>(j).ok()),
                ) {
                    crate::record::record_step(&mut rec.steps, step);
                }
            }
            _ => {}
        }
    });
    if method == "Fetch.requestPaused" {
        spawn_local(answer_paused(tab, p));
    }
}

/// Answer an intercepted request with its override, or let it through.
async fn answer_paused(tab: i64, p: Value) {
    use base64::Engine;
    let url = p["request"]["url"].as_str().unwrap_or("").to_string();
    let id = p["requestId"].clone();
    let hit = with(|s| s.overrides.get(&tab).and_then(|list| list.iter().find(|o| crate::glob::matches(&o.pattern, &url)).cloned()));
    let _ = match hit {
        Some(o) => {
            let body = base64::engine::general_purpose::STANDARD.encode(o.body.as_bytes());
            cdp_raw(
                tab,
                "Fetch.fulfillRequest",
                json!({"requestId": id, "responseCode": o.status,
                       "responseHeaders": [{"name": "Content-Type", "value": o.content_type}, {"name": "Access-Control-Allow-Origin", "value": "*"}],
                       "body": body}),
            )
            .await
        }
        None => cdp_raw(tab, "Fetch.continueRequest", json!({"requestId": id})).await,
    };
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
    // Right after a click starts a cross-site navigation the browser can refuse to
    // attach for a moment, so retry briefly before giving up.
    let mut last_err = String::new();
    let mut attached = false;
    for attempt in 0..8 {
        if attempt > 0 {
            sleep(300).await;
        }
        match call(&["chrome", "debugger"], "attach", vec![to_js(&json!({"tabId": tab})), "1.3".into()]).await {
            Ok(_) => {
                attached = true;
                break;
            }
            Err(e) if e.contains("Already attached") || e.contains("already attached") => {
                attached = true;
                break;
            }
            Err(e) if e == EXTENSION_FRAME_DENIED => return Err(format!("{e}. {PAGE_FALLBACK_NOTICE}")),
            Err(e) => last_err = e,
        }
    }
    if !attached {
        return Err(format!(
            "could not attach to tab {tab}: {last_err} (close DevTools on that tab if it is open; another extension's frame in the page can also block this)"
        ));
    }
    with(|s| {
        s.attached.insert(tab);
        s.logs.entry(tab).or_default();
    });
    for domain in ["Page.enable", "Runtime.enable", "Network.enable", "Log.enable", "DOM.enable"] {
        cdp_raw(tab, domain, json!({})).await?;
    }
    apply_tab_settings(tab).await
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
    mouse_btn(tab, kind, x, y, "left", 1).await
}

async fn mouse_btn(tab: i64, kind: &str, x: f64, y: f64, button: &str, count: u32) -> Result<(), String> {
    let mut p = json!({"type": kind, "x": x, "y": y});
    if kind != "mouseMoved" {
        p["button"] = json!(button);
        p["clickCount"] = json!(count);
    } else if button != "none" && count > 0 {
        // A move while a button is held (dragging).
        p["button"] = json!(button);
        p["buttons"] = json!(1);
    }
    cdp(tab, "Input.dispatchMouseEvent", p).await.map(|_| ())
}

/// A point to act on: `at: [x, y]`, else the centre of `target` (ref or selector).
async fn point(r: &Request, tab: i64, target_key: &str, at_key: &str) -> Result<(f64, f64, String), String> {
    if let Some(at) = arg(r, at_key).as_array() {
        let x = at.first().and_then(Value::as_f64).ok_or("bad x")?;
        let y = at.get(1).and_then(Value::as_f64).ok_or("bad y")?;
        return Ok((x, y, format!("{x},{y}")));
    }
    let target = arg_str(r, target_key)?;
    let b = resolve(tab, &target).await?;
    let (x, y) = center(tab, b).await?;
    Ok((x, y, target))
}

/// Re-apply per-tab settings after (re)attaching: headers, overrides, recorder.
async fn apply_tab_settings(tab: i64) -> Result<(), String> {
    let (headers, overrides, recording) = with(|s| {
        (
            s.headers.get(&tab).cloned(),
            s.overrides.get(&tab).map(|v| v.iter().map(|o| o.pattern.clone()).collect::<Vec<_>>()),
            s.recordings.contains_key(&tab),
        )
    });
    if let Some(h) = headers {
        cdp_raw(tab, "Network.setExtraHTTPHeaders", json!({"headers": h})).await?;
    }
    match overrides {
        Some(patterns) if !patterns.is_empty() => {
            let p: Vec<Value> = patterns.iter().map(|u| json!({"urlPattern": u, "requestStage": "Request"})).collect();
            cdp_raw(tab, "Fetch.enable", json!({"patterns": p})).await?;
        }
        _ => {
            let _ = cdp_raw(tab, "Fetch.disable", json!({})).await;
        }
    }
    if recording {
        install_recorder(tab).await?;
    }
    Ok(())
}

async fn install_recorder(tab: i64) -> Result<(), String> {
    cdp_raw(tab, "Runtime.addBinding", json!({"name": "__hrafnRecord"})).await?;
    let r = cdp_raw(tab, "Page.addScriptToEvaluateOnNewDocument", json!({"source": RECORDER_JS})).await?;
    let id = r["identifier"].as_str().map(String::from);
    with(|s| {
        if let Some(rec) = s.recordings.get_mut(&tab) {
            rec.script_id = id;
        }
    });
    cdp_raw(tab, "Runtime.evaluate", json!({"expression": RECORDER_JS})).await?;
    Ok(())
}

/// JSON string literal, for putting a value into a small JS expression safely.
fn js_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
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
        "current_tab" => {
            let tab = target_tab(r).await?;
            tab_info(tab).await
        }
        _ => {
            let tab = target_tab(r).await?;
            page_command(r, tab).await
        }
    }
}

fn bounded_text(value: &Value, max: usize) -> String {
    let text = value.as_str().unwrap_or("");
    if text.len() <= max {
        return text.to_string();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n… (truncated)", &text[..cut])
}

async fn main_page_action(r: &Request, tab: i64, url: &str, command: &str) -> Result<Value, String> {
    let action = with(|s| s.page_action.clone()).ok_or("main-page actions are unavailable")?;
    let options = to_js(&json!({
        "target": {"tabId": tab, "frameIds": [0]},
        "world": "ISOLATED",
        "args": [url, command, r.args],
    }));
    Reflect::set(&options, &"func".into(), action.as_ref()).map_err(js::err_text)?;
    let result = from_js(call(&["chrome", "scripting"], "executeScript", vec![options]).await?);
    if let Some(error) = result[0]["result"]["error"].as_str() {
        return Err(error.to_string());
    }
    result[0]["result"].get("value").cloned().ok_or_else(|| "the main page did not return a result".into())
}

async fn page_fallback(r: &Request, tab: i64) -> Result<Value, String> {
    let info = tab_info(tab).await?;
    let url = info["url"].as_str().ok_or("the tab has no page URL")?;
    match r.cmd.as_str() {
        "read" => {
            let mode = arg(r, "mode").as_str().unwrap_or("both");
            if mode == "tree" {
                return Err(PAGE_FALLBACK_NOTICE.into());
            }
            let max = arg(r, "max_chars").as_u64().map(|m| m as usize).unwrap_or(DEFAULT_MAX_CHARS);
            let text = main_page_action(r, tab, url, "read").await?;
            let mut out = json!({"text": bounded_text(&text, max)});
            if mode != "text" {
                out["notice"] = json!(PAGE_FALLBACK_NOTICE);
            }
            Ok(out)
        }
        "wait_for" => wait_for(r, tab, Some(url)).await,
        "html" => main_page_action(r, tab, url, "html").await,
        "click" => {
            if arg(r, "at").is_array() || arg(r, "button").as_str().unwrap_or("left") != "left" || arg(r, "count").as_u64().unwrap_or(1) != 1 {
                return Err(format!("{PAGE_FALLBACK_NOTICE} Only a single click by CSS selector is available."));
            }
            let target = arg_str(r, "target")?;
            main_page_action(r, tab, url, "click").await?;
            sleep(300).await;
            let info = tab_info(tab).await?;
            Ok(json!(format!("clicked {target} — now at {}", info["url"].as_str().unwrap_or(""))))
        }
        "type" => {
            let target = arg_str(r, "target")?;
            let text = arg_str(r, "text")?;
            main_page_action(r, tab, url, "type").await?;
            Ok(json!(format!("typed {} characters into {target}", text.chars().count())))
        }
        "focus" => {
            main_page_action(r, tab, url, "focus").await?;
            Ok(json!("focused"))
        }
        "select" => {
            let value = main_page_action(r, tab, url, "select").await?;
            Ok(json!(format!("selected {}", value.as_str().unwrap_or(""))))
        }
        _ => Err(PAGE_FALLBACK_NOTICE.into()),
    }
}

async fn wait_for(r: &Request, tab: i64, fallback_url: Option<&str>) -> Result<Value, String> {
    let timeout = arg(r, "timeout_ms").as_f64().unwrap_or(15_000.0);
    let (expr, what) = match (arg(r, "selector").as_str(), arg(r, "text").as_str()) {
        (Some(sel), _) => (format!("!!document.querySelector({})", js_str(sel)), format!("`{sel}`")),
        (None, Some(text)) => (
            format!("!!(document.body && document.body.innerText.includes({}))", js_str(text)),
            format!("\"{text}\""),
        ),
        _ => return Err("give a selector or --text".into()),
    };
    let gone = arg(r, "gone").as_bool().unwrap_or(false);
    let end = now() + timeout;
    loop {
        let result = match fallback_url {
            Some(url) => main_page_action(r, tab, url, "probe").await?,
            None => eval(tab, &expr).await?,
        };
        if result.as_bool() == Some(!gone) {
            return Ok(json!(format!("{what} {}", if gone { "is gone" } else { "is there" })));
        }
        if now() > end {
            return Err(format!("timed out waiting for {what}{}", if gone { " to go away" } else { "" }));
        }
        sleep(150).await;
    }
}

async fn page_command(r: &Request, tab: i64) -> Result<Value, String> {
    if matches!(r.cmd.as_str(), "wait_for" | "read" | "html" | "click" | "focus" | "type" | "select") {
        match attach(tab).await {
            Err(e) if e.starts_with(EXTENSION_FRAME_DENIED) => return page_fallback(r, tab).await,
            Err(e) => return Err(e),
            Ok(()) => {}
        }
    }
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
                out["text"] = json!(bounded_text(&text, max));
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
            let (x, y, what) = point(r, tab, "target", "at").await?;
            mouse(tab, "mouseMoved", x, y).await?;
            let mut verb = "hovering over";
            if r.cmd == "click" {
                let button = arg(r, "button").as_str().unwrap_or("left").to_string();
                let count = arg(r, "count").as_u64().unwrap_or(1).clamp(1, 3) as u32;
                for n in 1..=count {
                    mouse_btn(tab, "mousePressed", x, y, &button, n).await?;
                    mouse_btn(tab, "mouseReleased", x, y, &button, n).await?;
                }
                sleep(300).await;
                verb = match (button.as_str(), count) {
                    ("right", _) => "right-clicked",
                    (_, 2) => "double-clicked",
                    (_, 3) => "triple-clicked",
                    _ => "clicked",
                };
            }
            let info = tab_info(tab).await?;
            Ok(json!(format!("{verb} {what} — now at {}", info["url"].as_str().unwrap_or(""))))
        }
        "drag" => {
            let (x1, y1, from) = point(r, tab, "from", "from_at").await?;
            let (x2, y2, to) = point(r, tab, "to", "to_at").await?;
            let steps = arg(r, "steps").as_u64().unwrap_or(12).clamp(1, 200);
            mouse(tab, "mouseMoved", x1, y1).await?;
            mouse_btn(tab, "mousePressed", x1, y1, "left", 1).await?;
            for i in 1..=steps {
                let f = i as f64 / steps as f64;
                mouse_btn(tab, "mouseMoved", x1 + (x2 - x1) * f, y1 + (y2 - y1) * f, "left", 1).await?;
            }
            mouse_btn(tab, "mouseReleased", x2, y2, "left", 1).await?;
            sleep(200).await;
            Ok(json!(format!("dragged {from} to {to}")))
        }
        "wait_for" => wait_for(r, tab, None).await,
        "diff" => {
            let tree = read_tree(tab, None, usize::MAX).await?.tree;
            let before = with(|s| s.last_tree.insert(tab, tree.clone()));
            let Some(before) = before else {
                return Ok(json!("(first read of this tab; run diff again after the page changes)"));
            };
            // Refs are renumbered on every read, so compare lines without them.
            let strip = |l: &str| match l.rfind(" [e") {
                Some(i) if l.ends_with(']') => l[..i].to_string(),
                _ => l.to_string(),
            };
            let old: Vec<String> = before.lines().map(strip).collect();
            let new: Vec<String> = tree.lines().map(strip).collect();
            let mut out = String::new();
            for l in old.iter().filter(|l| !new.contains(l)) {
                out.push_str(&format!("- {}\n", l.trim_start()));
            }
            for (l, raw) in new.iter().zip(tree.lines()) {
                if !old.contains(l) {
                    out.push_str(&format!("+ {}\n", raw.trim_start()));
                }
            }
            Ok(json!(if out.is_empty() { "no changes".to_string() } else { out }))
        }
        "style" => match arg(r, "action").as_str().unwrap_or("add") {
            "add" => {
                let css = arg_str(r, "css")?;
                let id = with(|s| {
                    s.style_seq += 1;
                    format!("s{}", s.style_seq)
                });
                eval(
                    tab,
                    &format!(
                        "(() => {{ const s = document.createElement('style'); s.dataset.hrafn = {}; s.textContent = {}; document.documentElement.appendChild(s); }})()",
                        js_str(&id),
                        js_str(&css)
                    ),
                )
                .await?;
                Ok(json!(format!("added style {id} (remove with: hrafn style remove {id})")))
            }
            "remove" => {
                let id = arg_str(r, "id")?;
                let n = eval(tab, &format!("(() => {{ const s = document.querySelector('style[data-hrafn=' + JSON.stringify({}) + ']'); if (s) s.remove(); return !!s; }})()", js_str(&id))).await?;
                if n == Value::Bool(true) { Ok(json!(format!("removed style {id}"))) } else { Err(format!("no style {id} on this page")) }
            }
            _ => {
                let n = eval(tab, "(() => { const all = document.querySelectorAll('style[data-hrafn]'); all.forEach((s) => s.remove()); return all.length; })()").await?;
                Ok(json!(format!("removed {n} style(s)")))
            }
        },
        "headers" => {
            attach(tab).await?;
            let msg = with(|s| {
                let h = s.headers.entry(tab).or_default();
                match arg(r, "action").as_str().unwrap_or("list") {
                    "add" => {
                        let (Some(n), Some(v)) = (arg(r, "name").as_str(), arg(r, "value").as_str()) else {
                            return Err("usage: headers add <name> <value>".to_string());
                        };
                        h.insert(n.to_string(), json!(v));
                        Ok(format!("every request from this tab now sends {n}"))
                    }
                    "remove" => {
                        let n = arg(r, "name").as_str().unwrap_or("");
                        Ok(if h.remove(n).is_some() { format!("removed {n}") } else { format!("{n} was not set") })
                    }
                    "clear" => {
                        h.clear();
                        Ok("cleared extra headers".into())
                    }
                    _ => Ok(if h.is_empty() {
                        "no extra headers".into()
                    } else {
                        h.iter().map(|(k, v)| format!("{k}: {}", v.as_str().unwrap_or(""))).collect::<Vec<_>>().join("\n")
                    }),
                }
            })?;
            apply_tab_settings(tab).await?;
            Ok(json!(msg))
        }
        "override" => {
            attach(tab).await?;
            let msg = with(|s| {
                let list = s.overrides.entry(tab).or_default();
                match arg(r, "action").as_str().unwrap_or("list") {
                    "add" => {
                        let pattern = arg(r, "pattern").as_str().unwrap_or("").to_string();
                        if pattern.is_empty() {
                            return Err("give a URL pattern, e.g. '*/api/items*'".to_string());
                        }
                        list.retain(|o| o.pattern != pattern);
                        list.push(Override {
                            pattern: pattern.clone(),
                            status: arg(r, "status").as_u64().unwrap_or(200) as u16,
                            content_type: arg(r, "content_type").as_str().unwrap_or("application/json").to_string(),
                            body: arg(r, "body").as_str().unwrap_or("").to_string(),
                        });
                        Ok(format!("requests matching {pattern} now get the fake response"))
                    }
                    "clear" => {
                        list.clear();
                        Ok("removed all overrides".into())
                    }
                    _ => Ok(if list.is_empty() {
                        "no overrides".into()
                    } else {
                        list.iter().map(|o| format!("{}  -> {} {} ({} bytes)", o.pattern, o.status, o.content_type, o.body.len())).collect::<Vec<_>>().join("\n")
                    }),
                }
            })?;
            apply_tab_settings(tab).await?;
            Ok(json!(msg))
        }
        "record" => match arg(r, "action").as_str().unwrap_or("status") {
            "start" => {
                attach(tab).await?;
                let url = tab_info(tab).await?["url"].as_str().unwrap_or("").to_string();
                with(|s| s.recordings.insert(tab, Recording { start_url: url.clone(), ..Default::default() }));
                install_recorder(tab).await?;
                Ok(json!(format!("recording tab {tab} from {url}; stop with: hrafn record stop [plan-file]")))
            }
            "stop" => {
                let rec = with(|s| s.recordings.remove(&tab)).ok_or("this tab is not being recorded")?;
                if let Some(id) = &rec.script_id {
                    let _ = cdp_raw(tab, "Page.removeScriptToEvaluateOnNewDocument", json!({"identifier": id})).await;
                }
                let _ = cdp_raw(tab, "Runtime.removeBinding", json!({"name": "__hrafnRecord"})).await;
                Ok(json!({"start_url": rec.start_url, "steps": rec.steps}))
            }
            _ => Ok(json!(match with(|s| s.recordings.get(&tab).map(|r| r.steps.len())) {
                Some(n) => format!("recording tab {tab}: {n} step(s) so far"),
                None => "not recording".to_string(),
            })),
        },
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
        "upload" => {
            // Put files into an <input type=file> (the browser reads them itself, so
            // they must be somewhere the browser, sandboxed or not, can see).
            let target = arg_str(r, "target")?;
            let files: Vec<String> = arg(r, "files").as_array().into_iter().flatten().filter_map(|f| f.as_str().map(String::from)).collect();
            if files.is_empty() {
                return Err("give at least one file".into());
            }
            let b = resolve(tab, &target).await?;
            cdp(tab, "DOM.setFileInputFiles", json!({"files": files, "backendNodeId": b}))
                .await
                .map_err(|e| format!("{e} (is {target} an <input type=file>?)"))?;
            Ok(json!(format!("attached {} file(s) to {target}", files.len())))
        }
        "insert_text" => {
            // Type at the page's current cursor/focus (no target element).
            let text = arg_str(r, "text")?;
            cdp(tab, "Input.insertText", json!({"text": text})).await?;
            Ok(json!(format!("typed {} characters", text.chars().count())))
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
            // Where the image's top-left sits in the viewport (CSS px), so callers
            // can turn image positions into click positions.
            let (mut origin_x, mut origin_y) = (0.0, 0.0);
            if let Some(target) = arg(r, "target").as_str() {
                let b = resolve(tab, target).await?;
                let _ = cdp(tab, "DOM.scrollIntoViewIfNeeded", json!({"backendNodeId": b})).await;
                let q = cdp(tab, "DOM.getContentQuads", json!({"backendNodeId": b})).await?;
                let quad: Vec<f64> = q["quads"][0].as_array().ok_or("element is not visible")?.iter().filter_map(Value::as_f64).collect();
                if quad.len() != 8 {
                    return Err("element is not visible".into());
                }
                let xs = [quad[0], quad[2], quad[4], quad[6]];
                let ys = [quad[1], quad[3], quad[5], quad[7]];
                let (x0, x1) = (xs.iter().cloned().fold(f64::MAX, f64::min), xs.iter().cloned().fold(f64::MIN, f64::max));
                let (y0, y1) = (ys.iter().cloned().fold(f64::MAX, f64::min), ys.iter().cloned().fold(f64::MIN, f64::max));
                // Clip coordinates are page coordinates: viewport position + scroll.
                let scroll = eval(tab, "[window.scrollX, window.scrollY]").await?;
                let (sx, sy) = (scroll[0].as_f64().unwrap_or(0.0), scroll[1].as_f64().unwrap_or(0.0));
                p["captureBeyondViewport"] = json!(true);
                p["clip"] = json!({"x": x0 + sx, "y": y0 + sy, "width": (x1 - x0).max(1.0), "height": (y1 - y0).max(1.0), "scale": 1});
                (origin_x, origin_y) = (x0, y0);
            } else if arg(r, "full").as_bool().unwrap_or(false) {
                let m = cdp(tab, "Page.getLayoutMetrics", json!({})).await?;
                let size = &m["cssContentSize"];
                p["captureBeyondViewport"] = json!(true);
                p["clip"] = json!({"x": 0, "y": 0, "width": size["width"], "height": size["height"], "scale": 1});
            }
            let shot = cdp(tab, "Page.captureScreenshot", p).await?;
            let dpr = eval(tab, "window.devicePixelRatio").await?.as_f64().unwrap_or(1.0);
            Ok(json!({"data": shot["data"], "dpr": dpr, "origin": [origin_x, origin_y]}))
        }
        "eval" => eval(tab, &arg_str(r, "code")?).await,
        "net" if arg(r, "action") == "body" => {
            let id = arg_str(r, "id")?;
            let b = cdp(tab, "Network.getResponseBody", json!({"requestId": id}))
                .await
                .map_err(|e| format!("{e} (bodies are only kept for recent requests on an attached tab)"))?;
            Ok(json!({"body": b["body"], "base64": b["base64Encoded"]}))
        }
        "console" | "net" | "sse" => {
            attach(tab).await?;
            let limit = arg(r, "limit").as_u64().unwrap_or(50) as usize;
            let filter = arg(r, "filter").as_str().map(str::to_lowercase);
            let clear = arg(r, "clear").as_bool().unwrap_or(false) || arg(r, "action") == "clear";
            Ok(with(|s| {
                let log = s.logs.entry(tab).or_default();
                let q = match r.cmd.as_str() {
                    "console" => &mut log.console,
                    "sse" => &mut log.sse,
                    _ => &mut log.net,
                };
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
