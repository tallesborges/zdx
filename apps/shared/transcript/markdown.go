package transcript

import (
	"strings"
	"unicode/utf8"

	"charm.land/lipgloss/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/yuin/goldmark/v2/ast"
	"github.com/yuin/goldmark/v2/extension"
	extast "github.com/yuin/goldmark/v2/extension/ast"
	"github.com/yuin/goldmark/v2/parser"
)

// Markdown in transcript prose (user and assistant messages, expanded
// thinking, and tool bodies the engine marks as Markdown) is parsed with
// goldmark (CommonMark + GFM: tables, strikethrough, task lists, bare-URL
// links; a line break in the source is a line break, as in the Mini App)
// and laid out here as terminal lines. Every string taken from the
// tree goes through cleanText again: goldmark decodes entities, so
// `&#x1b;` would otherwise become an escape byte. Styling comes only from
// the theme below; the renderer never emits hyperlinks (OSC 8), and raw
// HTML is shown as literal dim text. Code is wrapped, never cut.

// mdFlag is a set of inline formats.
type mdFlag uint16

const (
	fBold mdFlag = 1 << iota
	fItalic
	fCode
	fLink
	fURL
	fStrike
	fHTML
	fHeading
	fHeading1
)

// Theme styles one kind of prose. A muted theme (thinking) keeps the
// structure and formats but draws everything dim. The user theme
// keeps the same structure but tints plain text a soft warm grey, so
// user messages read apart from assistant prose without a new hue.
type Theme struct {
	muted  bool
	user   bool
	styles map[mdFlag]lipgloss.Style
	paints map[mdFlag][2]string
}

var (
	Normal = newMDTheme(false, false)
	Muted  = newMDTheme(true, false)
	User   = newMDTheme(false, true)

	mdCodeColor  = lipgloss.ANSIColor(216)
	mdLinkColor  = lipgloss.Blue
	mdUserText   = lipgloss.ANSIColor(223)
	mdGutter     = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(243))
	mdQuoteBar   = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(66))
	mdTableLines = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(243))
	mdBullet     = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(66))
	mdCodeText   = lipgloss.NewStyle().Foreground(lipgloss.ANSIColor(250))
	mdItalic     = lipgloss.ANSIColor(244)

	codeGutter     = mdGutter.Render("▎ ")
	codeGutterMore = mdGutter.Render("┊ ")
	quoteBar       = mdQuoteBar.Render("│ ")
	tableBar       = mdTableLines.Render(" │ ")
)

func (t *Theme) style(f mdFlag) lipgloss.Style {
	if s, ok := t.styles[f]; ok {
		return s
	}
	s := lipgloss.NewStyle()
	switch {
	case t.muted:
		s = s.Foreground(lipgloss.BrightBlack)
	case t.user && f&(fCode|fLink|fURL|fHTML|fHeading|fHeading1) == 0:
		s = s.Foreground(mdUserText)
	case f&fHTML != 0 || f&fURL != 0:
		s = s.Foreground(lipgloss.BrightBlack)
	case f&fCode != 0:
		s = s.Foreground(mdCodeColor)
	case f&fLink != 0:
		s = s.Foreground(mdLinkColor)
	case f&(fHeading|fHeading1) != 0:
		s = s.Foreground(lipgloss.Cyan)
	}
	if f&(fBold|fHeading|fHeading1) != 0 {
		s = s.Bold(true)
	}
	if f&fItalic != 0 {
		s = s.Italic(true)
		// Plain emphasis is quieter than body text, so it reads
		// as aside without pulling focus. Bold, code, links and
		// headings keep body brightness. User prose keeps its tint.
		if f&(fBold|fCode|fLink|fURL|fHTML|fHeading|fHeading1|fStrike) == 0 && !t.muted && !t.user {
			s = s.Foreground(mdItalic)
		}
	}
	if f&(fLink|fHeading1) != 0 && !t.muted {
		s = s.Underline(true)
	}
	if f&fStrike != 0 {
		s = s.Strikethrough(true)
	}
	t.styles[f] = s
	return s
}

func newMDTheme(muted, user bool) *Theme {
	t := &Theme{muted: muted, user: user, styles: map[mdFlag]lipgloss.Style{}, paints: map[mdFlag][2]string{}}
	// Fill both maps now: the themes are shared, and tests render from
	// several goroutines.
	for f := mdFlag(0); f < fHeading1<<1; f++ {
		st := t.style(f)
		out := st.Render("x")
		i := strings.Index(out, "x")
		t.paints[f] = [2]string{out[:i], out[i+1:]}
	}
	return t
}

func (t *Theme) text() lipgloss.Style { return t.style(0) }

// paint is style(f).Render(s) for one line of clean text, from the
// sequences the style writes around a single character: a Render call per
// word was most of the layout time.
func (t *Theme) paint(f mdFlag, s string) string {
	if s == "" {
		return ""
	}
	p := t.paints[f]
	return p[0] + s + p[1]
}

// atom is one piece of inline text: a word fragment with its formats, a
// space, or a hard line break.
type atom struct {
	text  string
	flags mdFlag
	kind  atomKind
}

type atomKind uint8

const (
	atomText atomKind = iota
	atomSpace
	atomBreak
)

type mdRenderer struct {
	src   []byte
	theme *Theme
}

// renderMarkdown lays src out as lines at most width cells wide.
func Markdown(src string, width int, theme *Theme) []string {
	// Two cells hold any character, wide ones included.
	width = max(width, 2)
	src = Clean(src)
	source := []byte(src)
	doc := parser.New(parser.WithExtensions(extension.NewGFMParser())).Parse(source)
	r := &mdRenderer{src: source, theme: theme}
	lines := r.blocks(doc, width)
	if len(lines) == 0 && strings.TrimSpace(src) != "" {
		// Nothing renderable (only a link reference, for instance): show
		// the source rather than an empty message.
		return r.plain(src, width)
	}
	return lines
}

// str is a string from the tree, cleaned of anything a decoded entity or
// escape could have introduced. The source was cleaned before parsing, so a
// carriage return here came from an entity (`&#13;`): it is a space, not a
// return that would hide the text before it.
func (r *mdRenderer) str(s string) string {
	return Clean(strings.ReplaceAll(s, "\r", " "))
}

// plain wraps text without Markdown, in the theme's text style.
func (r *mdRenderer) plain(s string, width int) []string {
	var out []string
	for _, l := range WrapText(s, width) {
		out = append(out, r.theme.text().Render(l))
	}
	return out
}

// blocks renders the block children of n, a blank line between blocks
// unless n is an item of a tight list.
func (r *mdRenderer) blocks(n ast.Node, width int) []string {
	width = max(width, 2)
	tight := false
	if item, ok := n.(*ast.ListItem); ok {
		if list, ok := item.Parent().(*ast.List); ok {
			tight = list.IsTight
		}
	}
	var out []string
	for c := n.FirstChild(); c != nil; c = c.NextSibling() {
		lines := r.block(c, width)
		if len(lines) == 0 {
			continue
		}
		if len(out) > 0 && !tight {
			out = append(out, "")
		}
		out = append(out, lines...)
	}
	return out
}

func (r *mdRenderer) block(n ast.Node, width int) []string {
	switch n := n.(type) {
	case *ast.Paragraph:
		return r.wrap(r.inline(n, 0), width)
	case *ast.Heading:
		f := fHeading
		if n.Level == 1 {
			f |= fHeading1
		}
		return r.wrap(r.inline(n, f), width)
	case *ast.ThematicBreak:
		return []string{mdGutter.Render(strings.Repeat("─", min(width, 40)))}
	case *ast.CodeBlock:
		return r.code(n, width)
	case *ast.HTMLBlock:
		var out []string
		for _, seg := range n.Value.Segments() {
			for _, l := range hardWrap(expandTabs(r.str(strings.TrimRight(seg.Str(r.src), "\n"))), width) {
				out = append(out, r.theme.style(fHTML).Render(l))
			}
		}
		return out
	case *ast.List:
		return r.list(n, width)
	case *ast.Blockquote:
		if width < 8 {
			// No room for the bar: the text keeps the width.
			return r.blocks(n, width)
		}
		var out []string
		for _, l := range r.blocks(n, width-2) {
			out = append(out, quoteBar+l)
		}
		return out
	case *extast.Table:
		return r.table(n, width)
	case *ast.LinkReferenceDefinition:
		return nil
	}
	if !n.HasChildren() {
		return nil
	}
	if _, ok := n.FirstChild().(ast.InlineNode); ok {
		return r.wrap(r.inline(n, 0), width)
	}
	return r.blocks(n, width)
}

var bullets = []string{"•", "◦", "▪"}

func (r *mdRenderer) list(n *ast.List, width int) []string {
	depth := 0
	for p := n.Parent(); p != nil; p = p.Parent() {
		if _, ok := p.(*ast.List); ok {
			depth++
		}
	}
	// Ordered markers share one width so item text lines up.
	last := n.Start + n.ChildCount() - 1
	numW := len(itoa(last)) + 1
	var out []string
	num := n.Start
	for item := n.FirstChild(); item != nil; item = item.NextSibling() {
		marker := bullets[depth%len(bullets)]
		if n.IsOrdered() {
			marker = pad(itoa(num)+".", numW)
			num++
		}
		if status, ok := extension.TaskStatusOf(item); ok {
			if status == extension.TaskStatusCompleted {
				marker += " " + ok2("☑")
			} else {
				marker += " ☐"
			}
		}
		markW := ansi.StringWidth(marker) + 1
		if len(out) > 0 && !n.IsTight {
			out = append(out, "")
		}
		if width-markW < 6 {
			// No room to indent: the marker gets its own line.
			out = append(out, mdBullet.Render(ansi.Truncate(marker, width, "")))
			out = append(out, r.blocks(item, width)...)
			continue
		}
		body := r.blocks(item, width-markW)
		if len(body) == 0 {
			body = []string{""}
		}
		indent := strings.Repeat(" ", markW)
		for i, l := range body {
			if i == 0 {
				out = append(out, mdBullet.Render(marker)+" "+l)
			} else if l == "" {
				out = append(out, "")
			} else {
				out = append(out, indent+l)
			}
		}
	}
	return out
}

func ok2(s string) string { return ok.Render(s) }

func itoa(n int) string {
	if n == 0 {
		return "0"
	}
	neg := n < 0
	if neg {
		n = -n
	}
	var b []byte
	for n > 0 {
		b = append([]byte{byte('0' + n%10)}, b...)
		n /= 10
	}
	if neg {
		b = append([]byte{'-'}, b...)
	}
	return string(b)
}

// code renders a code block: an optional language label, then each line
// behind a gutter. A line longer than the width continues on the next row
// behind a dotted gutter; nothing is cut.
func (r *mdRenderer) code(n *ast.CodeBlock, width int) []string {
	var out []string
	if lang, ok := n.Language(r.src); ok {
		out = append(out, mdBullet.Render(ansi.Truncate(r.str(lang), width, "…")))
	}
	text := strings.TrimSuffix(n.Value.Str(r.src), "\n")
	paint := func(s string) string { return mdCodeText.Render(s) }
	if r.theme.muted {
		paint = func(s string) string { return r.theme.paint(0, s) }
	}
	gutters := width >= 6
	for _, line := range strings.Split(r.str(text), "\n") {
		if !gutters {
			for _, piece := range hardWrap(expandTabs(line), width) {
				out = append(out, paint(piece))
			}
			continue
		}
		for i, piece := range hardWrap(expandTabs(line), width-2) {
			gutter := codeGutter
			if i > 0 {
				gutter = codeGutterMore
			}
			out = append(out, gutter+paint(piece))
		}
	}
	return out
}

func expandTabs(s string) string {
	if !strings.Contains(s, "\t") {
		return s
	}
	var b strings.Builder
	col := 0
	for _, r := range s {
		if r == '\t' {
			n := 4 - col%4
			b.WriteString(strings.Repeat(" ", n))
			col += n
			continue
		}
		b.WriteRune(r)
		col += ansi.StringWidth(string(r))
	}
	return b.String()
}

// hardWrap splits a line into pieces of at most width cells, keeping
// every character; an empty line stays one empty piece.
func hardWrap(s string, width int) []string {
	width = max(width, 1)
	if ansi.StringWidth(s) <= width {
		return []string{s}
	}
	return strings.Split(ansi.Hardwrap(s, width, true), "\n")
}

// fitPrefix is ansi.Truncate(s, cells, "") reading only a window of s:
// Truncate measures all of s first, so splitting a long unbroken word (a
// token, a data URI) line by line was quadratic. The window starts at a few
// bytes per cell and doubles while it fits whole. A cut inside the window
// is at a grapheme boundary of s, because a boundary depends only on what
// comes before it and the next character; a window that fits whole may end
// inside a cluster, so it grows instead.
func fitPrefix(s string, cells int) string {
	for limit := cells*4 + 4; limit < len(s); limit *= 2 {
		for limit < len(s) && !utf8.RuneStart(s[limit]) {
			limit++
		}
		if limit >= len(s) {
			break
		}
		if piece := ansi.Truncate(s[:limit], cells, ""); len(piece) < limit {
			return piece
		}
	}
	return ansi.Truncate(s, cells, "")
}

// inline collects the atoms of n's inline children.
func (r *mdRenderer) inline(n ast.Node, f mdFlag) []atom {
	var out []atom
	r.walkInline(n, f, &out)
	return out
}

func (r *mdRenderer) words(s string, f mdFlag, out *[]atom) {
	s = strings.NewReplacer("\n", " ", "\t", " ").Replace(s)
	for i, w := range strings.Split(s, " ") {
		if i > 0 {
			*out = append(*out, atom{kind: atomSpace})
		}
		if w != "" {
			*out = append(*out, atom{text: w, flags: f})
		}
	}
}

func (r *mdRenderer) walkInline(n ast.Node, f mdFlag, out *[]atom) {
	for c := n.FirstChild(); c != nil; c = c.NextSibling() {
		switch c := c.(type) {
		case *ast.Text:
			r.words(r.str(c.Value.Value(r.src)), f, out)
			// A source line break is a line break, as in the Mini App
			// (marked with gfm and breaks): chat text means its newlines.
			if c.HardLineBreak() || c.SoftLineBreak() {
				*out = append(*out, atom{kind: atomBreak})
			}
		case *ast.CodeSpan:
			r.words(r.str(c.Value.Value(r.src)), f|fCode, out)
		case *ast.Emphasis:
			r.walkInline(c, f|fItalic, out)
		case *ast.Strong:
			r.walkInline(c, f|fBold, out)
		case *extast.Strikethrough:
			r.walkInline(c, f|fStrike, out)
		case *ast.Link:
			start := len(*out)
			r.walkInline(c, f|fLink, out)
			label := atomsText((*out)[start:])
			if dest := r.str(c.Destination.Value(r.src)); dest != "" && dest != label {
				*out = append(*out, atom{kind: atomSpace})
				r.words("("+dest+")", f|fURL, out)
			}
		case *ast.AutoLink:
			r.words(r.str(c.Label.Value(r.src)), f|fLink, out)
		case *ast.Image:
			r.words("[image:", f|fURL, out)
			*out = append(*out, atom{kind: atomSpace})
			r.walkInline(c, f|fItalic, out)
			if dest := r.str(c.Destination.Value(r.src)); dest != "" {
				*out = append(*out, atom{kind: atomSpace})
				r.words(dest, f|fURL, out)
			}
			*out = append(*out, atom{text: "]", flags: f | fURL})
		case *ast.RawHTML:
			raw := r.str(c.Value.Value(r.src))
			r.words(strings.ReplaceAll(raw, "\n", " "), f|fHTML, out)
		default:
			r.walkInline(c, f, out)
		}
	}
}

func atomsText(as []atom) string {
	var b strings.Builder
	for _, a := range as {
		switch a.kind {
		case atomText:
			b.WriteString(a.text)
		default:
			b.WriteByte(' ')
		}
	}
	return b.String()
}

// wrap fills lines of at most width cells with the atoms, breaking at
// spaces. A word wider than the line is split across lines.
func (r *mdRenderer) wrap(atoms []atom, width int) []string {
	var lines []string
	var line lineBuilder
	line.theme = r.theme
	flush := func() {
		lines = append(lines, line.String())
		line.reset()
	}
	put := func(w []atom, ww int) {
		if line.w > 0 {
			if line.w+1+ww > width {
				flush()
			} else {
				// A space between two words of one link or code span
				// keeps their style, so an underline does not break.
				line.add(line.f&w[0].flags, " ")
			}
		}
		if ww <= width-line.w {
			for _, a := range w {
				line.add(a.flags, a.text)
			}
			return
		}
		// Too wide for any line: split it cell by cell.
		for _, a := range w {
			rest := a.text
			for rest != "" {
				room := width - line.w
				if room <= 0 {
					flush()
					room = width
				}
				piece := fitPrefix(rest, room)
				if piece == "" {
					if line.w > 0 {
						// A wide character that does not fit the room left.
						flush()
						continue
					}
					// Wider than a whole line: place it alone.
					_, size := utf8.DecodeRuneInString(rest)
					piece = rest[:size]
				}
				line.add(a.flags, piece)
				rest = rest[len(piece):]
			}
		}
	}
	var w []atom
	ww := 0
	end := func() {
		if len(w) > 0 {
			put(w, ww)
		}
		w, ww = w[:0], 0
	}
	for _, a := range atoms {
		switch a.kind {
		case atomText:
			w = append(w, a)
			ww += ansi.StringWidth(a.text)
		case atomSpace:
			end()
		case atomBreak:
			end()
			flush()
		}
	}
	end()
	if line.w > 0 {
		flush()
	}
	// A trailing hard break leaves an empty last line; drop it.
	for len(lines) > 0 && lines[len(lines)-1] == "" {
		lines = lines[:len(lines)-1]
	}
	return lines
}

// lineBuilder joins styled text, opening a style once per run of equal
// formats.
type lineBuilder struct {
	theme *Theme
	b     strings.Builder
	run   strings.Builder
	f     mdFlag
	w     int
}

func (l *lineBuilder) add(f mdFlag, s string) {
	if l.run.Len() > 0 && f != l.f {
		l.end()
	}
	l.f = f
	l.run.WriteString(s)
	l.w += ansi.StringWidth(s)
}

func (l *lineBuilder) end() {
	if l.run.Len() > 0 {
		l.b.WriteString(l.theme.paint(l.f, l.run.String()))
		l.run.Reset()
	}
}

func (l *lineBuilder) String() string {
	l.end()
	return l.b.String()
}

func (l *lineBuilder) reset() {
	l.b.Reset()
	l.run.Reset()
	l.w = 0
}

// table lays a GFM table out with aligned columns, wrapping cell text
// inside its column. When even narrow columns do not fit, each row becomes
// a list of "header: value" lines instead.
func (r *mdRenderer) table(n *extast.Table, width int) []string {
	var head []atomsCell
	var rows [][]atomsCell
	var aligns []extast.Alignment
	var tableRows []ast.Node
	for c := n.FirstChild(); c != nil; c = c.NextSibling() {
		if body, ok := c.(*extast.TableBody); ok {
			for row := body.FirstChild(); row != nil; row = row.NextSibling() {
				tableRows = append(tableRows, row)
			}
			continue
		}
		tableRows = append(tableRows, c)
	}
	for _, row := range tableRows {
		var cells []atomsCell
		for cell := row.FirstChild(); cell != nil; cell = cell.NextSibling() {
			tc, _ := cell.(*extast.TableCell)
			f := mdFlag(0)
			if _, isHead := row.(*extast.TableHeader); isHead {
				f = fBold
				if tc != nil {
					aligns = append(aligns, tc.Alignment)
				}
			}
			cells = append(cells, atomsCell(r.inline(cell, f)))
		}
		if _, isHead := row.(*extast.TableHeader); isHead {
			head = cells
		} else {
			rows = append(rows, cells)
		}
	}
	cols := len(head)
	for _, row := range rows {
		cols = max(cols, len(row))
	}
	if cols == 0 {
		return nil
	}
	const sep = 3 // " │ "
	room := width - (cols-1)*sep
	if room < cols*4 {
		return r.tableRecords(head, rows, width)
	}
	natural := make([]int, cols)
	for _, row := range append([][]atomsCell{head}, rows...) {
		for i, cell := range row {
			natural[i] = max(natural[i], cell.width())
		}
	}
	widths := fitColumns(natural, room)
	line := func(cells []atomsCell) []string {
		parts := make([][]string, cols)
		height := 1
		for i := 0; i < cols; i++ {
			var text []string
			if i < len(cells) {
				text = r.wrap(cells[i], widths[i])
			}
			parts[i] = text
			height = max(height, len(text))
		}
		out := make([]string, height)
		for h := 0; h < height; h++ {
			var b strings.Builder
			for i := 0; i < cols; i++ {
				if i > 0 {
					b.WriteString(tableBar)
				}
				cell := ""
				if h < len(parts[i]) {
					cell = parts[i][h]
				}
				gap := max(widths[i]-ansi.StringWidth(cell), 0)
				align := extast.AlignLeft
				if i < len(aligns) {
					align = aligns[i]
				}
				switch align {
				case extast.AlignRight:
					b.WriteString(strings.Repeat(" ", gap) + cell)
				case extast.AlignCenter:
					b.WriteString(strings.Repeat(" ", gap/2) + cell + strings.Repeat(" ", gap-gap/2))
				default:
					b.WriteString(cell + strings.Repeat(" ", gap))
				}
			}
			out[h] = strings.TrimRight(b.String(), " ")
		}
		return out
	}
	var out []string
	if head != nil {
		out = append(out, line(head)...)
		var b strings.Builder
		for i, w := range widths {
			if i > 0 {
				b.WriteString("─┼─")
			}
			b.WriteString(strings.Repeat("─", w))
		}
		out = append(out, mdTableLines.Render(b.String()))
	}
	for _, row := range rows {
		out = append(out, line(row)...)
	}
	return out
}

// tableRecords is the narrow table layout: each row as "header: value"
// lines, rows apart by a blank line.
func (r *mdRenderer) tableRecords(head []atomsCell, rows [][]atomsCell, width int) []string {
	var out []string
	for ri, row := range rows {
		if ri > 0 {
			out = append(out, "")
		}
		for i, cell := range row {
			label := []atom{}
			if i < len(head) {
				label = append(label, head[i]...)
				label = append(label, atom{text: ":", flags: fBold})
				label = append(label, atom{kind: atomSpace})
			}
			out = append(out, r.wrap(append(label, cell...), width)...)
		}
	}
	return out
}

type atomsCell []atom

// width is the cell's width on one line.
func (c atomsCell) width() int {
	w, word := 0, false
	for _, a := range c {
		switch a.kind {
		case atomText:
			if !word && w > 0 {
				w++
			}
			w += ansi.StringWidth(a.text)
			word = true
		default:
			word = false
		}
	}
	return w
}

// fitColumns shares room between columns: each gets its natural width
// when everything fits; otherwise narrow columns keep theirs and the rest
// split what is left evenly.
func fitColumns(natural []int, room int) []int {
	widths := make([]int, len(natural))
	total := 0
	for i, n := range natural {
		widths[i] = max(n, 1)
		total += widths[i]
	}
	if total <= room {
		return widths
	}
	left := room
	open := make([]bool, len(natural))
	for i := range open {
		open[i] = true
	}
	for {
		count := 0
		for _, o := range open {
			if o {
				count++
			}
		}
		if count == 0 {
			break
		}
		share := left / count
		changed := false
		for i, o := range open {
			if o && widths[i] <= share {
				open[i] = false
				left -= widths[i]
				changed = true
			}
		}
		if changed {
			continue
		}
		for i, o := range open {
			if o {
				widths[i] = max(share, 1)
			}
		}
		// Hand out the remainder from the left.
		rest := left - share*count
		for i := range open {
			if open[i] && rest > 0 {
				widths[i]++
				rest--
			}
		}
		break
	}
	return widths
}

// Cache keeps rendered Markdown per activity item and width across
// rebuilds: a poll that adds one message renders only that message. Each
// rebuild keeps only the entries it used, so the cache never outgrows the
// transcript.
type Cache struct {
	prev, next map[cacheKey]cacheEntry
}

type cacheKey struct {
	seq   uint64
	width int
	muted bool
	user  bool
}

type cacheEntry struct {
	text  string
	lines []string
}

// Render returns the lines for one item's text, from the cache when the
// item, text, width, and theme match.
func (c *Cache) Render(seq uint64, text string, width int, theme *Theme) []string {
	if c == nil {
		return Markdown(text, width, theme)
	}
	if c.next == nil {
		c.next = map[cacheKey]cacheEntry{}
	}
	k := cacheKey{seq: seq, width: width, muted: theme.muted, user: theme.user}
	if e, ok := c.next[k]; ok && e.text == text {
		return e.lines
	}
	if e, ok := c.prev[k]; ok && e.text == text {
		c.next[k] = e
		return e.lines
	}
	lines := Markdown(text, width, theme)
	c.next[k] = cacheEntry{text: text, lines: lines}
	return lines
}

// Done ends a rebuild: entries not used by it are dropped.
func (c *Cache) Done() {
	if c == nil {
		return
	}
	c.prev, c.next = c.next, nil
}
