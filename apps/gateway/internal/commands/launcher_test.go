package commands

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"
)

type fakeLaunches struct {
	views     map[string]LauncherView
	resumable []ResumeThread
	sources   map[string]string
}

func (f *fakeLaunches) LauncherView(_ context.Context, _ int64, role string) (LauncherView, error) {
	return f.views[role], nil
}

func (f *fakeLaunches) Resumable(context.Context, int64) ([]ResumeThread, error) {
	return f.resumable, nil
}

func (f *fakeLaunches) ResumeSource(_ context.Context, source string) (string, error) {
	title, ok := f.sources[source]
	if !ok {
		return "", errors.New("source thread does not exist: " + source)
	}
	return title, nil
}

type fakeBlocker struct{ blocked []string }

func (b *fakeBlocker) BlockTopic(_ context.Context, id string) error {
	b.blocked = append(b.blocked, id)
	return nil
}

type launcherRig struct {
	tg       *fakeTG
	topics   *fakeTopics
	threads  *fakeThreads
	launches *fakeLaunches
	blocker  *fakeBlocker
	h        *Handler
}

func newLauncher(orchestratorChat bool) *launcherRig {
	now, _ := fixed(time.Date(2026, 11, 13, 9, 30, 0, 0, time.UTC))
	r := &launcherRig{
		tg: newTG(), topics: &fakeTopics{next: 800}, threads: &fakeThreads{}, blocker: &fakeBlocker{},
		launches: &fakeLaunches{views: map[string]LauncherView{
			RoleAgent: {
				Modes: []LaunchMode{
					{Name: "fast", Ref: "mode:fast", Primary: "openai:gpt-6-mini", Thinking: "off", Description: " quick <edits> "},
					{Name: "smart", Ref: "mode:smart", Primary: "anthropic:claude-big@high", Thinking: "high"},
				},
				DefaultMode: "mode:smart", DefaultModel: "anthropic:claude-big", DefaultThinking: "high",
			},
			RoleOrchestrator: {DefaultModel: "openai:gpt-6", DefaultThinking: "off", LaunchError: "orchestrator override names undefined mode `mode:gone`"},
		}},
	}
	r.h = &Handler{
		Telegram: r.tg, Topics: r.topics, Threads: r.threads, Launches: r.launches, Blocker: r.blocker,
		Chats: fakeChats{orchestrator: orchestratorChat}, Resolver: fakeResolver{thread: ordinary()}, Now: now, RepostDelay: time.Millisecond,
	}
	return r
}

func generalMsg(id int64, text string) Message {
	m := topicMsg(id, text)
	m.Topic = nil
	return m
}

var generalThread = Thread{ID: "telegram--1001234567890", Kind: KindOrdinary}

func TestLauncherViewText(t *testing.T) {
	r := newLauncher(false)
	if !r.h.HandleMessage(context.Background(), generalMsg(10, "/menu"), generalThread) {
		t.Fatal("/launcher not handled")
	}
	sent := r.tg.lastSend(t)
	want := "🚀 <b>Thread launcher</b>\nNew topics start as: <b>🤖 Agent</b>\nDefault: <code>mode:smart</code> → <code>claude-big</code> · high\n\n" +
		"<b>fast</b> → <code>gpt-6-mini</code> — quick &lt;edits&gt;\n<b>smart</b> → <code>claude-big@high</code> · high\n\n" +
		"<i>Tap a mode, or use 🎛 Custom / 🔄 Continue. The first button switches agent / orchestrator.</i>"
	if sent.text != want || sent.topic != nil || *sent.reply != 10 {
		t.Fatalf("launcher:\n%s", sent.text)
	}
	if got := buttons(sent.markup); got != "New as: 🤖 Agent ⇄=nt:as:o | fast=nt:p:fast,smart=nt:p:smart | 🎛 Custom=nt:custom,🔄 Continue=nt:resume | ✖ Dismiss=nt:x" {
		t.Fatalf("keyboard %s", got)
	}

	tap(t, r.h, r.tg, nil, sent.id, "nt:as:o")
	e := lastEdit(t, r.tg)
	want = "🚀 <b>Thread launcher</b>\nNew topics start as: <b>🧭 Orchestrator</b>\nDefault: <code>gpt-6</code>\nNo model modes configured yet — use 🎛 Custom, or add <code>[[model_modes]]</code> to your config.\n\n⚠️ orchestrator override names undefined mode `mode:gone`; new topics of this role are refused until it is fixed."
	if e.text != want || buttons(e.markup) != "New as: 🧭 Orchestrator ⇄=nt:as:a | 🎛 Custom=nt:ocustom,🔄 Continue=nt:resume | ✖ Dismiss=nt:x" {
		t.Fatalf("orchestrator view:\n%s\n%s", e.text, buttons(e.markup))
	}

	o := newLauncher(true)
	o.h.HandleMessage(context.Background(), generalMsg(10, "/launcher"), generalThread)
	if !strings.Contains(o.tg.lastSend(t).text, "<b>🧭 Orchestrator</b>") {
		t.Fatal("an orchestrator chat starts on the orchestrator role")
	}
}

func TestLauncherModeTopics(t *testing.T) {
	r := newLauncher(false)
	r.h.HandleMessage(context.Background(), generalMsg(10, "/launcher"), generalThread)
	launcher := r.tg.lastSend(t)

	if a := tap(t, r.h, r.tg, nil, launcher.id, "nt:p:fast"); a != "New thread ready ✓" {
		t.Fatalf("answer %q", a)
	}
	if r.topics.names[0] != "Chat 2026-11-13 09:30" {
		t.Fatalf("topic %v", r.topics.names)
	}
	s := r.threads.setups[0]
	if s.ThreadID != "telegram--1001234567890-topic-801" || s.Profile != "" || *s.Model != "mode:fast" || !s.PendingTitle {
		t.Fatalf("setup %+v", s)
	}
	if a := tap(t, r.h, r.tg, nil, launcher.id, "nt:p:gone"); a != "Mode no longer configured" {
		t.Fatalf("stale mode %q", a)
	}

	r.launches.views[RoleOrchestrator] = LauncherView{Modes: []LaunchMode{{Name: "fast", Ref: "mode:fast"}}}
	if a := tap(t, r.h, r.tg, nil, launcher.id, "nt:op:fast"); a != "New orchestrator ready ✓" {
		t.Fatalf("orchestrator answer %q", a)
	}
	s = r.threads.setups[1]
	if r.topics.names[1] != "🎛️ Chat 2026-11-13 09:30" || s.Profile != OrchestratorProfile || *s.Model != "mode:fast" {
		t.Fatalf("orchestrator setup %+v %v", s, r.topics.names)
	}

	r.launches.views[RoleAgent] = LauncherView{ConfigError: "config does not load"}
	if a := tap(t, r.h, r.tg, nil, launcher.id, "nt:p:fast"); a != "config does not load" || len(r.topics.names) != 2 {
		t.Fatalf("broken config %q", a)
	}
}

func TestOrchestratorLaunchFailsClosed(t *testing.T) {
	r := newLauncher(false)
	r.launches.views[RoleOrchestrator] = LauncherView{Modes: []LaunchMode{{Name: "fast", Ref: "mode:fast"}}}
	r.threads.err = errors.New("profile does not load")
	if a := tap(t, r.h, r.tg, nil, 5, "nt:op:fast"); a != "Couldn't create the orchestrator" {
		t.Fatalf("answer %q", a)
	}
	if r.blocker.blocked[0] != "telegram--1001234567890-topic-801" || r.topics.deleted[0] != 801 || len(r.tg.of("send")) != 0 {
		t.Fatalf("blocked %v deleted %v", r.blocker.blocked, r.topics.deleted)
	}

	r.topics.deleteErr = errors.New("not enough rights")
	tap(t, r.h, r.tg, nil, 5, "nt:op:fast")
	if got := r.tg.lastSend(t); got.text != UnsetupTopicNotice || *got.topic != 802 {
		t.Fatalf("notice %+v", got)
	}

	// An agent topic opens on defaults when its metadata write fails.
	if a := tap(t, r.h, r.tg, nil, 5, "nt:p:fast"); a != "New thread ready ✓" || len(r.topics.deleted) != 2 {
		t.Fatalf("agent %q", a)
	}

	r.h.Blocker = nil
	before := len(r.topics.names)
	if a := tap(t, r.h, r.tg, nil, 5, "nt:op:fast"); a != "Couldn't create the orchestrator" || len(r.topics.names) != before {
		t.Fatal("no orchestrator topic opens while nothing can block a failed one")
	}
}

func TestLauncherCustomPickCreatesAndReturns(t *testing.T) {
	r := newLauncher(false)
	models := &fakeModels{pickers: map[Scope]Picker{ScopeNewThread: samplePicker("anthropic:claude-big"), ScopeNewOrchestrator: samplePicker("x")}}
	r.h.Models = models
	tap(t, r.h, r.tg, nil, 5, "nt:custom")
	e := lastEdit(t, r.tg)
	if e.text != "Pick a model for a new thread. Default: <code>anthropic:claude-big</code>" || strings.Contains(buttons(e.markup), "⚡") || !strings.HasSuffix(buttons(e.markup), "← Back=model_cancel:newthread") {
		t.Fatalf("custom picker %q %s", e.text, buttons(e.markup))
	}
	if a := tap(t, r.h, r.tg, nil, 5, "model_thinking:openai:0:newthread:medium"); a != "New thread ready ✓" {
		t.Fatalf("answer %q", a)
	}
	if *r.threads.setups[0].Model != "openai:gpt-6@medium" {
		t.Fatalf("setup %+v", r.threads.setups[0])
	}
	if e := lastEdit(t, r.tg); !strings.HasPrefix(e.text, "🚀 <b>Thread launcher</b>\nNew topics start as: <b>🤖 Agent</b>") {
		t.Fatalf("launcher restored %q", e.text)
	}

	bad := samplePicker("x")
	bad.LaunchError = "orchestrator override names undefined mode"
	models.pickers[ScopeNewOrchestrator] = bad
	if a := tap(t, r.h, r.tg, nil, 5, "model_thinking:openai:0:neworch:low"); a != bad.LaunchError || len(r.topics.names) != 1 {
		t.Fatalf("launch error %q", a)
	}
	tap(t, r.h, r.tg, nil, 5, "model_cancel:neworch")
	if e := lastEdit(t, r.tg); !strings.Contains(e.text, "<b>🧭 Orchestrator</b>") {
		t.Fatalf("cancel returns to the launcher %q", e.text)
	}
}

func TestLauncherResume(t *testing.T) {
	r := newLauncher(false)
	if a := tap(t, r.h, r.tg, nil, 5, "nt:resume"); a != "No recent threads for this project yet" || len(r.tg.of("edit")) != 0 {
		t.Fatalf("empty %q", a)
	}
	ago := func(d time.Duration) *time.Time {
		v := time.Date(2026, 11, 13, 9, 30, 0, 0, time.UTC).Add(-d)
		return &v
	}
	r.launches.resumable = []ResumeThread{
		{ID: "a1", Title: strings.Repeat("long title ", 6), Modified: ago(30 * time.Second)},
		{ID: "b2", Title: "Fix", Modified: ago(90 * time.Minute)},
		{ID: "c3", Title: "Old", Modified: ago(50 * time.Hour)},
		{ID: "d4", Title: "Unknown"},
	}
	tap(t, r.h, r.tg, nil, 5, "nt:resume")
	e := lastEdit(t, r.tg)
	want := "long title long title long title long ti · now=nt:r:a1 | Fix · 1h=nt:r:b2 | Old · 2d=nt:r:c3 | Unknown · ?=nt:r:d4 | ← Back=nt:back"
	if e.text != "Resume a thread:" || buttons(e.markup) != want {
		t.Fatalf("resume %q\n%s", e.text, buttons(e.markup))
	}

	r.launches.sources = map[string]string{"b2": "Fix the build", "c3": ""}
	if a := tap(t, r.h, r.tg, nil, 5, "nt:r:b2"); a != "Resumed ✓" {
		t.Fatalf("resume %q", a)
	}
	if r.topics.names[0] != "Fix the build" || r.threads.setups[0] != (ThreadSetup{ThreadID: "telegram--1001234567890-topic-801", AliasTo: "b2"}) {
		t.Fatalf("resumed %v %+v", r.topics.names, r.threads.setups)
	}
	if !strings.HasPrefix(lastEdit(t, r.tg).text, "🚀") {
		t.Fatal("the launcher comes back")
	}
	tap(t, r.h, r.tg, nil, 5, "nt:r:c3-long-id")
	if a := r.tg.answers(); a[len(a)-1] != "That thread no longer exists" {
		t.Fatalf("gone %v", a)
	}
	tap(t, r.h, r.tg, nil, 5, "nt:r:c3")
	if r.topics.names[1] != "Resumed c3" {
		t.Fatalf("untitled %v", r.topics.names)
	}
}

func TestLauncherRepostAndDismiss(t *testing.T) {
	r := newLauncher(false)
	r.h.HandleMessage(context.Background(), generalMsg(10, "/launcher"), generalThread)
	first := r.tg.lastSend(t)
	r.h.ScheduleLauncherRepost(context.Background(), forumChat)
	r.h.ScheduleLauncherRepost(context.Background(), forumChat)
	r.h.Wait()
	if d := r.tg.deleted(); len(d) != 1 || d[0] != first.id {
		t.Fatalf("a burst reposts once: deleted %v", d)
	}
	sends := r.tg.of("send")
	if len(sends) != 2 || sends[1].reply != nil {
		t.Fatalf("sends %+v", sends)
	}
	second := sends[1]

	// Dismissing an old launcher keeps tracking the current one.
	tap(t, r.h, r.tg, nil, first.id, "nt:x")
	r.h.ScheduleLauncherRepost(context.Background(), forumChat)
	r.h.Wait()
	if len(r.tg.of("send")) != 3 {
		t.Fatal("the current launcher still reposts")
	}
	current := r.tg.lastSend(t)
	r.tg.failOn = map[string]error{"delete": errors.New("message can't be deleted")}
	tap(t, r.h, r.tg, nil, current.id, "nt:x")
	if m := r.tg.of("markup"); len(m) != 1 || m[0].id != current.id || len(m[0].markup.InlineKeyboard) != 0 {
		t.Fatalf("refused delete drops the buttons %+v", m)
	}
	r.h.ScheduleLauncherRepost(context.Background(), forumChat)
	r.h.Wait()
	if len(r.tg.of("send")) != 3 {
		t.Fatal("a dismissed launcher never comes back")
	}
	_ = second
}

func TestNewInGeneralAndWorktree(t *testing.T) {
	r := newLauncher(false)
	if !r.h.HandleMessage(context.Background(), generalMsg(10, "/new"), generalThread) {
		t.Fatal("/new in General")
	}
	if r.topics.names[0] != "Chat 2026-11-13 09:30" || r.threads.setups[0] != (ThreadSetup{ThreadID: "telegram--1001234567890-topic-801", PendingTitle: true}) {
		t.Fatalf("empty topic %v %+v", r.topics.names, r.threads.setups)
	}
	if r.h.HandleMessage(context.Background(), topicMsg(11, "/new"), ordinary()) {
		t.Fatal("/new in a topic is the bot's")
	}
	r.topics.err = errors.New("no rights")
	r.h.HandleMessage(context.Background(), generalMsg(12, "/new"), generalThread)
	if got := r.tg.lastSend(t); got.text != "⚠️ I couldn't create a new topic. Please try again." || got.topic != nil {
		t.Fatalf("refused %+v", got)
	}

	wt := &fakeWorktrees{path: "/srv/wt/<t>"}
	r.h.Worktrees = wt
	r.h.Chats = fakeChats{binding: Binding{Root: "/srv/repo"}}
	r.h.HandleMessage(context.Background(), topicMsg(13, "/wt"), ordinary())
	if wt.got != "/srv/repo|"+threadOf7 || r.tg.lastSend(t).text != "Worktree enabled: /srv/wt/&lt;t&gt;" {
		t.Fatalf("worktree %q %q", wt.got, r.tg.lastSend(t).text)
	}
	if last := r.threads.setups[len(r.threads.setups)-1]; last != (ThreadSetup{ThreadID: threadOf7, Root: "/srv/wt/<t>"}) {
		t.Fatalf("root %+v", last)
	}
	wt.err = errors.New("not a git repository")
	r.h.HandleMessage(context.Background(), topicMsg(14, "/worktree create"), ordinary())
	if got := r.tg.lastSend(t).text; got != "Failed to enable worktree: not a git repository\n\nTip: start the bot from inside a git repo (or a subdirectory of one)." {
		t.Fatalf("refused %q", got)
	}
}

type fakeWorktrees struct {
	path, got string
	err       error
}

func (f *fakeWorktrees) EnsureWorktree(_ context.Context, root, thread string) (string, error) {
	f.got = root + "|" + thread
	return f.path, f.err
}
