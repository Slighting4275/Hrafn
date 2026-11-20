//! Reading text from images (OCR) with `ocrs`, a pure-Rust engine running its
//! neural network models on the pure-Rust `rten` runtime. The two model files
//! (~12 MB) are downloaded once from the ocrs project and checked against pinned
//! SHA-256 hashes before use.

use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::PathBuf;

struct ModelFile {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
}

const MODELS: [ModelFile; 2] = [
    ModelFile {
        name: "text-detection.onnx",
        url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx",
        sha256: "a917b23dbd9524b465df7e922641b3ff2981623df4ded5a0234004ef2fee7cfe",
    },
    ModelFile {
        name: "text-recognition.onnx",
        url: "https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx",
        sha256: "86c145c2edb96c8caed5b1ebb8f44d706408922211451c309c625157dd6061c5",
    },
];

fn models_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"));
    base.join("hrafn/models")
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Paths of both models, downloading and verifying them on first use.
fn ensure_models() -> Result<[PathBuf; 2], String> {
    let dir = models_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    let mut out = Vec::new();
    for m in &MODELS {
        let path = dir.join(m.name);
        let ok = std::fs::read(&path).map(|b| sha256_hex(&b) == m.sha256).unwrap_or(false);
        if !ok {
            eprintln!("hrafn: downloading OCR model {} (one time)…", m.name);
            let resp = ureq::get(m.url).call().map_err(|e| format!("download of {} failed: {e}", m.name))?;
            let mut bytes = Vec::new();
            resp.into_body()
                .into_reader()
                .take(64 * 1024 * 1024)
                .read_to_end(&mut bytes)
                .map_err(|e| format!("download of {} failed: {e}", m.name))?;
            let got = sha256_hex(&bytes);
            if got != m.sha256 {
                return Err(format!("{} failed its checksum (got {got}); not using it", m.name));
            }
            let tmp = dir.join(format!(".{}.part", m.name));
            std::fs::write(&tmp, &bytes).and_then(|()| std::fs::rename(&tmp, &path)).map_err(|e| format!("could not save {}: {e}", m.name))?;
        }
        out.push(path);
    }
    Ok([out[0].clone(), out[1].clone()])
}

/// One recognised line: its text and each character's box in image pixels.
pub struct Line {
    pub text: String,
    /// (char, [left, top, right, bottom])
    pub chars: Vec<(char, [i32; 4])>,
}

impl Line {
    pub fn bounds(&self) -> [i32; 4] {
        bounds_of(self.chars.iter().map(|(_, r)| *r)).unwrap_or([0; 4])
    }
}

fn bounds_of(rects: impl Iterator<Item = [i32; 4]>) -> Option<[i32; 4]> {
    rects.fold(None, |acc, r| {
        Some(match acc {
            None => r,
            Some(a) => [a[0].min(r[0]), a[1].min(r[1]), a[2].max(r[2]), a[3].max(r[3])],
        })
    })
}

/// Run OCR on a PNG (or other supported) image.
pub fn recognize(image_bytes: &[u8]) -> Result<Vec<Line>, String> {
    use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
    let [det, rec] = ensure_models()?;
    let engine = OcrEngine::new(OcrEngineParams {
        detection_model: Some(rten::Model::load_file(&det).map_err(|e| format!("loading {}: {e}", det.display()))?),
        recognition_model: Some(rten::Model::load_file(&rec).map_err(|e| format!("loading {}: {e}", rec.display()))?),
        ..Default::default()
    })
    .map_err(|e| format!("OCR engine: {e}"))?;
    let img = image::load_from_memory(image_bytes).map_err(|e| format!("could not read the image: {e}"))?.into_rgb8();
    let source = ImageSource::from_bytes(img.as_raw(), img.dimensions()).map_err(|e| format!("image: {e}"))?;
    let input = engine.prepare_input(source).map_err(|e| format!("OCR: {e}"))?;
    let words = engine.detect_words(&input).map_err(|e| format!("OCR: {e}"))?;
    let line_rects = engine.find_text_lines(&input, &words);
    let lines = engine.recognize_text(&input, &line_rects).map_err(|e| format!("OCR: {e}"))?;
    Ok(lines
        .into_iter()
        .flatten()
        .map(|l| Line {
            text: l.to_string(),
            chars: l.chars().iter().map(|c| (c.char, [c.rect.left(), c.rect.top(), c.rect.right(), c.rect.bottom()])).collect(),
        })
        .filter(|l| !l.text.trim().is_empty())
        .collect())
}

fn norm(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

/// Find `query` in the lines; returns the line index and the box (image pixels)
/// around just the matching characters.
///
/// OCR output is imperfect, so matching ignores case and spaces (OCR often merges
/// or splits words) and, if there is no exact match, accepts up to one wrong
/// character per five. Exact matches win over close ones.
pub fn find(lines: &[Line], query: &str) -> Option<(usize, [i32; 4])> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(norm).collect();
    if q.is_empty() {
        return None;
    }
    let allowed = q.len() / 5;
    // ((edits, length difference), line, box): fewest edits, then closest length.
    let mut best: Option<((usize, usize), usize, [i32; 4])> = None;
    for (i, line) in lines.iter().enumerate() {
        let glyphs: Vec<&(char, [i32; 4])> = line.chars.iter().filter(|(c, _)| !c.is_whitespace()).collect();
        let text: Vec<char> = glyphs.iter().map(|(c, _)| norm(*c)).collect();
        // Windows a little shorter or longer than the query absorb OCR adding or
        // dropping a character ("rn" read for "m").
        let lens = q.len().saturating_sub(allowed).max(1)..=q.len() + allowed;
        for len in lens {
            if len > text.len() {
                break;
            }
            for start in 0..=text.len() - len {
                let edits = levenshtein(&text[start..start + len], &q);
                let score = (edits, len.abs_diff(q.len()));
                if edits <= allowed
                    && best.as_ref().is_none_or(|b| score < b.0)
                    && let Some(b) = bounds_of(glyphs[start..start + len].iter().map(|(_, r)| *r))
                {
                    if score == (0, 0) {
                        return Some((i, b));
                    }
                    best = Some((score, i, b));
                }
            }
        }
    }
    best.map(|(_, i, b)| (i, b))
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb)).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()]
}

/// The line that looks most like `query`, for error messages.
pub fn closest<'a>(lines: &'a [Line], query: &str) -> Option<&'a str> {
    let q: Vec<char> = query.chars().filter(|c| !c.is_whitespace()).map(norm).collect();
    lines
        .iter()
        .map(|l| {
            let t: Vec<char> = l.text.chars().filter(|c| !c.is_whitespace()).map(norm).collect();
            let shared = q.iter().filter(|c| t.contains(c)).count();
            (shared, l.text.as_str())
        })
        .filter(|(s, _)| *s > 0)
        .max_by_key(|(s, _)| *s)
        .map(|(_, t)| t)
}

/// Image pixels -> viewport CSS pixels (what `click --at` takes).
pub fn to_viewport(px: f64, py: f64, dpr: f64, origin: (f64, f64)) -> (f64, f64) {
    let d = if dpr > 0.0 { dpr } else { 1.0 };
    (origin.0 + px / d, origin.1 + py / d)
}

pub fn center(b: [i32; 4]) -> (f64, f64) {
    ((b[0] + b[2]) as f64 / 2.0, (b[1] + b[3]) as f64 / 2.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str, x: i32) -> Line {
        // 10 px wide characters on a line at y 100..120, starting at x.
        Line {
            text: text.into(),
            chars: text.chars().enumerate().map(|(i, c)| (c, [x + 10 * i as i32, 100, x + 10 * i as i32 + 9, 120])).collect(),
        }
    }

    #[test]
    fn finds_text_and_boxes_only_the_match() {
        let lines = vec![line("Welcome back", 0), line("Press me now", 50)];
        let (i, b) = find(&lines, "PRESS ME").unwrap();
        assert_eq!(i, 1);
        assert_eq!(b, [50, 100, 50 + 70 + 9, 120]);
        assert_eq!(find(&lines, "missing"), None);
        assert_eq!(find(&lines, ""), None);
        assert_eq!(lines[0].bounds(), [0, 100, 119, 120]);
    }

    #[test]
    fn tolerates_ocr_spacing_and_small_misreads() {
        let lines = vec![line("Pressme", 0), line("Subrnit order", 0), line("Total 42", 0)];
        // Merged words.
        assert_eq!(find(&lines, "Press me").map(|f| f.0), Some(0));
        // "rn" misread for "m": one wrong letter in 11 is allowed.
        assert_eq!(find(&lines, "Submit order").map(|f| f.0), Some(1));
        // Too different.
        assert_eq!(find(&lines, "Cancel"), None);
        // Short words need an exact match (one miss allowed only from 5 letters).
        assert_eq!(find(&lines, "Tota1"), Some((2, [0, 100, 49, 120])));
        assert_eq!(find(&lines, "43"), None);
        assert_eq!(closest(&lines, "Submit"), Some("Subrnit order"));
    }

    #[test]
    fn exact_match_beats_a_close_one() {
        let lines = vec![line("Sign 1n", 0), line("Sign in", 0)];
        assert_eq!(find(&lines, "Sign in").map(|f| f.0), Some(1));
    }

    #[test]
    fn image_to_viewport_coordinates() {
        assert_eq!(to_viewport(200.0, 100.0, 2.0, (0.0, 0.0)), (100.0, 50.0));
        assert_eq!(to_viewport(10.0, 10.0, 1.0, (30.0, 40.0)), (40.0, 50.0));
        assert_eq!(to_viewport(10.0, 10.0, 0.0, (0.0, 0.0)), (10.0, 10.0));
        assert_eq!(center([0, 0, 10, 20]), (5.0, 10.0));
    }

    #[test]
    fn checksums_are_pinned() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert!(MODELS.iter().all(|m| m.sha256.len() == 64 && m.url.starts_with("https://")));
    }
}
