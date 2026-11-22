// Brave only starts extensions from JavaScript, so this file just loads the Rust
// code (hrafn_ext_bg.wasm) and hands over.
// Listeners registered here, synchronously, are what wake the worker on browser
// start and on the keepalive alarm; the Rust side adds the real handlers.
import init, { start } from "./hrafn_ext.js";
import { pageAction } from "./page.js";
chrome.runtime.onStartup.addListener(() => {});
chrome.runtime.onInstalled.addListener(() => {});
chrome.alarms.onAlarm.addListener(() => {});
async function boot() {
  await init();
  start(pageAction);
}
void boot();
