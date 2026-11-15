package cli

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/shared/jobs"
	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func write(t *testing.T, path, content string, mode os.FileMode) {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), mode); err != nil {
		t.Fatal(err)
	}
}

// fakeResolve is a `zdx` stand-in that answers `jobs resolve` with output
// and saves its stdin and arguments.
func fakeResolve(t *testing.T, output string) (bin, dir string) {
	dir = t.TempDir()
	write(t, filepath.Join(dir, "output"), output, 0o644)
	bin = filepath.Join(dir, "zdx")
	write(t, bin, "#!/bin/sh\necho \"$*\" > \""+dir+"/args\"\ncat > \""+dir+"/stdin\"\ncat \""+dir+"/output\"\n", 0o755)
	return bin, dir
}

type result struct {
	code           int
	stdout, stderr string
}

func zdxJobs(t *testing.T, home string, args ...string) result {
	t.Helper()
	t.Setenv("ZDX_HOME", home)
	var stdout, stderr bytes.Buffer
	code := run(args, &stdout, &stderr)
	return result{code, stdout.String(), stderr.String()}
}

const settings = `{"root":{"path":"/caller","origin":"caller"},"model":{"spec":"fake:model@low","provider":"fake","account":null,"model":"model","thinking":"low","fast":false,"origin":"job","thinking_origin":"job"},"subagent":null}`

func homeWithJobs(t *testing.T) string {
	home := t.TempDir()
	write(t, filepath.Join(home, "jobs", "explicit.md"), "---\nschedule: \"0 8 * * *\"\nmodel: \"fake:model@low\"\n---\nReport.\n", 0o644)
	write(t, filepath.Join(home, "jobs", "typo.md"), "---\nschedul: x\n---\nReport.\n", 0o644)
	return home
}

func TestListJSONMergesStatusAndRustSettings(t *testing.T) {
	home := homeWithJobs(t)
	bin, dir := fakeResolve(t, `{"daemon":null,"results":[{"settings":`+settings+`,"error":null}]}`)
	r := zdxJobs(t, home, "--root", "/caller", "--zdx", bin, "list", "--json")
	if r.code != 0 {
		t.Fatal(r)
	}
	var list []map[string]json.RawMessage
	if err := json.Unmarshal([]byte(r.stdout), &list); err != nil || len(list) != 2 {
		t.Fatal(r.stdout, err)
	}
	if string(list[0]["name"]) != `"explicit"` || string(list[0]["status"]) != `"scheduled"` ||
		string(list[0]["grace_secs"]) != "600" || string(list[0]["declared_model"]) != `"fake:model@low"` {
		t.Fatal(r.stdout)
	}
	var got, want any
	_ = json.Unmarshal(list[0]["settings"], &got)
	_ = json.Unmarshal([]byte(settings), &want)
	if !jsonEqual(got, want) {
		t.Fatalf("settings changed: %s", list[0]["settings"])
	}
	if string(list[1]["status"]) != `"invalid"` || string(list[1]["schedule"]) != "null" || !strings.Contains(string(list[1]["error"]), "unknown field") {
		t.Fatal(r.stdout)
	}
	args, _ := os.ReadFile(filepath.Join(dir, "args"))
	stdin, _ := os.ReadFile(filepath.Join(dir, "stdin"))
	if strings.TrimSpace(string(args)) != "--root /caller jobs resolve" ||
		string(stdin) != `{"jobs":[{"job":"explicit","model":"fake:model@low","subagent":null,"root":null}]}` {
		t.Fatalf("%s %s", args, stdin)
	}

	text := zdxJobs(t, home, "--zdx", bin, "list")
	if !strings.Contains(text.stdout, "explicit - 0 8 * * * - scheduled, next ") || !strings.Contains(text.stdout, "typo - invalid: ") {
		t.Fatal(text.stdout)
	}
}

func TestListDaemonPassesTheContextThroughAndSurvivesAResolveFailure(t *testing.T) {
	home := homeWithJobs(t)
	daemon := `{"status":"unavailable","reason":"daemon is not running"}`
	bin, dir := fakeResolve(t, `{"daemon":`+daemon+`,"results":[{"settings":null,"error":"daemon settings unavailable: daemon is not running"}]}`)
	r := zdxJobs(t, home, "--zdx", bin, "list", "--daemon", "--json")
	var view struct {
		Daemon json.RawMessage
		Jobs   []map[string]any
		Error  *string
	}
	var compact bytes.Buffer
	if r.code != 0 || json.Unmarshal([]byte(r.stdout), &view) != nil || json.Compact(&compact, view.Daemon) != nil || compact.String() != daemon ||
		len(view.Jobs) != 2 || view.Jobs[0]["error"] != "daemon settings unavailable: daemon is not running" || view.Error != nil {
		t.Fatal(r)
	}
	if args, _ := os.ReadFile(filepath.Join(dir, "args")); !strings.HasSuffix(strings.TrimSpace(string(args)), "jobs resolve --daemon") {
		t.Fatal(string(args))
	}

	broken := zdxJobs(t, home, "--zdx", filepath.Join(t.TempDir(), "missing"), "list", "--daemon", "--json")
	if broken.code != 0 || !strings.Contains(broken.stdout, `"status": "unavailable"`) || !strings.Contains(broken.stdout, "cannot resolve daemon settings") {
		t.Fatal(broken)
	}
}

func TestValidateReportsEveryProblem(t *testing.T) {
	home := homeWithJobs(t)
	bin, _ := fakeResolve(t, `{"daemon":null,"results":[{"settings":null,"error":"prepare job run: load subagent 'missing'"}]}`)
	r := zdxJobs(t, home, "--zdx", bin, "validate")
	if r.code != 1 || !strings.Contains(r.stderr, "2 of 2 job(s) failed validation") || !strings.Contains(r.stderr, "- explicit: prepare job run") ||
		!strings.Contains(r.stderr, "typo (") {
		t.Fatal(r)
	}
	ok := t.TempDir()
	write(t, filepath.Join(ok, "jobs", "explicit.md"), "---\nschedule: \"0 8 * * *\"\n---\nReport.\n", 0o644)
	bin, _ = fakeResolve(t, `{"daemon":null,"results":[{"settings":`+settings+`,"error":null}]}`)
	r = zdxJobs(t, ok, "--zdx", bin, "validate")
	if r.code != 0 || !strings.Contains(r.stdout, "Validated 1 job(s).") ||
		!strings.Contains(r.stdout, "- explicit (user): schedule=0 8 * * *, status=scheduled, grace_secs=600, model=fake:model@low [job], root=/caller [caller], subagent=<default>, timeout_secs=<default>, max_retries=0") {
		t.Fatal(r)
	}
}

func TestRunsFiltersByJobAndFinishedDate(t *testing.T) {
	home := t.TempDir()
	write(t, filepath.Join(home, "jobs_runs.jsonl"),
		`{"job":"morning-report","trigger":"manual","attempt":1,"max_attempts":1,"started_at":"2026-02-11T08:00:00Z","finished_at":"2026-02-11T08:00:01Z","duration_ms":1000,"ok":true,"error":null,"schedule":"0 8 * * *","model":"gemini:gemini-2.5-flash"}
{"job":"morning-report","run_id":"r2","trigger":"daemon","attempt":1,"max_attempts":1,"started_at":"2026-02-12T08:00:00+00:00","finished_at":"2026-02-12T08:00:01+00:00","duration_ms":1000,"ok":false,"outcome":"failed","error":"boom","scheduled_for":"2026-02-12T08:00:00+00:00"}
{"job":"other","trigger":"manual","attempt":1,"max_attempts":1,"started_at":"2026-02-11T08:00:00Z","finished_at":"2026-02-11T08:00:01Z","duration_ms":1,"ok":true}
`, 0o644)
	r := zdxJobs(t, home, "runs", "morning-report")
	if r.code != 0 || !strings.Contains(r.stdout, "morning-report | manual | ok | 1000ms | attempt 1/1 | thread -") ||
		!strings.Contains(r.stdout, "| daemon | failed |") || !strings.Contains(r.stdout, "  error: boom") || strings.Contains(r.stdout, "other") {
		t.Fatal(r)
	}
	r = zdxJobs(t, home, "runs", "--date", "2026-02-12", "--json")
	var records []jobs.RunRecord
	if r.code != 0 || json.Unmarshal([]byte(r.stdout), &records) != nil || len(records) != 1 || *records[0].RunID != "r2" {
		t.Fatal(r)
	}
	for args, want := range map[string]string{
		"--date-start 2026-02-12 --date-end 2026-02-11": "--date-start must be on or before --date-end",
		"--date 2026-13-01":                             "expected YYYY-MM-DD",
	} {
		if r := zdxJobs(t, home, append([]string{"runs"}, strings.Fields(args)...)...); r.code != 1 || !strings.Contains(r.stderr, want) {
			t.Errorf("%s: %+v", args, r)
		}
	}
	if r := zdxJobs(t, t.TempDir(), "runs"); r.stdout != "No job runs found.\n" {
		t.Fatal(r)
	}
}

func TestSetModelEditsOnlyTheModelLine(t *testing.T) {
	home := t.TempDir()
	path := filepath.Join(home, "jobs", "paused.md")
	original := "---\n# paused: no schedule\ntimeout_secs: 60\n---\nDo the thing.\n"
	write(t, path, original, 0o644)
	r := zdxJobs(t, home, "set-model", "paused", "claude-cli@work:claude-opus-5@high")
	if r.code != 0 || r.stdout != "paused: model = claude-cli@work:claude-opus-5@high\n" {
		t.Fatal(r)
	}
	if b, _ := os.ReadFile(path); string(b) != strings.Replace(original, "timeout_secs: 60\n---", "timeout_secs: 60\nmodel: \"claude-cli@work:claude-opus-5@high\"\n---", 1) {
		t.Fatal(string(b))
	}
	if r := zdxJobs(t, home, "set-model", "paused", "--clear"); r.code != 0 || !strings.Contains(r.stdout, "model cleared") {
		t.Fatal(r)
	}
	for _, args := range [][]string{{"set-model", "paused"}, {"set-model", "paused", "bad \"spec"}, {"set-model", "nope", "--clear"}} {
		if r := zdxJobs(t, home, args...); r.code == 0 {
			t.Errorf("%v succeeded", args)
		}
	}
	if b, _ := os.ReadFile(path); string(b) != original {
		t.Fatal(string(b))
	}
}

func TestRunExitsBusyWhileTheJobIsLocked(t *testing.T) {
	home := t.TempDir()
	write(t, filepath.Join(home, "jobs", "busy.md"), "---\n---\nWork.\n", 0o644)
	lock, err := jobs.Home{Dir: home}.TryLock("busy")
	if err != nil || lock == nil {
		t.Fatal(err)
	}
	defer lock.Release()
	r := zdxJobs(t, home, "--no-thread", "--zdx", "/bin/false", "run", "busy")
	if r.code != jobs.ExitBusy || r.stderr != "job 'busy' is already running\n" {
		t.Fatal(r)
	}
	if _, err := os.Stat(filepath.Join(home, "jobs_runs.jsonl")); !os.IsNotExist(err) {
		t.Fatal("nothing ran")
	}
}

func TestUsageErrors(t *testing.T) {
	home := t.TempDir()
	for _, args := range [][]string{{}, {"nope"}, {"--bogus", "list"}, {"list", "--bogus"}, {"run"}, {"run", "a", "--occurrence", "k"}} {
		if r := zdxJobs(t, home, args...); r.code != 2 {
			t.Errorf("%v: %+v", args, r)
		}
	}
}

func jsonEqual(a, b any) bool {
	x, _ := json.Marshal(a)
	y, _ := json.Marshal(b)
	return bytes.Equal(x, y)
}
