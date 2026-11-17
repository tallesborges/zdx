package transcript

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func TestScrollbarHiddenWhenEverythingFits(t *testing.T) {
	if got := Scrollbar(5, 0, 10); got != nil {
		t.Fatalf("Scrollbar(5, 0, 10) = %v, want nil", got)
	}
	if got := Scrollbar(10, 0, 10); got != nil {
		t.Fatalf("Scrollbar(10, 0, 10) = %v, want nil", got)
	}
}

func TestScrollbarThumbMovesWithOffset(t *testing.T) {
	plain := func(cells []string) string {
		var b strings.Builder
		for _, c := range cells {
			b.WriteString(ansi.Strip(c))
		}
		return b.String()
	}
	top := plain(Scrollbar(100, 0, 10))
	if len([]rune(top)) != 10 || !strings.HasPrefix(top, "█") || strings.Count(top, "█") != 1 {
		t.Fatalf("top = %q, want one thumb cell at the start", top)
	}
	bottom := plain(Scrollbar(100, 90, 10))
	if !strings.HasSuffix(bottom, "█") || strings.Count(bottom, "█") != 1 {
		t.Fatalf("bottom = %q, want one thumb cell at the end", bottom)
	}
	mid := plain(Scrollbar(100, 45, 10))
	runes := []rune(mid)
	if strings.Count(mid, "█") != 1 || runes[0] == '█' || runes[len(runes)-1] == '█' {
		t.Fatalf("mid = %q, want one thumb cell in the middle", mid)
	}
	// A short transcript gets a longer thumb than a long one.
	short := plain(Scrollbar(20, 0, 10))
	if strings.Count(short, "█") <= strings.Count(top, "█") {
		t.Fatalf("short = %q, want a longer thumb than %q", short, top)
	}
	for _, c := range Scrollbar(100, 0, 10) {
		if ansi.StringWidth(c) != 1 {
			t.Fatalf("cell %q is not one cell wide", c)
		}
	}
}
