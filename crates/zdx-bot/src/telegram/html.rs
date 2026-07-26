//! Repair pass for Telegram HTML payloads.
//!
//! Telegram rejects an entire message when it hits a stray `<`, an unknown
//! entity, or an unbalanced tag. This module rewrites such text so that only
//! Telegram-supported tags stay live and everything else renders literally.

/// Tags Telegram accepts in `parse_mode=HTML`.
const ALLOWED_TAGS: &[&str] = &[
    "b",
    "strong",
    "i",
    "em",
    "u",
    "ins",
    "s",
    "strike",
    "del",
    "span",
    "a",
    "code",
    "pre",
    "blockquote",
    "tg-spoiler",
    "tg-emoji",
];

/// Entities Telegram's HTML parser understands by name.
const NAMED_ENTITIES: &[&str] = &["amp", "lt", "gt", "quot"];

struct Tag<'a> {
    name: &'a str,
    len: usize,
    closing: bool,
}

/// Escapes everything that is not a well-formed, Telegram-supported tag or
/// entity, and balances tags that were left open or crossed.
pub(crate) fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 16);
    let mut open: Vec<&str> = Vec::new();
    let mut rest = input;

    while let Some(pos) = rest.find(['<', '>', '&']) {
        out.push_str(&rest[..pos]);
        let tail = &rest[pos..];

        let consumed = match tail.as_bytes()[0] {
            b'<' => match parse_tag(tail) {
                Some(tag) if tag.closing => close_tag(&mut out, &mut open, &tag, tail),
                Some(tag) => {
                    open.push(tag.name);
                    out.push_str(&tail[..tag.len]);
                    tag.len
                }
                None => {
                    out.push_str("&lt;");
                    1
                }
            },
            b'>' => {
                out.push_str("&gt;");
                1
            }
            _ => {
                if let Some(len) = entity_len(tail) {
                    out.push_str(&tail[..len]);
                    len
                } else {
                    out.push_str("&amp;");
                    1
                }
            }
        };

        rest = &tail[consumed..];
    }
    out.push_str(rest);

    while let Some(name) = open.pop() {
        push_closing(&mut out, name);
    }

    out
}

/// Emits a closing tag, first closing anything opened inside it so crossed
/// tags (`<b><i>x</b>`) come out properly nested.
fn close_tag(out: &mut String, open: &mut Vec<&str>, tag: &Tag<'_>, tail: &str) -> usize {
    let Some(index) = open.iter().rposition(|name| *name == tag.name) else {
        out.push_str("&lt;");
        return 1;
    };

    while open.len() > index + 1 {
        if let Some(inner) = open.pop() {
            push_closing(out, inner);
        }
    }
    open.pop();
    out.push_str(&tail[..tag.len]);
    tag.len
}

fn push_closing(out: &mut String, name: &str) {
    out.push_str("</");
    out.push_str(name);
    out.push('>');
}

/// Parses a supported tag at the start of `tail`, returning its name and byte
/// length. Returns `None` for anything that is not a complete allowed tag.
fn parse_tag(tail: &str) -> Option<Tag<'_>> {
    let body = tail.strip_prefix('<')?;
    let (closing, body) = match body.strip_prefix('/') {
        Some(body) => (true, body),
        None => (false, body),
    };

    let name_len = body.find(|c: char| !c.is_ascii_alphanumeric() && c != '-')?;
    let name = &body[..name_len];
    if !ALLOWED_TAGS.contains(&name) {
        return None;
    }

    let after_name = &body[name_len..];
    let gt = after_name.find('>')?;
    let inner = &after_name[..gt];
    if inner.contains('<') {
        return None;
    }
    if closing && !inner.trim().is_empty() {
        return None;
    }

    let prefix = if closing { 2 } else { 1 };
    Some(Tag {
        name,
        len: prefix + name_len + gt + 1,
        closing,
    })
}

/// Returns the byte length of a supported HTML entity at the start of `tail`.
fn entity_len(tail: &str) -> Option<usize> {
    let body = tail.strip_prefix('&')?;
    let end = body.find(';')?;
    let name = &body[..end];

    let valid = if let Some(digits) = name.strip_prefix("#x").or(name.strip_prefix("#X")) {
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit())
    } else if let Some(digits) = name.strip_prefix('#') {
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit())
    } else {
        NAMED_ENTITIES.contains(&name)
    };

    valid.then_some(end + 2)
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn escapes_stray_less_than_and_keeps_formatting() {
        let input = "<b>No attestation</b> — very old (<Android 8) devices";
        assert_eq!(
            sanitize(input),
            "<b>No attestation</b> — very old (&lt;Android 8) devices"
        );
    }

    #[test]
    fn leaves_valid_markup_untouched() {
        let input = concat!(
            "<b>Title</b>\n<i>x</i> <code>a &amp; b</code> ",
            "<a href=\"https://example.com/?a=1&amp;b=2\">link</a>\n",
            "<blockquote>quote</blockquote><pre>code</pre>"
        );
        assert_eq!(sanitize(input), input);
    }

    #[test]
    fn escapes_unknown_tags_and_generics() {
        assert_eq!(
            sanitize("<code>Vec<T></code> and <div>x</div>"),
            "<code>Vec&lt;T&gt;</code> and &lt;div&gt;x&lt;/div&gt;"
        );
    }

    #[test]
    fn closes_unbalanced_tags() {
        assert_eq!(
            sanitize("<b>bold and <i>italic"),
            "<b>bold and <i>italic</i></b>"
        );
        assert_eq!(sanitize("<b><i>x</b>"), "<b><i>x</i></b>");
        assert_eq!(sanitize("stray </b> close"), "stray &lt;/b&gt; close");
    }

    #[test]
    fn escapes_unsupported_entities_only() {
        assert_eq!(
            sanitize("a & b &amp; c &nbsp; d &#8212; e &#x2014;"),
            "a &amp; b &amp; c &amp;nbsp; d &#8212; e &#x2014;"
        );
    }

    #[test]
    fn is_idempotent() {
        let once = sanitize("<b>a < b</b> & <i>c");
        assert_eq!(sanitize(&once), once);
    }
}
