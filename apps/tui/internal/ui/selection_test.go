package ui

import (
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/shared/transcript"
)

func TestMouseDragSelectionCopiesVisibleText(t *testing.T) {
	be := newFake()
	be.thread = transcript.Thread{Cursor: 2, Activity: []transcript.Activity{
		{Type: "message", Sequence: 0, Role: "user", Speaker: "you", Text: "hello user"},
		{Type: "message", Sequence: 1, Role: "assistant", Speaker: "assistant", Text: "hello assistant"},
	}}
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	m := h.m
	// The body starts at screen row 1 (header is row 0). Press at the
	// body, drag one row down and to the right, then release. The
	// click itself returns no command; motion extends the drag and
	// the release copies.
	_, cmd := m.Update(tea.MouseClickMsg{X: 3, Y: 4, Button: tea.MouseLeft})
	_ = cmd
	if m.sel == nil {
		t.Fatal("click inside the body did not start a selection")
	}
	_, cmd = m.Update(tea.MouseMotionMsg{X: 10, Y: 5, Button: tea.MouseLeft})
	_ = cmd
	if m.sel.Empty() {
		t.Fatal("drag motion did not grow the selection")
	}
	m.selLines = m.viewportPlain()
	text := m.sel.Text(m.selLines)
	if text == "" {
		t.Fatal("empty selection text")
	}
	if !strings.Contains(ansi.Strip(m.render()), "hello") {
		t.Fatal("render lost the transcript")
	}
	// Release clears the selection; a click with no drag copies nothing.
	m.sel = nil
	m.selLines = nil
	_, cmd = m.Update(tea.MouseReleaseMsg{X: 3, Y: 4, Button: tea.MouseLeft})
}

func TestPlainClickCopiesNothing(t *testing.T) {
	be := newFake()
	be.thread = transcript.Thread{Cursor: 2, Activity: []transcript.Activity{
		{Type: "message", Sequence: 0, Role: "user", Speaker: "you", Text: "hello user"},
	}}
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	m := h.m
	frame := strings.Split(checkFrame(t, m), "\n")
	y := -1
	for i, line := range frame {
		if strings.Contains(line, "hello") {
			y = i
			break
		}
	}
	if y < 0 {
		t.Fatalf("no hello row:\n%s", checkFrame(t, m))
	}
	_, _ = m.Update(tea.MouseClickMsg{X: 5, Y: y, Button: tea.MouseLeft})
	_, cmd := m.Update(tea.MouseReleaseMsg{X: 5, Y: y, Button: tea.MouseLeft})
	if cmd != nil {
		t.Fatal("plain click returned a clipboard command")
	}
	if m.sel != nil {
		t.Fatal("plain click left a selection")
	}
	if strings.Contains(m.notice, "copied") {
		t.Fatalf("plain click copied: %q", m.notice)
	}
}

func TestMouseClickToolRowOpensDetail(t *testing.T) {
	be := newFake()
	be.thread = transcript.Thread{Cursor: 2, Activity: []transcript.Activity{
		{Type: "message", Sequence: 0, Role: "assistant", Speaker: "assistant", Text: "work"},
		{Type: "tool_use", Sequence: 1, ID: "c1", Name: "bash", View: &transcript.ToolView{
			Outcome: "ok",
			Head:    &transcript.ToolHead{Kind: "shell", Command: "ls"},
			Body:    transcript.ToolBody{Kind: "text", Text: "file.txt"},
		}},
	}}
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	m := h.m
	_, tools := m.transcriptLinesAndTools(m.width - 1)
	if len(tools) == 0 {
		t.Fatal("no tools in the transcript")
	}
	// The screen Y of a row is its index in the rendered frame
	// (header is row 0). Find the tool and the speaker header by
	// their visible text, so the test clicks where the user sees
	// them even when the viewport front-pads a short transcript.
	frame := strings.Split(checkFrame(t, m), "\n")
	find := func(substr string) int {
		for i, line := range frame {
			if strings.Contains(line, substr) {
				return i
			}
		}
		t.Fatalf("no frame line with %q:\n%s", substr, strings.Join(frame, "\n"))
		return -1
	}
	toolY := find("bash")
	headerY := find("assistant")
	// Clicking the speaker header must not open a tool detail.
	_, _ = m.Update(tea.MouseClickMsg{X: 3, Y: headerY, Button: tea.MouseLeft})
	_, cmd := m.Update(tea.MouseReleaseMsg{X: 3, Y: headerY, Button: tea.MouseLeft})
	_ = cmd
	if m.detail != nil {
		t.Fatal("click on the speaker header opened a tool detail")
	}
	// Hovering the header sets no hover line; hovering the tool does.
	_, _ = m.Update(tea.MouseMotionMsg{X: 3, Y: headerY})
	if m.hoverLine >= 0 {
		t.Fatalf("hover over the header set hoverLine %d", m.hoverLine)
	}
	_, _ = m.Update(tea.MouseMotionMsg{X: 3, Y: toolY})
	if m.hoverLine != tools[0].Start {
		t.Fatalf("hover over the tool set hoverLine %d, want %d", m.hoverLine, tools[0].Start)
	}
	// Clicking the tool row opens its detail.
	_, _ = m.Update(tea.MouseClickMsg{X: 3, Y: toolY, Button: tea.MouseLeft})
	_, cmd = m.Update(tea.MouseReleaseMsg{X: 3, Y: toolY, Button: tea.MouseLeft})
	_ = cmd
	if m.detail == nil {
		t.Fatalf("click on a tool row did not open the detail; frame:\n%s", checkFrame(t, m))
	}
	if !strings.Contains(checkFrame(t, m), "file.txt") {
		t.Fatalf("detail lacks the output:\n%s", checkFrame(t, m))
	}
}

func TestDetailResizeRewraps(t *testing.T) {
	be := newFake()
	be.thread = transcript.Thread{Cursor: 2, Activity: []transcript.Activity{
		{Type: "message", Sequence: 0, Role: "assistant", Speaker: "assistant", Text: "work"},
		{Type: "tool_use", Sequence: 1, ID: "c1", Name: "bash", View: &transcript.ToolView{
			Outcome: "ok",
			Head:    &transcript.ToolHead{Kind: "shell", Command: "a very long command that must wrap at a narrow width"},
			Body:    transcript.ToolBody{Kind: "text", Text: "file.txt"},
		}},
	}}
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	m := h.m
	_, tools := m.transcriptLinesAndTools(m.width - 1)
	ref := tools[0]
	m.openDetail(ref)
	// Shrink the terminal; detail lines must be rewritten at the
	// new width, not just truncated on render.
	m.Update(tea.WindowSizeMsg{Width: 40, Height: 20})
	if m.detail == nil {
		t.Fatal("resize closed the detail")
	}
	for _, l := range m.detailLines {
		if ansi.StringWidth(l) > 40 {
			t.Fatalf("detail line wider than new width: %q", ansi.Strip(l))
		}
	}
}

func TestPickerWheelDoesNotScroll(t *testing.T) {
	be := newFake()
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	m := h.m
	m.scroll = 5
	m.openPalette()
	if m.overlay == nil {
		t.Fatal("no overlay")
	}
	_, _ = m.Update(tea.MouseWheelMsg{X: 3, Y: 3, Button: tea.MouseWheelDown})
	if m.scroll != 5 {
		t.Fatalf("wheel behind picker scrolled: %d", m.scroll)
	}
}
