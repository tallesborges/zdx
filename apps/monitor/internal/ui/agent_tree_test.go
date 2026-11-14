package ui

import (
	"context"
	"strings"
	"testing"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
)

func run(thread, parent string, pid uint32, role string) data.Agent {
	a := data.Agent{PID: pid, Role: ptr(role), Run: data.RunStatus{State: "tool"}}
	if thread != "" {
		a.ThreadID = ptr(thread)
	}
	if parent != "" {
		a.ParentThreadID = ptr(parent)
	}
	return a
}

// shape is the tree as "prefix+role" lines (context rows as "(parent X)").
func shape(rows []data.Agent) string {
	var out []string
	for _, n := range agentTree(rows) {
		if n.run < 0 {
			out = append(out, "(parent "+n.thread+")")
			continue
		}
		out = append(out, n.prefix+str(rows[n.run].Role))
	}
	return strings.Join(out, "\n")
}

func TestAgentTreeNestsByReportedLineage(t *testing.T) {
	rows := []data.Agent{
		run("w1", "orch-a", 2, "worker"), // the engine lists oldest first: children can come first
		run("sub", "w1", 3, "explorer"),
		run("orch-a", "", 1, "orchestrator"),
		run("orch-b", "", 4, "orchestrator"),
		run("w2", "orch-b", 5, "worker"),
		run("w3", "orch-a", 6, "worker"),
		run("", "", 7, "chat"),
	}
	want := strings.Join([]string{
		"orchestrator",
		"├─ worker",
		"│  └─ explorer",
		"└─ worker",
		"orchestrator",
		"└─ worker",
		"chat",
	}, "\n")
	if got := shape(rows); got != want {
		t.Fatalf("got\n%s\nwant\n%s", got, want)
	}
}

func TestAgentTreeIdleParentIsAContextRow(t *testing.T) {
	rows := []data.Agent{
		run("w1", "orch-idle", 1, "worker"),
		run("w2", "orch-idle", 2, "worker"),
		run("sub", "gone", 3, "explorer"),
	}
	want := "(parent orch-idle)\n├─ worker\n└─ worker\n(parent gone)\n└─ explorer"
	if got := shape(rows); got != want {
		t.Fatalf("got\n%s\nwant\n%s", got, want)
	}
	tree := agentTree(rows)
	if tree[0].kids != 2 || tree[0].run != -1 {
		t.Fatalf("context row %+v", tree[0])
	}
}

func TestAgentTreeBreaksCyclesAndSelfLinks(t *testing.T) {
	rows := []data.Agent{
		run("a", "b", 1, "A"),
		run("b", "a", 2, "B"),
		run("c", "c", 3, "C"),
	}
	got := shape(rows)
	if got != "B\n└─ A\nC" {
		t.Fatalf("got\n%s", got)
	}
	// Every run appears exactly once.
	if n := len(agentTree(rows)); n != 3 {
		t.Fatalf("%d rows", n)
	}
	long := []data.Agent{run("x", "z", 1, "X"), run("y", "x", 2, "Y"), run("z", "y", 3, "Z")}
	if n := len(agentTree(long)); n != 3 {
		t.Fatalf("a three-run loop must keep three rows, got %d:\n%s", n, shape(long))
	}
}

func TestAgentTreeDuplicateThreadUsesTheFirstRun(t *testing.T) {
	rows := []data.Agent{run("t", "", 1, "first"), run("t", "", 2, "second"), run("kid", "t", 3, "child")}
	if got := shape(rows); got != "first\n└─ child\nsecond" {
		t.Fatalf("got\n%s", got)
	}
}

// TestAgentsSelectionFollowsARunThatMoves: a run that gets a parent (or
// loses it) keeps the selection.
func TestAgentsSelectionFollowsARunThatMoves(t *testing.T) {
	rows := []data.Agent{run("orch", "", 1, "orchestrator"), run("w", "", 2, "worker"), run("x", "", 3, "other")}
	f := &fake{agents: func(context.Context) ([]data.Agent, error) { return rows, nil }}
	m, c := newModel(t, f)
	pump(m, m.Init())
	m.Update(key("j"))
	if a := m.agents.selectedAgent(); a == nil || str(a.ThreadID) != "w" {
		t.Fatalf("selected %+v", a)
	}
	rows = []data.Agent{run("x", "", 3, "other"), run("w", "orch", 2, "worker"), run("orch", "", 1, "orchestrator")}
	c.add(3e9)
	pump(m, m.refresh(c.now()))
	if a := m.agents.selectedAgent(); a == nil || str(a.ThreadID) != "w" {
		t.Fatalf("selection moved to %+v", a)
	}
	if !strings.Contains(plain(m), "└─ worker") {
		t.Fatalf("not nested:\n%s", plain(m))
	}
	// A context row is selectable; enter opens that thread.
	rows = []data.Agent{run("w", "idle-orch", 2, "worker")}
	c.add(3e9)
	pump(m, m.refresh(c.now()))
	m.Update(key("k"))
	if n := m.agents.selectedNode(); n == nil || n.run != -1 || n.thread != "idle-orch" {
		t.Fatalf("node %+v", n)
	}
	out := plain(m)
	if !strings.Contains(out, "Active agents  1 run") || !strings.Contains(out, "idle parent") || !strings.Contains(out, "not counted as a run") {
		t.Fatalf("screen:\n%s", out)
	}
	_, cmd := m.Update(special(tea.KeyEnter))
	pump(m, cmd)
	if m.transcript == nil || m.transcript.threadID != "idle-orch" {
		t.Fatal("enter on a context row opens its thread")
	}
}

func TestModelSpecUsesOnlyReportedParts(t *testing.T) {
	cases := []struct {
		model, provider, account, thinking *string
		want                               string
	}{
		{ptr("gpt-6-astra"), ptr("openai-codex"), nil, ptr("high"), "openai-codex:gpt-6-astra@high"},
		{ptr("gpt-6-astra"), ptr("openai-codex"), ptr("work"), ptr("high"), "openai-codex@work:gpt-6-astra@high"},
		{ptr("claude-opus-5-5"), ptr("claude-cli"), ptr("work"), nil, "claude-cli@work:claude-opus-5-5"},
		{ptr("claude-cli@work:claude-opus-5-5"), ptr("claude-cli"), nil, ptr("high"), "claude-cli@work:claude-opus-5-5@high"},
		{ptr("openai:gpt-5.6-terra@low"), ptr("openai"), nil, ptr("low"), "openai:gpt-5.6-terra@low"},
		{ptr("moonshotai/kimi-k3"), nil, nil, nil, "moonshotai/kimi-k3"},
		{ptr("moonshotai/kimi-k3"), nil, nil, ptr("off"), "moonshotai/kimi-k3@off"},
		{nil, ptr("openai"), nil, ptr("high"), ""},
		// `@` and `:` inside a model id are not modifiers or boundaries.
		{ptr("claude-sonnet-9@20260101"), ptr("vertex"), nil, ptr("high"), "vertex:claude-sonnet-9@20260101@high"},
		{ptr("gemini-x@HIGH"), ptr("gemini"), nil, ptr("high"), "gemini:gemini-x@HIGH"},
		{ptr("gemini-x@high@fast"), ptr("gemini"), nil, ptr("high"), "gemini:gemini-x@high@fast"},
		{ptr("openrouter@team"), ptr("openrouter"), nil, nil, "openrouter:openrouter@team"},
		{ptr("meta/llama-9:free"), ptr("openrouter"), nil, nil, "openrouter:meta/llama-9:free"},
	}
	for _, tc := range cases {
		a := data.Agent{Model: tc.model, Provider: tc.provider, Account: tc.account, Thinking: tc.thinking}
		if got := modelSpec(a); got != tc.want {
			t.Errorf("%v: got %q, want %q", tc, got, tc.want)
		}
	}
}

func TestShortSpecKeepsModelAndThinking(t *testing.T) {
	a := data.Agent{Model: ptr("gpt-6-astra"), Provider: ptr("openai-codex"), Account: ptr("work"), Thinking: ptr("high")}
	spec := modelSpec(a)
	for w, want := range map[int]string{
		40: "openai-codex@work:gpt-6-astra@high",
		30: "openai-codex:gpt-6-astra@high",
		24: "openai…:gpt-6-astra@high",
		18: "…:gpt-6-astra@high",
		12: "…-astra@high",
	} {
		got := spec
		if ansi.StringWidth(spec) > w {
			got = shortSpec(a, w)
		}
		if got != want || ansi.StringWidth(got) > w {
			t.Errorf("%d: got %q, want %q", w, got, want)
		}
	}
}

func TestShortSpecCutsAtTheProviderBoundary(t *testing.T) {
	a := data.Agent{Model: ptr("meta/llama-9:free"), Provider: ptr("openrouter")}
	for w, want := range map[int]string{
		30: "openrouter:meta/llama-9:free",
		26: "openrou…:meta/llama-9:free",
		22: "ope…:meta/llama-9:free",
		14: "…/llama-9:free",
	} {
		got := modelSpec(a)
		if ansi.StringWidth(got) > w {
			got = shortSpec(a, w)
		}
		if got != want || ansi.StringWidth(got) > w {
			t.Errorf("%d: got %q, want %q", w, got, want)
		}
	}
}
