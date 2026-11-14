package gateway

import (
	"context"
	"encoding/json"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/bot"
	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// confirmingTelegram models getUpdates confirmation: a poll with offset N
// confirms (drops) every update below N; unconfirmed ones are sent again.
type confirmingTelegram struct {
	mu      sync.Mutex
	pending []int64 // update ids not confirmed, ascending
	texts   map[int64]string
	seen    map[int64]int // how often each update was returned
	srv     *httptest.Server
	// confirmed counts how often a poll without a timeout (Confirm) returned
	// each update: a long poll its client gave up on can still read them.
	confirmed map[int64]int
}

func newConfirmingTelegram(t *testing.T) *confirmingTelegram {
	c := &confirmingTelegram{texts: map[int64]string{}, seen: map[int64]int{}, confirmed: map[int64]int{}}
	c.srv = httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		method := r.URL.Path[strings.LastIndex(r.URL.Path, "/")+1:]
		raw, _ := io.ReadAll(r.Body)
		if method != "getUpdates" {
			if method == "sendMessage" {
				io.WriteString(w, `{"ok":true,"result":{"message_id":900,"chat":{"id":-100,"type":"supergroup"},"date":1}}`)
				return
			}
			io.WriteString(w, `{"ok":true,"result":true}`)
			return
		}
		var body struct {
			Offset  *int64 `json:"offset"`
			Timeout int64  `json:"timeout"`
		}
		json.Unmarshal(raw, &body)
		c.mu.Lock()
		if body.Offset != nil {
			kept := c.pending[:0]
			for _, id := range c.pending {
				if id >= *body.Offset {
					kept = append(kept, id)
				}
			}
			c.pending = kept
		}
		var out []string
		for _, id := range c.pending {
			c.seen[id]++
			if body.Timeout == 0 {
				c.confirmed[id]++
			}
			out = append(out, fmt.Sprintf(`{"update_id":%d,"message":{"message_id":%d,"chat":{"id":-100,"type":"supergroup","is_forum":true},"message_thread_id":5,"from":{"id":7,"is_bot":false},"text":%q}}`, id, id, c.texts[id]))
		}
		c.mu.Unlock()
		if len(out) == 0 && body.Timeout > 0 {
			select {
			case <-r.Context().Done():
			case <-time.After(20 * time.Millisecond):
			}
		}
		io.WriteString(w, `{"ok":true,"result":[`+strings.Join(out, ",")+`]}`)
	}))
	t.Cleanup(c.srv.Close)
	return c
}

func (c *confirmingTelegram) push(id int64, text string) {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.pending = append(c.pending, id)
	c.texts[id] = text
}

func (c *confirmingTelegram) confirmPolls(id int64) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.confirmed[id]
}

func (c *confirmingTelegram) times(id int64) int {
	c.mu.Lock()
	defer c.mu.Unlock()
	return c.seen[id]
}

// countingSink counts dispatched messages by text; it runs no turn.
type countingSink struct {
	mu  sync.Mutex
	got map[string]int
}

func (s *countingSink) Dispatch(_ context.Context, in bot.Incoming) {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.got[*in.Message.Text]++
}
func (s *countingSink) HandleCallback(context.Context, *telegram.CallbackQuery) {}
func (s *countingSink) RenderTurn(context.Context, bot.RoutedTurn)              {}

func (s *countingSink) count(text string) int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.got[text]
}

func runCounting(t *testing.T, c *confirmingTelegram, socket string, sink *countingSink) (context.CancelFunc, chan error) {
	settings := Settings{BotToken: token, AllowlistUserIDs: []int64{7}, AllowlistChatIDs: []int64{-100}, Root: "/r"}
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan error, 1)
	go func() {
		done <- Run(ctx, Config{Settings: settings, Socket: socket, Log: quiet, DrainTimeout: 5 * time.Second,
			Telegram: telegram.Options{BaseURL: c.srv.URL, Logger: quiet},
			Build: func(Deps) (Components, error) {
				return Components{Messages: sink, Callbacks: sink, Renderer: sink}, nil
			}})
	}()
	return cancel, done
}

// Cutover, stop old then start new: the old gateway confirms what it
// handled, so the new one does not get it again; an update that arrived
// during the stop is not handled by the old one and reaches the new one.
func TestCutoverStopOldThenStartNewHandsEachUpdateOnce(t *testing.T) {
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	c := newConfirmingTelegram(t)
	c.push(7, "before")
	old := &countingSink{got: map[string]int{}}
	stopOld, oldDone := runCounting(t, c, f.socket, old)
	waitUntil(t, "the old gateway to take update 7", 5*time.Second, func() bool { return old.count("before") == 1 })
	c.push(8, "during")
	stopOld()
	if err := <-oldDone; err != nil {
		t.Fatal(err)
	}

	next := &countingSink{got: map[string]int{}}
	stopNext, nextDone := runCounting(t, c, f.socket, next)
	waitUntil(t, "the new gateway to take update 8", 5*time.Second, func() bool { return next.count("during") == 1 })
	stopNext()
	<-nextDone
	if next.count("before") != 0 || old.count("before") != 1 {
		t.Fatalf("update 7 is handled once: old %d new %d", old.count("before"), next.count("before"))
	}
	if old.count("during")+next.count("during") != 1 {
		t.Fatalf("update 8 is handled once: old %d new %d", old.count("during"), next.count("during"))
	}
}

// crashAfterFirstPoll forwards one poll, then never reaches Telegram
// again: the process dies before its next poll would confirm the batch.
type crashAfterFirstPoll struct {
	src   bot.UpdateSource
	polls int
}

func (c *crashAfterFirstPoll) GetUpdates(ctx context.Context, offset *int64, timeout time.Duration) ([]telegram.Update, error) {
	c.polls++
	if c.polls > 1 {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	return c.src.GetUpdates(ctx, offset, timeout)
}

// The documented duplicate window: Telegram confirms a batch only
// with the next poll. A gateway that dies before that poll (crash, kill)
// leaves the batch unconfirmed, so the next one gets it again. No
// update-id dedupe exists, as in the Rust bot. (The other side: work
// handed off and confirmed by the next poll but not yet submitted is lost
// in a crash.)
func TestCrashRedeliversTheLastBatch(t *testing.T) {
	c := newConfirmingTelegram(t)
	c.push(7, "before")
	tg := telegram.NewClient(token, telegram.Options{BaseURL: c.srv.URL, Logger: quiet})
	crashed := &countingSink{got: map[string]int{}}
	ctx, cancel := context.WithCancel(context.Background())
	p := &bot.Poller{Source: &crashAfterFirstPoll{src: tg}, Handler: &bot.Router{Allow: bot.NewAllowlist([]int64{7}, []int64{-100}), Answer: tg, Messages: crashed, Callbacks: crashed, Log: quiet}, Log: quiet}
	go p.Run(ctx)
	waitUntil(t, "update 7", 5*time.Second, func() bool { return crashed.count("before") == 1 })
	cancel() // no Confirm: the crash
	f := &fakeService{caps: []string{engine.CapRecordDelivery, engine.CapReplyParts, engine.CapHoldTokens}, quietSubscribe: true}
	startService(t, f)
	next := &countingSink{got: map[string]int{}}
	stop, done := runCounting(t, c, f.socket, next)
	waitUntil(t, "the redelivery", 5*time.Second, func() bool { return next.count("before") == 1 })
	stop()
	<-done
	if c.times(7) < 2 {
		t.Fatalf("update 7 must be delivered again after a crash: %d", c.times(7))
	}
}
