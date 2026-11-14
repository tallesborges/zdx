package wire_test

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/fakesvc"
	"github.com/tallesborges/zdx/apps/monitor/internal/sandbox"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func home(t *testing.T) string {
	t.Helper()
	dir, err := os.MkdirTemp("", "h")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

func TestViewChecksCapabilityAndSendsTheOpLine(t *testing.T) {
	h := home(t)
	svc := fakesvc.Start(t, h, wire.CapMonitorViews, wire.CapThreadViews)
	svc.HandleBody(wire.ViewUsageReport, map[string]any{"days": 30})
	c := wire.NewClient(svc.Socket)
	days := uint32(30)
	out, err := c.View(context.Background(), wire.View{Op: wire.ViewUsageReport, Days: &days})
	if err != nil {
		t.Fatal(err)
	}
	if string(out.Body) != `{"days":30}` {
		t.Fatalf("body %s", out.Body)
	}
	reqs := svc.Requests()
	if len(reqs) != 2 || reqs[0]["op"] != "status" || reqs[1]["op"] != "usage_report" || reqs[1]["days"] != float64(30) {
		t.Fatalf("requests %v", reqs)
	}
	if _, ok := reqs[1]["thread_id"]; ok {
		t.Fatalf("unset fields must be omitted: %v", reqs[1])
	}
}

func TestViewWithoutCapabilitySendsNothing(t *testing.T) {
	h := home(t)
	svc := fakesvc.Start(t, h, wire.CapThreadViews)
	c := wire.NewClient(svc.Socket)
	_, err := c.View(context.Background(), wire.View{Op: wire.ViewMonitorSnapshot})
	var unsupported *wire.ErrUnsupported
	if !errors.As(err, &unsupported) || unsupported.Capability != wire.CapMonitorViews {
		t.Fatalf("want ErrUnsupported, got %v", err)
	}
	if ops := svc.Ops(); len(ops) != 1 || ops[0] != "status" {
		t.Fatalf("only status may be sent, got %v", ops)
	}
}

func TestErrorReplyIsAServiceError(t *testing.T) {
	h := home(t)
	svc := fakesvc.Start(t, h, wire.CapThreadViews)
	svc.Handle(wire.ViewThreadView, func(map[string]any) any {
		return map[string]any{"reply": "error", "message": "Thread not found", "code": "not_found"}
	})
	id := "t-1"
	_, err := wire.NewClient(svc.Socket).View(context.Background(), wire.View{Op: wire.ViewThreadView, ThreadID: &id})
	var se *wire.ServiceError
	if !errors.As(err, &se) || se.Code != "not_found" {
		t.Fatalf("want ServiceError not_found, got %v", err)
	}
}

func TestNoListenerIsErrNotRunning(t *testing.T) {
	c := wire.NewClient(filepath.Join(home(t), "run", "orchestrator.sock"))
	_, err := c.Status(context.Background())
	if !errors.Is(err, wire.ErrNotRunning) {
		t.Fatalf("want ErrNotRunning, got %v", err)
	}
}

func TestSilentServiceTimesOutWithinTheBudget(t *testing.T) {
	h := home(t)
	svc := fakesvc.Start(t, h, wire.CapMonitorViews)
	svc.Handle(wire.ViewActiveAgents, func(map[string]any) any { return nil })
	c := &wire.Client{Socket: svc.Socket, ReplyTimeout: 200 * time.Millisecond}
	start := time.Now()
	_, err := c.View(context.Background(), wire.View{Op: wire.ViewActiveAgents})
	if err == nil || !strings.Contains(err.Error(), "did not answer") {
		t.Fatalf("want a timeout, got %v", err)
	}
	if d := time.Since(start); d > 2*time.Second {
		t.Fatalf("took %s", d)
	}
}

func TestCancelClosesTheConnection(t *testing.T) {
	h := home(t)
	svc := fakesvc.Start(t, h, wire.CapMonitorViews)
	svc.Handle(wire.ViewActiveAgents, func(map[string]any) any { return nil })
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, err := wire.NewClient(svc.Socket).View(ctx, wire.View{Op: wire.ViewActiveAgents})
		done <- err
	}()
	time.Sleep(100 * time.Millisecond)
	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("want an error after cancel")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("cancel did not stop the call")
	}
}

func TestEncodeAddsOp(t *testing.T) {
	id := "x"
	line, err := wire.View{Op: wire.ViewThreadView, ThreadID: &id}.Encode()
	if err != nil {
		t.Fatal(err)
	}
	var got map[string]any
	if err := json.Unmarshal(line, &got); err != nil {
		t.Fatal(err)
	}
	if got["op"] != "thread_view" || got["thread_id"] != "x" || len(got) != 2 {
		t.Fatalf("got %v", got)
	}
}
