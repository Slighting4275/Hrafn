# Hrafn

Drive your real Brave (or Chrome) from the terminal: open pages, read them,
click, type, take screenshots, and watch console and network activity. Your own
browser, logins and profile, nothing sent anywhere.

Everything is Rust. The command line tool and native host are a normal Rust
binary; the browser extension's logic is Rust compiled to WebAssembly. The only
JavaScript is `extension/background.js` (9 lines), because browsers start
extensions from JavaScript, plus the loader `wasm-bindgen` generates.

## How it works

```
hrafn (CLI) ──unix socket──▶ hrafn (native host) ──native messaging──▶ extension (Rust/WASM)
                                                                          │
                                                          chrome.debugger (DevTools Protocol)
                                                                          ▼
                                                                     your tabs
```

The extension controls tabs through the browser's debugger API. While a tab is
attached, Brave shows a "Hrafn started debugging this browser" bar;
`hrafn detach` removes it.

### Sandboxed browsers (firejail)

If your browser runs under firejail, its AppArmor profile only lets it run
programs from system folders, so the browser-side host lives at
`/usr/local/libexec/hrafn/hrafn` (one `sudo install` step). Its socket and log
go in the browser's config folder (`~/.config/BraveSoftware/hrafn/`), which the
sandbox can write and the CLI can reach. Nothing in the sandbox profile is
changed.

## Build and install

Needs Rust with the `wasm32-unknown-unknown` target and `wasm-bindgen-cli`
(same version as the `wasm-bindgen` crate, 0.2.130).

```sh
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.130 --locked
cargo xtask install
```

`install` builds everything, puts `hrafn` in `~/.local/bin`, registers the native
host with Brave and Chrome, copies the extension to `~/Downloads/Hrafn-Extension`
and prints the one `sudo` command for the system host. Then in
`brave://extensions`: turn on Developer mode, click **Load unpacked**, and pick
`Downloads/Hrafn-Extension`. Check with `hrafn doctor`.

## Commands

```
hrafn status | tabs | open <url> | navigate <url> | back | forward | reload
hrafn tab new [url] | tab switch <id> | tab close [id]
hrafn read [ref] [--tree-only|--text-only] [--max-chars N] | tree | text
hrafn find <words> [--role ROLE] | html <target>
hrafn click|hover|focus <target> | type <target> <text> [--append]
hrafn select <target> <value> | keys <combo> | scroll <up|down|top|bottom>
hrafn wait <ms> | wait-stable [--ms N] [--timeout N]
hrafn screenshot [file.png] [--full] | eval <javascript>
hrafn console [--limit N] [--clear] | net log [--filter TEXT] [--limit N] | net clear
hrafn cookies <domain> | cookies set '<json>' | cookies delete <url> <name>
hrafn detach | doctor | help
```

A *target* is a ref from `read` (`e12`) or a CSS selector. Refs reset when the
page changes; run `read` again. Add `--tab ID` to act on a specific tab and
`--json` for raw output.

## Tests

```sh
cargo test --workspace            # unit tests
cargo xtask e2e --unsandboxed     # full run in a throwaway headless Brave
cargo xtask e2e                   # the same, sandboxed like your Brave
```

The end-to-end test uses a temporary profile and a local test site; your own
browser and profile are never touched.

## License

MIT
