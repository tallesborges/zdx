package telegram

import (
	"context"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

func downloadServer(t *testing.T, h http.HandlerFunc) *Client {
	srv := httptest.NewServer(h)
	t.Cleanup(srv.Close)
	return NewClient(testToken, Options{BaseURL: srv.URL, Clock: newFakeClock(), Logger: slog.New(slog.DiscardHandler)})
}

func TestDownloadFileFetchesTheBotFilePath(t *testing.T) {
	var path string
	c := downloadServer(t, func(w http.ResponseWriter, r *http.Request) {
		path = r.URL.Path
		io.WriteString(w, "bytes")
	})
	data, err := c.DownloadFile(context.Background(), "photos/file_1.jpg", 0)
	if err != nil || string(data) != "bytes" || path != "/file/bot"+testToken+"/photos/file_1.jpg" {
		t.Fatalf("data=%q err=%v path=%s", data, err, path)
	}
}

func TestDownloadFileStopsAtTheLimit(t *testing.T) {
	big := strings.Repeat("x", 100)
	declared := downloadServer(t, func(w http.ResponseWriter, r *http.Request) { io.WriteString(w, big) })
	if _, err := declared.DownloadFile(context.Background(), "a/b", 10); !errors.Is(err, ErrFileTooLarge) {
		t.Fatalf("want ErrFileTooLarge, got %v", err)
	}
	streamed := downloadServer(t, func(w http.ResponseWriter, r *http.Request) {
		w.(http.Flusher).Flush() // chunked: no Content-Length
		io.WriteString(w, big)
	})
	if _, err := streamed.DownloadFile(context.Background(), "a/b", 10); !errors.Is(err, ErrFileTooLarge) {
		t.Fatalf("want ErrFileTooLarge without a length, got %v", err)
	}
	if data, err := streamed.DownloadFile(context.Background(), "a/b", 100); err != nil || len(data) != 100 {
		t.Fatalf("exactly the limit is fine: %d %v", len(data), err)
	}
}

func TestDownloadFileRefusesRedirectsAndBadPaths(t *testing.T) {
	var other int
	target := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) { other++ }))
	defer target.Close()
	c := downloadServer(t, func(w http.ResponseWriter, r *http.Request) {
		http.Redirect(w, r, target.URL+r.URL.Path, http.StatusFound)
	})
	_, err := c.DownloadFile(context.Background(), "a/b", 0)
	if err == nil || other != 0 || strings.Contains(err.Error(), "SECRET") {
		t.Fatalf("redirect must be refused without leaking: err=%v other=%d", err, other)
	}
	for _, bad := range []string{"", "/etc/passwd", "../x", "a/../../x", "a//b", "a?b", "a#b", "a%2e%2e/b", "a\\b", "a\nb"} {
		if _, err := c.DownloadFile(context.Background(), bad, 0); err == nil {
			t.Errorf("%q must be refused", bad)
		} else {
			var transport *TransportError
			var api *APIError
			if !errors.Is(err, ErrInvalidFilePath) || errors.As(err, &transport) || errors.As(err, &api) {
				t.Errorf("%q: a refused path is a definitive ErrInvalidFilePath, got %T %v", bad, err, err)
			}
		}
	}
}

func TestDownloadFileErrorsCarryNoToken(t *testing.T) {
	c := downloadServer(t, func(w http.ResponseWriter, r *http.Request) { http.Error(w, "gone "+r.URL.Path, http.StatusNotFound) })
	_, err := c.DownloadFile(context.Background(), "a/b", 0)
	var api *APIError
	if !errors.As(err, &api) || api.Code != 404 || strings.Contains(err.Error(), "SECRET") {
		t.Fatalf("got %v", err)
	}
	srv := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {}))
	url := srv.URL
	srv.Close()
	down := NewClient(testToken, Options{BaseURL: url, Clock: newFakeClock(), Logger: slog.New(slog.DiscardHandler)})
	if _, err := down.DownloadFile(context.Background(), "a/b", 0); err == nil || strings.Contains(err.Error(), "SECRET") || strings.Contains(err.Error(), "/file/bot") {
		t.Fatalf("got %v", err)
	}
}

func TestForumTopicCalls(t *testing.T) {
	var bodies []string
	c := downloadServer(t, func(w http.ResponseWriter, r *http.Request) {
		b, _ := io.ReadAll(r.Body)
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		bodies = append(bodies, method+" "+string(b))
		if method == "createForumTopic" {
			io.WriteString(w, `{"ok":true,"result":{"message_thread_id":77,"name":"n"}}`)
			return
		}
		io.WriteString(w, `{"ok":true,"result":true}`)
	})
	ctx := context.Background()
	if id, err := c.CreateForumTopic(ctx, -100, "n"); err != nil || id != 77 {
		t.Fatalf("create: %d %v", id, err)
	}
	if err := c.PinChatMessage(ctx, -100, 5); err != nil {
		t.Fatal(err)
	}
	if err := c.DeleteForumTopic(ctx, -100, 77); err != nil {
		t.Fatal(err)
	}
	want := []string{
		`createForumTopic {"chat_id":-100,"name":"n"}`,
		`pinChatMessage {"chat_id":-100,"message_id":5,"disable_notification":true}`,
		`deleteForumTopic {"chat_id":-100,"message_thread_id":77}`,
	}
	if strings.Join(bodies, "\n") != strings.Join(want, "\n") {
		t.Fatalf("calls:\n%s", strings.Join(bodies, "\n"))
	}
}
