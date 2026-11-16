package commands

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"
)

type fakeModels struct {
	mu       sync.Mutex
	pickers  map[Scope]Picker
	queries  []PickerQuery
	topicSet []string
	setErr   error
	staged   []string
	stageErr error
}

func (f *fakeModels) Picker(_ context.Context, q PickerQuery) (Picker, error) {
	f.mu.Lock()
	defer f.mu.Unlock()
	f.queries = append(f.queries, q)
	p, ok := f.pickers[q.Scope]
	if !ok {
		return Picker{}, errors.New("no picker for " + string(q.Scope))
	}
	return p, nil
}

func (f *fakeModels) SetTopicModel(_ context.Context, _ int64, thread, model string) (string, error) {
	f.topicSet = append(f.topicSet, thread+"="+model)
	if f.setErr != nil {
		return "", f.setErr
	}
	return "label of " + model, nil
}

func (f *fakeModels) ResolveStagingPick(_ context.Context, _ int64, source, profile, model string) (string, error) {
	f.staged = append(f.staged, source+"|"+profile+"|"+model)
	if f.stageErr != nil {
		return "", f.stageErr
	}
	return "resolved:" + model, nil
}

type fakeDefaults struct{ set []string }

func (f *fakeDefaults) SetChatModel(_ context.Context, _ int64, model string) error {
	f.set = append(f.set, model)
	return nil
}

func specs(id string) map[string]string {
	out := map[string]string{}
	for _, l := range ThinkingLevels {
		out[l] = id + "@" + l
	}
	return out
}

func samplePicker(label string) Picker {
	return Picker{
		Models: []PickerModel{
			{ID: "anthropic:claude-big", Name: "anthropic:claude-big", Specs: specs("anthropic:claude-big")},
			{ID: "openai:gpt-6", Name: "openai:gpt-6", Specs: specs("openai:gpt-6")},
			{ID: "anthropic:claude-small", Name: "anthropic:claude-small", Specs: specs("anthropic:claude-small")},
			{ID: "openai@work:gpt-6", Name: "openai@work:gpt-6", Specs: specs("openai@work:gpt-6")},
		},
		ProviderLabels: map[string]string{"anthropic": "Anthropic", "openai": "OpenAI", "openai@work": "OpenAI (work)"},
		Modes:          []PickerMode{{Name: "smart", Ref: "mode:smart"}},
		Label:          label,
		DefaultLabel:   "anthropic:claude-big",
		Model:          "anthropic:claude-big@high",
		Thinking:       "high",
	}
}

func modelRig() (*fakeTG, *fakeModels, *Handler) {
	tg := newTG()
	topic := samplePicker("mode:smart → openai:gpt-6@high")
	topic.Overridden, topic.Override = true, "mode:smart"
	models := &fakeModels{pickers: map[Scope]Picker{
		ScopeTopic:   topic,
		ScopeGeneral: samplePicker("anthropic:claude-big"),
	}}
	h := &Handler{Telegram: tg, Models: models, Resolver: fakeResolver{thread: ordinary()}}
	return tg, models, h
}

func tap(t *testing.T, h *Handler, tg *fakeTG, topic *int64, message int64, data string) string {
	t.Helper()
	before := len(tg.answers())
	if !h.HandleCallback(context.Background(), callback(forumChat, topic, message, data)) {
		t.Fatalf("%q not claimed", data)
	}
	a := tg.answers()
	if len(a) != before+1 {
		t.Fatalf("%q answered %d times", data, len(a)-before)
	}
	return a[len(a)-1]
}

func lastEdit(t *testing.T, tg *fakeTG) call {
	t.Helper()
	e := tg.of("edit")
	if len(e) == 0 {
		t.Fatal("nothing edited")
	}
	return e[len(e)-1]
}

func TestTopicModelPicker(t *testing.T) {
	tg, models, h := modelRig()
	if !h.HandleMessage(context.Background(), topicMsg(10, "/model"), ordinary()) {
		t.Fatal("/model not handled")
	}
	open := tg.lastSend(t)
	if open.text != "Current model: <code>mode:smart → openai:gpt-6@high</code> (topic override)\nDefault: <code>anthropic:claude-big</code>" {
		t.Fatalf("header %q", open.text)
	}
	if got := buttons(open.markup); got != "⚡ smart=model_mode:smart:topic | Anthropic=model_provider:anthropic:topic,OpenAI=model_provider:openai:topic,OpenAI (work)=model_provider:openai@work:topic | ✖ Cancel=model_cancel:topic" {
		t.Fatalf("providers %s", got)
	}
	if q := models.queries[0]; q.Scope != ScopeTopic || q.ThreadID != threadOf7 {
		t.Fatalf("query %+v", q)
	}

	tap(t, h, tg, ptr(topicID), open.id, "model_provider:anthropic:topic")
	e := lastEdit(t, tg)
	if e.text != "Select a <b>Anthropic</b> model:" || buttons(e.markup) != "claude-big=model_pick:anthropic:0:topic,claude-small=model_pick:anthropic:1:topic | ← Back=model_back:topic | ✖ Cancel=model_cancel:topic" {
		t.Fatalf("models step %q %s", e.text, buttons(e.markup))
	}

	tap(t, h, tg, ptr(topicID), open.id, "model_pick:openai@work:0:topic")
	e = lastEdit(t, tg)
	if e.text != "Select thinking for <code>openai@work:gpt-6</code>:" || !strings.Contains(buttons(e.markup), "✅ high=model_thinking:openai@work:0:topic:high") || !strings.HasSuffix(buttons(e.markup), "← Back=model_provider:openai@work:topic") {
		t.Fatalf("thinking step %q %s", e.text, buttons(e.markup))
	}

	if a := tap(t, h, tg, ptr(topicID), open.id, "model_pick:openai:7:topic"); a != "Model no longer available" {
		t.Fatalf("stale index %q", a)
	}
	if a := tap(t, h, tg, ptr(topicID), open.id, "model_pick:openai:x:topic"); a != "Invalid model selection" {
		t.Fatalf("bad index %q", a)
	}

	tap(t, h, tg, ptr(topicID), open.id, "model_thinking:openai@work:0:topic:low")
	if models.topicSet[0] != threadOf7+"=openai@work:gpt-6@low" {
		t.Fatalf("set %v", models.topicSet)
	}
	if e := lastEdit(t, tg); e.text != "✅ Model set to <code>label of openai@work:gpt-6@low</code> for this topic." || e.markup != nil {
		t.Fatalf("set reply %+v", e)
	}

	tap(t, h, tg, ptr(topicID), open.id, "model_mode:SMART:topic")
	if models.topicSet[1] != threadOf7+"=mode:smart" {
		t.Fatalf("mode set %v", models.topicSet)
	}
	if a := tap(t, h, tg, ptr(topicID), open.id, "model_mode:gone:topic"); a != "Mode no longer configured" {
		t.Fatalf("stale mode %q", a)
	}

	models.setErr = errors.New("unknown model <x>")
	tap(t, h, tg, ptr(topicID), open.id, "model_thinking:anthropic:1:topic:max")
	if e := lastEdit(t, tg); e.text != "❌ unknown model &lt;x&gt;" {
		t.Fatalf("refused set %q", e.text)
	}

	tap(t, h, tg, ptr(topicID), open.id, "model_back:topic")
	if e := lastEdit(t, tg); e.text != "Current model: <code>mode:smart → openai:gpt-6@high</code>\nCurrent override: <code>mode:smart</code>" {
		t.Fatalf("back header %q", e.text)
	}
	tap(t, h, tg, ptr(topicID), open.id, "model_cancel:topic")
	if e := lastEdit(t, tg); e.text != "Model change cancelled. Current model: <code>anthropic:claude-big@high</code>" {
		t.Fatalf("cancel %q", e.text)
	}
	if a := tap(t, h, tg, ptr(topicID), open.id, "model_back:bogus"); a != "" {
		t.Fatalf("bogus scope %q", a)
	}
}

func TestGeneralModelPickerNeedsTheChatDefaultOp(t *testing.T) {
	tg, models, h := modelRig()
	general := topicMsg(10, "/model list")
	general.Topic = nil
	h.HandleMessage(context.Background(), general, Thread{ID: "telegram--1001234567890", Kind: KindOrdinary})
	if got := tg.lastSend(t).text; got != chatDefaultUnavailable {
		t.Fatalf("general without the op %q", got)
	}
	if a := tap(t, h, tg, nil, 5, "model_thinking:openai:0:general:low"); a != "Not available in this gateway build yet" {
		t.Fatalf("general pick %q", a)
	}

	defaults := &fakeDefaults{}
	h.Defaults = defaults
	h.HandleMessage(context.Background(), general, Thread{ID: "telegram--1001234567890", Kind: KindOrdinary})
	open := tg.lastSend(t)
	if open.text != "Current model: <code>anthropic:claude-big</code>" || !strings.HasSuffix(buttons(open.markup), "✖ Cancel=model_cancel:general") {
		t.Fatalf("general picker %q %s", open.text, buttons(open.markup))
	}
	if models.queries[0].ThreadID != "" {
		t.Fatalf("a general picker names no thread: %+v", models.queries[0])
	}
	tap(t, h, tg, nil, open.id, "model_mode:smart:general")
	if defaults.set[0] != "mode:smart" || lastEdit(t, tg).text != "✅ Default model set to <code>mode:smart</code>." {
		t.Fatalf("general set %v %q", defaults.set, lastEdit(t, tg).text)
	}

	cfg := samplePicker("x")
	cfg.ConfigError = "This chat's workspace config does not load"
	models.pickers[ScopeGeneral] = cfg
	if a := tap(t, h, tg, nil, open.id, "model_mode:smart:general"); a != cfg.ConfigError || len(defaults.set) != 1 {
		t.Fatalf("broken config %q %v", a, defaults.set)
	}
}

func TestStagingModelPick(t *testing.T) {
	th := ordinary()
	r := newStaging(th)
	models := &fakeModels{pickers: map[Scope]Picker{ScopeStaging: samplePicker("openai:gpt-6@medium")}}
	r.h.Models = models
	r.msg(t, 10, "/btw", th)
	card := r.tg.lastSend(t)

	tap(t, r.h, r.tg, ptr(topicID), card.id, "stg:m")
	e := lastEdit(t, r.tg)
	if e.id != card.id || e.text != "Pick the model for the new topic (this thread keeps its own). Starts on: <code>openai:gpt-6@medium</code>" || !strings.HasSuffix(buttons(e.markup), "← Back=model_cancel:staging") {
		t.Fatalf("staging picker %q %s", e.text, buttons(e.markup))
	}
	if q := models.queries[0]; q.Scope != ScopeStaging || q.ThreadID != threadOf7 || q.Profile != "" {
		t.Fatalf("query %+v", q)
	}

	if a := tap(t, r.h, r.tg, ptr(topicID), card.id, "model_thinking:openai:0:staging:xhigh"); a != "New topic: resolved:openai:gpt-6@xhigh" {
		t.Fatalf("pick answer %q", a)
	}
	if e := lastEdit(t, r.tg); !strings.HasSuffix(e.text, "New topic model: <code>resolved:openai:gpt-6@xhigh</code>") || buttons(e.markup) != "🎛 Model=stg:m,🗑 Discard=stg:d" {
		t.Fatalf("card restored %q %s", e.text, buttons(e.markup))
	}

	tap(t, r.h, r.tg, ptr(topicID), card.id, "model_back:staging")
	if e := lastEdit(t, r.tg); !strings.HasSuffix(e.text, "\nPicked: <code>resolved:openai:gpt-6@xhigh</code>") {
		t.Fatalf("back shows the pick %q", e.text)
	}
	tap(t, r.h, r.tg, ptr(topicID), card.id, "model_cancel:staging")
	if e := lastEdit(t, r.tg); !strings.HasPrefix(e.text, "💬 <b>Side question</b>") {
		t.Fatalf("cancel restores the card %q", e.text)
	}

	r.msg(t, 11, "what changed?", th)
	if got := r.threads.setups[0].Model; got == nil || *got != "resolved:openai:gpt-6@xhigh" {
		t.Fatalf("the pick lands on the new thread only: %+v", r.threads.setups)
	}

	if a := tap(t, r.h, r.tg, ptr(topicID), card.id, "model_mode:smart:staging"); a != "Session expired — re-run the command" {
		t.Fatalf("expired %q", a)
	}
}

func TestHandoffStagingPickerFollowsTheRole(t *testing.T) {
	th := Thread{ID: threadOf7, Kind: KindOrdinary}
	r := newStaging(th)
	models := &fakeModels{pickers: map[Scope]Picker{ScopeStaging: samplePicker("x")}, stageErr: errors.New("unknown mode")}
	r.h.Models = models
	r.msg(t, 10, "/handoff", th)
	card := r.tg.lastSend(t)
	tap(t, r.h, r.tg, ptr(topicID), card.id, "stg:r")
	tap(t, r.h, r.tg, ptr(topicID), card.id, "stg:m")
	if q := models.queries[0]; q.Profile != OrchestratorProfile {
		t.Fatalf("picker follows Continue as: %+v", q)
	}
	if a := tap(t, r.h, r.tg, ptr(topicID), card.id, "model_mode:smart:staging"); a != "unknown mode" {
		t.Fatalf("refused pick %q", a)
	}
	if models.staged[0] != threadOf7+"|orchestrator|mode:smart" {
		t.Fatalf("resolved against %v", models.staged)
	}
	if c, _ := r.h.Card(threadOf7); c.Model != nil {
		t.Fatal("a refused pick is not kept")
	}
}

// A model without reasoning comes with no levels: picking it sets the model
// itself, with no thinking step.
func TestAModelWithoutLevelsSkipsTheThinkingStep(t *testing.T) {
	tg, models, h := modelRig()
	topic := models.pickers[ScopeTopic]
	topic.Models = append(topic.Models, PickerModel{ID: "anthropic:claude-fixed", Name: "anthropic:claude-fixed", Specs: map[string]string{}})
	models.pickers[ScopeTopic] = topic
	h.HandleMessage(context.Background(), topicMsg(10, "/model"), ordinary())
	open := tg.lastSend(t)
	edits := len(tg.of("edit"))
	tap(t, h, tg, ptr(topicID), open.id, "model_pick:anthropic:2:topic")
	if len(models.topicSet) != 1 || models.topicSet[0] != threadOf7+"=anthropic:claude-fixed" {
		t.Fatalf("set %v", models.topicSet)
	}
	for _, e := range tg.of("edit")[edits:] {
		if strings.Contains(e.text, "Select thinking") {
			t.Fatalf("thinking step shown: %q", e.text)
		}
	}
}
