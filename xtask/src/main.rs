//! Build, install and test Hrafn. Run with `cargo xtask <command>`.
//!
//!   build    CLI/host binary + extension (Rust -> WASM) into extension/dist
//!   install  build, copy `hrafn` to ~/.local/bin, register the native host with
//!            Brave and Chrome, and copy the extension to ~/Downloads/Hrafn-Extension
//!   e2e      end-to-end test in a throwaway headless Brave (your browser is untouched),
//!            sandboxed exactly like your Brave (needs the system host installed);
//!            `e2e --unsandboxed` runs Brave without its firejail wrapper

mod e2e;

use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is not set"))
}

fn run(cmd: &mut Command) {
    eprintln!("$ {cmd:?}");
    let status = cmd.status().unwrap_or_else(|e| panic!("could not start {cmd:?}: {e}"));
    assert!(status.success(), "command failed: {cmd:?}");
}

/// The extension's public key (fixes its ID, which the native host files allow).
/// It was generated once and lives in the repo; unpacked extensions never need the
/// private half. Changing it would change the extension ID, so a missing key is an
/// error rather than something to regenerate quietly.
fn public_key() -> String {
    let path = root().join("extension/key.pub.b64");
    match std::fs::read_to_string(&path) {
        Ok(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => panic!(
            "{} is missing. Restore it from git (git checkout extension/key.pub.b64); \
             a new key would give the extension a new ID.",
            path.display()
        ),
    }
}

/// Chrome's extension ID: first 128 bits of SHA-256(public key), hex digits 0-f as a-p.
pub fn extension_id() -> String {
    let der = base64::engine::general_purpose::STANDARD.decode(public_key()).expect("key");
    Sha256::digest(der)[..16]
        .iter()
        .flat_map(|b| [b >> 4, b & 15])
        .map(|n| (b'a' + n) as char)
        .collect()
}

pub fn dist() -> PathBuf {
    root().join("extension/dist")
}

/// The command line client.
pub fn cli_binary() -> PathBuf {
    root().join("target/release/hrafn")
}

/// The native messaging host the browser starts.
pub fn host_binary() -> PathBuf {
    root().join("target/release/hrafn-host")
}

pub fn build() {
    let root = root();
    run(Command::new("cargo").current_dir(&root).args(["build", "--release", "-p", "hrafn"]));
    run(Command::new("cargo")
        .current_dir(&root)
        .args(["build", "--release", "-p", "hrafn-ext", "--target", "wasm32-unknown-unknown"]));
    let dist = dist();
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist).unwrap();
    run(Command::new("wasm-bindgen")
        .arg(root.join("target/wasm32-unknown-unknown/release/hrafn_ext.wasm"))
        .args(["--target", "web", "--no-typescript", "--out-dir"])
        .arg(&dist));
    for script in ["background.js", "page.js"] {
        std::fs::copy(root.join("extension").join(script), dist.join(script)).unwrap();
    }
    let version = env!("CARGO_PKG_VERSION");
    let manifest = serde_json::json!({
        "manifest_version": 3,
        "name": "Hrafn",
        "version": version,
        "description": "Lets the hrafn command drive this browser from your terminal. Everything stays on your computer.",
        "key": public_key(),
        "minimum_chrome_version": "116",
        "permissions": ["nativeMessaging", "debugger", "tabs", "cookies", "alarms", "scripting"],
        "host_permissions": ["<all_urls>"],
        "background": { "service_worker": "background.js", "type": "module" },
        "content_security_policy": { "extension_pages": "script-src 'self' 'wasm-unsafe-eval'; object-src 'self'" }
    });
    std::fs::write(dist.join("manifest.json"), serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    eprintln!("extension ID {}  ->  {}", extension_id(), dist.display());
}

/// Native host manifest pointing the browser at `program`.
pub fn host_manifest(program: &Path) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": "com.hrafn.host",
        "description": "Hrafn native host",
        "path": program,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{}/", extension_id())]
    }))
    .unwrap()
}

/// Where the browser-side host must live: sandboxes (firejail's AppArmor profile)
/// only let browsers run programs from system folders like this one.
pub const SYSTEM_HOST: &str = "/usr/local/libexec/hrafn/hrafn-host";

/// Is the installed system host the same program we just built?
pub fn system_host_current() -> bool {
    match (std::fs::read(SYSTEM_HOST), std::fs::read(host_binary())) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

pub fn sudo_install_command() -> String {
    format!("sudo install -D -m 755 {} {SYSTEM_HOST}", host_binary().display())
}

/// Copy then rename, so a running host keeps its old file and nothing is half-written.
pub fn install_program(src: &Path, dst: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let dir = dst.parent().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    let tmp = dir.join(".hrafn.new");
    std::fs::copy(src, &tmp).unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::rename(&tmp, dst).unwrap();
    eprintln!("installed {}", dst.display());
}

fn copy_extension(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        std::fs::copy(entry.path(), dst.join(entry.file_name()))?;
    }
    Ok(())
}

fn install() {
    build();
    let home = home();
    // The command you type.
    install_program(&cli_binary(), &home.join(".local/bin/hrafn"));

    // Register the system host with each browser, and make the socket folder in the
    // browser's config folder (sandboxed browsers can write there; the CLI can reach it).
    for (profile_dir, socket_dir) in [
        (".config/BraveSoftware/Brave-Browser", ".config/BraveSoftware/hrafn"),
        (".config/google-chrome", ".config/google-chrome/hrafn"),
    ] {
        let profile_root = home.join(profile_dir);
        if !profile_root.is_dir() {
            continue;
        }
        let sd = home.join(socket_dir);
        std::fs::create_dir_all(&sd).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&sd, std::fs::Permissions::from_mode(0o700)).unwrap();
        let nm = profile_root.join("NativeMessagingHosts");
        std::fs::create_dir_all(&nm).unwrap();
        let file = nm.join("com.hrafn.host.json");
        std::fs::write(&file, host_manifest(Path::new(SYSTEM_HOST))).unwrap();
        eprintln!("registered {}", file.display());
    }
    if system_host_current() {
        eprintln!("system host {SYSTEM_HOST} is up to date");
    } else {
        eprintln!("\nONE STEP NEEDS ROOT: install the browser-side host where sandboxed browsers may run it:\n\n    {}\n", sudo_install_command());
    }

    // Brave's folder picker only shows XDG folders, so the extension goes to Downloads.
    let copy = home.join("Downloads/Hrafn-Extension");
    copy_extension(&dist(), &copy).unwrap();
    eprintln!("extension copied to {} (load it in brave://extensions with Developer mode on)", copy.display());
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("build") => build(),
        Some("install") => install(),
        Some("e2e") => e2e::run(std::env::args().any(|a| a == "--unsandboxed")),
        Some("id") => println!("{}", extension_id()),
        _ => eprintln!("usage: cargo xtask <build|install|e2e|id>"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copying_extension_updates_package_files_and_preserves_other_files() {
        let work = std::env::temp_dir().join(format!("hrafn-extension-copy-{}", std::process::id()));
        let src = work.join("dist");
        let dst = work.join("install");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::create_dir_all(&dst).unwrap();
        std::fs::write(src.join("manifest.json"), "new package").unwrap();
        std::fs::write(dst.join("manifest.json"), "old package").unwrap();
        std::fs::write(dst.join("notes.txt"), "local notes").unwrap();

        copy_extension(&src, &dst).unwrap();
        assert_eq!(std::fs::read_to_string(dst.join("manifest.json")).unwrap(), "new package");
        assert_eq!(std::fs::read_to_string(dst.join("notes.txt")).unwrap(), "local notes");
        std::fs::remove_dir_all(work).unwrap();
    }
}
