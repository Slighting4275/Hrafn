//! Randomized robustness tests for the CLI's parsers.

use crate::{cli, ocr, plan, sites};
use hrafn_proto::fuzz::Rng;
use serde_json::json;

#[test]
fn plan_quote_split_round_trips() {
    let mut rng = Rng::new(31337);
    for _ in 0..30_000 {
        let words: Vec<String> = (0..1 + rng.below(5)).map(|_| rng.text(6)).collect();
        // A plan line is a command name followed by quoted words.
        let line = std::iter::once("cmd".to_string()).chain(words.iter().map(|w| plan::quote(w))).collect::<Vec<_>>().join(" ");
        let back = plan::split(&line).unwrap().unwrap();
        assert_eq!(&back[1..], words.as_slice(), "line {line:?}");
    }
}

#[test]
fn hand_written_plan_can_quote_javascript_with_single_quotes() {
    let line = r#"eval 'document.body.dataset.note = "the raven"; /\w+/.test("raven")'"#;
    assert_eq!(
        plan::split(line).unwrap().unwrap(),
        ["eval", r#"document.body.dataset.note = "the raven"; /\w+/.test("raven")"#]
    );
    assert_eq!(plan::split("type #field ''").unwrap().unwrap(), ["type", "#field", ""]);
    assert!(plan::split("eval 'unclosed").is_err());
    assert_eq!(plan::split("click input[name='q']").unwrap().unwrap(), ["click", "input[name='q']"]);
    assert_eq!(plan::split("type #field O'Brien").unwrap().unwrap(), ["type", "#field", "O'Brien"]);
}

#[test]
fn plan_split_never_panics() {
    let mut rng = Rng::new(5);
    for _ in 0..50_000 {
        let _ = plan::split(&rng.text(30));
    }
}

#[test]
fn recorded_plans_parse_back() {
    let mut rng = Rng::new(8);
    let kinds = ["click", "dblclick", "type", "select", "keys", "navigate", "other"];
    for _ in 0..5_000 {
        let steps: Vec<_> = (0..rng.below(8))
            .map(|_| json!({"type": kinds[rng.below(kinds.len())], "target": rng.text(6), "text": rng.text(6), "value": rng.text(4), "combo": rng.text(3), "url": rng.text(5)}))
            .collect();
        let text = plan::from_steps(&rng.text(10), &steps);
        for line in text.lines() {
            assert!(plan::split(line).is_ok(), "unparseable plan line {line:?}");
        }
    }
}

#[test]
fn command_building_never_panics() {
    let mut rng = Rng::new(99);
    let cmds = [
        "open", "navigate", "tab", "read", "tree", "text", "find", "click", "dblclick", "rightclick", "hover", "drag", "focus", "html",
        "wait-for", "diff", "style", "headers", "override", "sse", "record", "upload", "type", "select", "keys", "scroll", "wait-stable",
        "screenshot", "eval", "console", "net", "cookies", "status", "tabs", "nope",
    ];
    let flags = ["--at", "--double", "--right", "--from-at", "--to-at", "--steps", "--text", "--gone", "--timeout", "--max-chars", "--role", "--append", "--limit", "--filter", "--status", "--type", "--body", "--ms", "--full", "add", "remove", "clear", "list", "body", "log", "set", "delete", "new", "switch", "close", "start", "stop", "1,2", "x,y", "-5"];
    for _ in 0..50_000 {
        let cmd = cmds[rng.below(cmds.len())];
        let args: Vec<String> = (0..rng.below(6))
            .map(|_| if rng.below(2) == 0 { flags[rng.below(flags.len())].to_string() } else { rng.text(4) })
            .collect();
        let _ = cli::build_for_tests(cmd, &args);
    }
}

#[test]
fn ocr_matching_never_panics() {
    let mut rng = Rng::new(2024);
    for _ in 0..3_000 {
        let lines: Vec<ocr::Line> = (0..rng.below(5))
            .map(|_| {
                let text = rng.text(12);
                let chars = text.chars().enumerate().map(|(i, c)| (c, [i as i32 * 10, 0, i as i32 * 10 + 9, 20])).collect();
                ocr::Line { text, chars }
            })
            .collect();
        let q = rng.text(8);
        if let Some((i, b)) = ocr::find(&lines, &q) {
            assert!(i < lines.len());
            assert!(b[0] <= b[2] && b[1] <= b[3]);
        }
        let _ = ocr::closest(&lines, &q);
        let _ = ocr::to_viewport(rng.below(5000) as f64, rng.below(5000) as f64, rng.below(4) as f64, (0.0, 0.0));
    }
}

#[test]
fn google_url_parsing_never_panics() {
    let mut rng = Rng::new(11);
    let pieces = ["https://docs.google.com", "/document", "/spreadsheets", "/presentation", "/d/", "/u/0", "abc", "#gid=", "?", "&", "/edit", "//", ""];
    for _ in 0..50_000 {
        let url: String = (0..rng.below(7)).map(|_| pieces[rng.below(pieces.len())]).collect::<String>() + &rng.text(3);
        if let Some((kind, id, _)) = sites::parse_google_url(&url) {
            assert!(!id.is_empty());
            for f in ["txt", "pdf", "csv", "pptx", "zip"] {
                let _ = sites::export_url("https://docs.google.com", &kind, &id, None, f);
            }
        }
    }
}
