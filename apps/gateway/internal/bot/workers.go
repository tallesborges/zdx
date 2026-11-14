package bot

import (
	"context"
	"encoding/json"
	"log/slog"
	"sync"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/engine"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// WorkerNotices presents worker events from the service, as the Rust bot's
// orchestrator bridge does for silent workers (crates/zdx-bot/src/
// orchestrator.rs): a finished worker turn posts a one-line notice in its
// orchestrator's topic, and worker changes refresh the owner's pinned
// header. Mirrors, when set, keeps the legacy mirror-topic feed.
type WorkerNotices struct {
	Telegram *telegram.Client
	Engine   *engine.Client
	// MiniAppBase links a worker without a mirror topic; "" for none.
	MiniAppBase string
	// RefreshHeader, when set, refreshes an orchestrator's pinned header.
	RefreshHeader func(ctx context.Context, ownerThreadID string)
	// Mirrors is the legacy mirror-topic feed; nil posts none.
	Mirrors *MirrorFeed
	Log     *slog.Logger

	queues ownerQueues
}

// Dispatch presents a service event off the bridge loop: worker events of
// one owner thread run in order on that owner's queue, so a slow Telegram
// call delays only that owner, never the renderer subscription. Call
// Before ahead of rendering a turn of the owner so the completion notice
// still precedes the review turn's status.
func (w *WorkerNotices) Dispatch(ctx context.Context, ev engine.Event) {
	if ev.Type != engine.EventWorker {
		return
	}
	owner := ev.Owner()
	if owner == "" {
		if we, err := ev.WorkerEvent(); err == nil {
			owner = we.OwnerThreadID
		}
	}
	w.queues.enqueue(owner, func() { w.Observe(ctx, ev) })
}

// Before waits until the worker events Dispatch took for thread before
// this call are presented, or ctx ends.
func (w *WorkerNotices) Before(ctx context.Context, thread string) {
	w.queues.barrier(ctx, thread)
}

// Busy reports worker events still being presented.
func (w *WorkerNotices) Busy() bool { return w.queues.busy() }

// Wait blocks until every dispatched worker event is presented.
func (w *WorkerNotices) Wait() { w.queues.wg.Wait() }

// ownerQueues run jobs in order per key, keys in parallel.
type ownerQueues struct {
	mu      sync.Mutex
	pending map[string][]func()
	wg      sync.WaitGroup
}

func (q *ownerQueues) enqueue(key string, job func()) {
	q.mu.Lock()
	defer q.mu.Unlock()
	if q.pending == nil {
		q.pending = map[string][]func(){}
	}
	jobs, running := q.pending[key]
	q.pending[key] = append(jobs, job)
	if running {
		return
	}
	q.wg.Add(1)
	go q.drain(key)
}

func (q *ownerQueues) drain(key string) {
	defer q.wg.Done()
	for {
		q.mu.Lock()
		jobs := q.pending[key]
		if len(jobs) == 0 {
			delete(q.pending, key)
			q.mu.Unlock()
			return
		}
		job := jobs[0]
		q.mu.Unlock()
		job()
		q.mu.Lock()
		q.pending[key] = q.pending[key][1:]
		q.mu.Unlock()
	}
}

func (q *ownerQueues) barrier(ctx context.Context, key string) {
	q.mu.Lock()
	_, running := q.pending[key]
	q.mu.Unlock()
	if !running {
		return
	}
	done := make(chan struct{})
	q.enqueue(key, func() { close(done) })
	select {
	case <-done:
	case <-ctx.Done():
	}
}

func (q *ownerQueues) busy() bool {
	q.mu.Lock()
	defer q.mu.Unlock()
	return len(q.pending) > 0
}

// WorkerNoticeTimeout bounds one notice: the bridge waits for it, so the
// notice goes out before the review turn that follows it.
const WorkerNoticeTimeout = 30 * time.Second

// Observe presents one service event now. The service emits a worker's
// completion before the review turn it starts, and the notice must precede
// that turn's status: production calls it through Dispatch and Before.
func (w *WorkerNotices) Observe(ctx context.Context, ev engine.Event) {
	if ev.Type != engine.EventWorker {
		return
	}
	we, err := ev.WorkerEvent()
	if err != nil {
		w.log().Warn("unreadable worker event", "err", err)
		return
	}
	owner := ev.Owner()
	ctx, cancel := context.WithTimeout(context.WithoutCancel(ctx), WorkerNoticeTimeout)
	defer cancel()
	switch we.Kind {
	case engine.WorkerCreated, engine.WorkerQueueChanged:
		w.refresh(ctx, owner)
	case engine.WorkerPrompted:
		if w.Mirrors != nil {
			w.Mirrors.Prompted(ctx, we.WorkerThreadID, we.Prompt)
		}
		w.refresh(ctx, owner)
	case engine.WorkerActivity:
		// Only a legacy mirror feed refreshes on activity (Rust): on the
		// first activity of a turn. Silent workers do not.
		if w.Mirrors != nil && w.Mirrors.Activity(ctx, we.WorkerThreadID, we.Activity) {
			w.refresh(ctx, owner)
		}
	case engine.WorkerCompleted:
		if w.Mirrors != nil {
			w.Mirrors.Completed(ctx, we)
		}
		// A self-owned worker (re-attached from a mirror topic) has no
		// orchestrator to wake.
		if we.OwnerThreadID != we.WorkerThreadID && ev.Route != nil {
			w.notice(ctx, *ev.Route, we)
		}
		w.refresh(ctx, we.OwnerThreadID)
	}
}

func (w *WorkerNotices) log() *slog.Logger {
	if w.Log != nil {
		return w.Log
	}
	return slog.Default()
}

func (w *WorkerNotices) refresh(ctx context.Context, owner string) {
	if owner != "" && w.RefreshHeader != nil {
		w.RefreshHeader(ctx, owner)
	}
}

func (w *WorkerNotices) notice(ctx context.Context, route engine.Route, we engine.WorkerEvent) {
	text := WorkerUpdateNotice(w.title(ctx, we.OwnerThreadID, we.WorkerThreadID), workerLink(we.MirrorURL, w.MiniAppBase, we.WorkerThreadID), we.Status)
	if _, err := w.Telegram.SendText(ctx, telegram.TextSend{ChatID: route.Chat, Text: text, MessageThreadID: route.Topic, DisablePreview: true}); err != nil {
		w.log().Warn("failed to post worker update notice", "worker", we.WorkerThreadID, "err", err)
	}
}

// title is the worker's display title from Rust's thread_workers view of
// its owner (the title, else the first 8 characters of the id).
func (w *WorkerNotices) title(ctx context.Context, owner, worker string) string {
	if t := WorkerTitles(ctx, w.Engine, owner)[worker]; t != "" {
		return t
	}
	if r := []rune(worker); len(r) > 8 {
		return string(r[:8])
	}
	return worker
}

// WorkerTitles are the titles Rust's thread_workers view gives an owner's
// workers, by worker thread id; empty when the view is unavailable.
func WorkerTitles(ctx context.Context, eng *engine.Client, owner string) map[string]string {
	if eng == nil || owner == "" {
		return nil
	}
	out, err := eng.View(ctx, engine.View{Op: engine.ViewThreadWorkers, ThreadID: &owner})
	if err != nil {
		return nil
	}
	var body struct {
		Workers []struct {
			ThreadID string `json:"thread_id"`
			Title    string `json:"title"`
		} `json:"workers"`
	}
	if json.Unmarshal(out.Body, &body) != nil {
		return nil
	}
	titles := map[string]string{}
	for _, row := range body.Workers {
		if row.Title != "" {
			titles[row.ThreadID] = row.Title
		}
	}
	return titles
}

// workerLink is where a worker link points: its legacy mirror topic, else
// the worker thread in the Mini App; "" when neither exists (Rust
// worker_link).
func workerLink(mirrorURL *string, miniAppBase, worker string) string {
	if mirrorURL != nil && *mirrorURL != "" {
		return *mirrorURL
	}
	if miniAppBase != "" {
		return miniAppBase + "?startapp=" + worker
	}
	return ""
}

// WorkerUpdateNotice is the one-line notice in the orchestrator topic when
// a worker turn ends (Rust worker_update_notice).
func WorkerUpdateNotice(title, link, status string) string {
	worker := EscapeHTML(title)
	if link != "" {
		worker = `<a href="` + EscapeHTML(link) + `">` + worker + `</a>`
	}
	glyph, verb := "❌", "failed"
	switch status {
	case "completed":
		glyph, verb = "✅", "finished"
	case "cancelled":
		glyph, verb = "🚫", "was cancelled"
	}
	return glyph + " Worker 🛠 " + worker + " " + verb + " · reviewing…"
}
