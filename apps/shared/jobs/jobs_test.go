package jobs

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

func writeFile(t *testing.T, path, content string) string {
	t.Helper()
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(path, []byte(content), 0o644); err != nil {
		t.Fatal(err)
	}
	return path
}

func readFile(t *testing.T, path string) string {
	t.Helper()
	b, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	return string(b)
}

func parse(t *testing.T, content string) (Definition, error) {
	t.Helper()
	return ParseContent(filepath.Join(t.TempDir(), "demo.md"), content)
}

func TestParseMinimalJobDefaults(t *testing.T) {
	def, err := parse(t, "---\n---\nGenerate morning report from recent threads.")
	if err != nil {
		t.Fatal(err)
	}
	if def.Name != "demo" || def.Schedule.Kind != Manual || def.Model != nil || def.Subagent != nil ||
		def.MaxRetries != 0 || def.Prompt != "Generate morning report from recent threads." {
		t.Fatalf("%+v", def)
	}
}

func TestParseReadsEveryKey(t *testing.T) {
	def, err := parse(t, "\ufeff---\r\n# a comment\r\nschedule: \"*/5 8-10 * * 1-5\" # weekdays\r\nmodel: claude-cli:claude-opus-5@high\r\nsubagent: 'explorer'\r\nroot: \"~/work/space\"\r\ntimeout_secs: 900\r\nmax_retries: 2\r\ngrace_secs: ~\r\n...\r\n\r\n  Prompt body.  \r\n")
	if err != nil {
		t.Fatal(err)
	}
	if def.Schedule.Kind != Recurring || def.Schedule.Cron != "*/5 8-10 * * 1-5" || *def.Model != "claude-cli:claude-opus-5@high" ||
		*def.Subagent != "explorer" || *def.Root != "~/work/space" || *def.TimeoutSecs != 900 || def.MaxRetries != 2 ||
		def.GraceSecs != nil || def.Prompt != "Prompt body." {
		t.Fatalf("%+v", def)
	}
	if g, _ := def.EffectiveGraceSecs(); g != DefaultRecurringGraceSecs {
		t.Fatalf("grace %d", g)
	}
}

func TestParseRejectsBadFiles(t *testing.T) {
	cases := map[string]string{
		"no frontmatter":                                  "Missing YAML frontmatter",
		"---\nmodel: a\n":                                 "Unterminated YAML frontmatter",
		"---\nschedul: x\n---\np":                         "unknown field `schedul`",
		"---\nrun_at: \"2030-01-02T09:00:00\"\n---\np":    "explicit offset",
		"---\nrun_at: \"2030-01-02T09:00:00.5Z\"\n---\np": "whole seconds",
		"---\nschedule: \"0 9 * * *\"\nrun_at: \"2030-01-02T09:00:00Z\"\n---\np": "not both",
		"---\nschedule: \"0 25 * * *\"\n---\np":                                  "out of range",
		"---\ngrace_secs: 600\n---\np":                                           "grace_secs needs",
		"---\nschedule: \"0 9 * * *\"\ngrace_secs: 30\n---\np":                   "between 60 and 604800",
		"---\ntimeout_secs: 0\n---\np":                                           "greater than zero",
		"---\nmax_retries: \"2\"\n---\np":                                        "non-negative integer",
		"---\nmax_retries: -1\n---\np":                                           "non-negative integer",
		"---\nmodel: \"  \"\n---\np":                                             "model cannot be empty",
		"---\nmodel: a\nmodel: b\n---\np":                                        "duplicate key",
		"---\nmodel: >\n  a\n---\np":                                             "only plain or quoted",
		"---\nmodel:\n  - a\n---\np":                                             "nested or indented",
		"---\nmodel: [a]\n---\np":                                                "only plain or quoted",
		"---\nmodel: \"a\n---\np":                                                "unterminated",
		"---\nmodel: a: b\n---\np":                                               "only plain or quoted",
		"---\n---\n   \n":                                                        "prompt body cannot be empty",
	}
	for content, want := range cases {
		_, err := parse(t, content)
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%q: got %v, want %q", content, err, want)
		}
	}
}

func TestParseKeepsAnEmptyRootForResolution(t *testing.T) {
	def, err := parse(t, "---\nroot: \"  \"\n---\nprompt")
	if err != nil || def.Root == nil || *def.Root != "" {
		t.Fatalf("%+v %v", def, err)
	}
}

func TestCatalogListsValidJobsAndIsolatesInvalidFiles(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	dir := home.JobsDir()
	writeFile(t, filepath.Join(dir, "report.md"), "---\nschedule: \"0 8 * * *\"\n---\nprompt")
	writeFile(t, filepath.Join(dir, "remind.MD"), "---\nrun_at: \"2026-10-20T09:00:00-03:00\"\ngrace_secs: 3600\n---\nprompt")
	writeFile(t, filepath.Join(dir, "typo.md"), "---\nschedul: \"0 8 * * *\"\n---\nprompt")
	writeFile(t, filepath.Join(dir, "bad-cron.md"), "---\nschedule: \"0 25 * * *\"\n---\nprompt")
	writeFile(t, filepath.Join(dir, "notes.txt"), "not a job")
	writeFile(t, filepath.Join(dir, "archive", "old--20261001T120000Z.md"), "---\n---\nprompt")

	cat, err := ReadCatalog(home)
	if err != nil {
		t.Fatal(err)
	}
	var valid, invalid []string
	for _, j := range cat.Jobs {
		valid = append(valid, j.Name)
	}
	for _, j := range cat.Invalid {
		invalid = append(invalid, j.Name)
	}
	if strings.Join(valid, ",") != "remind,report" || strings.Join(invalid, ",") != "bad-cron,typo" {
		t.Fatalf("valid %v invalid %v", valid, invalid)
	}
	if _, err := cat.Find("typo"); err == nil || !strings.Contains(err.Error(), "unknown field") {
		t.Fatal(err)
	}
	if _, err := cat.Find("missing"); err == nil || !strings.Contains(err.Error(), "not found") {
		t.Fatal(err)
	}
	remind, _ := cat.Find("remind")
	if g, _ := remind.EffectiveGraceSecs(); g != 3600 {
		t.Fatal(g)
	}
	if cat, err := ReadCatalog(Home{Dir: filepath.Join(t.TempDir(), "none")}); err != nil || len(cat.Jobs) != 0 {
		t.Fatal("a missing jobs dir has no jobs")
	}
}

func TestOnceJobStateFollowsItsClaimAndTheClock(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	writeFile(t, filepath.Join(home.JobsDir(), "remind.md"), "---\nrun_at: \"2026-10-20T12:00:00Z\"\n---\nprompt")
	job, err := LoadByName(home, "remind")
	if err != nil {
		t.Fatal(err)
	}
	at := func(h, m int) time.Time { return time.Date(2026, 10, 20, h, m, 0, 0, time.UTC) }
	state := StateOf(home, job, at(11, 0), time.UTC)
	if state.Status != StatusPending || !state.NextRunAt.Equal(at(12, 0)) {
		t.Fatalf("%+v", state)
	}
	if s := StateOf(home, job, at(13, 0), time.UTC).Status; s != StatusDue {
		t.Fatal(s)
	}
	if s := StateOf(home, job, at(18, 1), time.UTC).Status; s != StatusExpired {
		t.Fatal(s)
	}
	claim := NewClaim("remind", "20261020T120000Z", TriggerManual, Once, at(12, 0))
	if _, err := home.CreateClaim(claim); err != nil {
		t.Fatal(err)
	}
	if s := StateOf(home, job, at(13, 0), time.UTC).Status; s != StatusInterrupted {
		t.Fatal(s)
	}
	lock, _ := home.TryLock("remind")
	if s := StateOf(home, job, at(13, 0), time.UTC).Status; s != StatusRunning {
		t.Fatal(s)
	}
	lock.Release()
	finished := claim.Finished(Succeeded, nil)
	if err := home.UpdateClaim(finished); err != nil {
		t.Fatal(err)
	}
	// A crash before `last.json` was written: the claim still shows.
	state = StateOf(home, job, at(13, 0), time.UTC)
	if state.Status != StatusFinished || state.NextRunAt != nil || state.LastRun.RunID != finished.RunID {
		t.Fatalf("%+v", state)
	}
}

func TestSummariesSortValidAndInvalidByName(t *testing.T) {
	home := Home{Dir: t.TempDir()}
	writeFile(t, filepath.Join(home.JobsDir(), "b.md"), "---\nschedule: \"0 8 * * *\"\n---\nprompt")
	writeFile(t, filepath.Join(home.JobsDir(), "a.md"), "---\nschedule: \"nope\"\n---\nprompt")
	writeFile(t, filepath.Join(home.JobsDir(), "c.md"), "---\n---\nprompt")
	got, err := Summaries(home, time.Now(), time.UTC)
	if err != nil {
		t.Fatal(err)
	}
	if len(got) != 3 || got[0].Name != "a" || got[0].Status != StatusInvalid || got[0].Schedule != nil ||
		got[1].Status != StatusScheduled || got[1].NextRunAt == nil || got[2].Status != StatusManual {
		t.Fatalf("%+v", got)
	}
}

const editFixture = "---\n# owner: example\nroot: \"~/work\"   # keep me\nmodel: claude-cli:claude-opus-5@high # current pick\nsubagent: helper\nmax_retries: 1\n---\n\nBody line one.\n\nmodel: not frontmatter\n"

func TestSetModelReplacesOnlyTheModelLine(t *testing.T) {
	path := writeFile(t, filepath.Join(t.TempDir(), "job.md"), editFixture)
	updated, err := SetModel(path, ptr("claude-cli@work:claude-sonnet-5@low@fast"))
	if err != nil || *updated.Model != "claude-cli@work:claude-sonnet-5@low@fast" {
		t.Fatal(err)
	}
	want := strings.Replace(editFixture, "model: claude-cli:claude-opus-5@high # current pick",
		"model: \"claude-cli@work:claude-sonnet-5@low@fast\" # current pick", 1)
	if got := readFile(t, path); got != want {
		t.Fatalf("%q", got)
	}
}

func TestSetModelClearThenSetRoundTrips(t *testing.T) {
	path := writeFile(t, filepath.Join(t.TempDir(), "job.md"), editFixture)
	cleared, err := SetModel(path, nil)
	if err != nil || cleared.Model != nil {
		t.Fatal(err)
	}
	without := strings.Replace(editFixture, "model: claude-cli:claude-opus-5@high # current pick\n", "", 1)
	if got := readFile(t, path); got != without {
		t.Fatalf("%q", got)
	}
	if _, err := SetModel(path, ptr("stepfun:step-3.7-flash@low")); err != nil {
		t.Fatal(err)
	}
	want := strings.Replace(without, "max_retries: 1\n---", "max_retries: 1\nmodel: \"stepfun:step-3.7-flash@low\"\n---", 1)
	if got := readFile(t, path); got != want {
		t.Fatalf("%q", got)
	}
}

func TestSetModelKeepsCRLFLineEndings(t *testing.T) {
	path := writeFile(t, filepath.Join(t.TempDir(), "job.md"), "---\r\ntimeout_secs: 60\r\n---\r\nPrompt\r\n")
	if _, err := SetModel(path, ptr("gemini:gemini-3.8-flash")); err != nil {
		t.Fatal(err)
	}
	if got := readFile(t, path); got != "---\r\ntimeout_secs: 60\r\nmodel: \"gemini:gemini-3.8-flash\"\r\n---\r\nPrompt\r\n" {
		t.Fatalf("%q", got)
	}
}

func TestSetModelRefusesInvalidEditsAndLeavesTheFileIntact(t *testing.T) {
	dir := t.TempDir()
	path := writeFile(t, filepath.Join(dir, "job.md"), editFixture)
	for spec, want := range map[string]string{"a:b \"x": "invalid model spec", "  ": "model cannot be empty"} {
		if _, err := SetModel(path, ptr(spec)); err == nil || !strings.Contains(err.Error(), want) {
			t.Errorf("%q: %v", spec, err)
		}
		if readFile(t, path) != editFixture {
			t.Fatal("file changed")
		}
	}
	block := "---\nmodel: >\n  claude-cli:claude-opus-5\n---\nPrompt\n"
	blockPath := writeFile(t, filepath.Join(dir, "block.md"), block)
	if _, err := SetModel(blockPath, ptr("a:b")); err == nil || readFile(t, blockPath) != block {
		t.Fatal("a block scalar must not be edited")
	}
	entries, _ := os.ReadDir(dir)
	for _, e := range entries {
		if strings.HasSuffix(e.Name(), ".tmp") {
			t.Fatalf("leftover %s", e.Name())
		}
	}
}
