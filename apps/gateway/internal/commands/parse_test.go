package commands

import (
	"reflect"
	"testing"
)

func TestParseMatchesTheRustBot(t *testing.T) {
	cases := map[string]Command{
		"/new":                       CmdNew,
		" /new@zdx_bot ":             CmdNew,
		"/restart":                   CmdRestart,
		"/restart@zdx_bot --force":   CmdRestart,
		"/restart q":                 CmdRestart,
		"/status@zdx_bot":            CmdStatus,
		" /whereami@zdx_bot ":        CmdWhereAmI,
		"/handoff":                   CmdHandoff,
		"/btw@zdx_bot":               CmdBtw,
		"/launcher@zdx_bot":          CmdLauncher,
		"/menu":                      CmdLauncher,
		"/worktree create":           CmdWorktree,
		"/worktree create@zdx_bot":   CmdWorktree,
		"/worktree@zdx_bot create":   CmdWorktree,
		"/worktree create@zdx_bot x": CmdWorktree,
		"/wt@zdx_bot":                CmdWorktree,
		"/tldr":                      CmdTldr,
		"/prompt-builder":            CmdPromptBuilder,
		"/prompt_builder@zdx_bot":    CmdPromptBuilder,
		"/thread-id":                 CmdThreadID,
		"/thread":                    CmdThreads,
		"/threads":                   CmdThreads,
		"/commands@zdx_bot":          CmdCommands,
	}
	for text, want := range cases {
		if got, ok := Parse(text); !ok || got != want {
			t.Errorf("%q: got %q %v, want %q", text, got, ok, want)
		}
	}
	for _, text := range []string{"hello", "/new please", "/exit", "/restart please", "/restart@zdx_bot please", "/worktree please", "/handoff please", "/btw what files did we touch", "/cancel"} {
		if c, ok := Parse(text); ok {
			t.Errorf("%q must not parse, got %q", text, c)
		}
	}
}

func TestRestartModesAreStrict(t *testing.T) {
	cases := map[string]RestartMode{
		"/restart": RestartGated, "/restart f": RestartForce, "/restart force": RestartForce,
		"/restart --force": RestartForce, " /restart@zdx_bot f ": RestartForce,
		"/restart q": RestartQueued, "/restart queue": RestartQueued,
	}
	for text, want := range cases {
		if got, ok := ParseRestart(text); !ok || got != want {
			t.Errorf("%q: got %q %v", text, got, ok)
		}
	}
	for _, text := range []string{"/restart now", "/restart f now", "/restart q f", "/restart@ f"} {
		if _, ok := ParseRestart(text); ok {
			t.Errorf("%q must not parse", text)
		}
	}
}

func TestParseModel(t *testing.T) {
	cases := map[string]ModelCommand{
		"/model":                                 {Action: ModelShow},
		"/model@zdx_bot":                         {Action: ModelShow},
		"/model@zdx_bot list":                    {Action: ModelList},
		"/model set anthropic:claude-sonnet-4-5": {Action: ModelSet, Model: "anthropic:claude-sonnet-4-5"},
		"/model set m extra":                     {Action: ModelSet, Model: "m"},
		"/model set":                             {Action: ModelShow},
		"/model reset":                           {Action: ModelReset},
		"/model reset now":                       {Action: ModelShow},
	}
	for text, want := range cases {
		if got, ok := ParseModel(text); !ok || got != want {
			t.Errorf("%q: got %+v %v", text, got, ok)
		}
	}
	for _, text := range []string{"/models", "model", "/modelset x"} {
		if _, ok := ParseModel(text); ok {
			t.Errorf("%q must not parse", text)
		}
	}
}

func TestQueueTakesItsText(t *testing.T) {
	if got, ok := ParseQueue("/queue run the tests"); !ok || got != "run the tests" {
		t.Fatalf("got %q %v", got, ok)
	}
	if got, ok := ParseQueue("/queue@zdx_bot  later "); !ok || got != "later" {
		t.Fatalf("got %q %v", got, ok)
	}
	for _, text := range []string{"/queue", "/queued x", "please /queue x", "/queue@ x"} {
		if _, ok := ParseQueue(text); ok {
			t.Errorf("%q must not parse", text)
		}
	}
}

func TestQueueBypassAndTopicBlocking(t *testing.T) {
	for _, text := range []string{"/status", "/status@zdx_bot", "/whereami", "/tldr", "/btw", "/restart", "/restart q", "/restart f", "/commands", "/threadid"} {
		if !BypassesQueue(text) {
			t.Errorf("%q bypasses the queue", text)
		}
	}
	for _, text := range []string{"/new", "/model", "/handoff", "/prompt-builder", "hello"} {
		if BypassesQueue(text) {
			t.Errorf("%q waits in the queue", text)
		}
	}
	for _, text := range []string{"/new", "/restart@zdx_bot", "/status", "/whereami", "/worktree", "/handoff", "/btw", "/launcher", "/model", "/model list"} {
		if !BlocksTopicAutocreate(text) {
			t.Errorf("%q blocks topic auto-creation", text)
		}
	}
	if BlocksTopicAutocreate("let's chat") {
		t.Error("text opens a topic")
	}
}

func TestMenuAndNativeNames(t *testing.T) {
	seen := map[string]bool{}
	for _, c := range MenuCommands() {
		if c.Command == "" || c.Description == "" || seen[c.Command] {
			t.Fatalf("bad menu entry %+v", c)
		}
		seen[c.Command] = true
	}
	want := []string{"new", "restart", "status", "whereami", "worktree", "handoff", "btw", "commands", "tldr", "prompt_builder", "threadid", "threads", "launcher", "model", "cancel", "queue"}
	if got := NativeNames(); !reflect.DeepEqual(got, want) {
		t.Fatalf("native names %v", got)
	}
}
