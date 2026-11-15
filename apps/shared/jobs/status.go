package jobs

import (
	"sort"
	"time"
)

// Status is where a job stands now.
type Status string

const (
	StatusInvalid     Status = "invalid"
	StatusManual      Status = "manual"
	StatusScheduled   Status = "scheduled"
	StatusPending     Status = "pending"
	StatusDue         Status = "due"
	StatusExpired     Status = "expired"
	StatusRunning     Status = "running"
	StatusInterrupted Status = "interrupted"
	StatusFinished    Status = "finished"
)

// State is the live state of one job, shared by every listing surface.
type State struct {
	Status Status `json:"status"`
	// NextRunAt is the next recurring occurrence, or run_at of a once job
	// that has not run yet (in the past while due or expired).
	NextRunAt *time.Time `json:"next_run_at"`
	// LastRun is the latest final outcome, from any trigger.
	LastRun *LastRun `json:"last_run"`
}

// StateOf derives the job's state from its schedule, claim, lock, and last
// outcome. It reads a few small files; no scan.
func StateOf(home Home, job *Definition, now time.Time, loc *time.Location) State {
	last := home.LastRun(job.Name)
	running := home.IsRunning(job.Name)
	var status Status
	var next *time.Time
	switch job.Schedule.Kind {
	case Manual:
		status = StatusManual
	case Recurring:
		status = StatusScheduled
		if at, ok := NextRecurringAfter(job.Schedule.cron, now, loc); ok {
			next = &at
		}
	case Once:
		occ := OnceOccurrence(job.Schedule.RunAt)
		claim, _ := home.ReadClaim(job.Name, occ.Key)
		// The final claim wins over a `last.json` a crash left stale.
		if claim != nil {
			if fromClaim, ok := claim.LastRun(); ok {
				last = newerLastRun(last, &fromClaim)
			}
		}
		grace, _ := job.Grace()
		switch {
		case claim != nil && claim.Outcome != nil:
			status = StatusFinished
		case claim != nil && running:
			status = StatusRunning
		case claim != nil:
			status = StatusInterrupted
		case occ.At.After(now):
			status = StatusPending
		case now.Sub(occ.At) <= grace:
			status = StatusDue
		default:
			status = StatusExpired
		}
		if status == StatusPending || status == StatusDue || status == StatusExpired {
			at := occ.At
			next = &at
		}
	}
	if running && status != StatusFinished {
		status = StatusRunning
	}
	if next != nil {
		utc := next.UTC()
		next = &utc
	}
	return State{Status: status, NextRunAt: next, LastRun: last}
}

// Summary is the short listing of one job, without its resolved settings,
// for surfaces that refresh often (the Mini App).
type Summary struct {
	Name string `json:"name"`
	// Schedule is nil when the file is invalid.
	Schedule *Schedule `json:"schedule"`
	State
	Error *string `json:"error"`
}

// Summaries lists every job file, valid or not, sorted by name.
func Summaries(home Home, now time.Time, loc *time.Location) ([]Summary, error) {
	cat, err := ReadCatalog(home)
	if err != nil {
		return nil, err
	}
	out := make([]Summary, 0, len(cat.Jobs)+len(cat.Invalid))
	for i := range cat.Jobs {
		job := &cat.Jobs[i]
		schedule := job.Schedule
		out = append(out, Summary{Name: job.Name, Schedule: &schedule, State: StateOf(home, job, now, loc)})
	}
	for _, inv := range cat.Invalid {
		errText := inv.Error
		out = append(out, Summary{Name: inv.Name, State: State{Status: StatusInvalid}, Error: &errText})
	}
	sort.SliceStable(out, func(i, j int) bool { return out[i].Name < out[j].Name })
	return out, nil
}
