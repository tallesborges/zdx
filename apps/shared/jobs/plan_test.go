package jobs

import (
	"fmt"
	"testing"
	"time"
)

var planTZ = time.FixedZone("", -3*3600)

func at(h, m, s int) time.Time { return time.Date(2026, 10, 20, h, m, s, 0, time.UTC) }

func planJob(t *testing.T, name string, schedule, runAt *string, grace *uint32) Definition {
	t.Helper()
	s, err := ParseSchedule(schedule, runAt)
	if err != nil {
		t.Fatal(err)
	}
	return Definition{Name: name, Path: "/jobs/" + name + ".md", Schedule: s, GraceSecs: grace, Prompt: "prompt"}
}

type world struct {
	claims map[[2]string]ClaimStatus
	busy   map[string]bool
	missed map[string]time.Time
}

func newWorld() *world {
	return &world{claims: map[[2]string]ClaimStatus{}, busy: map[string]bool{}, missed: map[string]time.Time{}}
}

func (w *world) claim(job, key string, s ClaimStatus) *world {
	w.claims[[2]string{job, key}] = s
	return w
}

func (w *world) Claim(job, key string) ClaimStatus { return w.claims[[2]string{job, key}] }
func (w *world) Busy(job string) bool              { return w.busy[job] }
func (w *world) MissedThrough(job string) (time.Time, bool) {
	t, ok := w.missed[job]
	return t, ok
}

func (w *world) plan(in TickInput, jobs ...Definition) []Action { return PlanTick(in, jobs, planTZ, w) }

func tick(now, last time.Time) TickInput {
	return TickInput{Now: now, Since: at(0, 0, 0), LastCheckedAt: last}
}

func startKeys(actions []Action) []string {
	var out []string
	for _, a := range actions {
		if a.Kind == ActStart {
			out = append(out, a.Job+"@"+a.Occurrence.Key)
		}
	}
	return out
}

func kinds(actions []Action) string { return fmt.Sprint(actions) }

func TestOnceJobStartsOnTimeAndLateWithinGrace(t *testing.T) {
	j := planJob(t, "remind", nil, ptr("2026-10-20T09:00:00-03:00"), nil)
	w := newWorld()
	if a := w.plan(tick(at(11, 59, 30), at(11, 59, 0)), j); len(a) != 0 {
		t.Fatal("pending:", a)
	}
	if got := startKeys(w.plan(tick(at(12, 0, 5), at(11, 59, 35)), j)); fmt.Sprint(got) != "[remind@20261020T120000Z]" {
		t.Fatal(got)
	}
	if got := startKeys(w.plan(tick(at(17, 59, 0), at(17, 58, 30)), j)); len(got) != 1 {
		t.Fatal("still within the 6 h default grace:", got)
	}
}

func TestOnceJobPastGraceIsMissedNotRun(t *testing.T) {
	j := planJob(t, "remind", nil, ptr("2026-10-20T12:00:00Z"), ptr(uint32(600)))
	a := newWorld().plan(tick(at(12, 10, 1), at(12, 10, 0)), j)
	if len(a) != 1 || a[0].Kind != ActMissOnce || a[0].Job != "remind" {
		t.Fatal(kinds(a))
	}
}

func TestOnceJobClaimsDecideArchiveWaitOrRecover(t *testing.T) {
	j := planJob(t, "remind", nil, ptr("2026-10-20T12:00:00Z"), nil)
	key, now := "20261020T120000Z", tick(at(12, 1, 0), at(12, 0, 30))
	if a := newWorld().claim("remind", key, ClaimFinished).plan(now, j); len(a) != 1 || a[0].Kind != ActArchive {
		t.Fatal(kinds(a))
	}
	running := newWorld().claim("remind", key, ClaimRunning)
	running.busy["remind"] = true
	if a := running.plan(now, j); len(a) != 0 {
		t.Fatal("live run: wait", kinds(a))
	}
	if a := newWorld().claim("remind", key, ClaimRunning).plan(now, j); len(a) != 1 || a[0].Kind != ActRecover {
		t.Fatal(kinds(a))
	}
}

func TestBusyJobWaitsInsteadOfStartingTwice(t *testing.T) {
	j := planJob(t, "remind", nil, ptr("2026-10-20T12:00:00Z"), nil)
	w := newWorld()
	w.busy["remind"] = true
	if a := w.plan(tick(at(12, 0, 5), at(12, 0, 0)), j); len(a) != 0 {
		t.Fatal(kinds(a))
	}
}

func TestRecurringJobRunsOnlyTheLatestOccurrenceWithinGrace(t *testing.T) {
	// Every 5 minutes, local UTC-3. 11:50 and 11:55 (UTC) ran; the scheduler was
	// down from 11:58 to 12:07, so 12:00 and 12:05 passed while it was down.
	j := planJob(t, "poll", ptr("*/5 * * * *"), nil, nil)
	w := newWorld().claim("poll", "20261020T0850", ClaimFinished).claim("poll", "20261020T0855", ClaimFinished)
	a := w.plan(tick(at(12, 7, 0), at(11, 58, 0)), j)
	if fmt.Sprint(startKeys(a)) != "[poll@20261020T0905]" || len(a) != 1 {
		t.Fatal("12:00 is still inside the grace:", kinds(a))
	}
	// After 12:05 ran, 12:00 expires unstarted and counts as missed once.
	w.claim("poll", "20261020T0905", ClaimFinished)
	first := w.plan(tick(at(12, 10, 30), at(12, 7, 0)), j)
	if len(first) != 2 || first[0].Kind != ActMissRecurring || first[0].Count != 1 ||
		first[0].Occurrence.Key != "20261020T0900" || first[1].Kind != ActStart {
		t.Fatal(kinds(first))
	}
	w.claim("poll", "20261020T0910", ClaimFinished)
	for _, a := range w.plan(tick(at(12, 15, 30), at(12, 10, 30)), j) {
		if a.Kind == ActMissRecurring {
			t.Fatal("counted twice:", a)
		}
	}
}

func TestARecordedMissedSummaryIsNotRepeatedWhenTheTickWasNotSaved(t *testing.T) {
	// 10:55, 11:00, and 11:05 expired unstarted; their summary was written,
	// but last_checked_at stayed at 11:05.
	j := planJob(t, "poll", ptr("*/5 * * * *"), nil, nil)
	in := tick(at(11, 16, 0), at(11, 5, 0))
	found := false
	for _, a := range newWorld().plan(in, j) {
		if a.Kind == ActMissRecurring {
			found = a.Count == 3 && a.Occurrence.Key == "20261020T0805"
		}
	}
	if !found {
		t.Fatal(kinds(newWorld().plan(in, j)))
	}
	w := newWorld()
	w.missed["poll"] = at(11, 5, 0)
	for _, a := range w.plan(in, j) {
		if a.Kind == ActMissRecurring {
			t.Fatal("repeated:", a)
		}
	}
}

func TestRecurringOccurrenceAlreadyClaimedIsNotStartedAgain(t *testing.T) {
	j := planJob(t, "daily", ptr("0 9 * * *"), nil, nil)
	w := newWorld().claim("daily", "20261020T0900", ClaimFinished)
	if a := w.plan(tick(at(12, 0, 30), at(12, 0, 0)), j); len(a) != 0 {
		t.Fatal(kinds(a))
	}
	if a := w.plan(tick(at(12, 0, 59), at(12, 0, 30)), j); len(a) != 0 {
		t.Fatal("a fast failure in the same minute is not retried:", kinds(a))
	}
}

func TestRecurringOccurrencesBeforeTheFirstStartAreIgnored(t *testing.T) {
	j := planJob(t, "daily", ptr("0 9 * * *"), nil, nil)
	in := TickInput{Now: at(12, 5, 0), Since: at(12, 4, 0), LastCheckedAt: at(12, 4, 0)}
	if a := newWorld().plan(in, j); len(a) != 0 {
		t.Fatal(kinds(a))
	}
}

func TestLongOutageReportsMissedOccurrencesFromACappedWindow(t *testing.T) {
	j := planJob(t, "hourly", ptr("0 * * * *"), nil, nil)
	long := time.Date(2026, 9, 1, 0, 0, 0, 0, time.UTC)
	a := newWorld().plan(TickInput{Now: at(12, 30, 0), Since: long, LastCheckedAt: long}, j)
	if len(a) != 1 || a[0].Kind != ActMissRecurring || a[0].Count != 7*24 {
		t.Fatal("12:00 is outside the 10 min grace:", kinds(a))
	}
}

func TestManualJobsAreNeverPlannedAndNextDueIgnoresThem(t *testing.T) {
	jobs := []Definition{
		planJob(t, "adhoc", nil, nil, nil),
		planJob(t, "later", nil, ptr("2026-10-20T15:00:00Z"), nil),
		planJob(t, "daily", ptr("0 9 * * *"), nil, nil),
	}
	if a := PlanTick(tick(at(12, 30, 0), at(12, 29, 30)), jobs, planTZ, newWorld()); len(a) != 0 {
		t.Fatal(kinds(a))
	}
	if next, ok := NextDueAfter(jobs, at(12, 30, 0), planTZ); !ok || !next.Equal(at(15, 0, 0)) {
		t.Fatal(next)
	}
}
