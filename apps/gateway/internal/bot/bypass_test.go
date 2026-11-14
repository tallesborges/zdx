package bot

import (
	"context"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// btwHook is a fake commands package: /btw opens a session that takes the
// next non-synthetic message; /cancel ends it.
type btwHook struct {
	mu       sync.Mutex
	live     bool
	consumed []string
	// expireBeforeInput ends the session between the bypass decision and
	// the input.
	expireBeforeInput bool
	synthetic         []bool
}

func (b *btwHook) HandleCommand(_ context.Context, req CommandRequest) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	switch req.Text {
	case "/btw":
		b.live = true
		return true
	case "/cancel":
		b.live = false
		b.consumed = append(b.consumed, "/cancel")
		return true
	}
	return false
}

func (b *btwHook) StagedInput(_ context.Context, req CommandRequest, _ string, synthetic bool) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	b.synthetic = append(b.synthetic, synthetic)
	if b.expireBeforeInput {
		b.live = false
	}
	if !b.live || synthetic {
		return false
	}
	b.live = false
	b.consumed = append(b.consumed, req.Text)
	return true
}

func (b *btwHook) HasLiveSession(string) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.live
}

func (b *btwHook) BypassesQueue(text string) bool { return text == "/btw" }

func (b *btwHook) AwaitingBtwInput(_ string, synthetic bool) bool {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.live && !synthetic
}

func (b *btwHook) StaleBtwInput() string { return "STALE" }

func (b *btwHook) got() []string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return append([]string(nil), b.consumed...)
}

// btwSetup opens a /btw session and then blocks the topic's queue, as a
// slow message ahead would.
func btwSetup(t *testing.T) (*Handler, *btwHook, *fakeTG, *fakeSvc, func()) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText, engine.CapRecordDelivery})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	hook := &btwHook{}
	h.Commands, h.Bypass = hook, hook
	h.Dispatch(context.Background(), userMessage(t, 43, "/btw"))
	h.Wait()
	block := make(chan struct{})
	started := make(chan struct{})
	h.Queues.Enqueue(KeyOf(-100, ptr(int64(5))), func(context.Context) { close(started); <-block })
	<-started
	var once sync.Once
	return h, hook, tg, svc, func() { once.Do(func() { close(block) }) }
}

func TestBtwQuestionSkipsTheBusyTopicQueue(t *testing.T) {
	h, hook, _, svc, unblock := btwSetup(t)
	defer unblock()
	h.Dispatch(context.Background(), userMessage(t, 44, "side question"))
	deadline := time.Now().Add(5 * time.Second)
	for len(hook.got()) == 0 && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	if got := hook.got(); len(got) != 1 || got[0] != "side question" {
		t.Fatalf("the question must reach the session while the queue is busy: %v", got)
	}
	if len(svc.requests("compose_user_text")) != 0 || len(svc.requests("submit")) != 0 {
		t.Fatal("a /btw question never runs a turn")
	}
}

func TestStaleBtwQuestionIsAnsweredNotRun(t *testing.T) {
	h, hook, tg, svc, unblock := btwSetup(t)
	defer unblock()
	hook.mu.Lock()
	hook.expireBeforeInput = true
	hook.mu.Unlock()
	h.Dispatch(context.Background(), userMessage(t, 44, "late question"))
	tg.waitFor(t, "the stale notice", func() bool {
		for _, c := range tg.of("sendMessage") {
			if c.body["text"] == "STALE" && c.body["reply_to_message_id"] == float64(44) {
				return true
			}
		}
		return false
	})
	if len(svc.requests("compose_user_text")) != 0 || len(svc.requests("submit")) != 0 {
		t.Fatal("a stale /btw question never runs a turn")
	}
}

func TestBtwCancelSkipsTheQueue(t *testing.T) {
	h, hook, _, _, unblock := btwSetup(t)
	defer unblock()
	h.Dispatch(context.Background(), userMessage(t, 44, "/cancel"))
	h.standalone.Wait()
	if got := hook.got(); len(got) != 1 || got[0] != "/cancel" {
		t.Fatalf("/cancel must end the session off the queue: %v", got)
	}
}

// Older and synthetic messages keep their place in the queue and run in
// order once it frees; neither is taken as the question.
func TestOlderAndSyntheticMessagesAreNotTakenAsTheQuestion(t *testing.T) {
	h, hook, _, svc, unblock := btwSetup(t)
	older := userMessage(t, 42, "late album")
	synthetic := userMessage(t, 45, "feedback")
	synthetic.Synthetic = true
	h.Dispatch(context.Background(), older)
	h.Dispatch(context.Background(), synthetic)
	h.standalone.Wait()
	if len(hook.got()) != 0 || len(svc.ops()) != 0 {
		t.Fatalf("nothing may run while the queue is busy: %v %v", hook.got(), svc.ops())
	}
	unblock()
	h.Queues.Wait()
	hook.mu.Lock()
	defer hook.mu.Unlock()
	// In the queue the older message is offered to the session like any
	// other (the commands package applies its own message-id check; this
	// fake takes it).
	if len(hook.consumed) != 1 || hook.consumed[0] != "late album" {
		t.Fatalf("consumed in queue: %v", hook.consumed)
	}
	if len(hook.synthetic) == 0 || !hook.synthetic[len(hook.synthetic)-1] {
		t.Fatalf("the synthetic flag must reach the staging hook: %v", hook.synthetic)
	}
	if c := svc.requests("compose_user_text"); len(c) != 1 || !strings.Contains(c[0]["text"].(string), "feedback") {
		t.Fatalf("the synthetic message runs as a turn: %v", c)
	}
}

func TestBlockedTopicRefusesTurnsAcrossRestarts(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "unsetup-topics")
	first := &TopicBlocks{Dir: dir}
	if err := first.BlockTopic(context.Background(), thread); err != nil {
		t.Fatal(err)
	}
	if err := first.BlockTopic(context.Background(), "../x"); err == nil {
		t.Fatal("a thread id that leaves the directory must be refused")
	}
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapComposeUserText, engine.CapRecordDelivery})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Blocks = &TopicBlocks{Dir: dir} // a new process: only the marker
	h.Dispatch(context.Background(), userMessage(t, 42, "hello"))
	h.Queues.Wait()
	if s := tg.of("sendMessage"); len(s) != 1 || s[0].body["text"] != EscapeHTML(UnsetupTopicNotice) {
		t.Fatalf("unexpected notices %v", s)
	}
	if len(svc.requests("compose_user_text")) != 0 {
		t.Fatal("a blocked topic runs no turn")
	}
}
