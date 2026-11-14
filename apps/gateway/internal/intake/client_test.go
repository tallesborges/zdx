package intake

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"path/filepath"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// fakeBotAPI serves getFile and file fetches like Telegram: "flaky" answers
// 502 on its first fetch, "down" always, "huge" is over the download limit.
func fakeBotAPI(t *testing.T) (*httptest.Server, func() int) {
	t.Helper()
	var mu sync.Mutex
	getFiles := 0
	fetches := map[string]int{}
	mux := http.NewServeMux()
	mux.HandleFunc("/bot"+fakeToken+"/getFile", func(w http.ResponseWriter, r *http.Request) {
		var body struct {
			FileID string `json:"file_id"`
		}
		_ = json.NewDecoder(r.Body).Decode(&body)
		mu.Lock()
		getFiles++
		mu.Unlock()
		_ = json.NewEncoder(w).Encode(map[string]any{"ok": true, "result": map[string]any{"file_id": body.FileID, "file_path": "voice/" + body.FileID + ".oga"}})
	})
	mux.HandleFunc("/file/bot"+fakeToken+"/", func(w http.ResponseWriter, r *http.Request) {
		path := strings.TrimPrefix(r.URL.Path, "/file/bot"+fakeToken+"/")
		mu.Lock()
		fetches[path]++
		n := fetches[path]
		mu.Unlock()
		switch {
		case strings.Contains(path, "flaky") && n == 1, strings.Contains(path, "down"):
			w.WriteHeader(http.StatusBadGateway)
		case strings.Contains(path, "huge"):
			_, _ = w.Write(make([]byte, TelegramDownloadLimitBytes+1))
		default:
			_, _ = w.Write([]byte("ogg-bytes:" + path))
		}
	})
	srv := httptest.NewServer(mux)
	t.Cleanup(srv.Close)
	return srv, func() int { mu.Lock(); defer mu.Unlock(); return getFiles }
}

// The collector works against the real Bot API client: retries a 502,
// holds the message when the server keeps failing, turns an oversized body
// into a notice, and keeps the token out of results, errors and logs.
func TestCollectThroughTelegramClient(t *testing.T) {
	srv, getFiles := fakeBotAPI(t)
	var logs bytes.Buffer
	logger := slog.New(slog.NewTextHandler(&logs, &slog.HandlerOptions{Level: slog.LevelDebug}))
	client := telegram.NewClient(fakeToken, telegram.Options{BaseURL: srv.URL, Logger: logger})
	root := filepath.Join(t.TempDir(), "telegram")
	var sleeps []time.Duration
	c := &Collector{
		Files:       client,
		Store:       DirStore{Root: root},
		Transcriber: &fakeTranscriber{},
		Images:      &fakeImages{},
		Log:         logger,
		Sleep:       func(_ context.Context, d time.Duration) error { sleeps = append(sleeps, d); return nil },
	}

	r, err := c.Collect(context.Background(), msg(t, 80, `"caption":"listen","voice":{"file_id":"flaky","mime_type":"audio/ogg"}`), nil)
	if err != nil {
		t.Fatal(err)
	}
	if len(r.Compose.Audios) != 1 || r.Compose.Audios[0].Path != filepath.Join(root, "42", "80_flaky.oga") || getFiles() != 2 || len(sleeps) != 1 {
		t.Fatalf("result %+v, getFile %d, sleeps %v", r, getFiles(), sleeps)
	}

	_, held := c.Collect(context.Background(), msg(t, 81, `"voice":{"file_id":"down"}`), nil)
	var unavailable *DownloadUnavailable
	if !errors.As(held, &unavailable) {
		t.Fatalf("err %v", held)
	}

	r, err = c.Collect(context.Background(), msg(t, 82, `"document":{"file_id":"huge","mime_type":"application/zip"}`), nil)
	if err != nil || len(r.Compose.Notices) != 1 || len(r.Compose.Documents) != 0 {
		t.Fatalf("result %+v, err %v", r, err)
	}

	compose, _ := json.Marshal(r.Compose)
	for name, text := range map[string]string{"logs": logs.String(), "error": held.Error(), "compose": string(compose)} {
		if strings.Contains(text, fakeToken) || strings.Contains(text, "SYNTHETIC-TOKEN") {
			t.Fatalf("token in %s: %s", name, text)
		}
	}
}
