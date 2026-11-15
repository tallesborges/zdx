package transcript

import (
	"regexp"
	"strings"
	"testing"
	"time"

	"github.com/charmbracelet/x/ansi"
)

func mdPlain(src string, width int) []string {
	var out []string
	for _, l := range Markdown(src, width, Normal) {
		out = append(out, ansi.Strip(l))
	}
	return out
}

func joined(lines []string) string { return strings.Join(lines, "\n") }

func TestMarkdownBoldAndInlineCodeShowNoMarkers(t *testing.T) {
	src := "**Both `:g` and `#` worked together.**\n\nUse `go vet` and *read* the ~~old~~ output."
	styled := Markdown(src, 80, Normal)
	plain := joined(mdPlain(src, 80))
	for _, raw := range []string{"**", "`", "~~", "*read*"} {
		if strings.Contains(plain, raw) {
			t.Fatalf("raw %q left in:\n%s", raw, plain)
		}
	}
	if !strings.Contains(plain, "Both :g and # worked together.") || !strings.Contains(plain, "Use go vet and read the old output.") {
		t.Fatalf("text:\n%s", plain)
	}
	all := joined(styled)
	for _, want := range []string{
		Normal.paint(fBold, "Both "),
		Normal.paint(fBold|fCode, ":g"),
		Normal.paint(fBold, " worked together."),
		Normal.paint(fCode, "go vet"),
		Normal.paint(fItalic, "read"),
		Normal.paint(fStrike, "old"),
	} {
		if !strings.Contains(all, want) {
			t.Fatalf("missing styled %q in %q", want, all)
		}
	}
}

func TestMarkdownLineBreaksAreKept(t *testing.T) {
	got := mdPlain("**How to fix it**\nRun this in your terminal:", 80)
	if joined(got) != "How to fix it\nRun this in your terminal:" {
		t.Fatalf("got %q", got)
	}
}

func TestMarkdownNestedLists(t *testing.T) {
	src := "- one\n- two\n  - child\n    - grandchild\n  1. first\n  2. second\n- [x] done\n- [ ] open"
	want := []string{
		"• one",
		"• two",
		"  ◦ child",
		"    ▪ grandchild",
		"  1. first",
		"  2. second",
		"• ☑ done",
		"• ☐ open",
	}
	if got := mdPlain(src, 80); joined(got) != joined(want) {
		t.Fatalf("got\n%s\nwant\n%s", joined(got), joined(want))
	}
	// A wrapped item continues under its text, not under the bullet.
	got := mdPlain("- a long list item that has to wrap at this width", 20)
	for _, l := range got[1:] {
		if !strings.HasPrefix(l, "  ") || strings.HasPrefix(l, "  •") {
			t.Fatalf("continuation %q in %q", l, got)
		}
	}
}

func TestMarkdownFencedCodeKeepsEveryCharacter(t *testing.T) {
	long := "rustup component add rust-analyzer --toolchain 1.97.1-aarch64-apple-darwin-and-a-long-suffix"
	src := "Run:\n\n```sh\n" + long + "\n\tindented\n```\n\nThen check."
	for _, w := range []int{20, 40, 120} {
		got := mdPlain(src, w)
		i := indexOf(got, "sh")
		if i < 0 {
			t.Fatalf("%d: no language label: %q", w, got)
		}
		var code []string
		for _, l := range got[i+1:] {
			switch {
			case strings.HasPrefix(l, "▎ "):
				code = append(code, strings.TrimPrefix(l, "▎ "))
			case strings.HasPrefix(l, "┊ "):
				code[len(code)-1] += strings.TrimPrefix(l, "┊ ")
			}
		}
		if len(code) != 2 || code[0] != long || code[1] != "    indented" {
			t.Fatalf("%d: code %q", w, code)
		}
		if strings.Contains(joined(got), "```") {
			t.Fatalf("%d: fence markers shown", w)
		}
		for _, l := range got {
			if ansi.StringWidth(l) > w {
				t.Fatalf("%d: line %q too wide", w, l)
			}
		}
	}
}

func TestMarkdownUnclosedFenceIsCode(t *testing.T) {
	got := mdPlain("Partial answer:\n\n```go\nfunc main() {\n\tfmt.Println(\"**not bold**\")", 60)
	want := "Partial answer:\n\ngo\n▎ func main() {\n▎     fmt.Println(\"**not bold**\")"
	if joined(got) != want {
		t.Fatalf("got\n%s", joined(got))
	}
}

func TestMarkdownLinks(t *testing.T) {
	got := joined(mdPlain("See [the docs](https://example.com/docs), <https://example.com/a>, https://example.com/b and [https://example.com/c](https://example.com/c).", 200))
	want := "See the docs (https://example.com/docs), https://example.com/a, https://example.com/b and https://example.com/c."
	if got != want {
		t.Fatalf("got  %q\nwant %q", got, want)
	}
	styled := joined(Markdown("[docs](https://example.com)", 80, Normal))
	if strings.Contains(styled, "\x1b]8") {
		t.Fatal("no hyperlinks are emitted")
	}
}

func TestMarkdownTables(t *testing.T) {
	src := "| Tool | Status | Notes |\n|---|:---:|---:|\n| rustup | ok | installed with the default profile |\n| cargo | missing | — |"
	got := mdPlain(src, 60)
	want := []string{
		"Tool   │ Status  │                              Notes",
		"───────┼─────────┼───────────────────────────────────",
		"rustup │   ok    │ installed with the default profile",
		"cargo  │ missing │                                  —",
	}
	if joined(got) != joined(want) {
		t.Fatalf("got\n%s\nwant\n%s", joined(got), joined(want))
	}
	// Narrow: cells wrap inside their columns, every line fits.
	for _, l := range mdPlain(src, 28) {
		if ansi.StringWidth(l) > 28 {
			t.Fatalf("line %q too wide", l)
		}
	}
	// Too narrow for columns: one "header: value" line per cell.
	got = mdPlain(src, 10)
	if got[0] != "Tool:" || got[1] != "rustup" || got[2] != "Status: ok" {
		t.Fatalf("records %q", got)
	}
}

func TestMarkdownHTMLIsLiteral(t *testing.T) {
	src := "<script>alert(1)</script>\n\nInline <b>bold?</b> and <img src=x onerror=y>."
	got := joined(mdPlain(src, 80))
	for _, want := range []string{"<script>alert(1)</script>", "<b>bold?</b>", "<img src=x onerror=y>"} {
		if !strings.Contains(got, want) {
			t.Fatalf("missing literal %q in:\n%s", want, got)
		}
	}
}

// sgr matches the style sequences the renderer itself writes.
var sgr = regexp.MustCompile("\x1b\\[[0-9;:]*m")

func TestMarkdownNeverPassesControlSequences(t *testing.T) {
	inputs := []string{
		"plain \x1b[31mred\x1b[0m and \x1b]8;;https://evil.example\x07click\x1b]8;;\x07",
		"entity &#x1b;[2J and &#27;]8;;https://evil\x07 and C1 &#155;31m &#x9b;2J",
		"`code \x1b]0;title\x07` and **bold \x1bP+q\x1b\\**",
		"```\n\x1b]52;c;ZXZpbA==\x07copy\r\nover\n```",
		"[label](https://x.example/&#x1b;[31m) and \u009b6n",
		"| a\x1b[5m | b |\n|---|---|\n| \x1b]8;;x\x07c | d |",
	}
	for _, in := range inputs {
		for _, theme := range []*Theme{Normal, Muted} {
			for _, l := range Markdown(in, 40, theme) {
				rest := sgr.ReplaceAllString(l, "")
				for _, r := range rest {
					if r < 0x20 || r == 0x7f || (r >= 0x80 && r <= 0x9f) {
						t.Fatalf("control %U left from %q: %q", r, in, l)
					}
				}
			}
		}
	}
}

func TestMarkdownFitsAnyWidth(t *testing.T) {
	srcs := []string{
		"**日本語のテキスト** と `コード` を含む長い段落です。折り返しを確認します。",
		"Emoji 🚀🚀🚀 and a verylongwordwithoutanyspacesthatmustbesplitacrosslines.",
		"> - quoted list with `code` and **bold** text that wraps\n>   > nested quote",
		"1. one\n   ```\n   code in a list item that is long enough to wrap\n   ```\n2. two",
		"| 日本 | 語 |\n|---|---|\n| 🚀 | テキスト |",
		// Tabs in raw HTML count by their expanded width.
		"<div>\n\t\t\t\t\tx\t\t\t\t\t\t\t\ty\n</div>",
	}
	for _, src := range srcs {
		for w := 2; w <= 60; w++ {
			for _, l := range Markdown(src, w, Normal) {
				if lw := ansi.StringWidth(l); lw > w {
					t.Fatalf("width %d: line %q is %d wide (from %q)", w, ansi.Strip(l), lw, src)
				}
			}
		}
	}
}

// A word longer than the line is split across lines with nothing lost, in
// time linear in its length: each piece measures only what can fit, not the
// rest of the word (a 400 KB token took about a minute before).
func TestMarkdownLongWordSplitsInLinearTime(t *testing.T) {
	for _, unit := range []string{"x", "日", "👨‍👩‍👧"} {
		word := strings.Repeat(unit, 400_000/len(unit))
		start := time.Now()
		lines := Markdown("see "+word+" end", 80, Normal)
		if elapsed := time.Since(start); elapsed > 15*time.Second {
			t.Fatalf("%q: %v", unit, elapsed)
		}
		var got strings.Builder
		for _, l := range lines {
			if ansi.StringWidth(l) > 80 {
				t.Fatalf("%q: line %d wide", unit, ansi.StringWidth(l))
			}
			got.WriteString(ansi.Strip(l))
		}
		if strings.ReplaceAll(got.String(), " ", "") != "see"+word+"end" {
			t.Fatalf("%q: text changed", unit)
		}
	}
}

// fitPrefix reads a window of the word; it must cut exactly where
// ansi.Truncate does, also when a cluster crosses the window's end.
func TestFitPrefixMatchesTruncate(t *testing.T) {
	zalgo := "e" + strings.Repeat("\u0301", 300)
	for _, s := range []string{
		strings.Repeat("ab", 100),
		strings.Repeat("🇧🇷", 60),
		strings.Repeat("👨‍👩‍👧‍👦", 30),
		strings.Repeat("e\u0301", 80),
		strings.Repeat(zalgo, 3),
		"a" + zalgo + strings.Repeat("日", 50),
	} {
		for cells := 1; cells <= 12; cells++ {
			if got, want := fitPrefix(s, cells), ansi.Truncate(s, cells, ""); got != want {
				t.Fatalf("cells %d of %q: %q, want %q", cells, s[:min(len(s), 24)], got, want)
			}
		}
	}
}

// A carriage return from an entity is a space: the text before it stays.
func TestMarkdownEntityCarriageReturnKeepsText(t *testing.T) {
	if got := joined(mdPlain("abc&#13;def and x&#10;y", 80)); got != "abc def and x y" {
		t.Fatalf("got %q", got)
	}
}

func TestMarkdownCacheReusesAndDrops(t *testing.T) {
	var c Cache
	a := c.Render(1, "**one**", 40, Normal)
	b := c.Render(1, "**one**", 40, Normal)
	if &a[0] != &b[0] {
		t.Fatal("same item, text, and width must reuse the lines")
	}
	c.Done()
	d := c.Render(1, "**one**", 40, Normal)
	if &a[0] != &d[0] {
		t.Fatal("the next rebuild must reuse the lines")
	}
	if e := c.Render(1, "**one** edited", 40, Normal); ansi.Strip(e[0]) != "one edited" {
		t.Fatalf("changed text must render again: %q", e)
	}
	if f := c.Render(1, "**one**", 20, Normal); &f[0] == &a[0] {
		t.Fatal("another width must render again")
	}
	c.Done()
	c.Done()
	if len(c.prev) != 0 {
		t.Fatalf("unused entries must be dropped, %d left", len(c.prev))
	}
}

func indexOf(lines []string, s string) int {
	for i, l := range lines {
		if l == s {
			return i
		}
	}
	return -1
}

func BenchmarkTranscriptMarkdown(b *testing.B) {
	var acts []Activity
	for i := 0; i < 400; i++ {
		acts = append(acts, Activity{Type: "message", Sequence: uint64(i), Role: "assistant", Text: strings.Repeat("**Heading**\n\n- item with `code` and a [link](https://example.com)\n\n```sh\necho hello\n```\n\n", 3)})
	}
	th := &Thread{Activity: acts}
	b.Run("cold", func(b *testing.B) {
		for b.Loop() {
			Render(th, 100, Options{}, nil)
		}
	})
	b.Run("warm", func(b *testing.B) {
		var c Cache
		Render(th, 100, Options{}, &c)
		c.Done()
		for b.Loop() {
			Render(th, 100, Options{}, &c)
			c.Done()
		}
	})
}

func TestMarkdownLinkUnderlineSpansSpaces(t *testing.T) {
	got := joined(Markdown("See [the vet docs](https://example.com) now.", 80, Normal))
	if !strings.Contains(got, Normal.paint(fLink, "the vet docs")) {
		t.Fatalf("the link is not one run: %q", got)
	}
}
