package ui

import (
	"context"
	"errors"
	"strings"
	"sync"
	"testing"

	tea "charm.land/bubbletea/v2"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/wire"
)

// parentBackend serves agents and thread_view for idle-parent tests and
// records every thread_view call as "id@cursor" ("id@" for a full load).
type parentBackend struct {
	mu      sync.Mutex
	rows    []data.Agent
	threads map[string]data.Thread
	errs    map[string]error
	calls   []string
}

func (p *parentBackend) fake() *fake {
	return &fake{
		agents: func(context.Context) ([]data.Agent, error) {
			p.mu.Lock()
			defer p.mu.Unlock()
			return append([]data.Agent(nil), p.rows...), nil
		},
		thread: func(_ context.Context, id string, after *uint64) (data.Thread, error) {
			p.mu.Lock()
			defer p.mu.Unlock()
			c := ""
			if after != nil {
				c = itoa(int(*after))
			}
			p.calls = append(p.calls, id+"@"+c)
			if err := p.errs[id]; err != nil {
				return data.Thread{}, err
			}
			th, ok := p.threads[id]
			if !ok {
				return data.Thread{}, &wire.ServiceError{Message: "Thread not found", Code: "not_found"}
			}
			if after != nil {
				th.Partial = true
				var keep []data.Activity
				for _, a := range th.Activity {
					if a.Sequence >= *after {
						keep = append(keep, a)
					}
				}
				th.Activity = keep
			}
			return th, nil
		},
	}
}

func (p *parentBackend) set(f func()) {
	p.mu.Lock()
	defer p.mu.Unlock()
	f()
}

func (p *parentBackend) callLog() string {
	p.mu.Lock()
	defer p.mu.Unlock()
	return strings.Join(p.calls, " ")
}

func usage(seq uint64, provider, model string) data.Activity {
	return data.Activity{Type: "usage", Sequence: seq, Time: "09:0" + itoa(int(seq)), Model: ptr(model), Provider: ptr(provider)}
}

func parentThread(id, title string, acts ...data.Activity) data.Thread {
	cursor := uint64(0)
	for _, a := range acts {
		cursor = max(cursor, a.Sequence+1)
	}
	return data.Thread{ID: id, SourceID: id, Title: title, Cursor: cursor, Activity: acts}
}

func idleRow(m *Model, thread string) string {
	for _, l := range strings.Split(plain(m), "\n") {
		if strings.Contains(l, "◌") {
			return l
		}
	}
	return ""
}

func TestIdleParentShowsTitleAndLastModel(t *testing.T) {
	pb := &parentBackend{
		rows: []data.Agent{run("w1", "orch-idle", 2, "worker"), run("w2", "orch-idle", 3, "worker")},
		threads: map[string]data.Thread{"orch-idle": parentThread("orch-idle", "Plan the release",
			data.Activity{Type: "message", Sequence: 0, Role: "user", Text: "go"},
			usage(1, "openai-codex", "gpt-6-astra"))},
	}
	m, c := newModel(t, pb.fake())
	pump(m, m.Init())
	row := idleRow(m, "orch-idle")
	if !strings.Contains(row, "Plan the release") || !strings.Contains(row, "idle") || !strings.Contains(row, "openai-codex:gpt-6-astra") {
		t.Fatalf("row %q", row)
	}
	if strings.Contains(row, "orch-idle") && !strings.Contains(plain(m), "THREAD") {
		t.Fatalf("the thread id must not stand in for the model: %q", row)
	}
	if !strings.Contains(plain(m), "Active agents  2 runs") {
		t.Fatal("the idle parent is not a run")
	}
	// One lookup for the parent, even with two children.
	if got := pb.callLog(); got != "orch-idle@" {
		t.Fatalf("calls %q", got)
	}
	// Later looks ask for the delta only, and a delta without a request
	// keeps the model.
	pb.set(func() {
		th := pb.threads["orch-idle"]
		th.Activity = append(th.Activity, data.Activity{Type: "message", Sequence: 2, Role: "assistant", Text: "ok"})
		th.Cursor = 3
		pb.threads["orch-idle"] = th
	})
	c.add(16e9)
	pump(m, m.refresh(c.now()))
	if got := pb.callLog(); got != "orch-idle@ orch-idle@2" {
		t.Fatalf("calls %q", got)
	}
	if row := idleRow(m, "orch-idle"); !strings.Contains(row, "openai-codex:gpt-6-astra") {
		t.Fatalf("a delta without a request must keep the model: %q", row)
	}
	// A new request with another model shows it.
	pb.set(func() {
		th := pb.threads["orch-idle"]
		th.Activity = append(th.Activity, usage(3, "claude-cli", "claude-opus-5-5"))
		th.Cursor = 4
		pb.threads["orch-idle"] = th
	})
	c.add(16e9)
	pump(m, m.refresh(c.now()))
	if row := idleRow(m, "orch-idle"); !strings.Contains(row, "claude-cli:claude-opus-5-5") {
		t.Fatalf("model change not shown: %q", row)
	}
	// The detail pane names it the last request's model and keeps the id.
	m.Update(key("k"))
	m.Update(key("home"))
	out := plain(m)
	for _, want := range []string{"Title      Plan the release", "Thread     orch-idle", "claude-cli:claude-opus-5-5 · last request at 09:03; thinking not recorded", "not counted as a run"} {
		if !strings.Contains(out, want) {
			t.Fatalf("detail lacks %q:\n%s", want, out)
		}
	}
}

func TestIdleParentUnknownStates(t *testing.T) {
	pb := &parentBackend{
		rows: []data.Agent{run("w1", "gone", 2, "worker"), run("w2", "quiet", 3, "worker"), run("w3", "broken", 4, "worker")},
		threads: map[string]data.Thread{
			"quiet": parentThread("quiet", "No requests yet", data.Activity{Type: "message", Sequence: 0, Role: "user", Text: "hi"}),
		},
		errs: map[string]error{"broken": errors.New("socket closed")},
	}
	m, _ := newModel(t, pb.fake())
	m.Update(tea.WindowSizeMsg{Width: 120, Height: 40})
	pump(m, m.Init())
	out := plain(m)
	var rows []string
	for _, l := range strings.Split(out, "\n") {
		if strings.Contains(l, "◌") {
			rows = append(rows, l)
		}
	}
	if len(rows) != 3 {
		t.Fatalf("rows %q", rows)
	}
	for i, want := range []string{"idle parent (not found)", "No requests yet", "idle parent"} {
		if !strings.Contains(rows[i], want) || !strings.Contains(rows[i], "—") && i < 2 {
			t.Fatalf("row %d %q lacks %q or a dash model", i, rows[i], want)
		}
		for _, id := range []string{"gone", "quiet", "broken"} {
			if strings.Contains(rows[i], "  "+id+"  ") {
				t.Fatalf("row %d shows the id as a value: %q", i, rows[i])
			}
		}
	}
	// A failed look says so in the detail.
	m.agents.list.selected = 4
	m.agents.list.remember(m.agents.ids())
	if out := plain(m); !strings.Contains(out, "lookup failed: socket closed") {
		t.Fatalf("detail:\n%s", out)
	}
}

func TestIdleParentFailureKeepsOlderData(t *testing.T) {
	pb := &parentBackend{
		rows:    []data.Agent{run("w1", "orch", 2, "worker")},
		threads: map[string]data.Thread{"orch": parentThread("orch", "Release", usage(0, "openai", "gpt-5.6-terra"))},
	}
	m, c := newModel(t, pb.fake())
	pump(m, m.Init())
	pb.set(func() { pb.errs = map[string]error{"orch": errors.New("timeout")} })
	c.add(16e9)
	pump(m, m.refresh(c.now()))
	if row := idleRow(m, "orch"); !strings.Contains(row, "Release") || !strings.Contains(row, "openai:gpt-5.6-terra") {
		t.Fatalf("row %q", row)
	}
	m.Update(key("home"))
	if out := plain(m); !strings.Contains(out, "lookup failed: timeout") {
		t.Fatalf("detail:\n%s", out)
	}
}

func TestIdleParentAliasAndReset(t *testing.T) {
	alias := parentThread("topic", "Resumed topic", usage(0, "claude-cli", "claude-sonnet-5"))
	alias.SourceID = "source-thread"
	pb := &parentBackend{rows: []data.Agent{run("w1", "topic", 2, "worker")}, threads: map[string]data.Thread{"topic": alias}}
	m, c := newModel(t, pb.fake())
	pump(m, m.Init())
	m.Update(key("home"))
	if out := plain(m); !strings.Contains(out, "alias of source-thread") || !strings.Contains(out, "claude-cli:claude-sonnet-5") {
		t.Fatalf("screen:\n%s", out)
	}
	// History reset: the cursor goes back; the next look reads it all.
	pb.set(func() {
		th := parentThread("topic", "Resumed topic")
		th.SourceID = "source-thread"
		pb.threads["topic"] = th
	})
	c.add(16e9)
	pump(m, m.refresh(c.now()))
	c.add(16e9)
	pump(m, m.refresh(c.now()))
	if got := pb.callLog(); got != "topic@ topic@1 topic@" {
		t.Fatalf("calls %q", got)
	}
	if row := idleRow(m, "topic"); !strings.Contains(row, "—") || strings.Contains(row, "claude-sonnet-5") {
		t.Fatalf("after a reset the old model must go: %q", row)
	}
}

func TestIdleParentActiveTransitions(t *testing.T) {
	pb := &parentBackend{
		rows:    []data.Agent{run("w1", "orch", 2, "worker")},
		threads: map[string]data.Thread{"orch": parentThread("orch", "Release", usage(0, "openai", "gpt-5.6-terra"))},
	}
	m, c := newModel(t, pb.fake())
	pump(m, m.Init())
	m.Update(key("j"))
	// The parent starts a run: it becomes a run row, and no more lookups.
	pb.set(func() { pb.rows = []data.Agent{run("orch", "", 1, "orchestrator"), run("w1", "orch", 2, "worker")} })
	c.add(3e9)
	pump(m, m.refresh(c.now()))
	if a := m.agents.selectedAgent(); a == nil || str(a.ThreadID) != "w1" {
		t.Fatalf("selection moved to %+v", a)
	}
	if m.agents.parents["orch"].shown || strings.Contains(plain(m), "◌") {
		t.Fatal("a running parent is not a context row")
	}
	c.add(16e9)
	pump(m, m.refresh(c.now()))
	if got := pb.callLog(); got != "orch@" {
		t.Fatalf("calls %q", got)
	}
	// Its run ends again: at once, a look from its cursor (not the whole
	// thread), which shows the model its run used.
	pb.set(func() {
		pb.rows = []data.Agent{run("w1", "orch", 2, "worker")}
		th := pb.threads["orch"]
		th.Activity = append(th.Activity, usage(1, "claude-cli", "claude-opus-5-5"))
		th.Cursor = 2
		pb.threads["orch"] = th
	})
	c.add(3e9)
	pump(m, m.refresh(c.now()))
	if got := pb.callLog(); got != "orch@ orch@1" {
		t.Fatalf("calls %q", got)
	}
	if row := idleRow(m, "orch"); !strings.Contains(row, "Release") || !strings.Contains(row, "claude-cli:claude-opus-5-5") {
		t.Fatalf("row %q", row)
	}
}

// More idle parents than one fetch reads: every one is read in turn, the
// never-read ones first, and each later look is a delta.
func TestIdleParentLookupsRotate(t *testing.T) {
	pb := &parentBackend{threads: map[string]data.Thread{}}
	const n = 20
	for i := 0; i < n; i++ {
		id := "p" + itoa(i)
		pb.rows = append(pb.rows, run("w"+itoa(i), id, uint32(i+1), "worker"))
		pb.threads[id] = parentThread(id, "T"+itoa(i), usage(0, "openai", "m"+itoa(i)))
	}
	m, c := newModel(t, pb.fake())
	pump(m, m.Init())
	for range 2 {
		c.add(16e9)
		pump(m, m.refresh(c.now()))
	}
	calls := strings.Fields(pb.callLog())
	if len(calls) != 3*maxParentLookups {
		t.Fatalf("%d lookups: %q", len(calls), calls)
	}
	full := map[string]int{}
	for _, call := range calls[:n] {
		if id, cursor, _ := strings.Cut(call, "@"); cursor == "" {
			full[id]++
		}
	}
	if len(full) != n {
		t.Fatalf("not every parent was read once before any second read: %q", calls)
	}
	for _, call := range calls[n:] {
		if strings.HasSuffix(call, "@") {
			t.Fatalf("a second look read the whole thread: %q", calls)
		}
	}
	for i := 0; i < n; i++ {
		if p := m.agents.parents["p"+itoa(i)]; !p.loaded || p.model != "m"+itoa(i) {
			t.Fatalf("p%d: %+v", i, p)
		}
	}
}

// An answer for an entry that changed while the look ran is dropped: a
// delta must never land on an entry that has not had its full read.
func TestIdleParentStaleAnswerIsDropped(t *testing.T) {
	m, _ := newModel(t, &fake{})
	m.agents.parents = map[string]parentInfo{"orch": {shown: true}}
	two := uint64(2)
	sent := []parentLookup{{id: "orch", cursor: &two, sourceID: "orch"}}
	m.applyParents(parentsResult{"orch": {loaded: true, title: "Delta only", cursor: &two, sourceID: "orch"}}, sent)
	if p := m.agents.parents["orch"]; p.loaded || p.title != "" {
		t.Fatalf("stale delta applied: %+v", p)
	}
	// A look of the same entry applies.
	m.applyParents(parentsResult{"orch": {loaded: true, title: "Full", cursor: &two, sourceID: "orch"}}, []parentLookup{{id: "orch"}})
	if p := m.agents.parents["orch"]; !p.loaded || p.title != "Full" || !p.shown || p.looked == 0 {
		t.Fatalf("full look not applied: %+v", p)
	}
}

// Parents the tree no longer shows are kept for a return, up to a bound.
func TestIdleParentCacheIsBounded(t *testing.T) {
	var ta agentsTab
	for i := 0; i < maxKeptParents+10; i++ {
		ta.tree = []agentNode{{thread: "p" + itoa(i), run: -1}}
		ta.syncParents()
	}
	ta.tree = nil
	ta.syncParents()
	if len(ta.parents) != maxKeptParents {
		t.Fatalf("%d kept", len(ta.parents))
	}
	if _, ok := ta.parents["p0"]; ok {
		t.Fatal("the oldest hidden parent must go first")
	}
	if _, ok := ta.parents["p"+itoa(maxKeptParents+9)]; !ok {
		t.Fatal("the newest hidden parent must stay")
	}
}

func TestIdleParentLookupsAreBounded(t *testing.T) {
	pb := &parentBackend{threads: map[string]data.Thread{}}
	for i := 0; i < 20; i++ {
		id := "p" + itoa(i)
		pb.rows = append(pb.rows, run("w"+itoa(i), id, uint32(i+1), "worker"))
		pb.threads[id] = parentThread(id, "T"+itoa(i), usage(0, "openai", "m"))
	}
	m, _ := newModel(t, pb.fake())
	pump(m, m.Init())
	if n := len(strings.Fields(pb.callLog())); n != maxParentLookups {
		t.Fatalf("%d lookups, want %d", n, maxParentLookups)
	}
}

func TestIdleParentTextIsCleaned(t *testing.T) {
	th := parentThread("orch", "Bad \x1b]8;;https://x\x07title", usage(0, "open\x1b[31mai", "gpt\x07"))
	pb := &parentBackend{rows: []data.Agent{run("w1", "orch", 2, "worker")}, threads: map[string]data.Thread{"orch": th}}
	m, _ := newModel(t, pb.fake())
	pump(m, m.Init())
	p := m.agents.parents["orch"]
	for _, s := range []string{p.title, p.model, p.provider} {
		if strings.ContainsAny(s, "\x1b\x07") {
			t.Fatalf("not cleaned: %q", s)
		}
	}
}
