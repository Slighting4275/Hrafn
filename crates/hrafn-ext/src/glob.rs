//! URL patterns for request overrides: `*` matches any run of characters, `?` one
//! character, everything else literally. The whole URL must match.

pub fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn wildcards() {
        assert!(matches("*/api/data", "http://127.0.0.1:8080/api/data"));
        assert!(matches("*api*", "https://x.test/v1/api/items?q=1"));
        assert!(matches("https://x.test/item?", "https://x.test/item7"));
        assert!(matches("*", ""));
        assert!(!matches("*/api/data", "http://127.0.0.1/api/data2"));
        assert!(!matches("https://a.test/*", "http://a.test/x"));
        assert!(matches("a*b*c", "aXXbYYc"));
        assert!(!matches("a*b*c", "aXXbYY"));
    }
}
