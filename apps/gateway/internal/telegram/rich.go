package telegram

import (
	"bytes"
	"encoding/json"
	"strconv"
	"strings"
	"unicode"
)

// Rich messages (Bot API 10.1) carry no text: their words live in
// rich_message.blocks. This flattens them to plain text exactly as the Rust
// bot does (crates/zdx-bot/src/telegram/rich.rs). Blocks stay untyped on
// purpose: Telegram keeps adding block and text types, and an unknown one
// must degrade to its nested text instead of failing or dropping what the
// user wrote.

// richContentKeys carry user-visible content in any rich block or rich text
// object. The fallback walks only these, in this order, so file ids, URLs
// and layout hints never reach the prompt.
var richContentKeys = []string{"text", "summary", "caption", "credit", "blocks", "items", "cells", "buttons", "label", "expression"}

// PlainText is the text the sender wrote: Text, else the flattened rich
// message. nil when there is neither (Rust Message::plain_text).
func (m *Message) PlainText() *string {
	if m.Text != nil {
		return m.Text
	}
	return RichText(m.RichMessage)
}

// RichText flattens a rich_message object to Markdown-ish plain text; nil
// when it has no text or does not parse.
func RichText(raw json.RawMessage) *string {
	if len(raw) == 0 {
		return nil
	}
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	var msg struct {
		Blocks []any `json:"blocks"`
	}
	if dec.Decode(&msg) != nil {
		return nil
	}
	var parts []string
	for _, b := range msg.Blocks {
		if s, ok := renderBlock(b); ok {
			parts = append(parts, s)
		}
	}
	text := strings.TrimSpace(strings.Join(parts, "\n\n"))
	if text == "" {
		return nil
	}
	return &text
}

func field(v any, key string) any {
	if m, ok := v.(map[string]any); ok {
		return m[key]
	}
	return nil
}

func stringField(v any, key string) string {
	s, _ := field(v, key).(string)
	return s
}

func flag(v any, key string) bool {
	b, _ := field(v, key).(bool)
	return b
}

func blank(s string) bool { return strings.TrimSpace(s) == "" }

func renderBlock(block any) (string, bool) {
	kind := stringField(block, "type")
	var out string
	switch kind {
	case "paragraph", "footer":
		out = richTextAt(block, "text")
	case "heading":
		out = renderHeading(block)
	case "pre":
		out = renderPre(block)
	case "divider":
		out = "---"
	case "anchor":
	case "list":
		out = renderList(block)
	case "blockquote":
		out = renderQuote(renderNestedBlocks(block), richTextAt(block, "credit"))
	case "expandable_blockquote", "pullquote":
		out = renderQuote(richTextAt(block, "text"), richTextAt(block, "credit"))
	case "details":
		out = joinLines(richTextAt(block, "summary"), renderNestedBlocks(block))
	case "table":
		out = renderTable(block)
	case "collage", "slideshow":
		out = joinLines(renderNestedBlocks(block), richTextAt(block, "caption"))
	case "animation", "audio", "document", "map", "photo", "video", "voice_note":
		out = renderMedia(kind, block)
	default:
		out = nestedContent(block)
	}
	if blank(out) {
		return "", false
	}
	return strings.TrimRightFunc(out, unicode.IsSpace), true
}

// asU64 is serde_json's Value::as_u64: a non-negative integer literal.
func asU64(v any) (uint64, bool) {
	n, ok := v.(json.Number)
	if !ok {
		return 0, false
	}
	u, err := strconv.ParseUint(n.String(), 10, 64)
	return u, err == nil
}

func renderHeading(block any) string {
	text := richTextAt(block, "text")
	if blank(text) {
		return ""
	}
	size, ok := asU64(field(block, "size"))
	if !ok {
		size = 1
	}
	size = min(max(size, 1), 6)
	return strings.Repeat("#", int(size)) + " " + text
}

func renderPre(block any) string {
	text := richTextAt(block, "text")
	if blank(text) {
		return ""
	}
	return "```" + stringField(block, "language") + "\n" + text + "\n```"
}

func renderList(block any) string {
	items, ok := field(block, "items").([]any)
	if !ok {
		return nestedContent(block)
	}
	var out []string
	for _, item := range items {
		if s := renderListItem(item); s != "" {
			out = append(out, s)
		}
	}
	return strings.Join(out, "\n")
}

func renderListItem(item any) string {
	content := renderNestedBlocks(item)
	if blank(content) {
		return ""
	}
	var marker string
	switch {
	case flag(item, "has_checkbox") && flag(item, "is_checked"):
		marker = "- [x]"
	case flag(item, "has_checkbox"):
		marker = "- [ ]"
	default:
		marker = strings.TrimSpace(stringField(item, "label"))
		if marker == "" {
			marker = "-"
		}
	}
	lines := rustLines(content)
	first := ""
	if len(lines) > 0 {
		first = lines[0]
	}
	var b strings.Builder
	b.WriteString(marker + " " + first)
	for _, line := range lines[min(1, len(lines)):] {
		b.WriteString("\n  " + line)
	}
	return b.String()
}

// rustLines is Rust's str::lines: split on \n, drop one trailing \r per
// line, and no final empty line.
func rustLines(s string) []string {
	if s == "" {
		return nil
	}
	lines := strings.Split(s, "\n")
	if lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	for i, l := range lines {
		lines[i] = strings.TrimSuffix(l, "\r")
	}
	return lines
}

func renderQuote(content, credit string) string {
	if blank(content) {
		return ""
	}
	lines := rustLines(content)
	for i, l := range lines {
		lines[i] = "> " + l
	}
	quoted := strings.Join(lines, "\n")
	if !blank(credit) {
		quoted += "\n> — " + credit
	}
	return quoted
}

func renderTable(block any) string {
	rows, ok := field(block, "cells").([]any)
	if !ok {
		return nestedContent(block)
	}
	var lines []string
	for i, row := range rows {
		cells, ok := row.([]any)
		if !ok || len(cells) == 0 {
			continue
		}
		rendered := make([]string, len(cells))
		header := false
		for j, cell := range cells {
			rendered[j] = strings.ReplaceAll(richTextAt(cell, "text"), "\n", " ")
			header = header || flag(cell, "is_header")
		}
		lines = append(lines, "| "+strings.Join(rendered, " | ")+" |")
		if i == 0 && header {
			sep := make([]string, len(rendered))
			for j := range sep {
				sep[j] = "---"
			}
			lines = append(lines, "| "+strings.Join(sep, " | ")+" |")
		}
	}
	return joinLines(strings.Join(lines, "\n"), richTextAt(block, "caption"))
}

// renderMedia names media inside a rich message, which is not downloaded:
// a caption alone would hide that media was sent.
func renderMedia(kind string, block any) string {
	label := strings.ReplaceAll(kind, "_", " ")
	caption := richTextAt(block, "caption")
	if blank(caption) {
		return "[" + label + "]"
	}
	return "[" + label + "] " + caption
}

func renderNestedBlocks(v any) string {
	blocks, ok := field(v, "blocks").([]any)
	if !ok {
		return ""
	}
	var out []string
	for _, b := range blocks {
		if s, ok := renderBlock(b); ok {
			out = append(out, s)
		}
	}
	return strings.Join(out, "\n")
}

// nestedContent is the text of an unknown block: whatever its content keys
// hold.
func nestedContent(v any) string {
	var parts []string
	collectContent(v, &parts)
	return strings.Join(parts, "\n")
}

func collectContent(v any, out *[]string) {
	switch t := v.(type) {
	case string:
		if !blank(t) {
			*out = append(*out, t)
		}
	case []any:
		for _, item := range t {
			collectContent(item, out)
		}
	case map[string]any:
		for _, key := range richContentKeys {
			if child, ok := t[key]; ok {
				collectContent(child, out)
			}
		}
	}
}

func richTextAt(v any, key string) string {
	child, ok := v.(map[string]any)
	if !ok {
		return ""
	}
	value, ok := child[key]
	if !ok {
		return ""
	}
	return renderRichText(value)
}

func joinLines(parts ...string) string {
	var out []string
	for _, p := range parts {
		if !blank(p) {
			out = append(out, p)
		}
	}
	return strings.Join(out, "\n")
}

// renderRichText is a string, an array of rich texts, or an object wrapping
// more rich text with a style. Only the words survive, except where the
// payload is the content (LaTeX, custom emoji fallback, link targets).
func renderRichText(v any) string {
	switch t := v.(type) {
	case string:
		return t
	case []any:
		var b strings.Builder
		for _, item := range t {
			b.WriteString(renderRichText(item))
		}
		return b.String()
	case map[string]any:
		kind, _ := t["type"].(string)
		switch kind {
		case "mathematical_expression":
			return stringField(t, "expression")
		case "custom_emoji":
			return stringField(t, "alternative_text")
		case "anchor":
			return ""
		case "url":
			text := ""
			if inner, ok := t["text"]; ok {
				text = renderRichText(inner)
			}
			url := stringField(t, "url")
			if url == "" || strings.TrimSpace(text) == url {
				return text
			}
			return text + " (" + url + ")"
		default:
			if inner, ok := t["text"]; ok {
				return renderRichText(inner)
			}
			return ""
		}
	}
	return ""
}
