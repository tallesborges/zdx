package render

import (
	"fmt"
	"strings"

	"github.com/yuin/goldmark/v2/ast"
	"github.com/yuin/goldmark/v2/extension"
	extast "github.com/yuin/goldmark/v2/extension/ast"
	"github.com/yuin/goldmark/v2/parser"
)

// ToTelegramHTML converts model-authored Markdown into a Telegram-valid HTML
// payload, a port of the Rust bot's telegram/markdown.rs to_telegram_html.
//
// Parsing is CommonMark plus GFM tables (the Rust parser options: tables
// only, so no strikethrough, task lists, footnotes or autolinked bare URLs).
// Deliberate lowerings: headings become bold lines, lists become flat
// prefixed lines, tables become pipe-joined plain lines, images keep only
// their alt text. Formatting is never nested where Telegram forbids it:
// inline code closes and reopens open formatting, and inside a blockquote
// code and links become plain text. Text that is already Telegram HTML
// (only valid, balanced Telegram tags) passes through trimmed; any other
// raw HTML is escaped as literal text.
//
// Checked against the Rust function on 20,000 synthetic inputs and all of
// the repository's Markdown files (testdata/rust_golden.json keeps a
// sample). Known divergences, all from the parsers, none seen in the
// repository docs:
//   - A table written as lazy continuation of a list item or blockquote
//     paragraph is a table here, plain text in Rust.
//   - When a paragraph line before a delimiter row has the wrong cell
//     count, goldmark stops looking for a table; pulldown-cmark finds a
//     later valid one.
//   - Trailing space+tab before a line break is a hard break in Rust, soft
//     here; a closing fence followed by a tab closes the block here only.
//   - Leading spaces of a code span's continuation line are kept in Rust.
func ToTelegramHTML(markdown string) string {
	source := []byte(markdown)
	doc := newParser().Parse(source)
	if isLegacyTelegramHTML(doc, source) {
		return strings.TrimSpace(markdown)
	}
	r := &mdRenderer{source: source}
	_ = ast.Walk(doc, r.walk)
	return r.finish()
}

// newParser returns a fresh parser per call: goldmark documents neither
// parser nor extension reuse across goroutines.
func newParser() parser.Parser {
	return parser.New(parser.WithExtensions(extension.NewTableParser()))
}

// htmlSources calls f with the raw text of every HTML block line and inline
// HTML span, in document order.
func htmlSources(doc ast.Node, source []byte, f func(string) bool) {
	_ = ast.Walk(doc, func(n ast.Node, entering bool) (ast.WalkStatus, error) {
		if !entering {
			return ast.WalkContinue, nil
		}
		switch n := n.(type) {
		case *ast.HTMLBlock:
			for _, seg := range n.Value.Segments() {
				if !f(seg.Str(source)) {
					return ast.WalkStop, nil
				}
			}
		case *ast.RawHTML:
			if !f(n.Value.Value(source)) {
				return ast.WalkStop, nil
			}
		}
		return ast.WalkContinue, nil
	})
}

// isLegacyTelegramHTML reports whether every raw HTML tag in the document
// is a valid Telegram tag, properly nested, with at least one tag and none
// left open.
func isLegacyTelegramHTML(doc ast.Node, source []byte) bool {
	var stack []string
	sawTag, valid := false, true
	htmlSources(doc, source, func(raw string) bool {
		for {
			i := strings.IndexByte(raw, '<')
			if i < 0 {
				return true
			}
			raw = raw[i:]
			tag, ok := parseTelegramTag(raw)
			if !ok {
				valid = false
				return false
			}
			sawTag = true
			if tag.closing {
				if len(stack) == 0 || stack[len(stack)-1] != tag.name {
					valid = false
					return false
				}
				stack = stack[:len(stack)-1]
			} else {
				var parent string
				if len(stack) > 0 {
					parent = stack[len(stack)-1]
				}
				if tag.name == "code" && tag.attrs != "" && parent != "pre" {
					valid = false
					return false
				}
				if parent != "" && !validNesting(parent, tag.name) {
					valid = false
					return false
				}
				stack = append(stack, tag.name)
			}
			raw = raw[tag.n:]
		}
	})
	return valid && sawTag && len(stack) == 0
}

func validNesting(parent, child string) bool {
	if parent == "pre" && child == "code" {
		return true
	}
	if parent == "code" || parent == "pre" || child == "code" || child == "pre" {
		return false
	}
	return isStyleTag(parent) || isStyleTag(child)
}

func isStyleTag(name string) bool {
	switch name {
	case "b", "strong", "i", "em", "u", "ins", "s", "strike", "del", "span", "tg-spoiler":
		return true
	}
	return false
}

type inlineTag struct{ open, close string }

// listLevel is one open list: its next number, or -1 for a bullet list.
type listLevel struct{ next int }

// mdRenderer mirrors the Rust Renderer state machine; goldmark nodes are
// mapped onto the pulldown-cmark events it was written for.
type mdRenderer struct {
	source          []byte
	out             strings.Builder
	pendingNewlines int
	// suppressBreak drops the next block break so a list item or blockquote
	// can hold its first block on the line of the marker or opening tag.
	suppressBreak   bool
	lists           []listLevel
	inline          []inlineTag
	inlineOpen      bool
	blockquoteDepth int
	blockquoteOpen  bool
	// plainLinkURLs holds one entry per open link: a tagged <a> link, or
	// (inside a blockquote) the URL printed after its plain label.
	plainLinkURLs []plainLink
	rowStarted    bool
}

type plainLink struct {
	url    string
	tagged bool
}

func (r *mdRenderer) walk(n ast.Node, entering bool) (ast.WalkStatus, error) {
	switch n := n.(type) {
	case *ast.Document, *extast.TableBody, *ast.LinkReferenceDefinition:
	case *ast.Paragraph:
		if inTightList(n) {
			break
		}
		if entering {
			r.blockBreak(r.paragraphGap())
		} else {
			r.pendingNewlines = r.paragraphGap()
		}
	case *ast.Heading:
		if entering {
			r.blockBreak(2)
			r.ensureBlockquoteOpen()
			r.openInline("<b>", "</b>")
		} else {
			r.closeInline()
			r.pendingNewlines = 2
		}
	case *ast.ThematicBreak:
		if entering {
			r.blockBreak(2)
			r.ensureBlockquoteOpen()
			r.pushRaw("———")
			r.pendingNewlines = 2
		}
	case *ast.CodeBlock:
		if entering {
			r.codeBlock(n)
		}
		return ast.WalkSkipChildren, nil
	case *ast.HTMLBlock:
		if entering {
			r.blockBreak(2)
			for _, seg := range n.Value.Segments() {
				r.pushEscapedBlockHTML(seg.Str(r.source))
			}
		}
		return ast.WalkSkipChildren, nil
	case *ast.List:
		if entering {
			r.blockBreak(r.paragraphGap())
			level := listLevel{next: -1}
			if n.IsOrdered() {
				level.next = n.Start
			}
			r.lists = append(r.lists, level)
		} else {
			r.lists = r.lists[:len(r.lists)-1]
			r.pendingNewlines = r.paragraphGap()
		}
	case *ast.ListItem:
		if entering {
			r.blockBreak(1)
			depth := len(r.lists) - 1
			marker := "- "
			if top := r.lists[depth]; top.next >= 0 {
				marker = fmt.Sprintf("%d. ", top.next)
			}
			r.ensureBlockquoteOpen()
			r.pushRaw(strings.Repeat("  ", depth) + marker)
			r.suppressBreak = true
		} else {
			r.suppressBreak = false
			if top := &r.lists[len(r.lists)-1]; top.next >= 0 {
				top.next++
			}
			r.pendingNewlines = 1
		}
	case *ast.Blockquote:
		if entering {
			r.blockBreak(2)
			if r.blockquoteDepth == 0 {
				r.suppressBreak = true
			}
			r.blockquoteDepth++
		} else {
			r.blockquoteDepth = max(r.blockquoteDepth-1, 0)
			if r.blockquoteDepth == 0 {
				r.closeBlockquote()
				r.pendingNewlines = 2
			}
		}
	case *ast.Strong:
		r.styled(entering, "<b>", "</b>")
	case *ast.Emphasis:
		r.styled(entering, "<i>", "</i>")
	case *ast.Link:
		r.link(entering, n.Destination.Value(r.source))
	case *ast.AutoLink:
		if entering {
			// pulldown-cmark gives an email autolink its address without
			// "mailto:", and the Rust bot sends that as the href.
			dest, label := n.Destination.Value(r.source), n.Label.Value(r.source)
			if dest == "mailto:"+label {
				dest = label
			}
			r.link(true, dest)
			r.text(label)
			r.link(false, "")
		}
		return ast.WalkSkipChildren, nil
	case *ast.Image:
		// Only the alt text (the children) is rendered.
	case *ast.Text:
		if entering {
			r.text(n.Value.Value(r.source))
			switch {
			case n.HardLineBreak():
				r.ensureBlockquoteOpen()
				r.pushRaw("\n")
			case n.SoftLineBreak():
				r.ensureBlockquoteOpen()
				r.pushRaw(" ")
			}
		}
	case *ast.CodeSpan:
		if entering {
			r.inlineCode(n.Value.Value(r.source))
		}
		return ast.WalkSkipChildren, nil
	case *ast.RawHTML:
		if entering {
			// Like the Rust renderer: no blockquote or formatting is opened
			// for inline HTML.
			r.pushEscaped(rawHTML(n, r.source))
		}
		return ast.WalkSkipChildren, nil
	case *extast.Table:
		if entering {
			r.blockBreak(2)
		} else {
			r.pendingNewlines = 2
		}
	case *extast.TableHeader, *extast.TableRow:
		if entering {
			r.blockBreak(1)
			r.rowStarted = false
		} else {
			r.pendingNewlines = 1
		}
	case *extast.TableCell:
		if entering {
			if r.rowStarted {
				r.pushRaw(" | ")
			}
			r.rowStarted = true
		}
	}
	return ast.WalkContinue, nil
}

// inTightList reports a paragraph of a tight list item, which pulldown-cmark
// emits without paragraph events.
func inTightList(n ast.Node) bool {
	item, ok := n.Parent().(*ast.ListItem)
	if !ok {
		return false
	}
	list, ok := item.Parent().(*ast.List)
	return ok && list.IsTight
}

func (r *mdRenderer) paragraphGap() int {
	if len(r.lists) == 0 {
		return 2
	}
	return 1
}

func (r *mdRenderer) text(s string) {
	r.ensureBlockquoteOpen()
	r.pushEscaped(s)
}

func (r *mdRenderer) styled(entering bool, open, close string) {
	if entering {
		r.ensureBlockquoteOpen()
		r.openInline(open, close)
	} else {
		r.closeInline()
	}
}

// link opens or closes a link. Inside a blockquote the label stays plain
// text and the URL follows it in parentheses.
func (r *mdRenderer) link(entering bool, dest string) {
	if entering {
		if r.blockquoteDepth > 0 {
			r.ensureBlockquoteOpen()
			r.plainLinkURLs = append(r.plainLinkURLs, plainLink{url: dest})
		} else {
			r.openInline(`<a href="`+escapeAttr(dest)+`">`, "</a>")
			r.plainLinkURLs = append(r.plainLinkURLs, plainLink{tagged: true})
		}
		return
	}
	last := r.plainLinkURLs[len(r.plainLinkURLs)-1]
	r.plainLinkURLs = r.plainLinkURLs[:len(r.plainLinkURLs)-1]
	if last.tagged {
		r.closeInline()
		return
	}
	r.pushRaw(" (")
	r.pushEscaped(last.url)
	r.pushRaw(")")
}

func (r *mdRenderer) codeBlock(n *ast.CodeBlock) {
	r.suspendBlockquote()
	r.blockBreak(2)
	lang := ""
	if n.CodeBlockKind == ast.CodeBlockKindFenced {
		lang = languageToken(n.Info.Value(r.source))
	}
	code := strings.TrimSuffix(codeText(n, r.source), "\n")
	if lang != "" {
		r.pushRaw(`<pre><code class="language-` + lang + `">`)
	} else {
		r.pushRaw("<pre>")
	}
	r.pushEscaped(code)
	if lang != "" {
		r.pushRaw("</code></pre>")
	} else {
		r.pushRaw("</pre>")
	}
	if r.blockquoteDepth == 0 {
		r.openAllInline()
	}
	r.pendingNewlines = 2
}

// inlineCode: inline code cannot contain or sit inside another entity, so
// open formatting is closed before it and reopened after.
func (r *mdRenderer) inlineCode(code string) {
	if r.blockquoteDepth > 0 {
		r.ensureBlockquoteOpen()
		r.pushEscaped(code)
		return
	}
	r.closeAllInline()
	r.pushRaw("<code>")
	r.pushEscaped(code)
	r.pushRaw("</code>")
	r.openAllInline()
}

func (r *mdRenderer) openInline(open, close string) {
	r.pushRaw(open)
	r.inline = append(r.inline, inlineTag{open: open, close: close})
	r.inlineOpen = true
}

func (r *mdRenderer) closeInline() {
	if len(r.inline) > 0 {
		tag := r.inline[len(r.inline)-1]
		r.inline = r.inline[:len(r.inline)-1]
		if r.inlineOpen {
			r.pushRaw(tag.close)
		}
	}
	if len(r.inline) == 0 {
		r.inlineOpen = false
	}
}

func (r *mdRenderer) closeAllInline() {
	if !r.inlineOpen {
		return
	}
	for i := len(r.inline) - 1; i >= 0; i-- {
		r.pushRaw(r.inline[i].close)
	}
	r.inlineOpen = false
}

func (r *mdRenderer) openAllInline() {
	if r.inlineOpen || len(r.inline) == 0 {
		return
	}
	for _, tag := range r.inline {
		r.pushRaw(tag.open)
	}
	r.inlineOpen = true
}

func (r *mdRenderer) ensureBlockquoteOpen() {
	if r.blockquoteDepth == 0 || r.blockquoteOpen {
		r.openAllInline()
		return
	}
	r.pushRaw("<blockquote>")
	r.blockquoteOpen = true
	r.openAllInline()
}

func (r *mdRenderer) suspendBlockquote() {
	r.closeAllInline()
	r.closeBlockquote()
}

func (r *mdRenderer) closeBlockquote() {
	if r.blockquoteOpen {
		r.pendingNewlines = 0
		r.pushRaw("</blockquote>")
		r.blockquoteOpen = false
	}
}

func (r *mdRenderer) pushEscapedBlockHTML(html string) {
	trimmed := strings.TrimRight(html, "\n")
	stripped := len(html) - len(trimmed)
	r.pushEscaped(trimmed)
	if r.out.Len() > 0 && stripped > 0 {
		r.pendingNewlines = min(stripped, 2)
	}
}

func (r *mdRenderer) blockBreak(newlines int) {
	if r.suppressBreak {
		r.suppressBreak = false
		return
	}
	if r.out.Len() == 0 {
		return
	}
	r.pendingNewlines = max(r.pendingNewlines, newlines)
}

func (r *mdRenderer) pushRaw(s string) {
	if s == "" {
		return
	}
	for ; r.pendingNewlines > 0; r.pendingNewlines-- {
		r.out.WriteByte('\n')
	}
	r.out.WriteString(s)
}

func (r *mdRenderer) pushEscaped(s string) { r.pushRaw(EscapeHTML(s)) }

func (r *mdRenderer) finish() string {
	if r.inlineOpen {
		for i := len(r.inline) - 1; i >= 0; i-- {
			r.out.WriteString(r.inline[i].close)
		}
	}
	if r.blockquoteOpen {
		r.out.WriteString("</blockquote>")
	}
	return strings.TrimSpace(r.out.String())
}

// codeText is a code block's content. A fence left open at the end of the
// text whose last line is only spaces, with no line break, loses that line,
// as in pulldown-cmark.
func codeText(n *ast.CodeBlock, source []byte) string {
	code := n.Value.Str(source)
	segs := n.Value.Segments()
	if len(segs) == 0 {
		return code
	}
	last := segs[len(segs)-1]
	if last.Stop != len(source) || len(source) == 0 || source[len(source)-1] == '\n' {
		return code
	}
	trimmed := strings.TrimSuffix(code, "\n")
	cut := strings.LastIndexByte(trimmed, '\n') + 1
	if strings.Trim(trimmed[cut:], " \t") != "" {
		return code
	}
	return trimmed[:cut]
}

// rawHTML is the source text of an inline HTML span. goldmark drops the
// indentation of its continuation lines; pulldown-cmark keeps it.
func rawHTML(n *ast.RawHTML, source []byte) string {
	indices := n.Value.Indices()
	if len(indices) < 2 {
		return n.Value.Value(source)
	}
	var b strings.Builder
	for i, idx := range indices {
		start := idx.Start
		for i > 0 && start > 0 && (source[start-1] == ' ' || source[start-1] == '\t') {
			start--
		}
		b.Write(source[start:idx.Stop])
	}
	return b.String()
}

// languageToken extracts a usable language name from a fence info string.
func languageToken(info string) string {
	fields := strings.Fields(info)
	if len(fields) == 0 {
		return ""
	}
	token := fields[0]
	if len(token) > 32 || !allRunes(token, isLanguageRune) {
		return ""
	}
	return strings.ToLower(token)
}
