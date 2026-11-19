//! Native messaging host. Brave starts this when the extension calls
//! `chrome.runtime.connectNative("com.hrafn.host")` and keeps it alive while the
//! extension holds the port open.

use crate::paths;
use hrafn_proto::{Request, Response, frame};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long one command may take inside the browser (page loads, waits).
const COMMAND_TIMEOUT: Duration = Duration::from_secs(150);

type Pending = Arc<Mutex<HashMap<u64, Sender<Response>>>>;

fn log(msg: &str) {
    let line = format!("{} pid {} {msg}\n", now(), std::process::id());
    let path = paths::log_for(&paths::host_socket());
    // Keep the log small: start over once it passes 256 KiB.
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = f.write_all(line.as_bytes());
    }
}

fn now() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    format!("t={s}")
}

pub fn run(origin: &str) -> i32 {
    let sock = paths::host_socket();
    if let Some(dir) = sock.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // The newest browser connection wins: a stale socket from a crashed host, or
    // one from an older connection, is replaced.
    let _ = std::fs::remove_file(&sock);
    let listener = match UnixListener::bind(&sock) {
        Ok(l) => l,
        Err(e) => {
            log(&format!("cannot listen on {}: {e}", sock.display()));
            return 1;
        }
    };
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    let my_inode = std::fs::metadata(&sock).map(|m| m.ino()).ok();
    log(&format!("started for {origin}, listening on {}", sock.display()));

    let pending: Pending = Arc::default();
    let out = Arc::new(Mutex::new(std::io::stdout()));
    let next_id = Arc::new(AtomicU64::new(1));

    // Browser -> host: route each reply to the client waiting for it.
    {
        let pending = pending.clone();
        let sock = sock.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            loop {
                match frame::read(&mut stdin) {
                    Ok(Some(bytes)) => match serde_json::from_slice::<Response>(&bytes) {
                        Ok(resp) => {
                            if let Some(tx) = pending.lock().unwrap().remove(&resp.id) {
                                let _ = tx.send(resp);
                            }
                        }
                        Err(e) => log(&format!("unreadable message from browser: {e}")),
                    },
                    Ok(None) => {
                        log("browser closed the connection");
                        break;
                    }
                    Err(e) => {
                        log(&format!("read error: {e}"));
                        break;
                    }
                }
            }
            // Only remove the socket if it is still ours (a newer host may own it).
            if std::fs::metadata(&sock).map(|m| m.ino()).ok() == my_inode {
                let _ = std::fs::remove_file(&sock);
            }
            std::process::exit(0);
        });
    }

    for conn in listener.incoming() {
        let Ok(stream) = conn else { continue };
        let (pending, out, next_id) = (pending.clone(), out.clone(), next_id.clone());
        std::thread::spawn(move || serve_client(stream, pending, out, next_id));
    }
    0
}

fn serve_client(stream: UnixStream, pending: Pending, out: Arc<Mutex<std::io::Stdout>>, next_id: Arc<AtomicU64>) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut writer = stream;
    let mut line = String::new();
    while reader.read_line(&mut line).unwrap_or(0) > 0 {
        let resp = match serde_json::from_str::<Request>(line.trim()) {
            Ok(mut req) => {
                let client_id = req.id;
                // Ids are unique per host, not per client, so replies can't cross.
                req.id = next_id.fetch_add(1, Ordering::Relaxed);
                let (tx, rx) = mpsc::channel();
                pending.lock().unwrap().insert(req.id, tx);
                let sent = serde_json::to_vec(&req)
                    .map_err(|e| e.to_string())
                    .and_then(|b| frame::write(&mut *out.lock().unwrap(), &b).map_err(|e| e.to_string()));
                let mut resp = match sent {
                    Err(e) => Response::err(req.id, format!("could not reach the browser: {e}")),
                    Ok(()) => rx.recv_timeout(COMMAND_TIMEOUT).unwrap_or_else(|_| {
                        Response::err(req.id, "the browser did not answer in time")
                    }),
                };
                pending.lock().unwrap().remove(&req.id);
                resp.id = client_id;
                resp
            }
            Err(e) => Response::err(0, format!("bad request: {e}")),
        };
        let mut bytes = serde_json::to_vec(&resp).unwrap_or_default();
        bytes.push(b'\n');
        if writer.write_all(&bytes).is_err() {
            break;
        }
        line.clear();
    }
}
