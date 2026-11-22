# Hrafn

Drive your real Brave (or Chrome) from the terminal: open pages, read them,
click, type, take screenshots, and watch console and network activity. Your own
browser, logins and profile, nothing sent anywhere.

The command line tool and native host are Rust binaries. The browser extension
runs Rust compiled to WebAssembly. JavaScript starts the extension and runs
the recorder and basic page actions in the browser.

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

If another extension embeds a frame that blocks debugger attachment, Hrafn can
still read the web page's text and use CSS selectors for HTML, click, focus,
type, select, and wait-for. These actions work in the main page. Fallback clicks
and input events are synthetic. Accessibility refs, arbitrary JavaScript, and
network inspection still need the debugger.
If the debugger drops a reply during a click, Hrafn can confirm a matching
browser click in the main page. An unconfirmed click should be checked before
repeating the command.

`wait-stable` waits for the document to finish loading and for page requests to
go quiet. Browser-extension resources and open event streams remain visible in
the network log. Use `wait-for` with destination text or a selector when a click
starts a navigation or schedules an update.

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
hrafn upload <file-input> <file>...   (files in a folder the browser can see, e.g. ~/Downloads)
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
hrafn ocr [--target T] [--full] [--boxes] | ocr <image-file>
hrafn ocr --find TEXT | ocr --click TEXT
hrafn cookies <domain> | cookies set '<json>' | cookies delete <url> <name>
hrafn detach | doctor | help
```

### Sites: ChatGPT, LinkedIn, Google Docs

These work in your own signed-in browser session and are built from the generic
commands, so the sites see a normal browser. Their page structure changes often;
each reader has several fallbacks, and an empty result says so instead of
guessing.

```
hrafn chatgpt status | send <prompt> [--stream] [--timeout S] | read [--all]
hrafn chatgpt conversations | switch <id> | new | stop | model [name]
hrafn linkedin job|event|post|profile [url] | attendees [url] [--max N]
hrafn gdocs info | text | export <format> <file> | insert <text> [--end]
hrafn gdocs slides | slide <n> | notes
```

- `chatgpt send` types the prompt, submits it and waits until the answer has
  finished and stopped changing (`--stream` prints it as it arrives; `-` reads
  the prompt from stdin).
- `linkedin` reads the page you have open (or opens the URL you give). It never
  opens LinkedIn on its own. Automated use can break LinkedIn's terms; keep it to
  pages you would read anyway. `attendees` stops at 100 people unless `--max`.
- `gdocs text`/`export` use Google's own export links inside your session
  (Docs: txt, md, pdf, docx, odt, rtf, html, epub; Sheets: csv, tsv, xlsx, ods,
  pdf; Slides: txt, pdf, pptx, odp). Docs draws text on a canvas, so this is the
  reliable way to read it; `hrafn ocr` reads what is on screen.

### Reading text from pixels (OCR)

`hrafn ocr` reads the text that is actually drawn on screen, including text
inside images and canvas apps where the page has no readable text. It uses
[ocrs](https://github.com/robertknight/ocrs), a pure-Rust OCR engine, so nothing
leaves your computer. The two model files (about 12 MB) are downloaded once from
the ocrs project into `~/.local/share/hrafn/models` and checked against pinned
SHA-256 hashes; a damaged file is downloaded again.

`--boxes` prints each line with its centre as `X,Y` (viewport coordinates, the
same ones `click --at` takes). `--find TEXT` prints where TEXT is, and
`--click TEXT` clicks it. Matching ignores case and spacing and allows about one
misread character in five, because OCR output is never perfect.
`hrafn ocr image.png` works on any image file without a browser.

### Recording and replay

`hrafn record start` records your clicks, typing, dropdown choices and
Enter/Tab/Escape presses on the current tab. `hrafn record stop flow.txt` saves
them as a plan: one hrafn command per line, editable, `#` for comments.
`hrafn run flow.txt` replays it, waiting for the page to settle after each
step, and prints ok/FAIL per line (it stops at the first failure unless
`--keep-going`). On tabs with a blocked debugger, it waits for the main page to
load and its DOM to stop changing. Use `wait-for` when the next step needs a
specific result. Hand-written plans accept matching single quotes at argument
boundaries and double quotes with escapes. Bare apostrophes such as in `'tis`
and `'tain't` are literal. `hrafn run - --quiet` reads a plan from
stdin and prints only failed line numbers and a summary. Use it when plan
arguments or results should stay out of terminal output.

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
