package transcript

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestRangeOrderedAndEmpty(t *testing.T) {
	r := Range{Start: Pos{Row: 3, Col: 2}, End: Pos{Row: 1, Col: 5}}
	o := r.Ordered()
	if o.Start.Row != 1 || o.End.Row != 3 {
		t.Fatalf("ordered %+v", o)
	}
	if (Range{Start: Pos{Row: 1, Col: 1}, End: Pos{Row: 1, Col: 1}}).Empty() != true {
		t.Fatal("single cell should be empty")
	}
}

func TestRangeText(t *testing.T) {
	lines := []string{"hello world", "second line", "third"}
	got := (Range{Start: Pos{Row: 0, Col: 6}, End: Pos{Row: 1, Col: 5}}).Text(lines)
	if got != "world\nsecond" {
		t.Fatalf("text %q", got)
	}
	// Reverse drag gives the same text.
	rev := (Range{Start: Pos{Row: 1, Col: 5}, End: Pos{Row: 0, Col: 6}}).Text(lines)
	if rev != got {
		t.Fatalf("reverse %q", rev)
	}
	// Entire rows, trimmed at the trailing edge.
	full := (Range{Start: Pos{Row: 0, Col: 0}, End: Pos{Row: 2, Col: 99}}).Text(lines)
	if full != "hello world\nsecond line\nthird" {
		t.Fatalf("full %q", full)
	}
}

func TestRangeTextStripsStyles(t *testing.T) {
	lines := []string{Dim.Render("styled text")}
	got := (Range{Start: Pos{Row: 0, Col: 0}, End: Pos{Row: 0, Col: 5}}).Text(lines)
	if got != "styled" {
		t.Fatalf("text %q", got)
	}
}

func TestHighlightKeepsOutsideStyle(t *testing.T) {
	line := Dim.Render("hello world")
	plain := ansi.Strip(line)
	if plain != "hello world" {
		t.Fatalf("plain %q", plain)
	}
	out := Highlight(line, Range{Start: Pos{Row: 0, Col: 0}, End: Pos{Row: 0, Col: 4}}, 0)
	if !strings.Contains(ansi.Strip(out), "hello") {
		t.Fatalf("stripped %q", ansi.Strip(out))
	}
	if ansi.StringWidth(out) != 11 {
		t.Fatalf("width %d", ansi.StringWidth(out))
	}
	// Rows outside the range are unchanged.
	if Highlight(line, Range{Start: Pos{Row: 5, Col: 0}, End: Pos{Row: 5, Col: 4}}, 0) != line {
		t.Fatal("outside row changed")
	}
	// A span entirely outside the line changes nothing, even styles.
	if Highlight(line, Range{Start: Pos{Row: 0, Col: 100}, End: Pos{Row: 0, Col: 101}}, 0) != line {
		t.Fatal("outside span changed the line")
	}
}

func TestRangeTextKeepsGraphemes(t *testing.T) {
	// Decomposed accent: cells are [x][e+combining][y].
	lines := []string{"xe\u0301y"}
	if got := (Range{Start: Pos{Row: 0, Col: 0}, End: Pos{Row: 0, Col: 1}}).Text(lines); got != "xe\u0301" {
		t.Fatalf("decomposed text %q", got)
	}
	// Full range keeps every cluster whole.
	if got := (Range{Start: Pos{Row: 0, Col: 0}, End: Pos{Row: 0, Col: 2}}).Text(lines); got != "xe\u0301y" {
		t.Fatalf("full decomposed %q", got)
	}
}

func TestHighlightKeepsGraphemesAndWidth(t *testing.T) {
	lines := []string{"a e\u0301 👍🏽 👨‍👩‍👧‍👦 ｱ normal"}
	plain := lines[0]
	out := Highlight(plain, Range{Start: Pos{Row: 0, Col: 2}, End: Pos{Row: 0, Col: 6}}, 0)
	if got := ansi.Strip(out); got != plain {
		t.Fatalf("stripped %q, want %q", got, plain)
	}
	if ansi.StringWidth(out) != ansi.StringWidth(plain) {
		t.Fatalf("width %d, want %d", ansi.StringWidth(out), ansi.StringWidth(plain))
	}
	// Hover and selected preserve the full line too.
	tools := []ToolRef{{ID: "a", Start: 0, End: 1}}
	if got := ansi.Strip(Hover(plain, tools, 0, 0)); got != plain {
		t.Fatalf("hover stripped %q", got)
	}
	sel := Selected("✓ "+plain, tools, 0, 0, 60)
	if !strings.Contains(ansi.Strip(sel), "✓") {
		t.Fatalf("selected lost the mark: %q", ansi.Strip(sel))
	}
	if ansi.StringWidth(sel) != 60 {
		t.Fatalf("selected width %d, want 60", ansi.StringWidth(sel))
	}
}

func TestHighlightWideCharNoDuplicate(t *testing.T) {
	line := "ＡＢ"
	out := Highlight(line, Range{Start: Pos{Row: 0, Col: 1}, End: Pos{Row: 0, Col: 2}}, 0)
	if got := ansi.Strip(out); got != line {
		t.Fatalf("stripped %q, want %q", got, line)
	}
	if ansi.StringWidth(out) != 4 {
		t.Fatalf("width %d, want 4", ansi.StringWidth(out))
	}
}
