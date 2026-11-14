package render

import (
	"fmt"
	"strings"
	"testing"
)

// Cases from crates/zdx-bot/src/telegram/html.rs tests.
func TestSanitize(t *testing.T) {
	cases := []struct{ in, want string }{
		{"<b>No attestation</b> — very old (<Android 8) devices", "<b>No attestation</b> — very old (&lt;Android 8) devices"},
		{"<code>Vec<T></code> and <div>x</div>", "<code>Vec&lt;T&gt;</code> and &lt;div&gt;x&lt;/div&gt;"},
		{"<b>bold and <i>italic", "<b>bold and <i>italic</i></b>"},
		{"<b><i>x</b>", "<b><i>x</i></b>"},
		{"stray </b> close", "stray &lt;/b&gt; close"},
		{"a & b &amp; c &nbsp; d &#8212; e &#x2014;", "a &amp; b &amp; c &amp;nbsp; d &#8212; e &#x2014;"},
	}
	for _, c := range cases {
		if got := Sanitize(c.in); got != c.want {
			t.Errorf("Sanitize(%q)\n got %q\nwant %q", c.in, got, c.want)
		}
	}
	valid := "<b>Title</b>\n<i>x</i> <code>a &amp; b</code> " +
		"<a href=\"https://example.com/?a=1&amp;b=2\">link</a>\n" +
		"<blockquote>quote</blockquote><pre>code</pre>"
	if got := Sanitize(valid); got != valid {
		t.Errorf("valid markup changed: %q", got)
	}
	once := Sanitize("<b>a < b</b> & <i>c")
	if Sanitize(once) != once {
		t.Errorf("not idempotent: %q", once)
	}
}

// Cases from markdown.rs truncation tests.
func TestTruncateHTML(t *testing.T) {
	cases := []struct {
		in   string
		max  int
		want string
	}{
		{"<b>hello &amp; goodbye</b>", 9, "<b>hello &amp; …</b>"},
		{"<blockquote><b>one &lt; two</b></blockquote>", 7, "<blockquote><b>one &lt; …</b></blockquote>"},
		{"<task>abcdefghij</task>", 8, "<task>a…"},
		{"<a>abcdefghij</a>", 8, "<a>abcd…"},
		{"<b>short</b>", 10, "<b>short</b>"},
	}
	for _, c := range cases {
		if got := TruncateHTML(c.in, c.max); got != c.want {
			t.Errorf("TruncateHTML(%q, %d) = %q, want %q", c.in, c.max, got, c.want)
		}
	}
}

func TestVisibleUTF16Len(t *testing.T) {
	cases := []struct {
		in   string
		want int
	}{
		{"<b>ab</b>", 2},
		{"&amp;", 2},           // entities count as 2
		{"😀", 2},               // outside the BMP
		{"é中", 2},              // BMP, multibyte
		{"<task>x</task>", 14}, // unknown tags are text
		{`<a href="u">x</a>`, 1},
	}
	for _, c := range cases {
		if got := VisibleUTF16Len(c.in); got != c.want {
			t.Errorf("VisibleUTF16Len(%q) = %d, want %d", c.in, got, c.want)
		}
	}
}

// content is the text without tags: what a split may not change.
func content(s string) string { return stripTags(s) }

// checkSplit asserts every part fits, is balanced on its own, and nothing is
// lost (markdown.rs split_tests::check).
func checkSplit(t *testing.T, in string, max int) []string {
	t.Helper()
	parts := SplitHTML(in, max)
	var joined strings.Builder
	for _, part := range parts {
		if n := UTF16Len(part); n > max {
			t.Fatalf("part over %d: %d", max, n)
		}
		var open []string
		for cursor := 0; cursor < len(part); {
			if tag, ok := parseTelegramTag(part[cursor:]); ok {
				if tag.closing {
					if len(open) == 0 || open[len(open)-1] != tag.name {
						t.Fatalf("unbalanced %s in %q", tag.name, part)
					}
					open = open[:len(open)-1]
				} else {
					open = append(open, tag.name)
				}
				cursor += tag.n
			} else {
				cursor += runeLen(part[cursor:])
			}
		}
		if len(open) > 0 {
			t.Fatalf("unclosed %v in %q", open, part)
		}
		joined.WriteString(content(part))
	}
	if joined.String() != content(in) {
		t.Fatalf("text changed by split")
	}
	return parts
}

func TestSplitHTMLFitsUnchanged(t *testing.T) {
	got := SplitHTML("<b>a</b> &amp; b", MaxMessageLen)
	if len(got) != 1 || got[0] != "<b>a</b> &amp; b" {
		t.Fatalf("%q", got)
	}
}

func TestSplitHTMLBreaksAtLinesThenSpaces(t *testing.T) {
	in := strings.Repeat("a", 3000) + "\n" + strings.Repeat("b ", 1500)
	parts := checkSplit(t, in, MaxMessageLen)
	if len(parts) != 2 || parts[0] != strings.Repeat("a", 3000)+"\n" {
		t.Fatalf("line break: %d parts", len(parts))
	}
	parts = checkSplit(t, strings.Repeat("word ", 1000), MaxMessageLen)
	if len(parts) != 2 || !strings.HasSuffix(parts[0], "word ") {
		t.Fatalf("space break: %d parts", len(parts))
	}
}

func TestSplitHTMLReopensFormatting(t *testing.T) {
	href := `<a href="https://example.com/a?b=1&amp;c=2">`
	in := fmt.Sprintf(`<b>%s <i>x</i></b> %s%s</a> <pre><code class="language-rust">%s</code></pre>`,
		strings.Repeat("bold ", 900), href, strings.Repeat("link ", 900), strings.Repeat("let x = 1;\n", 400))
	parts := checkSplit(t, in, MaxMessageLen)
	if len(parts) < 3 {
		t.Fatalf("%d parts", len(parts))
	}
	if !strings.HasPrefix(parts[1], "<b>") && !strings.HasPrefix(parts[1], href) {
		t.Fatalf("part 2 not reopened: %.40q", parts[1])
	}
	found := false
	for _, p := range parts[1:] {
		found = found || strings.HasPrefix(p, `<pre><code class="language-rust">`)
	}
	if !found {
		t.Fatal("code block not reopened")
	}
}

func TestSplitHTMLKeepsEntitiesWhole(t *testing.T) {
	for _, part := range checkSplit(t, strings.Repeat("&amp;&lt;&#128512;", 800), MaxMessageLen) {
		for i := 0; i < len(part); i++ {
			if part[i] == '&' && entityLen(part[i:]) == 0 {
				t.Fatalf("split entity at %d", i)
			}
		}
	}
}

func TestSplitHTMLMeasuresUTF16(t *testing.T) {
	parts := checkSplit(t, strings.Repeat("😀 中文 ", 1500), MaxMessageLen)
	if len(parts) < 3 {
		t.Fatalf("%d parts", len(parts))
	}
}

func TestSplitHTMLKeepsCodeWhitespace(t *testing.T) {
	in := "<pre><code>" + strings.Repeat("x", 3000) + " " + strings.Repeat("y", 2000) + "</code></pre>"
	parts := checkSplit(t, in, MaxMessageLen)
	if len(parts) != 2 || !strings.HasSuffix(parts[0], " </code></pre>") || !strings.HasPrefix(parts[1], "<pre><code>y") {
		t.Fatalf("%d parts", len(parts))
	}
}

func TestSplitHTMLDropsFormattingThatCannotFit(t *testing.T) {
	anchor := `before <a href="https://e/` + strings.Repeat("a", 5000) + `">x</a> after`
	if got := strings.Join(checkSplit(t, anchor, MaxMessageLen), ""); got != "before x after" {
		t.Fatalf("%q", got)
	}
	deep := strings.Repeat("<b><i><u>", 5) + strings.Repeat("z ", 40) + strings.Repeat("</u></i></b>", 5)
	if parts := checkSplit(t, deep, 60); len(parts) < 2 {
		t.Fatalf("%d parts", len(parts))
	}
	checkSplit(t, "&#"+strings.Repeat("0", 5000)+";"+strings.Repeat("w", 10), MaxMessageLen)
}

func TestSplitHTMLCutsLongWords(t *testing.T) {
	parts := checkSplit(t, strings.Repeat("é", 5000), MaxMessageLen)
	if len(parts) != 2 || UTF16Len(parts[0]) != MaxMessageLen {
		t.Fatalf("%d parts", len(parts))
	}
}

func TestSplitHTMLDropsWhitespaceOnlyParts(t *testing.T) {
	for _, p := range SplitHTML(strings.Repeat("a", 10)+strings.Repeat(" ", 20)+"<b></b>", 10) {
		if strings.TrimSpace(stripTags(p)) == "" {
			t.Fatalf("empty part %q", p)
		}
	}
}
