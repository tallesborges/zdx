package engine

import "encoding/json"

// Streaming deltas: the only agent events a buffer may coalesce.
const (
	AgentToolInputDelta  = "tool_input_delta"
	AgentToolOutputDelta = "tool_output_delta"
)

// coalescible lists the agent events whose content the gateway never needs
// one by one: text, reasoning and tool-stream deltas. The final text comes
// from turn_finished, and the status line only needs the newest one. Every
// other event is kept, unknown future types included, so tool calls and
// results (worker links, the current tool) and anything Rust adds stay
// available until a policy for them exists.
var coalescible = map[string]bool{
	AgentReasoningDelta:  true,
	AgentAssistantDelta:  true,
	AgentToolInputDelta:  true,
	AgentToolOutputDelta: true,
}

// Coalescible reports whether ev is a delta a newer delta may replace.
func Coalescible(ev Event) bool {
	if ev.Type != EventAgent {
		return false
	}
	var head struct {
		Type string `json:"type"`
	}
	if json.Unmarshal(ev.Inner, &head) != nil {
		return false
	}
	return coalescible[head.Type]
}

// Buffer returns a channel pair that never blocks the writer, so a slow
// renderer cannot stall a socket reader or the renderer subscription. Every
// event is kept in order, except that a delta (Coalescible) replaces the
// delta right before it when nothing else came between. So the buffer holds
// at most two entries per kept event plus one, whatever the volume of
// streamed text or tool output; kept events are bounded by the turn's tool
// calls and lifecycle events. Closing in closes out once the buffer drains.
func Buffer() (chan<- Event, <-chan Event) {
	in := make(chan Event)
	out := make(chan Event)
	go func(src chan Event) {
		defer close(out)
		var buf []Event
		var tailLossy bool
		for src != nil || len(buf) > 0 {
			var send chan Event
			var next Event
			if len(buf) > 0 {
				send, next = out, buf[0]
			}
			select {
			case ev, ok := <-src:
				if !ok {
					src = nil
					continue
				}
				lossy := Coalescible(ev)
				if lossy && tailLossy && len(buf) > 0 {
					buf[len(buf)-1] = ev
				} else {
					buf = append(buf, ev)
				}
				tailLossy = lossy
			case send <- next:
				buf = buf[1:]
				if len(buf) == 0 {
					tailLossy = false
				}
			}
		}
	}(in)
	return in, out
}
