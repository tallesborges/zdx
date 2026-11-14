package bot

import (
	"context"
	"sync"
)

// TopicKey is one Telegram conversation: a chat and, in forums, a topic.
type TopicKey struct {
	Chat  int64
	Topic int64 // 0 for no topic
}

// KeyOf returns the key of a chat and optional topic.
func KeyOf(chat int64, topic *int64) TopicKey {
	if topic == nil {
		return TopicKey{Chat: chat}
	}
	return TopicKey{Chat: chat, Topic: *topic}
}

// Job is one unit of Telegram-side intake work for a topic.
type Job func(ctx context.Context)

// Ticket is a queued job. Cancel removes it if it has not started.
type Ticket struct {
	mu      sync.Mutex
	started bool
	dropped bool
}

// Cancel removes the job from its queue. It returns false when the job has
// already started (or was already cancelled).
func (t *Ticket) Cancel() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.started || t.dropped {
		return false
	}
	t.dropped = true
	return true
}

func (t *Ticket) begin() bool {
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.dropped {
		return false
	}
	t.started = true
	return true
}

type entry struct {
	job    Job
	ticket *Ticket
}

// TopicQueues runs jobs one at a time per topic and topics concurrently,
// like the Rust bot's chat queues. It orders Telegram-side intake only
// (staging, presentation, submit); execution order is the Rust service's.
type TopicQueues struct {
	ctx    context.Context
	mu     sync.Mutex
	queues map[TopicKey][]entry
	active map[TopicKey]bool
	wg     sync.WaitGroup
}

// NewTopicQueues returns queues whose jobs get ctx.
func NewTopicQueues(ctx context.Context) *TopicQueues {
	return &TopicQueues{ctx: ctx, queues: map[TopicKey][]entry{}, active: map[TopicKey]bool{}}
}

// Enqueue adds a job after the topic's earlier jobs.
func (q *TopicQueues) Enqueue(key TopicKey, job Job) *Ticket {
	t := &Ticket{}
	q.mu.Lock()
	q.queues[key] = append(q.queues[key], entry{job: job, ticket: t})
	start := !q.active[key]
	if start {
		q.active[key] = true
		q.wg.Add(1)
	}
	q.mu.Unlock()
	if start {
		go q.drain(key)
	}
	return t
}

// Pending is the number of jobs waiting (not running) for a topic.
func (q *TopicQueues) Pending(key TopicKey) int {
	q.mu.Lock()
	defer q.mu.Unlock()
	n := 0
	for _, e := range q.queues[key] {
		e.ticket.mu.Lock()
		if !e.ticket.dropped {
			n++
		}
		e.ticket.mu.Unlock()
	}
	return n
}

// Busy reports whether any topic has a running or waiting job.
func (q *TopicQueues) Busy() bool {
	q.mu.Lock()
	defer q.mu.Unlock()
	return len(q.active) > 0
}

// Wait blocks until every queue is drained.
func (q *TopicQueues) Wait() { q.wg.Wait() }

func (q *TopicQueues) drain(key TopicKey) {
	defer q.wg.Done()
	for {
		q.mu.Lock()
		list := q.queues[key]
		if len(list) == 0 {
			delete(q.queues, key)
			delete(q.active, key)
			q.mu.Unlock()
			return
		}
		e := list[0]
		q.queues[key] = list[1:]
		q.mu.Unlock()
		if e.ticket.begin() {
			e.job(q.ctx)
		}
	}
}
