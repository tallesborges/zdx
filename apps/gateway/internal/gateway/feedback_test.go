package gateway

import (
	"bufio"
	"context"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/miniapp"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Feedback goes out with the root of the requested thread's chat (the bot
// root when the id names no chat), never one from the body; queued
// feedback posts the Rust bot's notice to the reply route, a duplicate
// posts nothing.
func TestFeedbackSendsTheChatRootAndPostsTheNotice(t *testing.T) {
	dir, _ := os.MkdirTemp("", "gwfb")
	defer os.RemoveAll(dir)
	sock := filepath.Join(dir, "o.sock")
	ln, err := net.Listen("unix", sock)
	if err != nil {
		t.Fatal(err)
	}
	defer ln.Close()
	var mu sync.Mutex
	var sent []map[string]any
	reply := `{"reply":"feedback","status":"queued","thread_id":"telegram--100-topic-5","input_id":3,"annotations":2,"stale":false,"artifact_name":"a<b>.html","route":{"chat":-100,"topic":5}}`
	go func() {
		for {
			conn, err := ln.Accept()
			if err != nil {
				return
			}
			line, _ := bufio.NewReader(conn).ReadBytes('\n')
			var req map[string]any
			json.Unmarshal(line, &req)
			if req["op"] == "status" {
				conn.Write([]byte(`{"reply":"status","report":{"protocol":2,"capabilities":["feedback_submit"],"settled":true,"threads":[]}}` + "\n"))
			} else {
				mu.Lock()
				sent = append(sent, req)
				r := reply
				mu.Unlock()
				conn.Write([]byte(r + "\n"))
			}
			conn.Close()
		}
	}()
	var notices []map[string]any
	tg := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		raw, _ := io.ReadAll(r.Body)
		var body map[string]any
		json.Unmarshal(raw, &body)
		mu.Lock()
		notices = append(notices, body)
		mu.Unlock()
		io.WriteString(w, `{"ok":true,"result":{"message_id":9,"chat":{"id":-100,"type":"supergroup"},"date":1}}`)
	}))
	defer tg.Close()
	settings := Settings{Root: "/bot", Profiles: []Profile{{Name: "p", ChatID: -100, Cwd: "/project"}}}
	e := &MiniAppEngine{Client: engine.NewClient(sock), RootForChat: settings.RootForChat, Root: settings.Root,
		Telegram: telegram.NewClient("1:T", telegram.Options{BaseURL: tg.URL, Logger: quiet}), Log: quiet}
	ctx := context.Background()
	body := `{"root":"/evil","annotations":[]}`
	res, err := e.FeedbackSubmit(ctx, miniapp.FeedbackSubmit{ThreadID: "telegram--100-topic-5", UserID: 7, Submission: body})
	if err != nil || res.Status != "queued" || res.Annotations != 2 || *res.InputID != 3 {
		t.Fatalf("%+v %v", res, err)
	}
	mu.Lock()
	reply = `{"reply":"feedback","status":"duplicate","thread_id":"t","annotations":2,"stale":false}`
	mu.Unlock()
	if _, err := e.FeedbackSubmit(ctx, miniapp.FeedbackSubmit{ThreadID: "not-telegram", UserID: 7, Submission: body}); err != nil {
		t.Fatal(err)
	}
	mu.Lock()
	defer mu.Unlock()
	if sent[0]["root"] != "/project" || sent[0]["submission"] != body || sent[0]["user_id"] != float64(7) || sent[1]["root"] != "/bot" {
		t.Fatalf("requests %v", sent)
	}
	if len(notices) != 1 || notices[0]["text"] != "📝 Feedback on <b>a&lt;b&gt;.html</b> from the Mini App (2 notes)." ||
		notices[0]["chat_id"] != float64(-100) || notices[0]["message_thread_id"] != float64(5) {
		t.Fatalf("notices %v", notices)
	}
	if got := FeedbackNotice("x", 1); !strings.HasSuffix(got, "(1 note).") {
		t.Fatal(got)
	}
}
