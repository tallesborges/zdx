//! Pure helpers for formatting values into log fields.

/// Collapse whitespace and bound a value for use as a `tracing` field.
///
/// Two invariants matter for line-oriented log consumers such as the monitor's
/// Logs tab:
/// - **One event is one line.** Multi-line values (pretty-printed JSON error
///   bodies, captured stderr) would otherwise split a single event across many
///   lines, where every continuation line parses as unstructured.
/// - **Bounded size.** A large tool output or error body must not bloat the
///   log file.
///
/// Truncation happens on a character boundary and reports the original length.
#[must_use]
pub fn log_field(value: &str, max_bytes: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.len() <= max_bytes {
        return collapsed;
    }
    let mut cut = max_bytes;
    while !collapsed.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}… ({} bytes total)", &collapsed[..cut], collapsed.len())
}

#[cfg(test)]
mod tests {
    use super::log_field;

    #[test]
    fn collapses_multiline_values_to_one_line() {
        let pretty = "{\n  \"error\": {\n    \"message\": \"nope\"\n  }\n}";
        let out = log_field(pretty, 300);
        assert!(!out.contains('\n'), "{out}");
        assert_eq!(out, "{ \"error\": { \"message\": \"nope\" } }");
    }

    #[test]
    fn truncates_with_original_length() {
        let out = log_field(&"x".repeat(1000), 100);
        assert!(out.len() < 200, "{}", out.len());
        assert!(out.contains("1000 bytes total"), "{out}");
    }

    #[test]
    fn keeps_short_values_intact() {
        assert_eq!(log_field("Path does not exist", 300), "Path does not exist");
        assert_eq!(log_field("", 300), "");
    }

    #[test]
    fn truncates_on_a_character_boundary() {
        // Multi-byte characters must not be split mid-codepoint.
        let out = log_field(&"é".repeat(200), 50);
        assert!(out.contains("bytes total"), "{out}");
    }
}
