package miniapp

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

const feedbackPath = "/api/threads/telegram--1001234-topic-77/artifacts/feedback"

// The gate sees the route of a bot-minted id (conversation.Parse); a thread
// with no chat skips it and is refused by the engine.
func TestFeedbackGateGetsTheThreadRoute(t *testing.T) {
	var routes []FeedbackRoute
	engine := &fakeEngine{feedback: func(context.Context, FeedbackSubmit) (FeedbackResult, error) {
		return FeedbackResult{Status: "queued"}, nil
	}}
	ts := newTestServer(t, engine, func(cfg *Config) { cfg.Feedback = gate{allow: true, routes: &routes} })
	expect(t, ts.post(feedbackPath, `{"submission_id":"s"}`), http.StatusAccepted, "")
	expect(t, ts.post("/api/threads/telegram-4242/artifacts/feedback", `{"submission_id":"s"}`), http.StatusAccepted, "")
	expect(t, ts.post("/api/threads/cli-thread/artifacts/feedback", `{"submission_id":"s"}`), http.StatusAccepted, "")
	if len(routes) != 2 || routes[0].Chat != -1001234 || routes[0].Topic == nil || *routes[0].Topic != 77 ||
		routes[1].Chat != 4242 || routes[1].Topic != nil {
		t.Fatalf("gate routes = %+v", routes)
	}
}

func TestFeedbackIsQueuedOnceAndPassedVerbatim(t *testing.T) {
	var got FeedbackSubmit
	engine := &fakeEngine{feedback: func(_ context.Context, req FeedbackSubmit) (FeedbackResult, error) {
		got = req
		if strings.Contains(string(req.Submission), "again") {
			return FeedbackResult{Status: "duplicate", Annotations: 3, Stale: true}, nil
		}
		return FeedbackResult{Status: "queued", ThreadID: req.ThreadID, Annotations: 2, Stale: true}, nil
	}}
	ts := newTestServer(t, engine)

	// A lone surrogate must reach the engine as sent, so it can refuse it.
	body := "{\"submission_id\":\"s1\",\n \"notes\":[{\"text\":\"emoji \\ud83d\\ude00 lone \\ud800 <&> \u2028\"}]} "
	expect(t, ts.post(feedbackPath, body), http.StatusAccepted, `{"status":"queued","annotations":2,"stale":true}`)
	if got.Submission != body {
		t.Fatalf("submission changed on the way:\n got %s\nwant %s", got.Submission, body)
	}
	if got.ThreadID != "telegram--1001234-topic-77" || got.UserID != testUserID {
		t.Fatalf("engine got %+v", got)
	}

	expect(t, ts.post(feedbackPath, `{"submission_id":"s1","again":true}`), http.StatusOK,
		`{"status":"duplicate","annotations":0,"stale":false}`)
}

func TestFeedbackRefusalsBeforeTheEngine(t *testing.T) {
	engine := &fakeEngine{feedback: func(context.Context, FeedbackSubmit) (FeedbackResult, error) {
		return FeedbackResult{Status: "queued"}, nil
	}}
	ts := newTestServer(t, engine)
	auth := "tma " + freshInitData(testUserID, time.Now())

	expect(t, ts.post("/api/threads/active/artifacts/feedback", `{"submission_id":"s"}`),
		http.StatusBadRequest, "Reopen the thread from its chat to send feedback.")
	expect(t, ts.post("/api/threads/a%2Fb/artifacts/feedback", `{"submission_id":"s"}`),
		http.StatusBadRequest, "Invalid thread ID")
	expect(t, ts.post("/api/threads/%FF/artifacts/feedback", `{`),
		http.StatusBadRequest, "Invalid URL: Invalid UTF-8 in `id`")
	expect(t, ts.do(http.MethodPost, feedbackPath, strings.NewReader(`{"submission_id":"s"}`), map[string]string{"Authorization": auth}),
		http.StatusUnsupportedMediaType, "Expected request with `Content-Type: application/json`")
	expect(t, ts.post(feedbackPath, "{\"submission_id\":\"s\xc0\xff\"}"),
		http.StatusBadRequest, "Failed to parse the request body as JSON: invalid unicode code point")
	if engine.total() != 0 {
		t.Fatalf("engine called %d times for refused submissions", engine.total())
	}

	denied := newTestServer(t, engine, func(cfg *Config) { cfg.Feedback = gate{allow: false} })
	expect(t, denied.post(feedbackPath, `{"submission_id":"s"}`), http.StatusForbidden, "You cannot send feedback into this chat")
	noBot := newTestServer(t, engine, func(cfg *Config) { cfg.Feedback = nil })
	expect(t, noBot.post(feedbackPath, `{"submission_id":"s"}`), http.StatusServiceUnavailable, "Feedback delivery is not available on this server")
	if engine.total() != 0 {
		t.Fatalf("engine called %d times for refused senders", engine.total())
	}
}

// A thread with no chat goes to the engine, which knows whether it is a
// worker and words the refusal; the gate never sees it.
func TestFeedbackOnAThreadWithoutAChatIsRefusedByTheEngine(t *testing.T) {
	message := "Feedback on a worker thread cannot be sent from the Mini App yet."
	engine := &fakeEngine{feedback: func(context.Context, FeedbackSubmit) (FeedbackResult, error) {
		return FeedbackResult{}, &EngineError{Code: CodeNotRoutable, Message: message}
	}}
	ts := newTestServer(t, engine, func(cfg *Config) { cfg.Feedback = gate{allow: false} })
	expect(t, ts.post("/api/threads/worker-1/artifacts/feedback", `{"submission_id":"s"}`), http.StatusConflict, message)
}

func TestFeedbackEngineErrorsMapToTheExistingStatuses(t *testing.T) {
	var next error
	engine := &fakeEngine{feedback: func(context.Context, FeedbackSubmit) (FeedbackResult, error) {
		return FeedbackResult{}, next
	}}
	ts := newTestServer(t, engine)
	cases := []struct {
		err    error
		status int
		body   string
	}{
		{&EngineError{Code: CodeJSONSyntax, Message: "Failed to parse the request body as JSON: trailing characters at line 1 column 24"}, http.StatusBadRequest, "Failed to parse the request body as JSON: trailing characters at line 1 column 24"},
		{&EngineError{Code: CodeJSONData, Message: "Failed to deserialize the JSON body into the target type: missing field `artifact_path` at line 1 column 21"}, http.StatusUnprocessableEntity, "Failed to deserialize the JSON body into the target type: missing field `artifact_path` at line 1 column 21"},
		{&EngineError{Code: CodeInFlight, Message: "This feedback is still being sent. Try again in a moment."}, http.StatusConflict, "This feedback is still being sent. Try again in a moment."},
		{&EngineError{Code: CodeNotRoutable, Message: "not a chat"}, http.StatusConflict, "not a chat"},
		{&EngineError{Code: CodeConflict, Message: "thread 't' has a turn running elsewhere"}, http.StatusConflict, "thread 't' has a turn running elsewhere"},
		{&EngineError{Message: "no code"}, http.StatusInternalServerError, "Saving feedback failed"},
		{&EngineError{Code: CodeInvalid, Message: "bad note"}, http.StatusBadRequest, "bad note"},
		{&EngineError{Code: CodeArtifactNotFound, Message: "gone"}, http.StatusNotFound, "gone"},
		{&EngineError{Code: CodeUnsupportedArtifact, Message: "pdf"}, http.StatusUnprocessableEntity, "pdf"},
		{&EngineError{Code: CodeVersionMismatch, Message: "changed"}, http.StatusPreconditionFailed, "changed"},
		{&EngineError{Code: CodeIDConflict, Message: "reused"}, http.StatusConflict, "reused"},
		{&EngineError{Code: CodeBusy, Message: "busy"}, http.StatusServiceUnavailable, "busy"},
		{&EngineError{Code: CodeForbidden, Message: "no"}, http.StatusForbidden, "no"},
		{&EngineError{Code: "storage", Message: "/private/path failed"}, http.StatusInternalServerError, "Saving feedback failed"},
		{fmt.Errorf("dial: %w", ErrNotSent), http.StatusInternalServerError, "Feedback could not be queued. Try again."},
		{fmt.Errorf("read reply: %w", ErrOutcomeUnknown), http.StatusInternalServerError, feedbackOutcomeUnknown},
		{io.ErrUnexpectedEOF, http.StatusInternalServerError, feedbackOutcomeUnknown},
		{ErrUnavailable, http.StatusServiceUnavailable, "Feedback delivery is not available on this server"},
	}
	for _, tc := range cases {
		next = tc.err
		expect(t, ts.post(feedbackPath, `{"submission_id":"s"}`), tc.status, tc.body)
	}
}

func TestFeedbackSlotsInFlightKeysAndDisconnects(t *testing.T) {
	entered := make(chan context.Context, 4)
	release := make(chan struct{})
	engine := &fakeEngine{feedback: func(ctx context.Context, _ FeedbackSubmit) (FeedbackResult, error) {
		entered <- ctx
		<-release
		return FeedbackResult{Status: "queued", Annotations: 1}, nil
	}}
	ts := newTestServer(t, engine)

	results := make(chan response, 2)
	go func() { results <- ts.post(feedbackPath, `{"submission_id":"same"}`) }()
	first := <-entered

	// Same submission while the first is in flight.
	expect(t, ts.post(feedbackPath, `{"submission_id":"same"}`), http.StatusConflict,
		"This feedback is still being sent. Try again in a moment.")

	// A client that leaves does not cancel the engine call, and its slot is
	// held until the engine answers.
	ctx, cancel := context.WithCancel(context.Background())
	req, _ := http.NewRequestWithContext(ctx, http.MethodPost, ts.srv.URL+feedbackPath, strings.NewReader(`{"submission_id":"other"}`))
	req.Header.Set("Authorization", "tma "+freshInitData(testUserID, time.Now()))
	req.Header.Set("Content-Type", "application/json")
	go func() {
		res, err := http.DefaultClient.Do(req)
		if err == nil {
			res.Body.Close()
		}
	}()
	second := <-entered
	cancel()
	time.Sleep(50 * time.Millisecond)
	if first.Err() != nil || second.Err() != nil {
		t.Fatal("engine call was cancelled with the client")
	}
	expect(t, ts.post(feedbackPath, `{"submission_id":"third"}`), http.StatusTooManyRequests,
		"Another feedback upload is in progress. Try again in a moment.")

	close(release)
	expect(t, <-results, http.StatusAccepted, `{"status":"queued","annotations":1,"stale":false}`)
	deadline := time.Now().Add(2 * time.Second)
	for {
		got := ts.post(feedbackPath, `{"submission_id":"same"}`)
		if got.status == http.StatusAccepted {
			break
		}
		if time.Now().After(deadline) {
			t.Fatalf("slots never freed: %d %q", got.status, got.body)
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func TestFeedbackBodyLimit(t *testing.T) {
	if testing.Short() {
		t.Skip("allocates the 48 MiB limit")
	}
	engine := &fakeEngine{}
	ts := newTestServer(t, engine)
	body := io.MultiReader(strings.NewReader(`{"submission_id":"s","pad":"`),
		io.LimitReader(repeatReader('a'), maxSubmissionBytes), strings.NewReader(`"}`))
	got := ts.do(http.MethodPost, feedbackPath, body, map[string]string{
		"Authorization": "tma " + freshInitData(testUserID, time.Now()),
		"Content-Type":  "application/json",
	})
	expect(t, got, http.StatusRequestEntityTooLarge, "Failed to buffer the request body: length limit exceeded")
	if engine.total() != 0 {
		t.Fatal("oversized body reached the engine")
	}
}

type repeatReader byte

func (r repeatReader) Read(p []byte) (int, error) {
	for i := range p {
		p[i] = byte(r)
	}
	return len(p), nil
}

func TestMonitorCachesTheSnapshotAndNeverAwaitsQuotas(t *testing.T) {
	quotaGate := make(chan struct{})
	var quotaCalls atomic.Int32
	var failQuotas atomic.Bool
	engine := &fakeEngine{
		monitor: func() (json.RawMessage, error) {
			return json.RawMessage(`{"generated_at":"g","services":[],"jobs":[],"config":{"model":"m"}}`), nil
		},
		activeAgents: func() (json.RawMessage, error) { return json.RawMessage(`[{"pid":1}]`), nil },
		quotas: func() (json.RawMessage, error) {
			quotaCalls.Add(1)
			<-quotaGate
			if failQuotas.Load() {
				return nil, io.ErrUnexpectedEOF
			}
			return json.RawMessage(`[{"provider":"p","name":"n","windows":[]}]`), nil
		},
	}
	ts := newTestServer(t, engine)

	// The quota fetch blocks, yet the dashboard answers: pending, empty.
	first := jsonOf(t, ts.get("/api/monitor").body)
	if first["subscriptions_status"] != "pending" || first["subscriptions_age"] != nil {
		t.Fatalf("first monitor: %v", first)
	}
	if subs, _ := first["subscriptions"].([]any); subs == nil || len(subs) != 0 {
		t.Fatalf("pending subscriptions must be an empty list: %v", first["subscriptions"])
	}
	if agents, _ := first["active_agents"].([]any); len(agents) != 1 {
		t.Fatalf("active agents not overlaid: %v", first)
	}
	if first["config"].(map[string]any)["model"] != "m" {
		t.Fatalf("snapshot fields lost: %v", first)
	}
	// Single flight: a second request while the fetch runs starts no other.
	ts.get("/api/monitor")
	if quotaCalls.Load() != 1 {
		t.Fatalf("quota fetches = %d, want 1", quotaCalls.Load())
	}
	quotaGate <- struct{}{}

	deadline := time.Now().Add(2 * time.Second)
	var ready map[string]any
	requests := 2
	for time.Now().Before(deadline) {
		requests++
		ready = jsonOf(t, ts.get("/api/monitor").body)
		if ready["subscriptions_status"] == "ready" {
			break
		}
		time.Sleep(10 * time.Millisecond)
	}
	if ready["subscriptions_status"] != "ready" || ready["subscriptions_age"] != "0s" {
		t.Fatalf("quotas never became ready: %v", ready)
	}
	if subs, _ := ready["subscriptions"].([]any); len(subs) != 1 {
		t.Fatalf("subscriptions: %v", ready["subscriptions"])
	}
	if got := engine.count("monitor_snapshot"); got != 1 {
		t.Fatalf("snapshot built %d times inside the cache window, want 1", got)
	}
	if got := engine.count("active_agents"); got != requests {
		t.Fatalf("active agents read %d times, want once per request", got)
	}

	// A failed fetch keeps "pending" instead of claiming no subscriptions.
	failing := newTestServer(t, engine)
	failQuotas.Store(true)
	ts2 := jsonOf(t, failing.get("/api/monitor").body)
	if ts2["subscriptions_status"] != "pending" {
		t.Fatalf("status = %v", ts2["subscriptions_status"])
	}
	quotaGate <- struct{}{}
	time.Sleep(50 * time.Millisecond)
	if after := jsonOf(t, failing.get("/api/monitor").body); after["subscriptions_status"] != "pending" {
		t.Fatalf("a failed refresh changed the status to %v", after["subscriptions_status"])
	}
	close(quotaGate)
}

func TestMonitorSnapshotIsSingleFlight(t *testing.T) {
	start := make(chan struct{})
	engine := &fakeEngine{
		monitor: func() (json.RawMessage, error) {
			<-start
			return json.RawMessage(`{"services":[]}`), nil
		},
		activeAgents: func() (json.RawMessage, error) { return json.RawMessage(`[]`), nil },
		quotas:       func() (json.RawMessage, error) { return json.RawMessage(`[]`), nil },
	}
	ts := newTestServer(t, engine)
	var wg sync.WaitGroup
	for range 4 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			expect(t, ts.get("/api/monitor"), http.StatusOK, "")
		}()
	}
	time.Sleep(50 * time.Millisecond)
	close(start)
	wg.Wait()
	if got := engine.count("monitor_snapshot"); got != 1 {
		t.Fatalf("concurrent requests built %d snapshots, want 1", got)
	}
}

func TestUsageWindowValidationAndCache(t *testing.T) {
	var days []int
	engine := &fakeEngine{usage: func(d int) (json.RawMessage, error) {
		days = append(days, d)
		return json.RawMessage(`{"days":` + strconv.Itoa(d) + `}`), nil
	}}
	ts := newTestServer(t, engine)
	expect(t, ts.get("/api/usage"), http.StatusOK, `{"days":7}`)
	expect(t, ts.get("/api/usage?days=7"), http.StatusOK, `{"days":7}`)
	expect(t, ts.get("/api/usage?days=1"), http.StatusOK, `{"days":1}`)
	expect(t, ts.get("/api/usage?days=0"), http.StatusBadRequest, "days must be between 1 and 365")
	expect(t, ts.get("/api/usage?days=366"), http.StatusBadRequest, "days must be between 1 and 365")
	expect(t, ts.get("/api/usage?days=x"), http.StatusBadRequest, "")
	if len(days) != 2 || days[0] != 7 || days[1] != 1 {
		t.Fatalf("engine windows = %v, want [7 1] (cached per window)", days)
	}
}

func TestWorkspacesPassThrough(t *testing.T) {
	engine := &fakeEngine{workspaces: func() (json.RawMessage, error) {
		return json.RawMessage(`{"workspaces":[{"root":"/r"}]}`), nil
	}}
	ts := newTestServer(t, engine)
	expect(t, ts.get("/api/workspaces"), http.StatusOK, `{"workspaces":[{"root":"/r"}]}`)
}

func TestGitRoutesForwardToTheEngine(t *testing.T) {
	var diff GitDiff
	var scopeArgs, statusID string
	engine := &fakeEngine{
		gitStatus: func(id string) (json.RawMessage, error) {
			statusID = id
			return json.RawMessage(`{"repository":{}}`), nil
		},
		gitScope: func(id, scope string) (json.RawMessage, error) {
			scopeArgs = id + "|" + scope
			if scope == "bogus" {
				return nil, &EngineError{Code: CodeInvalid, Message: "Invalid Git scope"}
			}
			return json.RawMessage(`{"scope":"all"}`), nil
		},
		gitDiff: func(req GitDiff) (json.RawMessage, error) {
			diff = req
			return nil, &EngineError{Code: CodeNotFound, Message: "Git file is not in that scope"}
		},
	}
	ts := newTestServer(t, engine)

	expect(t, ts.get("/api/git?thread_id=%20"), http.StatusOK, `{"repository":{}}`)
	if statusID != "" {
		t.Fatalf("blank thread_id forwarded as %q", statusID)
	}
	expect(t, ts.get("/api/git?thread_id=active"), http.StatusOK, "")
	expect(t, ts.get("/api/git?thread_id=a%2Fb"), http.StatusBadRequest, "Invalid thread ID")
	expect(t, ts.get("/api/git/scope?thread_id=t1&scope=all"), http.StatusOK, `{"scope":"all"}`)
	if scopeArgs != "t1|all" {
		t.Fatalf("scope args %q", scopeArgs)
	}
	expect(t, ts.get("/api/git/scope?scope=bogus"), http.StatusBadRequest, "Invalid Git scope")
	expect(t, ts.get("/api/git/scope?thread_id=t1"), http.StatusBadRequest, "")
	expect(t, ts.get("/api/git/diff?thread_id=t1&kind=commit&commit=abc1234&path=src/a%20b.rs"), http.StatusNotFound, "Git file is not in that scope")
	if diff != (GitDiff{ThreadID: "t1", Path: "src/a b.rs", Kind: "commit", Commit: "abc1234"}) {
		t.Fatalf("diff request %+v", diff)
	}
	expect(t, ts.get("/api/git/diff?thread_id=t1&kind=staged"), http.StatusBadRequest, "")
}

func TestNewRequiresTokenAndEngine(t *testing.T) {
	if _, err := New(Config{Engine: &fakeEngine{}}); err == nil {
		t.Fatal("accepted an empty bot token")
	}
	if _, err := New(Config{BotToken: "t"}); err == nil {
		t.Fatal("accepted a nil engine")
	}
}

// idempotentEngine keeps the engine's feedback rules that matter to the
// route: a submission id is queued once, a retry of a queued one is a
// duplicate, a refused one leaves nothing behind. next, for one call, is
// asked before queueing (queuedNow false: a refusal or ErrNotSent ends the
// call there) and after queueing (queuedNow true: an error is a lost answer).
type idempotentEngine struct {
	fakeEngine
	mu     sync.Mutex
	queued map[string]int
	calls  int
	next   func(queuedNow bool) error
	before func()
}

func newIdempotentEngine() *idempotentEngine {
	e := &idempotentEngine{queued: map[string]int{}}
	e.fakeEngine.feedback = e.submit
	return e
}

func (e *idempotentEngine) submit(ctx context.Context, req FeedbackSubmit) (FeedbackResult, error) {
	if e.before != nil {
		e.before()
	}
	if ctx.Err() != nil {
		return FeedbackResult{}, fmt.Errorf("engine call cancelled: %w", ctx.Err())
	}
	id, _ := submissionID([]byte(req.Submission))
	key := req.ThreadID + "/" + id
	e.mu.Lock()
	defer e.mu.Unlock()
	e.calls++
	next := e.next
	e.next = nil
	if next != nil {
		if err := next(false); err != nil {
			var refused *EngineError
			if errors.As(err, &refused) || errors.Is(err, ErrNotSent) {
				return FeedbackResult{}, err
			}
		}
	}
	if e.queued[key] > 0 {
		return FeedbackResult{Status: "duplicate"}, nil
	}
	e.queued[key]++
	if next != nil {
		if err := next(true); err != nil {
			return FeedbackResult{}, err
		}
	}
	return FeedbackResult{Status: "queued", Annotations: 1}, nil
}

func (e *idempotentEngine) count(key string) int {
	e.mu.Lock()
	defer e.mu.Unlock()
	return e.queued[key]
}

func TestFeedbackRejectedNotSentAndUnknownOutcomes(t *testing.T) {
	const body = `{"submission_id":"s1"}`
	key := "telegram--1001234-topic-77/s1"
	queuedOK := `{"status":"queued","annotations":1,"stale":false}`
	duplicate := `{"status":"duplicate","annotations":0,"stale":false}`

	t.Run("a refusal is final and leaves nothing queued", func(t *testing.T) {
		engine := newIdempotentEngine()
		ts := newTestServer(t, &engine.fakeEngine)
		refusal := &EngineError{Code: CodeVersionMismatch, Message: "The file changed after you annotated it."}
		engine.next = func(bool) error { return refusal }
		expect(t, ts.post(feedbackPath, body), http.StatusPreconditionFailed, refusal.Message)
		if engine.count(key) != 0 {
			t.Fatal("a refused submission was queued")
		}
		expect(t, ts.post(feedbackPath, body), http.StatusAccepted, queuedOK)
	})

	t.Run("not sent says so and a retry queues it", func(t *testing.T) {
		engine := newIdempotentEngine()
		ts := newTestServer(t, &engine.fakeEngine)
		engine.next = func(bool) error { return fmt.Errorf("connect: %w", ErrNotSent) }
		expect(t, ts.post(feedbackPath, body), http.StatusInternalServerError, "Feedback could not be queued. Try again.")
		expect(t, ts.post(feedbackPath, body), http.StatusAccepted, queuedOK)
		if engine.count(key) != 1 {
			t.Fatalf("queued %d times", engine.count(key))
		}
	})

	t.Run("a lost answer after queueing is unknown and the retry is a duplicate", func(t *testing.T) {
		engine := newIdempotentEngine()
		ts := newTestServer(t, &engine.fakeEngine)
		engine.next = func(queued bool) error {
			if queued {
				return fmt.Errorf("no reply in 2m0s: %w", ErrOutcomeUnknown)
			}
			return nil
		}
		got := ts.post(feedbackPath, body)
		expect(t, got, http.StatusInternalServerError, feedbackOutcomeUnknown)
		for _, claim := range []string{"could not be queued", "never", "twice"} {
			if strings.Contains(got.body, claim) {
				t.Fatalf("an unknown outcome claims more than is known (%q): %s", claim, got.body)
			}
		}
		expect(t, ts.post(feedbackPath, body), http.StatusOK, duplicate)
		if engine.count(key) != 1 {
			t.Fatalf("queued %d times", engine.count(key))
		}
	})

	t.Run("a lost request is unknown and the retry queues it once", func(t *testing.T) {
		engine := newIdempotentEngine()
		ts := newTestServer(t, &engine.fakeEngine)
		// The service read nothing: the adapter cannot tell this from a lost
		// answer, so it is unknown too.
		engine.fakeEngine.feedback = func(context.Context, FeedbackSubmit) (FeedbackResult, error) {
			return FeedbackResult{}, fmt.Errorf("connection closed without a reply: %w", ErrOutcomeUnknown)
		}
		expect(t, ts.post(feedbackPath, body), http.StatusInternalServerError, feedbackOutcomeUnknown)
		engine.fakeEngine.feedback = engine.submit
		expect(t, ts.post(feedbackPath, body), http.StatusAccepted, queuedOK)
		expect(t, ts.post(feedbackPath, body), http.StatusOK, duplicate)
		if engine.count(key) != 1 {
			t.Fatalf("queued %d times", engine.count(key))
		}
	})
}

// A client that leaves mid-request does not cancel the engine call; the
// submission is queued once, a retry while it runs is told to wait, and a
// retry after it is a duplicate.
func TestFeedbackClientDisconnectThenRetry(t *testing.T) {
	const body = `{"submission_id":"s-disconnect"}`
	key := "telegram--1001234-topic-77/s-disconnect"
	engine := newIdempotentEngine()
	entered, release := make(chan struct{}), make(chan struct{})
	engine.before = func() {
		entered <- struct{}{}
		<-release
	}
	ts := newTestServer(t, &engine.fakeEngine)

	ctx, cancel := context.WithCancel(context.Background())
	req, _ := http.NewRequestWithContext(ctx, http.MethodPost, ts.srv.URL+feedbackPath, strings.NewReader(body))
	req.Header.Set("Authorization", "tma "+freshInitData(testUserID, time.Now()))
	req.Header.Set("Content-Type", "application/json")
	done := make(chan error, 1)
	go func() {
		res, err := http.DefaultClient.Do(req)
		if err == nil {
			res.Body.Close()
		}
		done <- err
	}()
	<-entered
	cancel()
	if err := <-done; err == nil {
		t.Fatal("the client request should have been cancelled")
	}

	expect(t, ts.post(feedbackPath, body), http.StatusConflict, "This feedback is still being sent. Try again in a moment.")
	close(release)

	deadline := time.Now().Add(2 * time.Second)
	for engine.count(key) == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if engine.count(key) != 1 {
		t.Fatal("the engine call did not finish after the client left")
	}
	engine.before = nil
	for time.Now().Before(deadline) {
		got := ts.post(feedbackPath, body)
		if got.status != http.StatusConflict {
			expect(t, got, http.StatusOK, `{"status":"duplicate","annotations":0,"stale":false}`)
			break
		}
		time.Sleep(5 * time.Millisecond)
	}
	if engine.count(key) != 1 {
		t.Fatalf("queued %d times", engine.count(key))
	}
}
