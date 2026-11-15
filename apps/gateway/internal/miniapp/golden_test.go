package miniapp

import (
	"bytes"
	"context"
	"encoding/base64"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"testing/fstest"
	"time"
)

// Golden answers of the real Rust stack for transport-level behavior, read
// from testdata/axum_*_golden.json. They were recorded from a harness built
// with the workspace's locked axum 0.8.9, tower-http 0.6.11, serde_json
// 1.0.151 and serde_urlencoded 0.7.1, whose extractors, query structs,
// layers and route table copy crates/zdx-bot/src/server.rs, with handlers
// that echo what they parsed (testdata/harness; the recorder there says how
// to re-record).
//
// They prove Go matches axum for routing, path decoding, query parsing,
// content-type and body-limit refusals, auth ordering, and compression. They
// do not prove the engine projections: the harness has no domain logic, so
// cases where server.rs adds a check (thread id rules, empty artifact path,
// the asset map) carry the expectation server.rs gives, noted per case.

type goldenCase struct {
	Name        string `json:"name"`
	Method      string `json:"method"`
	Path        string `json:"path"`
	ContentType string `json:"content_type"`
	Body        string `json:"body"`
	BodyB64     string `json:"body_b64"`
	Repeat      *struct {
		Prefix     string `json:"prefix"`
		Unit       string `json:"unit"`
		Count      int    `json:"count"`
		Suffix     string `json:"suffix"`
		SuffixUnit string `json:"suffix_unit"`
	} `json:"repeat"`
	NoAuth bool `json:"no_auth"`
}

type goldenAnswer struct {
	Name            string `json:"name"`
	Status          int    `json:"status"`
	ContentType     string `json:"content_type"`
	ContentEncoding string `json:"content_encoding"`
	Vary            string `json:"vary"`
	Allow           string `json:"allow"`
	Body            string `json:"body"`
}

func (c goldenCase) payload() []byte {
	switch {
	case c.BodyB64 != "":
		data, _ := base64.StdEncoding.DecodeString(c.BodyB64)
		return data
	case c.Repeat != nil:
		r := c.Repeat
		var buf bytes.Buffer
		buf.WriteString(r.Prefix)
		buf.WriteString(strings.Repeat(r.Unit, r.Count))
		if r.SuffixUnit != "" {
			buf.WriteString(strings.Repeat(r.SuffixUnit, r.Count))
		}
		buf.WriteString(r.Suffix)
		return buf.Bytes()
	}
	return []byte(c.Body)
}

func loadGolden[T any](t *testing.T, name string) []T {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("testdata", name))
	if err != nil {
		t.Fatal(err)
	}
	var out []T
	if err := json.Unmarshal(data, &out); err != nil {
		t.Fatalf("%s: %v", name, err)
	}
	return out
}

// echoEngine records what each route handed the engine in the harness's
// echo shape, so a golden 200 can be compared with what Go parsed.
type echoEngine struct {
	mu   sync.Mutex
	echo map[string]any
	file string
}

func (e *echoEngine) set(echo map[string]any) {
	e.mu.Lock()
	e.echo = echo
	e.mu.Unlock()
}

func (e *echoEngine) take() map[string]any {
	e.mu.Lock()
	defer e.mu.Unlock()
	echo := e.echo
	e.echo = nil
	return echo
}

func (e *echoEngine) ThreadList(_ context.Context, kind string) (json.RawMessage, error) {
	e.set(map[string]any{"kind": kind})
	return json.RawMessage(`{"threads":[]}`), nil
}

func (e *echoEngine) ThreadView(_ context.Context, id string, after *uint64) (ThreadView, error) {
	var a any
	if after != nil {
		a = float64(*after)
	}
	e.set(map[string]any{"id": id, "after": a})
	return ThreadView{Body: json.RawMessage(`{"id":"x"}`)}, nil
}

func (e *echoEngine) idOnly(id string) (json.RawMessage, error) {
	e.set(map[string]any{"id": id})
	return json.RawMessage(`{}`), nil
}

func (e *echoEngine) ThreadTrajectory(_ context.Context, id string) (json.RawMessage, error) {
	return e.idOnly(id)
}

func (e *echoEngine) ThreadWorkers(_ context.Context, id string) (json.RawMessage, error) {
	return e.idOnly(id)
}

func (e *echoEngine) ArtifactsList(_ context.Context, id string) (json.RawMessage, error) {
	return e.idOnly(id)
}

func (e *echoEngine) ArtifactResolve(_ context.Context, id, path string) (ResolvedArtifact, error) {
	e.set(map[string]any{"id": id, "path": path})
	return ResolvedArtifact{Path: e.file, Mime: "text/plain", Filename: "artifact.txt"}, nil
}

func (e *echoEngine) ArtifactCanonical(_ context.Context, id, path string) (CanonicalImage, error) {
	e.set(map[string]any{"id": id, "path": path})
	return CanonicalImage{Data: []byte("PNG"), Mime: "image/png"}, nil
}

func (e *echoEngine) ArtifactVersion(_ context.Context, id, path string) (json.RawMessage, error) {
	e.set(map[string]any{"id": id, "path": path})
	return json.RawMessage(`{}`), nil
}

func (e *echoEngine) FeedbackSubmit(_ context.Context, req FeedbackSubmit) (FeedbackResult, error) {
	e.set(map[string]any{"id": req.ThreadID, "submission": req.Submission})
	return FeedbackResult{Status: "queued"}, nil
}

func (e *echoEngine) UsageReport(_ context.Context, days int) (json.RawMessage, error) {
	e.set(map[string]any{"days": float64(days)})
	return json.RawMessage(`{}`), nil
}

func (e *echoEngine) MonitorSnapshot(context.Context) (json.RawMessage, error) {
	return json.RawMessage(`{}`), nil
}

func (e *echoEngine) JobSummaries(context.Context) (json.RawMessage, *string) {
	return json.RawMessage(`[]`), nil
}

func (e *echoEngine) ActiveAgents(context.Context) (json.RawMessage, error) {
	return json.RawMessage(`[]`), nil
}

func (e *echoEngine) SubscriptionQuotas(context.Context) (json.RawMessage, error) {
	return json.RawMessage(`[]`), nil
}

func (e *echoEngine) Workspaces(context.Context) (json.RawMessage, error) {
	return json.RawMessage(`{"workspaces":[]}`), nil
}

func (e *echoEngine) GitStatus(_ context.Context, id string) (json.RawMessage, error) {
	e.set(map[string]any{"thread_id": id})
	return json.RawMessage(`{}`), nil
}

func (e *echoEngine) GitScope(_ context.Context, id, scope string) (json.RawMessage, error) {
	e.set(map[string]any{"thread_id": id, "scope": scope})
	return json.RawMessage(`{}`), nil
}

func (e *echoEngine) GitDiff(_ context.Context, req GitDiff) (json.RawMessage, error) {
	e.set(map[string]any{"thread_id": req.ThreadID, "path": req.Path, "kind": req.Kind, "commit": req.Commit})
	return json.RawMessage(`{}`), nil
}

// serverRsAnswer is what server.rs adds on top of the harness for a case
// the harness accepted: its own input checks run after the extractors.
var serverRsAnswer = map[string][2]string{
	"thread id escaped slash":       {"400", "Invalid thread ID"},
	"empty id segment":              {"400", "Invalid thread ID"},
	"empty id workers":              {"400", "Invalid thread ID"},
	"empty id artifact file":        {"400", "Invalid thread ID"},
	"empty id feedback":             {"400", "Invalid thread ID"},
	"dot id":                        {"400", "Invalid thread ID"},
	"dotdot id":                     {"400", "Invalid thread ID"},
	"feedback bad id escaped slash": {"400", "Invalid thread ID"},
	"artifact file key only":        {"400", "Invalid artifact path"},
	// rust-embed has no such entries.
	"asset double slash":       {"404", "Not found"},
	"asset encoded dots":       {"404", "Not found"},
	"asset encoded slash dots": {"404", "Not found"},
	"asset only slash after":   {"404", "Not found"},
}

// normalize maps a harness echo to what Go hands the engine: absent
// optional values become the defaults server.rs applies before using them.
func normalize(echo map[string]any) map[string]any {
	out := map[string]any{}
	for key, value := range echo {
		out[key] = value
	}
	if _, ok := out["days"]; ok && out["days"] == nil {
		out["days"] = float64(defaultUsageDays)
	}
	for _, key := range []string{"thread_id", "commit"} {
		if value, ok := out[key]; ok && value == nil {
			out[key] = ""
		}
	}
	return out
}

func TestTransportMatchesTheRustStack(t *testing.T) {
	cases := loadGolden[goldenCase](t, "axum_cases.json")
	answers := loadGolden[goldenAnswer](t, "axum_golden.json")
	if len(cases) != len(answers) {
		t.Fatalf("%d cases, %d answers: re-record the golden file", len(cases), len(answers))
	}
	file := filepath.Join(t.TempDir(), "artifact.txt")
	if err := os.WriteFile(file, []byte("artifact"), 0o600); err != nil {
		t.Fatal(err)
	}
	engine := &echoEngine{file: file}
	assets := fstest.MapFS{
		"index.html":  {Data: []byte("shell")},
		"assets/x.js": {Data: []byte("asset")},
	}
	handler, err := New(Config{
		BotToken:       testBotToken,
		AllowedUserIDs: []int64{testUserID},
		Assets:         assets,
		Engine:         engine,
		Feedback:       gate{allow: true},
		Logger:         discardLogger(),
	})
	if err != nil {
		t.Fatal(err)
	}
	ts := &testServer{t: t, srv: newHTTPServer(t, handler)}

	for i, c := range cases {
		want := answers[i]
		if want.Name != c.Name {
			t.Fatalf("case %d is %q, answer is %q: re-record", i, c.Name, want.Name)
		}
		t.Run(c.Name, func(t *testing.T) {
			payload := c.payload()
			if len(payload) > 1<<20 && testing.Short() {
				t.Skip("large body")
			}
			headers := map[string]string{}
			if !c.NoAuth {
				headers["Authorization"] = "tma " + freshInitData(testUserID, time.Now())
			}
			if c.ContentType != "" {
				headers["Content-Type"] = c.ContentType
			}
			method := c.Method
			if method == "" {
				method = http.MethodGet
			}
			engine.take()
			got := ts.doRaw(method, c.Path, payload, headers)
			echo := engine.take()

			if override, ok := serverRsAnswer[c.Name]; ok {
				status, _ := strconv.Atoi(override[0])
				expect(t, got, status, override[1])
				return
			}
			jsonRefusal := strings.HasPrefix(want.Body, "Failed to parse the request body as JSON") ||
				strings.HasPrefix(want.Body, "Failed to deserialize the JSON body")
			switch {
			case jsonRefusal && utf8Body(payload):
				// The engine parses feedback bodies; Go must hand it the
				// exact text and not judge it.
				if echo == nil || echo["submission"] != string(payload) {
					t.Fatalf("body not handed to the engine unchanged: %v", echo)
				}
			case jsonRefusal:
				expect(t, got, want.Status, "Failed to parse the request body as JSON: invalid unicode code point")
			case want.Status != http.StatusOK:
				expect(t, got, want.Status, want.Body)
				if got.header.Get("Allow") != want.Allow {
					t.Errorf("Allow = %q, want %q", got.header.Get("Allow"), want.Allow)
				}
				if got.header.Get("Content-Type") != want.ContentType {
					t.Errorf("Content-Type = %q, want %q", got.header.Get("Content-Type"), want.ContentType)
				}
			case strings.HasSuffix(c.Path, "/feedback"):
				expect(t, got, http.StatusAccepted, "")
				if echo["submission"] != string(payload) {
					t.Fatalf("submission changed: %q", echo["submission"])
				}
			case method == http.MethodHead || !strings.HasPrefix(want.Body, "{"):
				expect(t, got, http.StatusOK, "")
				if method != http.MethodHead && want.Body == "shell" && got.body != "shell" {
					t.Fatalf("body %q, want the shell", got.body)
				}
			case strings.HasPrefix(c.Path, "/app/"):
				var parsed map[string]string
				_ = json.Unmarshal([]byte(want.Body), &parsed)
				expect(t, got, http.StatusOK, string(assets[parsed["path"]].Data))
			default:
				var parsed map[string]any
				if err := json.Unmarshal([]byte(want.Body), &parsed); err != nil {
					t.Fatal(err)
				}
				if got.status != http.StatusOK {
					t.Fatalf("status %d (%q), want 200", got.status, got.body)
				}
				wantEcho, _ := json.Marshal(normalize(parsed))
				gotEcho, _ := json.Marshal(echo)
				if string(wantEcho) != string(gotEcho) {
					t.Fatalf("engine got %s, Rust parsed %s", gotEcho, wantEcho)
				}
			}
		})
	}
}

func utf8Body(payload []byte) bool {
	return strings.ToValidUTF8(string(payload), "") == string(payload)
}

func TestCompressionMatchesTowerHTTP(t *testing.T) {
	answers := loadGolden[goldenAnswer](t, "axum_gzip_golden.json")
	type gzipCase struct {
		Name   string   `json:"name"`
		Kind   string   `json:"kind"`
		Accept []string `json:"accept"`
	}
	cases := loadGolden[gzipCase](t, "axum_gzip_cases.json")
	if len(cases) != len(answers) {
		t.Fatalf("%d cases, %d answers", len(cases), len(answers))
	}
	// The harness's /z routes.
	handler := gzipMiddleware(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		kind := strings.TrimPrefix(r.URL.Path, "/z/")
		contentType, body, status := "application/json", strings.Repeat("x", 400), http.StatusOK
		switch kind {
		case "small":
			contentType, body = "text/plain", strings.Repeat("x", 31)
		case "edge":
			contentType, body = "text/plain", strings.Repeat("x", 32)
		case "png":
			contentType = "image/png"
		case "svg":
			contentType = "image/svg+xml"
		case "upper":
			contentType = "Image/PNG"
		case "sse":
			contentType = "text/event-stream"
		case "grpc":
			contentType = "application/grpc"
		case "error":
			writeText(w, http.StatusUnauthorized, "Invalid or expired Telegram authorization")
			return
		}
		w.Header().Set("Content-Type", contentType)
		w.Header().Set("Content-Length", strconv.Itoa(len(body)))
		w.WriteHeader(status)
		_, _ = io.WriteString(w, body)
	}))
	ts := &testServer{t: t, srv: newHTTPServer(t, handler)}
	for i, c := range cases {
		want := answers[i]
		t.Run(c.Name, func(t *testing.T) {
			req, _ := http.NewRequest(http.MethodGet, ts.srv.URL+"/z/"+c.Kind, nil)
			for _, value := range c.Accept {
				req.Header.Add("Accept-Encoding", value)
			}
			transport := &http.Transport{DisableCompression: true}
			defer transport.CloseIdleConnections()
			res, err := (&http.Client{Transport: transport}).Do(req)
			if err != nil {
				t.Fatal(err)
			}
			res.Body.Close()
			if res.StatusCode != want.Status {
				t.Errorf("status %d, want %d", res.StatusCode, want.Status)
			}
			if got := res.Header.Get("Content-Encoding"); got != want.ContentEncoding {
				t.Errorf("Content-Encoding %q, want %q", got, want.ContentEncoding)
			}
			if got := strings.Join(res.Header.Values("Vary"), ", "); got != want.Vary {
				t.Errorf("Vary %q, want %q", got, want.Vary)
			}
		})
	}
}

// The engine owns feedback parsing. These Rust answers (top-level fields of
// zdx_engine FeedbackSubmission) show why Go cannot classify bodies itself:
// serde reports whichever problem comes first in the document, a type
// error (422) or a syntax error (400). Go hands every one of these bodies
// to the engine unchanged and shows the engine's code and text.
func TestFeedbackBodiesAreLeftToTheEngine(t *testing.T) {
	type jsonCase struct {
		Name string `json:"name"`
		Body string `json:"body"`
	}
	cases := loadGolden[jsonCase](t, "axum_json_cases.json")
	answers := loadGolden[goldenAnswer](t, "axum_json_golden.json")
	var submitted string
	var next error
	engine := &fakeEngine{feedback: func(_ context.Context, req FeedbackSubmit) (FeedbackResult, error) {
		submitted = req.Submission
		return FeedbackResult{Status: "queued"}, next
	}}
	ts := newTestServer(t, engine)
	for i, c := range cases {
		want := answers[i]
		t.Run(c.Name, func(t *testing.T) {
			submitted, next = "", nil
			switch want.Status {
			case http.StatusBadRequest:
				next = &EngineError{Code: CodeJSONSyntax, Message: want.Body}
			case http.StatusUnprocessableEntity:
				next = &EngineError{Code: CodeJSONData, Message: want.Body}
			}
			got := ts.do(http.MethodPost, feedbackPath, strings.NewReader(c.Body), map[string]string{
				"Authorization": "tma " + freshInitData(testUserID, time.Now()),
				"Content-Type":  "application/json",
			})
			if submitted != c.Body {
				t.Fatalf("engine got %q, want the body unchanged", submitted)
			}
			if want.Status == http.StatusOK {
				expect(t, got, http.StatusAccepted, "")
				return
			}
			expect(t, got, want.Status, want.Body)
		})
	}
}
