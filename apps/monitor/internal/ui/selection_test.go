package ui

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

func TestMouseDragSelectionCopiesTranscript(t *testing.T) {
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		return data.Thread{ID: id, Title: "selection", Cursor: 1, Activity: []data.Activity{
			{Type: "message", Sequence: 0, Role: "assistant", Speaker: "assistant", Text: "hello selection"},
		}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	if m.transcript == nil {
		t.Fatal("transcript not open")
	}
	// The body starts at frame row 3: the assistant header is body
	// row 0 and its text row 1, so pressing at frame row 4 lands on
	// the text and a drag down and right covers "hello selection".
	m.Update(tea.MouseClickMsg{X: 3, Y: 4, Button: tea.MouseLeft})
	tr := m.transcript
	if tr.sel == nil {
		t.Fatal("click inside the body did not start a selection")
	}
	m.Update(tea.MouseMotionMsg{X: 13, Y: 4, Button: tea.MouseLeft})
	if tr.sel.Empty() {
		t.Fatal("drag motion did not grow the selection")
	}
	text := strings.TrimSpace(tr.sel.Text(tr.selLines))
	if text == "" {
		t.Fatal("empty selection text")
	}
	out := plain(m)
	if !strings.Contains(out, "hello selection") {
		t.Fatalf("transcript lost from the frame:\n%s", out)
	}
	// Release copies and clears; a click outside the body starts none.
	m.Update(tea.MouseReleaseMsg{X: 3, Y: 4, Button: tea.MouseLeft})
	if tr.sel != nil {
		t.Fatal("release did not clear the selection")
	}
	m.Update(tea.MouseClickMsg{X: 3, Y: 1, Button: tea.MouseLeft})
	if tr.sel != nil {
		t.Fatal("click on chrome started a selection")
	}
}

func TestMouseClickToolRowOpensDetail(t *testing.T) {
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		return data.Thread{ID: id, Title: "tool", Cursor: 1, Activity: []data.Activity{
			{Type: "message", Sequence: 0, Role: "assistant", Speaker: "assistant", Text: "work"},
			{Type: "tool_use", Sequence: 1, ID: "c1", Name: "bash"},
		}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	tr := m.transcript
	if tr == nil {
		t.Fatal("no transcript")
	}
	// Find the tool row in body coordinates: it follows the header.
	// Press and release without dragging.
	m.Update(tea.MouseClickMsg{X: 3, Y: 6, Button: tea.MouseLeft})
	m.Update(tea.MouseReleaseMsg{X: 3, Y: 6, Button: tea.MouseLeft})
	if tr.pane == nil {
		t.Fatalf("click on a tool row did not open the detail; frame:\n%s", plain(m))
	}
}

func TestPlainClickCopiesNothing(t *testing.T) {
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		return data.Thread{ID: id, Title: "selection", Cursor: 1, Activity: []data.Activity{
			{Type: "message", Sequence: 0, Role: "assistant", Speaker: "assistant", Text: "hello selection"},
		}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	tr := m.transcript
	if tr == nil {
		t.Fatal("no transcript")
	}
	// Text row is frame row 4 (body starts at 3, header at body 0).
	_, _ = m.Update(tea.MouseClickMsg{X: 5, Y: 4, Button: tea.MouseLeft})
	_, cmd := m.Update(tea.MouseReleaseMsg{X: 5, Y: 4, Button: tea.MouseLeft})
	if cmd != nil {
		t.Fatal("plain click returned a clipboard command")
	}
	if tr.sel != nil {
		t.Fatal("plain click left a selection")
	}
	if tr.pane != nil {
		t.Fatal("plain click on text opened a pane")
	}
}

func TestHoverResetsOnScroll(t *testing.T) {
	f := &fake{thread: func(_ context.Context, id string, _ *uint64) (data.Thread, error) {
		return data.Thread{ID: id, Title: "selection", Cursor: 1, Activity: []data.Activity{
			{Type: "message", Sequence: 0, Role: "assistant", Speaker: "assistant", Text: "hello selection with enough lines to scroll a little"},
			{Type: "tool_use", Sequence: 1, ID: "c1", Name: "bash"},
		}}, nil
	}}
	m, _ := newModel(t, f)
	pump(m, m.openTranscript("t-1", "t-1"))
	tr := m.transcript
	if tr == nil {
		t.Fatal("no transcript")
	}
	tr.hoverLine = 2
	// Keyboard scroll must drop the stale hover mark.
	_, _ = m.Update(tea.KeyPressMsg{Code: 'j'})
	if tr.hoverLine != -1 {
		t.Fatalf("scroll left hoverLine %d", tr.hoverLine)
	}
}
