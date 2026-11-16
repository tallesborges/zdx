package gateway

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/commands"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// fakeCLI answers zdx service commands from a script and records them.
type fakeCLI struct {
	mu      sync.Mutex
	calls   []string
	answers map[string][]string // joined args -> answers in order; "ERR:" prefix fails
}

func (f *fakeCLI) run(_ context.Context, args ...string) ([]byte, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	key := strings.Join(args, " ")
	f.calls = append(f.calls, key)
	q := f.answers[key]
	if len(q) == 0 {
		return nil, fmt.Errorf("unexpected command %q", key)
	}
	a := q[0]
	if len(q) > 1 {
		f.answers[key] = q[1:]
	}
	if msg, ok := strings.CutPrefix(a, "ERR:"); ok {
		return nil, errors.New(msg)
	}
	return []byte(a), nil
}

func (f *fakeCLI) count(key string) int {
	f.mu.Lock()
	defer f.mu.Unlock()
	n := 0
	for _, c := range f.calls {
		if c == key {
			n++
		}
	}
	return n
}

func TestLifecycleMapsTheServiceCLI(t *testing.T) {
	cli := &fakeCLI{answers: map[string][]string{
		"service restart all --queue --thread telegram--100-topic-5 --json": {`{"already_pending":true,"active_runs":2,"request":{"services":["bot"]}}`, "ERR:the bot service is not running under a supervisor"},
		"service restart-peers --json":                                      {`{"outcome":"blocked","text":"restart blocked: 1 active agent run"}`, "ERR:scheduler is not installed", `{"outcome":"weird"}`},
		"service restart-peers --force --json":                              {`{"outcome":"restarted","text":"Restarted scheduler (PID 1 → 2)"}`},
	}}
	exits := 0
	l := &Lifecycle{Home: t.TempDir(), Run: cli.run, Exit: func() { exits++ }}
	ctx := context.Background()
	q, err := l.QueueRestart(ctx, "telegram--100-topic-5")
	if err != nil || !q.AlreadyPending || q.ActiveRuns != 2 {
		t.Fatalf("queue: %+v %v", q, err)
	}
	if _, err := l.QueueRestart(ctx, "telegram--100-topic-5"); err == nil || !strings.Contains(err.Error(), "supervisor") {
		t.Fatalf("a refused queue is an error with Rust's text: %v", err)
	}
	if r, err := l.RestartServices(ctx, false); err != nil || r.Outcome != commands.RestartBlocked || !strings.Contains(r.Text, "1 active") {
		t.Fatalf("blocked: %+v %v", r, err)
	}
	if _, err := l.RestartServices(ctx, false); err == nil || !strings.Contains(err.Error(), "not installed") {
		t.Fatalf("failed: %v", err)
	}
	if _, err := l.RestartServices(ctx, false); err == nil {
		t.Fatal("an unknown outcome is an error")
	}
	if r, err := l.RestartServices(ctx, true); err != nil || r.Outcome != commands.RestartDone {
		t.Fatalf("restarted: %+v %v", r, err)
	}
	if exits != 0 {
		t.Fatal("only RequestExit exits")
	}
	l.RequestExit()
	if exits != 1 {
		t.Fatal("RequestExit exits")
	}
}

// The poller runs Rust's check only while idle and a request exists,
// announces only into allowlisted Telegram threads, and exits once Rust
// says the bot must restart.
func TestQueuedRestartsPollAnnounceAndExit(t *testing.T) {
	home := t.TempDir()
	var mu sync.Mutex
	var sent []map[string]string
	tg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		mu.Lock()
		sent = append(sent, map[string]string{"body": string(raw)})
		mu.Unlock()
		io.WriteString(w, `{"ok":true,"result":{"message_id":1,"chat":{"id":-100,"type":"supergroup"},"date":1}}`)
	}))
	defer tg.Close()
	key := "service run-queued-restart --json"
	cli := &fakeCLI{answers: map[string][]string{key: {
		`{"status":"waiting","restart_bot":false,"restarted":[],"thread_id":null,"message":null}`,
		`{"status":"failed","restart_bot":false,"restarted":[],"thread_id":"telegram--999-topic-1","message":"not <allowed>"}`,
		`{"status":"done","restart_bot":true,"restarted":["scheduler"],"thread_id":"telegram--100-topic-5","message":"Restarted scheduler <1>"}`,
	}}}
	exited := make(chan struct{})
	l := &Lifecycle{Home: home, Run: cli.run, Exit: func() { close(exited) }}
	var idle sync.Mutex
	busy := true
	q := &QueuedRestarts{
		Lifecycle: l, Telegram: telegram.NewClient(token, telegram.Options{BaseURL: tg.URL, Logger: quiet}),
		Allow: bot.NewAllowlist([]int64{7}, []int64{-100}), Log: quiet, Every: 5 * time.Millisecond,
		Idle: func() bool { idle.Lock(); defer idle.Unlock(); return !busy },
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan struct{})
	go func() { q.Run(ctx); close(done) }()

	os.MkdirAll(filepath.Join(home, "run"), 0o777)
	os.WriteFile(filepath.Join(home, "run", "restart-queue.json"), []byte("{}"), 0o600)
	time.Sleep(50 * time.Millisecond)
	if cli.count(key) != 0 {
		t.Fatal("no check while the gateway is busy")
	}
	idle.Lock()
	busy = false
	idle.Unlock()
	select {
	case <-exited:
	case <-time.After(5 * time.Second):
		t.Fatalf("no exit; calls %v", cli.calls)
	}
	<-done
	if cli.count(key) != 3 {
		t.Fatalf("three checks: %v", cli.calls)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(sent) != 1 || !strings.Contains(sent[0]["body"], `"chat_id":-100`) || !strings.Contains(sent[0]["body"], `"message_thread_id":5`) || !strings.Contains(sent[0]["body"], "Restarted scheduler \\u0026lt;1\\u0026gt;") {
		t.Fatalf("only the allowlisted announcement goes out, escaped: %v", sent)
	}
}

// The real CLI in a temp home: no request means status none; queueing is
// refused while no live supervised bot holds run/bot.pid. Neither reaches
// launchctl.
func TestLifecycleWithRealZdx(t *testing.T) {
	bin := sandbox.RustBinary(t)
	home := t.TempDir()
	env := []string{"PATH=/usr/bin:/bin", "HOME=" + home, "ZDX_HOME=" + home}
	l := &Lifecycle{Home: home, Run: ExecRunner(bin, env), Exit: func() {}}
	q := &QueuedRestarts{Lifecycle: l, Log: quiet}
	if q.tick(context.Background()) {
		t.Fatal("nothing queued: no exit")
	}
	if _, err := l.QueueRestart(context.Background(), "telegram--100-topic-5"); err == nil || !strings.Contains(err.Error(), "supervisor") {
		t.Fatalf("queueing without a supervised bot is refused: %v", err)
	}
	// With this process as the live supervised bot the request is written
	// in the temp home (and never performed here).
	pid, err := AcquirePidFile(home)
	if err != nil {
		t.Fatal(err)
	}
	defer pid.Release()
	os.WriteFile(filepath.Join(home, "run", "bot.supervised"), []byte(strconv.Itoa(os.Getpid())), 0o666)
	r, err := l.QueueRestart(context.Background(), "telegram--100-topic-5")
	if err != nil || r.AlreadyPending {
		t.Fatalf("queue: %+v %v", r, err)
	}
	b, err := os.ReadFile(filepath.Join(home, "run", "restart-queue.json"))
	if err != nil || !strings.Contains(string(b), "telegram--100-topic-5") {
		t.Fatalf("the request names the thread: %s %v", b, err)
	}
	if r, err := l.QueueRestart(context.Background(), "telegram--100-topic-5"); err != nil || !r.AlreadyPending {
		t.Fatalf("a second queue merges: %+v %v", r, err)
	}
}

// Work admitted before the gate closed keeps the gateway busy: the check
// is skipped without asking Rust, and admission opens again.
func TestQueuedRestartSkipsWhileBusy(t *testing.T) {
	cli := &fakeCLI{answers: map[string][]string{}}
	gate := &bot.Admission{}
	q := &QueuedRestarts{Lifecycle: &Lifecycle{Home: t.TempDir(), Run: cli.run}, Gate: gate, Idle: func() bool { return false }, Log: quiet}
	if q.check(context.Background()) || len(cli.calls) != 0 {
		t.Fatalf("a busy gateway asks Rust nothing: %v", cli.calls)
	}
	release, ok := gate.Admit(context.Background())
	if !ok {
		t.Fatal("admission opens again after a skipped check")
	}
	release()
}

// A Rust check that hangs is cut at Timeout; admission opens again, and the
// same request is not checked again (its outcome is unknown: services may
// have restarted). A new request is checked.
func TestQueuedRestartCheckIsBoundedAndNotRepeated(t *testing.T) {
	home := t.TempDir()
	req := filepath.Join(home, "run", "restart-queue.json")
	os.MkdirAll(filepath.Dir(req), 0o777)
	os.WriteFile(req, []byte(`{"a":1}`), 0o600)
	var mu sync.Mutex
	calls := 0
	hang := true
	run := func(ctx context.Context, args ...string) ([]byte, error) {
		mu.Lock()
		calls++
		h := hang
		mu.Unlock()
		if h {
			<-ctx.Done()
			return nil, ctx.Err()
		}
		return []byte(`{"status":"waiting","restart_bot":false,"restarted":[],"thread_id":null,"message":null}`), nil
	}
	gate := &bot.Admission{}
	exits := 0
	q := &QueuedRestarts{Lifecycle: &Lifecycle{Home: home, Run: run, Exit: func() { exits++ }}, Gate: gate, Idle: func() bool { return true },
		Log: quiet, Every: 5 * time.Millisecond, Timeout: 50 * time.Millisecond}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() { q.Run(ctx); close(done) }()
	time.Sleep(300 * time.Millisecond)
	mu.Lock()
	n := calls
	hang = false
	mu.Unlock()
	if n != 1 {
		t.Fatalf("one bounded check for the request, got %d", n)
	}
	actx, acancel := context.WithTimeout(context.Background(), time.Second)
	release, ok := gate.Admit(actx)
	acancel()
	if !ok {
		t.Fatal("admission must open again after the timeout")
	}
	release()
	os.WriteFile(req, []byte(`{"a":22}`), 0o600)
	deadline := time.Now().Add(2 * time.Second)
	for time.Now().Before(deadline) {
		mu.Lock()
		n = calls
		mu.Unlock()
		if n > 1 {
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	cancel()
	<-done
	if n < 2 || exits != 0 {
		t.Fatalf("a new request is checked again: calls=%d exits=%d", n, exits)
	}
}
