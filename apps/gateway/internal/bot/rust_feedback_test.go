package bot

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/sandbox"
)

// Mini App feedback through the real service (capability feedback_submit):
// the gateway sends the body unparsed with the chat root, Rust queues an
// unfollowed routed turn (never a command, even when the note starts with
// `/`), and the renderer bridge delivers its answer to the topic. The same
// body again is a duplicate and queues nothing.
func TestRealRustFeedbackTurnRendersThroughTheBridge(t *testing.T) {
	r := startRealRust(t, answer("FEEDBACK-ANSWER"))
	ctx := context.Background()
	if r.eng.Require(ctx, engine.CapFeedbackSubmit) != nil {
		sandbox.RustFeatureMissing(t, "the binary lacks feedback_submit")
	}
	cfg := "[telegram]\nallowlist_user_ids = [42]\nallowlist_chat_ids = [-100]\n"
	if err := os.WriteFile(filepath.Join(r.home, "config.toml"), []byte(cfg), 0o600); err != nil {
		t.Fatal(err)
	}
	r.h.Dispatch(ctx, userMessage(t, 41, "make a page"))
	r.waitFor("the first answer", 20*time.Second, func() bool { return r.sent("FEEDBACK-ANSWER") })
	r.waitFor("settled", 20*time.Second, func() bool { return r.settled(realThread) })
	art := filepath.Join(r.home, "artifacts", "threads", realThread)
	if err := os.MkdirAll(art, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(art, "report.html"), []byte("<html><body><h1>Hi</h1></body></html>"), 0o600); err != nil {
		t.Fatal(err)
	}
	path := "report.html"
	thread := realThread
	v, err := r.eng.View(ctx, engine.View{Op: engine.ViewArtifactVersion, ThreadID: &thread, Path: &path})
	if err != nil {
		t.Fatal(err)
	}
	var version struct {
		SHA256 string `json:"sha256"`
	}
	if err := json.Unmarshal(v.Body, &version); err != nil || version.SHA256 == "" {
		t.Fatalf("version %s %v", v.Body, err)
	}
	body, _ := json.Marshal(map[string]any{
		"submission_id": "sub-00000001", "artifact_path": "report.html", "artifact_sha256": version.SHA256,
		"note":        "/new looks good",
		"annotations": []any{map[string]any{"intent": "change", "comment": "Make the heading blue", "target": map[string]any{"kind": "page"}}},
	})
	before := len(r.tg.of("sendMessage"))
	submit := engine.FeedbackSubmit{ThreadID: realThread, UserID: 42, Root: r.h.RootForChat(-100), Submission: string(body)}
	reply, err := r.eng.SubmitFeedback(ctx, submit)
	if err != nil {
		t.Fatalf("feedback: %v\n%s", err, r.log())
	}
	if reply.FeedbackStatus != "queued" || reply.ThreadID != realThread || reply.InputID == nil || string(reply.Annotations) != "1" ||
		reply.ArtifactName == nil || *reply.ArtifactName != "report.html" || reply.Route == nil || reply.Route.Chat != -100 || *reply.Route.Topic != 5 {
		t.Fatalf("receipt %+v", reply)
	}
	r.waitFor("the feedback answer", 30*time.Second, func() bool {
		for _, c := range r.tg.of("sendMessage")[before:] {
			if s, _ := c.body["text"].(string); strings.HasPrefix(s, "FEEDBACK-ANSWER") && c.body["message_thread_id"] == float64(5) {
				return true
			}
		}
		return false
	})
	r.waitFor("settled", 20*time.Second, func() bool { return r.settled(realThread) })
	r.mu.Lock()
	last := r.requests[len(r.requests)-1]
	r.mu.Unlock()
	if _, text, _ := lastTurn(last); !strings.Contains(text, "Make the heading blue") || !strings.Contains(text, "/new looks good") {
		t.Fatalf("the model sees the feedback text: %.400q", text)
	}
	if n := len(r.events(realThread, "delivery")); n != 4 {
		t.Fatalf("both turns record pending and final: %d", n)
	}
	again, err := r.eng.SubmitFeedback(ctx, submit)
	if err != nil || again.FeedbackStatus != "duplicate" {
		t.Fatalf("duplicate: %+v %v", again, err)
	}
	submit.UserID = 43
	var se *engine.ServiceError
	if _, err := r.eng.SubmitFeedback(ctx, submit); !errors.As(err, &se) || se.Code != "forbidden" {
		t.Fatalf("a sender outside the allowlist: %v", err)
	}
}
