package jobs

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"
)

// fakeZdx writes a `zdx` stand-in for `jobs attempt` and `jobs resolve`. Each
// attempt appends its argv to calls, its stdin to inputs, notes whether fd
// 3 is open, and answers with the next line of outcomes (`succeeded`,
// `failed`, `unprepared`, `interrupted`, `signal`, `noresult`), default
// `succeeded`.
func fakeZdx(t *testing.T, outcomes ...string) (bin, dir string) {
	t.Helper()
	dir = t.TempDir()
	writeFile(t, filepath.Join(dir, "outcomes"), strings.Join(outcomes, "\n")+"\n")
	script := `#!/bin/sh
dir="$(dirname "$0")"
echo "$*" >> "$dir/calls"
result=""
while [ $# -gt 0 ]; do
  case "$1" in
    --result) result="$2"; shift ;;
    resolve) cat > "$dir/resolve-input"; cat "$dir/resolve-output"; exit 0 ;;
  esac
  shift
done
cat >> "$dir/inputs"; echo >> "$dir/inputs"
if [ -e /dev/fd/3 ]; then echo yes >> "$dir/fd3"; else echo no >> "$dir/fd3"; fi
next="$(head -n 1 "$dir/outcomes")"; tail -n +2 "$dir/outcomes" > "$dir/outcomes.next"; mv "$dir/outcomes.next" "$dir/outcomes"
[ -n "$next" ] || next=succeeded
case "$next" in
  succeeded) echo '{"outcome":"succeeded","prepared":true,"error":null,"model":"fake:model@low","root":"/work"}' > "$result" ;;
  failed) echo '{"outcome":"failed","prepared":true,"error":"provider said no","model":"fake:model@low","root":"/work"}' > "$result"; exit 1 ;;
  unprepared) echo '{"outcome":"failed","prepared":false,"error":"prepare job run: root missing","model":null,"root":null}' > "$result"; exit 1 ;;
  interrupted) echo '{"outcome":"interrupted","prepared":true,"error":"interrupted","model":"fake:model@low","root":"/work"}' > "$result"; exit 130 ;;
  signal) kill -9 $$ ;;
  noresult) exit 3 ;;
esac
`
	bin = writeFile(t, filepath.Join(dir, "zdx"), script)
	if err := os.Chmod(bin, 0o755); err != nil {
		t.Fatal(err)
	}
	return bin, dir
}

func lines(t *testing.T, path string) []string {
	t.Helper()
	b, err := os.ReadFile(path)
	if os.IsNotExist(err) {
		return nil
	}
	if err != nil {
		t.Fatal(err)
	}
	return strings.Split(strings.TrimRight(string(b), "\n"), "\n")
}

func newRunner(t *testing.T, home Home, bin string) *Runner {
	t.Helper()
	return &Runner{Home: home, Engine: Engine{Zdx: bin, Root: "/caller/root"}, Out: &strings.Builder{}, Log: quiet,
		RetryDelay: func(uint32) time.Duration { return 0 }}
}

func records(t *testing.T, home Home) []RunRecord {
	t.Helper()
	r, err := home.ReadRunRecords(quiet)
	if err != nil {
		t.Fatal(err)
	}
	return r
}

func TestManualRunConsumesAOnceJobAndKeepDoesNot(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, dir := fakeZdx(t)
	path := onceJob(t, home, "remind", "2030-01-02T09:00:00-03:00", "Say hello.")
	r := newRunner(t, home, bin)

	if err := r.Run(RunRequest{Name: "remind", Keep: true, NoThread: true}); err != nil {
		t.Fatal(err)
	}
	if _, err := os.Stat(path); err != nil {
		t.Fatal("--keep leaves the job scheduled")
	}
	if c, _ := home.ReadClaim("remind", "20300102T120000Z"); c != nil {
		t.Fatal("--keep takes no occurrence")
	}
	job, _ := LoadByName(home, "remind")
	if st := StateOf(home, job, time.Now(), time.Local); st.Status != StatusPending || st.LastRun.Outcome != Succeeded {
		t.Fatalf("%+v", st)
	}

	if err := r.Run(RunRequest{Name: "remind", NoThread: true}); err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(r.Out.(*strings.Builder).String(), "Archived 'remind'") {
		t.Fatal(r.Out)
	}
	if _, err := os.Stat(filepath.Join(home.ArchiveDir(), "remind--20300102T120000Z.md")); err != nil {
		t.Fatal(err)
	}
	taken, _ := home.ReadClaim("remind", "20300102T120000Z")
	if taken == nil || *taken.Outcome != Succeeded || taken.Trigger != TriggerManual {
		t.Fatalf("%+v", taken)
	}
	all := records(t, home)
	if len(all) != 2 || all[0].ScheduledFor != nil || *all[1].ScheduledFor != "2030-01-02T12:00:00+00:00" ||
		*all[1].ScheduleKind != Once || *all[1].Outcome != Succeeded || *all[1].RunID != taken.RunID ||
		*all[1].Model != "fake:model@low" || *all[1].Root != "/work" || all[1].ThreadID != nil {
		t.Fatalf("%+v", all)
	}

	// Restoring the same file does not make the occurrence run again.
	onceJob(t, home, "remind", "2030-01-02T09:00:00-03:00", "Say hello.")
	job, _ = LoadByName(home, "remind")
	if st := StateOf(home, job, time.Now(), time.Local); st.Status != StatusFinished {
		t.Fatal(st.Status)
	}
	if err := r.Run(RunRequest{Name: "remind", NoThread: true}); err == nil || !strings.Contains(err.Error(), "already ran for run_at") {
		t.Fatal(err)
	}
	if got := len(lines(t, filepath.Join(dir, "calls"))); got != 2 {
		t.Fatal("attempts:", got)
	}
}

func TestAttemptGetsTheJobAndTheLockedDescriptor(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, dir := fakeZdx(t)
	writeFile(t, filepath.Join(home.JobsDir(), "daily.md"),
		"---\nschedule: \"0 9 * * *\"\nmodel: fake:model\nsubagent: helper\nroot: \"~/work\"\ntimeout_secs: 60\n---\nDo it.\n")
	r := newRunner(t, home, bin)
	r.Engine.SystemPrompt = ptr("Be brief.")
	r.Now = func() time.Time { return time.Date(2026, 10, 20, 12, 0, 1, 0, time.UTC) }
	if err := r.Run(RunRequest{Name: "daily"}); err != nil {
		t.Fatal(err)
	}
	calls := lines(t, filepath.Join(dir, "calls"))
	if len(calls) != 1 || !strings.HasPrefix(calls[0], "--root /caller/root --system-prompt Be brief. jobs attempt --result ") ||
		!strings.HasSuffix(calls[0], " --lock-fd 3") {
		t.Fatal(calls)
	}
	if fd := lines(t, filepath.Join(dir, "fd3")); fd[0] != "yes" {
		t.Fatal("fd 3 not passed")
	}
	var in AttemptInput
	if err := json.Unmarshal([]byte(lines(t, filepath.Join(dir, "inputs"))[0]), &in); err != nil {
		t.Fatal(err)
	}
	if in.Job != "daily" || in.Prompt != "Do it." || *in.Model != "fake:model" || *in.Subagent != "helper" ||
		*in.Root != "~/work" || *in.TimeoutSecs != 60 || *in.ThreadID != "job-daily-20261020-120001" || in.NoThread || in.RunID == "" {
		t.Fatalf("%+v", in)
	}
	if rec := records(t, home)[0]; *rec.RunID != in.RunID || *rec.ThreadID != *in.ThreadID || *rec.Subagent != "helper" {
		t.Fatalf("%+v", rec)
	}
}

func TestFailedAttemptsRetryAndUnpreparedOnesDoNot(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, _ := fakeZdx(t, "failed", "succeeded", "unprepared")
	writeFile(t, filepath.Join(home.JobsDir(), "flaky.md"), "---\nmax_retries: 2\n---\nTry.\n")
	r := newRunner(t, home, bin)
	if err := r.Run(RunRequest{Name: "flaky", NoThread: true}); err != nil {
		t.Fatal(err)
	}
	all := records(t, home)
	if len(all) != 2 || *all[0].Outcome != Retrying || all[0].OK || *all[0].Error != "provider said no" ||
		*all[1].Outcome != Succeeded || all[1].Attempt != 2 || all[1].MaxAttempts != 3 || *all[0].RunID != *all[1].RunID {
		t.Fatalf("%+v", all)
	}

	err := r.Run(RunRequest{Name: "flaky", NoThread: true})
	if err == nil || err.Error() != "job 'flaky' failed after 1 attempt(s): prepare job run: root missing" {
		t.Fatal(err)
	}
	all = records(t, home)
	if len(all) != 3 || *all[2].Outcome != Failed || all[2].Model != nil || all[2].Root != nil {
		t.Fatalf("%+v", all[2])
	}
	if last := home.LastRun("flaky"); last.Outcome != Failed || !strings.Contains(*last.Error, "root missing") {
		t.Fatalf("%+v", last)
	}
}

func TestInterruptedOrKilledAttemptsEndTheRun(t *testing.T) {
	for _, outcome := range []string{"interrupted", "signal"} {
		home := Home{Dir: t.TempDir()}
		bin, _ := fakeZdx(t, outcome, "succeeded")
		writeFile(t, filepath.Join(home.JobsDir(), "long.md"), "---\nmax_retries: 3\n---\nWork.\n")
		err := newRunner(t, home, bin).Run(RunRequest{Name: "long", NoThread: true})
		if !errors.Is(err, ErrInterrupted) {
			t.Fatalf("%s: %v", outcome, err)
		}
		if all := records(t, home); len(all) != 1 || *all[0].Outcome != Interrupted {
			t.Fatalf("%s: %+v", outcome, all)
		}
	}
}

func TestAnAttemptWithoutAResultFailsAndIsRetried(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, _ := fakeZdx(t, "noresult")
	writeFile(t, filepath.Join(home.JobsDir(), "crashy.md"), "---\nmax_retries: 1\n---\nWork.\n")
	if err := newRunner(t, home, bin).Run(RunRequest{Name: "crashy", NoThread: true}); err != nil {
		t.Fatal(err)
	}
	all := records(t, home)
	if len(all) != 2 || *all[0].Outcome != Retrying || !strings.Contains(*all[0].Error, "exited (status 3) without a result") {
		t.Fatalf("%+v", all)
	}
}

func TestAnInterruptDuringTheRetryWaitEndsTheRun(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, _ := fakeZdx(t, "failed")
	writeFile(t, filepath.Join(home.JobsDir(), "slow.md"), "---\nmax_retries: 1\n---\nWork.\n")
	interrupted := make(chan struct{})
	close(interrupted)
	r := newRunner(t, home, bin)
	r.RetryDelay = func(uint32) time.Duration { return time.Hour }
	r.Interrupted = interrupted
	if err := r.Run(RunRequest{Name: "slow", NoThread: true}); !errors.Is(err, ErrInterrupted) {
		t.Fatal(err)
	}
	if last := home.LastRun("slow"); last.Outcome != Interrupted {
		t.Fatalf("%+v", last)
	}
	all := records(t, home)
	if len(all) != 2 || *all[0].Outcome != Retrying || *all[1].Outcome != Interrupted || all[1].Attempt != 0 ||
		*all[1].RunID != *all[0].RunID {
		t.Fatalf("the run's last record holds its final outcome: %+v", all)
	}
}

func TestSignalsReachALiveAttempt(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	dir := t.TempDir()
	bin := writeFile(t, filepath.Join(dir, "zdx"), "#!/bin/sh\nexec sleep 30\n")
	_ = os.Chmod(bin, 0o755)
	writeFile(t, filepath.Join(home.JobsDir(), "stuck.md"), "---\n---\nWork.\n")
	signals := make(chan os.Signal, 1)
	r := newRunner(t, home, bin)
	r.Signals = signals
	time.AfterFunc(200*time.Millisecond, func() { signals <- syscall.SIGINT })
	started := time.Now()
	if err := r.Run(RunRequest{Name: "stuck", NoThread: true}); !errors.Is(err, ErrInterrupted) {
		t.Fatal(err)
	}
	if time.Since(started) > 10*time.Second {
		t.Fatal("the attempt was not stopped")
	}
}

func TestARunRefusesWhileTheJobLockIsHeld(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, dir := fakeZdx(t)
	writeFile(t, filepath.Join(home.JobsDir(), "busy.md"), "---\n---\nWork.\n")
	lock, _ := home.TryLock("busy")
	defer lock.Release()
	var busy ErrBusy
	if err := newRunner(t, home, bin).Run(RunRequest{Name: "busy"}); !errors.As(err, &busy) || err.Error() != "job 'busy' is already running" {
		t.Fatal(err)
	}
	if len(records(t, home)) != 0 || lines(t, filepath.Join(dir, "calls")) != nil {
		t.Fatal("nothing ran")
	}
}

func TestAScheduledRunSkipsAJobThatChanged(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	bin, dir := fakeZdx(t)
	onceJob(t, home, "moved", "2030-01-02T09:00:00Z", "Work.")
	writeFile(t, filepath.Join(home.JobsDir(), "manual.md"), "---\n---\nWork.\n")
	r := newRunner(t, home, bin)
	for _, name := range []string{"moved", "manual"} {
		if err := r.Run(RunRequest{Name: name, Scheduled: &Occurrence{At: utc(2030, 1, 1, 9, 0), Key: "20300101T090000Z"}}); err != nil {
			t.Fatal(err)
		}
	}
	out := r.Out.(*strings.Builder).String()
	if !strings.Contains(out, "Skipped 'moved': run_at changed") || !strings.Contains(out, "Skipped 'manual': it no longer has a schedule") {
		t.Fatal(out)
	}
	if lines(t, filepath.Join(dir, "calls")) != nil {
		t.Fatal("nothing ran")
	}
}
