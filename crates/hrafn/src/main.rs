//! `hrafn`: drive your real Brave (or Chrome) from the terminal.
//!
//! This is the command line client. The browser starts the separate `hrafn-host`
//! program; this binary still works as a host when started with an extension origin
//! (older installs registered it that way).

mod cli;
mod ocr;
mod plan;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = if args.first().is_some_and(|a| a.starts_with("chrome-extension://")) {
        hrafn::host::run(&args[0])
    } else {
        cli::run(args)
    };
    std::process::exit(code);
}
