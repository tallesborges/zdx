// Package render turns model reply text into Telegram output: Markdown to
// Telegram HTML, HTML repair, length checks, message splitting, keyboards,
// and the plan of messages one reply is delivered as.
//
// It is a port of the Rust bot's presentation helpers (crates/zdx-bot:
// telegram/markdown.rs, telegram/html.rs, handlers/message/media.rs,
// followups.rs, file_card.rs, handlers/message/response.rs). It does no I/O
// and derives no engine data: follow-up extraction, `<media>` path cleaning,
// artifact keys and artifact kinds come from Rust (see Reply).
package render

import (
	"strings"
	"unicode/utf8"
)

// Telegram limits, in characters after entity parsing (UTF-16 code units).
const (
	// MaxMessageLen is the sendMessage text limit. Longer reply text is split
	// with SplitHTML.
	MaxMessageLen = 4096
	// MaxCaptionLen is the media caption limit, measured with VisibleUTF16Len.
	MaxCaptionLen = 1024
)

// telegramTags are the tags Telegram accepts with parse_mode=HTML.
var telegramTags = map[string]bool{
	"b": true, "strong": true, "i": true, "em": true, "u": true, "ins": true,
	"s": true, "strike": true, "del": true, "span": true, "a": true,
	"code": true, "pre": true, "blockquote": true, "tg-spoiler": true,
	"tg-emoji": true,
}

// maxEntityLen is the longest entity SplitHTML keeps whole. A longer `&#…;`
// run is split like text; it is never a real character reference.
const maxEntityLen = 12

// EscapeHTML escapes text for a Telegram HTML message body.
func EscapeHTML(s string) string {
	s = strings.ReplaceAll(s, "&", "&amp;")
	s = strings.ReplaceAll(s, "<", "&lt;")
	return strings.ReplaceAll(s, ">", "&gt;")
}

func escapeAttr(s string) string {
	return strings.ReplaceAll(EscapeHTML(s), `"`, "&quot;")
}

type htmlTag struct {
	name    string
	attrs   string
	n       int // byte length of the whole tag
	closing bool
}

// scanTag parses a supported tag at the start of s. Strict mode also checks
// attributes the way Telegram expects them (markdown.rs parse_telegram_tag);
// loose mode accepts any attributes on an opening tag (html.rs parse_tag).
func scanTag(s string, strict bool) (htmlTag, bool) {
	body, ok := strings.CutPrefix(s, "<")
	if !ok {
		return htmlTag{}, false
	}
	body, closing := strings.CutPrefix(body, "/")
	nameLen := strings.IndexFunc(body, func(r rune) bool {
		return !isASCIIAlnum(r) && r != '-'
	})
	if nameLen < 0 {
		return htmlTag{}, false
	}
	name := body[:nameLen]
	if !telegramTags[name] {
		return htmlTag{}, false
	}
	tail := body[nameLen:]
	end := strings.IndexByte(tail, '>')
	if end < 0 {
		return htmlTag{}, false
	}
	inner := tail[:end]
	attrs := strings.TrimSpace(inner)
	if strings.Contains(inner, "<") || (closing && attrs != "") {
		return htmlTag{}, false
	}
	if strict && !closing && !validAttributes(name, attrs) {
		return htmlTag{}, false
	}
	prefix := 1
	if closing {
		prefix = 2
	}
	return htmlTag{name: name, attrs: attrs, n: prefix + nameLen + end + 1, closing: closing}, true
}

func parseTelegramTag(s string) (htmlTag, bool) { return scanTag(s, true) }

func validAttributes(name, attrs string) bool {
	switch name {
	case "a":
		_, ok := quotedAttribute(attrs, "href")
		return ok
	case "span":
		return attrs == `class="tg-spoiler"`
	case "tg-emoji":
		_, ok := quotedAttribute(attrs, "emoji-id")
		return ok
	case "blockquote":
		return attrs == "" || attrs == "expandable"
	case "code":
		if attrs == "" {
			return true
		}
		value, ok := quotedAttribute(attrs, "class")
		if !ok {
			return false
		}
		lang, ok := strings.CutPrefix(value, "language-")
		return ok && lang != "" && allRunes(lang, isLanguageRune)
	default:
		return attrs == ""
	}
}

func quotedAttribute(attrs, name string) (string, bool) {
	rest, ok := strings.CutPrefix(attrs, name)
	if !ok {
		return "", false
	}
	rest, ok = strings.CutPrefix(rest, `="`)
	if !ok {
		return "", false
	}
	value, ok := strings.CutSuffix(rest, `"`)
	if !ok || strings.Contains(value, `"`) {
		return "", false
	}
	return value, true
}

// entityLen is the byte length of a Telegram-supported entity at the start
// of s (`&amp;`, `&lt;`, `&gt;`, `&quot;`, decimal or hex numeric), or 0.
func entityLen(s string) int {
	body, ok := strings.CutPrefix(s, "&")
	if !ok {
		return 0
	}
	end := strings.IndexByte(body, ';')
	if end < 0 {
		return 0
	}
	name := body[:end]
	valid := false
	switch {
	case name == "amp" || name == "lt" || name == "gt" || name == "quot":
		valid = true
	case strings.HasPrefix(name, "#x") || strings.HasPrefix(name, "#X"):
		digits := name[2:]
		valid = digits != "" && allRunes(digits, isASCIIHex)
	case strings.HasPrefix(name, "#"):
		digits := name[1:]
		valid = digits != "" && allRunes(digits, isASCIIDigit)
	}
	if !valid {
		return 0
	}
	return end + 2
}

// runeLen is the byte length of the first character of s (1 for invalid
// UTF-8, so scanning always advances).
func runeLen(s string) int {
	_, n := utf8.DecodeRuneInString(s)
	if n == 0 {
		return 1
	}
	return n
}

// UTF16Len is the length of s in UTF-16 code units, the unit Telegram
// limits are counted in.
func UTF16Len(s string) int {
	n := 0
	for _, r := range s {
		if r > 0xFFFF {
			n += 2
		} else {
			n++
		}
	}
	return n
}

// VisibleUTF16Len is an upper bound on Telegram's "characters after entities
// parsing" for HTML: tags are dropped and text is counted in UTF-16 code
// units. Entities count as 2, so a numeric entity outside the BMP is never
// undercounted.
func VisibleUTF16Len(s string) int {
	count := 0
	for cursor := 0; cursor < len(s); {
		rest := s[cursor:]
		if tag, ok := parseTelegramTag(rest); ok {
			cursor += tag.n
		} else if n := entityLen(rest); n > 0 {
			cursor += n
			count += 2
		} else {
			n := runeLen(rest)
			count += UTF16Len(rest[:n])
			cursor += n
		}
	}
	return count
}

func visibleCharCount(s string) int {
	count := 0
	for cursor := 0; cursor < len(s); {
		rest := s[cursor:]
		if tag, ok := parseTelegramTag(rest); ok {
			cursor += tag.n
		} else if n := entityLen(rest); n > 0 {
			cursor += n
			count++
		} else {
			cursor += runeLen(rest)
			count++
		}
	}
	return count
}

// TruncateHTML truncates rendered Telegram HTML to at most maxVisible
// visible characters (an ending "…" included) without splitting tags or
// entities, then closes any formatting left open. Input that fits is
// returned unchanged.
func TruncateHTML(s string, maxVisible int) string {
	if visibleCharCount(s) <= maxVisible {
		return s
	}
	var out strings.Builder
	var open []string
	visible := 0
	for cursor := 0; cursor < len(s) && visible < maxVisible-1; {
		rest := s[cursor:]
		if tag, ok := parseTelegramTag(rest); ok {
			open = trackTag(tag, open)
			out.WriteString(rest[:tag.n])
			cursor += tag.n
		} else if n := entityLen(rest); n > 0 {
			out.WriteString(rest[:n])
			cursor += n
			visible++
		} else {
			n := runeLen(rest)
			out.WriteString(rest[:n])
			cursor += n
			visible++
		}
	}
	out.WriteString("…")
	for i := len(open) - 1; i >= 0; i-- {
		out.WriteString("</" + open[i] + ">")
	}
	return out.String()
}

func trackTag(tag htmlTag, open []string) []string {
	if !tag.closing {
		return append(open, tag.name)
	}
	for i := len(open) - 1; i >= 0; i-- {
		if open[i] == tag.name {
			return open[:i]
		}
	}
	return open
}

// Sanitize escapes everything that is not a well-formed, Telegram-supported
// tag or entity, and balances tags that were left open or crossed. It is the
// repair step after Telegram refuses a message's HTML (telegram.Client
// Repair), not a converter: Markdown goes through ToTelegramHTML.
func Sanitize(s string) string {
	var out strings.Builder
	out.Grow(len(s) + 16)
	var open []string
	rest := s
	for {
		pos := strings.IndexAny(rest, "<>&")
		if pos < 0 {
			break
		}
		out.WriteString(rest[:pos])
		tail := rest[pos:]
		consumed := 1
		switch tail[0] {
		case '<':
			tag, ok := scanTag(tail, false)
			switch {
			case !ok:
				out.WriteString("&lt;")
			case tag.closing:
				consumed, open = closeTag(&out, open, tag, tail)
			default:
				open = append(open, tag.name)
				out.WriteString(tail[:tag.n])
				consumed = tag.n
			}
		case '>':
			out.WriteString("&gt;")
		default:
			if n := entityLen(tail); n > 0 {
				out.WriteString(tail[:n])
				consumed = n
			} else {
				out.WriteString("&amp;")
			}
		}
		rest = tail[consumed:]
	}
	out.WriteString(rest)
	for i := len(open) - 1; i >= 0; i-- {
		out.WriteString("</" + open[i] + ">")
	}
	return out.String()
}

// closeTag emits a closing tag, first closing anything opened inside it so
// crossed tags (`<b><i>x</b>`) come out properly nested. A closing tag with
// nothing to close is escaped.
func closeTag(out *strings.Builder, open []string, tag htmlTag, tail string) (int, []string) {
	index := -1
	for i := len(open) - 1; i >= 0; i-- {
		if open[i] == tag.name {
			index = i
			break
		}
	}
	if index < 0 {
		out.WriteString("&lt;")
		return 1, open
	}
	for i := len(open) - 1; i > index; i-- {
		out.WriteString("</" + open[i] + ">")
	}
	out.WriteString(tail[:tag.n])
	return tag.n, open[:index]
}

func isASCIIAlnum(r rune) bool {
	return (r >= 'a' && r <= 'z') || (r >= 'A' && r <= 'Z') || isASCIIDigit(r)
}

func isASCIIDigit(r rune) bool { return r >= '0' && r <= '9' }

func isASCIIHex(r rune) bool {
	return isASCIIDigit(r) || (r >= 'a' && r <= 'f') || (r >= 'A' && r <= 'F')
}

func isLanguageRune(r rune) bool {
	return isASCIIAlnum(r) || strings.ContainsRune("+-_.#", r)
}

func allRunes(s string, f func(rune) bool) bool {
	for _, r := range s {
		if !f(r) {
			return false
		}
	}
	return true
}
