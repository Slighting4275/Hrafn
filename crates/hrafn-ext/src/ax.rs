//! Render Chrome's accessibility tree (`Accessibility.getFullAXTree`) as an
//! indented outline and hand out short refs (`e1`, `e2`...) for elements.

use serde_json::Value;
use std::collections::{HashMap, HashSet};

/// Roles that are only layout wrappers: not printed, their children are.
const WRAPPERS: [&str; 6] = ["generic", "none", "InlineTextBox", "LineBreak", "presentation", "Section"];
const MAX_NAME: usize = 120;

pub struct Rendered {
    pub tree: String,
    /// ref -> backend DOM node id
    pub refs: HashMap<String, i64>,
    /// (ref, role, name) for every element that got a ref
    pub items: Vec<(String, String, String)>,
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v[k]["value"].as_str().unwrap_or("")
}

fn clip(t: &str) -> String {
    let t = t.split_whitespace().collect::<Vec<_>>().join(" ");
    if t.chars().count() > MAX_NAME {
        format!("{}…", t.chars().take(MAX_NAME).collect::<String>())
    } else {
        t
    }
}

fn props(n: &Value) -> String {
    let mut out = Vec::new();
    for p in n["properties"].as_array().into_iter().flatten() {
        let name = p["name"].as_str().unwrap_or("");
        let v = &p["value"]["value"];
        match name {
            "level" => out.push(format!("level={v}")),
            "checked" | "pressed" if v.as_str() == Some("true") || v == &Value::Bool(true) => out.push(name.to_string()),
            "disabled" | "focused" | "required" | "expanded" | "selected" if v == &Value::Bool(true) => out.push(name.to_string()),
            _ => {}
        }
    }
    let value = s(n, "value");
    if !value.is_empty() {
        out.push(format!("value=\"{}\"", clip(value)));
    }
    if out.is_empty() { String::new() } else { format!(" ({})", out.join(", ")) }
}

/// How many nodes deep the walk may recurse. The extension runs as WebAssembly with
/// a small stack, so a pathologically deep page must not overflow it.
const MAX_NESTING: usize = 256;

/// `root_backend`: start from the node with this backend id instead of the page root.
/// Refs are numbered from `e1` in each read.
pub fn render(nodes: &[Value], root_backend: Option<i64>, max_chars: usize) -> Rendered {
    let by_id: HashMap<&str, &Value> =
        nodes.iter().filter_map(|n| n["nodeId"].as_str().map(|id| (id, n))).collect();
    let root = match root_backend {
        Some(b) => nodes.iter().find(|n| n["backendDOMNodeId"].as_i64() == Some(b)),
        None => nodes.iter().find(|n| n.get("parentId").is_none()).or(nodes.first()),
    };
    let mut w = Walk {
        by_id: &by_id,
        max: max_chars,
        out: Rendered { tree: String::new(), refs: HashMap::new(), items: Vec::new() },
        truncated: false,
        too_deep: false,
        seen: HashSet::new(),
    };
    if let Some(root) = root {
        w.walk(root, 0, 0, "");
    }
    let mut r = w.out;
    if w.truncated {
        r.tree.push_str("… (truncated; use --max-chars or read <ref> for a part of the page)\n");
    }
    if w.too_deep {
        r.tree.push_str("… (very deeply nested parts not shown; use read <ref> on a deeper element)\n");
    }
    r
}

struct Walk<'a> {
    by_id: &'a HashMap<&'a str, &'a Value>,
    max: usize,
    out: Rendered,
    truncated: bool,
    too_deep: bool,
    /// Nodes already visited: a malformed tree with a cycle must not loop forever.
    seen: HashSet<&'a str>,
}

impl<'a> Walk<'a> {
    fn walk(&mut self, n: &'a Value, depth: usize, nesting: usize, parent_name: &str) {
        if self.truncated {
            return;
        }
        if nesting > MAX_NESTING {
            self.too_deep = true;
            return;
        }
        if let Some(id) = n["nodeId"].as_str()
            && !self.seen.insert(id)
        {
            return;
        }
        let role = s(n, "role");
        // Line-layout pieces of text the parent already shows: skip them and their children.
        if role == "InlineTextBox" || role == "LineBreak" {
            return;
        }
        let name = clip(s(n, "name"));
        let ignored = n["ignored"].as_bool().unwrap_or(false);
        let wrapper = WRAPPERS.contains(&role) && name.is_empty();
        // Text that only repeats its parent's name ("link Docs" > "text Docs") adds noise.
        let echo = role == "StaticText" && name == parent_name;
        let print = !ignored && !wrapper && !echo && !(role == "StaticText" && name.is_empty());

        let mut child_depth = depth;
        let mut my_name = parent_name.to_string();
        if print {
            let r = &mut self.out;
            let indent = "  ".repeat(depth);
            let backend = n["backendDOMNodeId"].as_i64().filter(|_| role != "StaticText");
            let rf = backend.map(|_| format!("e{}", r.refs.len() + 1));
            let line = if role == "StaticText" {
                format!("{indent}- text \"{name}\"\n")
            } else {
                let mut line = format!("{indent}- {role}");
                if !name.is_empty() {
                    line.push_str(&format!(" \"{name}\""));
                }
                line.push_str(&props(n));
                if let Some(rf) = &rf {
                    line.push_str(&format!(" [{rf}]"));
                }
                line.push('\n');
                line
            };
            if r.tree.len() + line.len() > self.max {
                self.truncated = true;
                return;
            }
            // Only lines that are actually shown get a ref.
            if let (Some(rf), Some(b)) = (rf, backend) {
                r.refs.insert(rf.clone(), b);
                r.items.push((rf, role.to_string(), name.clone()));
            }
            r.tree.push_str(&line);
            child_depth = depth + 1;
            my_name = name;
        }
        let by_id = self.by_id;
        for c in n["childIds"].as_array().into_iter().flatten() {
            if let Some(child) = c.as_str().and_then(|id| by_id.get(id)) {
                self.walk(child, child_depth, nesting + 1, &my_name);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(id: &str, parent: Option<&str>, role: &str, name: &str, kids: &[&str], backend: i64) -> Value {
        let mut n = json!({
            "nodeId": id, "ignored": false,
            "role": {"type": "role", "value": role},
            "name": {"type": "computedString", "value": name},
            "childIds": kids, "backendDOMNodeId": backend,
        });
        if let Some(p) = parent {
            n["parentId"] = json!(p);
        }
        n
    }

    fn page() -> Vec<Value> {
        let mut h = node("3", Some("1"), "heading", "Welcome", &["4"], 12);
        h["properties"] = json!([{"name": "level", "value": {"type": "integer", "value": 1}}]);
        vec![
            node("1", None, "RootWebArea", "Test page", &["2"], 10),
            node("2", Some("1"), "generic", "", &["3", "5", "7"], 11),
            h,
            node("4", Some("3"), "StaticText", "Welcome", &[], 13),
            node("5", Some("2"), "link", "Docs", &["6"], 14),
            node("6", Some("5"), "StaticText", "Docs", &[], 15),
            node("7", Some("2"), "StaticText", "Plain words", &[], 16),
        ]
    }

    #[test]
    fn renders_outline_with_refs_and_skips_wrappers_and_echoes() {
        let r = render(&page(), None, 10_000);
        assert_eq!(
            r.tree,
            "- RootWebArea \"Test page\" [e1]\n  - heading \"Welcome\" (level=1) [e2]\n  - link \"Docs\" [e3]\n  - text \"Plain words\"\n"
        );
        assert_eq!(r.refs["e3"], 14);
        assert_eq!(r.items[1], ("e2".into(), "heading".into(), "Welcome".into()));
    }

    #[test]
    fn subtree_and_truncation() {
        let r = render(&page(), Some(14), 10_000);
        assert_eq!(r.tree, "- link \"Docs\" [e1]\n");
        let r = render(&page(), None, 40);
        assert!(r.tree.ends_with("(truncated; use --max-chars or read <ref> for a part of the page)\n"));
        assert!(r.tree.starts_with("- RootWebArea"));
    }

    #[test]
    fn inline_text_boxes_are_hidden() {
        let nodes = vec![
            node("1", None, "paragraph", "", &["2"], 1),
            node("2", Some("1"), "StaticText", "Two lines of text", &["3", "4"], 2),
            node("3", Some("2"), "InlineTextBox", "Two lines", &[], 3),
            node("4", Some("2"), "InlineTextBox", "of text", &[], 4),
        ];
        let r = render(&nodes, None, 10_000);
        assert_eq!(r.tree, "- paragraph [e1]\n  - text \"Two lines of text\"\n");
    }

    #[test]
    fn long_names_are_clipped() {
        let long = "word ".repeat(100);
        let r = render(&[node("1", None, "button", &long, &[], 1)], None, 10_000);
        assert!(r.tree.contains('…'));
        assert!(r.tree.len() < 200);
    }
}
