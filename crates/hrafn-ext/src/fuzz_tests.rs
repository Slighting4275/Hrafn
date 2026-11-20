//! Randomized robustness tests for the extension's pure parsers.

use crate::{ax, glob, keys, record};
use hrafn_proto::fuzz::Rng;
use serde_json::{Value, json};

/// Slow but obviously correct glob matcher to compare against.
fn reference_glob(p: &[char], t: &[char]) -> bool {
    match (p.first(), t.first()) {
        (None, None) => true,
        (Some('*'), _) => reference_glob(&p[1..], t) || (!t.is_empty() && reference_glob(p, &t[1..])),
        (Some('?'), Some(_)) => reference_glob(&p[1..], &t[1..]),
        (Some(a), Some(b)) if a == b => reference_glob(&p[1..], &t[1..]),
        _ => false,
    }
}

#[test]
fn glob_agrees_with_reference() {
    let mut rng = Rng::new(42);
    let alphabet = ['a', 'b', '*', '?', '/', 'é'];
    for _ in 0..50_000 {
        let p: Vec<char> = (0..rng.below(7)).map(|_| alphabet[rng.below(alphabet.len())]).collect();
        let t: Vec<char> = (0..rng.below(9)).map(|_| alphabet[rng.below(alphabet.len())]).filter(|c| *c != '*' && *c != '?').collect();
        let (ps, ts): (String, String) = (p.iter().collect(), t.iter().collect());
        assert_eq!(glob::matches(&ps, &ts), reference_glob(&p, &t), "pattern {ps:?} text {ts:?}");
    }
}

#[test]
fn keys_never_panic() {
    let mut rng = Rng::new(9);
    for _ in 0..50_000 {
        let combo = rng.text(6);
        if let Ok(k) = keys::parse(&combo) {
            assert!(!k.key.is_empty());
            assert!(k.modifiers <= 15);
        }
    }
}

fn random_tree(rng: &mut Rng) -> Vec<Value> {
    let n = 1 + rng.below(30);
    let roles = ["RootWebArea", "generic", "button", "link", "StaticText", "InlineTextBox", "heading", "none", "LineBreak", "textbox", ""];
    (0..n)
        .map(|i| {
            // Children may point anywhere, including ancestors and themselves (cycles),
            // and to ids that don't exist.
            let kids: Vec<String> = (0..rng.below(4)).map(|_| rng.below(n + 3).to_string()).collect();
            let mut node = json!({
                "nodeId": i.to_string(),
                "ignored": rng.below(5) == 0,
                "role": {"value": roles[rng.below(roles.len())]},
                "name": {"value": rng.text(12)},
                "childIds": kids,
                "properties": [{"name": "level", "value": {"value": rng.below(7)}}],
            });
            if rng.below(3) > 0 {
                node["backendDOMNodeId"] = json!(rng.below(1000));
            }
            if i > 0 {
                node["parentId"] = json!(rng.below(i).to_string());
            }
            if rng.below(6) == 0 {
                node["role"] = json!(rng.text(3)); // malformed field
            }
            node
        })
        .collect()
}

#[test]
fn accessibility_tree_rendering_never_panics_or_hangs() {
    let mut rng = Rng::new(1234);
    for _ in 0..5_000 {
        let nodes = random_tree(&mut rng);
        let max = 1 + rng.below(4000);
        let r = ax::render(&nodes, None, max);
        // Output respects the size limit (plus the one truncation notice line).
        assert!(r.tree.len() <= max + 100, "tree of {} bytes for limit {max}", r.tree.len());
        assert_eq!(r.refs.len(), r.items.len());
        let _ = ax::render(&nodes, Some(rng.below(1000) as i64), max);
    }
}

#[test]
fn recorder_never_panics() {
    let mut rng = Rng::new(77);
    let kinds = ["click", "dblclick", "type", "select", "keys", "navigate", "weird"];
    for _ in 0..2_000 {
        let mut steps = Vec::new();
        for _ in 0..rng.below(40) {
            let target = ["#a", "#b", "", "html > body"][rng.below(4)];
            record::record_step(&mut steps, json!({"type": kinds[rng.below(kinds.len())], "target": target, "text": rng.text(5)}));
        }
        // Consecutive typing into one field always collapses to a single step.
        for w in steps.windows(2) {
            assert!(!(w[0]["type"] == "type" && w[1]["type"] == "type" && w[0]["target"] == w[1]["target"]));
        }
    }
}
