package miniapp_test

import (
	"context"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/gateway"
	"github.com/tallesborges/zdx/apps/gateway/internal/miniapp"
)

type allowAll struct{}

func (allowAll) Allows(int64, miniapp.FeedbackRoute) bool { return true }

// testRenderer is the service's renderer: it records every routed turn the
// service starts and releases held ones with its hold token, as the
// gateway's bridge does.
type testRenderer struct {
	mu      sync.Mutex
	started []engine.Event
	ended   []engine.Event
}

func startRenderer(t *testing.T, p *parity) *testRenderer {
	t.Helper()
	ctx, cancel := context.WithCancel(context.Background())
	stream, err := p.eng.Open(ctx, engine.Subscribe{Renderer: true})
	if err != nil {
		t.Fatal(err)
	}
	first, err := stream.Next()
	if err != nil || first.Kind != engine.ReplySubscribed || first.Holder == "" {
		t.Fatalf("renderer subscription: %+v %v", first, err)
	}
	holder := first.Holder
	r := &testRenderer{}
	done := make(chan struct{})
	go func() {
		defer close(done)
		for {
			reply, err := stream.Next()
			if err != nil {
				return
			}
			if reply.Kind != engine.ReplyEvent || reply.Event == nil {
				continue
			}
			ev := *reply.Event
			switch ev.Type {
			case engine.EventTurnStarted:
				r.mu.Lock()
				r.started = append(r.started, ev)
				r.mu.Unlock()
			case engine.EventTurnEnded:
				r.mu.Lock()
				r.ended = append(r.ended, ev)
				r.mu.Unlock()
				if ev.Held {
					rctx, rcancel := context.WithTimeout(context.Background(), 10*time.Second)
					if err := p.eng.ReleaseHeld(rctx, ev.ThreadID, ev.TurnID, holder); err != nil {
						t.Errorf("release turn %d: %v", ev.TurnID, err)
					}
					rcancel()
				}
			}
		}
	}()
	t.Cleanup(func() {
		cancel()
		stream.Close()
		<-done
	})
	return r
}

func (r *testRenderer) turns() ([]engine.Event, int) {
	r.mu.Lock()
	defer r.mu.Unlock()
	return append([]engine.Event(nil), r.started...), len(r.ended)
}

// feedbackFixture is a real service with a chat config (allowlists and a
// transcription provider) and a mock speech-to-text endpoint.
type feedbackFixture struct {
	*parity
	renderer       *testRenderer
	transcriptions atomic.Int32
	// sttGate, when set, holds every transcription request until closed.
	sttGate atomic.Pointer[chan struct{}]
	sttSeen chan struct{}
	listed  map[string]string
	botRoot string
}

const (
	strangerUser = int64(5151) // passes Go auth, not in the chat config
	transcript   = "TRANSCRIBED-VOICE-NOTE"
)

func startFeedback(t *testing.T) *feedbackFixture {
	f := &feedbackFixture{sttSeen: make(chan struct{}, 16)}
	stt := func(w http.ResponseWriter, r *http.Request, _ []byte) bool {
		if !strings.HasSuffix(r.URL.Path, "/audio/transcriptions") {
			return false
		}
		f.transcriptions.Add(1)
		f.sttSeen <- struct{}{}
		if gate := f.sttGate.Load(); gate != nil {
			<-*gate
		}
		w.Header().Set("Content-Type", "application/json")
		io.WriteString(w, `{"text":"`+transcript+`"}`)
		return true
	}
	// The chat's allowlist and transcription live in its project root only,
	// so the service accepts the sender only when the adapter sends the chat's
	// root; the bot root (Root) is an empty directory.
	botRoot := t.TempDir()
	opts := parityOptions{
		projectConfig: fmt.Sprintf("[telegram]\nallowlist_user_ids = [%d]\nallowlist_chat_ids = [-100]\n\n[transcription]\nmodel = \"openai\"\n", parityUser),
		env:           []string{"OPENAI_API_KEY=test-openai-key", "OPENAI_BASE_URL=__MOCK__/v1"},
		provider:      stt,
		gate:          allowAll{},
		users:         []int64{strangerUser},
		engine:        func(p *parity) miniapp.Engine { return chatRootAdapter(p, botRoot) },
		caps:          []string{engine.CapFeedbackSubmit, engine.CapHoldTokens},
	}
	f.parity = startParityWith(t, opts)
	f.botRoot = botRoot
	f.renderer = startRenderer(t, f.parity)

	thread := parityThread
	list, err := f.eng.View(context.Background(), engine.View{Op: engine.ViewArtifactsList, ThreadID: &thread})
	if err != nil {
		t.Fatal(err)
	}
	var artifacts struct {
		Artifacts []struct{ Path string } `json:"artifacts"`
	}
	_ = json.Unmarshal(list.Body, &artifacts)
	f.listed = map[string]string{}
	for _, a := range artifacts.Artifacts {
		f.listed[filepath.Base(a.Path)] = a.Path
	}
	for _, name := range []string{"page.html", "picture.png", "notes.txt"} {
		if f.listed[name] == "" {
			t.Fatalf("%s not listed: %s", name, list.Body)
		}
	}
	return f
}

// chatRootAdapter is the real gateway adapter as run.go wires it: chat -100
// maps to the project root, anything else to the bot root.
func chatRootAdapter(p *parity, botRoot string) miniapp.Engine {
	return &gateway.MiniAppEngine{
		Client: p.eng,
		Root:   botRoot,
		RootForChat: func(chat int64) string {
			if chat == -100 {
				return p.root
			}
			return botRoot
		},
	}
}

func (f *feedbackFixture) post(srv string, user int64, thread, body string) answer {
	f.t.Helper()
	req, _ := http.NewRequest(http.MethodPost, srv+"/api/threads/"+thread+"/artifacts/feedback", strings.NewReader(body))
	req.Header.Set("Authorization", "tma "+initData(user))
	req.Header.Set("Content-Type", "application/json")
	res, err := (&http.Client{Timeout: 3 * time.Minute}).Do(req)
	if err != nil {
		f.t.Fatal(err)
	}
	defer res.Body.Close()
	data, _ := io.ReadAll(res.Body)
	return answer{res.StatusCode, res.Header, data}
}

// queuedFeedbackTurns are the thread's recorded user messages that carry a
// feedback prompt containing marker.
func (f *feedbackFixture) userMessages(marker string) int {
	b, _ := os.ReadFile(filepath.Join(f.home, "threads", parityThread+".jsonl"))
	n := 0
	for _, line := range strings.Split(string(b), "\n") {
		var ev map[string]any
		if json.Unmarshal([]byte(line), &ev) != nil {
			continue
		}
		if ev["type"] == "message" && ev["role"] == "user" && strings.Contains(line, marker) {
			n++
		}
	}
	return n
}

func (f *feedbackFixture) modelRequestsWith(marker string) int {
	n := 0
	for _, body := range f.modelRequests() {
		if strings.Contains(body, marker) {
			n++
		}
	}
	return n
}

func (f *feedbackFixture) settle() {
	f.t.Helper()
	thread := parityThread
	for deadline := time.Now().Add(20 * time.Second); ; {
		rep, err := f.eng.StatusReport(context.Background(), &thread)
		if err == nil && len(rep.Threads) == 1 && rep.Threads[0].Settled {
			return
		}
		if time.Now().After(deadline) {
			f.t.Fatalf("thread never settled: %+v %v\n%s", rep, err, f.log())
		}
		time.Sleep(20 * time.Millisecond)
	}
}

func (f *feedbackFixture) bundle(id string) string {
	return filepath.Join(f.home, "artifact-feedback", parityThread, id)
}

func exists(path string) bool { _, err := os.Stat(path); return err == nil }

func htmlBody(f *feedbackFixture, id, comment string, allowStale bool, sha string, media ...map[string]string) string {
	if sha == "" {
		page, _ := os.ReadFile(f.page)
		sha = shaHex(page)
	}
	note := map[string]any{
		"intent":  "change",
		"comment": comment,
		"target": map[string]any{"kind": "element", "element": map[string]any{
			"selector": "body", "tag": "body", "text": "synthetic page",
			"rect": map[string]float64{"x": 0, "y": 0, "width": 300, "height": 40},
		}},
	}
	if len(media) > 0 {
		note["media"] = media
	}
	body, _ := json.Marshal(map[string]any{
		"submission_id": id, "artifact_path": f.listed["page.html"], "artifact_sha256": sha,
		"allow_stale": allowStale, "annotations": []any{note},
	})
	return string(body)
}

func shaHex(b []byte) string { return sha(b) }

// webm is the start of a WebM file as browsers record it.
var webm = []byte{0x1A, 0x45, 0xDF, 0xA3, 0x9F, 0x42, 0x86, 0x81, 0x01, 0x42, 0xF7, 0x81, 0x01, 0x42, 0xF2}

// TestRealFeedback sends Mini App feedback over HTTP through the Go route
// into an isolated real service, and checks what the service did, not only
// the HTTP answer: model requests, thread records, the renderer's routed
// turns, bundles and their queued markers.
func TestRealFeedback(t *testing.T) {
	f := startFeedback(t)
	srv := f.http.URL
	thread := parityThread

	t.Run("refusals queue nothing", func(t *testing.T) {
		modelsBefore := len(f.modelRequests())
		page, _ := os.ReadFile(f.page)
		pageSHA := shaHex(page)
		cases := []struct {
			name   string
			user   int64
			thread string
			body   string
			status int
			text   string
		}{
			{"syntax", parityUser, thread, `{"submission_id":`, 400, "Invalid feedback body:"},
			{"data", parityUser, thread, `{"submission_id":"sub-data-0001"}`, 422, "Invalid feedback body:"},
			{"stale", parityUser, thread, htmlBody(f, "sub-stale-0001", "STALE-NOTE", false, strings.Repeat("ab", 32)), 412, "The file changed after you annotated it."},
			{"unsupported", parityUser, thread, strings.Replace(strings.Replace(htmlBody(f, "sub-txt-0001", "TXT-NOTE", false, pageSHA), f.listed["page.html"], f.listed["notes.txt"], 1), pageSHA, shaHex([]byte("plain notes\n")), 1), 422, ""},
			{"stranger", strangerUser, thread, htmlBody(f, "sub-stranger-0001", "STRANGER-NOTE", false, ""), 403, "You cannot send feedback into this chat"},
			{"not routable", parityUser, "cli-thread-1", htmlBody(f, "sub-cli-0001", "CLI-NOTE", false, ""), 409, "does not belong to a Telegram chat"},
			{"active", parityUser, "active", htmlBody(f, "sub-active-0001", "ACTIVE-NOTE", false, ""), 400, "Reopen the thread from its chat to send feedback."},
		}
		for _, c := range cases {
			got := f.post(srv, c.user, c.thread, c.body)
			if got.status != c.status || !strings.Contains(string(got.body), c.text) {
				t.Errorf("%s: %d %q, want %d containing %q\n%s", c.name, got.status, got.body, c.status, c.text, f.log())
			}
			t.Logf("%s: %d %q", c.name, got.status, got.body)
		}
		f.settle()
		if n := len(f.modelRequests()); n != modelsBefore {
			t.Fatalf("refused feedback reached the model: %d new requests", n-modelsBefore)
		}
		for _, id := range []string{"sub-data-0001", "sub-stale-0001", "sub-txt-0001", "sub-stranger-0001"} {
			if exists(filepath.Join(f.bundle(id), "queued")) {
				t.Errorf("%s left a queued marker", id)
			}
		}
	})

	t.Run("queued html note with a voice note runs once as data", func(t *testing.T) {
		voice := map[string]string{"kind": "audio", "mime": "audio/webm;codecs=opus", "data": base64.StdEncoding.EncodeToString(webm)}
		body := htmlBody(f, "sub-html-0001", "/new HTML-NOTE keep this text as data", false, "", voice)
		before := len(f.modelRequests())
		startedBefore, _ := f.renderer.turns()
		got := f.post(srv, parityUser, thread, body)
		if got.status != http.StatusAccepted || string(got.body) != `{"status":"queued","annotations":1,"stale":false}` {
			t.Fatalf("%d %s\n%s", got.status, got.body, f.log())
		}
		f.settle()
		if f.transcriptions.Load() != 1 {
			t.Fatalf("transcriptions %d, want 1", f.transcriptions.Load())
		}
		if n := f.modelRequestsWith("HTML-NOTE"); n != 1 {
			t.Fatalf("model requests with the note: %d, want 1", n)
		}
		last := f.modelRequests()[len(f.modelRequests())-1]
		for _, needle := range []string{"[artifact feedback]", "/new HTML-NOTE keep this text as data", transcript, "Voice note 1 for note 1"} {
			if !strings.Contains(last, needle) {
				t.Fatalf("the feedback turn's model request lacks %q: %.1500s", needle, last)
			}
		}
		if len(f.modelRequests()) != before+1 {
			t.Fatalf("model requests %d, want %d", len(f.modelRequests()), before+1)
		}
		// `/new` stayed data: the thread kept its history and records the note.
		if f.userMessages("HTML-NOTE") != 1 || f.userMessages("show me the page") != 1 {
			t.Fatalf("thread records: note %d, first message %d", f.userMessages("HTML-NOTE"), f.userMessages("show me the page"))
		}
		started, _ := f.renderer.turns()
		if len(started) != len(startedBefore)+1 {
			t.Fatalf("renderer saw %d new turns, want 1", len(started)-len(startedBefore))
		}
		turn := started[len(started)-1]
		if turn.ThreadID != parityThread || turn.Followed || turn.Route == nil || turn.Route.Chat != -100 ||
			turn.Route.Topic == nil || *turn.Route.Topic != 5 || len(turn.Inputs) != 1 {
			t.Fatalf("routed turn %+v", turn)
		}
		if !exists(filepath.Join(f.bundle("sub-html-0001"), "queued")) {
			t.Fatal("queued bundle has no marker")
		}

		// The same body again: duplicate, nothing queued, no model call.
		dup := f.post(srv, parityUser, thread, body)
		if dup.status != http.StatusOK || string(dup.body) != `{"status":"duplicate","annotations":0,"stale":false}` {
			t.Fatalf("retry: %d %s", dup.status, dup.body)
		}
		// The same id with other content: id_conflict.
		conflict := f.post(srv, parityUser, thread, htmlBody(f, "sub-html-0001", "OTHER-CONTENT", false, ""))
		if conflict.status != http.StatusConflict {
			t.Fatalf("reused id: %d %s", conflict.status, conflict.body)
		}
		f.settle()
		if f.modelRequestsWith("HTML-NOTE") != 1 || f.userMessages("HTML-NOTE") != 1 || f.modelRequestsWith("OTHER-CONTENT") != 0 {
			t.Fatal("a duplicate or a conflict was queued")
		}
		if started2, _ := f.renderer.turns(); len(started2) != len(started) {
			t.Fatal("a duplicate started a turn")
		}
	})

	t.Run("stale allowed is queued and flagged", func(t *testing.T) {
		got := f.post(srv, parityUser, thread, htmlBody(f, "sub-stale-ok-0001", "STALE-OK-NOTE", true, strings.Repeat("ab", 32)))
		if got.status != http.StatusAccepted || !strings.Contains(string(got.body), `"stale":true`) {
			t.Fatalf("%d %s", got.status, got.body)
		}
		f.settle()
		if f.modelRequestsWith("STALE-OK-NOTE") != 1 {
			t.Fatal("stale feedback did not run")
		}
	})

	t.Run("image note sends the marked image to the model", func(t *testing.T) {
		img := f.get("/api/threads/" + parityThread + "/artifacts/image?path=" + url.QueryEscape(f.listed["picture.png"]))
		if img.status != http.StatusOK {
			t.Fatalf("canonical: %d %s", img.status, img.body)
		}
		w, h := img.header.Get("X-Zdx-Canonical-Width"), img.header.Get("X-Zdx-Canonical-Height")
		body := fmt.Sprintf(`{"submission_id":"sub-img-0001","artifact_path":%q,"artifact_sha256":%q,`+
			`"image":{"canonical_sha256":%q,"width":%s,"height":%s},`+
			`"annotations":[{"intent":"change","comment":"IMG-NOTE brighter here","target":{"kind":"image_pin","point":{"x":0.5,"y":0.5}}}]}`,
			f.listed["picture.png"], img.header.Get("X-Zdx-Source-Sha256"), img.header.Get("X-Zdx-Canonical-Sha256"), w, h)
		got := f.post(srv, parityUser, thread, body)
		if got.status != http.StatusAccepted {
			t.Fatalf("%d %s\n%s", got.status, got.body, f.log())
		}
		f.settle()
		var req string
		for _, r := range f.modelRequests() {
			if strings.Contains(r, "IMG-NOTE") {
				req = r
			}
		}
		if req == "" || !strings.Contains(req, `"type":"image"`) || !strings.Contains(req, `"media_type":"image/`) {
			t.Fatalf("the image feedback turn sent no image to the model: %.800s", req)
		}
	})

	t.Run("in flight, disconnect and retry with the same id", func(t *testing.T) {
		// Signals of earlier transcriptions must not count for this one.
		for len(f.sttSeen) > 0 {
			<-f.sttSeen
		}
		gate := make(chan struct{})
		f.sttGate.Store(&gate)
		var once sync.Once
		release := func() { once.Do(func() { close(gate); f.sttGate.Store(nil) }) }
		defer release()
		voice := map[string]string{"kind": "audio", "mime": "audio/webm", "data": base64.StdEncoding.EncodeToString(webm)}
		body := htmlBody(f, "sub-slow-0001", "SLOW-NOTE", false, "", voice)

		ctx, cancel := context.WithCancel(context.Background())
		req, _ := http.NewRequestWithContext(ctx, http.MethodPost, srv+"/api/threads/"+thread+"/artifacts/feedback", strings.NewReader(body))
		req.Header.Set("Authorization", "tma "+initData(parityUser))
		req.Header.Set("Content-Type", "application/json")
		sent := make(chan error, 1)
		go func() {
			res, err := http.DefaultClient.Do(req)
			if err == nil {
				res.Body.Close()
			}
			sent <- err
		}()
		select {
		case <-f.sttSeen:
		case <-time.After(20 * time.Second):
			t.Fatalf("the service never transcribed\n%s", f.log())
		}

		// Another gateway (own Go guard) reaches the service's guard.
		other := f.newHandler(parityOptions{gate: allowAll{}, engine: func(p *parity) miniapp.Engine { return chatRootAdapter(p, f.botRoot) }})
		inFlight := f.post(other.URL, parityUser, thread, body)
		if inFlight.status != http.StatusConflict || string(inFlight.body) != "This feedback is still being sent. Try again in a moment." {
			t.Fatalf("service in_flight: %d %q\n%s", inFlight.status, inFlight.body, f.log())
		}

		cancel() // the user closes the Mini App mid-send
		if err := <-sent; err == nil {
			t.Fatal("the request should have been cancelled")
		}
		release()
		for deadline := time.Now().Add(20 * time.Second); !exists(filepath.Join(f.bundle("sub-slow-0001"), "queued")); {
			if time.Now().After(deadline) {
				t.Fatalf("the abandoned submission was never queued\n%s", f.log())
			}
			time.Sleep(20 * time.Millisecond)
		}
		f.settle()
		retry := f.post(srv, parityUser, thread, body)
		if retry.status != http.StatusOK || !strings.Contains(string(retry.body), `"duplicate"`) {
			t.Fatalf("retry after the disconnect: %d %s", retry.status, retry.body)
		}
		f.settle()
		if f.modelRequestsWith("SLOW-NOTE") != 1 || f.userMessages("SLOW-NOTE") != 1 {
			t.Fatalf("the abandoned submission ran %d times (%d records), want once", f.modelRequestsWith("SLOW-NOTE"), f.userMessages("SLOW-NOTE"))
		}
	})

	t.Run("a queue refusal removes the marker and the retry queues", func(t *testing.T) {
		lock := filepath.Join(f.home, "run", "thread-runs", parityThread+".lock")
		if err := os.MkdirAll(filepath.Dir(lock), 0o700); err != nil {
			t.Fatal(err)
		}
		file, err := os.OpenFile(lock, os.O_CREATE|os.O_WRONLY, 0o600)
		if err != nil {
			t.Fatal(err)
		}
		// Another run (a TUI, `zdx exec`) holds the thread.
		if err := syscall.Flock(int(file.Fd()), syscall.LOCK_EX|syscall.LOCK_NB); err != nil {
			t.Fatal(err)
		}
		body := htmlBody(f, "sub-locked-0001", "LOCKED-NOTE", false, "")
		got := f.post(srv, parityUser, thread, body)
		if got.status != http.StatusConflict {
			t.Fatalf("held thread: %d %s\n%s", got.status, got.body, f.log())
		}
		if !exists(f.bundle("sub-locked-0001")) || exists(filepath.Join(f.bundle("sub-locked-0001"), "queued")) {
			t.Fatalf("after a queue refusal the bundle must be saved and unmarked (dir %v, marker %v)",
				exists(f.bundle("sub-locked-0001")), exists(filepath.Join(f.bundle("sub-locked-0001"), "queued")))
		}
		_ = syscall.Flock(int(file.Fd()), syscall.LOCK_UN)
		file.Close()
		retry := f.post(srv, parityUser, thread, body)
		if retry.status != http.StatusAccepted {
			t.Fatalf("retry after the refusal: %d %s (a false duplicate would mean a stale marker)", retry.status, retry.body)
		}
		f.settle()
		if f.modelRequestsWith("LOCKED-NOTE") != 1 {
			t.Fatalf("LOCKED-NOTE ran %d times, want once", f.modelRequestsWith("LOCKED-NOTE"))
		}
	})

	t.Logf("model requests %d, transcriptions %d", len(f.modelRequests()), f.transcriptions.Load())
}
