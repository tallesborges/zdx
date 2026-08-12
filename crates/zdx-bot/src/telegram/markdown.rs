//! Markdown → Telegram HTML conversion for model-authored reply text.
//!
//! The model writes plain Markdown; this converter renders it into the strict
//! HTML subset Telegram accepts, emitting valid entity nesting on its own.
//! [`super::html::sanitize`] only repairs tag syntax after a Telegram
//! rejection, so semantically forbidden nesting has to be avoided here.
//!
//! Parser options must stay identical to the transcript renderer
//! (`zdx-transcript`), or content renders on one surface but not the other.
//!
//! Deliberate lowerings: headings become bold lines, lists become flat
//! prefixed lines, tables become pipe-joined plain lines, and images keep only
//! their alt text. Legacy Telegram HTML bypasses conversion as a whole; other
//! raw HTML is escaped as literal text.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

const TELEGRAM_TAGS: &[&str] = &[
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

/// Converts Markdown into a Telegram-valid HTML payload.
pub(crate) fn to_telegram_html(input: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);

    if is_legacy_telegram_html(input, options) {
        return input.trim().to_string();
    }

    let mut renderer = Renderer::default();
    for event in Parser::new_ext(input, options) {
        renderer.event(event);
    }
    renderer.finish()
}

/// Truncates rendered Telegram HTML by visible characters without splitting
/// tags or entities, then closes any formatting that remains open.
pub(crate) fn truncate_telegram_html(input: &str, max_visible_chars: usize) -> String {
    if visible_char_count(input) <= max_visible_chars {
        return input.to_string();
    }

    let mut out = String::new();
    let mut open_tags = Vec::new();
    let mut visible = 0;
    let mut cursor = 0;

    while cursor < input.len() && visible < max_visible_chars.saturating_sub(1) {
        let rest = &input[cursor..];
        if let Some(tag) = parse_telegram_tag(rest) {
            track_html_tag(&tag, &mut open_tags);
            out.push_str(&rest[..tag.len]);
            cursor += tag.len;
        } else if let Some(len) = telegram_entity_len(rest) {
            let entity = &rest[..len];
            out.push_str(entity);
            cursor += len;
            visible += 1;
        } else if let Some(ch) = rest.chars().next() {
            out.push(ch);
            cursor += ch.len_utf8();
            visible += 1;
        }
    }

    out.push('…');
    for name in open_tags.iter().rev() {
        out.push_str("</");
        out.push_str(name);
        out.push('>');
    }
    out
}

fn visible_char_count(input: &str) -> usize {
    let mut count = 0;
    let mut cursor = 0;
    while cursor < input.len() {
        let rest = &input[cursor..];
        if let Some(tag) = parse_telegram_tag(rest) {
            cursor += tag.len;
        } else if let Some(len) = telegram_entity_len(rest) {
            cursor += len;
            count += 1;
        } else if let Some(ch) = rest.chars().next() {
            cursor += ch.len_utf8();
            count += 1;
        }
    }
    count
}

struct HtmlTag<'a> {
    name: &'a str,
    attributes: &'a str,
    len: usize,
    closing: bool,
}

fn parse_telegram_tag(input: &str) -> Option<HtmlTag<'_>> {
    let body = input.strip_prefix('<')?;
    let (closing, body) = match body.strip_prefix('/') {
        Some(body) => (true, body),
        None => (false, body),
    };
    let name_len = body.find(|c: char| !c.is_ascii_alphanumeric() && c != '-')?;
    let name = &body[..name_len];
    if !TELEGRAM_TAGS.contains(&name) {
        return None;
    }
    let tail = &body[name_len..];
    let end = tail.find('>')?;
    let attributes = tail[..end].trim();
    if attributes.contains('<')
        || (closing && !attributes.is_empty())
        || (!closing && !valid_attributes(name, attributes))
    {
        return None;
    }
    let prefix = if closing { 2 } else { 1 };
    Some(HtmlTag {
        name,
        attributes,
        len: prefix + name_len + end + 1,
        closing,
    })
}

fn valid_attributes(name: &str, attributes: &str) -> bool {
    match name {
        "a" => quoted_attribute(attributes, "href").is_some(),
        "span" => attributes == "class=\"tg-spoiler\"",
        "tg-emoji" => quoted_attribute(attributes, "emoji-id").is_some(),
        "blockquote" => attributes.is_empty() || attributes == "expandable",
        "code" => {
            attributes.is_empty()
                || quoted_attribute(attributes, "class").is_some_and(|value| {
                    value.strip_prefix("language-").is_some_and(|language| {
                        !language.is_empty()
                            && language.chars().all(|c| {
                                c.is_ascii_alphanumeric()
                                    || matches!(c, '+' | '-' | '_' | '.' | '#')
                            })
                    })
                })
        }
        _ => attributes.is_empty(),
    }
}

fn quoted_attribute<'a>(attributes: &'a str, name: &str) -> Option<&'a str> {
    let value = attributes.strip_prefix(name)?.strip_prefix("=\"")?;
    value.strip_suffix('"').filter(|value| !value.contains('"'))
}

fn is_legacy_telegram_html(input: &str, options: Options) -> bool {
    let mut stack: Vec<String> = Vec::new();
    let mut saw_tag = false;

    for event in Parser::new_ext(input, options) {
        let (Event::Html(html) | Event::InlineHtml(html)) = event else {
            continue;
        };
        let mut raw = html.as_ref();
        while let Some(index) = raw.find('<') {
            raw = &raw[index..];
            let Some(tag) = parse_telegram_tag(raw) else {
                return false;
            };
            saw_tag = true;

            if tag.closing {
                if stack.pop().as_deref() != Some(tag.name) {
                    return false;
                }
            } else {
                if tag.name == "code"
                    && !tag.attributes.is_empty()
                    && stack.last().map(String::as_str) != Some("pre")
                {
                    return false;
                }
                if let Some(parent) = stack.last()
                    && !valid_nesting(parent, tag.name)
                {
                    return false;
                }
                stack.push(tag.name.to_string());
            }
            raw = &raw[tag.len..];
        }
    }

    saw_tag && stack.is_empty()
}

fn valid_nesting(parent: &str, child: &str) -> bool {
    if parent == "pre" && child == "code" {
        return true;
    }
    if matches!(parent, "code" | "pre") || matches!(child, "code" | "pre") {
        return false;
    }
    is_style_tag(parent) || is_style_tag(child)
}

fn is_style_tag(name: &str) -> bool {
    matches!(
        name,
        "b" | "strong" | "i" | "em" | "u" | "ins" | "s" | "strike" | "del" | "span" | "tg-spoiler"
    )
}

fn telegram_entity_len(input: &str) -> Option<usize> {
    let body = input.strip_prefix('&')?;
    let end = body.find(';')?;
    let name = &body[..end];
    let valid = matches!(name, "amp" | "lt" | "gt" | "quot")
        || name
            .strip_prefix('#')
            .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
        || name
            .strip_prefix("#x")
            .or_else(|| name.strip_prefix("#X"))
            .is_some_and(|digits| {
                !digits.is_empty() && digits.chars().all(|c| c.is_ascii_hexdigit())
            });
    valid.then_some(end + 2)
}

fn track_html_tag(tag: &HtmlTag<'_>, open_tags: &mut Vec<String>) {
    if tag.closing {
        if let Some(index) = open_tags.iter().rposition(|open| open == tag.name) {
            open_tags.truncate(index);
        }
    } else {
        open_tags.push(tag.name.to_string());
    }
}

struct InlineTag {
    open: String,
    close: &'static str,
}

#[derive(Default, PartialEq, Eq)]
enum OpenState {
    #[default]
    Closed,
    Open,
}

#[derive(Default)]
struct Renderer {
    out: String,
    pending_newlines: usize,
    /// Suppresses the next block break so a list item or blockquote can hold
    /// its first block on the same line as the marker or opening tag.
    suppress_break: bool,
    list_stack: Vec<Option<u64>>,
    inline_stack: Vec<InlineTag>,
    inline_state: OpenState,
    blockquote_depth: usize,
    blockquote_state: OpenState,
    plain_link_urls: Vec<Option<String>>,
    code_lang: Option<String>,
    code_buf: String,
    in_code_block: bool,
    row_started: bool,
}

impl Renderer {
    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start_tag(tag),
            Event::End(tag) => self.end_tag(tag),
            Event::Text(text) => {
                if self.in_code_block {
                    self.code_buf.push_str(&text);
                } else {
                    self.ensure_blockquote_open();
                    self.push_escaped(&text);
                }
            }
            Event::Code(code) => self.push_inline_code(&code),
            Event::SoftBreak => {
                self.ensure_blockquote_open();
                self.push_raw(" ");
            }
            Event::HardBreak => {
                self.ensure_blockquote_open();
                self.push_raw("\n");
            }
            Event::Html(html) => self.push_escaped_block_html(&html),
            Event::InlineHtml(html) => self.push_escaped(&html),
            Event::Rule => {
                self.block_break(2);
                self.ensure_blockquote_open();
                self.push_raw("———");
                self.pending_newlines = 2;
            }
            Event::TaskListMarker(checked) => {
                self.ensure_blockquote_open();
                self.push_raw(if checked { "[x] " } else { "[ ] " });
            }
            Event::FootnoteReference(_) | Event::InlineMath(_) | Event::DisplayMath(_) => {}
        }
    }

    fn start_tag(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => self.block_break(if self.list_stack.is_empty() { 2 } else { 1 }),
            Tag::Heading { .. } => {
                self.block_break(2);
                self.ensure_blockquote_open();
                self.open_inline("<b>".to_string(), "</b>");
            }
            Tag::CodeBlock(kind) => {
                self.suspend_blockquote();
                self.block_break(2);
                self.in_code_block = true;
                self.code_buf.clear();
                self.code_lang = match kind {
                    CodeBlockKind::Fenced(info) => language_token(&info),
                    CodeBlockKind::Indented => None,
                };
            }
            Tag::List(start) => {
                self.block_break(if self.list_stack.is_empty() { 2 } else { 1 });
                self.list_stack.push(start);
            }
            Tag::Item => {
                self.block_break(1);
                let depth = self.list_stack.len().saturating_sub(1);
                let marker = match self.list_stack.last() {
                    Some(Some(number)) => format!("{number}. "),
                    _ => "- ".to_string(),
                };
                self.ensure_blockquote_open();
                self.push_raw(&format!("{}{marker}", "  ".repeat(depth)));
                self.suppress_break = true;
            }
            Tag::BlockQuote(_) => {
                self.block_break(2);
                if self.blockquote_depth == 0 {
                    self.suppress_break = true;
                }
                self.blockquote_depth += 1;
            }
            Tag::Strong => {
                self.ensure_blockquote_open();
                self.open_inline("<b>".to_string(), "</b>");
            }
            Tag::Emphasis => {
                self.ensure_blockquote_open();
                self.open_inline("<i>".to_string(), "</i>");
            }
            Tag::Strikethrough => {
                self.ensure_blockquote_open();
                self.open_inline("<s>".to_string(), "</s>");
            }
            Tag::Link { dest_url, .. } => {
                if self.blockquote_depth > 0 {
                    self.ensure_blockquote_open();
                    self.plain_link_urls.push(Some(dest_url.to_string()));
                } else {
                    let open = format!("<a href=\"{}\">", escape_attr(&dest_url));
                    self.open_inline(open, "</a>");
                    self.plain_link_urls.push(None);
                }
            }
            Tag::Table(_) | Tag::HtmlBlock => self.block_break(2),
            Tag::TableHead | Tag::TableRow => {
                self.block_break(1);
                self.row_started = false;
            }
            Tag::TableCell => {
                if self.row_started {
                    self.push_raw(" | ");
                }
                self.row_started = true;
            }
            Tag::Image { .. }
            | Tag::Superscript
            | Tag::Subscript
            | Tag::FootnoteDefinition(_)
            | Tag::MetadataBlock(_)
            | Tag::DefinitionList
            | Tag::DefinitionListTitle
            | Tag::DefinitionListDefinition => {}
        }
    }

    fn end_tag(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                self.pending_newlines = if self.list_stack.is_empty() { 2 } else { 1 };
            }
            TagEnd::Heading(_) => {
                self.close_inline();
                self.pending_newlines = 2;
            }
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                let code = std::mem::take(&mut self.code_buf);
                let code = code.strip_suffix('\n').unwrap_or(&code).to_string();
                let lang = self.code_lang.take();
                match &lang {
                    Some(lang) => self.push_raw(&format!("<pre><code class=\"language-{lang}\">")),
                    None => self.push_raw("<pre>"),
                }
                self.push_escaped(&code);
                self.push_raw(if lang.is_some() {
                    "</code></pre>"
                } else {
                    "</pre>"
                });
                if self.blockquote_depth == 0 {
                    self.open_all_inline();
                }
                self.pending_newlines = 2;
            }
            TagEnd::List(_) => {
                self.list_stack.pop();
                self.pending_newlines = if self.list_stack.is_empty() { 2 } else { 1 };
            }
            TagEnd::Item => {
                self.suppress_break = false;
                if let Some(Some(number)) = self.list_stack.last_mut() {
                    *number += 1;
                }
                self.pending_newlines = 1;
            }
            TagEnd::BlockQuote(_) => {
                self.blockquote_depth = self.blockquote_depth.saturating_sub(1);
                if self.blockquote_depth == 0 {
                    self.close_blockquote();
                    self.pending_newlines = 2;
                }
            }
            TagEnd::Link => match self.plain_link_urls.pop().flatten() {
                Some(url) => {
                    self.push_raw(" (");
                    self.push_escaped(&url);
                    self.push_raw(")");
                }
                None => self.close_inline(),
            },
            TagEnd::Strong | TagEnd::Emphasis | TagEnd::Strikethrough => {
                self.close_inline();
            }
            TagEnd::Table => self.pending_newlines = 2,
            TagEnd::TableHead | TagEnd::TableRow => self.pending_newlines = 1,
            _ => {}
        }
    }

    /// Inline code cannot contain or sit inside another entity, so any open
    /// formatting is closed before it and reopened after.
    fn push_inline_code(&mut self, code: &str) {
        if self.blockquote_depth > 0 {
            self.ensure_blockquote_open();
            self.push_escaped(code);
            return;
        }
        self.close_all_inline();
        self.push_raw("<code>");
        self.push_escaped(code);
        self.push_raw("</code>");
        if self.blockquote_depth == 0 {
            self.open_all_inline();
        }
    }

    fn open_inline(&mut self, open: String, close: &'static str) {
        self.push_raw(&open);
        self.inline_stack.push(InlineTag { open, close });
        self.inline_state = OpenState::Open;
    }

    fn close_inline(&mut self) {
        if let Some(tag) = self.inline_stack.pop()
            && self.inline_state == OpenState::Open
        {
            self.push_raw(tag.close);
        }
        if self.inline_stack.is_empty() {
            self.inline_state = OpenState::Closed;
        }
    }

    fn close_all_inline(&mut self) {
        if self.inline_state == OpenState::Closed {
            return;
        }
        let closes: Vec<&'static str> = self
            .inline_stack
            .iter()
            .rev()
            .map(|tag| tag.close)
            .collect();
        for close in closes {
            self.push_raw(close);
        }
        self.inline_state = OpenState::Closed;
    }

    fn open_all_inline(&mut self) {
        if self.inline_state == OpenState::Open || self.inline_stack.is_empty() {
            return;
        }
        let opens: Vec<String> = self
            .inline_stack
            .iter()
            .map(|tag| tag.open.clone())
            .collect();
        for open in opens {
            self.push_raw(&open);
        }
        self.inline_state = OpenState::Open;
    }

    fn ensure_blockquote_open(&mut self) {
        if self.blockquote_depth == 0 || self.blockquote_state == OpenState::Open {
            self.open_all_inline();
            return;
        }
        self.push_raw("<blockquote>");
        self.blockquote_state = OpenState::Open;
        self.open_all_inline();
    }

    fn suspend_blockquote(&mut self) {
        self.close_all_inline();
        self.close_blockquote();
    }

    fn close_blockquote(&mut self) {
        if self.blockquote_state == OpenState::Open {
            self.pending_newlines = 0;
            self.push_raw("</blockquote>");
            self.blockquote_state = OpenState::Closed;
        }
    }

    fn push_escaped_block_html(&mut self, html: &str) {
        let trimmed = html.trim_end_matches('\n');
        let stripped = html.len() - trimmed.len();
        self.push_escaped(trimmed);
        if !self.out.is_empty() && stripped > 0 {
            self.pending_newlines = stripped.min(2);
        }
    }

    fn block_break(&mut self, newlines: usize) {
        if self.suppress_break {
            self.suppress_break = false;
            return;
        }
        if self.out.is_empty() {
            return;
        }
        self.pending_newlines = self.pending_newlines.max(newlines);
    }

    fn push_raw(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        for _ in 0..std::mem::take(&mut self.pending_newlines) {
            self.out.push('\n');
        }
        self.out.push_str(text);
    }

    fn push_escaped(&mut self, text: &str) {
        self.push_raw(&escape_text(text));
    }

    fn finish(mut self) -> String {
        if self.inline_state == OpenState::Open {
            while let Some(tag) = self.inline_stack.pop() {
                self.out.push_str(tag.close);
            }
        }
        if self.blockquote_state == OpenState::Open {
            self.out.push_str("</blockquote>");
        }
        self.out.trim().to_string()
    }
}

/// Extracts a usable language name from a fence info string.
fn language_token(info: &str) -> Option<String> {
    let token = info.split_whitespace().next()?;
    let valid = !token.is_empty()
        && token.len() <= 32
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '_' | '.' | '#'));
    valid.then(|| token.to_ascii_lowercase())
}

fn escape_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_attr(text: &str) -> String {
    escape_text(text).replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::{to_telegram_html, truncate_telegram_html};

    #[test]
    fn maps_inline_formatting() {
        assert_eq!(
            to_telegram_html("**bold** and *italic* and `code`"),
            "<b>bold</b> and <i>italic</i> and <code>code</code>"
        );
    }

    #[test]
    fn renders_heading_as_bold_line() {
        assert_eq!(
            to_telegram_html("## Answer\n\nBody text"),
            "<b>Answer</b>\n\nBody text"
        );
    }

    #[test]
    fn renders_link_with_escaped_href() {
        assert_eq!(
            to_telegram_html("[link](https://example.com/?a=1&b=2)"),
            "<a href=\"https://example.com/?a=1&amp;b=2\">link</a>"
        );
    }

    #[test]
    fn renders_fenced_code_block_with_language() {
        assert_eq!(
            to_telegram_html("```rust\nlet a = 1 < 2;\n```"),
            "<pre><code class=\"language-rust\">let a = 1 &lt; 2;</code></pre>"
        );
    }

    #[test]
    fn renders_fenced_code_block_without_language() {
        assert_eq!(
            to_telegram_html("```\njust code\n```"),
            "<pre>just code</pre>"
        );
    }

    #[test]
    fn preserves_blank_lines_inside_fenced_code() {
        assert_eq!(to_telegram_html("```\na\n\nb\n```"), "<pre>a\n\nb</pre>");
    }

    #[test]
    fn renders_flat_bullet_list() {
        assert_eq!(
            to_telegram_html("Intro\n\n- one\n- two\n\nOutro"),
            "Intro\n\n- one\n- two\n\nOutro"
        );
    }

    #[test]
    fn renders_ordered_list_with_numbers() {
        assert_eq!(to_telegram_html("1. one\n2. two"), "1. one\n2. two");
    }

    #[test]
    fn indents_nested_list_items() {
        assert_eq!(
            to_telegram_html("- one\n  - nested\n- two"),
            "- one\n  - nested\n- two"
        );
    }

    #[test]
    fn renders_blockquote() {
        assert_eq!(
            to_telegram_html("> quoted line\n\nafter"),
            "<blockquote>quoted line</blockquote>\n\nafter"
        );
    }

    #[test]
    fn flattens_nested_blockquotes() {
        assert_eq!(
            to_telegram_html("> outer\n>\n> > inner"),
            "<blockquote>outer\n\ninner</blockquote>"
        );
    }

    #[test]
    fn suspends_blockquote_around_code_block() {
        assert_eq!(to_telegram_html("> ```\n> code\n> ```"), "<pre>code</pre>");
    }

    #[test]
    fn renders_inline_code_as_plain_text_inside_blockquote() {
        assert_eq!(
            to_telegram_html("> before `code` after"),
            "<blockquote>before code after</blockquote>"
        );
    }

    #[test]
    fn keeps_blockquote_formatting_continuous_around_inline_code() {
        assert_eq!(
            to_telegram_html("> **before `code` after**"),
            "<blockquote><b>before code after</b></blockquote>"
        );
    }

    #[test]
    fn renders_link_label_and_url_as_plain_text_inside_blockquote() {
        assert_eq!(
            to_telegram_html("> before [link](https://example.com) after"),
            "<blockquote>before link (https://example.com) after</blockquote>"
        );
    }

    #[test]
    fn keeps_many_inline_code_spans_in_one_blockquote() {
        assert_eq!(
            to_telegram_html(
                "> `account.createAccountProof` takes `productAccountId` with `dotNsIdentifier`."
            ),
            concat!(
                "<blockquote>account.createAccountProof takes productAccountId with ",
                "dotNsIdentifier.</blockquote>"
            )
        );
    }

    #[test]
    fn closes_and_reopens_bold_around_inline_code() {
        assert_eq!(
            to_telegram_html("**bold with `code` inside**"),
            "<b>bold with </b><code>code</code><b> inside</b>"
        );
    }

    #[test]
    fn closes_and_reopens_link_around_inline_code() {
        assert_eq!(
            to_telegram_html("[run `zdx` now](https://example.com)"),
            concat!(
                "<a href=\"https://example.com\">run </a>",
                "<code>zdx</code>",
                "<a href=\"https://example.com\"> now</a>"
            )
        );
    }

    #[test]
    fn escapes_stray_angle_brackets_and_ampersands() {
        assert_eq!(
            to_telegram_html("very old (<Android 8) devices & more"),
            "very old (&lt;Android 8) devices &amp; more"
        );
    }

    #[test]
    fn escapes_commonmark_raw_html_as_literal_text() {
        assert_eq!(to_telegram_html("Vec<T>"), "Vec&lt;T&gt;");
        assert_eq!(
            to_telegram_html("<task>do this</task>"),
            "&lt;task&gt;do this&lt;/task&gt;"
        );
    }

    #[test]
    fn escapes_entities_inside_code_span_exactly_once() {
        assert_eq!(
            to_telegram_html("`a &amp; b`"),
            "<code>a &amp;amp; b</code>"
        );
    }

    #[test]
    fn renders_table_as_plain_lines() {
        assert_eq!(
            to_telegram_html("| a | b |\n| - | - |\n| 1 | 2 |"),
            "a | b\n1 | 2"
        );
    }

    #[test]
    fn keeps_image_alt_text_only() {
        assert_eq!(to_telegram_html("![alt text](img.png)"), "alt text");
    }

    #[test]
    fn passes_legacy_html_replies_through_unchanged() {
        let input = concat!(
            "<b>Answer:</b> Use <code>git rebase -i HEAD~3</code>.\n\n",
            "- Pick the commits to squash\n",
            "- Save and close the editor\n"
        );
        assert_eq!(
            to_telegram_html(input),
            concat!(
                "<b>Answer:</b> Use <code>git rebase -i HEAD~3</code>.\n\n",
                "- Pick the commits to squash\n",
                "- Save and close the editor"
            )
        );
    }

    #[test]
    fn passes_inline_legacy_html_through() {
        assert_eq!(
            to_telegram_html("Details here <i>Full details attached ↓</i>"),
            "Details here <i>Full details attached ↓</i>"
        );
    }

    #[test]
    fn legacy_html_bypass_preserves_markdown_markers_inside_code() {
        let input = "<code>*literal*</code>";
        assert_eq!(to_telegram_html(input), input);
    }

    #[test]
    fn malformed_or_mixed_legacy_html_is_escaped() {
        assert_eq!(to_telegram_html("<b>open"), "&lt;b&gt;open");
        assert_eq!(
            to_telegram_html("<b>legacy</b><task>x</task>"),
            "&lt;b&gt;legacy&lt;/b&gt;&lt;task&gt;x&lt;/task&gt;"
        );
        assert_eq!(
            to_telegram_html("<blockquote><code>x</code></blockquote>"),
            "&lt;blockquote&gt;&lt;code&gt;x&lt;/code&gt;&lt;/blockquote&gt;"
        );
    }

    #[test]
    fn legacy_html_requires_canonical_attributes() {
        assert_eq!(to_telegram_html("<a>link</a>"), "&lt;a&gt;link&lt;/a&gt;");
        assert_eq!(
            to_telegram_html("<span>secret</span>"),
            "&lt;span&gt;secret&lt;/span&gt;"
        );
        assert_eq!(
            to_telegram_html("<b arbitrary>bold</b>"),
            "&lt;b arbitrary&gt;bold&lt;/b&gt;"
        );
    }

    #[test]
    fn canonical_legacy_attributes_bypass_conversion() {
        for input in [
            "<a href=\"https://example.com\">link</a>",
            "<span class=\"tg-spoiler\">secret</span>",
            "<blockquote expandable>quote</blockquote>",
            "<tg-emoji emoji-id=\"123\">🙂</tg-emoji>",
            "<pre><code class=\"language-rust\">fn main() {}</code></pre>",
        ] {
            assert_eq!(to_telegram_html(input), input);
        }
    }

    #[test]
    fn collapses_extra_blank_lines_between_blocks() {
        assert_eq!(to_telegram_html("a\n\n\n\nb"), "a\n\nb");
    }

    #[test]
    fn soft_breaks_become_spaces() {
        assert_eq!(to_telegram_html("one\ntwo"), "one two");
    }

    #[test]
    fn is_idempotent_on_converted_output() {
        let source = "**bold** `code`\n\n- item\n\n> quote\n\n```rust\nfn main() {}\n```";
        let once = to_telegram_html(source);
        assert_eq!(to_telegram_html(&once), once);
    }

    #[test]
    fn truncates_by_visible_characters_and_closes_tags() {
        assert_eq!(
            truncate_telegram_html("<b>hello &amp; goodbye</b>", 9),
            "<b>hello &amp; …</b>"
        );
    }

    #[test]
    fn truncation_never_splits_entities_or_nested_tags() {
        assert_eq!(
            truncate_telegram_html("<blockquote><b>one &lt; two</b></blockquote>", 7),
            "<blockquote><b>one &lt; …</b></blockquote>"
        );
    }

    #[test]
    fn truncation_counts_unknown_html_like_text_as_visible() {
        assert_eq!(
            truncate_telegram_html("<task>abcdefghij</task>", 8),
            "<task>a…"
        );
    }

    #[test]
    fn truncation_counts_noncanonical_telegram_tags_as_visible() {
        assert_eq!(truncate_telegram_html("<a>abcdefghij</a>", 8), "<a>abcd…");
    }

    #[test]
    fn leaves_short_telegram_html_unchanged() {
        let input = "<b>short</b>";
        assert_eq!(truncate_telegram_html(input, 10), input);
    }
}
