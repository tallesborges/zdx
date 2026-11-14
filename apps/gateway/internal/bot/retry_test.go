package bot

import (
	"context"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// failThenAnswer fails the first submit's turn and answers the next.
func failThenAnswer(svc *fakeSvc) func(sc svcConn) {
	var n atomic.Int32
	return func(sc svcConn) {
		c := sc.conn
		turn := 10 + n.Add(1)
		write(c, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": turn, "queued_ahead": 0})
		status := map[string]any{"status": "completed"}
		final := "second try worked"
		if turn == 11 {
			status = map[string]any{"status": "failed", "kind": "api_error", "message": "overloaded"}
			final = ""
		}
		write(c, agent(thread, uint64(turn), map[string]any{"type": "turn_finished", "status": status, "final_text": final, "messages": []any{}}))
		write(c, event(map[string]any{"type": "turn_ended", "thread_id": thread, "turn_id": turn, "held": true}))
		for range 500 {
			if len(svc.requests("release")) >= int(turn-10) {
				break
			}
			time.Sleep(10 * time.Millisecond)
		}
		c.Close()
	}
}

var retryCaps = []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery, engine.CapRerun}

func TestFailedOrdinaryTurnOffersRetryAndRerunsTheHistory(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, retryCaps)
	svc.submit = failThenAnswer(svc)
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
	var buttonsID int64
	tg.waitFor(t, "retry button", func() bool {
		for _, c := range tg.of("sendMessage") {
			if c.body["text"] == RetryPrompt {
				return true
			}
		}
		return false
	})
	h.Wait()
	sends := tg.of("sendMessage")
	last := sends[len(sends)-1]
	if last.body["text"] != RetryPrompt || !strings.Contains(fmtAny(last.body["reply_markup"]), RetryGo) {
		t.Fatalf("unexpected buttons %v", last.body)
	}
	// The fake numbers messages in call order; the buttons are the last one.
	buttonsID = lastMessageID(tg)

	h.HandleCallback(context.Background(), tap(t, RetryGo, buttonsID, ""))
	tg.waitFor(t, "second reply", func() bool {
		for _, c := range tg.of("sendMessage") {
			if c.body["text"] == "second try worked" {
				return true
			}
		}
		return false
	})
	h.Wait()
	subs := svc.requests("submit")
	if len(subs) != 2 {
		t.Fatalf("want two submits, got %d", len(subs))
	}
	re := subs[1]
	if re["rerun"] != true || re["ordinary"] != true || re["text"] != "" || re["delivery"] != "queue" || re["thread_id"] != thread {
		t.Fatalf("unexpected rerun %v", re)
	}
	if len(svc.requests("compose_user_text")) != 1 {
		t.Fatal("a rerun records no new user message")
	}
	edits := tg.of("editMessageText")
	found := false
	for _, e := range edits {
		if e.body["text"] == retryingText && e.body["message_id"] == float64(buttonsID) {
			found = true
		}
	}
	if !found {
		t.Fatalf("the buttons message must say Retrying: %+v", edits)
	}
	// The button works once.
	h.HandleCallback(context.Background(), tap(t, RetryGo, buttonsID, ""))
	tg.waitFor(t, "stale answer", func() bool {
		for _, a := range tg.of("answerCallbackQuery") {
			if a.body["text"] == "This retry is no longer active" {
				return true
			}
		}
		return false
	})
}

func TestNoRetryButtonWithoutRerunOrOnProfileTopics(t *testing.T) {
	for name, c := range map[string]struct {
		caps []string
		kind TopicKind
	}{
		"no rerun capability": {[]string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery}, TopicOrdinary},
		"profile topic":       {retryCaps, TopicProfile},
	} {
		tg := newFakeTG(t)
		svc := newFakeSvc(t, c.caps)
		svc.submit = failThenAnswer(svc)
		h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: c.kind})
		h.Dispatch(context.Background(), userMessage(t, 42, "hi"))
		tg.waitFor(t, name, func() bool { return len(svc.requests("release")) == 1 })
		h.Wait()
		for _, s := range tg.of("sendMessage") {
			if s.body["text"] == RetryPrompt {
				t.Errorf("%s: no retry button expected", name)
			}
		}
	}
}

func TestRetryDismissDeletesTheButtons(t *testing.T) {
	tg := newFakeTG(t)
	h, _ := newHandler(tg, newFakeSvc(t, retryCaps), TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Presenter.Retries.put(-100, 900, RetryRequest{ThreadID: thread, Chat: -100})
	h.HandleCallback(context.Background(), tap(t, RetryDismiss, 900, ""))
	tg.waitFor(t, "delete", func() bool { return len(tg.of("deleteMessage")) == 1 })
	if _, ok := h.Presenter.Retries.take(-100, 900); ok {
		t.Fatal("dismiss drops the request")
	}
}
