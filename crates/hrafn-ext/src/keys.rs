//! Parse key combos like `Control+A` or `Shift+ArrowDown` into what CDP's
//! `Input.dispatchKeyEvent` needs.

#[derive(Debug, PartialEq)]
pub struct Key {
    pub key: String,
    pub code: String,
    pub vk: u32,
    /// Alt=1, Control=2, Meta=4, Shift=8
    pub modifiers: u32,
    /// Text the key types, if any (none when Control/Alt/Meta is held).
    pub text: Option<String>,
}

fn named(k: &str) -> Option<(&'static str, &'static str, u32, Option<&'static str>)> {
    Some(match k.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter", "Enter", 13, Some("\r")),
        "tab" => ("Tab", "Tab", 9, None),
        "escape" | "esc" => ("Escape", "Escape", 27, None),
        "backspace" => ("Backspace", "Backspace", 8, None),
        "delete" | "del" => ("Delete", "Delete", 46, None),
        "space" => (" ", "Space", 32, Some(" ")),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, None),
        "home" => ("Home", "Home", 36, None),
        "end" => ("End", "End", 35, None),
        "pageup" => ("PageUp", "PageUp", 33, None),
        "pagedown" => ("PageDown", "PageDown", 34, None),
        "insert" => ("Insert", "Insert", 45, None),
        _ => return None,
    })
}

pub fn parse(combo: &str) -> Result<Key, String> {
    let parts: Vec<&str> = combo.split('+').map(str::trim).filter(|p| !p.is_empty()).collect();
    let Some((last, mods)) = parts.split_last() else { return Err("empty key combo".into()) };
    let mut modifiers = 0;
    for m in mods {
        modifiers |= match m.to_ascii_lowercase().as_str() {
            "alt" | "option" => 1,
            "control" | "ctrl" => 2,
            "meta" | "cmd" | "super" | "win" => 4,
            "shift" => 8,
            other => return Err(format!("unknown modifier `{other}`")),
        };
    }
    let shift = modifiers & 8 != 0;
    let plain = modifiers & 7 == 0;
    if let Some((key, code, vk, text)) = named(last) {
        return Ok(Key { key: key.into(), code: code.into(), vk, modifiers, text: text.filter(|_| plain).map(String::from) });
    }
    if let Some(n) = last.strip_prefix(['F', 'f']).and_then(|n| n.parse::<u32>().ok()).filter(|n| (1..=12).contains(n)) {
        return Ok(Key { key: format!("F{n}"), code: format!("F{n}"), vk: 111 + n, modifiers, text: None });
    }
    let mut chars = last.chars();
    let (Some(c), None) = (chars.next(), chars.next()) else {
        return Err(format!("unknown key `{last}`"));
    };
    let (code, vk) = if c.is_ascii_alphabetic() {
        (format!("Key{}", c.to_ascii_uppercase()), c.to_ascii_uppercase() as u32)
    } else if c.is_ascii_digit() {
        (format!("Digit{c}"), c as u32)
    } else {
        (String::new(), 0)
    };
    let typed = if c.is_ascii_alphabetic() && shift { c.to_ascii_uppercase() } else { c };
    let key = if c.is_ascii_alphabetic() && !shift && mods.is_empty() { c } else { typed };
    Ok(Key { key: key.to_string(), code, vk, modifiers, text: plain.then(|| typed.to_string()) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys() {
        let k = parse("Enter").unwrap();
        assert_eq!((k.key.as_str(), k.vk, k.text.as_deref()), ("Enter", 13, Some("\r")));
        let k = parse("Shift+ArrowDown").unwrap();
        assert_eq!((k.key.as_str(), k.modifiers, k.text), ("ArrowDown", 8, None));
        assert_eq!(parse("F5").unwrap().vk, 116);
    }

    #[test]
    fn letters_and_modifiers() {
        let k = parse("Control+a").unwrap();
        assert_eq!((k.code.as_str(), k.vk, k.modifiers, k.text), ("KeyA", 65, 2, None));
        let k = parse("Shift+b").unwrap();
        assert_eq!((k.key.as_str(), k.text.as_deref()), ("B", Some("B")));
        assert_eq!(parse("x").unwrap().text.as_deref(), Some("x"));
        assert_eq!(parse("7").unwrap().code, "Digit7");
    }

    #[test]
    fn errors() {
        assert!(parse("").is_err());
        assert!(parse("Hyper+a").is_err());
        assert!(parse("Banana").is_err());
    }
}
