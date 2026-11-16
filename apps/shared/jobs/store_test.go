package jobs

import (
	"encoding/json"
	"io"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

var quiet = slog.New(slog.NewTextHandler(io.Discard, nil))

func TestAClaimIsCreatedOnceAndFinishedInPlace(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	first := NewClaim("demo", "20261020T120000Z", TriggerScheduler, Once, time.Now())
	if ok, err := home.CreateClaim(first); !ok || err != nil {
		t.Fatal(ok, err)
	}
	if ok, err := home.CreateClaim(NewClaim("demo", "20261020T120000Z", TriggerScheduler, Once, time.Now())); ok || err != nil {
		t.Fatal("the first claim wins", ok, err)
	}
	stored, err := home.ReadClaim("demo", "20261020T120000Z")
	if err != nil || stored.RunID != first.RunID || stored.Outcome != nil {
		t.Fatal(stored, err)
	}
	if err := home.UpdateClaim(stored.Finished(Succeeded, nil)); err != nil {
		t.Fatal(err)
	}
	claims, _ := home.AllClaims(quiet)
	if len(claims) != 1 || claims[0].Outcome == nil {
		t.Fatal(claims)
	}
	entries, _ := os.ReadDir(filepath.Join(home.Dir, "state/jobs/demo/claims"))
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".tmp") {
			t.Fatal("leftover", e.Name())
		}
	}
}

// The files keep the field names, order, and formats the Rust runner wrote
// (docs/SPEC.md §8 Jobs, State files), so existing state is read as is,
// including the trigger `daemon` of runs from before the scheduler rename.
func TestStateFilesKeepTheirFormat(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	existing := `{
  "job": "reminders-notify",
  "key": "20261115T0530",
  "run_id": "52f00b78-76f4-4f2f-a2b9-5e17f8f2bb8b",
  "trigger": "daemon",
  "schedule_kind": "recurring",
  "scheduled_for": "2026-11-15T08:30:00Z",
  "claimed_at": "2026-11-15T08:30:00.254097Z",
  "pid": 95044,
  "outcome": "succeeded",
  "finished_at": "2026-11-15T08:31:34.533556Z",
  "error": null,
  "thread_id": "job-reminders-notify-20261115-083000"
}`
	writeFile(t, home.claimPath("reminders-notify", "20261115T0530"), existing)
	c, err := home.ReadClaim("reminders-notify", "20261115T0530")
	if err != nil {
		t.Fatal(err)
	}
	if err := home.UpdateClaim(*c); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, home.claimPath("reminders-notify", "20261115T0530")); got != existing {
		t.Fatalf("claim round trip changed it:\n%s", got)
	}

	line := `{"job":"nightly","run_id":"f89101ba-e1cf-4a17-9b11-1ebe11bd9ec9","trigger":"daemon","schedule_kind":"recurring","scheduled_for":"2026-11-15T03:30:00+00:00","lateness_secs":0,"thread_id":"job-nightly-20261115-033000","attempt":1,"max_attempts":2,"started_at":"2026-11-15T03:30:00.449929+00:00","finished_at":"2026-11-15T03:32:34.630984+00:00","duration_ms":154179,"ok":true,"outcome":"succeeded","error":null,"schedule":"30 */3 * * *","model":"claude-cli:claude-opus-5-5@high","subagent":null,"root":"/work"}`
	old := `{"job":"legacy","trigger":"daemon","attempt":1,"max_attempts":1,"started_at":"2026-10-01T11:00:00Z","finished_at":"2026-10-01T11:00:10Z","duration_ms":10000,"ok":true,"error":null,"schedule":"0 8 * * *","model":null,"subagent":null}`
	writeFile(t, home.RunsLog(), line+"\n"+old+"\nnot json\n{\"job\":\"no-trigger\"}\n")
	records, err := home.ReadRunRecords(quiet)
	if err != nil || len(records) != 2 {
		t.Fatal(records, err)
	}
	encoded, _ := compactJSON(records[0])
	if string(encoded) != line {
		t.Fatalf("record round trip changed it:\n%s", encoded)
	}
	if records[1].RunID != nil || records[1].Outcome != nil || !records[1].OK {
		t.Fatalf("%+v", records[1])
	}
	if err := home.AppendRunRecord(records[0]); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, home.RunsLog()); !strings.HasSuffix(got, "\n"+line+"\n") {
		t.Fatal(got)
	}

	var last LastRun
	lastJSON := `{
  "run_id": "52f00b78-76f4-4f2f-a2b9-5e17f8f2bb8b",
  "trigger": "scheduler",
  "outcome": "succeeded",
  "scheduled_for": "2026-11-15T08:30:00Z",
  "finished_at": "2026-11-15T08:31:34.54121Z",
  "error": null,
  "thread_id": "job-reminders-notify-20261115-083000"
}`
	if err := json.Unmarshal([]byte(lastJSON), &last); err != nil {
		t.Fatal(err)
	}
	if err := home.WriteLastRun("r", last); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, filepath.Join(home.Dir, "state/jobs/r/last.json")); got != lastJSON {
		t.Fatal(got)
	}
}

func TestTheLockIsExclusiveAndReleased(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	if home.IsRunning("demo") {
		t.Fatal("no lock file yet")
	}
	lock, err := home.TryLock("demo")
	if err != nil || lock == nil {
		t.Fatal(err)
	}
	if !home.IsRunning("demo") {
		t.Fatal("held")
	}
	if again, err := home.TryLock("demo"); again != nil || err != nil {
		t.Fatal("exclusive", err)
	}
	lock.Release()
	if home.IsRunning("demo") {
		t.Fatal("released")
	}
	if again, _ := home.TryLock("demo"); again == nil {
		t.Fatal("free again")
	} else {
		again.Release()
	}
}

func TestPruningKeepsOnceClaimsAndRecentOrUnfinishedRecurringOnes(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	old := time.Now().Add(-30 * 24 * time.Hour)
	recurringOld := NewClaim("r", "20260901T0900", TriggerScheduler, Recurring, old).Finished(Succeeded, nil)
	recurringRunning := NewClaim("r", "20260902T0900", TriggerScheduler, Recurring, old)
	onceOld := NewClaim("o", "20260901T120000Z", TriggerScheduler, Once, old).Finished(Failed, nil)
	for _, c := range []Claim{recurringOld, recurringRunning, onceOld} {
		if _, err := home.CreateClaim(c); err != nil {
			t.Fatal(err)
		}
	}
	removed, err := home.PruneRecurringClaims(time.Now().Add(-ClaimRetention), quiet)
	if err != nil || removed != 1 {
		t.Fatal(removed, err)
	}
	for key, want := range map[[2]string]bool{{"r", "20260901T0900"}: false, {"r", "20260902T0900"}: true, {"o", "20260901T120000Z"}: true} {
		if c, _ := home.ReadClaim(key[0], key[1]); (c != nil) != want {
			t.Errorf("%v kept=%v", key, c != nil)
		}
	}
}

func onceJob(t *testing.T, home Home, name, runAt, body string) string {
	return writeFile(t, filepath.Join(home.JobsDir(), name+".md"), "---\nrun_at: \""+runAt+"\"\n---\n"+body+"\n")
}

func runAtOf(t *testing.T, raw string) time.Time {
	at, err := parseRunAt(raw)
	if err != nil {
		t.Fatal(err)
	}
	return at
}

func TestArchiveMovesOnlyAFileThatStillHasTheSameRunAt(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	runAt := runAtOf(t, "2026-10-20T09:00:00-03:00")
	path := onceJob(t, home, "remind", "2026-10-20T10:00:00-03:00", "Rescheduled.")
	if dest, err := home.ArchiveOnceJob(path, runAt); dest != "" || err != nil {
		t.Fatal("a rescheduled job stays", dest, err)
	}
	onceJob(t, home, "remind", "2026-10-20T12:00:00Z", "Same instant.")
	dest, err := home.ArchiveOnceJob(path, runAt)
	if err != nil || dest != filepath.Join(home.ArchiveDir(), "remind--20261020T120000Z.md") {
		t.Fatal(dest, err)
	}
	if _, err := os.Stat(path); !os.IsNotExist(err) {
		t.Fatal("moved")
	}
	onceJob(t, home, "remind", "2026-10-20T12:00:00Z", "Restored.")
	if dest, _ := home.ArchiveOnceJob(path, runAt); dest != filepath.Join(home.ArchiveDir(), "remind--20261020T120000Z-2.md") {
		t.Fatal(dest)
	}
	if dest, err := home.ArchiveOnceJob(path, runAt); dest != "" || err != nil {
		t.Fatal("gone", dest, err)
	}
}

func TestArchiveKeepsAFileReplacedDuringTheMove(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	runAt := runAtOf(t, "2026-10-20T12:00:00Z")
	path := onceJob(t, home, "remind", "2026-10-20T12:00:00Z", "Old.")
	rescheduled := "---\nrun_at: \"2026-10-21T12:00:00Z\"\n---\nNew.\n"
	dest, err := home.archiveOnceJob(path, runAt, func() {
		writeFile(t, path+".tmp", rescheduled)
		if err := os.Rename(path+".tmp", path); err != nil {
			t.Fatal(err)
		}
	})
	if err != nil || !strings.Contains(readFile(t, dest), "Old.") || readFile(t, path) != rescheduled {
		t.Fatal(dest, err)
	}
	entries, _ := os.ReadDir(home.JobsDir())
	for _, e := range entries {
		if strings.Contains(e.Name(), "archiving") {
			t.Fatal("stray", e.Name())
		}
	}
}

func TestArchiveSkipsAFileEditedInPlaceDuringTheMove(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	runAt := runAtOf(t, "2026-10-20T12:00:00Z")
	path := onceJob(t, home, "remind", "2026-10-20T12:00:00Z", "Old.")
	rescheduled := "---\nrun_at: \"2026-10-21T12:00:00Z\"\n---\nNew.\n"
	dest, err := home.archiveOnceJob(path, runAt, func() { writeFile(t, path, rescheduled) })
	if dest != "" || err != nil || readFile(t, path) != rescheduled {
		t.Fatal(dest, err)
	}
	if archived, _ := home.Archived(); len(archived) != 0 {
		t.Fatal("no archive copy left", archived)
	}
	// Removed during the move: the archived copy is the only one, kept.
	gone := onceJob(t, home, "gone", "2026-10-20T12:00:00Z", "Bye.")
	dest, err = home.archiveOnceJob(gone, runAt, func() { os.Remove(gone) })
	if err != nil || !strings.Contains(readFile(t, dest), "Bye.") {
		t.Fatal(dest, err)
	}
}

func TestArchiveNeverReplacesAnExistingArchiveFile(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	taken := writeFile(t, filepath.Join(home.ArchiveDir(), "remind--20261020T120000Z.md"), "earlier archive")
	path := onceJob(t, home, "remind", "2026-10-20T12:00:00Z", "Again.")
	dest, err := home.ArchiveOnceJob(path, runAtOf(t, "2026-10-20T12:00:00Z"))
	if err != nil || dest != filepath.Join(home.ArchiveDir(), "remind--20261020T120000Z-2.md") || readFile(t, taken) != "earlier archive" {
		t.Fatal(dest, err)
	}
}

func TestArchivedListsNewestFirstWithOutcomes(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	writeFile(t, filepath.Join(home.ArchiveDir(), "a--20261001T120000Z.md"), "x")
	writeFile(t, filepath.Join(home.ArchiveDir(), "b--20261002T120000Z-2.md"), "x")
	writeFile(t, filepath.Join(home.ArchiveDir(), "stray.md"), "x")
	for _, c := range []Claim{
		NewClaim("a", "20261001T120000Z", TriggerScheduler, Once, utc(2026, 10, 1, 12, 0)).Finished(Missed, ptr("late")),
		NewClaim("b", "20261002T120000Z", TriggerManual, Once, utc(2026, 10, 2, 12, 0)).Finished(Succeeded, nil),
	} {
		if _, err := home.CreateClaim(c); err != nil {
			t.Fatal(err)
		}
	}
	got, err := home.Archived()
	if err != nil || len(got) != 3 {
		t.Fatal(got, err)
	}
	if got[0].Name != "b" || *got[0].Key != "20261002T120000Z" || got[0].LastRun.Outcome != Succeeded ||
		got[1].Name != "a" || got[1].LastRun.Outcome != Missed || got[2].Name != "stray" || got[2].Key != nil {
		t.Fatalf("%+v", got)
	}
}

func TestReconcileRepairsAStaleLastRunFromAFinalClaimOnly(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	ago := func(m int) time.Time { return time.Now().UTC().Add(-time.Duration(m) * time.Minute) }
	last := func(id string, at time.Time) LastRun {
		return LastRun{RunID: id, Trigger: TriggerManual, Outcome: Succeeded, FinishedAt: at}
	}
	// "stale": the run finalized its claim, then died before last.json.
	_ = home.WriteLastRun("stale", last("older", ago(60)))
	done := NewClaim("stale", "k1", TriggerScheduler, Once, time.Now()).Finished(Failed, ptr("boom"))
	finished := ago(5)
	done.FinishedAt = &finished
	// "newer": a later manual run (no claim) already wrote last.json.
	_ = home.WriteLastRun("newer", last("manual", ago(1)))
	old := NewClaim("newer", "k2", TriggerScheduler, Once, time.Now()).Finished(Succeeded, nil)
	oldAt := ago(30)
	old.FinishedAt = &oldAt
	// "busy": its lock is held, so its run will write last.json itself.
	busy := NewClaim("busy", "k3", TriggerScheduler, Once, time.Now()).Finished(Succeeded, nil)
	unfinished := NewClaim("missing", "k4", TriggerScheduler, Once, time.Now())
	lock, _ := home.TryLock("busy")
	defer lock.Release()

	repaired, err := home.ReconcileLastRuns([]Claim{done, old, busy, unfinished})
	if err != nil || len(repaired) != 1 || repaired[0] != "stale" {
		t.Fatal(repaired, err)
	}
	if fixed := home.LastRun("stale"); fixed.RunID != done.RunID || fixed.Outcome != Failed {
		t.Fatalf("%+v", fixed)
	}
	if home.LastRun("newer").RunID != "manual" || home.LastRun("busy") != nil || home.LastRun("missing") != nil {
		t.Fatal("only stale is repaired")
	}
}

func TestMissedCheckpointNeverMovesBackward(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	_ = home.SetMissedThrough("j", utc(2026, 10, 20, 12, 0))
	_ = home.SetMissedThrough("j", utc(2026, 10, 20, 11, 0))
	if got, ok := home.MissedThrough("j"); !ok || !got.Equal(utc(2026, 10, 20, 12, 0)) {
		t.Fatal(got)
	}
}
