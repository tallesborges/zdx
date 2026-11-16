package jobs

import (
	"bufio"
	"bytes"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"io/fs"
	"log/slog"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"
)

// Trigger is what started a run. Runs from before the scheduler was
// renamed say `daemon`; they are read and shown as stored.
type Trigger string

const (
	TriggerManual    Trigger = "manual"
	TriggerScheduler Trigger = "scheduler"
)

// Outcome is the result of an attempt or an occurrence.
type Outcome string

const (
	Succeeded Outcome = "succeeded"
	Failed    Outcome = "failed"
	// Retrying is an attempt that failed with another to follow (run
	// records only).
	Retrying Outcome = "retrying"
	// Missed is an occurrence that did not start within its grace.
	Missed Outcome = "missed"
	// Interrupted is a run that stopped without a result.
	Interrupted Outcome = "interrupted"
)

// Claim is the durable record that one scheduled occurrence was taken.
type Claim struct {
	Job          string       `json:"job"`
	Key          string       `json:"key"`
	RunID        string       `json:"run_id"`
	Trigger      Trigger      `json:"trigger"`
	ScheduleKind ScheduleKind `json:"schedule_kind"`
	ScheduledFor time.Time    `json:"scheduled_for"`
	ClaimedAt    time.Time    `json:"claimed_at"`
	PID          int          `json:"pid"`
	// Outcome is nil while the run is in progress.
	Outcome    *Outcome   `json:"outcome"`
	FinishedAt *time.Time `json:"finished_at"`
	Error      *string    `json:"error"`
	ThreadID   *string    `json:"thread_id"`
}

// NewClaim is an unfinished claim made by this process now.
func NewClaim(job, key string, trigger Trigger, kind ScheduleKind, scheduledFor time.Time) Claim {
	return Claim{Job: job, Key: key, RunID: NewRunID(), Trigger: trigger, ScheduleKind: kind,
		ScheduledFor: scheduledFor.UTC(), ClaimedAt: time.Now().UTC(), PID: os.Getpid()}
}

// Finished marks the claim final now.
func (c Claim) Finished(outcome Outcome, errText *string) Claim {
	now := time.Now().UTC()
	c.Outcome, c.FinishedAt, c.Error = &outcome, &now, errText
	return c
}

// LastRun is the `last.json` view of a final claim; false while it runs.
func (c *Claim) LastRun() (LastRun, bool) {
	if c.Outcome == nil || c.FinishedAt == nil {
		return LastRun{}, false
	}
	at := c.ScheduledFor
	return LastRun{RunID: c.RunID, Trigger: c.Trigger, Outcome: *c.Outcome, ScheduledFor: &at,
		FinishedAt: *c.FinishedAt, Error: c.Error, ThreadID: c.ThreadID}, true
}

// LastRun is the latest final outcome of a job.
type LastRun struct {
	RunID        string     `json:"run_id"`
	Trigger      Trigger    `json:"trigger"`
	Outcome      Outcome    `json:"outcome"`
	ScheduledFor *time.Time `json:"scheduled_for"`
	FinishedAt   time.Time  `json:"finished_at"`
	Error        *string    `json:"error"`
	ThreadID     *string    `json:"thread_id"`
}

// newerLastRun is the later of two final outcomes by FinishedAt.
func newerLastRun(a, b *LastRun) *LastRun {
	switch {
	case a == nil:
		return b
	case b == nil:
		return a
	case b.FinishedAt.After(a.FinishedAt):
		return b
	}
	return a
}

// RunRecord is one line of `jobs_runs.jsonl`. Instants are RFC 3339 strings
// with a `+00:00` offset.
type RunRecord struct {
	Job          string        `json:"job"`
	RunID        *string       `json:"run_id"`
	Trigger      Trigger       `json:"trigger"`
	ScheduleKind *ScheduleKind `json:"schedule_kind"`
	ScheduledFor *string       `json:"scheduled_for"`
	LatenessSecs *int64        `json:"lateness_secs"`
	ThreadID     *string       `json:"thread_id"`
	Attempt      uint32        `json:"attempt"`
	MaxAttempts  uint32        `json:"max_attempts"`
	StartedAt    string        `json:"started_at"`
	FinishedAt   string        `json:"finished_at"`
	DurationMS   uint64        `json:"duration_ms"`
	OK           bool          `json:"ok"`
	Outcome      *Outcome      `json:"outcome"`
	Error        *string       `json:"error"`
	Schedule     *string       `json:"schedule"`
	Model        *string       `json:"model"`
	Subagent     *string       `json:"subagent"`
	Root         *string       `json:"root"`
}

// NotRunRecord records an occurrence that never ran (missed, failed to
// start, or interrupted before a result), stamped now.
func NotRunRecord(job, runID string, trigger Trigger, schedule Schedule, scheduledFor time.Time, outcome Outcome, errText string) RunRecord {
	now := time.Now().UTC()
	kind := schedule.Kind
	lateness := int64(now.Sub(scheduledFor) / time.Second)
	desc := schedule.Describe()
	return RunRecord{Job: job, RunID: &runID, Trigger: trigger, ScheduleKind: &kind,
		ScheduledFor: ptr(offsetRFC3339(scheduledFor.UTC())), LatenessSecs: &lateness,
		StartedAt: offsetRFC3339(now), FinishedAt: offsetRFC3339(now), Outcome: &outcome,
		Error: &errText, Schedule: &desc}
}

// SchedulerState is the scheduler bookkeeping that survives restarts.
type SchedulerState struct {
	// Since: recurring occurrences at or before it never run or count as
	// missed.
	Since time.Time `json:"since"`
	// LastCheckedAt is the latest tick; it never moves backward.
	LastCheckedAt time.Time `json:"last_checked_at"`
}

type missedCheckpoint struct {
	Through time.Time `json:"through"`
}

// NewRunID is a random UUID v4 for one run or one recorded occurrence.
func NewRunID() string {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		panic(err)
	}
	b[6] = b[6]&0x0f | 0x40
	b[8] = b[8]&0x3f | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}

// ---- claims ----

// CreateClaim writes c only if its occurrence has no claim. The file appears
// complete or not at all (written aside, then hard-linked).
func (h Home) CreateClaim(c Claim) (bool, error) {
	path := h.claimPath(c.Job, c.Key)
	data, err := prettyJSON(c)
	if err != nil {
		return false, err
	}
	tmp, err := writeAside(path, data)
	if err != nil {
		return false, err
	}
	err = os.Link(tmp, path)
	_ = os.Remove(tmp)
	switch {
	case err == nil:
		return true, nil
	case errors.Is(err, fs.ErrExist):
		return false, nil
	}
	return false, fmt.Errorf("create claim %s: %w", path, err)
}

// UpdateClaim replaces an existing claim.
func (h Home) UpdateClaim(c Claim) error {
	data, err := prettyJSON(c)
	if err != nil {
		return err
	}
	return writeAtomic(h.claimPath(c.Job, c.Key), data)
}

// ReadClaim reads one claim; nil when it does not exist.
func (h Home) ReadClaim(job, key string) (*Claim, error) {
	var c Claim
	ok, err := readJSON(h.claimPath(job, key), &c)
	if !ok || err != nil {
		return nil, err
	}
	return &c, nil
}

// AllClaims scans every claim of every job: call it at scheduler start and on
// its periodic rescan, not per tick. Unreadable claims are skipped.
func (h Home) AllClaims(log *slog.Logger) ([]Claim, error) {
	entries, err := os.ReadDir(h.stateDir())
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", h.stateDir(), err)
	}
	var out []Claim
	for _, job := range entries {
		dir := filepath.Join(h.stateDir(), job.Name(), "claims")
		files, err := os.ReadDir(dir)
		if err != nil {
			continue
		}
		for _, f := range files {
			if filepath.Ext(f.Name()) != ".json" {
				continue
			}
			path := filepath.Join(dir, f.Name())
			var c Claim
			ok, err := readJSON(path, &c)
			if err != nil {
				log.Warn("Skipping unreadable job claim", "path", path, "err", err)
				continue
			}
			if ok {
				out = append(out, c)
			}
		}
	}
	return out, nil
}

// PruneRecurringClaims removes finished recurring claims scheduled before
// before. Once-job claims stay: they keep a restored file from running
// again.
func (h Home) PruneRecurringClaims(before time.Time, log *slog.Logger) (int, error) {
	claims, err := h.AllClaims(log)
	if err != nil {
		return 0, err
	}
	removed := 0
	for _, c := range claims {
		if c.ScheduleKind == Recurring && c.Outcome != nil && c.ScheduledFor.Before(before) &&
			os.Remove(h.claimPath(c.Job, c.Key)) == nil {
			removed++
		}
	}
	return removed, nil
}

// ---- last outcome and missed checkpoint ----

// WriteLastRun replaces `last.json`.
func (h Home) WriteLastRun(job string, last LastRun) error {
	data, err := prettyJSON(last)
	if err != nil {
		return err
	}
	return writeAtomic(filepath.Join(h.jobStateDir(job), "last.json"), data)
}

// LastRun reads `last.json`; nil when missing or unreadable.
func (h Home) LastRun(job string) *LastRun {
	var last LastRun
	if ok, err := readJSON(filepath.Join(h.jobStateDir(job), "last.json"), &last); !ok || err != nil {
		return nil
	}
	return &last
}

// ReconcileLastRuns rewrites `last.json` of each job whose newest final
// claim is newer than it (or which has none). Jobs whose lock is held are
// skipped: their run writes it. It returns the jobs repaired.
func (h Home) ReconcileLastRuns(claims []Claim) ([]string, error) {
	newest := map[string]LastRun{}
	for i := range claims {
		last, ok := claims[i].LastRun()
		if !ok {
			continue
		}
		if cur, ok := newest[claims[i].Job]; !ok || cur.FinishedAt.Before(last.FinishedAt) {
			newest[claims[i].Job] = last
		}
	}
	names := make([]string, 0, len(newest))
	for name := range newest {
		names = append(names, name)
	}
	sort.Strings(names)
	var repaired []string
	for _, job := range names {
		fromClaim := newest[job]
		stale := func() bool {
			last := h.LastRun(job)
			return last == nil || last.FinishedAt.Before(fromClaim.FinishedAt)
		}
		if !stale() {
			continue
		}
		lock, err := h.TryLock(job)
		if err != nil {
			return repaired, err
		}
		if lock == nil {
			continue
		}
		// Re-read under the lock: the run may have just written it.
		if stale() {
			if err := h.WriteLastRun(job, fromClaim); err != nil {
				lock.Release()
				return repaired, err
			}
			repaired = append(repaired, job)
		}
		lock.Release()
	}
	return repaired, nil
}

// MissedThrough is the latest recurring occurrence already recorded as
// missed.
func (h Home) MissedThrough(job string) (time.Time, bool) {
	var cp missedCheckpoint
	ok, err := readJSON(filepath.Join(h.jobStateDir(job), "missed.json"), &cp)
	return cp.Through, ok && err == nil
}

// SetMissedThrough moves the missed checkpoint; it never moves backward.
func (h Home) SetMissedThrough(job string, through time.Time) error {
	if old, ok := h.MissedThrough(job); ok && old.After(through) {
		through = old
	}
	data, err := prettyJSON(missedCheckpoint{Through: through.UTC()})
	if err != nil {
		return err
	}
	return writeAtomic(filepath.Join(h.jobStateDir(job), "missed.json"), data)
}

// ---- scheduler state ----

// SchedulerState reads `state/jobs/scheduler.json`.
func (h Home) SchedulerState() (SchedulerState, bool) {
	var s SchedulerState
	ok, err := readJSON(filepath.Join(h.stateDir(), "scheduler.json"), &s)
	return s, ok && err == nil
}

// SaveSchedulerState replaces `state/jobs/scheduler.json`.
func (h Home) SaveSchedulerState(s SchedulerState) error {
	data, err := prettyJSON(SchedulerState{Since: s.Since.UTC(), LastCheckedAt: s.LastCheckedAt.UTC()})
	if err != nil {
		return err
	}
	return writeAtomic(filepath.Join(h.stateDir(), "scheduler.json"), data)
}

// ---- run log ----

// AppendRunRecord appends one line to `jobs_runs.jsonl` under
// `jobs_runs.lock`, in one write.
func (h Home) AppendRunRecord(r RunRecord) error {
	if err := os.MkdirAll(h.Dir, 0o777); err != nil {
		return fmt.Errorf("create %s: %w", h.Dir, err)
	}
	unlock, err := lockFile(filepath.Join(h.Dir, "jobs_runs.lock"))
	if err != nil {
		return err
	}
	defer unlock()
	line, err := compactJSON(r)
	if err != nil {
		return fmt.Errorf("serialize job run record: %w", err)
	}
	f, err := os.OpenFile(h.RunsLog(), os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o666)
	if err != nil {
		return fmt.Errorf("open job runs log %s: %w", h.RunsLog(), err)
	}
	defer f.Close()
	if _, err := f.Write(append(line, '\n')); err != nil {
		return fmt.Errorf("append %s: %w", h.RunsLog(), err)
	}
	return nil
}

// ReadRunRecords reads `jobs_runs.jsonl` (empty when missing). A line that
// does not parse is skipped with a warning.
func (h Home) ReadRunRecords(log *slog.Logger) ([]RunRecord, error) {
	f, err := os.Open(h.RunsLog())
	if os.IsNotExist(err) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", h.RunsLog(), err)
	}
	defer f.Close()
	var out []RunRecord
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 0, 64*1024), 64*1024*1024)
	for n := 1; sc.Scan(); n++ {
		line := bytes.TrimSpace(sc.Bytes())
		if len(line) == 0 {
			continue
		}
		var r struct {
			RunRecord
			Job        *string `json:"job"`
			Trigger    *string `json:"trigger"`
			StartedAt  *string `json:"started_at"`
			FinishedAt *string `json:"finished_at"`
			OK         *bool   `json:"ok"`
		}
		err := json.Unmarshal(line, &r)
		if err == nil && (r.Job == nil || r.Trigger == nil || r.StartedAt == nil || r.FinishedAt == nil || r.OK == nil) {
			err = errors.New("missing a required field")
		}
		if err != nil {
			log.Warn("Skipping unreadable job run record", "path", h.RunsLog(), "line", n, "err", err)
			continue
		}
		rec := r.RunRecord
		rec.Job, rec.Trigger, rec.StartedAt, rec.FinishedAt, rec.OK = *r.Job, Trigger(*r.Trigger), *r.StartedAt, *r.FinishedAt, *r.OK
		out = append(out, rec)
	}
	if err := sc.Err(); err != nil {
		return nil, fmt.Errorf("read %s: %w", h.RunsLog(), err)
	}
	return out, nil
}

// ---- archive ----

// ArchiveOnceJob moves the once job at path to
// `jobs/archive/<name>--<key>[-N].md` when it still declares runAt. A file
// that was rescheduled, made invalid, or removed stays. It returns the
// archive path when moved.
//
// Job files have no writer lock, so nothing is lost if the file is edited
// meanwhile: the archive name is hard-linked (never replacing a file) and
// checked again after the link; then whatever is at path is taken aside with
// one rename, and put back if it is not the archived file. On error every
// version is at path, in the archive, or beside path as
// `.<name>.md.archiving-<uuid>`.
func (h Home) ArchiveOnceJob(path string, runAt time.Time) (string, error) {
	return h.archiveOnceJob(path, runAt, func() {})
}

func (h Home) archiveOnceJob(path string, runAt time.Time, afterLink func()) (string, error) {
	def, err := onceDefinitionAt(path, runAt)
	if err != nil || def == nil {
		return "", err
	}
	dir := h.ArchiveDir()
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return "", fmt.Errorf("create %s: %w", dir, err)
	}
	stem := def.Name + "--" + OnceOccurrence(runAt).Key
	var dest string
	for n := 1; ; n++ {
		dest = filepath.Join(dir, stem+".md")
		if n > 1 {
			dest = filepath.Join(dir, fmt.Sprintf("%s-%d.md", stem, n))
		}
		err := os.Link(path, dest)
		if err == nil {
			break
		}
		if errors.Is(err, fs.ErrExist) {
			continue
		}
		if errors.Is(err, fs.ErrNotExist) {
			return "", nil
		}
		return "", fmt.Errorf("link %s to %s: %w", path, dest, err)
	}
	afterLink()

	// An in-place edit since the first check changes the linked file too.
	still, err := onceDefinitionAt(dest, runAt)
	if err != nil {
		return "", err
	}
	if still == nil {
		if err := os.Remove(dest); err != nil {
			return "", fmt.Errorf("remove %s: %w", dest, err)
		}
		return "", nil
	}
	aside := filepath.Join(filepath.Dir(path), "."+filepath.Base(path)+".archiving-"+NewRunID())
	if err := os.Rename(path, aside); err != nil {
		if errors.Is(err, fs.ErrNotExist) {
			// Removed meanwhile: the archived copy is the only one left.
			return dest, nil
		}
		return "", fmt.Errorf("move %s aside: %w", path, err)
	}
	a, errA := os.Stat(aside)
	d, errD := os.Stat(dest)
	if errA != nil || errD != nil {
		return "", fmt.Errorf("stat %s: %w", aside, errors.Join(errA, errD))
	}
	if os.SameFile(a, d) {
		if err := os.Remove(aside); err != nil {
			return "", fmt.Errorf("remove %s: %w", aside, err)
		}
		return dest, nil
	}
	// path was replaced by a newer version after the link: keep it.
	if err := os.Link(aside, path); err != nil {
		return "", fmt.Errorf("%s was replaced while archiving and a newer file now exists there; the replaced version is kept at %s: %w", path, aside, err)
	}
	if err := os.Remove(aside); err != nil {
		return "", fmt.Errorf("remove %s: %w", aside, err)
	}
	return dest, nil
}

// onceDefinitionAt is the definition at path when it parses and is a once
// job at runAt.
func onceDefinitionAt(path string, runAt time.Time) (*Definition, error) {
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return nil, nil
	}
	if err != nil {
		return nil, fmt.Errorf("read %s: %w", path, err)
	}
	def, err := ParseContent(path, string(b))
	if err != nil || def.Schedule.Kind != Once || !def.Schedule.RunAt.Equal(runAt) {
		return nil, nil
	}
	return &def, nil
}

// ArchivedJob is one archived once job.
type ArchivedJob struct {
	Name string `json:"name"`
	Path string `json:"path"`
	// Key is the occurrence key from the file name; nil for a file not
	// named by the archiver.
	Key          *string    `json:"key"`
	ScheduledFor *time.Time `json:"scheduled_for"`
	// LastRun is the occurrence's final outcome; nil when its claim is
	// missing.
	LastRun *LastRun `json:"last_run"`
}

// Archived lists `jobs/archive/`, newest scheduled first, each with its
// claim's final outcome.
func (h Home) Archived() ([]ArchivedJob, error) {
	files, err := markdownFiles(h.ArchiveDir())
	if err != nil {
		return nil, err
	}
	out := []ArchivedJob{}
	for _, path := range files {
		base := filepath.Base(path)
		name, key := splitArchiveStem(strings.TrimSuffix(base, filepath.Ext(base)))
		item := ArchivedJob{Name: name, Path: path, Key: key}
		if key != nil {
			if c, _ := h.ReadClaim(name, *key); c != nil {
				at := c.ScheduledFor
				item.ScheduledFor = &at
				if last, ok := c.LastRun(); ok {
					item.LastRun = &last
				}
			}
		}
		out = append(out, item)
	}
	sort.SliceStable(out, func(i, j int) bool {
		a, b := out[i].ScheduledFor, out[j].ScheduledFor
		switch {
		case a == nil && b == nil:
		case a == nil:
			return false
		case b == nil:
			return true
		case !a.Equal(*b):
			return a.After(*b)
		}
		return out[i].Name < out[j].Name
	})
	return out, nil
}

// splitArchiveStem: `<name>--<YYYYMMDDTHHMMSSZ>[-N]` → (name, key).
func splitArchiveStem(stem string) (string, *string) {
	i := strings.LastIndex(stem, "--")
	if i < 0 {
		return stem, nil
	}
	name, rest := stem[:i], stem[i+2:]
	key, _, _ := strings.Cut(rest, "-")
	if len(key) == 16 && strings.HasSuffix(key, "Z") {
		if _, err := time.Parse("20060102T150405Z", key); err == nil {
			return name, &key
		}
	}
	return stem, nil
}

// ---- files ----

// writeAside writes data to a unique synced temp file beside path.
func writeAside(path string, data []byte) (string, error) {
	dir := filepath.Dir(path)
	if err := os.MkdirAll(dir, 0o777); err != nil {
		return "", fmt.Errorf("create %s: %w", dir, err)
	}
	tmp := filepath.Join(dir, "."+filepath.Base(path)+"."+NewRunID()+".tmp")
	f, err := os.Create(tmp)
	if err != nil {
		return "", fmt.Errorf("create %s: %w", tmp, err)
	}
	_, err = f.Write(data)
	if err == nil {
		err = f.Sync()
	}
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err != nil {
		_ = os.Remove(tmp)
		return "", fmt.Errorf("write %s: %w", tmp, err)
	}
	return tmp, nil
}

func writeAtomic(path string, data []byte) error {
	tmp, err := writeAside(path, data)
	if err != nil {
		return err
	}
	if err := os.Rename(tmp, path); err != nil {
		_ = os.Remove(tmp)
		return fmt.Errorf("replace %s: %w", path, err)
	}
	return nil
}

// readJSON decodes path into v; false when it does not exist.
func readJSON(path string, v any) (bool, error) {
	b, err := os.ReadFile(path)
	if errors.Is(err, fs.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, fmt.Errorf("read %s: %w", path, err)
	}
	if err := json.Unmarshal(b, v); err != nil {
		return false, fmt.Errorf("parse %s: %w", path, err)
	}
	return true, nil
}

// prettyJSON is serde_json's pretty format: two-space indent, no HTML
// escaping, no trailing newline.
func prettyJSON(v any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	enc.SetIndent("", "  ")
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

func compactJSON(v any) ([]byte, error) {
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(v); err != nil {
		return nil, err
	}
	return bytes.TrimSuffix(buf.Bytes(), []byte("\n")), nil
}

// offsetRFC3339 formats like chrono's to_rfc3339: the time's own offset
// (`+00:00` for UTC, never `Z`) and 0, 3, 6, or 9 fraction digits.
func offsetRFC3339(t time.Time) string {
	layout := "2006-01-02T15:04:05"
	switch ns := t.Nanosecond(); {
	case ns == 0:
	case ns%1_000_000 == 0:
		layout += ".000"
	case ns%1_000 == 0:
		layout += ".000000"
	default:
		layout += ".000000000"
	}
	return t.Format(layout + "-07:00")
}

func ptr[T any](v T) *T { return &v }
