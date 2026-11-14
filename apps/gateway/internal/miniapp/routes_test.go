package miniapp

import (
	"context"
	"encoding/json"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"sync/atomic"
	"testing"
	"time"
)

func TestThreadViewSetsDeliveringAroundTheEngineRead(t *testing.T) {
	tracker := &DeliveryTracker{}
	var during func()
	var gotAfter *uint64
	engine := &fakeEngine{threadView: func(id string, after *uint64) (ThreadView, error) {
		gotAfter = after
		if during != nil {
			during()
		}
		return ThreadView{SourceID: "source-thread", Body: json.RawMessage(`{"id":"` + id + `","cursor":3,"delivering":false,"activity":[{"type":"message","text":"<b>&</b>"}]}`)}, nil
	}}
	ts := newTestServer(t, engine, func(cfg *Config) { cfg.Deliveries = tracker })

	delivering := func(path string) bool {
		t.Helper()
		got := ts.get(path)
		expect(t, got, http.StatusOK, "")
		body := jsonOf(t, got.body)
		value, ok := body["delivering"].(bool)
		if !ok {
			t.Fatalf("delivering missing: %s", got.body)
		}
		return value
	}

	if delivering("/api/threads/alias-topic") {
		t.Fatal("idle thread reported delivering")
	}
	if gotAfter != nil {
		t.Fatal("first load sent an after cursor")
	}

	// Running on the alias-resolved source thread.
	done := tracker.Start("source-thread")
	if !delivering("/api/threads/alias-topic?after=2") {
		t.Fatal("running delivery on the source thread not reported")
	}
	if gotAfter == nil || *gotAfter != 2 {
		t.Fatalf("after = %v, want 2", gotAfter)
	}
	// Finished during the read: the read may hold only the pending record.
	during = done
	if !delivering("/api/threads/alias-topic") {
		t.Fatal("delivery finished during the read not reported")
	}
	during = func() { tracker.Start("source-thread")() }
	if !delivering("/api/threads/alias-topic") {
		t.Fatal("delivery started and finished during the read not reported")
	}
	during = nil
	if delivering("/api/threads/alias-topic") {
		t.Fatal("idle again but reported delivering")
	}

	// Other fields and text pass through unchanged.
	got := ts.get("/api/threads/alias-topic")
	if !strings.Contains(got.body, `"text":"<b>&</b>"`) || !strings.Contains(got.body, `"cursor":3`) {
		t.Fatalf("engine body changed: %s", got.body)
	}
}

func TestThreadRoutesRefuseBadInputBeforeTheEngine(t *testing.T) {
	engine := &fakeEngine{}
	ts := newTestServer(t, engine)
	for _, path := range []string{
		"/api/threads/..",
		"/api/threads/.",
		"/api/threads//workers",
		"/api/threads/a%2Fb",
		"/api/threads/a%5Cb",
		"/api/threads/a%00b",
		"/api/threads/a%2Fb/trajectory",
		"/api/threads/a%2Fb/workers",
		"/api/threads/a%2Fb/artifacts",
	} {
		expect(t, ts.get(path), http.StatusBadRequest, "Invalid thread ID")
	}
	expect(t, ts.get("/api/threads/x?after=-1"), http.StatusBadRequest, "")
	expect(t, ts.get("/api/threads?kind=bogus"), http.StatusBadRequest, "")
	if engine.total() != 0 {
		t.Fatalf("engine called %d times for refused input", engine.total())
	}
}

func TestThreadRoutesMapEngineAnswers(t *testing.T) {
	notFound := &EngineError{Code: CodeNotFound, Message: "Thread not found"}
	engine := &fakeEngine{
		threadList: func(kind string) (json.RawMessage, error) {
			return json.RawMessage(`{"threads":[{"id":"` + kind + `"}]}`), nil
		},
		threadView: func(string, *uint64) (ThreadView, error) { return ThreadView{}, notFound },
		trajectory: func(string) (json.RawMessage, error) { return nil, notFound },
		workers:    func(id string) (json.RawMessage, error) { return json.RawMessage(`{"workers":[]}`), nil },
		artifacts:  func(string) (json.RawMessage, error) { return nil, &EngineError{Message: "disk"} },
	}
	ts := newTestServer(t, engine)
	expect(t, ts.get("/api/threads"), http.StatusOK, `{"threads":[{"id":"all"}]}`)
	expect(t, ts.get("/api/threads?kind=workers"), http.StatusOK, `{"threads":[{"id":"workers"}]}`)
	expect(t, ts.get("/api/threads/t1"), http.StatusNotFound, "Thread not found")
	expect(t, ts.get("/api/threads/t1/trajectory"), http.StatusNotFound, "Thread not found")
	expect(t, ts.get("/api/threads/t1/workers"), http.StatusOK, `{"workers":[]}`)
	expect(t, ts.get("/api/threads/t1/artifacts"), http.StatusInternalServerError, "Artifact list failed")
}

// A missing engine capability is a 503 that says so, never an empty 200.
func TestMissingCapabilitiesAnswer503(t *testing.T) {
	ts := newTestServer(t, &fakeEngine{})
	for _, route := range apiRoutes {
		var got response
		if route.method == http.MethodPost {
			got = ts.post(route.path, `{"submission_id":"s1"}`)
		} else {
			got = ts.get(route.path)
		}
		if got.status != http.StatusServiceUnavailable {
			t.Errorf("%s %s: %d %q, want 503", route.method, route.path, got.status, got.body)
		}
	}
}

func TestArtifactFileServesOnlyWhatTheEngineResolves(t *testing.T) {
	dir := t.TempDir()
	report := filepath.Join(dir, `re"port.html`)
	if err := os.WriteFile(report, []byte("<h1>report</h1>"), 0o600); err != nil {
		t.Fatal(err)
	}
	var asked []string
	engine := &fakeEngine{resolve: func(id, path string) (ResolvedArtifact, error) {
		asked = append(asked, id+"|"+path)
		switch path {
		case "report.html":
			// The engine names the download; Go must not use the file's
			// own base name.
			return ResolvedArtifact{Path: report, Mime: "text/html", Filename: "re_port.html"}, nil
		case "relative":
			return ResolvedArtifact{Path: "report.html", Mime: "text/html", Filename: "report.html"}, nil
		case "unnamed":
			return ResolvedArtifact{Path: report, Mime: "text/html"}, nil
		case "quoted name":
			return ResolvedArtifact{Path: report, Mime: "text/html", Filename: `a"b.html`}, nil
		case "control name":
			return ResolvedArtifact{Path: report, Mime: "text/html", Filename: "a\nb.html"}, nil
		case "no type":
			return ResolvedArtifact{Path: report, Filename: "report.html"}, nil
		case "dir":
			return ResolvedArtifact{Path: dir, Mime: "text/plain", Filename: "dir"}, nil
		case "gone":
			return ResolvedArtifact{Path: filepath.Join(dir, "gone"), Mime: "text/plain", Filename: "gone"}, nil
		case "bad thread":
			return ResolvedArtifact{}, &EngineError{Code: CodeInvalid, Message: "Invalid thread ID"}
		case "broken":
			return ResolvedArtifact{}, &EngineError{Code: CodeInternal, Message: "Artifact read failed"}
		case "no code":
			return ResolvedArtifact{}, &EngineError{Message: "/private/detail"}
		}
		return ResolvedArtifact{}, &EngineError{Code: CodeArtifactNotFound, Message: "Artifact not found"}
	}}
	ts := newTestServer(t, engine)
	file := func(path string) response {
		return ts.get("/api/threads/telegram-1/artifacts/file?path=" + url.QueryEscape(path))
	}

	got := file("report.html")
	expect(t, got, http.StatusOK, "<h1>report</h1>")
	for header, want := range map[string]string{
		"Content-Type":            "text/html",
		"Content-Disposition":     `inline; filename="re_port.html"`,
		"Cache-Control":           "private, no-store",
		"X-Content-Type-Options":  "nosniff",
		"Content-Length":          "15",
		"Content-Security-Policy": "sandbox",
	} {
		if value := got.header.Get(header); value != want {
			t.Errorf("%s = %q, want %q", header, value, want)
		}
	}
	expect(t, file("/etc/hosts"), http.StatusNotFound, "Artifact not found")
	expect(t, file("bad thread"), http.StatusBadRequest, "Invalid thread ID")
	expect(t, file("broken"), http.StatusInternalServerError, "Artifact read failed")
	expect(t, file("no code"), http.StatusInternalServerError, "Artifact read failed")
	for _, path := range []string{"relative", "unnamed", "quoted name", "control name", "no type"} {
		expect(t, file(path), http.StatusInternalServerError, "Artifact read failed")
	}
	expect(t, file("dir"), http.StatusNotFound, "Artifact not found")
	expect(t, file("gone"), http.StatusNotFound, "Artifact not found")

	before := len(asked)
	expect(t, ts.get("/api/threads/telegram-1/artifacts/file?path=%20"), http.StatusBadRequest, "Invalid artifact path")
	expect(t, ts.get("/api/threads/telegram-1/artifacts/file?path=a%00b"), http.StatusBadRequest, "Invalid artifact path")
	expect(t, ts.get("/api/threads/telegram-1/artifacts/file"), http.StatusBadRequest, "")
	expect(t, ts.get("/api/threads/a%2Fb/artifacts/file?path=x"), http.StatusBadRequest, "Invalid thread ID")
	if len(asked) != before {
		t.Fatal("engine asked to resolve refused input")
	}
	if asked[0] != "telegram-1|report.html" {
		t.Fatalf("engine got %q", asked[0])
	}
}

// The version is the engine's: Go passes the body through and never hashes.
func TestArtifactVersionIsTheEngines(t *testing.T) {
	var asked []string
	body := `{"sha256":"` + strings.Repeat("ab", 32) + `","size_bytes":3,"modified_at":"2026-11-13T10:00:00.123+00:00"}`
	engine := &fakeEngine{
		version: func(id, path string) (json.RawMessage, error) {
			asked = append(asked, id+"|"+path)
			switch path {
			case "gone":
				return nil, &EngineError{Code: CodeArtifactNotFound, Message: "Artifact not found"}
			case "broken":
				return nil, &EngineError{Code: CodeInternal, Message: "Artifact version failed"}
			}
			return json.RawMessage(body), nil
		},
		resolve: func(string, string) (ResolvedArtifact, error) {
			t.Error("the version route resolved and read the file itself")
			return ResolvedArtifact{}, ErrUnavailable
		},
	}
	ts := newTestServer(t, engine)
	expect(t, ts.get("/api/threads/active/artifacts/version?path=notes.md"), http.StatusOK, body)
	expect(t, ts.get("/api/threads/t/artifacts/version?path=gone"), http.StatusNotFound, "Artifact not found")
	expect(t, ts.get("/api/threads/t/artifacts/version?path=broken"), http.StatusInternalServerError, "Artifact version failed")
	expect(t, ts.get("/api/threads/t/artifacts/version?path=%20"), http.StatusBadRequest, "Invalid artifact path")
	if len(asked) != 3 || asked[0] != "active|notes.md" {
		t.Fatalf("engine asked %v", asked)
	}
	unavailableEngine := newTestServer(t, &fakeEngine{})
	expect(t, unavailableEngine.get("/api/threads/t/artifacts/version?path=x"), http.StatusServiceUnavailable,
		"Artifact versions is not available on this server yet")
}

func TestArtifactImageHeadersErrorsAndConcurrency(t *testing.T) {
	var running, peak int32
	release := make(chan struct{})
	engine := &fakeEngine{canonical: func(ctx context.Context, _ string, path string) (CanonicalImage, error) {
		if ctx.Err() != nil {
			t.Error("canonical call received a cancellable, cancelled context")
		}
		switch path {
		case "missing":
			return CanonicalImage{}, &EngineError{Code: CodeArtifactNotFound, Message: "Artifact not found"}
		case "doc.pdf":
			return CanonicalImage{}, &EngineError{Code: CodeUnsupportedArtifact, Message: "Annotation works on PNG, JPEG, and WebP images, not PDF files."}
		case "huge":
			return CanonicalImage{}, &EngineError{Code: CodeUnsupportedArtifact, Message: "The image is larger than 40 megapixels."}
		case "bad path":
			return CanonicalImage{}, &EngineError{Code: CodeInvalid, Message: "Invalid artifact path"}
		case "busy":
			return CanonicalImage{}, &EngineError{Code: CodeBusy, Message: "Image processing is busy. Try again in a moment."}
		case "empty":
			return CanonicalImage{Mime: "image/png", SourceSHA256: "src", SHA256: "canon"}, nil
		case "svg":
			return CanonicalImage{Data: []byte("<svg/>"), Mime: "image/svg+xml"}, nil
		case "html":
			return CanonicalImage{Data: []byte("<script>"), Mime: "text/html"}, nil
		case "header":
			return CanonicalImage{Data: []byte("x"), Mime: "image/png", SHA256: "a\r\nSet-Cookie: x"}, nil
		case "slow":
			now := atomic.AddInt32(&running, 1)
			for {
				old := atomic.LoadInt32(&peak)
				if now <= old || atomic.CompareAndSwapInt32(&peak, old, now) {
					break
				}
			}
			<-release
			atomic.AddInt32(&running, -1)
		}
		return CanonicalImage{Data: []byte("PNGDATA"), Mime: "image/png", SourceSHA256: "src", SHA256: "canon", Width: 10, Height: 20, SourceWidth: 100, SourceHeight: 200}, nil
	}}
	ts := newTestServer(t, engine)

	got := ts.get("/api/threads/t/artifacts/image?path=a.png")
	expect(t, got, http.StatusOK, "PNGDATA")
	for header, want := range map[string]string{
		"Content-Type":            "image/png",
		"Cache-Control":           "private, no-store",
		"X-Content-Type-Options":  "nosniff",
		"Content-Security-Policy": "sandbox",
		"X-Zdx-Source-Sha256":     "src",
		"X-Zdx-Canonical-Sha256":  "canon",
		"X-Zdx-Canonical-Width":   "10",
		"X-Zdx-Canonical-Height":  "20",
		"X-Zdx-Source-Width":      "100",
		"X-Zdx-Source-Height":     "200",
	} {
		if value := got.header.Get(header); value != want {
			t.Errorf("%s = %q, want %q", header, value, want)
		}
	}
	expect(t, ts.get("/api/threads/t/artifacts/image?path=missing"), http.StatusNotFound, "Artifact not found")
	expect(t, ts.get("/api/threads/t/artifacts/image?path=doc.pdf"), http.StatusUnprocessableEntity, "Annotation works on PNG, JPEG, and WebP images, not PDF files.")
	expect(t, ts.get("/api/threads/t/artifacts/image?path=huge"), http.StatusUnprocessableEntity, "The image is larger than 40 megapixels.")
	expect(t, ts.get("/api/threads/t/artifacts/image?path=busy"), http.StatusServiceUnavailable, "Image processing is busy. Try again in a moment.")
	expect(t, ts.get("/api/threads/t/artifacts/image?path=bad%20path"), http.StatusBadRequest, "Invalid artifact path")
	for _, path := range []string{"empty", "svg", "html", "header"} {
		expect(t, ts.get("/api/threads/t/artifacts/image?path="+path), http.StatusInternalServerError, "Image preview failed")
	}

	var wg sync.WaitGroup
	for range 4 {
		wg.Add(1)
		go func() {
			defer wg.Done()
			expect(t, ts.get("/api/threads/t/artifacts/image?path=slow"), http.StatusOK, "")
		}()
	}
	deadline := time.Now().Add(2 * time.Second)
	for atomic.LoadInt32(&running) < imageReads && time.Now().Before(deadline) {
		time.Sleep(5 * time.Millisecond)
	}
	time.Sleep(50 * time.Millisecond)
	if got := atomic.LoadInt32(&peak); got != imageReads {
		t.Fatalf("peak concurrent canonical calls = %d, want %d", got, imageReads)
	}
	close(release)
	wg.Wait()
}

func TestDeliveryTracker(t *testing.T) {
	tracker := &DeliveryTracker{}
	if tracker.Running("t") || tracker.Finished() != 0 {
		t.Fatal("new tracker not idle")
	}
	first, second := tracker.Start("t"), tracker.Start("t")
	first()
	first()
	if !tracker.Running("t") {
		t.Fatal("second delivery should still run")
	}
	second()
	if tracker.Running("t") || tracker.Finished() != 2 {
		t.Fatalf("running=%v finished=%d, want idle after 2", tracker.Running("t"), tracker.Finished())
	}
}

func TestFormatting(t *testing.T) {
	for d, want := range map[time.Duration]string{
		42 * time.Second:                      "42s",
		12 * time.Minute:                      "12m",
		3*time.Hour + 5*time.Minute:           "3h 5m",
		50*time.Hour + 30*time.Minute:         "2d 2h",
		-time.Second:                          "0s",
		59*time.Second + 999*time.Millisecond: "59s",
	} {
		if got := formatUptime(d); got != want {
			t.Errorf("formatUptime(%v) = %q, want %q", d, got, want)
		}
	}
}

// Artifact bytes are reachable only with the Authorization header. A
// browser navigation (a link, an <iframe src>, a new tab) cannot send it, so
// it never gets an artifact rendered as a document on the app's origin, and
// no other carrier of initData is accepted. Responses that do carry bytes
// are sandboxed in case one is ever rendered.
func TestArtifactBytesNeedTheHeaderAndAreSandboxed(t *testing.T) {
	page := filepath.Join(t.TempDir(), "page.html")
	if err := os.WriteFile(page, []byte("<script>fetch('/api/threads')</script>"), 0o600); err != nil {
		t.Fatal(err)
	}
	engine := &fakeEngine{
		resolve: func(string, string) (ResolvedArtifact, error) {
			return ResolvedArtifact{Path: page, Mime: "text/html", Filename: "page.html"}, nil
		},
		canonical: func(context.Context, string, string) (CanonicalImage, error) {
			return CanonicalImage{Data: []byte("PNG"), Mime: "image/png", SourceSHA256: "s", SHA256: "c"}, nil
		},
	}
	ts := newTestServer(t, engine)
	data := freshInitData(testUserID, time.Now())
	paths := []string{"/api/threads/t/artifacts/file?path=page.html", "/api/threads/t/artifacts/image?path=a.png"}
	for _, path := range paths {
		for name, headers := range map[string]map[string]string{
			"none":         {},
			"cookie":       {"Cookie": "Authorization=tma " + url.QueryEscape(data)},
			"query":        {},
			"referer only": {"Referer": ts.srv.URL + "/app#tgWebAppData=" + url.QueryEscape(data)},
		} {
			target := path
			if name == "query" {
				target += "&tgWebAppData=" + url.QueryEscape(data) + "&authorization=" + url.QueryEscape("tma "+data)
			}
			got := ts.do(http.MethodGet, target, nil, headers)
			expect(t, got, http.StatusUnauthorized, "Telegram authorization required")
			if got.header.Get("Content-Type") != "text/plain; charset=utf-8" {
				t.Errorf("%s %s: a refusal must not be HTML: %q", path, name, got.header.Get("Content-Type"))
			}
		}
		got := ts.get(path)
		expect(t, got, http.StatusOK, "")
		if got.header.Get("Content-Security-Policy") != "sandbox" || got.header.Get("X-Content-Type-Options") != "nosniff" {
			t.Errorf("%s: headers %v", path, got.header)
		}
		if got.header.Get("Access-Control-Allow-Origin") != "" {
			t.Errorf("%s: a CORS grant would let other origins read artifacts", path)
		}
	}
	if engine.total() != 2 {
		t.Fatalf("engine called %d times, want only the 2 authorized reads", engine.total())
	}
}
