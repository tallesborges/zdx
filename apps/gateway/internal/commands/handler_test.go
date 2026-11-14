package commands

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

type call struct {
	method string
	chat   int64
	id     int64
	text   string
	topic  *int64
	reply  *int64
	markup *telegram.InlineKeyboardMarkup
	answer *string
}

type fakeTG struct {
	mu     sync.Mutex
	next   int64
	calls  []call
	failOn map[string]error
}

func newTG() *fakeTG { return &fakeTG{next: 1000} }

func (f *fakeTG) record(c call) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.calls = append(f.calls, c)
	return f.failOn[c.method]
}

func (f *fakeTG) SendText(_ context.Context, s telegram.TextSend) (*telegram.Message, error) {
	f.mu.Lock()
	f.next++
	id := f.next
	f.mu.Unlock()
	if err := f.record(call{method: "send", chat: s.ChatID, id: id, text: s.Text, topic: s.MessageThreadID, reply: s.ReplyToMessageID, markup: s.ReplyMarkup}); err != nil {
		return nil, err
	}
	return &telegram.Message{ID: id, Chat: telegram.Chat{ID: s.ChatID}}, nil
}

func (f *fakeTG) EditMessageText(_ context.Context, chat, id int64, text string, markup *telegram.InlineKeyboardMarkup) error {
	return f.record(call{method: "edit", chat: chat, id: id, text: text, markup: markup})
}

func (f *fakeTG) EditMessageReplyMarkup(_ context.Context, chat, id int64, markup *telegram.InlineKeyboardMarkup) error {
	return f.record(call{method: "markup", chat: chat, id: id, markup: markup})
}

func (f *fakeTG) DeleteMessage(_ context.Context, chat, id int64) error {
	return f.record(call{method: "delete", chat: chat, id: id})
}

func (f *fakeTG) AnswerCallbackQuery(_ context.Context, id string, text *string) error {
	return f.record(call{method: "answer", answer: text})
}

func (f *fakeTG) of(method string) []call {
	f.mu.Lock()
	defer f.mu.Unlock()
	var out []call
	for _, c := range f.calls {
		if c.method == method {
			out = append(out, c)
		}
	}
	return out
}

func (f *fakeTG) lastSend(t *testing.T) call {
	t.Helper()
	s := f.of("send")
	if len(s) == 0 {
		t.Fatal("nothing sent")
	}
	return s[len(s)-1]
}

func (f *fakeTG) answers() []string {
	var out []string
	for _, c := range f.of("answer") {
		if c.answer == nil {
			out = append(out, "")
		} else {
			out = append(out, *c.answer)
		}
	}
	return out
}

func (f *fakeTG) deleted() []int64 {
	var out []int64
	for _, c := range f.of("delete") {
		out = append(out, c.id)
	}
	return out
}

type fakeChats struct {
	binding      Binding
	allowed      map[int64]bool
	url          string
	orchestrator bool
}

func (c fakeChats) OrchestratorChat(int64) bool { return c.orchestrator }

func (c fakeChats) Binding(int64) Binding       { return c.binding }
func (c fakeChats) ChatAllowed(chat int64) bool { return c.allowed[chat] }
func (c fakeChats) MiniAppURL(int64) string     { return c.url }

type fakeResolver struct {
	thread Thread
	err    error
}

func (r fakeResolver) ResolveThread(context.Context, int64, *int64) (Thread, error) {
	return r.thread, r.err
}

type fakeDispatch struct {
	mu   sync.Mutex
	sent []Synthetic
}

func (d *fakeDispatch) Dispatch(_ context.Context, s Synthetic) {
	d.mu.Lock()
	defer d.mu.Unlock()
	d.sent = append(d.sent, s)
}

type fakeTopics struct {
	mu        sync.Mutex
	next      int64
	names     []string
	err       error
	deleted   []int64
	deleteErr error
	pinned    []int64
}

func (f *fakeTopics) DeleteForumTopic(_ context.Context, _ int64, topic int64) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.deleted = append(f.deleted, topic)
	return f.deleteErr
}

func (f *fakeTopics) PinChatMessage(_ context.Context, _ int64, id int64) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.pinned = append(f.pinned, id)
	return nil
}

func (f *fakeTopics) CreateForumTopic(_ context.Context, _ int64, name string) (int64, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	if f.err != nil {
		return 0, f.err
	}
	f.next++
	f.names = append(f.names, name)
	return f.next, nil
}

type fakeSeeds struct {
	start    StartModel
	startErr error
}

type fakeThreads struct {
	mu     sync.Mutex
	setups []ThreadSetup
	err    error
}

func (f *fakeThreads) SetupThread(_ context.Context, s ThreadSetup) error {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.setups = append(f.setups, s)
	return f.err
}

func (s *fakeSeeds) StartModel(context.Context, int64, string, string) (StartModel, error) {
	return s.start, s.startErr
}

func (s *fakeSeeds) SideQuestionSeed(_ context.Context, source, question string) (string, error) {
	return "[side question from thread " + source + "]\n\n" + question, nil
}

type fakeGen struct {
	text string
	err  error
	got  []string
}

func (g *fakeGen) TLDR(_ context.Context, _ int64, thread string) (string, error) {
	g.got = append(g.got, "tldr:"+thread)
	return g.text, g.err
}

func (g *fakeGen) Handoff(_ context.Context, _ int64, thread, goal string) (string, error) {
	g.got = append(g.got, "handoff:"+thread+":"+goal)
	return g.text, g.err
}

func (g *fakeGen) PromptBuilder(_ context.Context, _ int64, thread, intent string) (string, error) {
	g.got = append(g.got, "prompt:"+thread+":"+intent)
	return g.text, g.err
}

const (
	forumChat int64 = -1001234567890
	topicID   int64 = 7
	threadOf7       = "telegram--1001234567890-topic-7"
	privateID int64 = 42
)

func ptr[T any](v T) *T { return &v }

func topicMsg(id int64, text string) Message {
	return Message{Chat: forumChat, ChatType: "supergroup", IsForum: true, Topic: ptr(topicID), ID: id, UserID: privateID, Text: text, ReplyTo: ptr(id)}
}

func ordinary() Thread { return Thread{ID: threadOf7, Kind: KindOrdinary} }

func callback(chat int64, topic *int64, messageID int64, data string) *telegram.CallbackQuery {
	forum := topic != nil
	typ := "supergroup"
	if chat > 0 {
		typ = "private"
	}
	return &telegram.CallbackQuery{
		ID: "q", From: telegram.User{ID: privateID}, Data: &data,
		Message: &telegram.Message{ID: messageID, Chat: telegram.Chat{ID: chat, Type: typ, IsForum: &forum}, ThreadID: topic},
	}
}

func buttons(m *telegram.InlineKeyboardMarkup) string {
	if m == nil {
		return "<nil>"
	}
	var rows []string
	for _, row := range m.InlineKeyboard {
		var cells []string
		for _, b := range row {
			data := ""
			switch {
			case b.CallbackData != nil:
				data = *b.CallbackData
			case b.URL != nil:
				data = *b.URL
			}
			cells = append(cells, b.Text+"="+data)
		}
		rows = append(rows, strings.Join(cells, ","))
	}
	return strings.Join(rows, " | ")
}

func TestImmediateCommands(t *testing.T) {
	tg := newTG()
	h := &Handler{Telegram: tg, Chats: fakeChats{binding: Binding{Profile: "work", Root: "/srv/a&b"}, allowed: map[int64]bool{forumChat: true}}}

	if !h.HandleMessage(context.Background(), topicMsg(10, "/threadid@zdx_bot"), ordinary()) {
		t.Fatal("/threadid not handled")
	}
	if got := tg.lastSend(t); got.text != "<b>Thread ID</b> <i>(tap to copy)</i>\n<code>"+threadOf7+"</code>" || *got.topic != topicID || *got.reply != 10 {
		t.Fatalf("threadid reply %+v", got)
	}

	h.HandleMessage(context.Background(), topicMsg(11, "/threads"), ordinary())
	if got := tg.lastSend(t).text; got != ThreadsNotConfigured {
		t.Fatalf("threads without a Mini App: %q", got)
	}
	h.Chats = fakeChats{binding: Binding{Profile: "work", Root: "/srv/a&b"}, allowed: map[int64]bool{forumChat: true}, url: "https://app.example/app"}
	h.HandleMessage(context.Background(), topicMsg(12, "/thread"), ordinary())
	if got := buttons(tg.lastSend(t).markup); got != "💬 Open Thread in Mini App=https://app.example/app?startapp="+threadOf7 {
		t.Fatalf("threads keyboard %s", got)
	}

	h.HandleMessage(context.Background(), topicMsg(13, "/whereami"), ordinary())
	want := "<b>Where am I</b>\nChat ID: <code>-1001234567890</code>\nTopic ID: <code>7</code>\nProfile: <code>work</code>\nCWD: <code>/srv/a&amp;b</code>"
	if got := tg.lastSend(t).text; got != want {
		t.Fatalf("whereami:\n%s", got)
	}

	// A group off the allowlist learns its ids, never the bot's folders.
	other := topicMsg(14, "/whereami")
	other.Chat = -1009
	h.HandleMessage(context.Background(), other, ordinary())
	if got := tg.lastSend(t).text; strings.Contains(got, "/srv") || !strings.Contains(got, "chat not on bot allowlist") || !strings.Contains(got, "zdx bot profile add &lt;name&gt; -1009 &lt;cwd&gt;") {
		t.Fatalf("discovery whereami:\n%s", got)
	}

	// A private chat is trusted; without a profile it shows the fallback.
	h.Chats = fakeChats{binding: Binding{Root: "/home/u"}}
	h.HandleMessage(context.Background(), Message{Chat: privateID, ChatType: "private", ID: 15, UserID: privateID, Text: "/whereami"}, Thread{ID: "telegram-42", Kind: KindOrdinary})
	if got := tg.lastSend(t).text; !strings.Contains(got, "Profile: <code>none</code> (fallback root)\nCWD: <code>/home/u</code>\nBind this chat with:") {
		t.Fatalf("fallback whereami:\n%s", got)
	}

	h.HandleMessage(context.Background(), topicMsg(16, "/launcher"), ordinary())
	if got := tg.lastSend(t).text; got != LauncherOutsideGeneral {
		t.Fatalf("launcher in a topic: %q", got)
	}

	general := topicMsg(17, "/handoff")
	general.Topic = nil
	if !h.HandleMessage(context.Background(), general, Thread{ID: "telegram--1001234567890", Kind: KindOrdinary}) {
		t.Fatal("general refusal not handled")
	}
	if got := tg.lastSend(t).text; got != "/handoff must be used inside a topic, not General." {
		t.Fatalf("general refusal %q", got)
	}

	for _, text := range []string{"/status", "/new", "/model", "/worktree", "/restart", "hello", "/tldr", "/commands", "/btw"} {
		if h.HandleMessage(context.Background(), topicMsg(18, text), ordinary()) {
			t.Errorf("%q must be left to the bot without its dependencies", text)
		}
	}
	media := topicMsg(19, "/threadid")
	media.HasMedia = true
	if h.HandleMessage(context.Background(), media, ordinary()) {
		t.Fatal("a command is never read from a message with media")
	}
	if h.HandleMessage(context.Background(), topicMsg(20, "/threadid"), Thread{ID: "w", Kind: KindWorkerMirror}) {
		t.Fatal("mirror topics go to the worker verbatim")
	}
}

func TestTldrEditsThePlaceholder(t *testing.T) {
	tg := newTG()
	gen := &fakeGen{text: "**Done**: <ok>"}
	h := &Handler{Telegram: tg, Generator: gen}
	h.HandleMessage(context.Background(), topicMsg(10, "/tldr"), ordinary())
	send := tg.lastSend(t)
	if send.text != TldrPending || send.markup == nil || len(send.markup.InlineKeyboard) != 0 {
		t.Fatalf("placeholder %+v", send)
	}
	edits := tg.of("edit")
	if len(edits) != 1 || edits[0].id != send.id || edits[0].text != "📝 <b>TLDR</b>\n<b>Done</b>: &lt;ok&gt;" {
		t.Fatalf("edit %+v", edits)
	}
	if gen.got[0] != "tldr:"+threadOf7 {
		t.Fatalf("generated for %v", gen.got)
	}

	gen.err = errors.New("generate: <unsupported>")
	h.HandleMessage(context.Background(), topicMsg(11, "/tldr"), ordinary())
	if got := tg.of("edit")[1].text; got != "⚠️ TLDR failed:\n<pre>generate: &lt;unsupported&gt;</pre>" {
		t.Fatalf("failure edit %q", got)
	}
}

type fakeCommands struct {
	list []CustomCommand
	err  error
}

func (f fakeCommands) CustomCommands(_ context.Context, _ int64, _ string, native []string) ([]CustomCommand, error) {
	if len(native) == 0 {
		return nil, errors.New("native names missing")
	}
	return f.list, f.err
}

func TestCommandPickerIsOneShot(t *testing.T) {
	tg := newTG()
	d := &fakeDispatch{}
	h := &Handler{Telegram: tg, Dispatch: d, Commands: fakeCommands{list: []CustomCommand{
		{Name: "bundled", Source: "builtin", Content: "b"},
		{Name: "mine", Description: "a <b>", Source: "user", Content: "m"},
		{Name: "proj", Source: "project", Content: "run the project check"},
	}}}
	h.HandleMessage(context.Background(), topicMsg(10, "/commands"), ordinary())
	send := tg.lastSend(t)
	wantBody := "🧰 <b>Project commands</b>\n• /proj <i>(project)</i>\n• /mine — a &lt;b&gt; <i>(user)</i>\n• /bundled <i>(builtin)</i>"
	if send.text != wantBody {
		t.Fatalf("body:\n%s", send.text)
	}
	if got := buttons(send.markup); got != "/proj=cmd:0,/mine=cmd:1 | /bundled=cmd:2 | ✕ Dismiss=cmd:x" {
		t.Fatalf("keyboard %s", got)
	}

	h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), send.id, "cmd:0"))
	if e := tg.of("edit"); len(e) != 1 || e[0].text != "▶️ /proj" {
		t.Fatalf("tap edit %+v", e)
	}
	if len(d.sent) != 1 || d.sent[0].Text != "run the project check" || d.sent[0].MessageID != send.id || *d.sent[0].Topic != topicID || d.sent[0].ChatType != "supergroup" || !d.sent[0].IsForum {
		t.Fatalf("dispatched %+v", d.sent)
	}
	h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), send.id, "cmd:1"))
	if a := tg.answers(); a[len(a)-1] != "This picker is no longer active" || len(d.sent) != 1 {
		t.Fatalf("second tap %v", a)
	}

	h.HandleMessage(context.Background(), topicMsg(11, "/commands"), ordinary())
	second := tg.lastSend(t)
	h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), second.id, "cmd:x"))
	if del := tg.deleted(); len(del) != 1 || del[0] != second.id {
		t.Fatalf("dismiss deleted %v", del)
	}

	h.Commands = fakeCommands{err: errors.New("load commands: denied")}
	h.HandleMessage(context.Background(), topicMsg(12, "/commands"), ordinary())
	if got := tg.lastSend(t).text; got != "❌ load commands: denied" {
		t.Fatalf("error reply %q", got)
	}
}

func TestPickerBodyIsCut(t *testing.T) {
	var many []CustomCommand
	for i := range 400 {
		many = append(many, CustomCommand{Name: fmt.Sprintf("c%03d", i), Source: "user"})
	}
	body := PickerBody(many)
	if n := len([]rune(body)); n != maxPickerChars || !strings.HasSuffix(body, "…") {
		t.Fatalf("body has %d characters", n)
	}
}

type fakeWorkers struct {
	got []string
	err error
}

func (w *fakeWorkers) CancelWorker(_ context.Context, id string) error {
	w.got = append(w.got, id)
	return w.err
}

func TestCancelWorkerButton(t *testing.T) {
	tg := newTG()
	w := &fakeWorkers{}
	mirror := fakeResolver{thread: Thread{ID: "worker-1", Kind: KindWorkerMirror}}
	h := &Handler{Telegram: tg, Workers: w, Resolver: mirror}
	press := func(topic *int64, data string) string {
		t.Helper()
		h.HandleCallback(context.Background(), callback(forumChat, topic, 5, data))
		a := tg.answers()
		return a[len(a)-1]
	}

	if a := press(ptr(topicID), "wk:c"); a != "Cancelling worker…" {
		t.Fatalf("cancel %q", a)
	}
	if a := press(nil, "wk:c"); a != "Not inside a worker topic" {
		t.Fatalf("outside a topic %q", a)
	}
	if a := press(ptr(topicID), "wk:z"); a != "Unknown action" {
		t.Fatalf("unknown %q", a)
	}

	// The service refused: it does not manage the worker. That says
	// nothing about whether the worker runs, so it is not "idle".
	w.err = &engine.ServiceError{Message: "Worker 'worker-1' is not managed"}
	if a := press(ptr(topicID), "wk:c"); a != "This worker is not managed by the service" {
		t.Fatalf("not managed %q", a)
	}
	// Any other refusal is shown as the service gave it, never as idle.
	w.err = &engine.ServiceError{Message: "worker is busy", Code: "busy"}
	if a := press(ptr(topicID), "wk:c"); a != "Couldn't cancel the worker: worker is busy" {
		t.Fatalf("other refusal %q", a)
	}
	w.err = &engine.ServiceError{Message: strings.Repeat("x", 300)}
	if a := press(ptr(topicID), "wk:c"); len([]rune(a)) != 200 || !strings.HasPrefix(a, "Couldn't cancel the worker: x") {
		t.Fatalf("long refusal is cut to an answer: %d", len([]rune(a)))
	}
	// The service is down or did not answer: the outcome is unknown.
	w.err = fmt.Errorf("%w: connect: no such file", engine.ErrRequestNotSent)
	if a := press(ptr(topicID), "wk:c"); a != "Couldn't cancel the worker; try again" {
		t.Fatalf("down service %q", a)
	}
	w.err = engine.ErrNoAnswer
	if a := press(ptr(topicID), "wk:c"); a != "Couldn't cancel the worker; try again" {
		t.Fatalf("no answer %q", a)
	}
	if len(w.got) != 6 || w.got[5] != "worker-1" {
		t.Fatalf("cancelled %v", w.got)
	}

	// A topic Rust says is no mirror is not a worker topic; a failed
	// lookup claims nothing about the topic and cancels nothing.
	h.Resolver = fakeResolver{thread: ordinary()}
	if a := press(ptr(topicID), "wk:c"); a != "Not a worker topic" {
		t.Fatalf("ordinary topic %q", a)
	}
	h.Resolver = fakeResolver{err: engine.ErrNotRunning}
	if a := press(ptr(topicID), "wk:c"); a != "Couldn't check this topic; try again" {
		t.Fatalf("lookup failure %q", a)
	}
	if len(w.got) != 6 {
		t.Fatalf("no cancel after a failed lookup: %v", w.got)
	}
}

func TestTopicLinkAndNames(t *testing.T) {
	if url, ok := TopicLink(-1001234567890, 42); !ok || url != "https://t.me/c/1234567890/42" {
		t.Fatalf("link %q", url)
	}
	for _, c := range [][2]int64{{5678, 42}, {-100, 42}, {-1001, 0}} {
		if _, ok := TopicLink(c[0], c[1]); ok {
			t.Errorf("%v has no link", c)
		}
	}
	cases := map[string]string{
		"Add status icon":              "🎛️ Add status icon",
		"  Handoff 2026-09-11 00:12  ": "🎛️ Handoff 2026-09-11 00:12",
		"🎛️ Add status icon":           "🎛️ Add status icon",
		"🎛 Add status icon":            "🎛 Add status icon",
	}
	for in, want := range cases {
		if got := OrchestratorTopicName(in); got != want {
			t.Errorf("%q: %q", in, got)
		}
	}
}

// fixed returns a clock that a test can move.
func fixed(at time.Time) (func() time.Time, func(time.Duration)) {
	var mu sync.Mutex
	now := at
	return func() time.Time {
			mu.Lock()
			defer mu.Unlock()
			return now
		}, func(d time.Duration) {
			mu.Lock()
			defer mu.Unlock()
			now = now.Add(d)
		}
}
