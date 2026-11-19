//! `hrafn`: drive your real Brave (or Chrome) from the terminal.
//!
//! One binary, two roles:
//! - Brave starts it as the native messaging host (first argument is the
//!   extension origin, `chrome-extension://<id>/`). It then relays commands between
//!   a local unix socket and the Hrafn extension.
//! - Run by you, it is the command line client that talks to that socket.

mod cli;
mod host;
mod paths;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = if args.first().is_some_and(|a| a.starts_with("chrome-extension://")) {
        host::run(&args[0])
    } else {
        cli::run(args)
    };
    std::process::exit(code);
}
