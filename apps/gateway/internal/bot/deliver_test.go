package bot

import (
	"bytes"
	"context"
	"errors"
	"image"
	"image/jpeg"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
)

type memStore struct {
	mu       sync.Mutex
	records  []engine.DeliveryRecord
	err      error
	notReady error
	// summary is returned for records with problems.
	summary string
	// holds are the holds the records named.
	holds []*engine.Hold
}

func (m *memStore) DeliveryReady(context.Context) error { return m.notReady }

func (m *memStore) RecordDelivery(_ context.Context, _ string, hold *engine.Hold, r engine.DeliveryRecord) (string, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.holds = append(m.holds, hold)
	cp := r
	cp.Items = append([]engine.DeliveryItem(nil), r.Items...)
	m.records = append(m.records, cp)
	if m.err != nil {
		return "", m.err
	}
	if r.HasProblems() {
		return m.summary, nil
	}
	return "", nil
}

func (m *memStore) final() engine.DeliveryRecord { return m.records[len(m.records)-1] }

func deliver(t *testing.T, tg *fakeTG, store *memStore, plan render.Plan) error {
	t.Helper()
	d := &Deliverer{Telegram: tg.client(), Store: store, Followups: &Followups{}, Log: quiet}
	status := int64(77)
	return d.Deliver(context.Background(), thread, nil, target(), &status, plan)
}

func target() ReplyTarget {
	return ReplyTarget{Chat: -100, Topic: ptr(int64(5)), ReplyTo: ptr(int64(42))}
}

func statuses(r engine.DeliveryRecord) []string {
	out := make([]string, len(r.Items))
	for i, it := range r.Items {
		out[i] = it.Status
	}
	return out
}

func plan(r render.Reply) render.Plan {
	return render.PlanReply(r, render.Options{MiniAppBase: "https://t.me/x/app", ThreadID: thread, TopicThreadID: thread})
}

func writeFile(t *testing.T, name string, data []byte) string {
	p := filepath.Join(t.TempDir(), name)
	if err := os.WriteFile(p, data, 0o600); err != nil {
		t.Fatal(err)
	}
	return p
}

func smallJPEG(t *testing.T) []byte {
	var buf bytes.Buffer
	jpeg.Encode(&buf, image.NewGray(image.Rect(0, 0, 10, 10)), nil)
	return buf.Bytes()
}

func TestTextAndDocumentGoOutAsPlannedAndAreRecorded(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	doc := writeFile(t, "report.pdf", []byte("%PDF-1.4"))
	deliver(t, tg, store, plan(render.Reply{HTML: "hi", Files: []render.File{{Path: doc, ArtifactKey: "k1", Linkable: true, Method: render.MethodDocument}}}))
	if len(store.records) != 2 || store.records[0].ID != store.records[1].ID {
		t.Fatalf("want pending and final with one id: %+v", store.records)
	}
	for _, it := range store.records[0].Items {
		if it.Status != engine.DeliveryPending {
			t.Fatalf("pending record must be all pending: %+v", store.records[0])
		}
	}
	f := store.final()
	if got := statuses(f); len(got) != 2 || got[0] != "sent" || got[1] != "sent" || *f.Items[1].Path != doc {
		t.Fatalf("unexpected record %+v", f.Items)
	}
	docs := tg.of("sendDocument")
	if len(docs) != 1 || len(tg.of("sendMessage")) != 1 || len(tg.of("deleteMessage")) != 1 {
		t.Fatalf("calls %+v", tg.calls)
	}
	if !strings.Contains(docs[0].body["reply_markup"].(string), "startapp=a_k1_") {
		t.Fatalf("the document carries its Open artifact link: %v", docs[0].body)
	}
}

func TestShortTextAndOnePhotoGoAsOneCaptionedMessage(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	photo := writeFile(t, "shot.jpg", smallJPEG(t))
	deliver(t, tg, store, plan(render.Reply{HTML: "look", Files: []render.File{{Path: photo, Method: render.MethodPhoto}}}))
	photos := tg.of("sendPhoto")
	if len(photos) != 1 || len(tg.of("sendMessage")) != 0 || !strings.HasPrefix(photos[0].body["caption"].(string), "look") {
		t.Fatalf("calls %+v", tg.calls)
	}
	f := store.final()
	if got := statuses(f); got[0] != "sent" || got[1] != "sent" || *f.Items[0].MessageID != *f.Items[1].MessageID {
		t.Fatalf("both items settle on one message: %+v", f.Items)
	}
}

func TestAPhotoNeedingPreparationFailsAndTheTextStillGoesOut(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	png := writeFile(t, "shot.png", []byte("\x89PNG\r\n\x1a\n0000"))
	deliver(t, tg, store, plan(render.Reply{HTML: "look", Files: []render.File{{Path: png, Method: render.MethodPhoto}}}))
	if len(tg.of("sendPhoto")) != 0 || len(tg.of("sendMessage")) != 1 {
		t.Fatalf("nothing may be uploaded; the text goes alone: %+v", tg.calls)
	}
	f := store.final()
	if got := statuses(f); got[0] != "sent" || got[1] != "failed" || !strings.Contains(*f.Items[1].Error, "needs resizing") {
		t.Fatalf("unexpected record %+v", f.Items)
	}
}

func TestVoiceAndAudioUseTheirMethods(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	voice := writeFile(t, "note.ogg", []byte("OggS"))
	song := writeFile(t, "song.mp3", []byte("ID3"))
	deliver(t, tg, store, plan(render.Reply{Files: []render.File{{Path: voice, Method: render.MethodVoice}, {Path: song, Method: render.MethodAudio}}}))
	v, a := tg.of("sendVoice"), tg.of("sendAudio")
	if len(v) != 1 || len(a) != 1 || v[0].body["voice"] != "note.ogg|audio/ogg" {
		t.Fatalf("calls %+v", tg.calls)
	}
	if got := statuses(store.final()); got[0] != "sent" || got[1] != "sent" {
		t.Fatalf("got %v", got)
	}
}

func TestHTMLFileWithALinkIsACardAndLinkOnly(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	page := writeFile(t, "page.html", []byte("<html></html>"))
	deliver(t, tg, store, plan(render.Reply{Files: []render.File{{Path: page, ArtifactKey: "k2", Linkable: true, HTML: true, Size: 13, Method: render.MethodDocument}}}))
	if len(tg.of("sendDocument")) != 0 || len(tg.of("sendMessage")) != 1 {
		t.Fatalf("a card is a text message, nothing uploaded: %+v", tg.calls)
	}
	f := store.final()
	if !f.Items[0].LinkOnly || f.Items[0].Status != "sent" {
		t.Fatalf("unexpected item %+v", f.Items[0])
	}
}

func TestMissingFileFailsWithoutASend(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	deliver(t, tg, store, plan(render.Reply{HTML: "x", Files: []render.File{{Path: "/nope/a.pdf", Missing: true, Method: render.MethodDocument}}}))
	f := store.final()
	if got := statuses(f); got[0] != "sent" || got[1] != "failed" || *f.Items[1].Error != render.FileNotFound {
		t.Fatalf("unexpected record %+v", f.Items)
	}
}

func TestSplitReplyRecordsEachPart(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	long := strings.Repeat("word ", 2000)
	deliver(t, tg, store, plan(render.Reply{HTML: long}))
	f := store.final()
	if len(f.Items) < 2 || f.Items[0].Part == nil || f.Items[0].Part.Index != 1 || f.Items[0].Part.Total != len(f.Items) {
		t.Fatalf("unexpected parts %+v", f.Items)
	}
	if n := len(tg.of("sendMessage")); n != len(f.Items) {
		t.Fatalf("one message per part, got %d", n)
	}
}

func TestAmbiguousReplyIsUnknownAndNeverResent(t *testing.T) {
	tg := newFakeTG(t)
	tg.queue("sendMessage", "not json")
	store := &memStore{}
	deliver(t, tg, store, plan(render.Reply{HTML: "x"}))
	if got := statuses(store.final()); got[0] != engine.DeliveryUnknown {
		t.Fatalf("got %v", got)
	}
	if n := len(tg.of("sendMessage")); n != 1 {
		t.Fatalf("an ambiguous send must not repeat, got %d", n)
	}
}

func TestInvalidReplyTargetIsDroppedOnce(t *testing.T) {
	tg := newFakeTG(t)
	tg.queue("sendMessage", `{"ok":false,"error_code":400,"description":"Bad Request: message to be replied not found"}`)
	store := &memStore{}
	deliver(t, tg, store, plan(render.Reply{HTML: "x"}))
	sends := tg.of("sendMessage")
	if len(sends) != 2 || sends[0].body["reply_to_message_id"] != float64(42) || sends[1].body["reply_to_message_id"] != nil {
		t.Fatalf("unexpected sends %+v", sends)
	}
	if got := statuses(store.final()); got[0] != engine.DeliverySent {
		t.Fatalf("got %v", got)
	}
}

func TestUnabsorbedFloodRefusalStopsTheRest(t *testing.T) {
	tg := newFakeTG(t)
	tg.queue("sendMessage", `{"ok":false,"error_code":429,"description":"Too Many Requests","parameters":{"retry_after":500}}`)
	store := &memStore{}
	doc := writeFile(t, "a.pdf", []byte("%PDF"))
	deliver(t, tg, store, plan(render.Reply{HTML: "a", Files: []render.File{{Path: doc, Method: render.MethodDocument}}}))
	f := store.final()
	if got := statuses(f); got[0] != "failed" || got[1] != "failed" || !strings.HasPrefix(*f.Items[1].Error, "not attempted after a flood refusal") {
		t.Fatalf("got %+v", f.Items)
	}
	if len(tg.of("sendDocument")) != 0 {
		t.Fatal("nothing more may be sent after the refusal")
	}
}

// A runtime recording failure is logged and the reply still goes out, as
// response.rs append_delivery does. The outcome record gets one retry with
// the same id; the reply is never sent again.
func TestRecordingFailureDoesNotStopTheReply(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{err: errors.New("down")}
	if err := deliver(t, tg, store, plan(render.Reply{HTML: "x"})); err != nil {
		t.Fatal(err)
	}
	if len(tg.of("sendMessage")) != 1 || len(store.records) != 3 {
		t.Fatalf("the reply must still go out; records %d", len(store.records))
	}
	if store.records[1].ID != store.records[0].ID || store.records[2].ID != store.records[0].ID || store.records[2].Items[0].Status != engine.DeliverySent {
		t.Fatalf("the outcome is retried under the same id: %+v", store.records)
	}
}

// A held follow-ups-only reply writes an empty pending record before it
// sends; a refused one sends nothing.
func TestHeldFollowupsOnlyReplyIsRecordedFirst(t *testing.T) {
	hold := &engine.Hold{TurnID: 4, Holder: "H"}
	p := plan(render.Reply{HTML: " ", Followups: []string{"Next"}})
	tg := newFakeTG(t)
	store := &memStore{}
	d := &Deliverer{Telegram: tg.client(), Store: store, Followups: &Followups{}, Log: quiet}
	if err := d.Deliver(context.Background(), "t", hold, ReplyTarget{Chat: -100}, nil, p); err != nil {
		t.Fatal(err)
	}
	if len(store.records) != 1 || len(store.records[0].Items) != 0 || store.holds[0] != hold || len(tg.of("sendMessage")) != 1 {
		t.Fatalf("records %+v sends %d", store.records, len(tg.of("sendMessage")))
	}
	tg = newFakeTG(t)
	refused := &memStore{err: &engine.ServiceError{Code: "conflict", Message: "not the holder"}}
	d = &Deliverer{Telegram: tg.client(), Store: refused, Followups: &Followups{}, Log: quiet}
	if err := d.Deliver(context.Background(), "t", hold, ReplyTarget{Chat: -100}, nil, p); !errors.Is(err, ErrDeliveryUnrecorded) || len(tg.calls) != 0 {
		t.Fatalf("err %v calls %d", err, len(tg.calls))
	}
}

// A service that cannot record deliveries at all gets no unrecorded reply.
func TestUnsupportedRecordingSendsNothing(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{err: &engine.ErrUnsupported{Capability: engine.CapRecordDelivery, Protocol: 2}}
	err := deliver(t, tg, store, plan(render.Reply{HTML: "x"}))
	if !errors.Is(err, ErrDeliveryUnrecorded) {
		t.Fatalf("want ErrDeliveryUnrecorded, got %v", err)
	}
	if len(tg.calls) != 0 {
		t.Fatalf("nothing may be sent or deleted: %+v", tg.calls)
	}
}

func TestEmptyPlanOnlyDeletesTheStatusAndPostsFollowups(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{}
	deliver(t, tg, store, plan(render.Reply{Followups: []string{"Run tests"}}))
	sends := tg.of("sendMessage")
	if len(store.records) != 0 || len(tg.of("deleteMessage")) != 1 || len(sends) != 1 || sends[0].body["text"] != render.SuggestedRepliesText {
		t.Fatalf("records=%v calls=%+v", store.records, tg.calls)
	}
}

func TestDebounceSendsTheNewestAfterTheInterval(t *testing.T) {
	now := mustTime()
	d := NewDebounce(StatusWaiting, now)
	d.Observe(ToolRunningStatus("bash"))
	if s, ok := d.Due(now); !ok || s != ToolRunningStatus("bash") {
		t.Fatal("the first change goes out at once")
	}
	d.Record(ToolRunningStatus("bash"), now, false)
	d.Observe(ToolRunningStatus("read"))
	d.Observe(StatusThinking)
	if _, ok := d.Due(now.Add(StatusDebounce - 1)); ok {
		t.Fatal("not before the interval")
	}
	if s, ok := d.Due(now.Add(StatusDebounce)); !ok || s != StatusThinking {
		t.Fatal("only the newest status is sent")
	}
	d.Record(StatusThinking, now.Add(StatusDebounce), true)
	if _, ok := d.NextFlush(); !ok {
		t.Fatal("a flood-wait refusal keeps the status pending")
	}
}

func mustTime() time.Time { return time.Unix(1_700_000_000, 0) }

// The pending record has problems too (all pending), but only the final
// record's summary is posted, once, after the reply.
func TestOnlyTheFinalSummaryIsPosted(t *testing.T) {
	tg := newFakeTG(t)
	store := &memStore{summary: "⚠️ Part of this reply was not delivered: <file>"}
	deliver(t, tg, store, plan(render.Reply{HTML: "x", Files: []render.File{{Path: "/nope.pdf", Missing: true, Method: render.MethodDocument}}}))
	sends := tg.of("sendMessage")
	if len(sends) != 2 || sends[1].body["text"] != "⚠️ Part of this reply was not delivered: &lt;file&gt;" || sends[1].body["reply_to_message_id"] != nil {
		t.Fatalf("want the reply then one summary: %+v", sends)
	}
	tg2 := newFakeTG(t)
	deliver(t, tg2, &memStore{summary: "never"}, plan(render.Reply{HTML: "x"}))
	if n := len(tg2.of("sendMessage")); n != 1 {
		t.Fatalf("a full delivery posts no summary, got %d sends", n)
	}
}

func TestPlanFromPartsMapsRustMedia(t *testing.T) {
	doc := writeFile(t, "r.html", []byte("<html>12345</html>"))
	parts := engine.ReplyParts{
		Text:      "**hi**",
		Followups: []string{"next"},
		Media: []engine.ReplyMedia{
			{Path: doc, ArtifactKey: "0123456789abcdef", Kind: "html", SendAs: engine.SendAsDocument, LexicalIsPhysical: true, Sendable: true},
			{Path: "relative.png", ArtifactKey: "fedcba9876543210", Kind: "image", SendAs: engine.SendAsPhoto, Sendable: false},
		},
	}
	p := PlanFromParts(parts, "https://t.me/x/app", target(), thread, nil)
	if len(p.Items) != 3 || p.Items[2].FailedBeforeSend != render.FileNotFound {
		t.Fatalf("unexpected items %+v", p.Items)
	}
	var card *render.Step
	for i := range p.Steps {
		if p.Steps[i].Kind == render.StepCard {
			card = &p.Steps[i]
		}
	}
	if card == nil || !strings.Contains(card.Text, "18 B") || !strings.Contains(p.Steps[0].Text, "<b>hi</b>") {
		t.Fatalf("unexpected steps %+v", p.Steps)
	}
	if len(p.Followups) != 1 {
		t.Fatal("follow-ups are kept")
	}
}

func TestUnknownSendAsFailsWithoutASend(t *testing.T) {
	doc := writeFile(t, "a.bin", []byte("x"))
	parts := engine.ReplyParts{Text: "t", Media: []engine.ReplyMedia{{Path: doc, ArtifactKey: "0123456789abcdef", Kind: "other", SendAs: "hologram", Sendable: true}}}
	tg := newFakeTG(t)
	store := &memStore{}
	deliver(t, tg, store, PlanFromParts(parts, "", target(), thread, nil))
	f := store.final()
	if got := statuses(f); got[1] != "failed" || *f.Items[1].Error != render.NoSendMethod {
		t.Fatalf("unexpected record %+v", f.Items)
	}
	for _, m := range []string{"sendDocument", "sendPhoto", "sendVoice", "sendAudio"} {
		if len(tg.of(m)) != 0 {
			t.Fatalf("nothing may be uploaded, got %s", m)
		}
	}
}

// With a hold (hold tokens) every record names the turn and its token, and
// nothing is sent until the pending record is acknowledged: one retry with
// the same id after no answer, none after a refusal.
func TestHeldTurnRecordsNameTheHoldAndAnUnacknowledgedPendingSendsNothing(t *testing.T) {
	hold := &engine.Hold{TurnID: 9, Holder: "H"}
	status := int64(77)

	tg := newFakeTG(t)
	store := &memStore{}
	d := &Deliverer{Telegram: tg.client(), Store: store, Followups: &Followups{}, Log: quiet}
	if err := d.Deliver(context.Background(), thread, hold, target(), &status, plan(render.Reply{HTML: "hi"})); err != nil {
		t.Fatal(err)
	}
	if len(store.holds) != 2 || *store.holds[0] != *hold || *store.holds[1] != *hold {
		t.Fatalf("pending and outcome name the hold: %v", store.holds)
	}

	for name, c := range map[string]struct {
		err     error
		records int
	}{
		"no answer: one retry with the same id": {engine.ErrNoAnswer, 2},
		"refused (conflict): no retry":          {&engine.ServiceError{Code: "conflict", Message: "not your hold"}, 1},
	} {
		tg = newFakeTG(t)
		store = &memStore{err: c.err}
		d = &Deliverer{Telegram: tg.client(), Store: store, Followups: &Followups{}, Log: quiet}
		err := d.Deliver(context.Background(), thread, hold, target(), &status, plan(render.Reply{HTML: "hi"}))
		if !errors.Is(err, ErrDeliveryUnrecorded) || len(tg.of("sendMessage")) != 0 || len(store.records) != c.records {
			t.Fatalf("%s: err=%v sends=%d records=%d", name, err, len(tg.of("sendMessage")), len(store.records))
		}
		if c.records == 2 && store.records[0].ID != store.records[1].ID {
			t.Fatalf("%s: the retry keeps the record id", name)
		}
	}
	// Without a hold the Rust bot's best effort stays: it sends.
	tg = newFakeTG(t)
	d.Telegram = tg.client()
	if err := d.Deliver(context.Background(), thread, nil, target(), &status, plan(render.Reply{HTML: "hi"})); err != nil || len(tg.of("sendMessage")) != 1 {
		t.Fatalf("best effort without hold tokens: %v %d", err, len(tg.of("sendMessage")))
	}
}
