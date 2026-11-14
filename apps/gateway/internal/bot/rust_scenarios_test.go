package bot

import (
	"bytes"
	"context"
	"encoding/json"
	"fmt"
	"image"
	stdpng "image/png"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"slices"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/intake"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// realRust is one isolated `zdx orchestrator serve` with a scripted mock
// provider, plus the Go bot wired to it: handler, presenter, delivery, and
// the renderer bridge. Telegram is the fake Bot API.
type realRust struct {
	t       *testing.T
	home    string
	eng     *engine.Client
	tg      *fakeTG
	h       *Handler
	root    string
	logPath string
	// newBridge builds the renderer bridge; startBridge runs one and
	// stopBridge stops it (one at a time).
	newBridge  func() *Bridge
	stopBridge func()
	// serve starts the service on home and returns its stop; stopService
	// stops the running one.
	serve       func() func()
	stopService func()
	// holds: the binary has hold_tokens (Rust holds v7.2).
	holds bool

	mu       sync.Mutex
	requests []string
}

// providerReply answers model request n (0-based); it may block on ctx.
type providerReply func(ctx context.Context, n int, w http.ResponseWriter)

func answer(text string) providerReply {
	return func(_ context.Context, _ int, w http.ResponseWriter) {
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, sse(text))
	}
}

func startRealRust(t *testing.T, reply providerReply) *realRust {
	t.Helper()
	bin := sandbox.RustBinary(t)
	home, err := os.MkdirTemp("", "zr")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(home) })
	// On failure, keep the service log in the test output (synthetic data:
	// temp home, mock provider, fake Bot API token).
	t.Cleanup(func() {
		if t.Failed() {
			if b, err := os.ReadFile(filepath.Join(home, "service.log")); err == nil {
				if len(b) > 16<<10 {
					b = b[len(b)-16<<10:]
				}
				t.Logf("service log tail:\n%s", b)
			}
		}
	})
	root := t.TempDir()
	r := &realRust{t: t, home: home, root: root, logPath: filepath.Join(home, "service.log"), stopBridge: func() {}}

	provider := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		body, _ := io.ReadAll(req.Body)
		r.mu.Lock()
		n := len(r.requests)
		r.requests = append(r.requests, string(body))
		r.mu.Unlock()
		reply(req.Context(), n, w)
	}))
	t.Cleanup(provider.Close)

	r.serve = func() func() {
		logFile, _ := os.OpenFile(r.logPath, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
		cmd := exec.Command(bin, "--root", root, "orchestrator", "serve")
		cmd.Dir = root
		cmd.Env = childEnv(os.Environ(), home, "ANTHROPIC_API_KEY=test-api-key", "ANTHROPIC_BASE_URL="+provider.URL)
		cmd.Stdout, cmd.Stderr = logFile, logFile
		socket := engine.SocketPath(home)
		os.Remove(socket)
		if err := cmd.Start(); err != nil {
			t.Fatal(err)
		}
		exited := make(chan error, 1)
		go func() { exited <- cmd.Wait() }()
		stop := sync.OnceFunc(func() { stopService(t, cmd, exited); logFile.Close() })
		waitListening(t, socket, exited, r.log)
		return stop
	}
	stop := r.serve()
	r.stopService = func() { stop() }
	t.Cleanup(func() { r.stopService() })
	socket := engine.SocketPath(home)
	r.eng = engine.NewClient(socket)
	if err := r.eng.RequireAll(context.Background(), append(realCaps, engine.CapRerun)...); err != nil {
		t.Fatalf("stale binary %s (rebuild it): %v", bin, err)
	}
	report, err := r.eng.StatusReport(context.Background(), nil)
	if err != nil {
		t.Fatal(err)
	}
	// With hold tokens the bot runs as in production: tokens on every
	// record and release, renderer and followers opted in to replay.
	r.holds = report.Has(engine.CapHoldTokens)
	turnCaps := []string{engine.CapRecordDelivery, engine.CapReplyParts}
	if r.holds {
		turnCaps = append(turnCaps, engine.CapHoldTokens)
	}

	r.tg = newFakeTG(t)
	client := r.tg.client()
	client.Repair = render.Sanitize
	pres := &Presenter{
		Telegram: client, Engine: r.eng, Retries: &Retries{}, Planner: ServicePlanner{Engine: r.eng}, Cancels: &Cancels{}, Log: quiet,
		Deliverer: &Deliverer{Telegram: client, Store: r.eng, Followups: &Followups{}, Log: quiet},
	}
	// With the v8 capabilities the bot runs as in production.
	if report.Has(engine.CapPreparePhoto) {
		pres.Deliverer.Photos = ServicePhotos{Engine: r.eng}
	}
	var mirrors *MirrorFeed
	if report.Has(engine.CapWorkerMirrorRoute) {
		mirrors = &MirrorFeed{Telegram: client, Lookup: r.eng.WorkerMirror, MiniAppBase: "https://t.me/x/app", Log: quiet, Interval: 100 * time.Millisecond}
	}
	acks := &SteerAcks{}
	ready := func(ctx context.Context) error {
		return r.eng.RequireAll(ctx, turnCaps...)
	}
	r.h = &Handler{
		Telegram: client, Engine: r.eng, Resolver: ServiceResolver{Engine: r.eng}, Composer: r.eng,
		Turns: pinnedModel{r.eng}, Queues: NewTopicQueues(context.Background()), Presenter: pres, Acks: acks,
		Artifacts: UnavailableArtifacts{}, ThreadOps: r.eng, RootForChat: func(int64) string { return root }, Log: quiet,
		TurnReady: ready, Replay: r.holds,
	}
	ctx, cancel := context.WithCancel(context.Background())
	steer := SteerObserver(ctx, acks, r.h, quiet)
	workers := &WorkerNotices{Telegram: client, Engine: r.eng, MiniAppBase: "https://t.me/x/app", Mirrors: mirrors, Log: quiet}
	observe := func(ev engine.Event) { steer(ev); workers.Observe(ctx, ev) }
	r.newBridge = func() *Bridge {
		b := &Bridge{Engine: r.eng, Renderer: pres, Observe: observe, Ready: ready, Log: quiet, Delay: 100 * time.Millisecond, Replay: r.holds}
		if mirrors != nil {
			b.Connected = mirrors.Forget
		}
		return b
	}
	r.startBridge()
	// Cleanups run last-registered first: this one before the service
	// stops, so it must not wait for turns that only end with the service.
	t.Cleanup(func() {
		cancel()
		r.stopBridge()
	})
	// The service reports no subscribers; the bridge subscribes within one
	// reconnect delay once the capabilities pass.
	time.Sleep(300 * time.Millisecond)
	return r
}

// restartService stops the service and starts it again on the same home
// (its memory is gone; persisted threads stay). The bridge reconnects.
func (r *realRust) restartService() {
	r.stopService()
	stop := r.serve()
	r.stopService = func() { stop() }
}

// startBridge runs a renderer bridge until stopBridge.
func (r *realRust) startBridge() {
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	b := r.newBridge()
	go func() { b.Run(ctx); close(done) }()
	r.stopBridge = func() {
		cancel()
		select {
		case <-done:
		case <-time.After(5 * time.Second):
			r.t.Log("bridge did not stop in 5s")
		}
	}
}

func (r *realRust) log() string {
	b, _ := os.ReadFile(r.logPath)
	return string(b)
}

func (r *realRust) waitFor(what string, limit time.Duration, pred func() bool) {
	r.t.Helper()
	deadline := time.Now().Add(limit)
	for !pred() {
		if time.Now().After(deadline) {
			r.t.Fatalf("timed out waiting for %s\ncalls: %+v\nservice log:\n%s", what, r.tg.calls, r.log())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (r *realRust) sent(text string) bool {
	for _, c := range r.tg.of("sendMessage") {
		if s, _ := c.body["text"].(string); strings.Contains(s, text) {
			return true
		}
	}
	return false
}

func (r *realRust) settled(threadID string) bool {
	rep, err := r.eng.StatusReport(context.Background(), &threadID)
	return err == nil && len(rep.Threads) == 1 && rep.Threads[0].State == "idle" && rep.Threads[0].Settled
}

// events returns the thread file's lines of one type.
func (r *realRust) events(threadID, typ string) []map[string]any {
	b, _ := os.ReadFile(filepath.Join(r.home, "threads", threadID+".jsonl"))
	var out []map[string]any
	for _, line := range strings.Split(string(b), "\n") {
		var ev map[string]any
		if json.Unmarshal([]byte(line), &ev) == nil && ev["type"] == typ {
			out = append(out, ev)
		}
	}
	return out
}

func (r *realRust) modelRequests() int {
	r.mu.Lock()
	defer r.mu.Unlock()
	return len(r.requests)
}

const realThread = "telegram--100-topic-5"

// A failed turn offers Try again; the tap reruns the saved history: no
// second user message, and the answer arrives.
func TestRealRustRetryReruns(t *testing.T) {
	r := startRealRust(t, func(ctx context.Context, n int, w http.ResponseWriter) {
		if n == 0 {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusBadRequest)
			io.WriteString(w, `{"type":"error","error":{"type":"invalid_request_error","message":"synthetic refusal"}}`)
			return
		}
		answer("RETRIED-ANSWER")(ctx, n, w)
	})
	r.h.Dispatch(context.Background(), userMessage(t, 42, "please retry me"))
	r.waitFor("retry button", 20*time.Second, func() bool { return r.sent(RetryPrompt) })
	r.waitFor("first turn released", 10*time.Second, func() bool { return r.settled(realThread) })
	r.h.HandleCallback(context.Background(), tap(t, RetryGo, lastMessageID(r.tg), ""))
	r.waitFor("retried answer", 20*time.Second, func() bool { return r.sent("RETRIED-ANSWER") })
	r.waitFor("released", 10*time.Second, func() bool { return r.settled(realThread) })

	users := 0
	for _, m := range r.events(realThread, "message") {
		if m["role"] == "user" {
			users++
		}
	}
	if users != 1 || r.modelRequests() != 2 {
		t.Fatalf("a rerun must not record a new user message: users=%d requests=%d", users, r.modelRequests())
	}
	if d := r.events(realThread, "delivery"); len(d) != 2 {
		t.Fatalf("only the answered turn delivers: %v", d)
	}
}

// Cancel stops the running turn in the service; the status says so and the
// thread settles.
func TestRealRustCancelStopsTheTurn(t *testing.T) {
	r := startRealRust(t, func(ctx context.Context, _ int, w http.ResponseWriter) {
		select {
		case <-ctx.Done():
		case <-time.After(30 * time.Second):
		}
	})
	r.h.Dispatch(context.Background(), userMessage(t, 42, "a long task"))
	r.waitFor("model request", 20*time.Second, func() bool { return r.modelRequests() == 1 })
	r.h.HandleCallback(context.Background(), tap(t, "cancel:-100:42", 1, ""))
	r.waitFor("Cancelled", 20*time.Second, func() bool {
		for _, e := range r.tg.of("editMessageText") {
			if e.body["text"] == StatusCancelled {
				return true
			}
		}
		return false
	})
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })
	if len(r.events(realThread, "delivery")) != 0 {
		t.Fatal("a cancelled turn delivers nothing")
	}
}

// A message sent while a text-only turn runs is steered; the turn ends
// before a safe point, so the service runs it as the next turn, which the
// renderer subscription renders and releases.
func TestRealRustSteerRunsAsNextTurnThroughTheRenderer(t *testing.T) {
	release := make(chan struct{})
	var releaseOnce sync.Once
	open := func() { releaseOnce.Do(func() { close(release) }) }
	t.Cleanup(open)
	r := startRealRust(t, func(ctx context.Context, n int, w http.ResponseWriter) {
		if n == 0 {
			select {
			case <-release:
			case <-ctx.Done():
				return
			}
		}
		answer(fmt.Sprintf("ANSWER-%d", n+1))(ctx, n, w)
	})
	r.h.Dispatch(context.Background(), userMessage(t, 42, "first"))
	r.waitFor("first model request", 20*time.Second, func() bool { return r.modelRequests() == 1 })
	r.h.Dispatch(context.Background(), userMessage(t, 43, "second"))
	r.waitFor("steer ack", 10*time.Second, func() bool { return r.sent(AckPending) })
	open()
	r.waitFor("both answers", 30*time.Second, func() bool { return r.sent("ANSWER-1") && r.sent("ANSWER-2") })
	r.waitFor("ack edit", 10*time.Second, func() bool {
		for _, e := range r.tg.of("editMessageText") {
			if e.body["text"] == AckNewTurn {
				return true
			}
		}
		return false
	})
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })
	if d := r.events(realThread, "delivery"); len(d) != 4 {
		t.Fatalf("two turns, two records each: %d", len(d))
	}
}

// A persistent-profile topic resolves as profile and is submitted without
// ordinary; the profile turn answers like an ordinary one.
func TestRealRustProfileTopic(t *testing.T) {
	r := startRealRust(t, answer("PROFILE-ANSWER"))
	// Create the profile thread the way `zdx orchestrator send` does.
	threadID := realThread
	m := mockModel
	setup, err := r.eng.SubmitFollow(context.Background(), engine.Submit{ThreadID: &threadID, Root: r.h.RootForChat(-100), Model: &m, Text: "setup"})
	if err != nil {
		t.Fatalf("create profile thread: %v\n%s", err, r.log())
	}
	setup.Turn.Wait()
	r.waitFor("setup settled", 20*time.Second, func() bool { return r.settled(realThread) })
	topic := int64(5)
	route, err := r.eng.ResolveThread(context.Background(), -100, &topic)
	if err != nil || route.Kind != engine.KindProfile {
		t.Fatalf("route=%+v err=%v", route, err)
	}
	r.h.Dispatch(context.Background(), userMessage(t, 42, "hello orchestrator"))
	r.waitFor("profile answer", 20*time.Second, func() bool { return r.sent("PROFILE-ANSWER") })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })
	if d := r.events(realThread, "delivery"); len(d) != 2 {
		t.Fatalf("the routed profile turn records its delivery: %d", len(d))
	}
}

// Real reply_parts on a reply with media: a document is uploaded with its
// Open artifact link, a missing file fails, and photos go through Rust
// prepare_photo (capability prepare_photo): a valid JPEG and a file whose
// size Rust cannot read are uploaded as they are, a PNG over the long-edge
// limit is uploaded as Rust's JPEG, and a PNG that must change but cannot
// be decoded fails (no document fallback, as in the Rust bot). The final
// record's summary (Rust's text) is posted once.
func TestRealRustReplyWithFiles(t *testing.T) {
	dir := t.TempDir()
	doc := writeFile(t, "report.pdf", []byte("%PDF-1.4 synthetic"))
	png := writeFile(t, "shot.png", []byte("\x89PNG\r\n\x1a\n0000"))
	jpg := writeFile(t, "small.jpg", smallJPEG(t))
	var big bytes.Buffer
	if err := stdpng.Encode(&big, image.NewGray(image.Rect(0, 0, 3000, 100))); err != nil {
		t.Fatal(err)
	}
	wide := writeFile(t, "wide.png", big.Bytes())
	// Cut short: Rust reads 3000x100, then cannot decode the pixels.
	broken := writeFile(t, "broken.png", big.Bytes()[:big.Len()-20])
	missing := dir + "/gone.pdf"
	media := []string{doc, missing, png, jpg, wide, broken}
	text := "FILES-ANSWER\\n<medias>"
	for _, m := range media {
		text += "<media>" + m + "</media>"
	}
	text += "</medias>"
	r := startRealRust(t, answer(text))
	if r.eng.Require(context.Background(), engine.CapPreparePhoto) != nil {
		sandbox.RustFeatureMissing(t, "the binary lacks prepare_photo")
	}
	r.h.Presenter.Planner = ServicePlanner{Engine: r.eng, MiniAppBase: "https://t.me/x/app"}
	r.h.Dispatch(context.Background(), userMessage(t, 42, "send files"))
	// Six files, one a 3000 px image Rust resizes: 10-14 s alone, over 20 s
	// under a loaded gate run.
	r.waitFor("summary", 60*time.Second, func() bool { return r.sent("Part of this reply was not delivered") })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })

	docs := r.tg.of("sendDocument")
	if len(docs) != 1 || docs[0].body["document"] != "report.pdf|application/pdf" || !strings.Contains(fmt.Sprint(docs[0].body["reply_markup"]), "startapp=a_") {
		t.Fatalf("unexpected uploads %+v", docs)
	}
	var photos []string
	for _, c := range r.tg.of("sendPhoto") {
		photos = append(photos, fmt.Sprint(c.body["photo"]))
	}
	if want := []string{"shot.png|image/png", "small.jpg|image/jpeg", "wide.jpg|image/jpeg"}; !slices.Equal(photos, want) {
		t.Fatalf("photos %v, want %v", photos, want)
	}
	d := r.events(realThread, "delivery")
	if len(d) != 2 {
		t.Fatalf("want pending and final, got %d", len(d))
	}
	items := d[1]["items"].([]any)
	var statuses []string
	for _, it := range items {
		statuses = append(statuses, it.(map[string]any)["status"].(string))
	}
	if strings.Join(statuses, ",") != "sent,sent,failed,sent,sent,sent,failed" {
		t.Fatalf("text, pdf, missing, png, jpg, wide, broken: %v", statuses)
	}
	summaries := 0
	for _, c := range r.tg.of("sendMessage") {
		if s, _ := c.body["text"].(string); strings.Contains(s, "Part of this reply was not delivered") {
			summaries++
			if !strings.Contains(s, "gone.pdf") || !strings.Contains(s, "broken.png") {
				t.Fatalf("Rust's summary names both failed files: %q", s)
			}
		}
	}
	if summaries != 1 {
		t.Fatalf("one summary, got %d", summaries)
	}
	t.Logf("final record: %v", d[1])
}

// A document album goes through Go intake (fake Bot API download, saved
// under the temp home) and Rust's compose_user_text: the model sees both
// saved paths and the caption, and the answer is delivered.
func TestRealRustDocumentAlbum(t *testing.T) {
	r := startRealRust(t, answer("DOCS-ANSWER"))
	r.tg.files = map[string]string{"docs/d1": "%PDF-1.4 one", "docs/d2": "%PDF-1.4 two"}
	store := filepath.Join(r.home, "telegram")
	r.h.Attachments = &intake.Collector{Files: r.h.Telegram, Store: intake.DirStore{Root: store}, Log: quiet}
	head := docMessage(t, 42, "d1", `,"caption":"compare these"`)
	r.h.Dispatch(context.Background(), Incoming{Message: head, Grouped: []*telegram.Message{docMessage(t, 43, "d2", "")}})
	r.waitFor("answer", 20*time.Second, func() bool { return r.sent("DOCS-ANSWER") })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })

	paths := []string{filepath.Join(store, "-100", "42_d1.pdf"), filepath.Join(store, "-100", "43_d2.pdf")}
	r.mu.Lock()
	req := r.requests[0]
	r.mu.Unlock()
	for _, p := range paths {
		if _, err := os.Stat(p); err != nil {
			t.Fatalf("saved file: %v", err)
		}
		if !strings.Contains(req, p) {
			t.Fatalf("the model must see %s:\n%s", p, req)
		}
	}
	if !strings.Contains(req, "compare these") || r.modelRequests() != 1 {
		t.Fatalf("caption and one request expected: %d\n%s", r.modelRequests(), req)
	}
	if d := r.events(realThread, "delivery"); len(d) != 2 {
		t.Fatalf("want pending and final records, got %d", len(d))
	}
}

// Send file and the thread view through Rust's views: the reply's HTML
// file card is tapped, Go finds the file by its artifact key
// (artifacts_list, artifact_resolve) and uploads it as a reply to the card;
// thread_view shows the turn. Needs a binary with the view capabilities.
func TestRealRustViewsSendFileAndThreadView(t *testing.T) {
	page := writeFile(t, "page.html", []byte("<html><body>synthetic</body></html>"))
	r := startRealRust(t, answer("VIEWS-ANSWER\\n<medias><media>"+page+"</media></medias>"))
	if err := r.eng.RequireAll(context.Background(), engine.CapThreadViews, engine.CapArtifactViews); err != nil {
		t.Fatalf("stale binary (rebuild it with views): %v", err)
	}
	r.h.Artifacts = ServiceArtifacts{Engine: r.eng}
	r.h.Presenter.Planner = ServicePlanner{Engine: r.eng, MiniAppBase: "https://t.me/x/app"}
	r.h.Dispatch(context.Background(), userMessage(t, 42, "send the page"))
	r.waitFor("card", 20*time.Second, func() bool { return r.sent("not uploaded") })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })

	var card tgCall
	for _, c := range r.tg.of("sendMessage") {
		if s, _ := c.body["text"].(string); strings.Contains(s, "not uploaded") {
			card = c
		}
	}
	var data string
	rows, _ := card.body["reply_markup"].(map[string]any)["inline_keyboard"].([]any)
	for _, row := range rows {
		for _, b := range row.([]any) {
			if d, _ := b.(map[string]any)["callback_data"].(string); strings.HasPrefix(d, render.SendFilePrefix) {
				data = d
			}
		}
	}
	if data == "" {
		t.Fatalf("the card has no Send file button: %v", card.body["reply_markup"])
	}
	r.h.HandleCallback(context.Background(), tap(t, data, card.id, ""))
	r.waitFor("upload", 20*time.Second, func() bool { return len(r.tg.of("sendDocument")) == 1 })
	up := r.tg.of("sendDocument")[0].body
	if up["document"] != "page.html|text/html" || up["reply_to_message_id"] != fmt.Sprint(card.id) {
		t.Fatalf("unexpected upload %v", up)
	}
	r.waitFor("the upload record", 10*time.Second, func() bool { return len(r.events(realThread, "delivery")) == 3 })

	thread := realThread
	view, err := r.eng.View(context.Background(), engine.View{Op: engine.ViewThreadView, ThreadID: &thread})
	if err != nil || !strings.Contains(string(view.Body), "send the page") {
		t.Fatalf("thread view: %v %.300s", err, view.Body)
	}
}

// testMedia is the gateway's Rust media adapter (internal/gateway can not
// be imported here).
type testMedia struct {
	eng  *engine.Client
	root string
}

func (m testMedia) Transcribe(ctx context.Context, a intake.AudioFile) (*string, error) {
	return m.eng.TranscribeAudio(ctx, engine.TranscribeAudio{Path: a.Path, FileName: a.FileName, MimeType: a.MimeType, Root: m.root})
}

func (m testMedia) PrepareImage(ctx context.Context, img intake.ImageFile) (intake.PreparedImage, error) {
	p, err := m.eng.PrepareImage(ctx, img.Path)
	return intake.PreparedImage{Delivery: p.Delivery, Inline: p.Image}, err
}

// A photo and a voice message through Go intake and Rust's prepare_image
// and transcribe_audio: the model gets the photo inline, and the voice
// message is saved without a transcript (the temp home configures no
// transcription), as Rust composes it.
func TestRealRustPhotoAndVoice(t *testing.T) {
	r := startRealRust(t, answer("MEDIA-ANSWER"))
	if err := r.eng.RequireAll(context.Background(), engine.CapPrepareImage, engine.CapTranscribeAudio); err != nil {
		t.Fatalf("stale binary (rebuild it with media operations): %v", err)
	}
	r.tg.files = map[string]string{"docs/p1": string(smallJPEG(t)), "docs/v1": "OggS synthetic voice"}
	store := filepath.Join(r.home, "telegram")
	media := testMedia{eng: r.eng, root: r.h.RootForChat(-100)}
	r.h.Attachments = &intake.Collector{Files: r.h.Telegram, Store: intake.DirStore{Root: store}, Transcriber: media, Images: media, Log: quiet}
	base := `"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false}`
	photo := msg(t, `{"message_id":42,`+base+`,"caption":"what is this","photo":[{"file_id":"p1","width":10,"height":10,"file_size":400}]}`)
	r.h.Dispatch(context.Background(), Incoming{Message: photo})
	r.waitFor("photo answer", 20*time.Second, func() bool { return r.sent("MEDIA-ANSWER") })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })
	voice := msg(t, `{"message_id":43,`+base+`,"voice":{"file_id":"v1","mime_type":"audio/ogg","file_size":20}}`)
	r.h.Dispatch(context.Background(), Incoming{Message: voice})
	r.waitFor("second request", 20*time.Second, func() bool { return r.modelRequests() == 2 })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })

	r.mu.Lock()
	first, second := r.requests[0], r.requests[1]
	r.mu.Unlock()
	photoPath := filepath.Join(store, "-100", "42_p1")
	if !strings.Contains(first, `"type":"image"`) || !strings.Contains(first, "what is this") {
		t.Fatalf("the photo must reach the model inline with its caption:\n%.2000s", first)
	}
	if _, err := os.Stat(filepath.Join(store, "-100")); err != nil {
		t.Fatalf("saved media dir: %v", err)
	}
	if !strings.Contains(second, filepath.Join(store, "-100", "43_")) {
		t.Fatalf("the voice message path must reach the model:\n%.2000s", second)
	}
	t.Logf("photo saved near %s", photoPath)
}

// Voice through Rust's transcribe_audio with transcription configured in the
// chat root (`.zdx/config.toml`: OpenAI-compatible endpoint, a mock): a
// transcript reaches the model; a provider failure leaves the saved path
// and the turn still runs, as in the Rust bot.
func TestRealRustVoiceTranscription(t *testing.T) {
	var mu sync.Mutex
	var stt []string
	fail := false
	mock := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		_ = req.ParseMultipartForm(1 << 20)
		name := ""
		if fh := req.MultipartForm.File["file"]; len(fh) == 1 {
			name = fh[0].Filename + "|" + fh[0].Header.Get("Content-Type")
		}
		mu.Lock()
		stt = append(stt, req.URL.Path+" "+req.Header.Get("Authorization")+" "+req.FormValue("model")+" "+name)
		failing := fail
		mu.Unlock()
		if failing {
			http.Error(w, `{"error":{"message":"synthetic outage"}}`, http.StatusInternalServerError)
			return
		}
		io.WriteString(w, `{"text":"VOICE-TRANSCRIPT pay the rent"}`)
	}))
	defer mock.Close()

	r := startRealRust(t, answer("VOICE-ANSWER"))
	root := r.h.RootForChat(-100)
	if err := os.MkdirAll(filepath.Join(root, ".zdx"), 0o700); err != nil {
		t.Fatal(err)
	}
	cfg := fmt.Sprintf("[transcription]\nmodel = \"openai:whisper-1\"\n\n[providers.openai]\napi_key = \"test-stt-key\"\nbase_url = %q\n", mock.URL)
	if err := os.WriteFile(filepath.Join(root, ".zdx", "config.toml"), []byte(cfg), 0o600); err != nil {
		t.Fatal(err)
	}
	r.tg.files = map[string]string{"docs/v1": "OggS one", "docs/v2": "OggS two"}
	store := filepath.Join(r.home, "telegram")
	media := testMedia{eng: r.eng, root: root}
	r.h.Attachments = &intake.Collector{Files: r.h.Telegram, Store: intake.DirStore{Root: store}, Transcriber: media, Images: media, Log: quiet}
	base := `"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false}`

	r.h.Dispatch(context.Background(), Incoming{Message: msg(t, `{"message_id":42,`+base+`,"voice":{"file_id":"v1","mime_type":"audio/ogg","file_size":8}}`)})
	r.waitFor("first answer", 20*time.Second, func() bool { return r.modelRequests() == 1 })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })

	mu.Lock()
	fail = true
	mu.Unlock()
	r.h.Dispatch(context.Background(), Incoming{Message: msg(t, `{"message_id":43,`+base+`,"voice":{"file_id":"v2","mime_type":"audio/ogg","file_size":8}}`)})
	r.waitFor("second answer", 20*time.Second, func() bool { return r.modelRequests() == 2 })
	r.waitFor("settled", 10*time.Second, func() bool { return r.settled(realThread) })

	r.mu.Lock()
	first, second := r.requests[0], r.requests[1]
	r.mu.Unlock()
	if !strings.Contains(first, "VOICE-TRANSCRIPT pay the rent") {
		t.Fatalf("the transcript must reach the model:\n%.3000s", first)
	}
	// The second request replays the first turn; only its last message is
	// the new one.
	if _, last, _ := lastTurn(second); strings.Contains(last, "VOICE-TRANSCRIPT") || !strings.Contains(last, filepath.Join(store, "-100", "43_")) {
		t.Fatalf("a failed transcription keeps the saved path and no transcript:\n%.3000s", last)
	}
	mu.Lock()
	defer mu.Unlock()
	if len(stt) != 2 || !strings.HasPrefix(stt[0], "/audio/transcriptions Bearer test-stt-key whisper-1 v1|audio/ogg") {
		t.Fatalf("transcription requests: %q", stt)
	}
}
