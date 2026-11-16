// Package fakesvc is a fake engine socket for tests: it answers
// `status` with a fixed report and each view op with a handler, and records
// every request line.
package fakesvc

import (
	"bufio"
	"encoding/json"
	"net"
	"os"
	"path/filepath"
	"sync"
	"testing"
)

// Server is a fake service listening on a Unix socket.
type Server struct {
	Socket string

	mu        sync.Mutex
	caps      []string
	protocol  *int
	viewsRoot *string
	views     map[string]func(req map[string]any) any
	requests  []map[string]any
	ln        net.Listener
}

// Start listens on <home>/run/engine.sock and closes on cleanup.
func Start(t *testing.T, home string, caps ...string) *Server {
	t.Helper()
	dir := filepath.Join(home, "run")
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	two := 2
	s := &Server{Socket: filepath.Join(dir, "engine.sock"), caps: caps, protocol: &two, views: map[string]func(map[string]any) any{}}
	ln, err := net.Listen("unix", s.Socket)
	if err != nil {
		t.Fatal(err)
	}
	s.ln = ln
	go s.serve()
	t.Cleanup(func() { ln.Close() })
	return s
}

// Protocol sets the status report's protocol (default 2); nil omits the
// field, as a protocol 1 service does.
func (s *Server) Protocol(version *int) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.protocol = version
}

// ViewsRoot sets the status report's views_root.
func (s *Server) ViewsRoot(root string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.viewsRoot = &root
}

// Handle answers view op with the reply line handler returns (marshalled
// as is).
func (s *Server) Handle(op string, handler func(req map[string]any) any) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.views[op] = handler
}

// HandleBody answers view op with `{"reply":"view","body":body}`.
func (s *Server) HandleBody(op string, body any) {
	s.Handle(op, func(map[string]any) any { return map[string]any{"reply": "view", "body": body} })
}

// Requests returns the request lines seen so far.
func (s *Server) Requests() []map[string]any {
	s.mu.Lock()
	defer s.mu.Unlock()
	return append([]map[string]any(nil), s.requests...)
}

// Ops returns the op of every request seen so far.
func (s *Server) Ops() []string {
	var ops []string
	for _, r := range s.Requests() {
		op, _ := r["op"].(string)
		ops = append(ops, op)
	}
	return ops
}

func (s *Server) serve() {
	for {
		conn, err := s.ln.Accept()
		if err != nil {
			return
		}
		go s.handle(conn)
	}
}

func (s *Server) handle(conn net.Conn) {
	defer conn.Close()
	sc := bufio.NewScanner(conn)
	if !sc.Scan() {
		return
	}
	var req map[string]any
	if err := json.Unmarshal(sc.Bytes(), &req); err != nil {
		return
	}
	s.mu.Lock()
	s.requests = append(s.requests, req)
	op, _ := req["op"].(string)
	var reply any
	if op == "status" {
		report := map[string]any{"capabilities": s.caps, "settled": true, "threads": []any{}}
		if s.protocol != nil {
			report["protocol"] = *s.protocol
		}
		if s.viewsRoot != nil {
			report["views_root"] = *s.viewsRoot
		}
		reply = map[string]any{"reply": "status", "report": report}
	} else if h := s.views[op]; h != nil {
		s.mu.Unlock()
		reply = h(req)
		s.mu.Lock()
	} else {
		reply = map[string]any{"reply": "error", "message": "unknown op " + op, "code": "invalid"}
	}
	s.mu.Unlock()
	if reply == nil {
		// A handler returning nil never answers: the connection stays open
		// until the client gives up.
		_, _ = bufio.NewReader(conn).ReadByte()
		return
	}
	line, _ := json.Marshal(reply)
	_, _ = conn.Write(append(line, '\n'))
}
