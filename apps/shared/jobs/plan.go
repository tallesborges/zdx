package jobs

import (
	"sort"
	"time"
)

// MissedScanCap is how far back one tick looks for recurring occurrences
// that expired unstarted.
const MissedScanCap = 7 * 24 * time.Hour

// ClaimStatus is what the planner knows about an occurrence's claim.
type ClaimStatus int

const (
	NoClaim ClaimStatus = iota
	// ClaimRunning is claimed with no outcome yet.
	ClaimRunning
	// ClaimFinished is claimed and final.
	ClaimFinished
)

// TickInput is the clock of one tick.
type TickInput struct {
	Now time.Time
	// Since: recurring occurrences at or before it are ignored.
	Since         time.Time
	LastCheckedAt time.Time
}

// ActionKind is what the scheduler does for one job in a tick.
type ActionKind int

const (
	// ActStart starts a run for Occurrence.
	ActStart ActionKind = iota
	// ActMissOnce records a once job that passed its grace without starting.
	ActMissOnce
	// ActMissRecurring records one summary for Count recurring occurrences
	// that left their grace unstarted, the latest being Occurrence, then
	// moves the job's missed checkpoint to it.
	ActMissRecurring
	// ActArchive archives a once job whose occurrence is final.
	ActArchive
	// ActRecover marks interrupted a once job's claim that has no outcome
	// while nothing holds its lock.
	ActRecover
)

// Action is one planned step.
type Action struct {
	Kind       ActionKind
	Job        string
	Occurrence Occurrence
	Count      int
}

// JobView is what the planner reads about each job's durable state.
type JobView interface {
	Claim(job, key string) ClaimStatus
	// Busy is true while a run of the job is live (or about to start).
	Busy(job string) bool
	// MissedThrough is the latest recurring occurrence already recorded as
	// missed.
	MissedThrough(job string) (time.Time, bool)
}

// PlanTick plans one tick from the clock and each job's durable state.
// Every action that writes state is checked again under the job's lock.
// Starts come last, oldest occurrence first.
func PlanTick(in TickInput, jobs []Definition, loc *time.Location, view JobView) []Action {
	var starts, others []Action
	for i := range jobs {
		job := &jobs[i]
		grace, ok := job.Grace()
		if !ok {
			continue
		}
		name := job.Name
		switch job.Schedule.Kind {
		case Once:
			occ := OnceOccurrence(job.Schedule.RunAt)
			switch view.Claim(name, occ.Key) {
			case ClaimFinished:
				others = append(others, Action{Kind: ActArchive, Job: name, Occurrence: occ})
			case ClaimRunning:
				if !view.Busy(name) {
					others = append(others, Action{Kind: ActRecover, Job: name, Occurrence: occ})
				}
			default:
				switch {
				case occ.At.After(in.Now):
				case in.Now.Sub(occ.At) <= grace:
					if !view.Busy(name) {
						starts = append(starts, Action{Kind: ActStart, Job: name, Occurrence: occ})
					}
				default:
					others = append(others, Action{Kind: ActMissOnce, Job: name, Occurrence: occ})
				}
			}
		case Recurring:
			// An occurrence is in grace at t while it is at or after
			// t - grace, i.e. strictly after edge(t).
			edge := func(t time.Time) time.Time { return t.Add(-grace - time.Nanosecond) }
			due := RecurringOccurrences(job.Schedule.cron, later(edge(in.Now), in.Since), in.Now, loc)
			if n := len(due); n > 0 && view.Claim(name, due[n-1].Key) == NoClaim && !view.Busy(name) {
				starts = append(starts, Action{Kind: ActStart, Job: name, Occurrence: due[n-1]})
			}

			// LastCheckedAt bounds the scan; the per-job checkpoint keeps a
			// summary that was written, but whose tick was not saved, from
			// counting again.
			after := later(later(edge(in.LastCheckedAt), in.Since), edge(in.Now).Add(-MissedScanCap))
			if through, ok := view.MissedThrough(name); ok {
				after = later(after, through)
			}
			var missed []Occurrence
			for _, occ := range RecurringOccurrences(job.Schedule.cron, after, edge(in.Now), loc) {
				if view.Claim(name, occ.Key) == NoClaim {
					missed = append(missed, occ)
				}
			}
			if n := len(missed); n > 0 {
				others = append(others, Action{Kind: ActMissRecurring, Job: name, Occurrence: missed[n-1], Count: n})
			}
		}
	}
	sort.SliceStable(starts, func(i, j int) bool { return starts[i].Occurrence.At.Before(starts[j].Occurrence.At) })
	return append(others, starts...)
}

// NextDueAfter is the next instant any job becomes due after now.
func NextDueAfter(jobs []Definition, now time.Time, loc *time.Location) (time.Time, bool) {
	var best time.Time
	found := false
	for i := range jobs {
		var at time.Time
		ok := false
		switch jobs[i].Schedule.Kind {
		case Once:
			at = OnceOccurrence(jobs[i].Schedule.RunAt).At
			ok = at.After(now)
		case Recurring:
			at, ok = NextRecurringAfter(jobs[i].Schedule.cron, now, loc)
		}
		if ok && (!found || at.Before(best)) {
			best, found = at, true
		}
	}
	return best, found
}

func later(a, b time.Time) time.Time {
	if b.After(a) {
		return b
	}
	return a
}
