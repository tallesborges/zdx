package bot

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

func tap(t *testing.T, data string, messageID int64, markup string) *telegram.CallbackQuery {
	raw := fmt.Sprintf(`{"message_id":%d,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5`, messageID)
	if markup != "" {
		raw += `,"reply_markup":` + markup
	}
	raw += "}"
	return &telegram.CallbackQuery{ID: "cb", From: telegram.User{ID: 7}, Message: msg(t, raw), Data: &data}
}

const linkMarkup = `{"inline_keyboard":[[{"text":"↗ Open artifact","url":"https://t.me/x/app?startapp=a_k_t"}],[{"text":"Run tests","callback_data":"fu:0"}],[{"text":"✕ Dismiss","callback_data":"fu:x"}]]}`

func lastMarkup(t *testing.T, tg *fakeTG) string {
	t.Helper()
	edits := tg.of("editMessageReplyMarkup")
	if len(edits) == 0 {
		t.Fatal("no keyboard edit")
	}
	return fmt.Sprint(edits[len(edits)-1].body["reply_markup"])
}

func TestFollowupTapSendsTheSuggestionAndKeepsLinks(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery})
	svc.submit = func(sc svcConn) {
		write(sc.conn, map[string]any{"reply": "submitted", "thread_id": thread, "input_id": 2, "queued_ahead": 0, "in_turn": true})
		sc.conn.Close()
	}
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Presenter.Deliverer.Followups.Register(-100, 900, []string{"Run tests"})

	h.HandleCallback(context.Background(), tap(t, "fu:0", 900, linkMarkup))
	tg.waitFor(t, "submit", func() bool { return len(svc.requests("submit")) == 1 })
	if text := svc.requests("submit")[0]["text"]; text != "composed:Run tests" {
		t.Fatalf("the suggestion is the next user message: %v", text)
	}
	kb := lastMarkup(t, tg)
	if !strings.Contains(kb, "Open artifact") || !strings.Contains(kb, "▶️ Run tests") || !strings.Contains(kb, render.FollowupChosen) || strings.Contains(kb, "Dismiss") {
		t.Fatalf("unexpected keyboard %s", kb)
	}
	if len(tg.of("editMessageText")) != 0 {
		t.Fatal("a tap never edits the text")
	}

	// The same buttons again: no longer active, and nothing is submitted.
	h.HandleCallback(context.Background(), tap(t, "fu:0", 900, linkMarkup))
	tg.waitFor(t, "answer", func() bool { return len(tg.of("answerCallbackQuery")) == 2 })
	if a := tg.of("answerCallbackQuery")[1].body["text"]; a != "These suggestions are no longer active" {
		t.Fatalf("unexpected answer %v", a)
	}
	if len(svc.requests("submit")) != 1 {
		t.Fatal("a stale tap submits nothing")
	}
}

func TestFollowupDismissKeepsOnlyLinkRows(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, nil)
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	h.Presenter.Deliverer.Followups.Register(-100, 900, []string{"Run tests"})
	h.HandleCallback(context.Background(), tap(t, "fu:x", 900, linkMarkup))
	tg.waitFor(t, "answer", func() bool { return len(tg.of("answerCallbackQuery")) == 1 })
	kb := lastMarkup(t, tg)
	if !strings.Contains(kb, "Open artifact") || strings.Contains(kb, "Run tests") {
		t.Fatalf("unexpected keyboard %s", kb)
	}
	// Without link rows the keyboard is emptied, not left as is.
	h.HandleCallback(context.Background(), tap(t, "fu:x", 901, ""))
	tg.waitFor(t, "answer", func() bool { return len(tg.of("answerCallbackQuery")) == 2 })
	if kb := lastMarkup(t, tg); kb != "map[inline_keyboard:[]]" {
		t.Fatalf("unexpected keyboard %s", kb)
	}
	if len(svc.ops()) != 0 {
		t.Fatal("dismiss calls no engine op")
	}
}

func TestInertFollowupOnlyAnswers(t *testing.T) {
	tg := newFakeTG(t)
	h, _ := newHandler(tg, newFakeSvc(t, nil), TopicInfo{})
	h.HandleCallback(context.Background(), tap(t, render.FollowupChosen, 900, ""))
	tg.waitFor(t, "answer", func() bool { return len(tg.of("answerCallbackQuery")) == 1 })
	if len(tg.of("editMessageReplyMarkup")) != 0 {
		t.Fatal("the inert button changes nothing")
	}
}

// A reply's follow-ups are registered under the message that carries them.
func TestDeliveryRegistersFollowups(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	d := &Deliverer{Telegram: tg.client(), Store: store, Followups: &Followups{}, Log: quiet}
	status := int64(77)
	d.Deliver(context.Background(), thread, nil, target(), &status, plan(render.Reply{HTML: "x", Followups: []string{"a", "b"}}))
	id := *store.final().Items[0].MessageID
	if item, ok := d.Followups.take(-100, id, 1); !ok || item != "b" {
		t.Fatalf("follow-ups must be registered under message %d", id)
	}
}

type fakeArtifacts struct {
	mu    sync.Mutex
	path  string
	err   error
	calls []string
	block chan struct{}
}

func (f *fakeArtifacts) FindArtifact(_ context.Context, threadID, key string) (string, string, error) {
	f.mu.Lock()
	f.calls = append(f.calls, threadID+"/"+key)
	block := f.block
	f.mu.Unlock()
	if block != nil {
		<-block
	}
	return f.path, "", f.err
}

const cardKey = "0123456789abcdef"

func TestSendFileUploadsAsAReplyToTheCardAndRecordsIt(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, []string{engine.CapRecordDelivery})
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: "effective-thread", Kind: TopicOrdinary})
	page := writeFile(t, "page.html", []byte("<html></html>"))
	arts := &fakeArtifacts{path: page}
	h.Artifacts = arts
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "record", func() bool { return len(svc.requests("record_delivery")) == 1 })
	if a := tg.of("answerCallbackQuery"); len(a) != 1 || a[0].body["text"] != "Sending the file…" {
		t.Fatalf("answers %+v", a)
	}
	docs := tg.of("sendDocument")
	if len(docs) != 1 || docs[0].body["reply_to_message_id"] != "500" || docs[0].body["message_thread_id"] != "5" || docs[0].body["document"] != "page.html|text/html" {
		t.Fatalf("unexpected upload %+v", docs)
	}
	if arts.calls[0] != "effective-thread/"+cardKey {
		t.Fatalf("lookup must use the effective thread: %v", arts.calls)
	}
	rec := svc.requests("record_delivery")[0]
	item := rec["record"].(map[string]any)["items"].([]any)[0].(map[string]any)
	if rec["thread_id"] != "effective-thread" || item["status"] != "sent" || item["path"] != page {
		t.Fatalf("unexpected record %v", rec)
	}
}

func TestStaleSendFileTapUploadsNothingAndNotifiesOnce(t *testing.T) {
	tg := newFakeTG(t)
	stale := `{"ok":false,"error_code":400,"description":"Bad Request: query is too old and response timeout expired or query ID is invalid"}`
	tg.queue("answerCallbackQuery", stale)
	tg.queue("answerCallbackQuery", stale)
	h, _ := newHandler(tg, newFakeSvc(t, []string{engine.CapRecordDelivery}), TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	arts := &fakeArtifacts{path: writeFile(t, "p.html", []byte("x"))}
	h.Artifacts = arts
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "second answer", func() bool { return len(tg.of("answerCallbackQuery")) == 2 })
	if n := len(tg.of("sendMessage")); n != 1 {
		t.Fatalf("one notice per card, got %d", n)
	}
	if notice := tg.of("sendMessage")[0].body; !strings.Contains(notice["text"].(string), "reached the bot late") || notice["reply_to_message_id"] != float64(500) {
		t.Fatalf("unexpected notice %v", notice)
	}
	if len(tg.of("sendDocument")) != 0 || len(arts.calls) != 0 {
		t.Fatal("a stale tap uploads and looks up nothing")
	}
}

func TestSendFileRefusals(t *testing.T) {
	tg := newFakeTG(t)
	h, _ := newHandler(tg, newFakeSvc(t, []string{engine.CapRecordDelivery}), TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	arts := &fakeArtifacts{err: errors.New("this file is no longer part of this chat")}
	h.Artifacts = arts
	h.HandleCallback(context.Background(), tap(t, "sf:NOTHEX", 500, ""))
	tg.waitFor(t, "answer", func() bool { return len(tg.of("answerCallbackQuery")) == 1 })
	if a := tg.of("answerCallbackQuery")[0].body["text"]; a != "This button no longer works" {
		t.Fatalf("unexpected answer %v", a)
	}
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if text := tg.of("sendMessage")[0].body["text"]; text != "⚠️ Not sent: this file is no longer part of this chat" {
		t.Fatalf("unexpected notice %v", text)
	}
	if len(tg.of("sendDocument")) != 0 {
		t.Fatal("nothing may be uploaded")
	}
}

func TestSendFileWithoutDeliveryRecordingUploadsNothing(t *testing.T) {
	tg := newFakeTG(t)
	h, _ := newHandler(tg, newFakeSvc(t, nil), TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	arts := &fakeArtifacts{path: writeFile(t, "p.html", []byte("x"))}
	h.Artifacts = arts
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if text := tg.of("sendMessage")[0].body["text"].(string); !strings.Contains(text, engine.CapRecordDelivery) {
		t.Fatalf("unexpected notice %s", text)
	}
	if len(tg.of("sendDocument")) != 0 || len(arts.calls) != 0 {
		t.Fatal("nothing may be looked up or uploaded")
	}
}

func TestSendFileTapWhileSendingIsRefused(t *testing.T) {
	tg := newFakeTG(t)
	h, _ := newHandler(tg, newFakeSvc(t, []string{engine.CapRecordDelivery}), TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	arts := &fakeArtifacts{path: writeFile(t, "p.html", []byte("x")), block: make(chan struct{})}
	h.Artifacts = arts
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "lookup", func() bool { arts.mu.Lock(); defer arts.mu.Unlock(); return len(arts.calls) == 1 })
	h.HandleCallback(context.Background(), tap(t, render.SendFilePrefix+cardKey, 500, ""))
	tg.waitFor(t, "second answer", func() bool { return len(tg.of("answerCallbackQuery")) == 2 })
	if a := tg.of("answerCallbackQuery")[1].body["text"]; a != "Already sending this file" {
		t.Fatalf("unexpected answer %v", a)
	}
	close(arts.block)
	tg.waitFor(t, "upload", func() bool { return len(tg.of("sendDocument")) == 1 })
}

func TestServiceArtifactsFindsByKeyThroughRustViews(t *testing.T) {
	svc := newFakeSvc(t, []string{engine.CapArtifactViews})
	svc.view = func(req map[string]any) map[string]any {
		if req["op"] == engine.ViewArtifactResolve {
			if req["path"] == "/abs/gone.pdf" {
				return map[string]any{"reply": "error", "code": "artifact_not_found", "message": "Artifact not found"}
			}
			return map[string]any{"reply": "view", "body": map[string]any{"path": req["path"], "mime": "application/pdf", "filename": "a.pdf"}}
		}
		return map[string]any{"reply": "view", "body": map[string]any{"artifacts": []any{
			map[string]any{"key": "k1", "name": "a.pdf", "path": "/abs/a.pdf", "exists": true},
			map[string]any{"key": "k2", "name": "old.pdf", "path": "/abs/old.pdf", "exists": false},
			map[string]any{"key": "k3", "name": "gone.pdf", "path": "/abs/gone.pdf", "exists": true},
		}}}
	}
	a := ServiceArtifacts{Engine: engine.NewClient(svc.socket)}
	ctx := context.Background()
	if path, name, err := a.FindArtifact(ctx, "t", "k1"); err != nil || path != "/abs/a.pdf" || name != "a.pdf" {
		t.Fatalf("k1: %q %q %v", path, name, err)
	}
	for key, want := range map[string]string{
		"k2": "old.pdf is no longer on the Mac",
		"k3": "gone.pdf is no longer on the Mac",
		"k9": "this file is no longer part of this chat",
	} {
		if _, _, err := a.FindArtifact(ctx, "t", key); err == nil || err.Error() != want {
			t.Errorf("%s: want %q, got %v", key, want, err)
		}
	}
	svc.setCaps()
	if _, _, err := a.FindArtifact(ctx, "t", "k1"); err == nil || !strings.Contains(err.Error(), "cannot look up") {
		t.Fatalf("without artifact_views: %v", err)
	}
}
