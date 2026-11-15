package engine

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

// fakeService answers one request per connection with handler's lines.
type fakeService struct {
	path string
	mu   sync.Mutex
	seen []map[string]any
}

func serve(t *testing.T, caps []string, handler func(req map[string]any) []string) *fakeService {
	t.Helper()
	dir, err := os.MkdirTemp("", "tui")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	f := &fakeService{path: filepath.Join(dir, "s.sock")}
	ln, err := net.Listen("unix", f.path)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer conn.Close()
				line, err := bufio.NewReader(conn).ReadBytes('\n')
				if err != nil {
					return
				}
				var req map[string]any
				_ = json.Unmarshal(line, &req)
				f.mu.Lock()
				f.seen = append(f.seen, req)
				f.mu.Unlock()
				var out []string
				if req["op"] == "status" {
					c, _ := json.Marshal(caps)
					out = []string{fmt.Sprintf(`{"reply":"status","report":{"protocol":2,"capabilities":%s,"settled":true,"threads":[]}}`, c)}
				} else {
					out = handler(req)
				}
				for _, l := range out {
					if _, err := conn.Write([]byte(l + "\n")); err != nil {
						return
					}
				}
			}()
		}
	}()
	return f
}

func (f *fakeService) requests(op string) []map[string]any {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []map[string]any
	for _, r := range f.seen {
		if r["op"] == op {
			out = append(out, r)
		}
	}
	return out
}

var allCaps = []string{"ordinary_submit", "chat_submit", "subscribe_thread", "update_thread", "catalog", "thread_views", "rerun"}

func agentLine(turn int, inner string) string {
	return fmt.Sprintf(`{"reply":"event","event":{"type":"agent","thread_id":"t1","turn_id":%d,"event":%s}}`, turn, inner)
}

// Every event of the thread subscription reaches the channel in order:
// none is merged or dropped, however many come.
func TestSubscribeStreamsEveryEventInOrder(t *testing.T) {
	const n = 2000
	f := serve(t, allCaps, func(req map[string]any) []string {
		out := []string{`{"reply":"ok"}`,
			`{"reply":"event","event":{"type":"turn_started","thread_id":"t1","turn_id":3,"kind":"user","inputs":[{"input_id":7}],"followed":false}}`}
		for i := 0; i < n; i++ {
			out = append(out, agentLine(3, fmt.Sprintf(`{"type":"assistant_delta","text":"%d,"}`, i)))
		}
		return append(out, `{"reply":"event","event":{"type":"turn_ended","thread_id":"t1","turn_id":3,"held":false}}`)
	})
	c := NewAt(f.path, "/root")
	sub, err := c.Subscribe(context.Background(), "t1")
	if err != nil {
		t.Fatal(err)
	}
	var b strings.Builder
	var types []string
	for ev := range sub.Events {
		types = append(types, ev.Type)
		if ev.Type == EventAgent {
			a, err := ev.Agent()
			if err != nil {
				t.Fatal(err)
			}
			b.WriteString(a.Text)
		}
	}
	if err := sub.Err(); err != nil {
		t.Fatal(err)
	}
	var want strings.Builder
	for i := 0; i < n; i++ {
		fmt.Fprintf(&want, "%d,", i)
	}
	if b.String() != want.String() {
		t.Fatalf("text differs: got %d bytes, want %d", b.Len(), want.Len())
	}
	if types[0] != EventTurnStarted || types[len(types)-1] != EventTurnEnded || len(types) != n+2 {
		t.Fatalf("event types %v…%v (%d)", types[:1], types[len(types)-1:], len(types))
	}
	req := f.requests("subscribe")[0]
	if req["thread_id"] != "t1" || req["renderer"] != false {
		t.Fatalf("subscribe request %v", req)
	}
}

func TestSubmitSendsAChatTurn(t *testing.T) {
	f := serve(t, allCaps, func(map[string]any) []string {
		return []string{`{"reply":"submitted","thread_id":"t1","input_id":7,"queued_ahead":1,"in_turn":false}`}
	})
	sub, err := NewAt(f.path, "/root").Submit(context.Background(), SubmitRequest{ThreadID: "t1", Text: "hi"})
	if err != nil {
		t.Fatal(err)
	}
	if sub.ThreadID != "t1" || sub.InputID != 7 || sub.QueuedAhead != 1 || sub.InTurn {
		t.Fatalf("submission %+v", sub)
	}
	req := f.requests("submit")[0]
	for k, v := range map[string]any{"ordinary": true, "chat": true, "follow": "none", "delivery": "queue", "root": "/root", "thread_id": "t1"} {
		if req[k] != v {
			t.Errorf("submit %s = %v, want %v", k, req[k], v)
		}
	}
	if imgs, ok := req["images"].([]any); !ok || len(imgs) != 0 {
		t.Errorf("images = %v, want []", req["images"])
	}
}

func TestSteerReceipt(t *testing.T) {
	f := serve(t, allCaps, func(map[string]any) []string {
		return []string{`{"reply":"submitted","thread_id":"t1","input_id":9,"queued_ahead":0,"in_turn":true}`}
	})
	sub, err := NewAt(f.path, "/r").Submit(context.Background(), SubmitRequest{ThreadID: "t1", Text: "also", Steer: true})
	if err != nil {
		t.Fatal(err)
	}
	if !sub.InTurn {
		t.Fatalf("submission %+v", sub)
	}
	if got := f.requests("submit")[0]["delivery"]; got != "steer" {
		t.Fatalf("delivery %v", got)
	}
}

// An older service without chat_submit gets no submit: it would ignore
// `chat` and run the non-interactive layer.
func TestSubmitNeedsChatCapability(t *testing.T) {
	f := serve(t, []string{"ordinary_submit"}, func(map[string]any) []string {
		t.Error("submit sent to a service without chat_submit")
		return nil
	})
	_, err := NewAt(f.path, "/r").Submit(context.Background(), SubmitRequest{ThreadID: "t1", Text: "hi"})
	var unsupported *protocol.ErrUnsupported
	if !errors.As(err, &unsupported) || unsupported.Capability != protocol.CapChatSubmit {
		t.Fatalf("err = %v", err)
	}
}

func TestSubmitRefusalIsServiceError(t *testing.T) {
	f := serve(t, allCaps, func(map[string]any) []string {
		return []string{`{"reply":"error","message":"thread 't1' is running in another process","code":"conflict"}`}
	})
	_, err := NewAt(f.path, "/r").Submit(context.Background(), SubmitRequest{ThreadID: "t1", Text: "hi"})
	var se *protocol.ServiceError
	if !errors.As(err, &se) || se.Code != "conflict" {
		t.Fatalf("err = %v", err)
	}
}

// Cancelling the context ends the subscription without an error.
func TestSubscriptionEndsWithContext(t *testing.T) {
	f := serve(t, allCaps, func(map[string]any) []string {
		return []string{`{"reply":"ok"}`}
	})
	ctx, cancel := context.WithCancel(context.Background())
	sub, err := NewAt(f.path, "/r").Subscribe(ctx, "t1")
	if err != nil {
		t.Fatal(err)
	}
	cancel()
	select {
	case _, ok := <-sub.Events:
		if ok {
			t.Fatal("unexpected event")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("subscription did not end")
	}
	if err := sub.Err(); err != nil {
		t.Fatal(err)
	}
}

func TestUpdateAndCatalogShapes(t *testing.T) {
	f := serve(t, allCaps, func(req map[string]any) []string {
		switch req["op"] {
		case "update_thread":
			return []string{`{"reply":"thread_updated","thread_id":"t1","model_label":"Model X"}`}
		case "catalog":
			return []string{`{"reply":"catalog","body":{"models":[{"id":"p:m","name":"p:m","specs":{"High":"p:m@high"}}],"modes":[],"label":"M","model":"p:m@high","thinking":"High"}}`}
		}
		return []string{`{"reply":"error","message":"unexpected"}`}
	})
	c := NewAt(f.path, "/root")
	spec := "p:m@high"
	up, err := c.UpdateThread(context.Background(), Update{ThreadID: "t1", Model: ptr(&spec)})
	if err != nil || up.ModelLabel != "Model X" {
		t.Fatalf("update %+v %v", up, err)
	}
	req := f.requests("update_thread")[0]
	if req["model"] != spec || req["config_root"] != "/root" {
		t.Fatalf("update request %v", req)
	}
	p, err := c.Picker(context.Background(), "t1")
	if err != nil || len(p.Models) != 1 || p.Models[0].Specs["High"] != spec {
		t.Fatalf("picker %+v %v", p, err)
	}
	q := f.requests("catalog")[0]
	if q["kind"] != "picker" || q["scope"] != "topic" || q["thread_id"] != "t1" || q["root"] != "/root" {
		t.Fatalf("catalog request %v", q)
	}
}

func ptr(p *string) **string { return &p }
