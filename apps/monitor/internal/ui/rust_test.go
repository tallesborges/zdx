package ui

import (
	"context"
	"encoding/json"
	"os"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"

	"github.com/tallesborges/zdx/apps/monitor/internal/data"
	"github.com/tallesborges/zdx/apps/monitor/internal/realsvc"
	"github.com/tallesborges/zdx/apps/monitor/internal/source"
)

const (
	seedMain  = "seed-main"
	seedAlias = "seed-alias"
)

func ev(kind string, fields map[string]any) map[string]any {
	fields["type"] = kind
	if _, ok := fields["ts"]; !ok {
		fields["ts"] = realsvc.Now()
	}
	return fields
}

func bashResult(id, stdout string, started, completed string) map[string]any {
	return ev("tool_result", map[string]any{
		"tool_use_id": id, "ok": true, "duration_ms": 1200, "started_at": started, "completed_at": completed,
		"output": map[string]any{"ok": true, "data": map[string]any{
			"exit_code": 0, "stdout": stdout, "stderr": "", "stdout_total_bytes": len(stdout),
			"stderr_total_bytes": 0, "stdout_truncated": false, "stderr_truncated": false, "timed_out": false,
		}},
	})
}

// activityJSON is the comparable form of a transcript: every item as the
// engine sent it.
func activityJSON(t *testing.T, th data.Thread) string {
	t.Helper()
	b, err := json.Marshal(th.Activity)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func types(th data.Thread) string {
	var out []string
	for _, a := range th.Activity {
		out = append(out, a.Type)
	}
	return strings.Join(out, ",")
}

func find(th data.Thread, kind, id string) *data.Activity {
	for i := range th.Activity {
		a := &th.Activity[i]
		if a.Type == kind && (a.ID == id || a.ToolUseID == id) {
			return a
		}
	}
	return nil
}

// TestRealServiceTranscript seeds thread files in a temp home, starts the
// real orchestrator service there, and checks the monitor's transcript
// against what the engine sends: a full load, an alias, incremental polls
// with the cursor, a running tool from a run marker and its completion,
// and the trajectory view. Then it drives the UI model over the real
// backend. Opt in with ZDX_MONITOR_TEST_ZDX (a prebuilt binary).
func TestRealServiceTranscript(t *testing.T) {
	bin := realsvc.Binary(t)
	home, root := realsvc.Home(t), realsvc.Home(t)
	t0 := time.Now().UTC().Add(-time.Minute)
	at := func(d time.Duration) string { return t0.Add(d).Format("2006-01-02T15:04:05.000Z") }

	realsvc.AppendEvents(t, home, seedMain,
		ev("meta", map[string]any{"schema_version": 1, "title": "Seeded transcript", "root_path": root, "ts": at(0)}),
		ev("message", map[string]any{"role": "user", "text": "run the tests", "ts": at(time.Second)}),
		ev("reasoning", map[string]any{"text": "plan: run go test first", "ts": at(2 * time.Second)}),
		ev("usage", map[string]any{"input_tokens": 1200, "output_tokens": 80, "cache_read_tokens": 0, "cache_write_tokens": 0,
			"model": "test-model", "provider": "test", "duration_ms": 900, "ttft_ms": 300,
			"started_at": at(time.Second), "completed_at": at(1900 * time.Millisecond), "ts": at(2 * time.Second)}),
		ev("tool_use", map[string]any{"id": "c1", "name": "bash", "input": map[string]any{"command": "go test ./..."}, "ts": at(2 * time.Second)}),
		bashResult("c1", "ok  example/pkg 0.1s\n", at(2*time.Second), at(3200*time.Millisecond)),
		ev("message", map[string]any{"role": "assistant", "text": "All tests pass.", "ts": at(4 * time.Second)}),
	)
	realsvc.AppendEvents(t, home, seedAlias,
		ev("meta", map[string]any{"schema_version": 1, "title": "Alias topic", "alias_to": seedMain, "ts": at(0)}))

	realsvc.Start(t, bin, home, root)
	src := source.New(source.Config{Root: root, Home: home, Zdx: bin})
	ctx := context.Background()

	// Full load.
	full, origin, err := src.Thread(ctx, seedMain, nil)
	if err != nil {
		t.Fatal(err)
	}
	if origin != source.OriginSocket || full.SourceID != seedMain || full.Title != "Seeded transcript" || full.Partial {
		t.Fatalf("full: origin %s source %q title %q partial %v", origin, full.SourceID, full.Title, full.Partial)
	}
	if full.Cursor != 7 {
		t.Fatalf("cursor %d, want the event count 7", full.Cursor)
	}
	if got := types(full); got != "message,reasoning,usage,tool_use,tool_result,message" {
		t.Fatalf("activity %s", got)
	}
	if r := full.Activity[1]; r.Text != "plan: run go test first" || r.Redacted {
		t.Fatalf("reasoning %+v", r)
	}
	res := find(full, "tool_result", "c1")
	if res == nil || res.View == nil || res.View.Outcome != "ok" || res.View.Head == nil || res.View.Head.Command != "go test ./..." ||
		res.View.Body.Kind != "streams" || len(res.View.Body.Streams) != 1 || !strings.Contains(res.View.Body.Streams[0].Text, "example/pkg") {
		t.Fatalf("tool result %+v", res)
	}
	if u := find(full, "tool_use", "c1"); u == nil || u.View != nil {
		t.Fatalf("a call with a result carries its view on the result, not the use: %+v", u)
	}
	if full.Activity[0].Speaker != "You" || full.Activity[5].Speaker != "Z" {
		t.Fatalf("speakers %q %q", full.Activity[0].Speaker, full.Activity[5].Speaker)
	}

	// Alias: the requested id stays, the events come from the source.
	alias, _, err := src.Thread(ctx, seedAlias, nil)
	if err != nil {
		t.Fatal(err)
	}
	if alias.ID != seedAlias || alias.SourceID != seedMain || activityJSON(t, alias) != activityJSON(t, full) {
		t.Fatalf("alias: id %q source %q types %s", alias.ID, alias.SourceID, types(alias))
	}

	// A new call without a result: the delta holds only it.
	realsvc.AppendEvents(t, home, seedMain,
		ev("tool_use", map[string]any{"id": "c2", "name": "bash", "input": map[string]any{"command": "go vet ./..."}}))
	after := full.Cursor
	delta, _, err := src.Thread(ctx, seedMain, &after)
	if err != nil {
		t.Fatal(err)
	}
	if !delta.Partial || delta.Cursor != 8 || types(delta) != "tool_use" || delta.Activity[0].Sequence != 7 {
		t.Fatalf("delta: partial %v cursor %d %s", delta.Partial, delta.Cursor, types(delta))
	}
	if v := delta.Activity[0].View; v == nil || v.Outcome != "pending" {
		t.Fatalf("a call with no result carries an input-only view: %+v", v)
	}
	merged := mergeThread(&full, delta)
	fresh, _, err := src.Thread(ctx, seedMain, nil)
	if err != nil {
		t.Fatal(err)
	}
	if activityJSON(t, merged) != activityJSON(t, fresh) {
		t.Fatalf("merged delta differs from a full load:\n%s\n%s", activityJSON(t, merged), activityJSON(t, fresh))
	}

	// A running round from a run marker: the delta resends it as tool_running
	// at the cursor, with the run status.
	marker := func(completed bool) map[string]any {
		tool := map[string]any{"id": "c3", "name": "Bash", "summary": "sleep 30", "input": map[string]any{"command": "sleep 30"},
			"output_tail": "waiting", "started_at": realsvc.Now()}
		if completed {
			tool["completed_at"], tool["ok"], tool["duration_ms"] = realsvc.Now(), true, 2000
		}
		now := realsvc.Now()
		return map[string]any{"pid": os.Getpid(), "started_at": now, "thread_id": seedMain, "surface": "exec",
			"model": "test-model", "provider": "test", "kind": "agent", "current_tools": []any{tool},
			"phase": "tool", "last_activity_at": now, "heartbeat_at": now}
	}
	realsvc.WriteRunMarker(t, home, "seed-run", marker(false))
	after = fresh.Cursor
	live, _, err := src.Thread(ctx, seedMain, &after)
	if err != nil {
		t.Fatal(err)
	}
	run := find(live, "tool_running", "c3")
	if live.Run == nil || live.Run.State != "tool" || run == nil || run.Sequence != 8 || run.Name != "bash" || run.RunningFor == "" ||
		run.OutputTail != "waiting" || run.View == nil || run.View.Head == nil || run.View.Head.Command != "sleep 30" {
		t.Fatalf("live: run %+v tool %+v", live.Run, run)
	}
	agents, _, err := src.ActiveAgents(ctx)
	if err != nil || len(agents) != 1 || str(agents[0].ThreadID) != seedMain || agents[0].Run.RunningTools != 1 {
		t.Fatalf("agents %+v %v", agents, err)
	}
	merged = mergeThread(&merged, live)
	if types(merged) != "message,reasoning,usage,tool_use,tool_result,message,tool_use,tool_running" {
		t.Fatalf("merged live %s", types(merged))
	}

	// The call finished but its round is not in the file yet: the delta
	// resends it as tool_running with completed_at and ok, and the
	// transcript says it finished, not that it runs.
	realsvc.WriteRunMarker(t, home, "seed-run", marker(true))
	finished, _, err := src.Thread(ctx, seedMain, &after)
	if err != nil {
		t.Fatal(err)
	}
	done := find(finished, "tool_running", "c3")
	if done == nil || done.CompletedAt == nil || done.OK == nil || !*done.OK || done.DurationMS == nil || *done.DurationMS != 2000 {
		t.Fatalf("finished live tool %+v", done)
	}
	finishedLine, _ := renderActivity(&finished, 120, false)
	if got := ansi.Strip(strings.Join(finishedLine, "\n")); !strings.Contains(got, "✓ bash $ sleep 30 finished 2s, not saved yet") || strings.Contains(got, "sleep 30 running") {
		t.Fatalf("finished live tool renders as:\n%s", got)
	}

	// The round lands in the file while the marker still lists the call:
	// the engine skips the persisted id, and the merge drops the old
	// running row.
	realsvc.AppendEvents(t, home, seedMain,
		ev("tool_use", map[string]any{"id": "c3", "name": "bash", "input": map[string]any{"command": "sleep 30"}}),
		bashResult("c3", "", realsvc.Now(), realsvc.Now()))
	after = live.Cursor
	landed, _, err := src.Thread(ctx, seedMain, &after)
	if err != nil {
		t.Fatal(err)
	}
	if types(landed) != "tool_use,tool_result" || landed.Cursor != 10 {
		t.Fatalf("landed %s cursor %d", types(landed), landed.Cursor)
	}
	merged = mergeThread(&merged, landed)
	realsvc.RemoveRunMarker(t, home, "seed-run")
	final, _, err := src.Thread(ctx, seedMain, nil)
	if err != nil {
		t.Fatal(err)
	}
	if final.Run != nil || activityJSON(t, merged) != activityJSON(t, final) {
		t.Fatalf("after completion the merge must equal a full load:\n%s\n%s", activityJSON(t, merged), activityJSON(t, final))
	}

	// Trajectory.
	tr, _, err := src.Trajectory(ctx, seedMain)
	if err != nil {
		t.Fatal(err)
	}
	if tr.SourceEventCount != 10 || len(tr.Turns) != 1 || tr.Total == 0 {
		t.Fatalf("trajectory %+v", tr)
	}
	var tool, model *data.TrajectorySpan
	for i := range tr.Turns[0].Spans {
		s := &tr.Turns[0].Spans[i]
		if s.Kind == "tool" && s.ToolUseID != nil && *s.ToolUseID == "c1" {
			tool = s
		}
		if s.Kind == "model" {
			model = s
		}
	}
	if tool == nil || tool.Status != "ok" || tool.DurationMS == nil || *tool.DurationMS != 1200 {
		t.Fatalf("tool span %+v", tool)
	}
	if model == nil || model.TTFTMS == nil || *model.TTFTMS != 300 {
		t.Fatalf("model span %+v", model)
	}

	// The UI over the real backend.
	uictx, cancel := context.WithCancel(ctx)
	defer cancel()
	m := New(uictx, src, Options{Root: root, Home: home})
	m.Update(tea.WindowSizeMsg{Width: 140, Height: 40})
	pump(m, m.Init())
	_, cmd := m.Update(key("2"))
	pump(m, cmd)
	if !strings.Contains(plain(m), "Seeded transcript") {
		t.Fatalf("threads tab:\n%s", plain(m))
	}
	pump(m, m.openTranscript(seedAlias, seedAlias))
	out := plain(m)
	for _, want := range []string{"Seeded transcript", "alias of seed-main", "run the tests", "✓ bash $ go test ./...", "bash $ go vet ./... (no result)", "∴ thinking · 1 lines"} {
		if !strings.Contains(out, want) {
			t.Fatalf("transcript lacks %q:\n%s", want, out)
		}
	}
	_, cmd = m.Update(key("i"))
	pump(m, cmd)
	out = plain(m)
	for _, want := range []string{"Client-observed trajectory", "Turn 1", "1.2s", "TTFT 300ms"} {
		if !strings.Contains(out, want) {
			t.Fatalf("trajectory lacks %q:\n%s", want, out)
		}
	}
	m.Update(key("i"))
	if m.transcript == nil || m.transcript.traj != nil {
		t.Fatal("i closes the trajectory and keeps the transcript")
	}
}
