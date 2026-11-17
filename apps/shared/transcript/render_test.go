package transcript

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func ptr(s string) *string { return &s }

// turns is two assistant turns: the first reads a file between two
// messages and makes two requests, the second follows a user message.
func turns() *Thread {
	return &Thread{Activity: []Activity{
		{Type: "message", Sequence: 0, Role: "user", Speaker: "you", Time: "12:15", Text: "what is it?"},
		{Type: "message", Sequence: 1, Role: "assistant", Speaker: "Z", Time: "12:16", Text: "Let me read it."},
		{Type: "tool_use", Sequence: 2, ID: "c1", Name: "read", View: &ToolView{Outcome: "ok", Head: &ToolHead{Kind: "path", Path: "SPEC.md"}}},
		{Type: "usage", Sequence: 3, InputTokens: 1000, OutputTokens: 100, CacheReadTokens: 500, Model: ptr("m-1")},
		{Type: "tool_result", Sequence: 4, ToolUseID: "c1", View: &ToolView{Outcome: "ok", Head: &ToolHead{Kind: "path", Path: "SPEC.md"}}},
		{Type: "message", Sequence: 5, Role: "assistant", Speaker: "Z", Time: "12:16", Text: "Short answer."},
		{Type: "usage", Sequence: 6, InputTokens: 2000, OutputTokens: 200, CacheReadTokens: 1500, Model: ptr("m-1")},
		{Type: "message", Sequence: 7, Role: "user", Speaker: "you", Time: "12:17", Text: "thanks"},
		{Type: "reasoning", Sequence: 8, Time: "12:18", Text: "thinking"},
		{Type: "message", Sequence: 9, Role: "assistant", Speaker: "Z", Time: "12:18", Text: "Welcome."},
		{Type: "usage", Sequence: 10, InputTokens: 10, OutputTokens: 5, Model: ptr("m-2")},
	}}
}

func plain(lines []string) string {
	return ansi.Strip(strings.Join(lines, "\n"))
}

func TestGroupTurnsShowsOneHeaderAndOneUsagePerTurn(t *testing.T) {
	lines, tools := Render(turns(), 100, Options{GroupTurns: true}, nil)
	got := plain(lines)
	if n := strings.Count(got, "▌ Z"); n != 2 {
		t.Fatalf("%d assistant headers, want one per turn:\n%s", n, got)
	}
	want := []string{
		"▌ Z  12:16",
		"  Short answer.\n\n  ──  ↑3.0k ↓300 · 2.0k cached · 2x\n",
		"▌ you  12:17",
		"▌ Z  12:18\n∴ thinking",
		"  Welcome.\n\n  ──  ↑10 ↓5",
	}
	for _, w := range want {
		if !strings.Contains(got, w) {
			t.Fatalf("missing %q:\n%s", w, got)
		}
	}
	// The first turn's usage closes it: after its last message, before the
	// next user message.
	if strings.HasSuffix(got, " ") || strings.Contains(got, "· usage") || strings.Contains(got, "m-1") {
		t.Fatalf("grouped turns use the divider only:\n%s", got)
	}
	if strings.Index(got, "2x") > strings.Index(got, "▌ you  12:17") {
		t.Fatalf("usage out of place:\n%s", got)
	}
	if len(tools) != 1 || !strings.Contains(ansi.Strip(lines[tools[0].Start]), "read SPEC.md") {
		t.Fatalf("tool rows %+v", tools)
	}
}

// The divider names cache writes only when there were any, and the models
// only when more than one ran; it is cut to the width.
func TestGroupedDividerParts(t *testing.T) {
	u := usageSum{requests: 1, input: 1200, output: 30, create: 400, models: []string{"m-1", "m-2"}}
	if got := ansi.Strip(u.divider(100)); got != "  ──  ↑1.2k ↓30 · 400 cache write · m-1, m-2" {
		t.Fatalf("divider %q", got)
	}
	if got := ansi.Strip(u.divider(14)); ansi.StringWidth(got) > 14 || !strings.HasSuffix(got, "…") {
		t.Fatalf("narrow divider %q", got)
	}
}

func TestUngroupedKeepsAHeaderPerMessageAndUsagePerRequest(t *testing.T) {
	got := plain(firstLines(Render(turns(), 100, Options{}, nil)))
	if n := strings.Count(got, "▌ Z"); n != 3 {
		t.Fatalf("%d headers:\n%s", n, got)
	}
	if n := strings.Count(got, "· usage"); n != 3 || strings.Contains(got, "──") || !strings.Contains(got, "  · usage in 1.0k out 100 cache 500/0 m-1") {
		t.Fatalf("usage lines:\n%s", got)
	}
}

func firstLines(lines []string, _ []ToolRef) []string { return lines }
