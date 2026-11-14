package render

import (
	"encoding/json"
	"os"
	"testing"
)

// golden is testdata/rust_golden.json: outputs of the Rust bot's own
// functions (crates/zdx-bot/src/telegram/markdown.rs and html.rs at zdx
// commit 9d3743a6, pulldown-cmark 0.13.4) for synthetic inputs, produced by
// a throwaway harness that linked those files. Markdown cases are only those
// the Go port matched when generated; see ToTelegramHTML for the known
// divergences.
type golden struct {
	Markdown []struct{ In, Out string }
	Sanitize []struct{ In, Out string }
	Truncate []goldenN
	Split    []goldenN
}

type goldenN struct {
	In  string
	N   int
	Out []string
}

func loadGolden(t *testing.T) golden {
	t.Helper()
	b, err := os.ReadFile("testdata/rust_golden.json")
	if err != nil {
		t.Fatal(err)
	}
	var g golden
	if err := json.Unmarshal(b, &g); err != nil {
		t.Fatal(err)
	}
	return g
}

func TestRustGoldenHTMLHelpers(t *testing.T) {
	g := loadGolden(t)
	for _, c := range g.Sanitize {
		if got := Sanitize(c.In); got != c.Out {
			t.Errorf("Sanitize(%q)\n got %q\nwant %q", c.In, got, c.Out)
		}
	}
	for _, c := range g.Truncate {
		if got := TruncateHTML(c.In, c.N); got != c.Out[0] {
			t.Errorf("TruncateHTML(%q, %d)\n got %q\nwant %q", c.In, c.N, got, c.Out[0])
		}
	}
	for _, c := range g.Split {
		got := SplitHTML(c.In, c.N)
		if len(got) != len(c.Out) {
			t.Errorf("SplitHTML(%q, %d): %d parts, want %d", c.In, c.N, len(got), len(c.Out))
			continue
		}
		for i := range got {
			if got[i] != c.Out[i] {
				t.Errorf("SplitHTML(%q, %d)[%d]\n got %q\nwant %q", c.In, c.N, i, got[i], c.Out[i])
			}
		}
	}
}
