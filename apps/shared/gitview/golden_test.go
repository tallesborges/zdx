package gitview

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

// goldenFile holds the replies the retired Rust Git views gave for
// viewCases on the synthetic world, recorded from a Rust service before
// they were removed. They are the contract and cannot be recorded again:
// never write them from Go or by hand.
const goldenFile = "testdata/rust_views.json"

func TestMain(m *testing.M) {
	os.Exit(sandbox.Main(func() int {
		// The views run git with this process's environment: keep user and
		// system config out, as the fixtures do.
		os.Setenv("GIT_CONFIG_GLOBAL", "/dev/null")
		os.Setenv("GIT_CONFIG_NOSYSTEM", "1")
		return m.Run()
	}))
}

type golden struct {
	Recorder string       `json:"recorder"`
	Git      string       `json:"git"`
	Cases    []goldenCase `json:"cases"`
}

type goldenCase struct {
	Name     string          `json:"name"`
	Fallback string          `json:"fallback"`
	View     string          `json:"view"`
	Reply    json.RawMessage `json:"reply"`
}

// viewRequest is the part of a view line the Go views take.
type viewRequest struct {
	Op       string  `json:"op"`
	ThreadID string  `json:"thread_id"`
	Scope    string  `json:"scope"`
	Path     string  `json:"path"`
	Kind     string  `json:"kind"`
	Commit   *string `json:"commit"`
}

// locateFor is what the Rust thread_root view answers for these threads:
// the id itself (no alias), its stored root, and the views root.
func (w *world) locateFor(id, fallback string) Locate {
	return func(context.Context) (Selection, error) {
		sel := Selection{ThreadID: &id, EffectiveID: &id, FallbackRoot: w.path(fallback)}
		if root, ok := threads[id]; ok {
			p := w.path(root)
			sel.RootPath = &p
		}
		return sel, nil
	}
}

// runGo answers one case with this package, in the recorded reply form.
func (w *world) runGo(t *testing.T, c viewCase) []byte {
	t.Helper()
	var req viewRequest
	if err := json.Unmarshal([]byte(w.expand(c.view)), &req); err != nil {
		t.Fatalf("%s: %v", c.name, err)
	}
	locate := w.locateFor(req.ThreadID, c.fallback)
	ctx := context.Background()
	var body any
	var err error
	switch req.Op {
	case "git_status":
		body, err = Status(ctx, locate)
	case "git_scope":
		body, err = Scope(ctx, locate, req.Scope)
	case "git_diff":
		body, err = Diff(ctx, locate, DiffRequest{Path: req.Path, Kind: req.Kind, Commit: req.Commit})
	default:
		t.Fatalf("%s: op %q", c.name, req.Op)
	}
	if err != nil {
		var ve *Error
		if !errors.As(err, &ve) {
			t.Fatalf("%s: untyped error %v", c.name, err)
		}
		if ve.Code == CodeInternal {
			t.Logf("%s: internal cause: %v", c.name, ve.Err)
		}
		return refusal(ve.Code, ve.Message)
	}
	encoded, err := Encode(body)
	if err != nil {
		t.Fatal(err)
	}
	return []byte(`{"body":` + string(encoded) + `}`)
}

func refusal(code, message string) []byte {
	b, _ := json.Marshal(map[string]string{"code": code, "message": message})
	return b
}

// normalize makes a reply comparable across runs: the world directory
// becomes $WORLD and relative commit times $RELATIVE. Keys are sorted.
func (w *world) normalize(t *testing.T, raw []byte) []byte {
	t.Helper()
	var value any
	dec := json.NewDecoder(bytes.NewReader(raw))
	dec.UseNumber()
	if err := dec.Decode(&value); err != nil {
		t.Fatalf("not JSON: %v: %.300s", err, raw)
	}
	var clean func(key string, v any) any
	clean = func(key string, v any) any {
		switch v := v.(type) {
		case map[string]any:
			for k, inner := range v {
				v[k] = clean(k, inner)
			}
		case []any:
			for i := range v {
				v[i] = clean("", v[i])
			}
		case string:
			if key == "relative_time" {
				return "$RELATIVE"
			}
			v = strings.ReplaceAll(v, w.dir, "$WORLD")
			if key == "content" && len(v) > 4096 {
				// A long diff is kept as its digest, so the file stays small
				// and the comparison exact.
				sum := sha256.Sum256([]byte(v))
				return fmt.Sprintf("$SHA256 %x, %d bytes, ends %q", sum, len(v), v[len(v)-48:])
			}
			return v
		}
		return v
	}
	var buf bytes.Buffer
	enc := json.NewEncoder(&buf)
	enc.SetEscapeHTML(false)
	if err := enc.Encode(clean("", value)); err != nil {
		t.Fatal(err)
	}
	return bytes.TrimSpace(buf.Bytes())
}

func readGolden(t *testing.T) golden {
	t.Helper()
	data, err := os.ReadFile(goldenFile)
	if err != nil {
		t.Fatal(err)
	}
	var g golden
	if err := json.Unmarshal(data, &g); err != nil {
		t.Fatal(err)
	}
	return g
}

// TestGoldenRustViews runs every case with this package and compares it
// with the reply Rust recorded. Every case must be recorded, and every
// recorded case must still be a case.
func TestGoldenRustViews(t *testing.T) {
	g := readGolden(t)
	recorded := map[string]goldenCase{}
	for _, c := range g.Cases {
		recorded[c.Name] = c
	}
	if len(recorded) != len(viewCases) || len(g.Cases) != len(viewCases) {
		t.Fatalf("%s has %d cases (%d names), the test %d: recorded cases are fixed, test new behavior outside viewCases", goldenFile, len(g.Cases), len(recorded), len(viewCases))
	}
	w := newWorld(t)
	ran := 0
	for _, c := range viewCases {
		t.Run(c.name, func(t *testing.T) {
			rec, ok := recorded[c.name]
			if !ok || rec.View != c.view || rec.Fallback != c.fallback {
				t.Fatalf("case %s is not recorded as it is defined (recorded %+v)", c.name, rec)
			}
			want := w.normalize(t, rec.Reply)
			got := w.normalize(t, w.runGo(t, c))
			if !bytes.Equal(got, want) {
				t.Fatalf("Go differs from the recorded Rust reply\n  go: %.2000s\nrust: %.2000s", got, want)
			}
			ran++
		})
	}
	if ran != len(viewCases) {
		t.Fatalf("ran %d of %d cases", ran, len(viewCases))
	}
}
