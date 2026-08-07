//! Shared FTS5 `MATCH` expression builder for the derived search indexes.
//!
//! Thread search and memory search must interpret a query the same way. They
//! drifted apart once — threads matched any prefix term while memory required
//! every exact term — so the same words silently returned everything in one
//! tool and nothing in the other. One builder keeps that from recurring.

/// Builds an OR of quoted token-prefix phrases, or `None` when the query has
/// no usable words.
///
/// OR is a recall policy, not sloppiness: agents write long descriptive
/// queries, and requiring every term to co-occur turns one paraphrase into a
/// silent zero-result answer. Prefix matching covers the morphology that
/// exact terms miss (`cover` vs `coverage`). Both indexes rank matches by
/// `bm25` scaled by recency, so breadth costs relevance nothing — it only
/// decides what is eligible to be ranked.
#[must_use]
pub fn or_prefix_match(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|word| !word.is_empty())
        .map(|word| format!("\"{}\"*", word.replace('"', "\"\"")))
        .collect();
    if terms.is_empty() {
        return None;
    }
    Some(terms.join(" OR "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_an_or_of_prefix_phrases() {
        assert_eq!(
            or_prefix_match("identity backend").as_deref(),
            Some("\"identity\"* OR \"backend\"*")
        );
    }

    #[test]
    fn single_term_needs_no_operator() {
        assert_eq!(or_prefix_match("zmux").as_deref(), Some("\"zmux\"*"));
    }

    #[test]
    fn quotes_are_escaped_so_the_expression_stays_parseable() {
        assert_eq!(
            or_prefix_match("say \"hi\"").as_deref(),
            Some("\"say\"* OR \"\"\"hi\"\"\"*")
        );
    }

    #[test]
    fn blank_queries_have_no_expression() {
        assert!(or_prefix_match("").is_none());
        assert!(or_prefix_match("   \t\n ").is_none());
    }
}
