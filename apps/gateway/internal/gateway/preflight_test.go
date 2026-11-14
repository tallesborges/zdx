package gateway

import (
	"context"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
)

// Preflight changes nothing and reports each gap: a Mini App without its
// build, and every capability the service lacks.
func TestPreflightReportsWithoutSideEffects(t *testing.T) {
	home, err := os.MkdirTemp("", "gwp")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(home)
	f := &fakeService{caps: []string{engine.CapRecordDelivery}}
	startService(t, f)
	zdx := fakeZdx(t, `{"bot_token":"`+token+`","allowlist_user_ids":[7],"allowlist_chat_ids":[],"server":{"enabled":true,"port":4141},"root":"/r","profiles":[]}`)
	r := Preflight(context.Background(), PreflightOptions{Home: home, Root: home, Zdx: zdx, Socket: f.socket, WebDist: filepath.Join(home, "nodist")})
	text := strings.Join(r.Problems, "\n")
	if r.Ready || r.Service != "bot" || !strings.Contains(text, "index.html") ||
		!strings.Contains(text, "lacks capability "+engine.CapReplyParts) || !strings.Contains(text, engine.CapHoldTokens) ||
		!strings.Contains(text, "lacks capability "+engine.CapCatalog) || strings.Contains(text, "lacks capability "+engine.CapRecordDelivery) {
		t.Fatalf("unexpected %+v", r)
	}
	if strings.Contains(text, "SECRET") {
		t.Fatal("the token never appears")
	}
	entries, _ := os.ReadDir(home)
	if len(entries) != 0 {
		t.Fatalf("preflight writes nothing in the home: %v", entries)
	}

	// An unreachable service is a warning: it may be installed later.
	r = Preflight(context.Background(), PreflightOptions{Home: home, Root: home, Zdx: zdx, Socket: filepath.Join(home, "none.sock"), WebDist: filepath.Join(home, "nodist")})
	if len(r.Warnings) != 1 || strings.Contains(strings.Join(r.Problems, "\n"), "lacks capability") {
		t.Fatalf("unexpected %+v", r)
	}
}

// Preflight names the engine binary it checked and refuses a service whose
// views read another root (or none), or an engine path that is no program.
func TestPreflightChecksViewsRootAndEnginePath(t *testing.T) {
	home, err := os.MkdirTemp("", "gwp")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(home)
	root := filepath.Join(home, "project")
	os.MkdirAll(root, 0o700)
	f := &fakeService{caps: ServiceCapabilities}
	startService(t, f)
	zdx := fakeZdx(t, `{"bot_token":"`+token+`","allowlist_user_ids":[7],"allowlist_chat_ids":[],"server":{"enabled":false,"port":4141},"root":"/r","profiles":[]}`)
	run := func(o PreflightOptions) PreflightResult {
		o.Home, o.Socket = home, f.socket
		if o.Root == "" {
			o.Root = root
		}
		if o.Zdx == "" {
			o.Zdx = zdx
		}
		return Preflight(context.Background(), o)
	}
	if r := run(PreflightOptions{}); r.Ready || !strings.Contains(strings.Join(r.Problems, "\n"), "no views_root") {
		t.Fatalf("a service without views_root: %+v", r)
	}
	f.mu.Lock()
	f.viewsRoot = filepath.Join(home, "other")
	f.mu.Unlock()
	if r := run(PreflightOptions{}); r.Ready || !strings.Contains(strings.Join(r.Problems, "\n"), "not "+root) {
		t.Fatalf("another root: %+v", r)
	}
	// The same directory through a symlink and a trailing slash matches.
	link := filepath.Join(home, "link")
	os.Symlink(root, link)
	f.mu.Lock()
	f.viewsRoot = link + "/"
	f.mu.Unlock()
	r := run(PreflightOptions{ZdxFrom: "ZDX_BIN"})
	if !r.Ready || r.Zdx != zdx || len(r.Warnings) != 0 {
		t.Fatalf("same root: %+v", r)
	}
	if r := run(PreflightOptions{ZdxFrom: "PATH"}); !r.Ready || len(r.Warnings) != 1 || !strings.Contains(r.Warnings[0], "PATH") {
		t.Fatalf("an engine found on PATH is a warning: %+v", r)
	}
	if r := run(PreflightOptions{Zdx: home}); r.Ready || !strings.Contains(strings.Join(r.Problems, "\n"), "not an executable") {
		t.Fatalf("a directory is no engine: %+v", r)
	}
}
