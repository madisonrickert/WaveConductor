//! Small helpers shared by more than one xtask subcommand.
//!
//! Each subcommand hand-emits its own JSON (rather than depending on a JSON
//! value tree crate for a handful of flat objects), so the one genuinely
//! shared piece is string escaping. Centralized here so `check-secrets`,
//! `validate-shaders`, and `manifest` can't drift out of sync on quoting
//! rules the way the pre-extraction copies did.

/// Escape a string for inclusion as a JSON string value.
///
/// Handles the characters that are illegal unescaped inside a JSON string
/// (`"`, `\`, and the C0 control characters) plus the common `\n`/`\r`/`\t`
/// shorthands; everything else (including all non-ASCII `char`s) passes
/// through unchanged, which is valid per the JSON spec.
pub fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                use std::fmt::Write as _;
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::json_escape;

    #[test]
    fn escapes_quotes_and_backslashes() {
        assert_eq!(json_escape(r#"a"b\c"#), r#"a\"b\\c"#);
    }

    #[test]
    fn escapes_common_whitespace_shorthands() {
        assert_eq!(json_escape("a\nb\rc\td"), "a\\nb\\rc\\td");
    }

    #[test]
    fn escapes_other_control_characters_as_unicode_sequences() {
        assert_eq!(json_escape("a\u{0001}b"), "a\\u0001b");
    }

    #[test]
    fn passes_through_plain_ascii_and_unicode_unchanged() {
        assert_eq!(json_escape("plain text"), "plain text");
        assert_eq!(json_escape("emoji: 🎛"), "emoji: 🎛");
    }
}
