package jobs

import (
	"context"
	"fmt"
	"log/slog"
	"os"
	"os/exec"
	"syscall"
	"time"
)

const (
	// MaxLiveRuns is how many scheduled runs may be live at once.
	MaxLiveRuns = 3
	// ClaimRetention is how long finished recurring claims are kept.
	ClaimRetention = 8 * 24 * time.Hour
	// RescanInterval is how often unfinished claims are rescanned, to catch
	// runs that died in processes this daemon did not start.
	RescanInterval = 10 * time.Minute
)

// ChildExit is one finished run child.
type ChildExit struct {
	Job        string
	Occurrence Occurrence
	// Code is the exit code; -1 when a signal killed it.
	Code int
}

// Spawner starts one run child for an occurrence; done receives its exit.
type Spawner func(job string, occ Occurrence, done chan<- ChildExit) (pid int, err error)

// Scheduler is the jobs daemon's state between ticks. It writes only
// missed, failed-to-start, and interrupted outcomes, each under the job's
// lock; runs write their own.
type Scheduler struct {
	Home     Home
	Location *time.Location
	Log      *slog.Logger
	Spawn    Spawner
	Exits    chan ChildExit

	live      map[string]Occurrence
	attempted map[[2]string]bool
	watch     [][2]string
	state     SchedulerState
	jobs      []Definition
	lastError string
	lastInv   []string
	lastPrune time.Time
	lastScan  time.Time
}

// Start loads (or begins) the scheduler state and recovers runs that died
// with the previous daemon.
func (s *Scheduler) Start(now time.Time) error {
	s.live = map[string]Occurrence{}
	s.attempted = map[[2]string]bool{}
	if s.Exits == nil {
		s.Exits = make(chan ChildExit, 16)
	}
	state, ok := s.Home.SchedulerState()
	if !ok {
		state = SchedulerState{Since: now.UTC(), LastCheckedAt: now.UTC()}
	}
	s.state = state
	if err := s.Home.SaveSchedulerState(s.state); err != nil {
		return fmt.Errorf("write scheduler state: %w", err)
	}
	cat, _ := ReadCatalog(s.Home)
	s.rescanUnfinished(&cat)
	return nil
}

// State is the scheduler state as last saved.
func (s *Scheduler) State() SchedulerState { return s.state }

// Loop ticks until ctx ends: at the next due instant or after poll, and on
// every child exit.
func (s *Scheduler) Loop(ctx context.Context, poll time.Duration) {
	for {
		s.Tick(time.Now())
		timer := time.NewTimer(s.wait(poll))
		select {
		case <-ctx.Done():
			timer.Stop()
			return
		case <-timer.C:
		case exit := <-s.Exits:
			timer.Stop()
			s.HandleExit(exit)
			for drained := false; !drained; {
				select {
				case exit := <-s.Exits:
					s.HandleExit(exit)
				default:
					drained = true
				}
			}
		}
	}
}

func (s *Scheduler) wait(poll time.Duration) time.Duration {
	now := time.Now()
	next, ok := NextDueAfter(s.jobs, now, s.Location)
	if !ok {
		return poll
	}
	until := next.Sub(now) + 200*time.Millisecond
	if until < 0 || until > poll {
		return poll
	}
	return until
}

// rescanUnfinished marks dead unfinished claims interrupted and watches the
// ones another process still runs. It also repairs `last.json` from final
// claims, for runs that died between the two writes.
func (s *Scheduler) rescanUnfinished(cat *Catalog) {
	claims, err := s.Home.AllClaims(s.Log)
	if err != nil {
		s.Log.Error("Failed to read job claims", "err", err)
		return
	}
	repaired, err := s.Home.ReconcileLastRuns(claims)
	if err != nil {
		s.Log.Warn("Failed to repair last runs", "err", err)
	}
	if len(repaired) > 0 {
		s.Log.Warn("Repaired stale last-run records from claims", "jobs", repaired)
	}
	for _, c := range claims {
		if c.Outcome != nil {
			continue
		}
		id := [2]string{c.Job, c.Key}
		if occ, ok := s.live[c.Job]; ok && occ.Key == c.Key {
			continue
		}
		if s.watching(id) {
			continue
		}
		if !s.markInterrupted(cat, c.Job, c.Key) {
			s.watch = append(s.watch, id)
		}
	}
}

func (s *Scheduler) watching(id [2]string) bool {
	for _, w := range s.watch {
		if w == id {
			return true
		}
	}
	return false
}

// Tick plans and carries out one tick.
func (s *Scheduler) Tick(now time.Time) {
	now = now.UTC()
	cat, err := ReadCatalog(s.Home)
	if err != nil {
		if msg := err.Error(); msg != s.lastError {
			s.Log.Error("Failed to read jobs", "err", msg)
			s.lastError = msg
		}
		return
	}
	s.lastError = ""
	s.logInvalid(&cat)
	s.checkWatch(&cat)
	if s.lastScan.IsZero() || now.Sub(s.lastScan) > RescanInterval {
		s.lastScan = now
		s.rescanUnfinished(&cat)
	}

	in := TickInput{Now: now, Since: s.state.Since, LastCheckedAt: s.state.LastCheckedAt}
	complete := true
	for _, a := range PlanTick(in, cat.Jobs, s.Location, planView{s}) {
		if !s.apply(&cat, a) {
			complete = false
		}
	}
	// A missed summary that could not be recorded keeps the tick from
	// advancing, so the next tick finds it again.
	if complete {
		s.state.LastCheckedAt = later(s.state.LastCheckedAt, now)
	}
	prune := s.lastPrune.IsZero() || now.Sub(s.lastPrune) > 24*time.Hour
	cutoff := now.Add(-ClaimRetention)
	if prune {
		// Raise the floor first, so no occurrence whose claim is about to be
		// deleted can be scheduled again.
		s.state.Since = later(s.state.Since, cutoff)
	}
	if err := s.Home.SaveSchedulerState(s.state); err != nil {
		s.Log.Error("Failed to save scheduler state", "err", err)
	} else if prune {
		s.lastPrune = now
		if _, err := s.Home.PruneRecurringClaims(cutoff, s.Log); err != nil {
			s.Log.Warn("Failed to prune job claims", "err", err)
		}
	}
	s.jobs = cat.Jobs
}

func (s *Scheduler) logInvalid(cat *Catalog) {
	current := make([]string, 0, len(cat.Invalid))
	for _, inv := range cat.Invalid {
		current = append(current, inv.Path+": "+inv.Error)
	}
	same := len(current) == len(s.lastInv)
	for i := 0; same && i < len(current); i++ {
		same = current[i] == s.lastInv[i]
	}
	if !same {
		for _, line := range current {
			s.Log.Error("Invalid job file; it will not run", "job", line)
		}
		s.lastInv = current
	}
}

func (s *Scheduler) checkWatch(cat *Catalog) {
	watch := s.watch
	s.watch = nil
	for _, id := range watch {
		c, _ := s.Home.ReadClaim(id[0], id[1])
		if c != nil && c.Outcome == nil && !s.markInterrupted(cat, id[0], id[1]) {
			s.watch = append(s.watch, id)
		}
	}
}

func find(cat *Catalog, name string) *Definition {
	for i := range cat.Jobs {
		if cat.Jobs[i].Name == name {
			return &cat.Jobs[i]
		}
	}
	return nil
}

// apply carries out one action. It returns false only when a missed summary
// could not be recorded and must be planned again.
func (s *Scheduler) apply(cat *Catalog, a Action) bool {
	job := find(cat, a.Job)
	switch a.Kind {
	case ActStart:
		if job != nil {
			s.start(job, a.Occurrence)
		}
	case ActMissOnce:
		if job != nil {
			grace, _ := job.EffectiveGraceSecs()
			s.Log.Warn("Once job missed", "name", a.Job, "run_at", a.Occurrence.At)
			s.recordNotRun(job, a.Occurrence, Missed, fmt.Sprintf(
				"did not start within its grace (%ds after run_at): the daemon was down, busy, or the job was already running", grace))
		}
	case ActMissRecurring:
		if job != nil {
			return s.recordRecurringMissed(job, a.Occurrence, a.Count)
		}
	case ActArchive:
		if job != nil {
			s.archiveIfFinal(job, a.Occurrence)
		}
	case ActRecover:
		s.markInterrupted(cat, a.Job, a.Occurrence.Key)
	}
	return true
}

func (s *Scheduler) start(job *Definition, occ Occurrence) {
	id := [2]string{job.Name, occ.Key}
	if len(s.live) >= MaxLiveRuns || s.attempted[id] {
		return
	}
	pid, err := s.Spawn(job.Name, occ, s.Exits)
	if err != nil {
		s.Log.Error("Failed to start job run", "name", job.Name, "err", err)
		// Unrecorded (job busy): let a later tick try again.
		if s.recordNotRun(job, occ, Failed, fmt.Sprintf("could not start the run process: %v", err)) {
			s.attempted[id] = true
		}
		return
	}
	s.Log.Info("Starting job", "name", job.Name, "occurrence", occ.Key, "pid", pid)
	s.live[job.Name] = occ
	s.attempted[id] = true
}

// HandleExit records what a run child left behind.
func (s *Scheduler) HandleExit(exit ChildExit) {
	delete(s.live, exit.Job)
	key := exit.Occurrence.Key
	id := [2]string{exit.Job, key}
	if exit.Code == ExitBusy {
		// Another run held the lock; plan the occurrence again.
		delete(s.attempted, id)
		return
	}
	cat, _ := ReadCatalog(s.Home)
	claim, err := s.Home.ReadClaim(exit.Job, key)
	switch {
	case err != nil:
		s.Log.Error("Failed to read job claim", "err", err)
	case claim != nil && claim.Outcome == nil:
		if !s.markInterrupted(&cat, exit.Job, key) {
			s.watch = append(s.watch, id)
		}
	case claim != nil:
	case exit.Code == 0:
		// The run skipped itself (the job changed since it was planned);
		// plan the occurrence again if it is still due.
		delete(s.attempted, id)
	default:
		job := find(&cat, exit.Job)
		recorded := job != nil && s.recordNotRun(job, exit.Occurrence, Failed, fmt.Sprintf(
			"the run process exited (%s) before taking its occurrence; see the daemon log", exitLabel(exit.Code)))
		if !recorded {
			delete(s.attempted, id)
		}
	}
}

func exitLabel(code int) string {
	if code < 0 {
		return "killed by a signal"
	}
	return fmt.Sprintf("status %d", code)
}

// recordRecurringMissed records one summary under the job's lock: append
// the record, then move the missed checkpoint. False when nothing was
// recorded (lock busy or the append failed).
func (s *Scheduler) recordRecurringMissed(job *Definition, latest Occurrence, count int) bool {
	lock := s.lock(job.Name)
	if lock == nil {
		return false
	}
	defer lock.Release()
	s.Log.Warn("Recurring job missed", "name", job.Name, "count", count, "latest", latest.At)
	runID := NewRunID()
	text := fmt.Sprintf("%d occurrence(s) did not start within grace; latest scheduled for %s", count, offsetRFC3339(latest.At.UTC()))
	if err := s.Home.AppendRunRecord(NotRunRecord(job.Name, runID, TriggerDaemon, job.Schedule, latest.At, Missed, text)); err != nil {
		s.Log.Error("Failed to record missed occurrences", "err", err)
		return false
	}
	if err := s.Home.SetMissedThrough(job.Name, latest.At); err != nil {
		s.Log.Error("Failed to save the missed checkpoint", "err", err)
	}
	s.writeLast(job.Name, runID, &latest.At, Missed, text)
	return true
}

// recordNotRun records an occurrence that never ran under the job's lock,
// then archives a once job. False when nothing was recorded.
func (s *Scheduler) recordNotRun(job *Definition, occ Occurrence, outcome Outcome, text string) bool {
	lock := s.lock(job.Name)
	if lock == nil {
		return false
	}
	defer lock.Release()
	claim := NewClaim(job.Name, occ.Key, TriggerDaemon, job.Schedule.Kind, occ.At).Finished(outcome, &text)
	created, err := s.Home.CreateClaim(claim)
	if err != nil {
		s.Log.Error("Failed to write job claim", "err", err)
		return false
	}
	if !created {
		return true
	}
	s.append(NotRunRecord(job.Name, claim.RunID, TriggerDaemon, job.Schedule, occ.At, outcome, text))
	s.writeLast(job.Name, claim.RunID, &occ.At, outcome, text)
	s.archive(job, occ.Key)
	return true
}

// markInterrupted marks an unfinished claim interrupted when no run holds
// the job's lock. False while a run still holds it.
func (s *Scheduler) markInterrupted(cat *Catalog, job, key string) bool {
	lock := s.lock(job)
	if lock == nil {
		return false
	}
	defer lock.Release()
	claim, err := s.Home.ReadClaim(job, key)
	if err != nil || claim == nil || claim.Outcome != nil {
		return true
	}
	text := "the run stopped without a result (its process exited or was killed)"
	s.Log.Warn("Job run interrupted", "name", job, "occurrence", key)
	if err := s.Home.UpdateClaim(claim.Finished(Interrupted, &text)); err != nil {
		s.Log.Error("Failed to update job claim", "err", err)
		return false
	}
	def := find(cat, job)
	schedule := Schedule{Kind: Manual}
	if def != nil {
		schedule = def.Schedule
	}
	rec := NotRunRecord(job, claim.RunID, claim.Trigger, schedule, claim.ScheduledFor, Interrupted, text)
	kind := claim.ScheduleKind
	rec.ScheduleKind = &kind
	if def == nil {
		rec.Schedule = nil
	}
	rec.ThreadID = claim.ThreadID
	s.append(rec)
	at := claim.ScheduledFor
	s.writeLast(job, claim.RunID, &at, Interrupted, text)
	if def != nil {
		s.archive(def, key)
	}
	return true
}

func (s *Scheduler) archiveIfFinal(job *Definition, occ Occurrence) {
	lock := s.lock(job.Name)
	if lock == nil {
		return
	}
	defer lock.Release()
	if c, _ := s.Home.ReadClaim(job.Name, occ.Key); c != nil && c.Outcome != nil {
		s.archive(job, occ.Key)
	}
}

// archive archives a once job whose current occurrence is key. A job
// rescheduled since that occurrence keeps its file.
func (s *Scheduler) archive(job *Definition, key string) {
	if job.Schedule.Kind != Once || OnceOccurrence(job.Schedule.RunAt).Key != key {
		return
	}
	dest, err := s.Home.ArchiveOnceJob(job.Path, job.Schedule.RunAt)
	switch {
	case err != nil:
		s.Log.Error("Failed to archive job", "err", err)
	case dest != "":
		s.Log.Info("Archived once job", "name", job.Name, "dest", dest)
	}
}

func (s *Scheduler) lock(job string) *Lock {
	lock, err := s.Home.TryLock(job)
	if err != nil {
		s.Log.Error("Failed to lock job", "err", err, "name", job)
		return nil
	}
	return lock
}

func (s *Scheduler) append(r RunRecord) {
	if err := s.Home.AppendRunRecord(r); err != nil {
		s.Log.Warn("Failed to append job run log", "err", err)
	}
}

func (s *Scheduler) writeLast(job, runID string, scheduledFor *time.Time, outcome Outcome, text string) {
	var at *time.Time
	if scheduledFor != nil {
		utc := scheduledFor.UTC()
		at = &utc
	}
	last := LastRun{RunID: runID, Trigger: TriggerDaemon, Outcome: outcome, ScheduledFor: at,
		FinishedAt: time.Now().UTC(), Error: &text}
	if err := s.Home.WriteLastRun(job, last); err != nil {
		s.Log.Warn("Failed to record the job's last run", "err", err)
	}
}

// planView is the scheduler's live state as the planner sees it.
type planView struct{ s *Scheduler }

func (v planView) Claim(job, key string) ClaimStatus {
	c, _ := v.s.Home.ReadClaim(job, key)
	switch {
	case c == nil:
		return NoClaim
	case c.Outcome != nil:
		return ClaimFinished
	}
	return ClaimRunning
}

func (v planView) Busy(job string) bool {
	_, live := v.s.live[job]
	return live || v.s.Home.IsRunning(job)
}

func (v planView) MissedThrough(job string) (time.Time, bool) { return v.s.Home.MissedThrough(job) }

// ProcessSpawner starts each run as `<exe> --root ROOT [--system-prompt S]
// [--no-thread] run NAME --occurrence KEY --scheduled-for INSTANT` in this
// process's working directory, with stdin closed and stdout and stderr
// inherited.
func ProcessSpawner(exe string, globalArgs []string) Spawner {
	return func(job string, occ Occurrence, done chan<- ChildExit) (int, error) {
		args := append(append([]string{}, globalArgs...), "run", job,
			"--occurrence", occ.Key, "--scheduled-for", offsetRFC3339(occ.At.UTC()))
		cmd := exec.Command(exe, args...)
		cmd.Stdout, cmd.Stderr = os.Stdout, os.Stderr
		if err := cmd.Start(); err != nil {
			return 0, err
		}
		go func() {
			code := 0
			if err := cmd.Wait(); err != nil {
				code = -1
				if exitErr, ok := err.(*exec.ExitError); ok {
					if ws, ok := exitErr.Sys().(syscall.WaitStatus); !ok || !ws.Signaled() {
						code = exitErr.ExitCode()
					}
				}
			}
			done <- ChildExit{Job: job, Occurrence: occ, Code: code}
		}()
		return cmd.Process.Pid, nil
	}
}
