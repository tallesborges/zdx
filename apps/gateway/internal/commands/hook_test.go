package commands

import (
	"context"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

func request(text string) bot.CommandRequest {
	forum := true
	topic := topicID
	return bot.CommandRequest{
		Message: &telegram.Message{ID: 30, Chat: telegram.Chat{ID: forumChat, Type: "supergroup", IsForum: &forum}, From: &telegram.User{ID: privateID}, ThreadID: &topic},
		Text:    text,
		Topic:   bot.TopicInfo{ThreadID: threadOf7, Kind: bot.TopicOrdinary},
		ReplyTo: ptr(int64(30)),
	}
}

func TestHookClaimsKnownCommandsOnly(t *testing.T) {
	tg := newTG()
	k := NewHook(&Handler{Telegram: tg})
	for _, text := range []string{"/status", "/model", "/model list", "/queue later", "/restart f", "/launcher@zdx_bot"} {
		if !k.HandleCommand(context.Background(), request(text)) {
			t.Errorf("%q must be claimed", text)
		}
	}
	if got := tg.of("send")[0].text; got != "This gateway build cannot handle /status yet." {
		t.Fatalf("refusal %q", got)
	}
	if got := tg.of("send")[2].text; got != "This gateway build cannot handle /model yet." {
		t.Fatalf("model refusal %q", got)
	}
	for _, text := range []string{"/foo", "/cancel", "/handoff please"} {
		if k.HandleCommand(context.Background(), request(text)) {
			t.Errorf("%q runs as a turn, as in the Rust bot", text)
		}
	}

	k.Handler.Chats = fakeChats{}
	if !k.HandleCommand(context.Background(), request("/threadid")) {
		t.Fatal("/threadid runs")
	}
}

func TestHookTldrDoesNotHoldTheTopic(t *testing.T) {
	tg := newTG()
	gen := &fakeGen{text: "recap"}
	k := NewHook(&Handler{Telegram: tg, Generator: gen})
	if !k.HandleCommand(context.Background(), request("/tldr")) {
		t.Fatal("/tldr claimed")
	}
	k.Wait()
	if len(tg.of("edit")) != 1 {
		t.Fatal("the recap landed")
	}
}

func TestHookHandoffReadsTheSourceProfile(t *testing.T) {
	r := newStaging(Thread{ID: threadOf7, Kind: KindProfile, Profile: OrchestratorProfile})
	k := NewHook(r.h)
	req := request("/handoff")
	req.Topic.Kind = bot.TopicProfile
	k.HandleCommand(context.Background(), req)
	if c, ok := r.h.Card(threadOf7); !ok || c.Profile != OrchestratorProfile {
		t.Fatalf("card %+v", c)
	}

	// Plain staged input comes in through StagedInput.
	r.gen.text = "ctx"
	in := request("carry on")
	in.Message.ID = 31
	if !k.StagedInput(context.Background(), in, "", false) {
		t.Fatal("staged input consumed")
	}
	if len(r.d.sent) != 1 {
		t.Fatal("handoff seeded")
	}
	if k.StagedInput(context.Background(), request("hello"), "", false) {
		t.Fatal("no session: a normal turn")
	}
}

func TestHookAnswersOwnedButtons(t *testing.T) {
	tg := newTG()
	k := NewHook(&Handler{Telegram: tg})
	for _, data := range []string{"nt:custom", "model_cancel:topic", "cancel_q:1:2", "thread_header:refresh", "wk:c"} {
		if !k.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), 5, data)) {
			t.Errorf("%q is owned", data)
		}
	}
	if k.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), 5, "fu:0")) {
		t.Fatal("follow-ups are the bot's")
	}
	k.Wait()
	for _, a := range tg.answers() {
		if a != "Not available in this gateway build yet" {
			t.Fatalf("answers %v", tg.answers())
		}
	}
	if len(tg.answers()) != 5 {
		t.Fatalf("every owned button is answered: %v", tg.answers())
	}
}

type recordingBot struct{ in []bot.Incoming }

func (b *recordingBot) Dispatch(_ context.Context, in bot.Incoming) { b.in = append(b.in, in) }

func TestBotDispatchBuildsTheSyntheticMessage(t *testing.T) {
	b := &recordingBot{}
	BotDispatch{Bot: b}.Dispatch(context.Background(), Synthetic{Chat: forumChat, ChatType: "supergroup", IsForum: true, Topic: ptr(int64(501)), MessageID: 501, UserID: privateID, Text: "seed"})
	if !b.in[0].Synthetic {
		t.Fatal("a bot-made message is marked synthetic")
	}
	m := b.in[0].Message
	if m.ID != 501 || *m.Text != "seed" || m.From.ID != privateID || !m.Chat.IsForumEnabled() || *m.EffectiveThreadID() != 501 {
		t.Fatalf("message %+v", m)
	}
}

type fakeRoutes struct{ route engine.ThreadRoute }

func (f fakeRoutes) ResolveThread(context.Context, int64, *int64) (engine.ThreadRoute, error) {
	return f.route, nil
}

func TestEngineThreadsUsesTheEffectiveThread(t *testing.T) {
	profile := OrchestratorProfile
	th, err := EngineThreads{Engine: fakeRoutes{engine.ThreadRoute{ThreadID: "telegram-1-topic-2", EffectiveThreadID: "source", Kind: engine.KindProfile, Profile: &profile}}}.
		ResolveThread(context.Background(), 1, ptr(int64(2)))
	if err != nil || th != (Thread{ID: "source", Kind: KindProfile, Profile: OrchestratorProfile}) {
		t.Fatalf("thread %+v %v", th, err)
	}
}

func TestCommandsFollowTheLoadedMedia(t *testing.T) {
	str := func(s string) *string { return &s }
	cases := []struct {
		name    string
		media   func(m *telegram.Message)
		loaded  bool
		command bool
	}{
		{"pdf caption", func(m *telegram.Message) { m.Document = &telegram.File{FileID: "d", MimeType: str("application/pdf")} }, false, true},
		{"photo that failed to load", func(m *telegram.Message) { m.Photo = []telegram.PhotoSize{{FileID: "p"}} }, false, true},
		{"loaded photo", func(m *telegram.Message) { m.Photo = []telegram.PhotoSize{{FileID: "p"}} }, true, false},
		// The head carries only the caption; a later album member's image
		// loaded.
		{"image in a later album member", func(*telegram.Message) {}, true, false},
	}
	for _, c := range cases {
		tg := newTG()
		k := NewHook(&Handler{Telegram: tg})
		req := request("/threadid")
		c.media(req.Message)
		req.HasLoadedImagesOrAudio = c.loaded
		claimed := k.HandleCommand(context.Background(), req)
		ran := len(tg.of("send")) == 1 && strings.Contains(tg.of("send")[0].text, "Thread ID")
		if ran != c.command {
			t.Errorf("%s: command ran %v, want %v (claimed %v)", c.name, ran, c.command, claimed)
		}
	}
}

func TestSyntheticMessagesNeverStageOrAnswer(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	k := NewHook(r.h)

	// A synthetic staged command runs as an ordinary prompt.
	for _, text := range []string{"/btw", "/handoff", "/prompt_builder"} {
		req := request(text)
		req.Synthetic = true
		if k.HandleCommand(context.Background(), req) {
			t.Errorf("synthetic %s must be forwarded as a prompt", text)
		}
	}
	if r.h.HasLiveSession(threadOf7) || len(r.tg.of("send")) != 0 {
		t.Fatal("no session starts and nothing is answered")
	}

	// Other commands from a synthetic message still run, as in Rust.
	req := request("/threadid")
	req.Synthetic = true
	if !k.HandleCommand(context.Background(), req) {
		t.Fatal("a synthetic /threadid still runs")
	}

	// A live session never takes synthetic input, in either path.
	if !k.HandleCommand(context.Background(), request("/btw")) {
		t.Fatal("/btw starts")
	}
	in := request("a picker prompt")
	in.Message.ID = 31
	in.Synthetic = true
	if k.StagedInput(context.Background(), in, "", false) || k.StagedInput(context.Background(), request("x"), "", true) {
		t.Fatal("synthetic input is not staged input")
	}
	cancel := request("/cancel")
	cancel.Message.ID = 32
	cancel.Synthetic = true
	if k.HandleCommand(context.Background(), cancel) || !r.h.HasLiveSession(threadOf7) {
		t.Fatal("a synthetic /cancel does not end the session")
	}
	if len(r.d.sent) != 0 || len(r.topics.names) != 0 {
		t.Fatal("no side topic opened")
	}
	if r.h.AwaitingBtwInput(threadOf7, true) || !r.h.AwaitingBtwInput(threadOf7, false) {
		t.Fatal("only a user message skips the queue as the /btw question")
	}
}
