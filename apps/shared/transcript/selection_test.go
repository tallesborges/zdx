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
}
