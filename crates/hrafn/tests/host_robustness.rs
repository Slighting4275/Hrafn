//! Runs the real `hrafn-host` binary, plays the browser on its stdin/stdout and the
//! CLI on its socket, and throws malformed input at both sides.

use hrafn_proto::fuzz::Rng;
use hrafn_proto::{Request, Response, frame};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn host_survives_garbage_and_routes_replies() {
    let dir = std::env::temp_dir().join(format!("hrafn-host-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("hrafn.sock");
    let mut host = Command::new(env!("CARGO_BIN_EXE_hrafn-host"))
        .arg("chrome-extension://abcdefghijklmnopabcdefghijklmnop/")
        .env("HRAFN_SOCKET", &sock)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !sock.exists() {
        assert!(start.elapsed() < Duration::from_secs(10), "host never opened its socket");
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut to_host = host.stdin.take().unwrap();
    let mut from_host = host.stdout.take().unwrap();

    // The fake browser: answer every forwarded request with its own cmd as result,
    // after first sending junk the host must ignore.
    let browser = std::thread::spawn(move || {
        let mut rng = Rng::new(3);
        // Unknown ids and non-JSON frames.
        for _ in 0..50 {
            let junk = rng.bytes(40);
            frame::write(&mut to_host, &junk).unwrap();
            let stray = serde_json::to_vec(&Response::ok(rng.next_u64(), serde_json::json!("stray"))).unwrap();
            frame::write(&mut to_host, &stray).unwrap();
        }
        let mut answered = 0;
        while let Ok(Some(bytes)) = frame::read(&mut from_host) {
            let req: Request = serde_json::from_slice(&bytes).unwrap();
            let reply = serde_json::to_vec(&Response::ok(req.id, serde_json::json!(req.cmd))).unwrap();
            frame::write(&mut to_host, &reply).unwrap();
            answered += 1;
            if answered == 40 {
                break;
            }
        }
        answered
    });

    // Clients: garbage lines get an error reply; real requests come back with the
    // client's own id and the right answer, even with several clients at once.
    let clients: Vec<_> = (0..4)
        .map(|c| {
            let sock = sock.clone();
            std::thread::spawn(move || {
                let mut rng = Rng::new(100 + c);
                let stream = UnixStream::connect(&sock).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut w = stream;
                for i in 0..10u64 {
                    // Garbage first.
                    let mut junk = rng.text(30).replace('\n', " ");
                    junk.push('\n');
                    w.write_all(junk.as_bytes()).unwrap();
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    let resp: Response = serde_json::from_str(&line).unwrap();
                    assert!(!resp.ok, "garbage was accepted: {junk:?}");
                    // Then a real request.
                    let req = Request { id: 1000 * c + i, cmd: format!("cmd-{c}-{i}"), args: serde_json::Value::Null, tab: None };
                    let mut bytes = serde_json::to_vec(&req).unwrap();
                    bytes.push(b'\n');
                    w.write_all(&bytes).unwrap();
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    let resp: Response = serde_json::from_str(&line).unwrap();
                    assert_eq!(resp.id, req.id);
                    assert!(resp.ok);
                    assert_eq!(resp.result, serde_json::json!(req.cmd));
                }
            })
        })
        .collect();
    for c in clients {
        c.join().unwrap();
    }
    assert_eq!(browser.join().unwrap(), 40);

    // The fake browser's end of stdin was dropped with its thread: like a closing
    // browser, that must make the host exit by itself and remove its socket.
    let start = Instant::now();
    let exited = loop {
        if let Some(status) = host.try_wait().unwrap() {
            break Some(status);
        }
        if start.elapsed() > Duration::from_secs(10) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if exited.is_none() {
        let _ = host.kill();
    }
    let socket_left = sock.exists();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(exited.is_some_and(|s| s.success()), "host did not exit when the browser closed the pipe");
    assert!(!socket_left, "host left its socket behind");
}
