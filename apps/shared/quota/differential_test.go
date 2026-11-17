package quota

import (
	"bytes"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"testing"
)

// testdata/differential holds raw provider bodies (<provider>/<case>.in)
// with the output the Rust readers recorded for each (<case>.out.json). The
// outputs come from the Rust recorder kept as reference text in
// testdata/differential/recorder.rs.txt, which also names the commit and
// crate versions. They are the contract: never rewrite them from Go. Each
// case runs the same decode and parse path Fetch uses after a 2xx answer.
const differentialDir = "testdata/differential"

// recorderFile is the recorder's reference source, not a case.
const recorderFile = "recorder.rs.txt"

// acceptedDifferences are cases where Go deliberately answers differently
// from the recorded Rust output, keyed by case input. The .out.json stays the
// Rust record; each entry is listed under "Known differences from Rust" in
// AGENTS.md.
var acceptedDifferences = map[string]string{
	// Meta omits subs_usage while no rolling window is open.
	"muse-code/no-usage.in": `{
  "quota": {
    "plan": null,
    "windows": [
      {
        "label": "rolling",
        "resets_at": null,
        "scope": null,
        "used_percent": 0.0
      }
    ]
  }
}
`,
}

func differentialCases(t *testing.T, root string) map[string][]byte {
	t.Helper()
	out := map[string][]byte{}
	err := filepath.WalkDir(root, func(path string, d os.DirEntry, err error) error {
		if err != nil || d.IsDir() {
			return err
		}
		rel, _ := filepath.Rel(root, path)
		data, err := os.ReadFile(path)
		out[filepath.ToSlash(rel)] = data
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return out
}

// runDifferential is the Go twin of the Rust test's run_case.
func runDifferential(provider string, body []byte) []byte {
	var q *Quota
	var e *Error
	switch provider {
	case "claude":
		q, e = incompatibleUnless(parseClaude(body, nil))
	case "claude-profile":
		var b strings.Builder
		writeJSON(&b, jsonObject{{"plan", optText(claudePlan(body))}}, 0)
		return []byte(b.String() + "\n")
	case "codex":
		q, e = incompatibleUnless(parseCodex(body))
	case "antigravity":
		q, e = incompatibleUnless(parseAntigravity(body))
	case "grok":
		q, e = incompatibleUnless(parseGrok(body))
	case "muse-code":
		q, e = museOutcome(body)
	default:
		panic("unknown differential provider directory " + provider)
	}
	var v jsonValue
	if e != nil {
		v = jsonObject{{"error", jsonText(e.Reason())}}
	} else {
		windows := make([]jsonValue, 0, len(q.Windows))
		for _, w := range q.Windows {
			windows = append(windows, jsonObject{
				{"label", jsonText(w.Label)},
				{"resets_at", optText(formatReset(w.ResetsAt))},
				{"scope", optText(w.Scope)},
				{"used_percent", jsonNum(w.UsedPercent)},
			})
		}
		v = jsonObject{{"quota", jsonObject{{"plan", optText(q.Plan)}, {"windows", jsonArray(windows)}}}}
	}
	var b strings.Builder
	writeJSON(&b, v, 0)
	return []byte(b.String() + "\n")
}

func TestDifferentialFixturesMatchRecordedRustOutput(t *testing.T) {
	files := differentialCases(t, differentialDir)
	if _, ok := files[recorderFile]; !ok {
		t.Errorf("%s is missing: the recorded outputs need their recorder source", recorderFile)
	}
	var inputs []string
	for name := range files {
		dir, file, nested := strings.Cut(name, "/")
		switch {
		case name == recorderFile:
		case !nested || strings.Contains(file, "/"):
			t.Errorf("%s: unexpected file (cases are <provider>/<case>.in and .out.json)", name)
		case strings.HasSuffix(file, ".in"):
			inputs = append(inputs, name)
		case strings.HasSuffix(file, ".out.json"):
			if _, ok := files[dir+"/"+strings.TrimSuffix(file, ".out.json")+".in"]; !ok {
				t.Errorf("%s: recorded output without an input", name)
			}
		default:
			t.Errorf("%s: unexpected file", name)
		}
	}
	sort.Strings(inputs)
	if len(inputs) != 58 {
		t.Errorf("%d differential cases, want the 58 Rust recorded", len(inputs))
	}
	providersSeen := map[string]bool{}
	for _, in := range inputs {
		provider, _, _ := strings.Cut(in, "/")
		providersSeen[provider] = true
		want, ok := files[strings.TrimSuffix(in, ".in")+".out.json"]
		if !ok {
			t.Errorf("%s: no recorded output", in)
			continue
		}
		if accepted, ok := acceptedDifferences[in]; ok {
			want = []byte(accepted)
		}
		if got := runDifferential(provider, files[in]); !bytes.Equal(got, want) {
			t.Errorf("%s:\n--- go\n%s--- rust\n%s", in, got, want)
		}
	}
	for _, p := range []string{"claude", "claude-profile", "codex", "antigravity", "grok", "muse-code"} {
		if !providersSeen[p] {
			t.Errorf("no cases for %s", p)
		}
	}
}
