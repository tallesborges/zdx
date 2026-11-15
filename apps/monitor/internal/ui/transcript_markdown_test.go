package ui

import (
	"strings"
	"testing"

	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

// TestTranscriptMarkdownKeepsToolRows: Markdown changes message heights;
// tool rows, their selection, and the detail pane still point at the right
// call, and a refresh with new text does not move them.
func TestTranscriptMarkdownKeepsToolRows(t *testing.T) {
	ok := true
	th := &data.Thread{ID: "t", Activity: []data.Activity{
		{Type: "message", Sequence: 0, Role: "assistant", Speaker: "Z", Text: "**Plan**\n\n- one\n- two\n\n```sh\necho hi\n```"},
		{Type: "tool_use", Sequence: 1, ID: "c1", Name: "bash"},
		{Type: "tool_result", Sequence: 2, ToolUseID: "c1", OK: &ok, View: &data.ToolView{Outcome: "ok", Head: &data.ToolHead{Kind: "shell", Command: "echo hi"}}},
		{Type: "message", Sequence: 3, Role: "assistant", Speaker: "Z", Text: "Done. See `out.txt`."},
		{Type: "tool_use", Sequence: 4, ID: "c2", Name: "read"},
	}}
	ts := &transcriptState{threadID: "t", thread: th, selTool: -1, viewRows: 20}
	for _, w := range []int{40, 100} {
		ts.rebuild(w)
		if len(ts.tools) != 2 {
			t.Fatalf("%d: tools %+v", w, ts.tools)
		}
		for i, id := range []string{"c1", "c2"} {
			ref := ts.tools[i]
			line := ansi.Strip(ts.lines[ref.Start])
			if ref.ID != id || !strings.Contains(line, map[string]string{"c1": "bash $ echo hi", "c2": "read"}[id]) {
				t.Fatalf("%d: tool %d at %d is %q", w, i, ref.Start, line)
			}
		}
		for _, l := range ts.lines {
			if ansi.StringWidth(l) > w {
				t.Fatalf("%d: line too wide: %q", w, ansi.Strip(l))
			}
		}
	}
	ts.selectTool(1, 20)
	ts.selectTool(1, 20)
	if ts.tools[ts.selTool].ID != "c2" {
		t.Fatalf("selected %d", ts.selTool)
	}
	// New text above does not change which call is selected.
	th.Activity[0].Text += "\n\nOne more **bold** paragraph."
	ts.rebuild(100)
	if ts.tools[ts.selTool].ID != "c2" || !strings.Contains(ansi.Strip(ts.lines[ts.tools[ts.selTool].Start]), "read") {
		t.Fatal("selection moved")
	}
	if !strings.Contains(ansi.Strip(strings.Join(ts.lines, "\n")), "One more bold paragraph.") {
		t.Fatal("edited text not rendered")
	}
}
