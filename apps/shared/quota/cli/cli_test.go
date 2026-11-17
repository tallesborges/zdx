package cli

import (
	"bytes"
	"context"
	"encoding/json"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/shared/quota"
	"github.com/tallesborges/zdx/apps/shared/sandbox"
)

func TestMain(m *testing.M) { os.Exit(sandbox.Main(m.Run)) }

const testVersion = "0.0.0-test"

// env is a synthetic environment: only ZDX_HOME is set, so no real home is seen.
func env(home string, extra map[string]string) func(string) (string, bool) {
	return func(k string) (string, bool) {
		if k == "ZDX_HOME" {
			return home, true
		}
		v, ok := extra[k]
		return v, ok
	}
}

func runQuota(t *testing.T, base quota.Options, args ...string) (int, string, string) {
	t.Helper()
	var stdout, stderr bytes.Buffer
	code := run(context.Background(), args, &stdout, &stderr, base)
	return code, stdout.String(), stderr.String()
}

// The exact stdout of the Rust `zdx quota --json` on a home without
// credentials (serde_json pretty, sorted keys).
const emptyJSON = `{
  "providers": [
    {
      "account": null,
      "error": "not logged in",
      "provider": "claude-cli"
    },
    {
      "account": null,
      "error": "not logged in",
      "provider": "openai-codex"
    },
    {
      "account": null,
      "error": "not logged in",
      "provider": "google-antigravity"
    },
    {
      "account": null,
      "error": "not logged in",
      "provider": "grok-build"
    },
    {
      "account": null,
      "error": "not logged in",
      "provider": "muse-code"
    }
  ]
}
`

func TestEmptyHomeReportsEveryProviderNotLoggedIn(t *testing.T) {
	base := quota.Options{LookupEnv: env(t.TempDir(), nil)}
	code, out, errOut := runQuota(t, base, "--root", "/r", "--zdx-version", testVersion, "--json")
	if code != 0 || errOut != "" {
		t.Fatalf("exit %d: %s", code, errOut)
	}
	if out != emptyJSON {
		t.Fatalf("json differs from the Rust output:\n%s", out)
	}

	code, out, _ = runQuota(t, base, "--zdx-version", testVersion)
	want := "Subscriptions (live quota)\n" +
		"  Claude   not logged in\n  Codex   not logged in\n  Antigravity   not logged in\n" +
		"  Grok   not logged in\n  Muse Code   not logged in\n"
	if code != 0 || out != want {
		t.Fatalf("exit %d, text:\n%s", code, out)
	}
}

func TestMissingVersionFailsBeforeAnyRequest(t *testing.T) {
	srv := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		t.Error("no request may be sent without a version")
	}))
	defer srv.Close()
	base := quota.Options{
		LookupEnv: env(t.TempDir(), nil),
	}
	code, out, errOut := runQuota(t, base, "--json")
	if code != 1 || out != "" || !strings.Contains(errOut, "zdx version unknown") || !strings.Contains(errOut, "--zdx-version") {
		t.Fatalf("exit %d, stdout %q, stderr %q", code, out, errOut)
	}
}

func TestUnreadableAccountStoreFailsWithoutLeakingIt(t *testing.T) {
	const secret = "sk-test-SECRET-0123456789"
	cases := map[string]func(path string){
		"malformed": func(path string) {
			os.WriteFile(path, []byte(`{"claude-cli": {"type":"oauth","access":"`+secret+`"`), 0o600)
		},
		"incomplete entry": func(path string) {
			os.WriteFile(path, []byte(`{"claude-cli": {"type":"oauth","access":"`+secret+`"}}`), 0o600)
		},
		"unreadable": func(path string) {
			os.WriteFile(path, []byte(`{}`), 0o600)
			os.Chmod(path, 0o000)
		},
	}
	for name, write := range cases {
		t.Run(name, func(t *testing.T) {
			home := t.TempDir()
			path := filepath.Join(home, "oauth.json")
			write(path)
			t.Cleanup(func() { os.Chmod(path, 0o600) })
			if name == "unreadable" {
				if f, err := os.Open(path); err == nil {
					f.Close()
					t.Skip("running with permission to read a 0000 file")
				}
			}
			code, out, errOut := runQuota(t, quota.Options{LookupEnv: env(home, nil)}, "--zdx-version", testVersion, "--json")
			if code != 1 || out != "" {
				t.Fatalf("a discovery failure must fail, not print healthy data: exit %d, stdout %q", code, out)
			}
			if !strings.Contains(errOut, "account discovery failed") || strings.Contains(errOut, secret) {
				t.Fatalf("stderr %q", errOut)
			}
		})
	}
}

func TestReadsStoredAccountsReadOnly(t *testing.T) {
	const access = "test-access-token-AAAA"
	const expiredAccess = "test-access-token-EXPIRED"
	var mu sync.Mutex
	var auths []string
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		mu.Lock()
		auths = append(auths, r.URL.Path+" "+r.Header.Get("Authorization"))
		mu.Unlock()
		if r.URL.Path != "/usage" {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		w.Write([]byte(`{"five_hour":{"utilization":12.5,"resets_at":"2026-07-14T01:09:59+00:00"},"seven_day":{"utilization":45.0,"resets_at":null}}`))
	}))
	defer srv.Close()

	home := t.TempDir()
	far := time.Now().Add(time.Hour).UnixMilli()
	store := `{"claude-cli":{"type":"oauth","refresh":"test-refresh","access":"` + access + `","expires":` + jsonInt(far) + `},` +
		`"claude-cli@work":{"type":"oauth","refresh":"test-refresh-2","access":"` + expiredAccess + `","expires":1}}`
	path := filepath.Join(home, "oauth.json")
	if err := os.WriteFile(path, []byte(store), 0o600); err != nil {
		t.Fatal(err)
	}
	before, _ := os.Stat(path)

	base := quota.Options{
		LookupEnv: env(home, nil),
		HTTP:      srv.Client(),
		Endpoints: quota.Endpoints{
			ClaudeUsage: srv.URL + "/usage", ClaudeProfile: srv.URL + "/profile",
			ClaudeToken: srv.URL + "/token", CodexToken: srv.URL + "/token/codex",
			AntigravityToken: srv.URL + "/token/ag", GrokToken: srv.URL + "/token/grok",
		},
	}
	code, out, errOut := runQuota(t, base, "--zdx-version", testVersion, "--json")
	if code != 0 {
		t.Fatalf("exit %d: %s", code, errOut)
	}
	if strings.Contains(out+errOut, access) || strings.Contains(out+errOut, expiredAccess) || strings.Contains(out+errOut, "test-refresh") {
		t.Fatal("a token reached the output")
	}
	var got struct {
		Providers []struct {
			Provider string
			Account  *string
			Error    *string
			Windows  []struct {
				Label       string
				UsedPercent float64 `json:"used_percent"`
				ResetsAt    *string `json:"resets_at"`
			}
		}
	}
	if err := json.Unmarshal([]byte(out), &got); err != nil {
		t.Fatal(err)
	}
	if len(got.Providers) != 6 || got.Providers[0].Provider != "claude-cli" || got.Providers[1].Provider != "claude-cli" {
		t.Fatalf("default account first, then named, then the other providers: %s", out)
	}
	def, work := got.Providers[0], got.Providers[1]
	if def.Account != nil || def.Error != nil || len(def.Windows) != 2 ||
		def.Windows[0].UsedPercent != 12.5 || def.Windows[1].UsedPercent != 45 ||
		def.Windows[0].ResetsAt == nil || *def.Windows[0].ResetsAt != "2026-07-14T01:09:59+00:00" || def.Windows[1].ResetsAt != nil {
		t.Fatalf("default account: %s", out)
	}
	if work.Account == nil || *work.Account != "work" || work.Error == nil || *work.Error != "expired · re-login in zdx" {
		t.Fatalf("expired account: %s", out)
	}
	if !strings.Contains(out, `"used_percent": 45.0`) {
		t.Fatalf("whole percents keep the serde_json `.0`: %s", out)
	}
	mu.Lock()
	defer mu.Unlock()
	for _, a := range auths {
		if strings.Contains(a, expiredAccess) {
			t.Fatalf("an expired token was sent: %v", auths)
		}
	}
	after, _ := os.Stat(path)
	if !after.ModTime().Equal(before.ModTime()) || after.Size() != before.Size() {
		t.Fatal("oauth.json changed: failed refresh must not write credentials")
	}
	// oauth.lock is created when a refresh is attempted; it may exist.
	// A failed refresh leaves oauth.json untouched (checked above).
}

func TestRejectsUnknownArguments(t *testing.T) {
	code, _, errOut := runQuota(t, quota.Options{LookupEnv: env(t.TempDir(), nil)}, "--zdx-version", testVersion, "extra")
	if code != 2 || !strings.Contains(errOut, "unexpected argument") {
		t.Fatalf("exit %d: %s", code, errOut)
	}
}

func jsonInt(v int64) string {
	b, _ := json.Marshal(v)
	return string(b)
}
