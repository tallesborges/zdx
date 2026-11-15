package socket

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"io"
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

// fake is a synthetic orchestrator service on a temp Unix socket. handle
// gets each connection's request line and the connection.
type fake struct {
	socket string
	mu     sync.Mutex
	seen   []string
	closed chan string
}

func newFake(t *testing.T, handle func(line string, conn net.Conn)) *fake {
	t.Helper()
	dir, err := os.MkdirTemp("", "sk")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	f := &fake{socket: filepath.Join(dir, "o.sock"), closed: make(chan string, 16)}
	ln, err := net.Listen("unix", f.socket)
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
				r := bufio.NewReader(conn)
				line, err := r.ReadString('\n')
				if err != nil {
					conn.Close()
					return
				}
				line = strings.TrimSuffix(line, "\n")
				f.mu.Lock()
				f.seen = append(f.seen, line)
				f.mu.Unlock()
				handle(line, conn)
				// Report when the client closed its end.
				go func() {
					io.Copy(io.Discard, r)
					f.closed <- line
				}()
			}()
		}
	}()
	return f
}

func (f *fake) ops() []string {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []string
	for _, line := range f.seen {
		var req struct{ Op string }
		json.Unmarshal([]byte(line), &req)
		out = append(out, req.Op)
	}
	return out
}

func write(conn net.Conn, line string) { conn.Write([]byte(line + "\n")) }

const v2Status = `{"reply":"status","report":{"protocol":2,"settled":true,"threads":[],"capabilities":["thread_views","monitor_views"]}}`

func TestCallReturnsTheReplyLine(t *testing.T) {
	f := newFake(t, func(_ string, conn net.Conn) {
		write(conn, `{"reply":"ok"}`)
		conn.Close()
	})
	got, err := NewClient(f.socket).Call(context.Background(), []byte(`{"op":"x"}`), 0)
	if err != nil || string(got) != `{"reply":"ok"}` {
		t.Fatalf("%s %v", got, err)
	}
	if f.seen[0] != `{"op":"x"}` {
		t.Fatalf("sent %q", f.seen[0])
	}
}

func TestNotRunningIsNotSent(t *testing.T) {
	dir, err := os.MkdirTemp("", "n")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(dir)
	_, err = NewClient(filepath.Join(dir, "none.sock")).Call(context.Background(), []byte(`{}`), 0)
	if !errors.Is(err, ErrNotRunning) || !errors.Is(err, ErrRequestNotSent) || errors.Is(err, ErrRequestOutcomeUnknown) {
		t.Fatalf("got %v", err)
	}
}

func TestCloseWithoutReplyIsOutcomeUnknown(t *testing.T) {
	f := newFake(t, func(_ string, conn net.Conn) { conn.Close() })
	_, err := NewClient(f.socket).Call(context.Background(), []byte(`{}`), 0)
	if !errors.Is(err, ErrRequestOutcomeUnknown) || errors.Is(err, ErrNoAnswer) {
		t.Fatalf("got %v", err)
	}
}

// A service that accepts and never reads cannot hold a large request line
// past the budget: the budget closes the connection during the write.
func TestBlockedWriteEndsAtTheBudget(t *testing.T) {
	dir, err := os.MkdirTemp("", "bw")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(dir)
	socket := filepath.Join(dir, "o.sock")
	ln, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	accepted := make(chan net.Conn, 1)
	go func() {
		if conn, err := ln.Accept(); err == nil {
			accepted <- conn
		}
	}()
	c := &Client{Socket: socket, ReplyTimeout: 200 * time.Millisecond}
	line := []byte(`{"op":"big","data":"` + strings.Repeat("x", 16<<20) + `"}`)
	start := time.Now()
	_, err = c.Call(context.Background(), line, 0)
	if elapsed := time.Since(start); elapsed > 5*time.Second {
		t.Fatalf("blocked write took %v", elapsed)
	}
	if !errors.Is(err, ErrRequestOutcomeUnknown) || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("got %v", err)
	}
	select {
	case conn := <-accepted:
		conn.Close()
	case <-time.After(time.Second):
		t.Fatal("never connected")
	}
}

func TestTimeoutIsNoAnswerAndClosesTheConnection(t *testing.T) {
	f := newFake(t, func(string, net.Conn) {})
	c := &Client{Socket: f.socket, ReplyTimeout: 50 * time.Millisecond}
	_, err := c.Call(context.Background(), []byte(`{"op":"slow"}`), 0)
	if !errors.Is(err, ErrNoAnswer) || !errors.Is(err, ErrRequestOutcomeUnknown) || !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("got %v", err)
	}
	if err.Error() != ErrNoAnswer.Error() {
		t.Fatalf("message changed: %q", err)
	}
	select {
	case <-f.closed:
	case <-time.After(5 * time.Second):
		t.Fatal("the connection stayed open after the budget")
	}
}

func TestCancelIsNoAnswerWithCanceled(t *testing.T) {
	f := newFake(t, func(string, net.Conn) {})
	ctx, cancel := context.WithCancel(context.Background())
	go func() { time.Sleep(30 * time.Millisecond); cancel() }()
	_, err := NewClient(f.socket).Call(ctx, []byte(`{}`), time.Minute)
	if !errors.Is(err, ErrNoAnswer) || !errors.Is(err, context.Canceled) {
		t.Fatalf("got %v", err)
	}
	select {
	case <-f.closed:
	case <-time.After(5 * time.Second):
		t.Fatal("cancel did not close the connection")
	}
}

func TestStreamKeepsEveryLineInOrder(t *testing.T) {
	big := strings.Repeat("x", 1<<20)
	f := newFake(t, func(_ string, conn net.Conn) {
		for i := range 50 {
			write(conn, `{"reply":"event","n":`+string(rune('0'+i%10))+`}`)
		}
		write(conn, `{"reply":"event","text":"`+big+`"}`)
		conn.Close()
	})
	s, err := NewClient(f.socket).Open(context.Background(), []byte(`{"op":"subscribe"}`))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	var lines [][]byte
	for {
		line, err := s.Next()
		if errors.Is(err, io.EOF) {
			break
		}
		if err != nil {
			t.Fatal(err)
		}
		lines = append(lines, line)
	}
	if len(lines) != 51 {
		t.Fatalf("got %d lines", len(lines))
	}
	for i := range 50 {
		if want := `{"reply":"event","n":` + string(rune('0'+i%10)) + `}`; string(lines[i]) != want {
			t.Fatalf("line %d: %s", i, lines[i])
		}
	}
	if len(lines[50]) != len(big)+len(`{"reply":"event","text":""}`) {
		t.Fatal("large line cut")
	}
	if s.Close() != nil || s.Close() != nil {
		t.Fatal("Close must be idempotent")
	}
}

func TestStreamStaysOpenUntilTheServiceCloses(t *testing.T) {
	release := make(chan struct{})
	f := newFake(t, func(_ string, conn net.Conn) {
		write(conn, `{"reply":"submitted"}`)
		<-release
		write(conn, `{"reply":"event"}`)
		conn.Close()
	})
	s, err := NewClient(f.socket).Open(context.Background(), []byte(`{"op":"submit"}`))
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	if line, err := s.Next(); err != nil || string(line) != `{"reply":"submitted"}` {
		t.Fatalf("%s %v", line, err)
	}
	select {
	case <-f.closed:
		t.Fatal("the client closed the stream early")
	case <-time.After(50 * time.Millisecond):
	}
	close(release)
	if line, err := s.Next(); err != nil || string(line) != `{"reply":"event"}` {
		t.Fatalf("%s %v", line, err)
	}
	if _, err := s.Next(); !errors.Is(err, io.EOF) {
		t.Fatalf("want EOF, got %v", err)
	}
}

func TestViewChecksTheCapabilityFirst(t *testing.T) {
	f := newFake(t, func(line string, conn net.Conn) {
		switch {
		case strings.HasPrefix(line, `{"op":"status"`):
			write(conn, `{"reply":"status","report":{"protocol":2,"settled":true,"threads":[],"capabilities":["thread_views"]}}`)
		default:
			write(conn, `{"reply":"view","body":[]}`)
		}
		conn.Close()
	})
	c := NewClient(f.socket)
	_, err := c.View(context.Background(), protocol.View{Op: protocol.ViewMonitorSnapshot})
	var unsupported *protocol.ErrUnsupported
	if !errors.As(err, &unsupported) || unsupported.Capability != protocol.CapMonitorViews || unsupported.Protocol != 2 {
		t.Fatalf("got %v", err)
	}
	if ops := f.ops(); len(ops) != 1 || ops[0] != "status" {
		t.Fatalf("sent %v", ops)
	}
	out, err := c.View(context.Background(), protocol.View{Op: protocol.ViewThreadsList})
	if err != nil || string(out.Body) != `[]` {
		t.Fatalf("%s %v", out.Body, err)
	}
	for _, op := range []string{"nope", "subscription_quotas"} {
		_, err = c.View(context.Background(), protocol.View{Op: op})
		if !errors.Is(err, ErrRequestNotSent) {
			t.Fatalf("unknown view %s: %v", op, err)
		}
	}
	if ops := f.ops(); len(ops) != 3 || ops[2] != "threads_list" {
		t.Fatalf("sent %v", ops)
	}
}

func TestViewPassesARefusalThrough(t *testing.T) {
	f := newFake(t, func(line string, conn net.Conn) {
		if strings.HasPrefix(line, `{"op":"status"`) {
			write(conn, v2Status)
		} else {
			write(conn, `{"reply":"error","message":"no such thread","code":"not_found"}`)
		}
		conn.Close()
	})
	thread := "t"
	_, err := NewClient(f.socket).View(context.Background(), protocol.View{Op: protocol.ViewThreadView, ThreadID: &thread})
	var se *protocol.ServiceError
	if !errors.As(err, &se) || se.Code != "not_found" {
		t.Fatalf("got %v", err)
	}
}

func TestRequireNamesTheFirstMissingCapability(t *testing.T) {
	f := newFake(t, func(_ string, conn net.Conn) {
		write(conn, v2Status)
		conn.Close()
	})
	c := NewClient(f.socket)
	if err := c.Require(context.Background(), protocol.CapThreadViews, protocol.CapMonitorViews); err != nil {
		t.Fatal(err)
	}
	err := c.Require(context.Background(), protocol.CapThreadViews, protocol.CapThreadRoot, protocol.CapHoldTokens)
	var unsupported *protocol.ErrUnsupported
	if !errors.As(err, &unsupported) || unsupported.Capability != protocol.CapThreadRoot {
		t.Fatalf("got %v", err)
	}
}
