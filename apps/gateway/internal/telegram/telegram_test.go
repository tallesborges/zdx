package telegram

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"image"
	"image/jpeg"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

const testToken = "123456:SECRET-test-token"

type fakeClock struct {
	mu  sync.Mutex
	now time.Time
}

func newFakeClock() *fakeClock { return &fakeClock{now: time.Unix(1_700_000_000, 0)} }

func (c *fakeClock) Now() time.Time {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.now
}

func (c *fakeClock) Sleep(ctx context.Context, d time.Duration) error {
	if err := ctx.Err(); err != nil {
		return err
	}
	c.mu.Lock()
	c.now = c.now.Add(d)
	c.mu.Unlock()
	return nil
}

type seenCall struct {
	method string
	body   map[string]any
	at     time.Time
}

// fakeTelegram is a synthetic Bot API. Replies are queued per method; with
// none queued it answers a generic success.
type fakeTelegram struct {
	t       *testing.T
	srv     *httptest.Server
	clock   *fakeClock
	mu      sync.Mutex
	calls   []seenCall
	replies map[string][]func(w http.ResponseWriter)
}

func newFakeTelegram(t *testing.T, clock *fakeClock) *fakeTelegram {
	f := &fakeTelegram{t: t, clock: clock, replies: map[string][]func(http.ResponseWriter){}}
	f.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		prefix := "/bot" + testToken + "/"
		if !strings.HasPrefix(r.URL.Path, prefix) {
			http.NotFound(w, r)
			return
		}
		method := strings.TrimPrefix(r.URL.Path, prefix)
		body := map[string]any{}
		if strings.HasPrefix(r.Header.Get("Content-Type"), "multipart/") {
			if err := r.ParseMultipartForm(64 << 20); err == nil {
				for k, v := range r.MultipartForm.Value {
					body[k] = v[0]
				}
				for k, fh := range r.MultipartForm.File {
					body[k] = fh[0].Filename + "|" + fh[0].Header.Get("Content-Type")
				}
			}
		} else {
			raw, _ := io.ReadAll(r.Body)
			_ = json.Unmarshal(raw, &body)
		}
		f.mu.Lock()
		f.calls = append(f.calls, seenCall{method: method, body: body, at: clock.Now()})
		var reply func(http.ResponseWriter)
		if q := f.replies[method]; len(q) > 0 {
			reply, f.replies[method] = q[0], q[1:]
		}
		f.mu.Unlock()
		if reply != nil {
			reply(w)
			return
		}
		switch method {
		case "sendMessage", "sendDocument", "sendPhoto", "sendVoice", "sendAudio":
			io.WriteString(w, `{"ok":true,"result":{"message_id":42,"chat":{"id":-1001,"type":"supergroup"},"date":1}}`)
		case "getUpdates":
			io.WriteString(w, `{"ok":true,"result":[]}`)
		default:
			io.WriteString(w, `{"ok":true,"result":true}`)
		}
	}))
	t.Cleanup(f.srv.Close)
	return f
}

func (f *fakeTelegram) queue(method string, reply func(w http.ResponseWriter)) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.replies[method] = append(f.replies[method], reply)
}

func (f *fakeTelegram) callsOf(method string) []seenCall {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []seenCall
	for _, c := range f.calls {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

func body(s string) func(http.ResponseWriter) {
	return func(w http.ResponseWriter) { io.WriteString(w, s) }
}

func flood(retryAfter int) func(http.ResponseWriter) {
	return func(w http.ResponseWriter) {
		w.WriteHeader(429)
		json.NewEncoder(w).Encode(map[string]any{
			"ok": false, "error_code": 429, "description": "Too Many Requests",
			"parameters": map[string]any{"retry_after": retryAfter},
		})
	}
}

func (f *fakeTelegram) client() *Client {
	return NewClient(testToken, Options{BaseURL: f.srv.URL, Clock: f.clock, Logger: slog.New(slog.DiscardHandler)})
}

const chat = int64(-1001)

func text(s string) TextSend { return TextSend{ChatID: chat, Text: s} }

func TestReliableSendWaitsOutFloodRefusalAndRetries(t *testing.T) {
	clock := newFakeClock()
	f := newFakeTelegram(t, clock)
	f.queue("sendMessage", flood(3))
	msg, err := f.client().Reliable().SendText(context.Background(), text("hi"))
	if err != nil || msg.ID != 42 {
		t.Fatalf("msg=%v err=%v", msg, err)
	}
	calls := f.callsOf("sendMessage")
	if len(calls) != 2 || calls[1].at.Sub(calls[0].at) < 3*time.Second {
		t.Fatalf("want a retry after 3s, got %+v", calls)
	}
}

func TestReliableSendGivesUpWhenWaitExceedsDeadline(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	f.queue("sendMessage", flood(200))
	_, err := f.client().Reliable().SendText(context.Background(), text("hi"))
	if class, wait := Classify(err); class != ClassFloodWait || wait != 200*time.Second {
		t.Fatalf("class=%v wait=%v err=%v", class, wait, err)
	}
	if n := len(f.callsOf("sendMessage")); n != 1 {
		t.Fatalf("want 1 request, got %d", n)
	}
}

func TestReliableSendStopsAfterThreeAttempts(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	for range 4 {
		f.queue("sendMessage", flood(1))
	}
	_, err := f.client().Reliable().SendText(context.Background(), text("hi"))
	if class, _ := Classify(err); class != ClassFloodWait {
		t.Fatalf("err=%v", err)
	}
	if n := len(f.callsOf("sendMessage")); n != 3 {
		t.Fatalf("want 3 attempts, got %d", n)
	}
}

func TestAmbiguousAnswersAreNeverResent(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	f.queue("sendMessage", body(`<html>bad gateway</html>`))
	_, err := f.client().Reliable().SendText(context.Background(), text("hi"))
	if class, _ := Classify(err); class != ClassUnknown || class.DefinitelyUndelivered() {
		t.Fatalf("want unknown, got %v", err)
	}
	if n := len(f.callsOf("sendMessage")); n != 1 {
		t.Fatalf("ambiguous send must not be repeated, got %d", n)
	}
}

func TestAcceptedButUndecodableIsUnknownAndNotEchoed(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	f.queue("sendMessage", body(`{"ok":true,"result":"private-body-text"}`))
	_, err := f.client().Reliable().SendText(context.Background(), text("hi"))
	if class, _ := Classify(err); class != ClassUnknown {
		t.Fatalf("want unknown, got %v", err)
	}
	if strings.Contains(err.Error(), "private-body-text") {
		t.Fatalf("error echoes the response: %v", err)
	}
}

func TestImmediateCallsSkipChatInFloodWaitWithoutRequest(t *testing.T) {
	clock := newFakeClock()
	f := newFakeTelegram(t, clock)
	f.queue("sendMessage", flood(30))
	c := f.client()
	if _, err := c.SendText(context.Background(), text("a")); err == nil {
		t.Fatal("want refusal")
	}
	_, err := c.SendText(context.Background(), text("b"))
	var active *FloodWaitActive
	if !errors.As(err, &active) || active.ChatID != chat {
		t.Fatalf("want local skip, got %v", err)
	}
	if err := c.EditMessageText(context.Background(), chat, 1, "x", nil); !errors.As(err, &active) {
		t.Fatalf("immediate edit must be skipped too, got %v", err)
	}
	if err := c.Paced().SendChatAction(context.Background(), chat, "typing", nil); !errors.As(err, &active) {
		t.Fatalf("typing never waits, got %v", err)
	}
	if n := len(f.calls); n != 1 {
		t.Fatalf("skipped calls must not reach Telegram, got %d calls", n)
	}
	// Another chat is unaffected.
	if _, err := c.SendText(context.Background(), TextSend{ChatID: 55, Text: "c"}); err != nil {
		t.Fatal(err)
	}
}

func TestPacedCallsWaitForTheFloodWait(t *testing.T) {
	clock := newFakeClock()
	f := newFakeTelegram(t, clock)
	f.queue("sendMessage", flood(10))
	c := f.client()
	c.SendText(context.Background(), text("a"))
	start := clock.Now()
	if err := c.Paced().EditMessageText(context.Background(), chat, 1, "x", nil); err != nil {
		t.Fatal(err)
	}
	if waited := clock.Now().Sub(start); waited < 10*time.Second {
		t.Fatalf("paced edit waited %v", waited)
	}
}

func TestPacedNewMessagesAreSpacedPerChat(t *testing.T) {
	clock := newFakeClock()
	f := newFakeTelegram(t, clock)
	c := f.client().Paced()
	for range 3 {
		if _, err := c.SendText(context.Background(), text("x")); err != nil {
			t.Fatal(err)
		}
	}
	calls := f.callsOf("sendMessage")
	for i := 1; i < len(calls); i++ {
		if gap := calls[i].at.Sub(calls[i-1].at); gap < MinNewMessageSpacing {
			t.Fatalf("messages %d and %d are %v apart", i-1, i, gap)
		}
	}
	// Edits are not paced.
	start := clock.Now()
	c.EditMessageText(context.Background(), chat, 1, "y", nil)
	if clock.Now() != start {
		t.Fatal("edits must not wait for a pacing slot")
	}
}

func TestConcurrentPacedSendersStaySpaced(t *testing.T) {
	clock := newFakeClock()
	f := newFakeTelegram(t, clock)
	c := f.client().Paced()
	start := clock.Now()
	var wg sync.WaitGroup
	for range 3 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			if _, err := c.SendText(context.Background(), text("x")); err != nil {
				t.Error(err)
			}
		}()
	}
	wg.Wait()
	// Admission is atomic, so three senders take three slots: at least two
	// spacing waits happened.
	if elapsed := clock.Now().Sub(start); elapsed < 2*MinNewMessageSpacing {
		t.Fatalf("three concurrent sends waited only %v in total", elapsed)
	}
}

func TestGroupWindowLimitsTwentyPerMinute(t *testing.T) {
	var p ChatPacer
	now := time.Unix(0, 0)
	for i := range GroupWindowMaxNewMessages {
		at, ok := p.TryAdmitNewMessage(now, true)
		if !ok {
			now = at
			if _, ok := p.TryAdmitNewMessage(now, true); !ok {
				t.Fatalf("message %d not admitted at its slot", i)
			}
		}
	}
	at, ok := p.TryAdmitNewMessage(now.Add(time.Second), true)
	if ok || at.Sub(time.Unix(0, 0)) != GroupWindow {
		t.Fatalf("21st message must wait for the window: ok=%v at=%v", ok, at)
	}
	var private ChatPacer
	for i := range 25 {
		if _, ok := private.TryAdmitNewMessage(time.Unix(int64(i), 0), false); !ok {
			t.Fatalf("private chats have no window limit (message %d)", i)
		}
	}
}

func TestHTMLParseRefusalFallsBackToRepairThenPlain(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	parseErr := body(`{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: unclosed tag"}`)
	f.queue("sendMessage", parseErr)
	f.queue("sendMessage", parseErr)
	c := f.client()
	c.Repair = func(s string) string { return s + "</b>" }
	if _, err := c.SendText(context.Background(), text("<b>x")); err != nil {
		t.Fatal(err)
	}
	calls := f.callsOf("sendMessage")
	if len(calls) != 3 {
		t.Fatalf("want 3 requests, got %d", len(calls))
	}
	if calls[1].body["text"] != "<b>x</b>" || calls[1].body["parse_mode"] != "HTML" {
		t.Fatalf("second request must be the repaired HTML: %v", calls[1].body)
	}
	if _, ok := calls[2].body["parse_mode"]; ok || calls[2].body["text"] != "<b>x" {
		t.Fatalf("third request must be the original as plain text: %v", calls[2].body)
	}
}

func TestOtherRefusalsAreNotRetried(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	f.queue("sendMessage", body(`{"ok":false,"error_code":400,"description":"Bad Request: message to be replied not found"}`))
	_, err := f.client().Reliable().SendText(context.Background(), text("x"))
	if class, _ := Classify(err); class != ClassRejected || !IsInvalidReplyError(err) {
		t.Fatalf("got %v", err)
	}
	if n := len(f.callsOf("sendMessage")); n != 1 {
		t.Fatalf("got %d requests", n)
	}
}

func TestTransportErrorsDoNotLeakTheToken(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	url := srv.URL
	srv.Close()
	c := NewClient(testToken, Options{BaseURL: url, Clock: newFakeClock()})
	_, err := c.SendText(context.Background(), text("x"))
	if err == nil || strings.Contains(err.Error(), "SECRET") {
		t.Fatalf("err=%v", err)
	}
	if class, _ := Classify(err); class != ClassNotSent {
		t.Fatalf("refused connection is not sent, got %v", err)
	}

	hang := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		time.Sleep(300 * time.Millisecond)
	}))
	defer hang.Close()
	c = NewClient(testToken, Options{BaseURL: hang.URL, HTTP: &http.Client{Timeout: 50 * time.Millisecond}, Clock: newFakeClock()})
	_, err = c.SendText(context.Background(), text("x"))
	if err == nil || strings.Contains(err.Error(), "SECRET") || strings.Contains(err.Error(), "/bot") {
		t.Fatalf("err=%v", err)
	}
	if class, _ := Classify(err); class != ClassUnknown {
		t.Fatalf("a timeout after sending is ambiguous, got %v", err)
	}
}

func TestFloodWaitParsing(t *testing.T) {
	zero := int64(0)
	cases := []struct {
		err  APIError
		want time.Duration
	}{
		{APIError{Code: 429, RetryAfter: &zero}, time.Second},
		{APIError{Code: 429}, FallbackRetryAfter},
		{APIError{Code: 429, Description: "Too Many Requests: retry after 17"}, 17 * time.Second},
	}
	for _, c := range cases {
		got, ok := c.err.FloodWait()
		if !ok || got != c.want {
			t.Errorf("%+v: got %v", c.err, got)
		}
	}
	if _, ok := (&APIError{Code: 400}).FloodWait(); ok {
		t.Error("400 is not a flood wait")
	}
}

func TestGetUpdatesRequestsMessagesAndCallbacks(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	f.queue("getUpdates", body(`{"ok":true,"result":[{"update_id":5,"message":{"message_id":1,"chat":{"id":7,"type":"private"},"from":{"id":9,"is_bot":false},"text":"hi","future_field":1}}]}`))
	offset := int64(4)
	updates, err := f.client().GetUpdates(context.Background(), &offset, PollTimeout)
	if err != nil || len(updates) != 1 || *updates[0].Message.Text != "hi" {
		t.Fatalf("updates=%v err=%v", updates, err)
	}
	if !strings.Contains(string(updates[0].Message.Raw), "future_field") {
		t.Fatal("raw message must keep unmodeled fields")
	}
	b := f.callsOf("getUpdates")[0].body
	if b["offset"] != float64(4) || b["timeout"] != float64(30) {
		t.Fatalf("unexpected request %v", b)
	}
	allowed, _ := b["allowed_updates"].([]any)
	if len(allowed) != 2 || allowed[0] != "message" || allowed[1] != "callback_query" {
		t.Fatalf("unexpected allowed_updates %v", allowed)
	}
}

func TestOversizedFilesAreRefusedBeforeReadingOrSending(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	_, err := f.client().SendDocument(context.Background(), chat, Upload{Data: make([]byte, MaxDocumentBytes+1), FileName: "a.bin"}, MediaSend{})
	if err == nil || len(f.callsOf("sendDocument")) != 0 {
		t.Fatalf("err=%v", err)
	}
	if class, _ := Classify(err); class != ClassNotSent {
		t.Fatalf("local refusal is not sent, got %v", class)
	}
	path := filepath.Join(t.TempDir(), "big.bin")
	fh, _ := os.Create(path)
	fh.Truncate(MaxDocumentBytes + 1)
	fh.Close()
	if _, err := ReadUpload(path, MethodDocument); err == nil || !strings.Contains(err.Error(), "exceeds Telegram limit") {
		t.Fatalf("want a size refusal, got %v", err)
	}
}

func TestReadUploadNamesAndTypes(t *testing.T) {
	dir := t.TempDir()
	write := func(name string, data []byte) string {
		p := filepath.Join(dir, name)
		os.WriteFile(p, data, 0o600)
		return p
	}
	png := []byte("\x89PNG\r\n\x1a\n0000")
	cases := []struct {
		path string
		m    FileMethod
		mime string
	}{
		{write("shot.png", png), MethodPhoto, "image/png"},
		{write("notes.txt", []byte("plain text")), MethodDocument, "application/octet-stream"},
		{write("page.html", []byte("  <html><body>x</body></html>")), MethodDocument, "text/html"},
		{write("run.sh", []byte("#!/bin/sh\necho")), MethodDocument, "text/x-shellscript"},
		{write("talk.ogg", []byte("anything")), MethodVoice, "audio/ogg"},
		{write("song.mp3", []byte("no magic")), MethodAudio, "audio/mpeg"},
	}
	for _, c := range cases {
		u, err := ReadUpload(c.path, c.m)
		if err != nil || u.MimeType != c.mime || u.FileName != filepath.Base(c.path) {
			t.Errorf("%s: %+v %v", c.path, u.MimeType, err)
		}
	}
	if _, err := ReadUpload(filepath.Join(dir, "none"), MethodDocument); err == nil {
		t.Error("a missing file is an error")
	}
}

func TestCheckPhotoAcceptsOnlyWhatTelegramTakesAsIs(t *testing.T) {
	jpegOf := func(w, h int) Upload {
		var buf bytes.Buffer
		jpeg.Encode(&buf, image.NewGray(image.Rect(0, 0, w, h)), nil)
		return Upload{Data: buf.Bytes(), MimeType: "image/jpeg"}
	}
	if err := CheckPhoto(jpegOf(800, 600)); err != nil {
		t.Fatalf("a small JPEG passes: %v", err)
	}
	for name, u := range map[string]Upload{
		"too long": jpegOf(1921, 100),
		"too thin": jpegOf(1000, 49),
		"png":      {Data: []byte("x"), MimeType: "image/png"},
	} {
		if err := CheckPhoto(u); !errors.Is(err, ErrPhotoNeedsPreparation) {
			t.Errorf("%s: want ErrPhotoNeedsPreparation, got %v", name, err)
		}
	}
}

func TestFileSendCarriesReplyTargetAndCaptionFallback(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	parseErr := body(`{"ok":false,"error_code":400,"description":"Bad Request: can't parse entities: x"}`)
	f.queue("sendPhoto", parseErr)
	f.queue("sendPhoto", parseErr)
	c := f.client()
	c.Repair = func(s string) string { return s + "!" }
	caption := " <b>cap "
	reply, topic := int64(9), int64(4)
	kb := &InlineKeyboardMarkup{InlineKeyboard: [][]InlineKeyboardButton{{URLButton("Open", "https://x")}}}
	msg, err := c.Reliable().SendFile(context.Background(), chat, MethodPhoto, Upload{Data: []byte("img"), FileName: "a.jpg", MimeType: "image/jpeg"},
		MediaSend{Caption: &caption, ReplyToMessageID: &reply, MessageThreadID: &topic, ReplyMarkup: kb})
	if err != nil || msg.ID != 42 {
		t.Fatalf("msg=%v err=%v", msg, err)
	}
	calls := f.callsOf("sendPhoto")
	if len(calls) != 3 {
		t.Fatalf("want HTML, repaired, plain: %d calls", len(calls))
	}
	first, second, third := calls[0].body, calls[1].body, calls[2].body
	if first["caption"] != "<b>cap" || first["parse_mode"] != "HTML" || first["photo"] != "a.jpg|image/jpeg" ||
		first["reply_to_message_id"] != "9" || first["allow_sending_without_reply"] != "true" || first["message_thread_id"] != "4" ||
		!strings.Contains(first["reply_markup"].(string), "https://x") {
		t.Fatalf("unexpected first request %v", first)
	}
	if second["caption"] != "<b>cap!" || second["parse_mode"] != "HTML" {
		t.Fatalf("unexpected repaired request %v", second)
	}
	if _, ok := third["parse_mode"]; ok || third["caption"] != "<b>cap" {
		t.Fatalf("unexpected plain request %v", third)
	}
}

func TestAmbiguousFileSendIsUnknownAndNotResent(t *testing.T) {
	f := newFakeTelegram(t, newFakeClock())
	f.queue("sendVoice", body(`garbage`))
	_, err := f.client().Reliable().SendFile(context.Background(), chat, MethodVoice, Upload{Data: []byte("v"), FileName: "v.ogg", MimeType: "audio/ogg"}, MediaSend{})
	if class, _ := Classify(err); class != ClassUnknown {
		t.Fatalf("got %v", err)
	}
	if n := len(f.callsOf("sendVoice")); n != 1 {
		t.Fatalf("got %d sends", n)
	}
}

func TestBotIdentity(t *testing.T) {
	if BotIdentity(testToken) != "123456" || BotIdentity("abc:x") != "unknown" || BotIdentity("") != "unknown" {
		t.Fatal("bad identity")
	}
}
