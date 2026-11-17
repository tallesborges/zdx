package source_test

import (
	"context"
	"errors"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sync/atomic"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/monitor/internal/source"
	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/version"
)

const testVersion = "9.9.9-test"

// quotaSource is a Source whose quota fetch reads home and sends every
// provider request to handler. No variable of the test process is read.
func quotaSource(t *testing.T, home, zdxVersion string, handler http.HandlerFunc) (*source.Source, *atomic.Int32) {
	t.Helper()
	var calls atomic.Int32
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		calls.Add(1)
		handler(w, r)
	}))
	t.Cleanup(srv.Close)
	u := func(path string) string { return srv.URL + path }
	// Token refresh endpoints point at a separate server that always
	// rejects, so expired tokens stay expired in tests without touching
	// the handler's call counter or real network.
	tokenSrv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusBadRequest)
	}))
	t.Cleanup(tokenSrv.Close)
	tu := func(path string) string { return tokenSrv.URL + path }
	return source.New(source.Config{Home: home, ZdxVersion: zdxVersion, Quota: quota.Options{
		HTTP:      srv.Client(),
		LookupEnv: func(string) (string, bool) { return "", false },
		Now:       func() time.Time { return time.Unix(1_700_000_000, 0) },
		Endpoints: quota.Endpoints{
			ClaudeUsage: u("/claude/usage"), ClaudeProfile: u("/claude/profile"), CodexUsage: u("/codex"),
			AntigravityQuota: u("/antigravity"), GrokBilling: u("/grok"), MuseCodeKey: u("/muse"),
			ClaudeToken: tu("/token/claude"), CodexToken: tu("/token/codex"),
			AntigravityToken: tu("/token/antigravity"), GrokToken: tu("/token/grok"),
		},
	}}), &calls
}

func unexpected(t *testing.T) http.HandlerFunc {
	return func(w http.ResponseWriter, r *http.Request) {
		t.Errorf("unexpected request %s %s", r.Method, r.URL.Path)
		w.WriteHeader(http.StatusInternalServerError)
	}
}

func writeOAuth(t *testing.T, home, body string) string {
	t.Helper()
	path := filepath.Join(home, "oauth.json")
	if err := os.WriteFile(path, []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
	return path
}

func TestQuotasNeedAKnownVersion(t *testing.T) {
	if version.Zdx != "" {
		t.Skip("built with a version linker flag")
	}
	home := tempDir(t, "q")
	writeOAuth(t, home, `{"openai-codex":{"type":"oauth","refresh":"r","access":"a","expires":4102444800000,"account_id":"acct-1"}}`)
	s, calls := quotaSource(t, home, "", unexpected(t))
	subs, origin, err := s.Quotas(context.Background())
	if !errors.Is(err, version.ErrUnknown) || subs != nil || origin != source.OriginDirect {
		t.Fatalf("want ErrUnknown and no rows, got %v %v %q", subs, err, origin)
	}
	if calls.Load() != 0 {
		t.Fatal("no provider may be asked without a version")
	}
}

func TestQuotasReadEveryAccountWithTheVersion(t *testing.T) {
	home := tempDir(t, "q")
	path := writeOAuth(t, home, `{
		"openai-codex": {"type":"oauth","refresh":"r-1","access":"access-1","expires":4102444800000,"account_id":"acct-1"},
		"claude-cli@work": {"type":"oauth","refresh":"r-2","access":"access-2","expires":1}
	}`)
	before, err := os.Stat(path)
	if err != nil {
		t.Fatal(err)
	}
	s, _ := quotaSource(t, home, testVersion, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/codex" {
			unexpected(t)(w, r)
			return
		}
		if got := r.Header.Get("User-Agent"); got != "zdx/"+testVersion {
			t.Errorf("user agent %q", got)
		}
		if got := r.Header.Get("Authorization"); got != "Bearer access-1" {
			t.Errorf("authorization %q", got)
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"plan_type":"pro","rate_limit":{"primary_window":{"used_percent":42.5,"limit_window_seconds":18000,"reset_at":1700003600}}}`))
	})
	subs, origin, err := s.Quotas(context.Background())
	if err != nil || origin != source.OriginDirect {
		t.Fatalf("got %v %q", err, origin)
	}
	if len(subs) != 2 {
		t.Fatalf("want two rows, got %+v", subs)
	}
	claude, codex := subs[0], subs[1]
	if claude.Name != "Claude @work" || claude.Error == nil || *claude.Error != "expired · re-login in zdx" || claude.Windows == nil || len(claude.Windows) != 0 {
		t.Fatalf("expired account %+v", claude)
	}
	if codex.Name != "Codex" || codex.Plan == nil || *codex.Plan != "pro" || codex.Error != nil || len(codex.Windows) != 1 {
		t.Fatalf("codex %+v", codex)
	}
	if w := codex.Windows[0]; w.Label != "5h" || w.UsedPercent != 42.5 || w.ResetsAt == nil || *w.ResetsAt != "2023-11-14T23:13:20+00:00" {
		t.Fatalf("window %+v (resets %v)", w, deref(w.ResetsAt))
	}
	after, err := os.Stat(path)
	if err != nil || after.ModTime() != before.ModTime() || after.Size() != before.Size() {
		t.Fatalf("oauth.json must stay unchanged: %v", err)
	}
}

func TestQuotasBadOAuthIsAnError(t *testing.T) {
	home := tempDir(t, "q")
	writeOAuth(t, home, `{`)
	s, calls := quotaSource(t, home, testVersion, unexpected(t))
	subs, _, err := s.Quotas(context.Background())
	if !errors.Is(err, quota.ErrDiscovery) || subs != nil {
		t.Fatalf("want ErrDiscovery and no rows, got %v %v", subs, err)
	}
	if calls.Load() != 0 {
		t.Fatal("no provider may be asked")
	}
}

func TestQuotasCancelIsAnError(t *testing.T) {
	home := tempDir(t, "q")
	writeOAuth(t, home, `{"openai-codex":{"type":"oauth","refresh":"r","access":"a","expires":4102444800000,"account_id":"acct-1"}}`)
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	s, _ := quotaSource(t, home, testVersion, func(w http.ResponseWriter, r *http.Request) {
		cancel()
		<-r.Context().Done()
	})
	subs, _, err := s.Quotas(ctx)
	if !errors.Is(err, context.Canceled) || subs != nil {
		t.Fatalf("want context.Canceled and no rows, got %v %v", subs, err)
	}
}

func deref(s *string) string {
	if s == nil {
		return "<nil>"
	}
	return *s
}
