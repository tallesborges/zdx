package gateway

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// fakeZdx writes a zdx stand-in: settings from settingsJSON, and a queued
// restart that is done and asks the bot to restart.
func fakeZdx(t *testing.T, settingsJSON string) string {
	dir := t.TempDir()
	path := filepath.Join(dir, "zdx")
	script := `#!/bin/sh
case "$*" in
*"telegram settings --include-token"*) cat <<'JSON'
` + settingsJSON + `
JSON
;;
*"service run-queued-restart --json"*) echo '{"status":"done","restart_bot":true,"restarted":["scheduler"],"thread_id":"telegram--100-topic-5","message":"Restarted scheduler"}' ;;
*) echo "unexpected $*" >&2; exit 3 ;;
esac
`
	if err := os.WriteFile(path, []byte(script), 0o755); err != nil {
		t.Fatal(err)
	}
	return path
}

func serveTelegram(t *testing.T) (*httptest.Server, func() []string) {
	var mu sync.Mutex
	var sent []string
	tg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		raw, _ := io.ReadAll(r.Body)
		switch method {
		case "getUpdates":
			select {
			case <-r.Context().Done():
			case <-time.After(20 * time.Millisecond):
			}
			io.WriteString(w, `{"ok":true,"result":[]}`)
		case "sendMessage":
			mu.Lock()
			sent = append(sent, string(raw))
			mu.Unlock()
			io.WriteString(w, `{"ok":true,"result":{"message_id":1,"chat":{"id":-100,"type":"supergroup"},"date":1}}`)
		default:
			io.WriteString(w, `{"ok":true,"result":true}`)
		}
	}))
	t.Cleanup(tg.Close)
	return tg, func() []string { mu.Lock(); defer mu.Unlock(); return append([]string(nil), sent...) }
}

// The service start path, isolated: pidfile first, settings from the zdx
// CLI, the supervision marker under launchd, a second instance refused,
// and a queued restart that announces and exits with ErrExitRequested
// after releasing the pidfile.
func TestServeLifecycleIsolated(t *testing.T) {
	home, err := os.MkdirTemp("", "gws")
	if err != nil {
		t.Fatal(err)
	}
	defer os.RemoveAll(home)
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	tg, sent := serveTelegram(t)
	zdx := fakeZdx(t, `{"bot_token":"`+token+`","allowlist_user_ids":[7],"allowlist_chat_ids":[-100],"server":{"enabled":false,"port":4141},"root":"/r","profiles":[]}`)
	opts := ServeOptions{
		Home: home, Root: home, Zdx: zdx, Socket: f.socket, Log: quiet, QueuedRestartEvery: 20 * time.Millisecond,
		Getenv:   func(k string) string { return map[string]string{SupervisorEnv: "launchd"}[k] },
		Telegram: telegram.Options{BaseURL: tg.URL, Logger: quiet},
	}
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	done := make(chan error, 1)
	go func() { done <- Serve(ctx, opts) }()
	waitUntil(t, "the supervision marker", 5*time.Second, func() bool { return Supervised(home) })
	if err := Serve(ctx, opts); err == nil || !strings.Contains(err.Error(), "already running") {
		t.Fatalf("a second gateway must be refused: %v", err)
	}

	os.WriteFile(filepath.Join(home, "run", "restart-queue.json"), []byte("{}"), 0o600)
	select {
	case err := <-done:
		if !errors.Is(err, ErrExitRequested) {
			t.Fatalf("a done queued restart exits for launchd: %v", err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("no exit")
	}
	if s := sent(); len(s) != 1 || !strings.Contains(s[0], "Restarted scheduler") || !strings.Contains(s[0], `"message_thread_id":5`) {
		t.Fatalf("the restart is announced in its thread: %v", s)
	}
	if _, err := os.Stat(filepath.Join(home, "run", "bot.pid")); !os.IsNotExist(err) {
		t.Fatal("the pidfile is released on exit")
	}
}

// With the Mini App enabled and no build, the gateway refuses to start
// (never a shell that answers 500) and holds no pidfile afterwards.
func TestServeRefusesTheMiniAppWithoutAssets(t *testing.T) {
	home := t.TempDir()
	zdx := fakeZdx(t, `{"bot_token":"`+token+`","allowlist_user_ids":[7],"allowlist_chat_ids":[],"server":{"enabled":true,"port":4141},"root":"/r","profiles":[]}`)
	err := Serve(context.Background(), ServeOptions{Home: home, Root: home, Zdx: zdx, WebDist: filepath.Join(home, "nodist"), Log: quiet})
	if err == nil || !strings.Contains(err.Error(), "index.html") {
		t.Fatalf("unexpected %v", err)
	}
	if _, err := os.Stat(filepath.Join(home, "run", "bot.pid")); !os.IsNotExist(err) {
		t.Fatal(fmt.Sprint("the pidfile is released: ", err))
	}
}
