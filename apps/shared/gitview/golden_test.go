package gitview

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/shared/protocol"
	"github.com/tallesborges/zdx/apps/shared/sandbox"
	"github.com/tallesborges/zdx/apps/shared/socket"
)

// goldenFile holds the replies the Rust Git views (view_bodies/git.rs)
// gave for viewCases on the synthetic world. They are the contract: they
// are written only from a Rust service (recordEnv), never from Go.
const goldenFile = "testdata/rust_views.json"

// rustEnv names a prebuilt zdx (absolute path) whose Git views the
// differential test compares with this package, live, on one world.
// recordEnv=1 also rewrites goldenFile from that binary's replies.
const (
	rustEnv   = sandbox.KeepPrefix + "GITVIEW_RUST"
	recordEnv = sandbox.KeepPrefix + "GITVIEW_RECORD"
)

func TestMain(m *testing.M) {
	os.Exit(sandbox.Main(func() int {
		// The views run git with this process's environment: keep user and
		// system config out, as the fixtures do.
		os.Setenv("GIT_CONFIG_GLOBAL", "/dev/null")
		os.Setenv("GIT_CONFIG_NOSYSTEM", "1")
		return m.Run()
	}))
}

type golden struct {
	Recorder string       `json:"recorder"`
	Git      string       `json:"git"`
	Cases    []goldenCase `json:"cases"`
}

type goldenCase struct {
	Name     string          `json:"name"`
	Fallback string          `json:"fallback"`
	View     string          `json:"view"`
	Reply    json.RawMessage `json:"reply"`
}

// viewRequest is the part of a view line the Go views take.
type viewRequest struct {
	Op       string  `json:"op"`
	ThreadID string  `json:"thread_id"`
	Scope    string  `json:"scope"`
	Path     string  `json:"path"`
	Kind     string  `json:"kind"`
	Commit   *string `json:"commit"`
}

// locateFor is what the Rust thread_root view answers for these threads:
// the id itself (no alias), its stored root, and the views root.
func (w *world) locateFor(id, fallback string) Locate {
	return func(context.Context) (Selection, error) {
		sel := Selection{ThreadID: &id, EffectiveID: &id, FallbackRoot: w.path(fallback)}
		if root, ok := threads[id]; ok {
			p := w.path(root)
			sel.RootPath = &p
		}
		return sel, nil
	}
}

// runGo answers one case with this package, in the recorded reply form.
func (w *world) runGo(t *testing.T, c viewCase) []byte {
	t.Helper()
	var req viewRequest
	if err := json.Unmarshal([]byte(w.expand(c.view)), &req); err != nil {
		t.Fatalf("%s: %v", c.name, err)
	}
	locate := w.locateFor(req.ThreadID, c.fallback)
	ctx := context.Background()
	var body any
	var err error
	switch req.Op {
	case "git_status":
		body, err = Status(ctx, locate)
	case "git_scope":
		body, err = Scope(ctx, locate, req.Scope)
	case "git_diff":
		body, err = Diff(ctx, locate, DiffRequest{Path: req.Path, Kind: req.Kind, Commit: req.Commit})
	default:
		t.Fatalf("%s: op %q", c.name, req.Op)
	}
	if err != nil {
		var ve *Error
		if !errors.As(err, &ve) {
			t.Fatalf("%s: untyped error %v", c.name, err)
		}
		if ve.Code == CodeInternal {
			t.Logf("%s: internal cause: %v", c.name, ve.Err)
		}
		return refusal(ve.Code, ve.Message)
	}
	encoded, err := Encode(body)
	if err != nil {
		t.Fatal(err)
	}
	return []byte(`{"body":` + string(encoded) + `}`)
}

func refusal(code, message string) []byte {
	b, _ := json.Marshal(map[string]string{"code": code, "message": message})
	return b
}

// normalize makes a reply comparable across runs: the world directory
// becomes $WORLD and relative commit times $RELATIVE. Keys are sorted.
func (w *world) normalize(t *testing.T, raw []byte) []byte {
	t.Helper()
	var value any
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	if err := dec.Decode(&value); err != nil {
		t.Fatalf("not JSON: %v: %.300s", err, raw)
	}
	var clean func(key string, v any) any
	clean = func(key string, v any) any {
		switch v := v.(type) {
		case map[string]any:
			for k, inner := range v {
				v[k] = clean(k, inner)
			}
		case []any:
			for i := range v {
				v[i] = clean("", v[i])
			}
		case string:
			if key == "relative_time" {
				return "$RELATIVE"
			}
			v = strings.ReplaceAll(v, w.dir, "$WORLD")
			if key == "content" && len(v) > 4096 {
				// A long diff is kept as its digest, so the file stays small
				// and the comparison exact.
				sum := sha256.Sum256([]byte(v))
				return fmt.Sprintf("$SHA256 %x, %d bytes, ends %q", sum, len(v), v[len(v)-48:])
			}
			return v
		}
		return v
	}
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(clean("", value)); err != nil {
		t.Fatal(err)
	}
	return bytes.TrimSpace(buf.Bytes())
}

func readGolden(t *testing.T) golden {
	t.Helper()
	data, err := os.ReadFile(goldenFile)
	if err != nil {
		t.Fatal(err)
	}
	var g golden
	if err := json.Unmarshal(data, &g); err != nil {
		t.Fatal(err)
	}
	return g
}

// TestGoldenRustViews runs every case with this package and compares it
// with the reply Rust recorded. Every case must be recorded, and every
// recorded case must still be a case.
func TestGoldenRustViews(t *testing.T) {
	g := readGolden(t)
	recorded := map[string]goldenCase{}
	for _, c := range g.Cases {
		recorded[c.Name] = c
	}
	if len(recorded) != len(viewCases) || len(g.Cases) != len(viewCases) {
		t.Fatalf("%s has %d cases (%d names), the test %d: re-record with %s", goldenFile, len(g.Cases), len(recorded), len(viewCases), recordEnv)
	}
	w := newWorld(t)
	ran := 0
	for _, c := range viewCases {
		t.Run(c.name, func(t *testing.T) {
			rec, ok := recorded[c.name]
			if !ok || rec.View != c.view || rec.Fallback != c.fallback {
				t.Fatalf("case %s is not recorded as it is defined (recorded %+v): re-record with %s", c.name, rec, recordEnv)
			}
			want := w.normalize(t, rec.Reply)
			got := w.normalize(t, w.runGo(t, c))
			if !bytes.Equal(got, want) {
				t.Fatalf("Go differs from the recorded Rust reply\n  go: %.2000s\nrust: %.2000s", got, want)
			}
			ran++
		})
	}
	if ran != len(viewCases) {
		t.Fatalf("ran %d of %d cases", ran, len(viewCases))
	}
}

// rustService is an isolated `zdx orchestrator serve` on a temp home with
// the cases' thread files, whose views root is one world directory.
type rustService struct {
	client *socket.Client
	stop   func()
}

func (w *world) startRust(t *testing.T, bin, fallback string) *rustService {
	t.Helper()
	// Short: macOS limits Unix socket paths to 104 bytes.
	home, err := os.MkdirTemp("", "g")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(home) })
	if n := len(protocol.SocketPath(home)); n > 100 {
		t.Fatalf("socket path is %d bytes: %s", n, protocol.SocketPath(home))
	}
	if err := os.MkdirAll(filepath.Join(home, "threads"), 0o700); err != nil {
		t.Fatal(err)
	}
	for id, root := range threads {
		meta, _ := json.Marshal(map[string]any{"type": "meta", "schema_version": 1, "root_path": w.path(root), "ts": "2026-01-02T00:00:00Z"})
		if err := os.WriteFile(filepath.Join(home, "threads", id+".jsonl"), append(meta, '\n'), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	env := []string{"HOME=" + home, "ZDX_HOME=" + home, "GIT_CONFIG_GLOBAL=/dev/null", "GIT_CONFIG_NOSYSTEM=1"}
	for _, kv := range os.Environ() {
		key, _, _ := strings.Cut(kv, "=")
		if key == "PATH" || key == "TMPDIR" || key == "LANG" || strings.HasPrefix(key, "LC_") {
			env = append(env, kv)
		}
	}
	logPath := filepath.Join(home, "service.log")
	logFile, err := os.Create(logPath)
	if err != nil {
		t.Fatal(err)
	}
	cmd := exec.Command(bin, "--root", w.path(fallback), "orchestrator", "serve")
	cmd.Dir, cmd.Env, cmd.Stdout, cmd.Stderr = w.path(fallback), env, logFile, logFile
	if err := cmd.Start(); err != nil {
		t.Fatal(err)
	}
	exited := make(chan struct{})
	go func() { _ = cmd.Wait(); close(exited) }()
	stop := func() {
		_ = cmd.Process.Signal(syscall.SIGTERM)
		select {
		case <-exited:
		case <-time.After(5 * time.Second):
			_ = cmd.Process.Kill()
			<-exited
		}
		logFile.Close()
	}
	t.Cleanup(stop)
	client := socket.NewClient(protocol.SocketPath(home))
	for deadline := time.Now().Add(30 * time.Second); ; {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		report, err := client.Status(ctx, nil)
		cancel()
		if err == nil {
			if !report.Has(protocol.CapGitViews) {
				t.Fatalf("%s lacks %s: %v", bin, protocol.CapGitViews, report.Capabilities)
			}
			break
		}
		select {
		case <-exited:
			log, _ := os.ReadFile(logPath)
			t.Fatalf("service exited: %s", log)
		case <-time.After(20 * time.Millisecond):
		}
		if time.Now().After(deadline) {
			log, _ := os.ReadFile(logPath)
			t.Fatalf("service never answered: %v\n%s", err, log)
		}
	}
	return &rustService{client: client, stop: stop}
}

// ask sends one view line and returns the reply in the recorded form.
func (s *rustService) ask(t *testing.T, line string) []byte {
	t.Helper()
	reply, err := s.client.Call(context.Background(), []byte(line), 0)
	if err != nil {
		t.Fatalf("%s: %v", line, err)
	}
	var r protocol.Reply
	if err := json.Unmarshal(reply, &r); err != nil {
		t.Fatalf("%s: %v: %s", line, err, reply)
	}
	if r.Kind == protocol.ReplyError {
		return refusal(r.Code, r.Message)
	}
	if r.Kind != protocol.ReplyView {
		t.Fatalf("%s: reply %s", line, reply)
	}
	return []byte(`{"body":` + string(r.Body) + `}`)
}

// TestRustDifferential compares this package with the Rust Git views of
// the binary named by rustEnv, live on one world. With recordEnv=1 it
// also rewrites goldenFile from the Rust replies.
func TestRustDifferential(t *testing.T) {
	bin := os.Getenv(rustEnv)
	if bin == "" {
		t.Skipf("%s not set: the live Rust comparison is opt-in (the golden test covers the recorded contract)", rustEnv)
	}
	if !filepath.IsAbs(bin) {
		t.Fatalf("%s must be an absolute path, got %q", rustEnv, bin)
	}
	w := newWorld(t)
	services := map[string]*rustService{}
	var cases []goldenCase
	for _, c := range viewCases {
		svc := services[c.fallback]
		if svc == nil {
			svc = w.startRust(t, bin, c.fallback)
			services[c.fallback] = svc
		}
		rust := svc.ask(t, w.expand(c.view))
		cases = append(cases, goldenCase{Name: c.name, Fallback: c.fallback, View: c.view, Reply: w.normalize(t, rust)})
		t.Run(c.name, func(t *testing.T) {
			if got, want := w.normalize(t, w.runGo(t, c)), w.normalize(t, rust); !bytes.Equal(got, want) {
				t.Errorf("Go differs from Rust\n  go: %.2000s\nrust: %.2000s", got, want)
			}
		})
	}
	if os.Getenv(recordEnv) != "1" {
		return
	}
	version, err := exec.Command(bin, "--version").Output()
	if err != nil {
		t.Fatal(err)
	}
	gitVersion, err := exec.Command("git", "--version").Output()
	if err != nil {
		t.Fatal(err)
	}
	var out bytes.Buffer
	enc := json.NewEncoder(&out)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(golden{
		Recorder: strings.TrimSpace(string(version)) + " (Rust Git views, crates/zdx-engine/src/orchestration/view_bodies/git.rs), over the socket of an isolated service",
		Git:      strings.TrimSpace(string(gitVersion)),
		Cases:    cases,
	}); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(goldenFile, out.Bytes(), 0o644); err != nil {
		t.Fatal(err)
	}
	t.Logf("recorded %d cases from %s into %s", len(cases), bin, goldenFile)
}
