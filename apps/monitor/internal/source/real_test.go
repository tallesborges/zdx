package source_test

import (
	"context"
	"os"
	"path/filepath"
	"testing"

	"github.com/tallesborges/zdx/apps/monitor/internal/source"
)

// TestRealZdxShapes runs a prebuilt zdx (ZDX_MONITOR_TEST_ZDX, never built
// here) in the sandbox home, with no service running there, and decodes
// every surface the monitor reads through the CLI: views via `zdx views`,
// service status, and scheduler jobs (which `zdx` runs with the prebuilt
// zdx-go, ZDX_MONITOR_TEST_GO). It checks that the Go types match what the
// programs write.
func TestRealZdxShapes(t *testing.T) {
	bin, goBin := os.Getenv("ZDX_MONITOR_TEST_ZDX"), os.Getenv("ZDX_MONITOR_TEST_GO")
	if bin == "" || goBin == "" {
		t.Skip("set ZDX_MONITOR_TEST_ZDX to a prebuilt zdx binary and ZDX_MONITOR_TEST_GO to a prebuilt zdx-go")
	}
	t.Setenv("ZDX_GO", goBin)
	home := os.Getenv("ZDX_HOME")
	root := tempDir(t, "r")
	if err := os.MkdirAll(filepath.Join(home, "jobs"), 0o700); err != nil {
		t.Fatal(err)
	}
	job := "---\nschedule: \"0 8 * * *\"\n---\nReport.\n"
	if err := os.WriteFile(filepath.Join(home, "jobs", "daily-report.md"), []byte(job), 0o600); err != nil {
		t.Fatal(err)
	}
	s := source.New(source.Config{Root: root, Home: home, Zdx: bin, CLIFallback: true})
	ctx := context.Background()

	snap, origin, err := s.Snapshot(ctx)
	if err != nil || origin != source.OriginCLI {
		t.Fatalf("snapshot: %v %v", origin, err)
	}
	if len(snap.Services) == 0 || snap.Config.Model == "" {
		t.Fatalf("snapshot %+v", snap)
	}
	if _, _, err := s.ActiveAgents(ctx); err != nil {
		t.Fatalf("agents: %v", err)
	}
	if _, _, err := s.Threads(ctx, "all"); err != nil {
		t.Fatalf("threads: %v", err)
	}
	if u, _, err := s.Usage(ctx, 7); err != nil || u.Days != 7 {
		t.Fatalf("usage: %+v %v", u, err)
	}
	if w, _, err := s.Workspaces(ctx); err != nil || len(w.Workspaces) == 0 || len(w.Workspaces[0].Settings) == 0 {
		t.Fatalf("workspaces: %+v %v", w, err)
	}
	svc, err := s.Services(ctx)
	if err != nil || len(svc.Services) == 0 || svc.Services[0].Label == "" {
		t.Fatalf("services: %+v %v", svc, err)
	}
	jobs, err := s.Jobs(ctx)
	if err != nil || len(jobs.Jobs) != 1 || jobs.Jobs[0].Name != "daily-report" || jobs.Jobs[0].Schedule == nil ||
		jobs.Jobs[0].Schedule.Kind != "recurring" || jobs.Scheduler.Status == "" {
		t.Fatalf("jobs: %+v %v", jobs, err)
	}
}
