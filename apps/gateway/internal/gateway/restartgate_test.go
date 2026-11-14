package gateway

import (
	"context"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"strings"
	"sync/atomic"
	"testing"
	"testing/fstest"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// blockingCheck is a run-queued-restart runner that blocks until answered.
type blockingCheck struct {
	called chan struct{}
	answer chan string
}

func (b *blockingCheck) run(ctx context.Context, args ...string) ([]byte, error) {
	if strings.Join(args, " ") != "service run-queued-restart --json" {
		return nil, io.ErrUnexpectedEOF
	}
	b.called <- struct{}{}
	select {
	case a := <-b.answer:
		return []byte(a), nil
	case <-ctx.Done():
		return nil, ctx.Err()
	}
}

type gateRig struct {
	tg      *confirmingTelegram
	sink    *countingSink
	check   *blockingCheck
	addr    chan net.Addr
	done    chan error
	cancel  context.CancelFunc
	home    string
	httpURL string
}

func startGateRig(t *testing.T) *gateRig {
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	r := &gateRig{tg: newConfirmingTelegram(t), sink: &countingSink{got: map[string]int{}}, home: t.TempDir(),
		check: &blockingCheck{called: make(chan struct{}, 4), answer: make(chan string)}, addr: make(chan net.Addr, 1), done: make(chan error, 1)}
	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/r"}
	settings.Server.Enabled, settings.Server.Port = true, 1
	ctx, cancel := context.WithCancel(context.Background())
	r.cancel = cancel
	os.MkdirAll(filepath.Join(r.home, "run"), 0o777)
	os.WriteFile(filepath.Join(r.home, "run", "restart-queue.json"), []byte("{}"), 0o600)
	go func() {
		r.done <- Run(ctx, Config{
			Settings: settings, Socket: f.socket, Log: quiet, DrainTimeout: 5 * time.Second, HTTPAddr: "127.0.0.1:0",
			Assets: fstest.MapFS{"index.html": {Data: []byte("<html></html>")}}, Ready: func(a net.Addr) { r.addr <- a },
			Telegram: telegram.Options{BaseURL: r.tg.srv.URL, Logger: quiet},
			Build: func(d Deps) (Components, error) {
				l := &Lifecycle{Home: r.home, Run: r.check.run, Exit: d.RequestExit}
				q := &QueuedRestarts{Lifecycle: l, Idle: d.Idle, Gate: d.Admission, Telegram: d.Telegram, Allow: d.Allow, Log: quiet, Every: 10 * time.Millisecond}
				return Components{Messages: r.sink, Callbacks: r.sink, Renderer: r.sink, Background: q.Run}, nil
			},
		})
	}()
	r.httpURL = "http://" + (<-r.addr).String()
	t.Cleanup(cancel)
	return r
}

// postFeedback sends a Mini App write and returns its status, or 0 when it
// did not answer within wait.
func (r *gateRig) postFeedback(wait time.Duration) chan int {
	out := make(chan int, 1)
	go func() {
		resp, err := http.Post(r.httpURL+"/api/threads/telegram--100-topic-5/artifacts/feedback", "application/json", strings.NewReader("{}"))
		if err != nil {
			out <- -1
			return
		}
		body, _ := io.ReadAll(resp.Body)
		resp.Body.Close()
		if resp.StatusCode == http.StatusServiceUnavailable && !strings.Contains(string(body), "restarting") {
			out <- -2
			return
		}
		out <- resp.StatusCode
	}()
	return out
}

// While Rust checks a queued restart, admission is closed: an update and a
// Mini App write that arrive wait. Rust answers "waiting", admission opens,
// and both go on; nothing is lost or taken twice.
func TestQueuedRestartCheckHoldsNewWorkThenReopens(t *testing.T) {
	r := startGateRig(t)
	<-r.check.called
	r.tg.push(9, "during check")
	waitUntil(t, "the poll to fetch update 9", 5*time.Second, func() bool { return r.tg.times(9) >= 1 })
	post := r.postFeedback(0)
	time.Sleep(150 * time.Millisecond)
	if r.sink.count("during check") != 0 {
		t.Fatal("an update must not be handed off during the check")
	}
	select {
	case code := <-post:
		t.Fatalf("a Mini App write must wait during the check, got %d", code)
	default:
	}
	// No further checks: the request is gone once Rust answered.
	os.Remove(filepath.Join(r.home, "run", "restart-queue.json"))
	r.check.answer <- `{"status":"waiting","restart_bot":false,"restarted":[],"thread_id":null,"message":null}`
	waitUntil(t, "update 9 after the check", 5*time.Second, func() bool { return r.sink.count("during check") == 1 })
	if code := <-post; code == http.StatusServiceUnavailable || code < 0 {
		t.Fatalf("the write runs after the check, got %d", code)
	}
	r.cancel()
	if err := <-r.done; err != nil {
		t.Fatal(err)
	}
	if r.sink.count("during check") != 1 {
		t.Fatal("update 9 is handled once")
	}
}

// When Rust answers that the bot must restart, the held update is never
// handed off nor confirmed (the next process gets it), and the waiting
// Mini App write is refused with 503 instead of starting work.
func TestQueuedRestartExitLeavesHeldWorkForTheNextProcess(t *testing.T) {
	r := startGateRig(t)
	<-r.check.called
	r.tg.push(9, "during check")
	waitUntil(t, "the poll to fetch update 9", 5*time.Second, func() bool { return r.tg.times(9) >= 1 })
	post := r.postFeedback(0)
	time.Sleep(100 * time.Millisecond)
	r.check.answer <- `{"status":"done","restart_bot":true,"restarted":["daemon"],"thread_id":null,"message":"Restarted daemon"}`
	select {
	case err := <-r.done:
		if err != ErrExitRequested {
			t.Fatalf("want ErrExitRequested, got %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("no exit")
	}
	if code := <-post; code != http.StatusServiceUnavailable {
		t.Fatalf("the waiting write is refused with the restart text, got %d", code)
	}
	if r.sink.count("during check") != 0 {
		t.Fatal("the held update is not handed off by the exiting gateway")
	}
	r.tg.mu.Lock()
	still := slices.Contains(r.tg.pending, 9)
	r.tg.mu.Unlock()
	if !still {
		t.Fatal("the held update is not confirmed: Telegram keeps it for the next process")
	}
}

// A conflict (409) ends Run with ErrPollConflict after the drain.
func TestPollConflictEndsRun(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	tg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.WriteHeader(http.StatusConflict)
		io.WriteString(w, `{"ok":false,"error_code":409,"description":"Conflict: terminated by other getUpdates request"}`)
	}))
	defer tg.Close()
	sink := &countingSink{got: map[string]int{}}
	err := Run(context.Background(), Config{Settings: Settings{BotToken: token, AllowlistUserIDs: []int64{7}, Root: "/r"}, Socket: f.socket, Log: quiet,
		Telegram: telegram.Options{BaseURL: tg.URL, Logger: quiet},
		Build: func(Deps) (Components, error) {
			return Components{Messages: sink, Callbacks: sink, Renderer: sink}, nil
		}})
	if !errors.Is(err, bot.ErrPollConflict) {
		t.Fatalf("want a poll conflict, got %v", err)
	}
}

// Updates in the Confirm poll's own answer are neither handled nor
// confirmed: the next process gets them.
func TestConfirmLeavesNewUpdatesForTheNextProcess(t *testing.T) {
	c := newConfirmingTelegram(t)
	c.push(7, "handled")
	tg := telegram.NewClient(token, telegram.Options{BaseURL: c.srv.URL, Logger: quiet})
	sink := &countingSink{got: map[string]int{}}
	ctx, cancel := context.WithCancel(context.Background())
	p := &bot.Poller{Source: tg, Handler: &bot.Router{Allow: bot.NewAllowlist([]int64{7}, []int64{-100}), Answer: tg, Messages: sink, Callbacks: sink, Log: quiet}, Log: quiet}
	done := make(chan struct{})
	go func() { p.Run(ctx); close(done) }()
	waitUntil(t, "update 7", 5*time.Second, func() bool { return sink.count("handled") == 1 })
	cancel()
	<-done
	c.push(8, "arrived at stop")
	if err := p.Confirm(context.Background()); err != nil {
		t.Fatal(err)
	}
	if sink.count("arrived at stop") != 0 || c.confirmPolls(8) != 1 {
		t.Fatalf("the confirm poll saw update 8 and did not hand it off: %d %d", sink.count("arrived at stop"), c.confirmPolls(8))
	}
	next := &countingSink{got: map[string]int{}}
	ctx2, cancel2 := context.WithCancel(context.Background())
	p2 := &bot.Poller{Source: tg, Handler: &bot.Router{Allow: bot.NewAllowlist([]int64{7}, []int64{-100}), Answer: tg, Messages: next, Callbacks: next, Log: quiet}, Log: quiet}
	done2 := make(chan struct{})
	go func() { p2.Run(ctx2); close(done2) }()
	waitUntil(t, "update 8 in the next process", 5*time.Second, func() bool { return next.count("arrived at stop") == 1 })
	cancel2()
	<-done2
	if next.count("handled") != 0 {
		t.Fatal("update 7 was confirmed and is not sent again")
	}
}

// A queued-restart check that times out reopens admission while Rust may
// still be restarting its peers. An update polled then is held by the
// service gate: not handed off and not confirmed while the orchestrator
// service is gone, handled once when it is back. The same request is not
// checked again (its outcome is unknown).
func TestQueuedRestartTimeoutThenServiceGoneHoldsUpdates(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	down := f.socket + ".down"
	tg := newConfirmingTelegram(t)
	sink := &countingSink{got: map[string]int{}}
	home := t.TempDir()
	os.MkdirAll(filepath.Join(home, "run"), 0o777)
	os.WriteFile(filepath.Join(home, "run", "restart-queue.json"), []byte("{}"), 0o600)
	var checks atomic.Int64
	timedOut := make(chan struct{})
	// The check takes the service down (peers restarting) and never answers.
	run := func(ctx context.Context, args ...string) ([]byte, error) {
		if checks.Add(1) == 1 {
			if err := os.Rename(f.socket, down); err != nil {
				t.Error(err)
			}
		}
		<-ctx.Done()
		close(timedOut)
		return nil, ctx.Err()
	}
	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/r"}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{
			Settings: settings, Socket: f.socket, Log: quiet, DrainTimeout: 5 * time.Second,
			Telegram: telegram.Options{BaseURL: tg.srv.URL, Logger: quiet},
			Build: func(d Deps) (Components, error) {
				l := &Lifecycle{Home: home, Run: run, Exit: d.RequestExit}
				q := &QueuedRestarts{Lifecycle: l, Idle: d.Idle, Gate: d.Admission, Telegram: d.Telegram, Allow: d.Allow, Log: quiet,
					Every: 10 * time.Millisecond, Timeout: 200 * time.Millisecond}
				return Components{Messages: sink, Callbacks: sink, Renderer: sink, Background: q.Run}, nil
			},
		})
	}()
	waitUntil(t, "the check", 5*time.Second, func() bool { return checks.Load() == 1 })
	tg.push(9, "while peers restart")
	select {
	case <-timedOut:
	case <-time.After(5 * time.Second):
		t.Fatal("the check must time out")
	}
	waitUntil(t, "the poll to fetch update 9", 5*time.Second, func() bool { return tg.times(9) >= 1 })
	time.Sleep(1500 * time.Millisecond)
	tg.mu.Lock()
	held := slices.Contains(tg.pending, 9)
	tg.mu.Unlock()
	if sink.count("while peers restart") != 0 || !held {
		t.Fatalf("update 9 must be held unconfirmed while the service is gone: handed=%d held=%v", sink.count("while peers restart"), held)
	}
	if err := os.Rename(down, f.socket); err != nil {
		t.Fatal(err)
	}
	waitUntil(t, "update 9 once the service is back", 10*time.Second, func() bool { return sink.count("while peers restart") == 1 })
	cancel()
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	if n := checks.Load(); n != 1 || sink.count("while peers restart") != 1 {
		t.Fatalf("one check for the request, one hand-off: checks=%d", n)
	}
}
