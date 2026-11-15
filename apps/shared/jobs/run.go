package jobs

import (
	"errors"
	"fmt"
	"io"
	"log/slog"
	"math"
	"os"
	"time"
)

// ExitBusy is the exit code of a run refused because the job already has a
// live run; the scheduler plans the occurrence again.
const ExitBusy = 75

// ErrBusy is a run refused because another run holds the job's lock.
type ErrBusy struct{ Job string }

func (e ErrBusy) Error() string { return fmt.Sprintf("job '%s' is already running", e.Job) }

// ErrInterrupted is a run that stopped on a signal.
var ErrInterrupted = errors.New("interrupted")

// RunRequest is how `zdx jobs run` was asked to run.
type RunRequest struct {
	Name string
	// Keep runs a once job as a test, without consuming its occurrence.
	Keep bool
	// Scheduled is set when the scheduler started the run.
	Scheduled *Occurrence
	// ThreadID is an explicit `--thread`; NoThread is `--no-thread`.
	ThreadID *string
	NoThread bool
}

// Runner carries out one run: lock, claim, attempts, records, final outcome,
// archive.
type Runner struct {
	Home   Home
	Engine Engine
	// Out receives the user-facing lines (`Archived ...`, `Skipped ...`).
	Out io.Writer
	Log *slog.Logger
	// Interrupted is closed on the first SIGINT or SIGTERM: a retry wait
	// then ends the run as interrupted. Signals are sent on to a live
	// attempt.
	Interrupted <-chan struct{}
	Signals     <-chan os.Signal
	// RetryDelay is the wait before attempt n+1 (nil: one minute per failed
	// attempt).
	RetryDelay func(failedAttempt uint32) time.Duration
	Now        func() time.Time
}

type finishedRun struct {
	outcome  Outcome
	err      error
	threadID *string
}

// Run runs one job by name. It returns ErrBusy when the job is locked,
// ErrInterrupted after an interrupt, and an error for a failed run.
func (r *Runner) Run(req RunRequest) error {
	job, err := LoadByName(r.Home, req.Name)
	if err != nil {
		return err
	}
	trigger := TriggerManual
	if req.Scheduled != nil {
		trigger = TriggerDaemon
	}
	occ, skip := r.occurrenceToTake(job, req)
	if skip {
		return nil
	}
	lock, err := r.Home.TryLock(job.Name)
	if err != nil {
		return err
	}
	if lock == nil {
		return ErrBusy{Job: job.Name}
	}
	defer lock.Release()

	var claim *Claim
	if occ != nil {
		c := NewClaim(job.Name, occ.Key, trigger, job.Schedule.Kind, occ.At)
		created, err := r.Home.CreateClaim(c)
		if err != nil {
			return err
		}
		if !created {
			return r.alreadyTaken(job, *occ, req.Scheduled != nil)
		}
		claim = &c
	}
	runID := NewRunID()
	if claim != nil {
		runID = claim.RunID
	}
	finished := r.attempts(job, req, trigger, runID, occ, lock)
	if err := r.recordOutcome(job, claim, trigger, runID, finished); err != nil {
		return err
	}
	return finished.err
}

// occurrenceToTake is the occurrence a run claims (nil: none), or skip for a
// scheduled run whose job changed since it was planned.
func (r *Runner) occurrenceToTake(job *Definition, req RunRequest) (*Occurrence, bool) {
	switch {
	case req.Scheduled != nil && job.Schedule.Kind == Once:
		occ := OnceOccurrence(job.Schedule.RunAt)
		if occ.Key != req.Scheduled.Key {
			fmt.Fprintf(r.Out, "Skipped '%s': run_at changed since it was planned.\n", job.Name)
			return nil, true
		}
		return &occ, false
	case req.Scheduled != nil && job.Schedule.Kind == Recurring:
		occ := *req.Scheduled
		return &occ, false
	case req.Scheduled != nil:
		fmt.Fprintf(r.Out, "Skipped '%s': it no longer has a schedule.\n", job.Name)
		return nil, true
	case job.Schedule.Kind == Once && !req.Keep:
		occ := OnceOccurrence(job.Schedule.RunAt)
		return &occ, false
	}
	return nil, false
}

// alreadyTaken: a scheduled run skips an occurrence someone else took; a
// manual run explains why the once job cannot run again.
func (r *Runner) alreadyTaken(job *Definition, occ Occurrence, scheduled bool) error {
	claim, err := r.Home.ReadClaim(job.Name, occ.Key)
	if err != nil {
		return err
	}
	state := "in progress"
	if claim != nil && claim.Outcome != nil {
		state = string(*claim.Outcome)
	}
	if scheduled {
		fmt.Fprintf(r.Out, "Skipped '%s': occurrence %s was already taken (%s).\n", job.Name, occ.Key, state)
		return nil
	}
	return fmt.Errorf("job '%s' already ran for run_at %s (%s); change run_at to schedule it again, or pass --keep for a test run",
		job.Name, offsetRFC3339(occ.At), state)
}

func (r *Runner) now() time.Time {
	if r.Now != nil {
		return r.Now().UTC()
	}
	return time.Now().UTC()
}

// attempts runs up to max_retries+1 attempts, appending one record each.
// A failed attempt that was prepared is retried after RetryDelay; an
// unprepared or interrupted one ends the run.
func (r *Runner) attempts(job *Definition, req RunRequest, trigger Trigger, runID string, occ *Occurrence, lock *Lock) finishedRun {
	attempts := job.MaxRetries
	if attempts < math.MaxUint32 {
		attempts++
	}
	threadID := req.ThreadID
	if !req.NoThread && threadID == nil {
		threadID = ptr(fmt.Sprintf("job-%s-%s", job.Name, r.now().Format("20060102-150405")))
	}
	if req.NoThread {
		threadID = nil
	}
	input := AttemptInput{Job: job.Name, Prompt: job.Prompt, Model: job.Model, Subagent: job.Subagent,
		Root: job.Root, TimeoutSecs: job.TimeoutSecs, RunID: runID, ThreadID: threadID, NoThread: req.NoThread}

	for attempt := uint32(1); ; attempt++ {
		started, clock := r.now(), time.Now()
		result := r.Engine.Attempt(input, lock.File(), r.Signals)
		finishedAt, elapsed := r.now(), time.Since(clock)
		retry := result.Outcome == Failed && result.Prepared && attempt < attempts
		outcome := result.Outcome
		if retry {
			outcome = Retrying
		}
		kind := job.Schedule.Kind
		desc := job.Schedule.Describe()
		rec := RunRecord{Job: job.Name, RunID: &runID, Trigger: trigger, ScheduleKind: &kind,
			ThreadID: threadID, Attempt: attempt, MaxAttempts: attempts,
			StartedAt: offsetRFC3339(started), FinishedAt: offsetRFC3339(finishedAt),
			DurationMS: uint64(elapsed.Milliseconds()), OK: result.Outcome == Succeeded,
			Outcome: &outcome, Error: result.Error, Schedule: &desc, Model: result.Model,
			Subagent: job.Subagent, Root: result.Root}
		if occ != nil {
			rec.ScheduledFor = ptr(offsetRFC3339(occ.At))
			rec.LatenessSecs = ptr(int64(started.Sub(occ.At) / time.Second))
		}
		if err := r.Home.AppendRunRecord(rec); err != nil {
			r.Log.Warn("Failed to append job run log", "err", err)
		}

		switch {
		case result.Outcome == Succeeded:
			return finishedRun{outcome: Succeeded, threadID: threadID}
		case result.Outcome == Interrupted:
			return finishedRun{outcome: Interrupted, err: r.failure(job, attempt, result.Error, ErrInterrupted), threadID: threadID}
		case !retry:
			return finishedRun{outcome: Failed, err: r.failure(job, attempt, result.Error, nil), threadID: threadID}
		}
		delay := time.Duration(attempt) * time.Minute
		if r.RetryDelay != nil {
			delay = r.RetryDelay(attempt)
		}
		r.Log.Warn("Job failed, retrying", "name", job.Name, "attempt", attempt, "attempts", attempts,
			"delay_secs", int(delay.Seconds()), "err", deref(result.Error))
		select {
		case <-time.After(delay):
		case <-r.Interrupted:
			// The run's last record must carry its final outcome, which the
			// history reads; attempt 0 adds no attempt.
			text := "interrupted while waiting to retry"
			now := offsetRFC3339(r.now())
			final := rec
			final.Attempt, final.StartedAt, final.FinishedAt, final.DurationMS = 0, now, now, 0
			final.OK, final.Outcome, final.Error, final.Model, final.Root = false, ptr(Interrupted), &text, nil, nil
			final.LatenessSecs = nil
			if err := r.Home.AppendRunRecord(final); err != nil {
				r.Log.Warn("Failed to append job run log", "err", err)
			}
			return finishedRun{outcome: Interrupted, err: r.failure(job, attempt, &text, ErrInterrupted), threadID: threadID}
		}
	}
}

// runError is a failed or interrupted run: its text is the run's error, and
// it wraps ErrInterrupted for an interrupt.
type runError struct {
	text string
	kind error
}

func (e runError) Error() string { return e.text }
func (e runError) Unwrap() error { return e.kind }

func (r *Runner) failure(job *Definition, attempt uint32, cause *string, kind error) error {
	text := fmt.Sprintf("job '%s' failed after %d attempt(s)", job.Name, attempt)
	if cause != nil {
		text += ": " + *cause
	}
	return runError{text: text, kind: kind}
}

// recordOutcome writes the final claim and `last.json`, then archives a once
// job whose occurrence this run consumed.
func (r *Runner) recordOutcome(job *Definition, claim *Claim, trigger Trigger, runID string, f finishedRun) error {
	var errText *string
	if f.err != nil {
		errText = ptr(summarizeError(f.err.Error()))
	}
	var scheduledFor *time.Time
	if claim != nil {
		at := claim.ScheduledFor
		scheduledFor = &at
		final := claim.Finished(f.outcome, errText)
		final.ThreadID = f.threadID
		if err := r.Home.UpdateClaim(final); err != nil {
			return err
		}
	}
	last := LastRun{RunID: runID, Trigger: trigger, Outcome: f.outcome, ScheduledFor: scheduledFor,
		FinishedAt: r.now(), Error: errText, ThreadID: f.threadID}
	if err := r.Home.WriteLastRun(job.Name, last); err != nil {
		r.Log.Warn("Failed to record the job's last run", "err", err)
	}
	if claim != nil && job.Schedule.Kind == Once {
		dest, err := r.Home.ArchiveOnceJob(job.Path, job.Schedule.RunAt)
		switch {
		case err != nil:
			r.Log.Warn("Failed to archive once job", "err", err)
		case dest != "":
			fmt.Fprintf(r.Out, "Archived '%s' to %s\n", job.Name, dest)
		}
	}
	return nil
}

func deref(s *string) string {
	if s == nil {
		return ""
	}
	return *s
}
