package jobs

import (
	"bytes"
	"errors"
	"fmt"
	"log/slog"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"
)

// inProcess spawns each scheduled run as a goroutine running the Runner, and
// reports its exit code as the run child would.
func inProcess(t *testing.T, home Home, bin string) (Spawner, *sync.WaitGroup) {
	var wg sync.WaitGroup
	return func(job string, occ Occurrence, done chan<- ChildExit) (int, error) {
		wg.Add(1)
		go func() {
			defer wg.Done()
			err := newRunner(t, home, bin).Run(RunRequest{Name: job, Scheduled: &occ, NoThread: true})
			code := 0
			var busy ErrBusy
			switch {
			case errors.As(err, &busy):
				code = ExitBusy
			case err != nil:
				code = 1
			}
			done <- ChildExit{Job: job, Occurrence: occ, Code: code}
		}()
		return 1, nil
	}, &wg
}

type logBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (l *logBuffer) Write(p []byte) (int, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.buf.Write(p)
}

func (l *logBuffer) String() string {
	l.mu.Lock()
	defer l.mu.Unlock()
	return l.buf.String()
}

func newScheduler(t *testing.T, home Home, spawn Spawner, loc *time.Location) (*Scheduler, *logBuffer) {
	t.Helper()
	logs := &logBuffer{}
	return &Scheduler{Home: home, Location: loc, Log: slog.New(slog.NewTextHandler(logs, nil)), Spawn: spawn}, logs
}

func unfinishedClaim(t *testing.T, home Home, job string, at time.Time) string {
	t.Helper()
	key := at.UTC().Format("20060102T150405Z")
	c := Claim{Job: job, Key: key, RunID: "run-from-a-dead-daemon", Trigger: TriggerDaemon, ScheduleKind: Once,
		ScheduledFor: at.UTC(), ClaimedAt: at.UTC(), PID: 1}
	if _, err := home.CreateClaim(c); err != nil {
		t.Fatal(err)
	}
	return key
}

func outcomesOf(t *testing.T, home Home, job string) string {
	var out []string
	for _, r := range records(t, home) {
		if r.Job == job {
			out = append(out, string(*r.Outcome))
		}
	}
	return strings.Join(out, ",")
}

func TestDaemonRunsMissesAndRecoversOnceJobs(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, _ := fakeZdx(t)
	now := time.Now().UTC().Truncate(time.Second)
	rfc := func(t time.Time) string { return t.Format(time.RFC3339) }
	due := now.Add(-time.Minute)
	onceJob(t, home, "remind", rfc(due), "Say hello.")
	late := now.Add(-7 * time.Hour)
	onceJob(t, home, "late", rfc(late), "Too late.")
	crashedAt := now.Add(-5 * time.Minute)
	onceJob(t, home, "crashed", rfc(crashedAt), "Crashed.")
	crashedKey := unfinishedClaim(t, home, "crashed", crashedAt)
	// Rescheduled after its run died: the old occurrence is recorded, the
	// file stays for its new run_at.
	onceJob(t, home, "moved", rfc(now.Add(time.Hour)), "Moved.")
	unfinishedClaim(t, home, "moved", crashedAt)
	writeFile(t, filepath.Join(home.JobsDir(), "broken.md"), "---\nschedule: \"not cron\"\n---\nx\n")

	spawn, wg := inProcess(t, home, bin)
	s, logs := newScheduler(t, home, spawn, time.Local)
	if err := s.Start(now); err != nil {
		t.Fatal(err)
	}
	s.Tick(now)
	wg.Wait()
	s.HandleExit(<-s.Exits)
	s.Tick(now.Add(time.Second))

	dueKey := due.Format("20060102T150405Z")
	if c, _ := home.ReadClaim("remind", dueKey); c == nil || *c.Outcome != Succeeded || c.Trigger != TriggerDaemon {
		t.Fatalf("%+v\n%s", c, logs)
	}
	for _, f := range []string{"remind--" + dueKey, "crashed--" + crashedKey, "late--" + late.Format("20060102T150405Z")} {
		if !exists(filepath.Join(home.ArchiveDir(), f+".md")) {
			t.Errorf("%s not archived", f)
		}
	}
	if c, _ := home.ReadClaim("crashed", crashedKey); *c.Outcome != Interrupted || c.RunID != "run-from-a-dead-daemon" {
		t.Fatalf("%+v", c)
	}
	if c, _ := home.ReadClaim("moved", crashedKey); *c.Outcome != Interrupted || !exists(filepath.Join(home.JobsDir(), "moved.md")) {
		t.Fatal("moved: recorded, file stays")
	}
	if c, _ := home.ReadClaim("late", late.Format("20060102T150405Z")); *c.Outcome != Missed {
		t.Fatalf("%+v", c)
	}
	for job, want := range map[string]string{"remind": "succeeded", "crashed": "interrupted", "moved": "interrupted", "late": "missed"} {
		if got := outcomesOf(t, home, job); got != want {
			t.Errorf("%s: %s", job, got)
		}
	}
	if !exists(filepath.Join(home.JobsDir(), "broken.md")) || !strings.Contains(logs.String(), "Invalid job file") {
		t.Fatal(logs)
	}
	if strings.Count(logs.String(), "Invalid job file") != 1 {
		t.Fatal("an unchanged invalid file is logged once")
	}
}

func TestDaemonRecordsARecurringMissOnceAcrossRestarts(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	loc := time.FixedZone("", -3*3600)
	now := time.Date(2026, 10, 20, 15, 0, 0, 0, time.UTC)
	missedAt := now.Add(-30 * time.Minute).In(loc)
	writeFile(t, filepath.Join(home.JobsDir(), "daily.md"),
		fmt.Sprintf("---\nschedule: \"%d %d * * *\"\n---\nReport.\n", missedAt.Minute(), missedAt.Hour()))
	downSince := now.Add(-2 * time.Hour)
	seed := SchedulerState{Since: downSince, LastCheckedAt: downSince}
	if err := home.SaveSchedulerState(seed); err != nil {
		t.Fatal(err)
	}
	spawn := func(string, Occurrence, chan<- ChildExit) (int, error) { t.Fatal("nothing is due"); return 0, nil }

	s, _ := newScheduler(t, home, spawn, loc)
	if err := s.Start(now); err != nil {
		t.Fatal(err)
	}
	s.Tick(now)
	all := records(t, home)
	if len(all) != 1 || *all[0].Outcome != Missed || !strings.HasPrefix(*all[0].Error, "1 occurrence(s)") || all[0].Attempt != 0 {
		t.Fatalf("%+v", all)
	}
	if through, ok := home.MissedThrough("daily"); !ok || !through.Equal(missedAt) {
		t.Fatal(through)
	}
	if last := home.LastRun("daily"); last.Outcome != Missed {
		t.Fatalf("%+v", last)
	}

	// A crash before the tick was saved: the next start sees the old state.
	if err := home.SaveSchedulerState(seed); err != nil {
		t.Fatal(err)
	}
	s, _ = newScheduler(t, home, spawn, loc)
	if err := s.Start(now.Add(time.Minute)); err != nil {
		t.Fatal(err)
	}
	s.Tick(now.Add(time.Minute))
	if got := len(records(t, home)); got != 1 {
		t.Fatal("the summary was repeated:", got)
	}
	if st, _ := home.SchedulerState(); !st.LastCheckedAt.Equal(now.Add(time.Minute)) {
		t.Fatalf("%+v", st)
	}
}

func TestABusyMissedSummaryHoldsTheTickBack(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	loc := time.UTC
	now := time.Date(2026, 10, 20, 15, 0, 0, 0, time.UTC)
	writeFile(t, filepath.Join(home.JobsDir(), "daily.md"), "---\nschedule: \"30 14 * * *\"\n---\nReport.\n")
	downSince := now.Add(-2 * time.Hour)
	_ = home.SaveSchedulerState(SchedulerState{Since: downSince, LastCheckedAt: downSince})
	lock, _ := home.TryLock("daily")
	s, _ := newScheduler(t, home, nil, loc)
	_ = s.Start(now)
	s.Tick(now)
	if st, _ := home.SchedulerState(); !st.LastCheckedAt.Equal(downSince) || len(records(t, home)) != 0 {
		t.Fatalf("%+v", st)
	}
	lock.Release()
	s.Tick(now.Add(time.Second))
	if got := len(records(t, home)); got != 1 {
		t.Fatal(got)
	}
}

func TestAChildThatExitsWithoutAClaimIsRecordedFailedOrPlannedAgain(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	onceJob(t, home, "remind", "2030-01-02T09:00:00Z", "x")
	job, _ := LoadByName(home, "remind")
	occ := OnceOccurrence(job.Schedule.RunAt)
	s, _ := newScheduler(t, home, nil, time.UTC)
	_ = s.Start(time.Now())

	s.attempted[[2]string{"remind", occ.Key}] = true
	s.HandleExit(ChildExit{Job: "remind", Occurrence: occ, Code: ExitBusy})
	s.HandleExit(ChildExit{Job: "remind", Occurrence: occ, Code: 0})
	if s.attempted[[2]string{"remind", occ.Key}] || len(records(t, home)) != 0 {
		t.Fatal("busy and skipped runs are planned again")
	}
	s.HandleExit(ChildExit{Job: "remind", Occurrence: occ, Code: -1})
	c, _ := home.ReadClaim("remind", occ.Key)
	if c == nil || *c.Outcome != Failed || !strings.Contains(*c.Error, "killed by a signal") {
		t.Fatalf("%+v", c)
	}
	if !exists(filepath.Join(home.ArchiveDir(), "remind--"+occ.Key+".md")) {
		t.Fatal("archived")
	}
}

func TestAtMostThreeRunsAreLive(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	now := time.Date(2026, 10, 20, 15, 0, 30, 0, time.UTC)
	for i := 0; i < 5; i++ {
		writeFile(t, filepath.Join(home.JobsDir(), fmt.Sprintf("j%d.md", i)), "---\nschedule: \"0 15 * * *\"\n---\nx\n")
	}
	var started []string
	spawn := func(job string, _ Occurrence, _ chan<- ChildExit) (int, error) {
		started = append(started, job)
		return 1, nil
	}
	_ = home.SaveSchedulerState(SchedulerState{Since: now.Add(-time.Hour), LastCheckedAt: now.Add(-time.Minute)})
	s, _ := newScheduler(t, home, spawn, time.UTC)
	_ = s.Start(now)
	s.Tick(now)
	s.Tick(now.Add(time.Second))
	if len(started) != MaxLiveRuns {
		t.Fatal(started)
	}
}

func exists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func TestTheDaemonFilesRefuseASecondDaemonAndAreRemovedOnRelease(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	root := t.TempDir()
	files, err := AcquireDaemon(home, root, "/config/dir")
	if err != nil {
		t.Fatal(err)
	}
	var rec DaemonRecord
	if ok, err := readJSON(filepath.Join(home.Dir, "run", "daemon.context.json"), &rec); !ok || err != nil ||
		rec.PID != os.Getpid() || rec.ConfigDir != "/config/dir" || rec.Root == "" {
		t.Fatalf("%+v %v", rec, err)
	}
	if pid, ok := readPID(filepath.Join(home.Dir, "run", "daemon.pid")); !ok || pid != os.Getpid() {
		t.Fatal(pid)
	}
	if _, err := AcquireDaemon(home, root, "/config/dir"); err == nil || !strings.Contains(err.Error(), "already running") {
		t.Fatal(err)
	}
	files.Release()
	for _, name := range []string{"daemon.pid", "daemon.context.json"} {
		if exists(filepath.Join(home.Dir, "run", name)) {
			t.Fatalf("%s left behind", name)
		}
	}
}
