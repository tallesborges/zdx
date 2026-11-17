package transcript

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"
)

func ptr(s string) *string { return &s }

// turns is two assistant turns: the first reads a file between two
// messages and makes two requests, the second follows a user message. As
// the engine saves them, a request's usage comes before its items.
func turns() *Thread {
	return &Thread{Activity: []Activity{
		{Type: "message", Sequence: 0, Role: "user", Speaker: "you", Time: "12:15", Text: "what is it?"},
		{Type: "usage", Sequence: 1, Time: "12:16", InputTokens: 1000, OutputTokens: 100, CacheReadTokens: 500, Model: ptr("m-1")},
		{Type: "message", Sequence: 2, Role: "assistant", Speaker: "Z", Time: "12:16", Text: "Let me read it."},
		{Type: "tool_use", Sequence: 3, ID: "c1", Name: "read", View: &ToolView{Outcome: "ok", Head: &ToolHead{Kind: "path", Path: "SPEC.md"}}},
		{Type: "tool_result", Sequence: 4, ToolUseID: "c1", View: &ToolView{Outcome: "ok", Head: &ToolHead{Kind: "path", Path: "SPEC.md"}}},
		{Type: "usage", Sequence: 5, Time: "12:16", InputTokens: 2000, OutputTokens: 200, CacheReadTokens: 1500, Model: ptr("m-1")},
		{Type: "message", Sequence: 6, Role: "assistant", Speaker: "Z", Time: "12:16", Text: "Short answer."},
		{Type: "message", Sequence: 7, Role: "user", Speaker: "you", Time: "12:17", Text: "thanks"},
		{Type: "usage", Sequence: 8, Time: "12:18", InputTokens: 10, OutputTokens: 5, Model: ptr("m-2")},
		{Type: "reasoning", Sequence: 9, Time: "12:18", Text: "thinking"},
		{Type: "message", Sequence: 10, Role: "assistant", Speaker: "Z", Time: "12:18", Text: "Welcome."},
	}}
}

func plain(lines []string) string {
	return ansi.Strip(strings.Join(lines, "\n"))
}

func TestRenderShowsOneHeaderAndOneUsagePerTurn(t *testing.T) {
	lines, tools := Render(turns(), 100, Options{}, nil)
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
		t.Fatalf("usage shows only in the divider:\n%s", got)
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

// A turn whose requests showed nothing (a silent reply, or a request
// whose items are not saved yet) still gets a header, with the last
// assistant speaker, so its usage never sits under the user message.
func TestUsageOnlyTurnGetsItsOwnHeader(t *testing.T) {
	th := turns()
	th.Activity = append(th.Activity,
		Activity{Type: "message", Sequence: 11, Role: "user", Speaker: "you", Time: "12:19", Text: "update"},
		Activity{Type: "usage", Sequence: 12, Time: "12:20", InputTokens: 7, OutputTokens: 4, Model: ptr("m-1")},
		Activity{Type: "message", Sequence: 13, Role: "user", Speaker: "you", Time: "12:21", Text: "and now?"},
		Activity{Type: "usage", Sequence: 14, Time: "12:22", InputTokens: 8, OutputTokens: 3, Model: ptr("m-1")},
	)
	got := plain(firstLines(Render(th, 100, Options{}, nil)))
	for _, w := range []string{
		"  update\n\n▌ Z  12:20\n  ──  ↑7 ↓4\n\n▌ you  12:21",
		"  and now?\n\n▌ Z  12:22\n  ──  ↑8 ↓3",
	} {
		if !strings.Contains(got, w) {
			t.Fatalf("missing %q:\n%s", w, got)
		}
	}
}

// A notice or an interruption ends the turn: its usage closes it above
// them, and a turn with no earlier speaker is the assistant's.
func TestTurnEndsBeforeANotice(t *testing.T) {
	th := &Thread{Activity: []Activity{
		{Type: "message", Sequence: 0, Role: "user", Speaker: "you", Time: "12:15", Text: "go"},
		{Type: "usage", Sequence: 1, Time: "12:16", InputTokens: 5, OutputTokens: 2},
		{Type: "notice", Sequence: 2, Kind: "turn_failed", Message: "boom"},
	}}
	got := plain(firstLines(Render(th, 100, Options{}, nil)))
	if want := "  go\n\n▌ assistant  12:16\n  ──  ↑5 ↓2\n\n✗ turn failed: boom"; got != "▌ you  12:15\n"+want {
		t.Fatalf("got:\n%s", got)
	}
}

func firstLines(lines []string, _ []ToolRef) []string { return lines }
