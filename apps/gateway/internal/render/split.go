package render

import "strings"

type openTag struct {
	name string
	raw  string
}

type cut struct {
	end  int       // byte offset where the part's text ends
	open []openTag // tags open at end
}

// SplitHTML splits rendered Telegram HTML into parts of at most maxUTF16
// UTF-16 units each. Tags and entities count in full, so a part also fits
// when Telegram refuses its HTML and the plain-text fallback sends it raw.
// A part ends after the last line break that keeps it at least half full,
// else after the last such space, else between two characters; never inside
// a tag or an entity. The break character stays at the end of its part, so
// the text of the parts, read in order, is exactly the input's text.
// Formatting open at a break is closed at the end of the part and reopened
// with the same opening tag at the start of the next, so every part is valid
// on its own.
//
// When formatting cannot fit at all (one tag longer than a part, or nesting
// whose reopening and closing tags leave no room), the rest of the text is
// split without its tags: the words stay, that formatting is lost. Parts
// with no visible text are dropped, since Telegram refuses an empty message;
// only whitespace can be lost that way. Text that fits is returned
// unchanged.
func SplitHTML(s string, maxUTF16 int) []string {
	if UTF16Len(s) <= maxUTF16 {
		return []string{s}
	}
	var parts []string
	var open []openTag
	source := s
	pos := 0
	for {
		rest := source[pos:]
		var reopen strings.Builder
		for _, tag := range open {
			reopen.WriteString(tag.raw)
		}
		reopenLen := UTF16Len(reopen.String())
		if reopenLen+UTF16Len(rest) <= maxUTF16 {
			return pushPart(parts, reopen.String()+rest)
		}
		c, ok := findCut(rest, open, reopenLen, maxUTF16)
		if !ok {
			if len(open) == 0 && !hasTags(rest) {
				// Only when maxUTF16 is below one character: nothing fits.
				return parts
			}
			source = stripTags(reopen.String() + rest)
			pos = 0
			open = nil
			continue
		}
		part := reopen.String() + rest[:c.end]
		for i := len(c.open) - 1; i >= 0; i-- {
			part += "</" + c.open[i].name + ">"
		}
		parts = pushPart(parts, part)
		pos += c.end
		if pos >= len(source) {
			return parts
		}
		open = c.open
	}
}

// findCut finds the cut for the next part of rest, or false when not even
// its first token fits with the formatting that must be reopened and closed.
func findCut(rest string, openAtStart []openTag, reopenLen, limit int) (cut, bool) {
	closers := func(open []openTag) int {
		n := 0
		for _, tag := range open {
			n += len(tag.name) + 3
		}
		return n
	}
	open := append([]openTag(nil), openAtStart...)
	used := reopenLen
	var line, space, last *cut
	for cursor := 0; cursor < len(rest); {
		tail := rest[cursor:]
		tag, isTag := parseTelegramTag(tail)
		var n int
		switch {
		case isTag:
			n = tag.n
		case entityLen(tail) > 0 && entityLen(tail) <= maxEntityLen:
			n = entityLen(tail)
		default:
			n = runeLen(tail)
		}
		token := tail[:n]
		used += UTF16Len(token)
		if isTag {
			if tag.closing {
				for i := len(open) - 1; i >= 0; i-- {
					if open[i].name == tag.name {
						open = open[:i]
						break
					}
				}
			} else {
				open = append(open, openTag{name: tag.name, raw: token})
			}
		}
		if used+closers(open) > limit {
			break
		}
		cursor += n
		c := &cut{end: cursor, open: append([]openTag(nil), open...)}
		switch token {
		case "\n":
			line = c
		case " ":
			space = c
		}
		last = c
	}
	if last == nil {
		return cut{}, false
	}
	halfFull := func(c *cut) bool { return c != nil && c.end*2 >= last.end }
	switch {
	case halfFull(line):
		return *line, true
	case halfFull(space):
		return *space, true
	default:
		return *last, true
	}
}

func hasTags(s string) bool {
	for cursor := 0; cursor < len(s); cursor += runeLen(s[cursor:]) {
		if _, ok := parseTelegramTag(s[cursor:]); ok {
			return true
		}
	}
	return false
}

// stripTags is s without its Telegram tags: text and entities only.
func stripTags(s string) string {
	var out strings.Builder
	eachText(s, func(text string) { out.WriteString(text) })
	return out.String()
}

func pushPart(parts []string, part string) []string {
	visible := false
	eachText(part, func(text string) { visible = visible || strings.TrimSpace(text) != "" })
	if visible {
		parts = append(parts, part)
	}
	return parts
}

// eachText calls f with each run of text between tags.
func eachText(s string, f func(string)) {
	start := 0
	for cursor := 0; cursor < len(s); {
		if tag, ok := parseTelegramTag(s[cursor:]); ok {
			f(s[start:cursor])
			cursor += tag.n
			start = cursor
		} else {
			cursor += runeLen(s[cursor:])
		}
	}
	f(s[start:])
}
