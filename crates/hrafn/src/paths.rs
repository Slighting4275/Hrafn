//! Where the socket and the host log live.
//!
//! Browsers are often sandboxed (firejail + AppArmor, Flatpak) and only see, and
//! only run programs from, a few places. The host program is therefore installed in
//! a system folder sandboxes allow running from (/usr/local/libexec/hrafn), while its
//! socket and log go in the browser's own config folder, which the sandbox can write
//! and the CLI (outside the sandbox) can reach.

use std::path::{Path, PathBuf};

/// Where the installed host program lives (needs root to install, by design).
pub const SYSTEM_HOST: &str = "/usr/local/libexec/hrafn/hrafn";

/// Per-browser socket folders, relative to $HOME.
pub const SOCKET_DIRS: [(&str, &str); 2] = [("Brave", ".config/BraveSoftware/hrafn"), ("Chrome", ".config/google-chrome/hrafn")];

fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

fn env_socket() -> Option<PathBuf> {
    std::env::var_os("HRAFN_SOCKET").filter(|s| !s.is_empty()).map(PathBuf::from)
}

/// Which browser started us, from the parent process's program path.
fn parent_browser() -> Option<&'static str> {
    let ppid = std::fs::read_to_string("/proc/self/stat").ok().and_then(|s| {
        // Field 4 is the parent pid; field 2 (the name) can contain spaces, so skip past ')'.
        s.rsplit_once(')').and_then(|(_, rest)| rest.split_whitespace().nth(1).map(String::from))
    })?;
    let exe = std::fs::read_link(format!("/proc/{ppid}/exe")).ok()?.to_string_lossy().to_lowercase();
    if exe.contains("brave") {
        Some("Brave")
    } else if exe.contains("chrom") {
        Some("Chrome")
    } else {
        None
    }
}

/// The socket a host listens on.
pub fn host_socket() -> PathBuf {
    if let Some(s) = env_socket() {
        return s;
    }
    let home = home();
    let by_name = |name: &str| SOCKET_DIRS.iter().find(|(n, _)| *n == name).map(|(_, d)| home.join(d).join("hrafn.sock"));
    if let Some(p) = parent_browser().and_then(by_name) {
        return p;
    }
    // Unknown parent: use the first browser folder that exists (in a sandbox, only
    // the browser's own folder is visible).
    SOCKET_DIRS
        .iter()
        .map(|(_, d)| home.join(d))
        .find(|d| d.parent().is_some_and(Path::is_dir))
        .unwrap_or_else(|| home.join(SOCKET_DIRS[0].1))
        .join("hrafn.sock")
}

/// Sockets the CLI tries, in order.
pub fn client_sockets() -> Vec<PathBuf> {
    if let Some(s) = env_socket() {
        return vec![s];
    }
    let home = home();
    SOCKET_DIRS.iter().map(|(_, d)| home.join(d).join("hrafn.sock")).collect()
}

/// Connection log beside a socket: start, stop, errors. Never page content.
pub fn log_for(sock: &Path) -> PathBuf {
    sock.with_extension("log")
}
