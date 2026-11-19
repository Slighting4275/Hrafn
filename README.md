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
hrafn (CLI) ──unix socket──▶ hrafn-host ──native messaging──▶ extension (Rust/WASM)
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
`/usr/local/libexec/hrafn/hrafn-host` (one `sudo install` step). It is a small
separate program that rarely changes, so updating the CLI or the extension does
not need root again. Its socket and log
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
hrafn read [ref] [--tree-only|--text-only] [--max-chars N] | tree | text | diff
hrafn find <words> [--role ROLE] | html <target>
hrafn click <target> [--double|--triple|--right] | click --at X,Y
hrafn dblclick|rightclick|hover|focus <target> | drag <from> <to> [--steps N]
hrafn type <target> <text> [--append] | select <target> <value>
hrafn keys <combo> | scroll <up|down|top|bottom>
hrafn wait <ms> | wait-stable [--ms N] [--timeout N]
hrafn wait-for <selector> | wait-for --text TEXT  [--gone] [--timeout MS]
hrafn screenshot [file.png] [--full] | eval <javascript>
hrafn record start | record stop [plan-file] | record status | run <plan-file|->
hrafn console [--limit N] [--clear] | sse [--filter TEXT] | sse clear
hrafn net log [--filter TEXT] [--limit N] | net body <id> [--out FILE] | net clear
hrafn headers [list] | headers add <name> <value> | headers remove <name> | headers clear
hrafn override [list] | override add <url-pattern> [--status N] [--type MIME]
               [--body TEXT | --file PATH] | override clear
hrafn style add '<css>' | style remove <id> | style clear
hrafn cookies <domain> | cookies set '<json>' | cookies delete <url> <name>
hrafn detach | doctor | help
```

### Recording and replay

`hrafn record start` records your clicks, typing, dropdown choices and
Enter/Tab/Escape presses on the current tab. `hrafn record stop flow.txt` saves
them as a plan: one hrafn command per line, editable, `#` for comments.
`hrafn run flow.txt` replays it, waiting for the page to settle after each
step, and prints ok/FAIL per line (it stops at the first failure unless
`--keep-going`). Plans can also be written by hand. The recorder is the one
other piece of JavaScript: it has to run inside the page to see your clicks,
and only reports them to the Rust side.

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
