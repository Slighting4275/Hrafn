//! Thin helpers for calling the browser's `chrome.*` APIs from Rust.

use js_sys::{Array, Function, Promise, Reflect};
use serde::Serialize;
use serde_json::Value;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

pub fn global() -> JsValue {
    js_sys::global().into()
}

pub fn get(obj: &JsValue, key: &str) -> JsValue {
    Reflect::get(obj, &JsValue::from_str(key)).unwrap_or(JsValue::UNDEFINED)
}

/// `path(&["chrome", "tabs"])` is `globalThis.chrome.tabs`.
pub fn path(p: &[&str]) -> JsValue {
    p.iter().fold(global(), |o, k| if o.is_undefined() { o } else { get(&o, k) })
}

pub fn err_text(e: JsValue) -> String {
    if let Some(s) = e.as_string() {
        return s;
    }
    get(&e, "message").as_string().unwrap_or_else(|| format!("{e:?}"))
}

/// Call `<path>.<method>(...args)` and await it if it returns a promise.
pub async fn call(p: &[&str], method: &str, args: Vec<JsValue>) -> Result<JsValue, String> {
    let obj = path(p);
    let f: Function = get(&obj, method)
        .dyn_into()
        .map_err(|_| format!("{}.{method} is not available in this browser", p.join(".")))?;
    let arr: Array = args.into_iter().collect();
    let ret = f.apply(&obj, &arr).map_err(err_text)?;
    if ret.is_instance_of::<Promise>() {
        JsFuture::from(Promise::from(ret)).await.map_err(err_text)
    } else {
        Ok(ret)
    }
}

pub fn to_js(v: &Value) -> JsValue {
    v.serialize(&serde_wasm_bindgen::Serializer::json_compatible()).unwrap_or(JsValue::NULL)
}

pub fn from_js(v: JsValue) -> Value {
    if v.is_undefined() || v.is_null() {
        return Value::Null;
    }
    serde_wasm_bindgen::from_value(v).unwrap_or(Value::Null)
}

/// `<path>.addListener(f)`; `f` is a leaked closure (listeners live as long as the worker).
pub fn listen(p: &[&str], f: JsValue) {
    listen_on(&path(p), f);
}

/// `event.addListener(f)` for an event object you already hold (e.g. `port.onMessage`).
pub fn listen_on(ev: &JsValue, f: JsValue) {
    if let Ok(add) = get(ev, "addListener").dyn_into::<Function>() {
        let _ = add.call1(ev, &f);
    }
}

/// Synchronous call of `obj.method(arg)`, for the few APIs that don't return promises.
pub fn call_sync(obj: &JsValue, method: &str, arg: &JsValue) -> Option<JsValue> {
    let f: Function = get(obj, method).dyn_into().ok()?;
    f.call1(obj, arg).ok()
}

pub async fn sleep(ms: i32) {
    let p = Promise::new(&mut |resolve, _| {
        if let Ok(st) = get(&global(), "setTimeout").dyn_into::<Function>() {
            let _ = st.call2(&JsValue::NULL, &resolve, &JsValue::from(ms));
        }
    });
    let _ = JsFuture::from(p).await;
}

pub fn now() -> f64 {
    js_sys::Date::now()
}
