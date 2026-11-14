package commands

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

type stagingRig struct {
	tg      *fakeTG
	d       *fakeDispatch
	topics  *fakeTopics
	seeds   *fakeSeeds
	threads *fakeThreads
	gen     *fakeGen
	h       *Handler
	advance func(time.Duration)
}

func newStaging(thread Thread) *stagingRig {
	now, advance := fixed(time.Date(2026, 11, 13, 9, 30, 0, 0, time.UTC))
	r := &stagingRig{tg: newTG(), d: &fakeDispatch{}, topics: &fakeTopics{next: 500}, seeds: &fakeSeeds{}, threads: &fakeThreads{}, gen: &fakeGen{}, advance: advance}
	r.h = &Handler{
		Telegram: r.tg, Dispatch: r.d, Topics: r.topics, Threads: r.threads, Seeds: r.seeds, Generator: r.gen,
		Resolver: fakeResolver{thread: thread}, Now: now,
	}
	return r
}

func (r *stagingRig) msg(t *testing.T, id int64, text string, thread Thread) bool {
	t.Helper()
	return r.h.HandleMessage(context.Background(), topicMsg(id, text), thread)
}

func TestBtwOpensTheSideTopicWithoutAccept(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	if !r.msg(t, 10, "/btw", th) {
		t.Fatal("/btw not handled")
	}
	ask := r.tg.lastSend(t)
	if !strings.HasPrefix(ask.text, "💬 <b>Side question</b>\nSend your question") || buttons(ask.markup) != "🎛 Model=stg:m,🗑 Discard=stg:d" {
		t.Fatalf("ask card %q %s", ask.text, buttons(ask.markup))
	}
	if !r.h.AwaitingBtwInput(threadOf7, false) || r.h.AwaitingBtwInput(threadOf7, true) || !r.h.HasLiveSession(threadOf7) {
		t.Fatal("a live /btw session waits for its question")
	}

	// A message already in flight when the command came is not its input.
	if r.msg(t, 9, "older", th) {
		t.Fatal("an older message is not staged input")
	}
	synthetic := topicMsg(11, "feedback")
	synthetic.Synthetic = true
	if r.h.HandleMessage(context.Background(), synthetic, th) {
		t.Fatal("a synthetic prompt is never staged input")
	}

	if !r.msg(t, 12, "why <T>?", th) {
		t.Fatal("the question is consumed")
	}
	if len(r.topics.names) != 1 || r.topics.names[0] != "Btw 2026-11-13 09:30" {
		t.Fatalf("topics %v", r.topics.names)
	}
	newThread := "telegram--1001234567890-topic-501"
	if len(r.threads.setups) != 1 || r.threads.setups[0] != (ThreadSetup{ThreadID: newThread, HandoffFrom: threadOf7, PendingTitle: true}) {
		t.Fatalf("recorded %+v", r.threads.setups)
	}
	sends := r.tg.of("send")
	record := sends[len(sends)-2]
	if *record.topic != 501 || record.text != "💬 <b>Side question</b>\n<blockquote>why &lt;T&gt;?</blockquote>\n↩ <a href=\"https://t.me/c/1234567890/7\">Back to the source topic</a>" {
		t.Fatalf("record %+v", record)
	}
	link := sends[len(sends)-1]
	if *link.topic != topicID || link.reply != nil || link.text != "💬 <b>Side question created</b> → <a href=\"https://t.me/c/1234567890/501\">Btw 2026-11-13 09:30</a>" {
		t.Fatalf("link %+v", link)
	}
	if len(r.d.sent) != 1 || r.d.sent[0].Text != "[side question from thread "+threadOf7+"]\n\nwhy <T>?" || r.d.sent[0].MessageID != 501 || *r.d.sent[0].Topic != 501 {
		t.Fatalf("seed %+v", r.d.sent)
	}
	if got := r.tg.deleted(); len(got) != 3 || got[0] != ask.id || got[1] != 10 || got[2] != 12 {
		t.Fatalf("cleanup deleted %v", got)
	}
	if r.h.HasLiveSession(threadOf7) {
		t.Fatal("the session is done")
	}
}

func TestBtwFailureKeepsTheSession(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	r.msg(t, 10, "/btw", th)
	r.topics.err = errors.New("not enough rights")
	r.msg(t, 11, "q", th)
	if got := r.tg.lastSend(t); !strings.Contains(got.text, "Couldn't open the side topic:\n<pre>create forum topic: not enough rights</pre>") || *got.reply != 11 {
		t.Fatalf("failure %+v", got)
	}
	if !r.h.HasLiveSession(threadOf7) || len(r.d.sent) != 0 {
		t.Fatal("the next message retries")
	}
	r.topics.err = nil
	r.msg(t, 12, "q again", th)
	if len(r.d.sent) != 1 {
		t.Fatal("retry opened the topic")
	}
}

func TestStagingNeedsAForumTopicAndExpires(t *testing.T) {
	th := Thread{ID: "telegram-42", Kind: KindOrdinary}
	r := newStaging(th)
	r.h.HandleMessage(context.Background(), Message{Chat: privateID, ChatType: "private", ID: 3, UserID: privateID, Text: "/handoff"}, th)
	if got := r.tg.lastSend(t).text; got != "/handoff needs a forum topic (a group with topics enabled)." {
		t.Fatalf("private /handoff %q", got)
	}

	ot := ordinary()
	r.msg(t, 10, "/prompt_builder", ot)
	r.advance(StagingTTL + time.Second)
	if r.msg(t, 11, "late", ot) {
		t.Fatal("an expired session lets the message run as a turn")
	}
	if got := r.tg.deleted(); len(got) != 2 {
		t.Fatalf("expired cleanup %v", got)
	}

	r.msg(t, 20, "/prompt_builder", ot)
	if !r.msg(t, 21, " /cancel ", ot) {
		t.Fatal("/cancel is consumed")
	}
	if r.h.HasLiveSession(threadOf7) || len(r.tg.deleted()) != 5 {
		t.Fatalf("cancel deleted %v", r.tg.deleted())
	}
	if r.msg(t, 22, "/cancel", ot) {
		t.Fatal("/cancel without a session is a normal message")
	}
}

func TestHandoffRoleModelAndFailClosed(t *testing.T) {
	th := Thread{ID: threadOf7, Kind: KindProfile, Profile: OrchestratorProfile}
	r := newStaging(th)
	r.msg(t, 10, "/handoff", th)
	card := r.tg.lastSend(t)
	if buttons(card.markup) != "🎛 Model=stg:m,Continue as: Orchestrator=stg:r | 🗑 Discard=stg:d" {
		t.Fatalf("handoff card %s", buttons(card.markup))
	}
	if c, ok := r.h.Card(threadOf7); !ok || c.Profile != OrchestratorProfile || c.Model != nil {
		t.Fatalf("card %+v", c)
	}

	// The role toggles in place; the text stays.
	r.h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), card.id, "stg:r"))
	if m := r.tg.of("markup"); len(m) != 1 || !strings.Contains(buttons(m[0].markup), "Continue as: Agent=stg:r") {
		t.Fatalf("role edit %+v", m)
	}
	r.h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), card.id, "stg:r"))
	if a := r.tg.answers(); a[0] != "The new topic continues as Agent" || a[1] != "The new topic continues as Orchestrator" {
		t.Fatalf("answers %v", a)
	}

	// The model picker is not built: the button says so.
	r.h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), card.id, "stg:m"))
	if a := r.tg.answers(); a[2] != "Not available in this gateway build yet" {
		t.Fatalf("picker answer %v", a)
	}
	pick := "openai:gpt-6@high"
	if !r.h.FinishModelPick(context.Background(), threadOf7, forumChat, card.id, &pick) {
		t.Fatal("the session is live")
	}
	if e := r.tg.of("edit"); !strings.HasSuffix(e[0].text, "\n\nNew topic model: <code>openai:gpt-6@high</code>") {
		t.Fatalf("card after pick %q", e[0].text)
	}

	// Generation fails: the session stays, the card offers the role again.
	r.gen.err = errors.New("generate unsupported")
	r.msg(t, 11, "continue the release", th)
	e := r.tg.of("edit")
	if last := e[len(e)-1]; last.text != "⚠️ Handoff failed:\n<pre>generate unsupported</pre>\nSend another message to retry, or /cancel." || !strings.Contains(buttons(last.markup), "Continue as: Orchestrator") {
		t.Fatalf("failure edit %+v", last)
	}

	// An orchestrator handoff whose profile cannot be recorded fails closed.
	r.gen.err = nil
	r.gen.text = "Goal: ship\n\n- step"
	r.threads.err = errors.New("profile does not load")
	r.msg(t, 12, "again", th)
	e = r.tg.of("edit")
	if last := e[len(e)-1]; !strings.Contains(last.text, "record the profile on the handoff thread: profile does not load") {
		t.Fatalf("fail-closed edit %q", last.text)
	}
	if len(r.d.sent) != 0 {
		t.Fatal("no seed runs where an orchestrator was asked for")
	}

	r.threads.err = nil
	r.msg(t, 13, "third", th)
	if r.topics.names[len(r.topics.names)-1] != "🎛️ Handoff 2026-11-13 09:30" {
		t.Fatalf("orchestrator topic name %v", r.topics.names)
	}
	last := r.threads.setups[len(r.threads.setups)-1]
	if last.Profile != OrchestratorProfile || last.Model == nil || *last.Model != pick {
		t.Fatalf("recorded %+v", last)
	}
	if got := r.d.sent[0].Text; got != r.gen.text {
		t.Fatalf("seed %q", got)
	}
	sends := r.tg.of("send")
	if rec := sends[len(sends)-2].text; !strings.HasPrefix(rec, "🔀 <b>Handoff context</b>\n<blockquote>") || !strings.HasSuffix(rec, "Back to the source topic</a>") {
		t.Fatalf("record %q", rec)
	}
	if !strings.HasPrefix(sends[len(sends)-1].text, "🔀 <b>Handoff created</b> → ") {
		t.Fatalf("link %q", sends[len(sends)-1].text)
	}
	if r.h.HasLiveSession(threadOf7) {
		t.Fatal("done")
	}
	if strings.Join(r.gen.got, "|") != "handoff:"+threadOf7+":continue the release|handoff:"+threadOf7+":again|handoff:"+threadOf7+":third" {
		t.Fatalf("generations %v", r.gen.got)
	}
}

func TestHandoffPinsTheStartModelWithoutAPick(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	r.seeds.start = StartModel{Pin: ptr("anthropic:big@low")}
	r.gen.text = "ctx"
	r.msg(t, 10, "/handoff", th)
	r.msg(t, 11, "go", th)
	got := r.threads.setups[0]
	if got.Profile != "" || got.Model == nil || *got.Model != "anthropic:big@low" || r.topics.names[0] != "Handoff 2026-11-13 09:30" {
		t.Fatalf("recorded %+v %v", got, r.topics.names)
	}

	r.seeds.startErr = errors.New("this chat's workspace config does not load")
	r.msg(t, 20, "/handoff", th)
	r.msg(t, 21, "go", th)
	if len(r.topics.names) != 1 {
		t.Fatal("nothing is created when the start model does not resolve")
	}
}

func TestPromptBuilderAcceptRunsHere(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	r.gen.text = "**Fix** the build"
	r.msg(t, 10, "/prompt-builder", th)
	if got := buttons(r.tg.lastSend(t).markup); got != "🗑 Discard=stg:d" {
		t.Fatalf("prompt builder card %s", got)
	}
	empty := topicMsg(11, "")
	r.h.HandleMessage(context.Background(), empty, th)
	if got := r.tg.lastSend(t).text; got != "Send text or a voice note describing the prompt you want, or /cancel to abort." {
		t.Fatalf("hint %q", got)
	}
	voice := topicMsg(12, "")
	voice.Transcript = "fix the build"
	r.h.HandleMessage(context.Background(), voice, th)
	pending := r.tg.lastSend(t)
	if pending.text != "⏳ Building prompt…" {
		t.Fatalf("pending %q", pending.text)
	}
	e := r.tg.of("edit")
	if e[0].id != pending.id || !strings.Contains(e[0].text, "🛠 <b>Prompt preview</b>\n\n<b>Fix</b> the build") || buttons(e[0].markup) != "✅ Accept=stg:a,🗑 Discard=stg:d" {
		t.Fatalf("preview %+v", e[0])
	}
	if r.gen.got[0] != "prompt:"+threadOf7+":fix the build" {
		t.Fatalf("generated %v", r.gen.got)
	}

	r.h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), pending.id, "stg:a"))
	e = r.tg.of("edit")
	if e[len(e)-1].text != "▶️ Prompt accepted — running…" {
		t.Fatalf("accept edit %q", e[len(e)-1].text)
	}
	for _, id := range r.tg.deleted() {
		if id == pending.id {
			t.Fatal("the preview stays as the reply anchor")
		}
	}
	if len(r.d.sent) != 1 || r.d.sent[0].Text != r.gen.text || r.d.sent[0].MessageID != pending.id || *r.d.sent[0].Topic != topicID {
		t.Fatalf("dispatched %+v", r.d.sent)
	}

	// A card from before a restart clears itself.
	r.h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), pending.id, "stg:a"))
	r.h.HandleCallback(context.Background(), callback(forumChat, ptr(topicID), 77, "stg:d"))
	a := r.tg.answers()
	if a[len(a)-2] != "Session expired after restart — re-run the command" || a[len(a)-1] != "Discarded ✓" {
		t.Fatalf("stale answers %v", a)
	}
	if d := r.tg.deleted(); d[len(d)-1] != 77 {
		t.Fatalf("stale discard deleted %v", d)
	}
}

func TestStagedCommandsNeedTheirDependencies(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	r.h.Generator = nil
	for _, text := range []string{"/handoff", "/prompt_builder"} {
		if r.msg(t, 10, text, th) {
			t.Errorf("%s needs generate", text)
		}
	}
	if !r.msg(t, 11, "/btw", th) {
		t.Fatal("/btw makes no generation")
	}
	r.h.Topics = nil
	if r.msg(t, 12, "/btw", th) {
		t.Fatal("/btw needs topic creation")
	}
}
