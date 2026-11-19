//! The native messaging host the browser starts (kept separate from the CLI so it
//! rarely changes: it is installed in a root-owned folder sandboxes may run from).

fn main() {
    let origin = std::env::args().nth(1).unwrap_or_default();
    if !origin.starts_with("chrome-extension://") {
        eprintln!("hrafn-host is started by the browser, not by hand. Use `hrafn` instead.");
        std::process::exit(2);
    }
    std::process::exit(hrafn::host::run(&origin));
}
