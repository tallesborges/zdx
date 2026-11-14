package engine

import (
	"context"
	"crypto/rand"
	"fmt"
	"strings"
	"unicode/utf8"
)

// Delivery statuses of zdx_engine::core::delivery::DeliveryStatus.
const (
	DeliveryPending = "pending"
	DeliverySent    = "sent"
	DeliveryFailed  = "failed"
	DeliveryUnknown = "unknown"
)

// Delivery item kinds.
const (
	DeliveryItemText = "text"
	DeliveryItemFile = "file"
)

// maxDeliveryErrorChars is MAX_ERROR_CHARS of delivery.rs.
const maxDeliveryErrorChars = 300

// TextPart is the position of one message of split reply text (1-based).
type TextPart struct {
	Index int `json:"index"`
	Total int `json:"total"`
}

// DeliveryItem is zdx_engine::core::delivery::DeliveryItem.
type DeliveryItem struct {
	Kind      string    `json:"kind"`
	Path      *string   `json:"path,omitempty"`
	Status    string    `json:"status"`
	MessageID *int64    `json:"message_id,omitempty"`
	Error     *string   `json:"error,omitempty"`
	Part      *TextPart `json:"part,omitempty"`
	LinkOnly  bool      `json:"link_only,omitempty"`
}

// DeliveryRecord is zdx_engine::core::delivery::DeliveryRecord. The pending
// record and its outcome share ID; the later one supersedes the earlier.
type DeliveryRecord struct {
	ID      string         `json:"id"`
	Surface string         `json:"surface"`
	ChatID  *int64         `json:"chat_id,omitempty"`
	TopicID *int64         `json:"topic_id,omitempty"`
	Items   []DeliveryItem `json:"items"`
}

// NewDeliveryRecord starts a record with a fresh id, every item pending.
func NewDeliveryRecord(chat int64, topic *int64, items []DeliveryItem) DeliveryRecord {
	return DeliveryRecord{ID: newUUID(), Surface: "telegram", ChatID: &chat, TopicID: topic, Items: items}
}

// TextItems is one item per message of reply text: plain text for one
// message, numbered parts when split.
func TextItems(parts int) []DeliveryItem {
	switch parts {
	case 0:
		return nil
	case 1:
		return []DeliveryItem{{Kind: DeliveryItemText, Status: DeliveryPending}}
	}
	items := make([]DeliveryItem, parts)
	for i := range items {
		items[i] = DeliveryItem{Kind: DeliveryItemText, Status: DeliveryPending, Part: &TextPart{Index: i + 1, Total: parts}}
	}
	return items
}

// FileItem is a pending item for a file.
func FileItem(path string) DeliveryItem {
	return DeliveryItem{Kind: DeliveryItemFile, Path: &path, Status: DeliveryPending}
}

// MarkSent records a delivered item.
func (i *DeliveryItem) MarkSent(messageID *int64) {
	i.Status = DeliverySent
	i.MessageID = messageID
	i.Error = nil
}

// MarkFailed records an item known not delivered.
func (i *DeliveryItem) MarkFailed(err string) {
	i.Status = DeliveryFailed
	e := truncateError(err)
	i.Error = &e
}

// MarkUnknown records an item that may or may not have arrived.
func (i *DeliveryItem) MarkUnknown(err string) {
	i.Status = DeliveryUnknown
	e := truncateError(err)
	i.Error = &e
}

// HasProblems reports an item not confirmed as delivered.
func (r *DeliveryRecord) HasProblems() bool {
	for _, i := range r.Items {
		if i.Status != DeliverySent {
			return true
		}
	}
	return false
}

func truncateError(s string) string {
	s = strings.TrimSpace(s)
	if utf8.RuneCountInString(s) <= maxDeliveryErrorChars {
		return s
	}
	r := []rune(s)
	return string(r[:maxDeliveryErrorChars-1]) + "…"
}

func newUUID() string {
	var b [16]byte
	_, _ = rand.Read(b[:])
	b[6] = b[6]&0x0f | 0x40
	b[8] = b[8]&0x3f | 0x80
	return fmt.Sprintf("%x-%x-%x-%x-%x", b[0:4], b[4:6], b[6:8], b[8:10], b[10:16])
}

// RecordDelivery asks the service to persist a delivery record on a thread
// (capability record_delivery), answered by delivery_recorded with the
// record's problem summary, if any.
type RecordDelivery struct {
	ThreadID string `json:"thread_id"`
	// TurnID and Holder name the held turn and its hold token (capability
	// hold_tokens, held turns only). While a thread is held the service
	// writes only records that match its hold.
	TurnID *uint64        `json:"turn_id,omitempty"`
	Holder string         `json:"holder,omitempty"`
	Record DeliveryRecord `json:"record"`
}

// Hold is a held turn and the token that owns it.
type Hold struct {
	TurnID uint64
	Holder string
}

func (RecordDelivery) op() string { return "record_delivery" }

// RecordDelivery persists a record through the service and returns its
// user summary (set when an item is not confirmed delivered). It needs
// capability record_delivery, and hold_tokens with a hold, and sends
// nothing without them.
func (c *Client) RecordDelivery(ctx context.Context, threadID string, hold *Hold, record DeliveryRecord) (string, error) {
	caps := []string{CapRecordDelivery}
	req := RecordDelivery{ThreadID: threadID, Record: record}
	if hold != nil {
		caps = append(caps, CapHoldTokens)
		turn := hold.TurnID
		req.TurnID, req.Holder = &turn, hold.Holder
	}
	if err := c.RequireAll(ctx, caps...); err != nil {
		return "", err
	}
	reply, err := c.CallOK(ctx, req)
	if err != nil {
		return "", err
	}
	if reply.Kind != ReplyDeliveryRecorded {
		return "", fmt.Errorf("unexpected reply %q to record_delivery", reply.Kind)
	}
	if reply.UserSummary == nil {
		return "", nil
	}
	return *reply.UserSummary, nil
}

// DeliveryReady fails with *ErrUnsupported unless the service can record
// deliveries. Callers check it before starting a turn whose reply they must
// record.
func (c *Client) DeliveryReady(ctx context.Context) error {
	return c.Require(ctx, CapRecordDelivery)
}

// EnqueueToWorker queues a prompt typed in a worker's legacy mirror topic
// and returns the worker's snapshot (nil when the service no longer manages
// it).
func (c *Client) EnqueueToWorker(ctx context.Context, workerThreadID, prompt string) (*WorkerSnapshot, error) {
	reply, err := c.CallOK(ctx, EnqueueFromTopic{WorkerThreadID: workerThreadID, Prompt: prompt})
	if err != nil {
		return nil, err
	}
	if reply.Kind != ReplyWorker {
		return nil, fmt.Errorf("unexpected reply %q to enqueue_from_topic", reply.Kind)
	}
	return reply.Worker, nil
}
