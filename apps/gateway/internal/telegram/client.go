package telegram

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"net/url"
	"os"
	"time"
)

// Timeouts as in the Rust client. The HTTP timeout covers the 30s long poll.
const (
	ConnectTimeout = 2 * time.Second
	HTTPTimeout    = 35 * time.Second
	PollTimeout    = 30 * time.Second
	ParseModeHTML  = "HTML"
	// TypingInterval is how often the typing action is repeated.
	TypingInterval = 4 * time.Second
)

// SendMode is how a call uses the per-chat gate.
type SendMode int

const (
	// Immediate never waits: skipped locally while the chat is in a flood
	// wait. For the polling loop and the event bridge, which must not stall
	// on one busy chat.
	Immediate SendMode = iota
	// Paced waits for the chat's flood wait and, for new messages, the
	// pacing slot. One attempt.
	Paced
	// Reliable is Paced and retries explicit flood refusals within
	// RetryImportant. For reply text and files. Ambiguous failures are never
	// retried.
	Reliable
)

// CallKind is what a call does to the chat.
type CallKind int

const (
	// KindNewMessage creates a chat message: paced and counted.
	KindNewMessage CallKind = iota
	// KindUpdate changes existing state (edit, delete, pin, topic): not
	// paced or counted; waits out a flood wait when the mode allows.
	KindUpdate
	// KindEphemeral is typing: skipped during a flood wait, never waits.
	KindEphemeral
	// KindUnscoped is not tied to a chat (polling, files, commands,
	// callback answers).
	KindUnscoped
)

type callSpec struct {
	method string
	chat   *int64
	topic  *int64
	kind   CallKind
}

func newMessageSpec(method string, chat int64, topic *int64) callSpec {
	return callSpec{method: method, chat: &chat, topic: topic, kind: KindNewMessage}
}

func updateSpec(method string, chat int64) callSpec {
	return callSpec{method: method, chat: &chat, kind: KindUpdate}
}

func unscopedSpec(method string) callSpec { return callSpec{method: method, kind: KindUnscoped} }

// Client is a Bot API client. Copies made by Paced and Reliable share the
// per-chat gate.
type Client struct {
	http    *http.Client
	baseURL string
	token   string
	gate    *chatGate
	mode    SendMode
	clock   Clock
	log     *slog.Logger
	// Repair rewrites HTML Telegram refused to parse; nil skips the repair
	// step and falls back to plain text directly.
	Repair func(string) string
}

// Options configure a client. Zero values use the production defaults.
type Options struct {
	BaseURL string
	HTTP    *http.Client
	Clock   Clock
	Logger  *slog.Logger
}

// NewClient returns an Immediate-mode client.
func NewClient(token string, opts Options) *Client {
	c := &Client{
		http:    opts.HTTP,
		baseURL: opts.BaseURL,
		token:   token,
		gate:    &chatGate{},
		mode:    Immediate,
		clock:   opts.Clock,
		log:     opts.Logger,
	}
	if c.http == nil {
		dialer := &net.Dialer{Timeout: ConnectTimeout}
		transport := http.DefaultTransport.(*http.Transport).Clone()
		transport.DialContext = dialer.DialContext
		c.http = &http.Client{Timeout: HTTPTimeout, Transport: transport}
	}
	if c.baseURL == "" {
		c.baseURL = "https://api.telegram.org"
	}
	if c.clock == nil {
		c.clock = RealClock
	}
	if c.log == nil {
		c.log = slog.Default()
	}
	return c
}

func (c *Client) withMode(m SendMode) *Client {
	cp := *c
	cp.mode = m
	return &cp
}

// Paced returns a copy whose calls wait for the chat's flood wait and pacing
// slot. Use it only off the polling loop.
func (c *Client) Paced() *Client { return c.withMode(Paced) }

// Reliable returns a copy for reply text and files.
func (c *Client) Reliable() *Client { return c.withMode(Reliable) }

// Mode is the client's send mode.
func (c *Client) Mode() SendMode { return c.mode }

func (c *Client) url(method string) string {
	return c.baseURL + "/bot" + c.token + "/" + method
}

// call runs one Bot API call through the chat gate, decodes the result, logs
// each attempt, and in Reliable mode retries explicit flood refusals. The
// retry budget is per request; the deadline bounds the start of the last
// attempt. Ambiguous failures, including a result that does not decode, are
// never retried: the message may already be in the chat.
func call[T any](ctx context.Context, c *Client, spec callSpec, build func() (*http.Request, error)) (T, error) {
	var zero T
	first := c.clock.Now()
	admissionLimit := first.Add(RetryImportant.Deadline)
	for attempt := 1; ; attempt++ {
		waited, err := c.beforeCall(ctx, spec, admissionLimit)
		if err != nil {
			c.logCall(spec, attempt, 0, 0, nil, err)
			return zero, err
		}
		started := c.clock.Now()
		value, err := c.sendOnce(ctx, spec.method, build)
		var decoded T
		var messageID *int64
		if err == nil {
			messageID = resultMessageID(value)
			if derr := json.Unmarshal(value, &decoded); derr != nil {
				err = &TransportError{Method: spec.method, Stage: Ambiguous, Detail: "accepted, but the result did not decode"}
			}
		}
		c.logCall(spec, attempt, waited, c.clock.Now().Sub(started), messageID, err)
		if err == nil {
			return decoded, nil
		}
		class, retryAfter := Classify(err)
		if class != ClassFloodWait {
			return zero, err
		}
		if spec.chat != nil {
			now := c.clock.Now()
			c.gate.with(*spec.chat, func(p *ChatPacer) { p.NoteFloodWait(now, retryAfter) })
		}
		if c.mode != Reliable {
			return zero, err
		}
		if _, ok := RetryImportant.DelayBeforeRetry(attempt, c.clock.Now().Sub(first), retryAfter); !ok {
			return zero, err
		}
		if spec.chat == nil {
			if serr := c.clock.Sleep(ctx, retryAfter); serr != nil {
				return zero, err
			}
		}
	}
}

// beforeCall applies the chat gate and returns how long the call waited, or
// a *FloodWaitActive when the call is skipped: at once in Immediate mode and
// for typing, or when admission would come after limit.
func (c *Client) beforeCall(ctx context.Context, spec callSpec, limit time.Time) (time.Duration, error) {
	if spec.chat == nil {
		return 0, nil
	}
	chat := *spec.chat
	started := c.clock.Now()
	if c.mode == Immediate || spec.kind == KindEphemeral {
		var remaining time.Duration
		var cooling bool
		c.gate.with(chat, func(p *ChatPacer) {
			remaining, cooling = p.CooldownRemaining(started)
			if !cooling && spec.kind == KindNewMessage {
				p.RecordNewMessage(started)
			}
		})
		if cooling {
			return 0, &FloodWaitActive{Method: spec.method, ChatID: chat, Remaining: remaining}
		}
		return 0, nil
	}
	for {
		now := c.clock.Now()
		var admitAt time.Time
		var admitted bool
		c.gate.with(chat, func(p *ChatPacer) {
			if spec.kind == KindNewMessage {
				admitAt, admitted = p.TryAdmitNewMessage(now, chat < 0)
				return
			}
			remaining, cooling := p.CooldownRemaining(now)
			admitted = !cooling
			admitAt = now.Add(remaining)
		})
		if admitted {
			return c.clock.Now().Sub(started), nil
		}
		if admitAt.After(limit) {
			return 0, &FloodWaitActive{Method: spec.method, ChatID: chat, Remaining: admitAt.Sub(now)}
		}
		if err := c.clock.Sleep(ctx, admitAt.Sub(now)); err != nil {
			return 0, &TransportError{Method: spec.method, Stage: NotSent, Detail: "cancelled while waiting for the chat"}
		}
	}
}

// sendOnce performs one HTTP exchange and returns the envelope's result.
func (c *Client) sendOnce(ctx context.Context, method string, build func() (*http.Request, error)) (json.RawMessage, error) {
	req, err := build()
	if err != nil {
		return nil, &TransportError{Method: method, Stage: NotSent, Detail: "request build failed"}
	}
	resp, err := c.http.Do(req.WithContext(ctx))
	if err != nil {
		return nil, transportError(method, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		return nil, &TransportError{Method: method, Stage: Ambiguous, Detail: "reading the answer failed: " + scrub(err)}
	}
	return parseResponse(method, body)
}

// transportError classifies a client.Do failure. Only a failed dial is
// certainly unsent; everything else may have reached Telegram. The URL is
// dropped because it holds the token.
func transportError(method string, err error) error {
	stage := Ambiguous
	var op *net.OpError
	if errors.As(err, &op) && op.Op == "dial" {
		stage = NotSent
	}
	return &TransportError{Method: method, Stage: stage, Detail: scrub(err)}
}

// scrub returns the error text without the *url.Error wrapper, which quotes
// the request URL and with it the token.
func scrub(err error) string {
	for {
		var u *url.Error
		if !errors.As(err, &u) {
			break
		}
		err = u.Err
	}
	if errors.Is(err, context.DeadlineExceeded) {
		return "timed out"
	}
	if errors.Is(err, context.Canceled) {
		return "cancelled"
	}
	return err.Error()
}

// parseResponse decodes a Bot API envelope into its result.
func parseResponse(method string, body []byte) (json.RawMessage, error) {
	var env struct {
		OK          *bool           `json:"ok"`
		Result      json.RawMessage `json:"result"`
		ErrorCode   int             `json:"error_code"`
		Description *string         `json:"description"`
		Parameters  *struct {
			RetryAfter *int64 `json:"retry_after"`
		} `json:"parameters"`
	}
	unreadable := &TransportError{Method: method, Stage: Ambiguous, Detail: fmt.Sprintf("unreadable response (%d bytes)", len(body))}
	if err := json.Unmarshal(body, &env); err != nil || env.OK == nil {
		return nil, unreadable
	}
	if *env.OK {
		if len(env.Result) == 0 {
			return json.RawMessage("true"), nil
		}
		return env.Result, nil
	}
	apiErr := &APIError{Method: method, Code: env.ErrorCode, Description: "Telegram API error"}
	if env.Description != nil {
		apiErr.Description = *env.Description
	}
	if env.Parameters != nil {
		apiErr.RetryAfter = env.Parameters.RetryAfter
	}
	return nil, apiErr
}

func resultMessageID(v json.RawMessage) *int64 {
	var m struct {
		MessageID *int64 `json:"message_id"`
	}
	if json.Unmarshal(v, &m) != nil {
		return nil
	}
	return m.MessageID
}

// logCall logs one attempt. New messages log at info, others at debug.
// Bodies and tokens are never logged.
func (c *Client) logCall(spec callSpec, attempt int, waited, duration time.Duration, messageID *int64, err error) {
	attrs := []any{
		"client", "gateway", "pid", os.Getpid(), "bot", BotIdentity(c.token), "method", spec.method,
		"chat", optional(spec.chat), "topic", optional(spec.topic), "attempt", attempt,
		"waited_ms", waited.Milliseconds(), "duration_ms", duration.Milliseconds(),
	}
	newMessage := spec.kind == KindNewMessage
	if err == nil {
		attrs = append(attrs, "message_id", optional(messageID), "outcome", "ok")
		if newMessage {
			c.log.Info("Telegram call", attrs...)
		} else {
			c.log.Debug("Telegram call", attrs...)
		}
		return
	}
	class, retryAfter := Classify(err)
	attrs = append(attrs, "outcome", class.Label(), "error", err.Error())
	if class == ClassFloodWait {
		attrs = append(attrs, "retry_after_s", int64(retryAfter/time.Second))
	}
	var skipped *FloodWaitActive
	if newMessage && !errors.As(err, &skipped) {
		c.log.Warn("Telegram call", attrs...)
	} else {
		c.log.Debug("Telegram call", attrs...)
	}
}

func postJSON[T any](ctx context.Context, c *Client, spec callSpec, body any) (T, error) {
	payload, err := json.Marshal(body)
	if err != nil {
		var zero T
		return zero, fmt.Errorf("encode %s request: %w", spec.method, err)
	}
	url := c.url(spec.method)
	return call[T](ctx, c, spec, func() (*http.Request, error) {
		req, err := http.NewRequest(http.MethodPost, url, bytes.NewReader(payload))
		if err != nil {
			return nil, err
		}
		req.Header.Set("Content-Type", "application/json")
		return req, nil
	})
}

// GetUpdates long-polls for message and callback_query updates.
func (c *Client) GetUpdates(ctx context.Context, offset *int64, timeout time.Duration) ([]Update, error) {
	body := struct {
		Offset         *int64   `json:"offset,omitempty"`
		Timeout        int64    `json:"timeout"`
		AllowedUpdates []string `json:"allowed_updates"`
	}{offset, int64(timeout / time.Second), []string{"message", "callback_query"}}
	return postJSON[[]Update](ctx, c, unscopedSpec("getUpdates"), body)
}

// TextSend is a text message, sent as HTML with the repair/plain fallback.
type TextSend struct {
	ChatID           int64
	Text             string
	ReplyToMessageID *int64
	MessageThreadID  *int64
	ReplyParameters  *ReplyParameters
	ReplyMarkup      *InlineKeyboardMarkup
	DisablePreview   bool
}

type sendMessageRequest struct {
	ChatID                   int64                 `json:"chat_id"`
	Text                     string                `json:"text"`
	MessageThreadID          *int64                `json:"message_thread_id,omitempty"`
	ReplyToMessageID         *int64                `json:"reply_to_message_id,omitempty"`
	AllowSendingWithoutReply *bool                 `json:"allow_sending_without_reply,omitempty"`
	ParseMode                *string               `json:"parse_mode,omitempty"`
	ReplyMarkup              *InlineKeyboardMarkup `json:"reply_markup,omitempty"`
	ReplyParameters          *ReplyParameters      `json:"reply_parameters,omitempty"`
	LinkPreviewOptions       *linkPreviewOptions   `json:"link_preview_options,omitempty"`
}

type linkPreviewOptions struct {
	IsDisabled bool `json:"is_disabled"`
}

// SendText sends HTML; on a parse refusal it retries with the repaired text,
// then as plain text. Each fallback is a new request: the earlier one was
// refused, so nothing is duplicated.
func (c *Client) SendText(ctx context.Context, s TextSend) (*Message, error) {
	html := ParseModeHTML
	allow := true
	req := sendMessageRequest{
		ChatID:                   s.ChatID,
		Text:                     s.Text,
		MessageThreadID:          s.MessageThreadID,
		ReplyToMessageID:         s.ReplyToMessageID,
		AllowSendingWithoutReply: &allow,
		ParseMode:                &html,
		ReplyMarkup:              s.ReplyMarkup,
		ReplyParameters:          s.ReplyParameters,
	}
	if s.DisablePreview {
		req.LinkPreviewOptions = &linkPreviewOptions{IsDisabled: true}
	}
	spec := newMessageSpec("sendMessage", s.ChatID, s.MessageThreadID)
	msg, err := postJSON[*Message](ctx, c, spec, req)
	if err == nil || !IsHTMLParseError(err) {
		return msg, err
	}
	if c.Repair != nil {
		if repaired := c.Repair(s.Text); repaired != s.Text {
			retry := req
			retry.Text = repaired
			msg, err := postJSON[*Message](ctx, c, spec, retry)
			if err == nil || !IsHTMLParseError(err) {
				return msg, err
			}
		}
	}
	plain := req
	plain.ParseMode = nil
	return postJSON[*Message](ctx, c, spec, plain)
}

// EditMessageText edits a message's HTML text. A nil markup keeps the
// buttons; EmptyKeyboard removes them. On a parse refusal it resends plain.
func (c *Client) EditMessageText(ctx context.Context, chatID, messageID int64, text string, markup *InlineKeyboardMarkup) error {
	html := ParseModeHTML
	req := struct {
		ChatID      int64                 `json:"chat_id"`
		MessageID   int64                 `json:"message_id"`
		Text        string                `json:"text"`
		ParseMode   *string               `json:"parse_mode,omitempty"`
		ReplyMarkup *InlineKeyboardMarkup `json:"reply_markup,omitempty"`
	}{chatID, messageID, text, &html, markup}
	spec := updateSpec("editMessageText", chatID)
	_, err := postJSON[json.RawMessage](ctx, c, spec, req)
	if err != nil && IsHTMLParseError(err) {
		req.ParseMode = nil
		_, err = postJSON[json.RawMessage](ctx, c, spec, req)
	}
	return err
}

// EditMessageReplyMarkup replaces a message's inline keyboard.
func (c *Client) EditMessageReplyMarkup(ctx context.Context, chatID, messageID int64, markup *InlineKeyboardMarkup) error {
	req := struct {
		ChatID      int64                 `json:"chat_id"`
		MessageID   int64                 `json:"message_id"`
		ReplyMarkup *InlineKeyboardMarkup `json:"reply_markup,omitempty"`
	}{chatID, messageID, markup}
	_, err := postJSON[json.RawMessage](ctx, c, updateSpec("editMessageReplyMarkup", chatID), req)
	return err
}

// ForumTopic is the createForumTopic answer.
type ForumTopic struct {
	MessageThreadID int64  `json:"message_thread_id"`
	Name            string `json:"name"`
}

// CreateForumTopic opens a forum topic and returns its id. Telegram posts a
// service message for it, so it is paced like a new message. A topic whose
// request got no readable answer may exist; it is never created again
// automatically.
func (c *Client) CreateForumTopic(ctx context.Context, chat int64, name string) (int64, error) {
	req := struct {
		ChatID int64  `json:"chat_id"`
		Name   string `json:"name"`
	}{chat, name}
	topic, err := postJSON[ForumTopic](ctx, c, newMessageSpec("createForumTopic", chat, nil), req)
	if err != nil {
		return 0, err
	}
	if topic.MessageThreadID == 0 {
		return 0, &APIError{Method: "createForumTopic", Description: "answer has no message_thread_id"}
	}
	return topic.MessageThreadID, nil
}

// EditForumTopic renames a forum topic.
func (c *Client) EditForumTopic(ctx context.Context, chat, topic int64, name string) error {
	req := struct {
		ChatID          int64  `json:"chat_id"`
		MessageThreadID int64  `json:"message_thread_id"`
		Name            string `json:"name"`
	}{chat, topic, name}
	_, err := postJSON[bool](ctx, c, updateSpec("editForumTopic", chat), req)
	return err
}

// ForwardMessage forwards a message into a chat or forum topic. It posts
// a new message, so it is paced like one.
func (c *Client) ForwardMessage(ctx context.Context, chat, fromChat, messageID int64, topic *int64) error {
	req := struct {
		ChatID          int64  `json:"chat_id"`
		FromChatID      int64  `json:"from_chat_id"`
		MessageID       int64  `json:"message_id"`
		MessageThreadID *int64 `json:"message_thread_id,omitempty"`
	}{chat, fromChat, messageID, topic}
	_, err := postJSON[json.RawMessage](ctx, c, newMessageSpec("forwardMessage", chat, topic), req)
	return err
}

// DeleteForumTopic deletes a forum topic and its messages.
func (c *Client) DeleteForumTopic(ctx context.Context, chat, topic int64) error {
	req := struct {
		ChatID          int64 `json:"chat_id"`
		MessageThreadID int64 `json:"message_thread_id"`
	}{chat, topic}
	_, err := postJSON[bool](ctx, c, updateSpec("deleteForumTopic", chat), req)
	return err
}

// PinChatMessage pins a message without notifying members.
func (c *Client) PinChatMessage(ctx context.Context, chat, messageID int64) error {
	req := struct {
		ChatID              int64 `json:"chat_id"`
		MessageID           int64 `json:"message_id"`
		DisableNotification bool  `json:"disable_notification"`
	}{chat, messageID, true}
	_, err := postJSON[bool](ctx, c, updateSpec("pinChatMessage", chat), req)
	return err
}

// DeleteMessage deletes a message.
func (c *Client) DeleteMessage(ctx context.Context, chatID, messageID int64) error {
	req := struct {
		ChatID    int64 `json:"chat_id"`
		MessageID int64 `json:"message_id"`
	}{chatID, messageID}
	_, err := postJSON[bool](ctx, c, updateSpec("deleteMessage", chatID), req)
	return err
}

// AnswerCallbackQuery acknowledges a button press, optionally with a toast.
func (c *Client) AnswerCallbackQuery(ctx context.Context, id string, text *string) error {
	req := struct {
		CallbackQueryID string  `json:"callback_query_id"`
		Text            *string `json:"text,omitempty"`
	}{id, text}
	_, err := postJSON[bool](ctx, c, unscopedSpec("answerCallbackQuery"), req)
	return err
}

// SendChatAction sends a chat action such as "typing". Skipped during a
// flood wait; never waits.
func (c *Client) SendChatAction(ctx context.Context, chatID int64, action string, topic *int64) error {
	req := struct {
		ChatID          int64  `json:"chat_id"`
		Action          string `json:"action"`
		MessageThreadID *int64 `json:"message_thread_id,omitempty"`
	}{chatID, action, topic}
	spec := callSpec{method: "sendChatAction", chat: &chatID, topic: topic, kind: KindEphemeral}
	_, err := postJSON[bool](ctx, c, spec, req)
	return err
}

// SetMyCommands sets the command menu for one scope type ("default",
// "all_private_chats", "all_group_chats").
func (c *Client) SetMyCommands(ctx context.Context, commands []BotCommand, scope string) error {
	req := struct {
		Commands []BotCommand      `json:"commands"`
		Scope    map[string]string `json:"scope"`
	}{commands, map[string]string{"type": scope}}
	_, err := postJSON[bool](ctx, c, unscopedSpec("setMyCommands"), req)
	return err
}

// GetFile resolves a file id to a download path.
func (c *Client) GetFile(ctx context.Context, fileID string) (*RemoteFile, error) {
	req := struct {
		FileID string `json:"file_id"`
	}{fileID}
	return postJSON[*RemoteFile](ctx, c, unscopedSpec("getFile"), req)
}

// Typing repeats the typing action every TypingInterval until stop is
// called. Failures are ignored, like the Rust indicator.
func (c *Client) Typing(chatID int64, topic *int64) (stop func()) {
	ctx, cancel := context.WithCancel(context.Background())
	go func() {
		for {
			_ = c.SendChatAction(ctx, chatID, "typing", topic)
			if c.clock.Sleep(ctx, TypingInterval) != nil {
				return
			}
		}
	}()
	return cancel
}

// optional renders an optional id for logs: the value, or nil.
func optional(v *int64) any {
	if v == nil {
		return nil
	}
	return *v
}
