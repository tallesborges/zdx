package ui

import (
	"context"
	"fmt"
	"os"
	"strings"
	"sync"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/shared/sandbox"
	"github.com/tallesborges/zdx/apps/shared/transcript"
	"github.com/tallesborges/zdx/apps/tui/internal/engine"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func agent(turn uint64, js string) engine.Event {
	return engine.Event{Type: engine.EventAgent, ThreadID: "t", TurnID: turn, Inner: []byte(js)}
}

func TestLiveKeepsEveryDeltaAndCompletedTextWins(t *testing.T) {
	l := newLive(1)
	l.user("hi", 0)
	for _, js := range []string{
		`{"type":"reasoning_delta","text":"think "}`,
		`{"type":"reasoning_delta","text":"more"}`,
		`{"type":"reasoning_completed","block":{}}`,
		`{"type":"assistant_delta","text":"Hel"}`,
		`{"type":"assistant_delta","text":"lo"}`,
		`{"type":"tool_requested","id":"c1","name":"bash","input":{"command":"ls -la"}}`,
		`{"type":"tool_started","id":"c1","name":"bash"}`,
		`{"type":"tool_output_delta","id":"c1","chunk":"a\n"}`,
		`{"type":"tool_output_delta","id":"c1","chunk":"b\n"}`,
		`{"type":"tool_completed","id":"c1","result":{"ok":true,"data":{}},"duration_ms":5}`,
		`{"type":"assistant_delta","text":"Do"}`,
		`{"type":"assistant_completed","text":"Done."}`,
	} {
		a, err := agent(1, js).Agent()
		if err != nil {
			t.Fatal(err)
		}
		l.apply(a)
	}
	var got []string
	for _, a := range l.items {
		got = append(got, a.Type+":"+a.Text)
	}
	want := []string{"message:hi", "reasoning:think more", "message:Hello", "tool_use:", "tool_result:", "message:Done."}
	if strings.Join(got, "|") != strings.Join(want, "|") {
		t.Fatalf("items %v, want %v", got, want)
	}
	lines, tools := transcript.Render(l.thread(), 80, transcript.Options{}, nil)
	plain := ansi.Strip(strings.Join(lines, "\n"))
	if len(tools) != 1 || !strings.Contains(plain, "✓ bash ls -la") {
		t.Fatalf("tool row missing:\n%s", plain)
	}
	if detail := ansi.Strip(strings.Join(transcript.Detail(l.thread(), "c1", 80), "\n")); !strings.Contains(detail, "a\n b") {
		t.Fatalf("detail lacks the output:\n%s", detail)
	}
}

func TestLiveFailureAndInterrupt(t *testing.T) {
	l := newLive(1)
	a, _ := agent(1, `{"type":"turn_finished","status":{"status":"failed","kind":"http_status","message":"429"},"final_text":"","messages":[]}`).Agent()
	l.apply(a)
	if !l.failed || l.items[0].Type != "notice" || !strings.Contains(l.items[0].Message, "429") {
		t.Fatalf("items %+v", l.items)
	}
	a, _ = agent(2, `{"type":"turn_finished","status":{"status":"interrupted"},"final_text":"","messages":[]}`).Agent()
	l.apply(a)
	if l.items[len(l.items)-1].Type != "interrupted" {
		t.Fatalf("items %+v", l.items)
	}
}

// fakeBackend is a service with one thread subscription: onSubmit gives
// the events a submit causes, pushed in order onto the subscription.
type fakeBackend struct {
	mu        sync.Mutex
	stream    chan engine.Event
	onSubmit  func(req engine.SubmitRequest, input uint64) []engine.Event
	inTurn    bool
	delay     time.Duration
	nextInput uint64
	submits   []engine.SubmitRequest
	thread    transcript.Thread
	updates   []engine.Update
	cancels   int
}

func newFake() *fakeBackend { return &fakeBackend{stream: make(chan engine.Event, 4096)} }

func (f *fakeBackend) Require(context.Context, ...string) error { return nil }
func (f *fakeBackend) Submit(_ context.Context, req engine.SubmitRequest) (engine.Submitted, error) {
	f.mu.Lock()
	f.submits = append(f.submits, req)
	f.nextInput++
	input := f.nextInput
	var evs []engine.Event
	if f.onSubmit != nil {
		evs = f.onSubmit(req, input)
	}
	delay := f.delay
	f.mu.Unlock()
	for _, ev := range evs {
		f.stream <- ev
	}
	time.Sleep(delay)
	return engine.Submitted{ThreadID: req.ThreadID, InputID: input, InTurn: f.inTurn && req.Steer}, nil
}
func (f *fakeBackend) Subscribe(context.Context, string) (*engine.Subscription, error) {
	return &engine.Subscription{Events: f.stream, Err: func() error { return nil }}, nil
}
func (f *fakeBackend) Cancel(context.Context, string, bool) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.cancels++
	return nil
}
func (f *fakeBackend) UpdateThread(_ context.Context, u engine.Update) (engine.Updated, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.updates = append(f.updates, u)
	return engine.Updated{}, nil
}
func (f *fakeBackend) GenerateTitle(context.Context, string, string) (string, error) {
	return "A title", nil
}
func (f *fakeBackend) Picker(context.Context, string) (engine.Picker, error) {
	return engine.Picker{Models: []engine.PickerModel{{ID: "p:a", Name: "p:a", Specs: map[string]string{"off": "p:a@off", "high": "p:a@high"}}}, Model: "p:a", Thinking: "off"}, nil
}
func (f *fakeBackend) Status(context.Context, string) (engine.ThreadStatus, error) {
	return engine.ThreadStatus{Model: "p:a", Thinking: "Off", Root: "/r", ContextLimit: 1000, ContextTokens: 100}, nil
}
func (f *fakeBackend) Threads(context.Context) ([]transcript.ThreadListItem, error) {
	return []transcript.ThreadListItem{{ID: "old", Title: "Old thread"}}, nil
}
func (f *fakeBackend) Thread(_ context.Context, id string, after *uint64) (transcript.Thread, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	th := f.thread
	th.ID = id
	return th, nil
}
func (f *fakeBackend) PrepareImage(context.Context, string) (*engine.Image, error) {
	return &engine.Image{MimeType: "image/png", Data: "AA=="}, nil
}
func (f *fakeBackend) Followups(context.Context, string) ([]string, error) {
	return []string{"Do the next thing"}, nil
}
func (f *fakeBackend) RunningTurn(context.Context, string) (uint64, error) { return 0, nil }
func (f *fakeBackend) TranscribeAudio(context.Context, string, string) (string, error) {
	return "spoken", nil
}

func (f *fakeBackend) requests() []engine.SubmitRequest {
	f.mu.Lock()
	defer f.mu.Unlock()
	return append([]engine.SubmitRequest(nil), f.submits...)
}

// harness is the Bubble Tea loop for one model: commands run on their own
// goroutines, and their messages go back through Update in arrival order.
type harness struct {
	t       *testing.T
	m       *Model
	results chan tea.Msg
}

func start(t *testing.T, be *fakeBackend, opts Options) *harness {
	t.Helper()
	h := &harness{t: t, m: New(context.Background(), be, opts), results: make(chan tea.Msg, 1024)}
	t.Cleanup(h.m.Close)
	h.m.Update(tea.WindowSizeMsg{Width: 100, Height: 30})
	h.run(h.m.Init())
	h.settle()
	return h
}

func (h *harness) run(cmd tea.Cmd) {
	if cmd != nil {
		go func() { h.results <- cmd() }()
	}
}

// settle handles messages until none comes for a while.
func (h *harness) settle() {
	h.t.Helper()
	for {
		select {
		case msg := <-h.results:
			h.handle(msg)
		case <-time.After(300 * time.Millisecond):
			return
		}
	}
}

func (h *harness) handle(msg tea.Msg) {
	switch msg := msg.(type) {
	case nil:
	case tea.BatchMsg:
		for _, c := range msg {
			h.run(c)
		}
	default:
		if n := strings.TrimPrefix(fmt.Sprintf("%T", msg), "*"); !strings.HasPrefix(n, "ui.") {
			return
		}
		_, next := h.m.Update(msg)
		h.run(next)
	}
}

// send gives the model one input message, then settles.
func (h *harness) send(msg tea.Msg) {
	_, cmd := h.m.Update(msg)
	h.run(cmd)
	h.settle()
}

func (h *harness) typeEnter(text string) {
	h.m.composer.SetValue(text)
	h.send(tea.KeyPressMsg{Code: tea.KeyEnter})
}

func started(turn, input uint64) engine.Event {
	ev := engine.Event{Type: engine.EventTurnStarted, ThreadID: "t", TurnID: turn}
	ev.Inputs = append(ev.Inputs, struct {
		InputID uint64 `json:"input_id"`
	}{input})
	return ev
}

func ended(turn uint64) engine.Event {
	return engine.Event{Type: engine.EventTurnEnded, ThreadID: "t", TurnID: turn}
}

func checkFrame(t *testing.T, m *Model) string {
	t.Helper()
	frame := m.render()
	lines := plainLines(frame)
	if len(lines) != m.height {
		t.Fatalf("%dx%d: frame has %d lines", m.width, m.height, len(lines))
	}
	for _, l := range strings.Split(frame, "\n") {
		if w := ansi.StringWidth(l); w > m.width {
			t.Fatalf("%dx%d: line %d cells: %q", m.width, m.height, w, ansi.Strip(l))
		}
	}
	return strings.Join(lines, "\n")
}

func TestChatTurnStreamsAndSettles(t *testing.T) {
	be := newFake()
	be.onSubmit = func(req engine.SubmitRequest, input uint64) []engine.Event {
		be.thread = transcript.Thread{Cursor: 3, Activity: []transcript.Activity{
			{Type: "message", Sequence: 0, Role: "user", Speaker: "you", Text: "hi there"},
			{Type: "message", Sequence: 1, Role: "assistant", Speaker: "assistant", Text: "Hello **world**"},
		}}
		return []engine.Event{
			started(4, input),
			agent(4, `{"type":"assistant_delta","text":"Hello "}`),
			agent(4, `{"type":"assistant_delta","text":"**world**"}`),
			agent(4, `{"type":"turn_finished","status":{"status":"completed"},"final_text":"Hello **world**","messages":[]}`),
			ended(4),
		}
	}
	h := start(t, be, Options{Root: "/r", Home: "/h"})
	m := h.m
	checkFrame(t, m)
	h.typeEnter("hi there")
	reqs := be.requests()
	if len(reqs) != 1 || reqs[0].Text != "hi there" || reqs[0].Steer || reqs[0].ThreadID != m.threadID {
		t.Fatalf("submits %+v", reqs)
	}
	f := checkFrame(t, m)
	if !strings.Contains(f, "Hello world") || strings.Count(f, "hi there") != 1 || !strings.Contains(f, "1. Do the next thing") {
		t.Fatalf("frame:\n%s", f)
	}
	if m.busy() || len(m.turns) != 0 || m.title != "A title" {
		t.Fatalf("busy=%v turns=%d title=%q", m.busy(), len(m.turns), m.title)
	}
	for _, size := range [][2]int{{20, 8}, {40, 12}, {120, 40}, {200, 60}} {
		m.Update(tea.WindowSizeMsg{Width: size[0], Height: size[1]})
		checkFrame(t, m)
	}
}

// The subscription can show a turn's start before the submit's receipt
// arrives; the message still shows in that turn, once.
func TestReceiptAfterTurnStart(t *testing.T) {
	be := newFake()
	be.delay = 200 * time.Millisecond
	be.onSubmit = func(_ engine.SubmitRequest, input uint64) []engine.Event {
		return []engine.Event{started(1, input), agent(1, `{"type":"assistant_delta","text":"answer"}`)}
	}
	h := start(t, be, Options{Root: "/r"})
	h.typeEnter("question")
	f := checkFrame(t, h.m)
	if strings.Count(f, "question") != 1 || !strings.Contains(f, "answer") || len(h.m.outbox) != 0 {
		t.Fatalf("outbox %+v frame:\n%s", h.m.outbox, f)
	}
	if q, a := strings.Index(f, "question"), strings.Index(f, "answer"); q > a {
		t.Fatalf("message after the answer:\n%s", f)
	}
}

// A message queued while a reply streams never lands inside that reply,
// and the completed text replaces the deltas once.
func TestQueuedMessageStaysOutOfTheStreamingReply(t *testing.T) {
	be := newFake()
	h := start(t, be, Options{Root: "/r"})
	be.stream <- started(1, 99)
	be.stream <- agent(1, `{"type":"assistant_delta","text":"Hel"}`)
	h.settle()
	h.m.composer.SetValue("next one")
	h.send(tea.KeyPressMsg{Code: tea.KeyTab})
	be.stream <- agent(1, `{"type":"assistant_delta","text":"lo"}`)
	be.stream <- agent(1, `{"type":"assistant_completed","text":"Hello"}`)
	h.settle()
	f := checkFrame(t, h.m)
	if strings.Count(f, "Hel") != 1 || !strings.Contains(f, "Hello") || !strings.Contains(f, "you (queued)") {
		t.Fatalf("frame:\n%s", f)
	}
	if reqs := be.requests(); len(reqs) != 1 || reqs[0].Steer {
		t.Fatalf("submits %+v", reqs)
	}
	// The queued message starts the next turn and moves into it.
	be.stream <- ended(1)
	be.stream <- started(2, 1)
	h.settle()
	if len(h.m.outbox) != 0 || len(h.m.turns) < 1 || h.m.turns[len(h.m.turns)-1].items[0].Text != "next one" {
		t.Fatalf("outbox %+v turns %d", h.m.outbox, len(h.m.turns))
	}
}

func TestEnterWhileRunningSteersAndEscInterrupts(t *testing.T) {
	be := newFake()
	be.inTurn = true
	h := start(t, be, Options{Root: "/r"})
	be.stream <- started(9, 50)
	h.settle()
	h.typeEnter("change course")
	if reqs := be.requests(); len(reqs) != 1 || !reqs[0].Steer {
		t.Fatalf("submits %+v", reqs)
	}
	if f := checkFrame(t, h.m); !strings.Contains(f, "waiting for the turn") {
		t.Fatalf("steer not shown:\n%s", f)
	}
	be.stream <- agent(9, `{"type":"user_message_injected","id":"1","source":"user","text":"change course"}`)
	h.settle()
	if f := checkFrame(t, h.m); strings.Contains(f, "waiting for the turn") || strings.Count(f, "change course") != 1 {
		t.Fatalf("injected steer:\n%s", f)
	}
	h.send(tea.KeyPressMsg{Code: tea.KeyEscape})
	if be.cancels != 1 {
		t.Fatalf("cancels %d", be.cancels)
	}
}

func TestRetryReruns(t *testing.T) {
	be := newFake()
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	h.typeEnter("/retry")
	reqs := be.requests()
	if len(reqs) != 1 || !reqs[0].Rerun || reqs[0].Text != "" {
		t.Fatalf("submits %+v", reqs)
	}
	if h.m.composer.Value() != "" {
		t.Fatalf("composer %q", h.m.composer.Value())
	}
}

// An older load answer never replaces a newer one.
func TestStaleThreadLoadIsDropped(t *testing.T) {
	be := newFake()
	h := start(t, be, Options{Root: "/r", ThreadID: "t"})
	m := h.m
	newer := transcript.Thread{Cursor: 5, Activity: []transcript.Activity{{Type: "message", Role: "assistant", Text: "new"}}}
	m.loadSeq = 10
	m.Update(threadMsg{gen: m.gen, seq: 10, thread: newer})
	m.Update(threadMsg{gen: m.gen, seq: 9, thread: transcript.Thread{Cursor: 1}})
	if m.thread == nil || m.thread.Cursor != 5 {
		t.Fatalf("thread %+v", m.thread)
	}
}

func TestModelPickedBeforeFirstMessageGoesWithIt(t *testing.T) {
	be := newFake()
	h := start(t, be, Options{Root: "/r"})
	h.send(tea.KeyPressMsg{Code: 'l', Mod: tea.ModCtrl})
	if h.m.overlay == nil || len(h.m.overlay.items) != 1 {
		t.Fatalf("picker %+v", h.m.overlay)
	}
	checkFrame(t, h.m)
	h.send(tea.KeyPressMsg{Code: tea.KeyEnter})
	// Two levels: the level picker opens; pick high.
	if h.m.overlay == nil || len(h.m.overlay.items) != 2 {
		t.Fatalf("level picker %+v", h.m.overlay)
	}
	h.send(tea.KeyPressMsg{Code: tea.KeyDown})
	h.send(tea.KeyPressMsg{Code: tea.KeyEnter})
	if len(be.updates) != 0 || h.m.pendingSel == nil || *h.m.pendingSel != "p:a@high" {
		t.Fatalf("updates %+v pending %v", be.updates, h.m.pendingSel)
	}
	h.typeEnter("go")
	if len(be.updates) < 1 || be.updates[0].Model == nil || **be.updates[0].Model != "p:a@high" || be.updates[0].Root != "/r" {
		t.Fatalf("updates %+v", be.updates)
	}
}

func TestImagePathPaste(t *testing.T) {
	dir := t.TempDir()
	p := dir + "/shot one.png"
	if err := os.WriteFile(p, []byte("x"), 0o600); err != nil {
		t.Fatal(err)
	}
	for _, in := range []string{p, "'" + p + "'", strings.ReplaceAll(p, " ", `\ `), "file://" + p} {
		if got, ok := imagePath(in); !ok || got != p {
			t.Errorf("imagePath(%q) = %q, %v", in, got, ok)
		}
	}
	for _, in := range []string{dir + "/missing.png", dir, "hello", p + "\nmore"} {
		if _, ok := imagePath(in); ok {
			t.Errorf("imagePath(%q) accepted", in)
		}
	}
}

// Streamed text reaches the screen without foreign escape sequences or
// controls.
func TestLiveTextCarriesNoForeignControls(t *testing.T) {
	l := newLive(1)
	for _, js := range []string{
		`{"type":"assistant_delta","text":"a\u001b]52;c;eA==\u0007b \u001b[2Jcleared"}`,
		`{"type":"tool_requested","id":"c1","name":"ba\u001b[2Jsh","input":{"command":"x\u001b]8;;u\u0007"}}`,
		`{"type":"tool_output_delta","id":"c1","chunk":"out\u001bPpass\u001b\\ \r over"}`,
		`{"type":"notice","kind":"output_limit","message":"m\u001b]52;c;eA==\u0007"}`,
	} {
		a, err := agent(1, js).Agent()
		if err != nil {
			t.Fatal(err)
		}
		l.apply(a)
	}
	lines, _ := transcript.Render(l.thread(), 100, transcript.Options{Thinking: true}, nil)
	frame := strings.Join(append(lines, transcript.Detail(l.thread(), "c1", 100)...), "\n")
	for _, bad := range []string{"\x1b]", "\x1bP", "\x07", "\x1b[2J", "\r"} {
		if strings.Contains(frame, bad) {
			t.Fatalf("%q reached the frame:\n%q", bad, frame)
		}
	}
	if !strings.Contains(ansi.Strip(frame), "bash") || !strings.Contains(ansi.Strip(frame), "over") {
		t.Fatalf("text missing:\n%s", ansi.Strip(frame))
	}
}
