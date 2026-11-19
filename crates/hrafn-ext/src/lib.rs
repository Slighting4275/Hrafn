//! The Hrafn browser extension, written in Rust and compiled to WebAssembly.
//!
//! The extension's service worker (`background.js`, a few lines Brave needs to
//! start anything) loads this module and calls [`start`]. From then on all logic is
//! here: it holds a native messaging port to the `hrafn` host, runs each command it
//! receives against a tab through the browser's debugger API (Chrome DevTools
//! Protocol), and records console and network activity for attached tabs.

pub mod ax;
pub mod keys;

#[cfg(target_arch = "wasm32")]
mod js;
#[cfg(target_arch = "wasm32")]
mod ext;

#[cfg(target_arch = "wasm32")]
pub use ext::start;
