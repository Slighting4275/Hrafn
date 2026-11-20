//! Randomized robustness tests ("fuzzing" without extra tools or dependencies):
//! feed parsers large amounts of random input and check they never panic and that
//! round trips hold. Shared by every crate's tests through [`Rng`].

/// Small deterministic xorshift generator: the same seed gives the same inputs,
/// so any failure is reproducible.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n.max(1) as u64) as usize
    }
    pub fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = self.below(max + 1);
        (0..n).map(|_| self.next_u64() as u8).collect()
    }
    /// Text mixing ASCII, punctuation that parsers care about, and multi-byte
    /// characters (the usual source of slicing panics).
    pub fn text(&mut self, max: usize) -> String {
        const PIECES: &[&str] = &[
            "a", "Z", "0", "9", " ", "  ", "\t", "\n", "\"", "\\", "#", "*", "?", "+", "-", ",", ".", ":", "/", "[", "]", "e1", "e12",
            "é", "ß", "€", "✓", "😀", "中", "Ω", "\u{0}", "\u{200b}", "Control", "Shift", "F5", "Enter", "http://", "/d/", "gid=",
        ];
        let n = self.below(max + 1);
        (0..n).map(|_| PIECES[self.below(PIECES.len())]).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;
    use crate::{Request, frame};

    #[test]
    fn frames_never_panic_on_garbage_and_round_trip() {
        let mut rng = Rng::new(0xF00D);
        for _ in 0..20_000 {
            // Arbitrary bytes: read must return Ok/Err/None, never panic.
            let junk = rng.bytes(64);
            let mut r = &junk[..];
            while let Ok(Some(_)) = frame::read(&mut r) {}
            // Valid frames always come back byte for byte.
            let body = rng.bytes(300);
            let mut buf = Vec::new();
            frame::write(&mut buf, &body).unwrap();
            assert_eq!(frame::read(&mut &buf[..]).unwrap().unwrap(), body);
        }
    }

    #[test]
    fn request_parsing_never_panics() {
        let mut rng = Rng::new(7);
        for _ in 0..20_000 {
            let s = rng.text(40);
            let _ = serde_json::from_str::<Request>(&s);
            let wrapped = format!(r#"{{"id":{},"cmd":"{}","args":{{}},"tab":{}}}"#, rng.next_u64(), s.replace(['"', '\\'], ""), rng.below(5));
            let _ = serde_json::from_str::<Request>(&wrapped);
        }
    }
}
