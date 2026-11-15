package ui

import (
	"encoding/json"
	"strings"

	"github.com/tallesborges/zdx/apps/shared/transcript"
	"github.com/tallesborges/zdx/apps/tui/internal/engine"
)

// liveBase keeps live items' sequences apart from persisted ones, so the
// Markdown cache never mixes them up.
const liveBase = uint64(1) << 60

// maxOutputTail bounds a running tool's streamed output kept on screen.
const maxOutputTail = 16 << 10

// live is one turn the screen shows past the saved thread, built from its
// events. Every delta is appended as it arrives; nothing is merged away.
// The saved items replace it once the thread view has the turn.
type live struct {
	turn  uint64
	ended bool
	items []transcript.Activity
	// open is the index of the reasoning or assistant item deltas extend,
	// or -1.
	open     int
	openKind string
	// running maps a tool call id to its tool_running item.
	running map[string]int
	next    uint64
	usage   *engine.AgentEvent
	failed  bool
}

func newLive(turn uint64) *live { return &live{turn: turn, open: -1, running: map[string]int{}} }

func (l *live) add(a transcript.Activity) int {
	a.Sequence = liveBase + l.turn<<24 + l.next
	l.next++
	l.items = append(l.items, a)
	return len(l.items) - 1
}

// userItem is a message the user sent; ok is false for none (a rerun).
func userItem(speaker, text string, images int) (transcript.Activity, bool) {
	if images > 0 {
		text = strings.TrimSpace(text + "\n\n" + transcript.Count(images, "image", "images") + " attached")
	}
	return transcript.Activity{Type: "message", Role: "user", Speaker: speaker, Text: text}, text != ""
}

// user shows a message of this turn.
func (l *live) user(text string, images int) {
	if a, ok := userItem("you", text, images); ok {
		l.close()
		l.add(a)
	}
}

// prependUser shows a message of this turn whose receipt came after the
// turn's first events.
func (l *live) prependUser(text string, images int) {
	a, ok := userItem("you", text, images)
	if !ok {
		return
	}
	a.Sequence = liveBase + l.turn<<24 + l.next
	l.next++
	l.items = append([]transcript.Activity{a}, l.items...)
	if l.open >= 0 {
		l.open++
	}
	for id, i := range l.running {
		l.running[id] = i + 1
	}
}

func (l *live) close() { l.open, l.openKind = -1, "" }

func (l *live) extend(kind, text string) {
	if l.open >= 0 && l.openKind == kind {
		l.items[l.open].Text += text
		return
	}
	a := transcript.Activity{Type: "message", Role: "assistant", Speaker: "assistant", Text: text}
	if kind == "reasoning" {
		a = transcript.Activity{Type: "reasoning", Text: text}
	}
	l.open, l.openKind = l.add(a), kind
}

// notice adds a warning line.
func (l *live) notice(kind, message string) {
	l.close()
	l.add(transcript.Activity{Type: "notice", Kind: kind, Message: message})
}

// apply takes one agent event of the running turn. Streamed text is
// external: names, output chunks, and messages are cleaned here; message
// and thinking text is cleaned by the Markdown layout.
func (l *live) apply(ev engine.AgentEvent) {
	ev.ID, ev.Name, ev.Chunk = transcript.Clean(ev.ID), transcript.Clean(ev.Name), transcript.Clean(ev.Chunk)
	ev.Message, ev.Kind = transcript.Clean(ev.Message), transcript.Clean(ev.Kind)
	switch ev.Type {
	case "reasoning_delta":
		l.extend("reasoning", ev.Text)
	case "reasoning_completed":
		if l.openKind == "reasoning" {
			l.close()
		}
	case "assistant_delta":
		l.extend("assistant", ev.Text)
	case "assistant_completed":
		// The completed text is the whole message: it replaces what the
		// deltas built, so a reply is never shown short.
		if l.openKind == "assistant" {
			l.items[l.open].Text = ev.Text
		} else if ev.Text != "" {
			l.extend("assistant", ev.Text)
		}
		l.close()
	case "tool_requested", "tool_input_completed":
		l.close()
		if i, ok := l.running[ev.ID]; ok {
			l.items[i].Input = ev.Input
			l.items[i].View = liveView("pending", ev.Input)
			return
		}
		l.running[ev.ID] = l.add(transcript.Activity{Type: "tool_running", ID: ev.ID, Name: ev.Name, Input: ev.Input, View: liveView("pending", ev.Input)})
	case "tool_started":
		if _, ok := l.running[ev.ID]; !ok {
			l.close()
			l.running[ev.ID] = l.add(transcript.Activity{Type: "tool_running", ID: ev.ID, Name: ev.Name})
		}
	case "tool_output_delta":
		if i, ok := l.running[ev.ID]; ok {
			tail := l.items[i].OutputTail + ev.Chunk
			if len(tail) > maxOutputTail {
				tail = tail[len(tail)-maxOutputTail:]
			}
			l.items[i].OutputTail = tail
		}
	case "tool_completed":
		i, ok := l.running[ev.ID]
		if !ok {
			l.close()
			i = l.add(transcript.Activity{Type: "tool_running", ID: ev.ID, Name: "tool"})
		}
		delete(l.running, ev.ID)
		outcome := "failed"
		var note string
		if ev.Result != nil {
			switch {
			case ev.Result.OK:
				outcome = "ok"
			case ev.Result.Error != nil && ev.Result.Error.Code == "canceled":
				outcome = "canceled"
			}
			if ev.Result.Error != nil {
				note = ev.Result.Error.Message
			}
		}
		run := l.items[i]
		view := liveView(outcome, run.Input)
		if note != "" {
			view.Notes = []string{transcript.OneLine(note)}
		}
		if run.OutputTail != "" {
			view.Body = transcript.ToolBody{Kind: "text", Text: run.OutputTail}
		}
		okv := outcome == "ok"
		l.items[i] = transcript.Activity{Type: "tool_use", Sequence: run.Sequence, ID: ev.ID, Name: run.Name, Input: run.Input, View: view}
		l.add(transcript.Activity{Type: "tool_result", ToolUseID: ev.ID, OK: &okv, View: view, DurationMS: ev.DurationMS})
	case "user_message_injected":
		l.close()
		l.add(transcript.Activity{Type: "message", Role: "user", Speaker: "you (steer)", Text: ev.Text})
	case "error":
		l.notice("error", ev.Message)
	case "notice":
		l.notice(ev.Kind, ev.Message)
	case "usage_update":
		u := ev
		l.usage = &u
	case "turn_finished":
		l.close()
		if ev.Status == nil {
			return
		}
		switch ev.Status.Status {
		case "interrupted":
			l.add(transcript.Activity{Type: "interrupted", Role: "assistant"})
		case "failed":
			l.failed = true
			l.notice("failed", strings.TrimSpace(ev.Status.Kind+": "+ev.Status.Message))
		}
	}
}

// liveView is the view of a call while its round is not saved: the engine
// derives the real one (zdx_types::tool_view) when the thread view has it.
// Until then the head is the call's first short string argument.
func liveView(outcome string, input json.RawMessage) *transcript.ToolView {
	v := &transcript.ToolView{Outcome: outcome, Body: transcript.ToolBody{Kind: "empty"}}
	if s := inputSummary(input); s != "" {
		v.Head = &transcript.ToolHead{Kind: "text", Text: s}
	}
	return v
}

// summaryKeys are the arguments a live tool row shows, best first.
var summaryKeys = []string{"command", "cmd", "file_path", "path", "pattern", "query", "url", "prompt", "goal"}

func inputSummary(input json.RawMessage) string {
	var args map[string]any
	if len(input) == 0 || json.Unmarshal(input, &args) != nil {
		return ""
	}
	for _, k := range summaryKeys {
		if s, ok := args[k].(string); ok && s != "" {
			return transcript.OneLine(s)
		}
	}
	return ""
}

// thread is the turn's items as a thread for transcript.Render.
func (l *live) thread() *transcript.Thread {
	return &transcript.Thread{Activity: l.items}
}
