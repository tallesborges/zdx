package transcript

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestToolAt(t *testing.T) {
	tools := []ToolRef{{ID: "a", Start: 2, End: 3}, {ID: "b", Start: 5, End: 8}}
	for line, want := range map[int]string{2: "a", 5: "b", 7: "b"} {
		got, ok := ToolAt(tools, line)
		if !ok || got.ID != want {
			t.Fatalf("line %d: got %+v %v", line, got, ok)
		}
	}
	for _, line := range []int{0, 1, 3, 4, 8} {
		if _, ok := ToolAt(tools, line); ok {
			t.Fatalf("line %d: unexpected hit", line)
		}
	}
}

func TestHover(t *testing.T) {
	tools := []ToolRef{{ID: "a", Start: 1, End: 2}, {ID: "b", Start: 2, End: 3}}
	line := "tool row"
	out := Hover(line, tools, 1, 1)
	if ansi.StringWidth(out) != 8 {
		t.Fatalf("width %d", ansi.StringWidth(out))
	}
	if !strings.Contains(ansi.Strip(out), "tool") {
		t.Fatalf("stripped %q", ansi.Strip(out))
	}
	if Hover(line, tools, 0, 1) != line {
		t.Fatal("off-row should be unchanged")
	}
	// A neighbouring tool row is not the hovered row.
	if Hover(line, tools, 2, 1) != line {
		t.Fatal("neighbour tool row should be unchanged")
	}
}
