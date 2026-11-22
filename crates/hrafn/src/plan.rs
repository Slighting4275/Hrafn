//! Plan files: one hrafn command per line, `#` comments, shell-style quoting.
//!
//! ```text
//! # recorded by hrafn
//! open "http://127.0.0.1:8080/"
//! click "#go"
//! type "#name" "Test User"
//! keys Enter
//! ```

use serde_json::Value;

/// Split one line into words. A pair of single quotes at word boundaries preserves literal text.
/// Other apostrophes are literal. Inside double quotes, `\"`, `\\`,
/// `\n`, `\r` and `\t` are escapes (so multi-line text fits on one plan line).
/// Returns `None` for blank lines and comments.
pub fn split(line: &str) -> Result<Option<Vec<String>>, String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return Ok(None);
    }
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !in_word => {
                let rest = chars.as_str();
                // Accept the first closing apostrophe only at a word boundary.
                // This preserves old bare arguments such as 'tis and 'tain't.
                if let Some(end) = rest.find('\'').filter(|end| rest[end + 1..].chars().next().is_none_or(char::is_whitespace)) {
                    cur.push_str(&rest[..end]);
                    chars = rest[end + 1..].chars();
                } else {
                    cur.push(c);
                }
                in_word = true;
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some('n') => cur.push('\n'),
                            Some('r') => cur.push('\r'),
                            Some('t') => cur.push('\t'),
                            Some(e) => cur.push(e),
                            None => return Err("unfinished escape".into()),
                        },
                        Some(ch) => cur.push(ch),
                        None => return Err("missing closing double quote".into()),
                    }
                }
            }
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                cur.push(c);
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    Ok(Some(words))
}

/// Quote a word for a plan line when it needs it.
pub fn quote(w: &str) -> String {
    if !w.is_empty() && w.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:+=,@#".contains(c)) {
        return w.to_string();
    }
    let mut out = String::with_capacity(w.len() + 2);
    out.push('"');
    for c in w.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Turn recorded steps (from the extension) into plan lines.
pub fn from_steps(start_url: &str, steps: &[Value]) -> String {
    let mut out = String::from("# hrafn plan (recorded). Edit freely; run with: hrafn run <file>\n");
    if !start_url.is_empty() {
        out.push_str(&format!("open {}\n", quote(start_url)));
    }
    for s in steps {
        let target = s["target"].as_str().unwrap_or("");
        let line = match s["type"].as_str().unwrap_or("") {
            "click" if !target.is_empty() => format!("click {}", quote(target)),
            "dblclick" if !target.is_empty() => format!("click {} --double", quote(target)),
            "type" if !target.is_empty() => format!("type {} {}", quote(target), quote(s["text"].as_str().unwrap_or(""))),
            "select" if !target.is_empty() => format!("select {} {}", quote(target), quote(s["value"].as_str().unwrap_or(""))),
            "keys" => format!("keys {}", quote(s["combo"].as_str().unwrap_or("Enter"))),
            // A comment must stay on one line whatever the page reported.
            "navigate" => format!(
                "# page changed to {}",
                s["url"].as_str().unwrap_or("").chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>()
            ),
            _ => continue,
        };
        out.push_str(&line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn splits_quotes_and_comments() {
        assert_eq!(split("  # note").unwrap(), None);
        assert_eq!(split("").unwrap(), None);
        assert_eq!(split(r##"type "#name" "Test User""##).unwrap().unwrap(), vec!["type", "#name", "Test User"]);
        assert_eq!(split(r#"click "div > p:nth-of-type(2)""#).unwrap().unwrap(), vec!["click", "div > p:nth-of-type(2)"]);
        assert_eq!(split(r#"type x "say \"hi\"""#).unwrap().unwrap(), vec!["type", "x", "say \"hi\""]);
        assert_eq!(split(r#"type x """#).unwrap().unwrap(), vec!["type", "x", ""]);
        assert!(split(r#"click "open"#).is_err());
    }

    #[test]
    fn quote_round_trips() {
        for w in ["#go", "Test User", "a \"b\" \\c", "", "https://x.test/a?b=c", "two\nlines\tand\r\n", "back\\nslash"] {
            let line = format!("cmd {}", quote(w));
            assert_eq!(split(&line).unwrap().unwrap()[1], w, "word {w:?}");
        }
    }

    #[test]
    fn steps_become_plan_lines() {
        let steps = vec![
            json!({"type": "click", "target": "#go"}),
            json!({"type": "type", "target": "#name", "text": "Test User"}),
            json!({"type": "select", "target": "#color", "value": "blue"}),
            json!({"type": "keys", "combo": "Enter"}),
            json!({"type": "click", "target": ""}),
        ];
        let plan = from_steps("http://127.0.0.1:9/", &steps);
        assert_eq!(
            plan.lines().skip(1).collect::<Vec<_>>(),
            vec!["open http://127.0.0.1:9/", "click #go", "type #name \"Test User\"", "select #color blue", "keys Enter"]
        );
    }
}
