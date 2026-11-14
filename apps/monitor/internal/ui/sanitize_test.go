package ui

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

func TestCleanTextRemovesTerminalControl(t *testing.T) {
	cases := map[string]string{
		"plain\ttext\nnext":                                   "plain\ttext\nnext",
		"ünïcode 漢字":                                          "ünïcode 漢字",
		"\x1b[8mhidden\x1b[0m shown":                          "hidden shown",
		"progress 10%\rprogress 99%\ndone\r\n":                "progress 99%\ndone\n",
		"a\x1bPtmux;\x1b\x1b]52;c;eA==\x07\x1b\\b":            "ab",
		"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\": "link",
		"bell\x07 del\x7f c1\u009b31m":                        "bell del c131m",
		"stray \x9b byte":                                     "stray \uFFFD byte",
	}
	for in, want := range cases {
		if got := cleanText(in); got != want {
			t.Errorf("cleanText(%q) = %q, want %q", in, got, want)
		}
	}
}

// Thread content reaches the frame without its escape sequences or
// controls, while the monitor's own styles stay.
func TestTranscriptFrameCarriesNoForeignControls(t *testing.T) {
	hostile := "\x1b[8mconcealed\x1b[0m \x1bPpass\x1b\\ \x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\\nover\rwritten"
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		return data.Thread{ID: id, Title: "t\x1b[2Jitle", Cursor: 2, Activity: []data.Activity{
			{Type: "message", Sequence: 0, Role: "user", Speaker: "user", Text: hostile},
			{Type: "tool_result", Sequence: 1, ToolUseID: "c1", View: &data.ToolView{
				Outcome: "ok", Head: &data.ToolHead{Kind: "shell", Command: "echo \x1b[31mred"},
				Body: data.ToolBody{Kind: "streams", Streams: []data.OutputStream{{Name: "stdout", Text: hostile}}},
			}},
		}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	check := func(frame string) {
		t.Helper()
		for _, bad := range []string{"\x1b[8m", "\x1bP", "\x1b]8;", "\x1b[2J", "\x1b[31m", "\r"} {
			if strings.Contains(frame, bad) {
				t.Fatalf("frame carries %q:\n%q", bad, frame)
			}
		}
		if !strings.Contains(frame, "\x1b[") {
			t.Fatal("the monitor's own styles must stay")
		}
	}
	frame := m.render()
	check(frame)
	if out := plain(m); !strings.Contains(out, "concealed") || !strings.Contains(out, "link") || !strings.Contains(out, "written") || strings.Contains(out, "pass") || strings.Contains(out, "over") {
		t.Fatalf("text must stay readable:\n%s", plain(m))
	}
	m.Update(special(tea.KeyTab))
	m.Update(special(tea.KeyEnter))
	check(m.render())
}
