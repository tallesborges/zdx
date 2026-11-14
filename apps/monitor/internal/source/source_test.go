package source_test

import (
	"context"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/fakesvc"
	"github.com/tallesborges/zdx/apps/monitor/internal/sandbox"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func tempDir(t *testing.T, prefix string) string {
	t.Helper()
	dir, err := os.MkdirTemp("", prefix)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(dir) })
	return dir
}

// fakeZdx writes a script that records its argv, stdin, and ZDX_HOME, then
// prints out (or fails with code and stderr).
func fakeZdx(t *testing.T, out string, code int) (bin, record string) {
	t.Helper()
	dir := tempDir(t, "z")
	record = filepath.Join(dir, "record")
	script := fmt.Sprintf(`#!/bin/sh
{ echo "args: $*"; echo "home: $ZDX_HOME"; echo "stdin: $(cat)"; } > %q
cat <<'EOF'
%s
EOF
if [ %d -ne 0 ]; then echo "Error: something failed" >&2; exit %d; fi
`, record, out, code, code)
	bin = filepath.Join(dir, "zdx")
	if err := os.WriteFile(bin, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	return bin, record
}

func readRecord(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func TestServicesRunsTheReadOnlyStatusJSON(t *testing.T) {
	home := tempDir(t, "h")
	bin, record := fakeZdx(t, `{"services":[{"service":"bot","label":"dev.zdx.bot","installed":true,"status":"running","pid":42,"uptime_secs":90}],"queued_restart":null}`, 0)
	s := source.New(source.Config{Root: home, Home: home, Zdx: bin})
	got, err := s.Services(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(got.Services) != 1 || got.Services[0].Service != "bot" || *got.Services[0].PID != 42 {
		t.Fatalf("got %+v", got)
	}
	rec := readRecord(t, record)
	if !strings.Contains(rec, "args: service status --json") || !strings.Contains(rec, "home: "+home) {
		t.Fatalf("record %q", rec)
	}
}

func TestJobsDecodesTheDaemonView(t *testing.T) {
	home := tempDir(t, "h")
	bin, record := fakeZdx(t, `{"daemon":{"status":"unavailable","reason":"daemon is not running"},"jobs":[{"name":"daily-report","path":"/j/daily-report.md","schedule":{"kind":"recurring","cron":"0 8 * * *"},"grace_secs":600,"status":"scheduled","next_run_at":"2026-11-14T11:00:00Z","last_run":null,"declared_root":null,"declared_model":"m","subagent":null,"timeout_secs":null,"max_retries":0,"settings":null,"error":"daemon settings unavailable"}],"error":null}`, 0)
	got, err := source.New(source.Config{Root: home, Home: home, Zdx: bin}).Jobs(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if got.Daemon.Status != "unavailable" || len(got.Jobs) != 1 || *got.Jobs[0].Schedule.Cron != "0 8 * * *" {
		t.Fatalf("got %+v", got)
	}
	if !strings.Contains(readRecord(t, record), "args: jobs list --daemon --json") {
		t.Fatal("wrong command")
	}
}

func TestCLIFailureCarriesItsMessage(t *testing.T) {
	home := tempDir(t, "h")
	bin, _ := fakeZdx(t, ``, 3)
	_, err := source.New(source.Config{Root: home, Home: home, Zdx: bin}).Services(context.Background())
	if err == nil || !strings.Contains(err.Error(), "something failed") {
		t.Fatalf("got %v", err)
	}
}

func TestMissingBinaryIsAnError(t *testing.T) {
	home := tempDir(t, "h")
	_, err := source.New(source.Config{Root: home, Home: home}).Jobs(context.Background())
	if err == nil || !strings.Contains(err.Error(), "no zdx binary") {
		t.Fatalf("got %v", err)
	}
}

func TestViewsUseTheSocket(t *testing.T) {
	home := tempDir(t, "h")
	svc := fakesvc.Start(t, home, wire.CapMonitorViews)
	svc.HandleBody(wire.ViewActiveAgents, []any{map[string]any{"pid": 7, "uptime": "3m", "run": map[string]any{"state": "tool", "label": "bash: ls"}}})
	bin, record := fakeZdx(t, `{}`, 0)
	agents, origin, err := source.New(source.Config{Root: home, Home: home, Zdx: bin, CLIFallback: true}).ActiveAgents(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if origin != source.OriginSocket || len(agents) != 1 || agents[0].Run.Label != "bash: ls" {
		t.Fatalf("got %v %+v", origin, agents)
	}
	if _, err := os.Stat(record); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("the CLI must not run while the socket answers")
	}
}

func TestWithoutFallbackAStoppedServiceIsAnError(t *testing.T) {
	home := tempDir(t, "h")
	bin, record := fakeZdx(t, `{}`, 0)
	_, _, err := source.New(source.Config{Root: home, Home: home, Zdx: bin}).Snapshot(context.Background())
	if !errors.Is(err, wire.ErrNotRunning) {
		t.Fatalf("got %v", err)
	}
	if _, err := os.Stat(record); !errors.Is(err, os.ErrNotExist) {
		t.Fatal("the CLI must not run without --cli-fallback")
	}
}

func TestFallbackRunsZdxViewsWithTheRequest(t *testing.T) {
	home, root := tempDir(t, "h"), tempDir(t, "r")
	bin, record := fakeZdx(t, `{"reply":"view","body":{"threads":[{"id":"t-1","title":"One","root_path":null,"project":null,"age":"2m","telegram_link":null,"workers":null,"is_worker":false,"run":null}]}}`, 0)
	list, origin, err := source.New(source.Config{Root: root, Home: home, Zdx: bin, CLIFallback: true}).Threads(context.Background(), "workers")
	if err != nil {
		t.Fatal(err)
	}
	if origin != source.OriginCLI || len(list.Threads) != 1 || list.Threads[0].Title != "One" {
		t.Fatalf("got %v %+v", origin, list)
	}
	rec := readRecord(t, record)
	for _, want := range []string{"args: --root " + root + " views --file -", `"op":"threads_list"`, `"kind":"workers"`, "home: " + home} {
		if !strings.Contains(rec, want) {
			t.Fatalf("record %q lacks %q", rec, want)
		}
	}
}

func TestLogTailReadsNewestRollingFileOnly(t *testing.T) {
	home := tempDir(t, "h")
	dir := source.LogDir(home)
	if err := os.MkdirAll(dir, 0o700); err != nil {
		t.Fatal(err)
	}
	write := func(name, text string) {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(text), 0o600); err != nil {
			t.Fatal(err)
		}
	}
	write("zdx.log.2026-11-13", "old\n")
	write("zdx.log.2026-11-14", "a\nb\nc\nd\n")
	write("automations-daemon.log", "other\n")
	tail, err := source.ReadLog(home, 0, 3, source.LogStamp{})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Join(tail.Files, ",") != "zdx.log.2026-11-14,zdx.log.2026-11-13" {
		t.Fatalf("files %v", tail.Files)
	}
	if strings.Join(tail.Lines, ",") != "b,c,d" || !tail.Truncated {
		t.Fatalf("lines %v truncated %v", tail.Lines, tail.Truncated)
	}
	again, err := source.ReadLog(home, 0, 3, tail.Stamp)
	if err != nil || !again.Unchanged || again.Lines != nil {
		t.Fatalf("an unchanged file must not be read again: %+v %v", again, err)
	}
	older, err := source.ReadLog(home, 1, 3, tail.Stamp)
	if err != nil || older.File != "zdx.log.2026-11-13" || strings.Join(older.Lines, ",") != "old" {
		t.Fatalf("older %+v %v", older, err)
	}
	// An append changes the stamp.
	time.Sleep(10 * time.Millisecond)
	write("zdx.log.2026-11-14", "a\nb\nc\nd\ne\n")
	grown, _ := source.ReadLog(home, 0, 3, tail.Stamp)
	if grown.Unchanged || strings.Join(grown.Lines, ",") != "c,d,e" {
		t.Fatalf("grown %+v", grown)
	}
}

func TestNoLogDirectoryIsEmptyNotAnError(t *testing.T) {
	tail, err := source.ReadLog(tempDir(t, "h"), 0, 10, source.LogStamp{})
	if err != nil || tail.File != "" || len(tail.Files) != 0 {
		t.Fatalf("got %+v %v", tail, err)
	}
}

// Cancelling a CLI call kills the binary's children too, not only the
// binary (a wrapper script that waits on a child).
func TestCancelKillsTheWholeProcessGroup(t *testing.T) {
	home := tempDir(t, "h")
	dir := tempDir(t, "z")
	pidFile := filepath.Join(dir, "child.pid")
	script := fmt.Sprintf("#!/bin/sh\nsleep 60 &\necho $! > %q\nwait\n", pidFile)
	bin := filepath.Join(dir, "zdx")
	if err := os.WriteFile(bin, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		_, err := source.New(source.Config{Root: home, Home: home, Zdx: bin}).Services(ctx)
		done <- err
	}()
	var pid int
	for deadline := time.Now().Add(5 * time.Second); pid == 0; {
		if b, err := os.ReadFile(pidFile); err == nil {
			fmt.Sscan(string(b), &pid)
		}
		if time.Now().After(deadline) {
			t.Fatal("the fake binary never started its child")
		}
		time.Sleep(10 * time.Millisecond)
	}
	cancel()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("a cancelled call must fail")
		}
	case <-time.After(5 * time.Second):
		t.Fatal("cancel did not end the call")
	}
	for deadline := time.Now().Add(2 * time.Second); syscall.Kill(pid, 0) == nil; {
		if time.Now().After(deadline) {
			syscall.Kill(pid, syscall.SIGKILL)
			t.Fatalf("child %d outlived the cancelled call", pid)
		}
		time.Sleep(10 * time.Millisecond)
	}
}
