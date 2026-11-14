package bot

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/intake"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// mediaHandler is newHandler with attachments saved under a temp store.
func mediaHandler(t *testing.T, tg *fakeTG, svc *fakeSvc) (*Handler, string) {
	t.Helper()
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	store := t.TempDir()
	h.Attachments = &intake.Collector{
		Files: h.Telegram, Store: intake.DirStore{Root: store}, Log: quiet,
		Sleep: func(context.Context, time.Duration) error { return nil },
	}
	return h, store
}

func docMessage(t *testing.T, id int64, fileID, extra string) *telegram.Message {
	return msg(t, fmt.Sprintf(`{"message_id":%d,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"media_group_id":"g","document":{"file_id":%q,"file_name":"%s.pdf","mime_type":"application/pdf","file_size":3}%s}`, id, fileID, fileID, extra))
}

var mediaCaps = []string{engine.CapOrdinarySubmit, engine.CapComposeUserText, engine.CapRecordDelivery}

func TestDocumentAlbumIsSavedComposedAndSubmitted(t *testing.T) {
	tg := newFakeTG(t)
	tg.files = map[string]string{"docs/d1": "one", "docs/d2": "two"}
	svc := newFakeSvc(t, mediaCaps)
	h, store := mediaHandler(t, tg, svc)
	head := docMessage(t, 42, "d1", `,"caption":" read both "`)
	h.Dispatch(context.Background(), Incoming{Message: head, Grouped: []*telegram.Message{docMessage(t, 43, "d2", "")}})
	tg.waitFor(t, "the submit", func() bool { return len(svc.requests("submit")) == 1 })

	compose := svc.requests("compose_user_text")[0]
	docs, _ := compose["documents"].([]any)
	if compose["text"] != "read both" || len(docs) != 2 {
		t.Fatalf("unexpected compose %v", compose)
	}
	for i, name := range []string{"d1.pdf", "d2.pdf"} {
		d := docs[i].(map[string]any)
		want := filepath.Join(store, "-100", fmt.Sprintf("%d_%s", 42+i, name))
		if d["file_name"] != name || d["path"] != want {
			t.Fatalf("document %d: %v, want path %s", i, d, want)
		}
		if b, err := os.ReadFile(want); err != nil || string(b) != []string{"one", "two"}[i] {
			t.Fatalf("saved file %s: %q %v", want, b, err)
		}
	}
	sub := svc.requests("submit")[0]
	if imgs, _ := sub["images"].([]any); sub["text"] != "composed:read both" || len(imgs) != 0 {
		t.Fatalf("unexpected submit %v", sub)
	}
}

// Images and audio need their Rust operations: without the capability the
// message is refused before any download.
func TestImagesAndAudioNeedTheirRustCapabilities(t *testing.T) {
	for capability, extra := range map[string]string{
		engine.CapPrepareImage:    `"photo":[{"file_id":"p","width":10,"height":10}]`,
		engine.CapTranscribeAudio: `"voice":{"file_id":"v","mime_type":"audio/ogg"}`,
	} {
		tg := newFakeTG(t)
		svc := newFakeSvc(t, mediaCaps)
		h, _ := mediaHandler(t, tg, svc)
		m := msg(t, `{"message_id":42,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},`+extra+`}`)
		// An image in any album member needs the capability for the album.
		h.Dispatch(context.Background(), Incoming{Message: docMessage(t, 41, "d1", ""), Grouped: []*telegram.Message{m}})
		h.Queues.Wait()
		sends := tg.of("sendMessage")
		if len(sends) != 1 || !strings.Contains(sends[0].body["text"].(string), capability) {
			t.Fatalf("%s: unexpected notices %v", capability, sends)
		}
		if len(svc.requests("compose_user_text")) != 0 || len(tg.of("getFile")) != 0 {
			t.Fatalf("%s: nothing is downloaded or composed: %v", capability, svc.ops())
		}
	}
}

func TestUnreadableAndEmptyAttachments(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, mediaCaps)
	h, _ := mediaHandler(t, tg, svc)
	// Too large to load: the Rust bot answers that it could not read it.
	big := msg(t, `{"message_id":42,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"document":{"file_id":"x","mime_type":"application/pdf","file_size":99999999}}`)
	h.Dispatch(context.Background(), Incoming{Message: big})
	tg.waitFor(t, "notice", func() bool { return len(tg.of("sendMessage")) == 1 })
	if text := tg.of("sendMessage")[0].body["text"]; text != intake.UnreadableAttachmentReply {
		t.Fatalf("unexpected notice %v", text)
	}
	// A document without a MIME type is not an attachment intake reads.
	bare := msg(t, `{"message_id":43,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"document":{"file_id":"y"}}`)
	h.Dispatch(context.Background(), Incoming{Message: bare})
	h.Queues.Wait()
	if len(tg.of("sendMessage")) != 1 || len(svc.requests("compose_user_text")) != 0 || len(svc.requests("submit")) != 0 {
		t.Fatalf("an empty message is ignored: %v %v", tg.of("sendMessage"), svc.ops())
	}
}

func TestFailedDownloadOffersToLoadTheMessageAgain(t *testing.T) {
	tg := newFakeTG(t)
	tg.files = map[string]string{"docs/d1": "one"}
	tg.failing = map[string]bool{"docs/d1": true}
	svc := newFakeSvc(t, mediaCaps)
	h, _ := mediaHandler(t, tg, svc)
	h.Dispatch(context.Background(), Incoming{Message: docMessage(t, 42, "d1", "")})
	h.Queues.Wait()
	offer := tg.of("sendMessage")[0].body
	if !strings.Contains(offer["text"].(string), "couldn't download your file") || !strings.Contains(fmtAny(offer["reply_markup"]), RetryGo) {
		t.Fatalf("unexpected offer %v", offer)
	}
	if n := len(tg.of("file:docs/d1")); n != intake.DownloadAttempts {
		t.Fatalf("want %d tries, got %d", intake.DownloadAttempts, n)
	}
	if len(svc.requests("compose_user_text")) != 0 || len(svc.requests("submit")) != 0 {
		t.Fatal("no turn runs without the attachment")
	}

	tg.mu.Lock()
	tg.failing = nil
	buttons := tg.lastSent
	tg.mu.Unlock()
	data := RetryGo
	q := &telegram.CallbackQuery{ID: "q", From: telegram.User{ID: 7}, Data: &data,
		Message: msg(t, fmt.Sprintf(`{"message_id":%d,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5}`, buttons))}
	h.HandleCallback(context.Background(), q)
	tg.waitFor(t, "the submit", func() bool { return len(svc.requests("submit")) == 1 })
	if edits := tg.of("editMessageText"); len(edits) == 0 || edits[0].body["text"] != loadingAgainText {
		t.Fatalf("the buttons must say it is loading again: %+v", edits)
	}
	h.HandleCallback(context.Background(), q)
	tg.waitFor(t, "the stale answer", func() bool {
		for _, c := range tg.of("answerCallbackQuery") {
			if c.body["text"] == "This retry is no longer active" {
				return true
			}
		}
		return false
	})
}

// A rich message (no text) runs as a turn with its flattened text, and a
// reply to one quotes that text.
func TestRichMessageRunsWithItsFlattenedText(t *testing.T) {
	tg := newFakeTG(t)
	svc := newFakeSvc(t, mediaCaps)
	h, _ := newHandler(tg, svc, TopicInfo{ThreadID: thread, Kind: TopicOrdinary})
	rich := `"rich_message":{"blocks":[{"type":"paragraph","text":"August:"},{"type":"list","items":[{"label":"•","blocks":[{"type":"paragraph","text":"rent"}]}]}]}`
	base := `"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false}`
	h.Dispatch(context.Background(), Incoming{Message: msg(t, `{"message_id":42,`+base+`,`+rich+`}`)})
	h.Dispatch(context.Background(), Incoming{Message: msg(t, `{"message_id":43,`+base+`,"text":"total?","reply_to_message":{"message_id":41,"chat":{"id":-100,"type":"supergroup"},`+rich+`}}`)})
	h.Queues.Wait()
	c := svc.requests("compose_user_text")
	if len(c) != 2 || c[0]["text"] != "August:\n\n• rent" {
		t.Fatalf("unexpected compose %v", c)
	}
	if r, _ := c[1]["reply_to"].(map[string]any); r["text"] != "August:\n\n• rent" {
		t.Fatalf("the reply must quote the rich text: %v", c[1])
	}
	for _, s := range tg.of("sendMessage") {
		if strings.Contains(fmtAny(s.body["text"]), "cannot handle") {
			t.Fatalf("rich messages are supported: %v", s.body)
		}
	}
}

type cmdLog struct {
	mu   sync.Mutex
	reqs []CommandRequest
}

func (c *cmdLog) HandleCommand(_ context.Context, req CommandRequest) bool {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.reqs = append(c.reqs, req)
	return true
}

type fakePreparer struct{}

func (fakePreparer) PrepareImage(context.Context, intake.ImageFile) (intake.PreparedImage, error) {
	return intake.PreparedImage{Delivery: engine.ImageDelivery{Type: "saved_only", Reason: "r"}}, nil
}

// The loaded-media flag comes from what intake loaded across the album:
// a photo in a later member counts, a photo whose download failed does not.
func TestCommandRequestCarriesLoadedMediaAndSynthetic(t *testing.T) {
	tg := newFakeTG(t)
	tg.files = map[string]string{"docs/d1": "one", "docs/p1": "img"}
	svc := newFakeSvc(t, append(mediaCaps, engine.CapPrepareImage))
	h, _ := mediaHandler(t, tg, svc)
	h.Attachments.(*intake.Collector).Images = fakePreparer{}
	log := &cmdLog{}
	h.Commands = log
	base := `"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false}`
	photo := func(id int64, file string) *telegram.Message {
		return msg(t, fmt.Sprintf(`{"message_id":%d,%s,"media_group_id":"g","photo":[{"file_id":%q,"width":1,"height":1}]}`, id, base, file))
	}
	// Album: the head is a document with a /status caption, the photo is
	// the second member.
	h.Dispatch(context.Background(), Incoming{Message: docMessage(t, 42, "d1", `,"caption":"/status"`), Grouped: []*telegram.Message{photo(43, "p1")}})
	h.Queues.Wait()
	// The photo's download fails for good (404): nothing loaded.
	h.Dispatch(context.Background(), Incoming{Message: docMessage(t, 44, "d1", `,"caption":"/status"`), Grouped: []*telegram.Message{photo(45, "gone")}})
	h.Queues.Wait()
	synthetic := userMessage(t, 46, "/status")
	synthetic.Synthetic = true
	h.Dispatch(context.Background(), synthetic)
	h.Queues.Wait()

	log.mu.Lock()
	defer log.mu.Unlock()
	if len(log.reqs) != 3 {
		t.Fatalf("want 3 hook calls, got %+v", log.reqs)
	}
	if !log.reqs[0].HasLoadedImagesOrAudio || log.reqs[1].HasLoadedImagesOrAudio || log.reqs[2].HasLoadedImagesOrAudio {
		t.Fatalf("loaded flags: %v %v %v", log.reqs[0].HasLoadedImagesOrAudio, log.reqs[1].HasLoadedImagesOrAudio, log.reqs[2].HasLoadedImagesOrAudio)
	}
	if log.reqs[0].Synthetic || log.reqs[1].Synthetic || !log.reqs[2].Synthetic {
		t.Fatal("only the dispatched message is synthetic")
	}
	if n := len(tg.of("file:docs/p1")) + len(tg.of("file:docs/gone")); n != 2 {
		t.Fatalf("each photo is downloaded once (no retry on 404): %d", n)
	}
}

// With a loaded image the bot's own /new does not run (Rust reads no
// command from such a message).
func TestBuiltinCommandsSkipMessagesWithLoadedImages(t *testing.T) {
	tg := newFakeTG(t)
	tg.files = map[string]string{"docs/p1": "img"}
	svc := newFakeSvc(t, append(mediaCaps, engine.CapPrepareImage, engine.CapResetThread))
	h, _ := mediaHandler(t, tg, svc)
	h.Attachments.(*intake.Collector).Images = fakePreparer{}
	m := msg(t, `{"message_id":42,"chat":{"id":7,"type":"private"},"from":{"id":7,"is_bot":false},"caption":"/new","photo":[{"file_id":"p1","width":1,"height":1}]}`)
	h.Dispatch(context.Background(), Incoming{Message: m})
	h.Queues.Wait()
	if len(svc.requests("reset_thread")) != 0 || len(svc.requests("compose_user_text")) != 1 {
		t.Fatalf("the photo runs as a turn: %v", svc.ops())
	}
}
