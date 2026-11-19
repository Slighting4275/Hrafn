// Brave only starts extensions from JavaScript, so this file just loads the Rust
// code (hrafn_ext_bg.wasm) and hands over. All logic lives in crates/hrafn-ext.
// Listeners registered here, synchronously, are what wake the worker on browser
// start and on the keepalive alarm; the Rust side adds the real handlers.
import init, { start } from "./hrafn_ext.js";
chrome.runtime.onStartup.addListener(() => {});
chrome.runtime.onInstalled.addListener(() => {});
chrome.alarms.onAlarm.addListener(() => {});
init().then(start);
