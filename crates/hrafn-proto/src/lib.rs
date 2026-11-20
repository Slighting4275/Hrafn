//! Messages shared by the `hrafn` CLI, the native host and the browser extension.
//!
//! Path of a command: CLI --(unix socket, one JSON line)--> host
//! --(native messaging frame)--> extension, and the reply comes back the same way.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Random-input test helpers (public so every crate's tests can use them).
#[doc(hidden)]
pub mod fuzz;

/// Name Brave/Chrome use to find the native host manifest.
pub const HOST_NAME: &str = "com.hrafn.host";

/// A command for the extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Request {
    pub id: u64,
    pub cmd: String,
    #[serde(default)]
    pub args: Value,
    /// Tab to act on. `None` means the current tab (last one opened or switched to,
    /// else the active tab).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab: Option<i64>,
}

/// The extension's answer to a [`Request`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: Value) -> Self {
        Self { id, ok: true, result, error: None }
    }
    pub fn err(id: u64, msg: impl Into<String>) -> Self {
        Self { id, ok: false, result: Value::Null, error: Some(msg.into()) }
    }
}

/// Native messaging frames: a 32-bit length in native byte order, then UTF-8 JSON.
pub mod frame {
    use std::io::{self, Read, Write};

    /// Browsers refuse host-to-extension messages over 1 MiB.
    pub const MAX_TO_BROWSER: usize = 1024 * 1024;
    /// Browsers send at most 64 MiB to a host.
    pub const MAX_FROM_BROWSER: usize = 64 * 1024 * 1024;

    pub fn write(w: &mut impl Write, json: &[u8]) -> io::Result<()> {
        if json.len() > MAX_TO_BROWSER {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "message larger than 1 MiB"));
        }
        w.write_all(&(json.len() as u32).to_ne_bytes())?;
        w.write_all(json)?;
        w.flush()
    }

    /// Returns `Ok(None)` at a clean end of input (the browser closed the pipe).
    pub fn read(r: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
        let mut len = [0u8; 4];
        match r.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let n = u32::from_ne_bytes(len) as usize;
        if n > MAX_FROM_BROWSER {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("frame of {n} bytes is too large")));
        }
        let mut buf = vec![0u8; n];
        r.read_exact(&mut buf)?;
        Ok(Some(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip_and_clean_eof() {
        let mut buf = Vec::new();
        frame::write(&mut buf, br#"{"id":1}"#).unwrap();
        frame::write(&mut buf, b"{}").unwrap();
        let mut r = &buf[..];
        assert_eq!(frame::read(&mut r).unwrap().unwrap(), br#"{"id":1}"#);
        assert_eq!(frame::read(&mut r).unwrap().unwrap(), b"{}");
        assert!(frame::read(&mut r).unwrap().is_none());
    }

    #[test]
    fn oversized_frames_are_refused() {
        let big = vec![b' '; frame::MAX_TO_BROWSER + 1];
        assert!(frame::write(&mut Vec::new(), &big).is_err());
        let mut bad = Vec::new();
        bad.extend_from_slice(&((frame::MAX_FROM_BROWSER + 1) as u32).to_ne_bytes());
        assert!(frame::read(&mut &bad[..]).is_err());
    }

    #[test]
    fn request_tab_is_optional() {
        let r: Request = serde_json::from_str(r#"{"id":7,"cmd":"tabs"}"#).unwrap();
        assert_eq!(r.tab, None);
        assert_eq!(r.args, Value::Null);
        assert!(!serde_json::to_string(&r).unwrap().contains("tab\""));
    }
}
