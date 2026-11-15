package gateway

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
)

// The Mini App's job summaries come from the shared jobs package, read in
// this process; an invalid file is listed with its error.
func TestJobSummariesReadTheHomesJobFiles(t *testing.T) {
	home := t.TempDir()
	dir := filepath.Join(home, "jobs")
	if err := os.MkdirAll(dir, 0o755); err != nil {
		t.Fatal(err)
	}
	for name, content := range map[string]string{
		"daily.md":  "---\nschedule: \"0 8 * * *\"\n---\nReport.\n",
		"broken.md": "---\nschedul: x\n---\nReport.\n",
	} {
		if err := os.WriteFile(filepath.Join(dir, name), []byte(content), 0o644); err != nil {
			t.Fatal(err)
		}
	}
	body, jobsError := (&MiniAppEngine{Home: home}).JobSummaries(context.Background())
	if jobsError != nil {
		t.Fatal(*jobsError)
	}
	var got []struct {
		Name     string          `json:"name"`
		Status   string          `json:"status"`
		Schedule json.RawMessage `json:"schedule"`
		Error    *string         `json:"error"`
	}
	if err := json.Unmarshal(body, &got); err != nil {
		t.Fatal(err)
	}
	if len(got) != 2 || got[0].Name != "broken" || got[0].Status != "invalid" || got[0].Error == nil ||
		got[1].Name != "daily" || got[1].Status != "scheduled" || string(got[1].Schedule) != `{"kind":"recurring","cron":"0 8 * * *"}` {
		t.Fatalf("%s", body)
	}
}
