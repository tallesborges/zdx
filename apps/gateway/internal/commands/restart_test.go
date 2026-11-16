package commands

import (
	"context"
	"errors"
	"strings"
	"testing"
)

type fakeLifecycle struct {
	supervised bool
	queued     QueuedRestart
	queueErr   error
	result     RestartResult
	restartErr error
	queuedFor  []string
	forced     []bool
	exits      int
}

func (f *fakeLifecycle) Supervised(context.Context) bool { return f.supervised }

func (f *fakeLifecycle) QueueRestart(_ context.Context, thread string) (QueuedRestart, error) {
	f.queuedFor = append(f.queuedFor, thread)
	return f.queued, f.queueErr
}

func (f *fakeLifecycle) RestartServices(_ context.Context, force bool) (RestartResult, error) {
	f.forced = append(f.forced, force)
	return f.result, f.restartErr
}

func (f *fakeLifecycle) RequestExit() { f.exits++ }

func TestRestart(t *testing.T) {
	tg := newTG()
	lc := &fakeLifecycle{}
	h := &Handler{Telegram: tg, Lifecycle: lc}
	run := func(text string) string {
		t.Helper()
		if !h.HandleMessage(context.Background(), topicMsg(10, text), ordinary()) {
			t.Fatalf("%q not handled", text)
		}
		return tg.lastSend(t).text
	}

	if got := run("/restart"); got != notSupervised || len(lc.forced) != 0 {
		t.Fatalf("unsupervised %q", got)
	}
	lc.supervised = true

	lc.queued = QueuedRestart{ActiveRuns: 2}
	if got := run("/restart q"); got != "⏳ Restart queued. I'll restart once no agent run, worker, or pending orchestrator turn is left (2 agent runs active now).\n\n<code>/restart f</code> restarts immediately instead." {
		t.Fatalf("queued %q", got)
	}
	if lc.queuedFor[0] != threadOf7 {
		t.Fatalf("announced in %v", lc.queuedFor)
	}
	lc.queued = QueuedRestart{AlreadyPending: true}
	if got := run("/restart queue"); !strings.HasPrefix(got, "⏳ A restart is already queued;") || !strings.Contains(got, "(0 agent runs active now) and will be announced here.") {
		t.Fatalf("pending %q", got)
	}
	lc.queueErr = errors.New("bot <not> supervised")
	if got := run("/restart q"); got != "⚠️ Couldn't queue the restart.\n<code>bot &lt;not&gt; supervised</code>" {
		t.Fatalf("queue error %q", got)
	}

	lc.result = RestartResult{Outcome: RestartBlocked, Text: "2 agent runs are active"}
	if got := run("/restart"); got != "⚠️ 2 agent runs are active.\n\n<code>/restart q</code> — restart automatically once it finishes\n<code>/restart f</code> — interrupt it and restart now" || lc.exits != 0 {
		t.Fatalf("blocked %q", got)
	}
	lc.result = RestartResult{Outcome: RestartDone, Text: "Restarted scheduler, orchestrator"}
	if got := run("/restart@zdx_bot f"); got != "🔄 Restarted scheduler, orchestrator.\n👋 Exiting bot… launchd will restart it shortly." || lc.exits != 1 {
		t.Fatalf("restarted %q exits %d", got, lc.exits)
	}
	lc.restartErr = errors.New("launchctl failed")
	if got := run("/restart --force"); got != "⚠️ Service restart failed, so I left the bot running.\n<code>launchctl failed</code>" || lc.exits != 1 {
		t.Fatalf("failed %q", got)
	}
	if strings.Join([]string{boolText(lc.forced[0]), boolText(lc.forced[1]), boolText(lc.forced[2])}, ",") != "false,true,true" {
		t.Fatalf("force %v", lc.forced)
	}

	// /restart works in General too; with arguments it is no command.
	if !h.HandleMessage(context.Background(), generalMsg(11, "/restart f"), generalThread) {
		t.Fatal("/restart in General")
	}
	if h.HandleMessage(context.Background(), topicMsg(12, "/restart now"), ordinary()) {
		t.Fatal("/restart now is not a command")
	}
	h.Lifecycle = nil
	if h.HandleMessage(context.Background(), topicMsg(13, "/restart"), ordinary()) {
		t.Fatal("without a lifecycle the bot refuses /restart")
	}
}

func boolText(b bool) string {
	if b {
		return "true"
	}
	return "false"
}

func TestHookRunsRestartOffTheQueue(t *testing.T) {
	tg := newTG()
	lc := &fakeLifecycle{supervised: true, result: RestartResult{Outcome: RestartDone, Text: "ok"}}
	k := NewHook(&Handler{Telegram: tg, Lifecycle: lc})
	if !k.HandleCommand(context.Background(), request("/restart")) {
		t.Fatal("claimed")
	}
	k.Wait()
	if lc.exits != 1 {
		t.Fatal("restarted")
	}
}
