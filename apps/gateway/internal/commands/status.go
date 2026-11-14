package commands

import (
	"context"
	"encoding/json"
	"fmt"
	"strconv"
	"strings"

	"github.com/tallesborges/zdx/apps/gateway/internal/conversation"
	"github.com/tallesborges/zdx/apps/gateway/internal/render"
	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// headerRefresh is the pinned thread header's `↻ Refresh` data.
const headerRefresh = "thread_header:refresh"

func sourceLabel(source string) string {
	switch source {
	case "orchestrator":
		return "orchestrator override"
	case "topic":
		return "topic override"
	}
	return "workspace/global default"
}

// StatusText is the status card of a thread (/status with heading
// "<b>Status</b>", the pinned header with "🧵 <b>Thread</b>"), laid out like
// the Rust bot's. Every value comes from the Rust projection.
func StatusText(s ThreadStatus, heading, miniAppURL string) string {
	if s.ConfigError != "" {
		return heading + "\n⚠️ <pre>" + esc(s.ConfigError) + "</pre>"
	}
	if s.Orchestrator {
		heading = "🎛 <b>Orchestrator</b>"
	}
	lines := []string{heading}
	if s.Refused != "" {
		lines = append(lines, "⚠️ Turns here are refused: "+esc(s.Refused)+". Pick another model with /model.")
	}
	if s.Mode != "" {
		lines = append(lines, "Model: <code>"+esc(s.Model)+"</code> ("+sourceLabel(s.ModelSource)+", <code>"+esc(s.Mode)+"</code>)")
	} else {
		lines = append(lines, "Model: <code>"+esc(s.Model)+"</code> ("+sourceLabel(s.ModelSource)+")")
	}
	lines = append(lines,
		"Thinking: <code>"+s.Thinking+"</code> ("+sourceLabel(s.ThinkingSource)+")",
		"Thread: <code>"+esc(s.ThreadID)+"</code>",
	)
	profile := s.Profile
	if profile == "" {
		profile = "fallback"
	}
	lines = append(lines, "Profile: <code>"+esc(profile)+"</code>")
	if !s.Orchestrator {
		branch := s.Branch
		if branch == "" {
			branch = "n/a"
		}
		lines = append(lines, "Root: <code>"+esc(s.Root)+"</code>", "Branch: <code>"+esc(branch)+"</code>")
	}
	if s.ContextLimit > 0 {
		pct := float64(s.ContextTokens) / float64(s.ContextLimit) * 100
		lines = append(lines, fmt.Sprintf("Context usage: <code>%.0f%% of %s (%s)</code>", pct, contextLimit(s.ContextLimit), tokens(s.ContextTokens)))
	} else {
		lines = append(lines, "Context usage: <code>"+tokens(s.ContextTokens)+"</code> (limit unknown)")
	}
	u := s.Usage
	lines = append(lines,
		"Usage totals: <code>↑"+tokens(u.Input)+" ↓"+tokens(u.Output)+" R"+tokens(u.CacheRead)+" W"+tokens(u.CacheWrite)+"</code>",
		pricingLine(s.Pricing, u),
	)
	if s.Orchestrator {
		lines = append(lines, workerLines(s.Workers, miniAppURL)...)
	}
	return strings.Join(lines, "\n")
}

func pricingLine(p Pricing, u Usage) string {
	switch p.Kind {
	case PricingSubscription:
		return "Pricing: <code>subscription</code> (OAuth provider)"
	case PricingPriced:
	default:
		return "Pricing: <code>unknown</code> (model registry metadata not found)"
	}
	line := "Pricing: <code>" + cost(p.Cost) + "</code> total · rates <code>$" + price(p.Input) + "/$" + price(p.Output) + "/$" + price(p.CacheRead) + "/$" + price(p.CacheWrite) + "</code>/1M"
	switch {
	case p.Savings > 0.001:
		line += " · saved <code>" + cost(p.Savings) + "</code>"
	case u.CacheRead > 0:
		line += " · cached"
	}
	return line
}

func workerLines(workers []StatusWorker, miniAppURL string) []string {
	if len(workers) == 0 {
		return []string{"Workers: <i>none yet</i>"}
	}
	running, queued := 0, 0
	for _, w := range workers {
		switch w.Status {
		case "running":
			running++
		case "queued":
			queued++
		}
	}
	lines := []string{fmt.Sprintf("Workers: <code>%d running · %d queued · %d settled</code>", running, queued, len(workers)-running-queued)}
	for i, w := range workers {
		if i == 5 {
			lines = append(lines, fmt.Sprintf("• … and %d more", len(workers)-5))
			break
		}
		title := w.Title
		if title == "" {
			title = truncateRunes(w.ThreadID, 8)
		}
		label := esc(title)
		if url, ok := render.WorkerURL(w.MirrorURL, miniAppURL, w.ThreadID); ok {
			label = `<a href="` + url + `">` + label + "</a>"
		}
		lines = append(lines, "• "+workerGlyph(w.Status)+" "+label+" — "+w.Status)
	}
	return lines
}

func workerGlyph(status string) string {
	switch status {
	case "running":
		return "⚙️"
	case "queued":
		return "⏳"
	case "completed":
		return "✅"
	case "failed":
		return "❌"
	}
	return "🚫"
}

func truncateRunes(s string, n int) string {
	if r := []rune(s); len(r) > n {
		return string(r[:n])
	}
	return s
}

func tokens(n uint64) string {
	switch {
	case n >= 1_000_000:
		return strconv.FormatFloat(float64(n)/1_000_000, 'f', 1, 64) + "M"
	case n >= 1_000:
		return strconv.FormatFloat(float64(n)/1_000, 'f', 1, 64) + "k"
	}
	return strconv.FormatUint(n, 10)
}

func contextLimit(n uint64) string {
	switch {
	case n >= 1_000_000:
		return strconv.FormatFloat(float64(n)/1_000_000, 'f', 0, 64) + "M"
	case n >= 1_000:
		return strconv.FormatFloat(float64(n)/1_000, 'f', 0, 64) + "k"
	}
	return strconv.FormatUint(n, 10)
}

func cost(c float64) string {
	switch {
	case c < 0.001:
		return "$" + strconv.FormatFloat(c, 'f', 4, 64)
	case c < 0.01:
		return "$" + strconv.FormatFloat(c, 'f', 3, 64)
	}
	return "$" + strconv.FormatFloat(c, 'f', 2, 64)
}

func price(v float64) string {
	t := strconv.FormatFloat(v, 'f', 4, 64)
	if strings.Contains(t, ".") {
		t = strings.TrimRight(t, "0")
		t = strings.TrimSuffix(t, ".")
	}
	return t
}

// status reads a thread's status card values. The bot profile is the
// chat's binding, which the Rust projection leaves out.
func (h *Handler) status(ctx context.Context, chat int64, threadID string) (ThreadStatus, error) {
	st, err := h.Statuses.ThreadStatus(ctx, chat, threadID)
	if err == nil && h.Chats != nil {
		st.Profile = h.Chats.Binding(chat).Profile
	}
	return st, err
}

// header is a pinned thread header this process posted.
type header struct {
	chat, message int64
	rendered      string
}

func headerText(s ThreadStatus, miniAppURL string) string {
	return StatusText(s, "🧵 <b>Thread</b>", miniAppURL) + "\n\n<i>Tap Refresh for current usage.</i>"
}

// HeaderKeyboard is the header's buttons: the Mini App thread, the owning
// orchestrator (its topic, else the Mini App), and Refresh.
func HeaderKeyboard(miniAppURL, threadID, parentOrchestrator string) *telegram.InlineKeyboardMarkup {
	var rows [][]telegram.InlineKeyboardButton
	if miniAppURL != "" {
		rows = append(rows, []telegram.InlineKeyboardButton{telegram.URLButton("💬 Open Thread", miniAppURL+"?startapp="+threadID)})
	}
	if parentOrchestrator != "" {
		url := ""
		if r, ok := conversation.Parse(parentOrchestrator); ok && r.Topic != nil {
			url, _ = TopicLink(r.Chat, *r.Topic)
		}
		if url == "" && miniAppURL != "" {
			url = miniAppURL + "?startapp=" + parentOrchestrator
		}
		if url != "" {
			rows = append(rows, []telegram.InlineKeyboardButton{telegram.URLButton("🎛 Open orchestrator", url)})
		}
	}
	rows = append(rows, []telegram.InlineKeyboardButton{telegram.CallbackButton("↻ Refresh", headerRefresh)})
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

func rendered(text string, kb *telegram.InlineKeyboardMarkup) string {
	b, _ := json.Marshal(kb)
	return text + "\n" + string(b)
}

func (h *Handler) headerView(ctx context.Context, chat int64, threadID string) (string, *telegram.InlineKeyboardMarkup, error) {
	st, err := h.status(ctx, chat, threadID)
	if err != nil {
		return "", nil, err
	}
	url := h.miniApp(chat)
	return headerText(st, url), HeaderKeyboard(url, threadID, st.ParentOrchestrator), nil
}

// PostThreadHeader posts and pins a topic's thread header card, and
// remembers it for RefreshHeader.
func (h *Handler) PostThreadHeader(ctx context.Context, chat, topic int64, threadID string) error {
	if h.Statuses == nil {
		return fmt.Errorf("thread headers need the thread status view")
	}
	text, kb, err := h.headerView(ctx, chat, threadID)
	if err != nil {
		return err
	}
	msg, err := h.Telegram.SendText(ctx, telegram.TextSend{ChatID: chat, Text: text, MessageThreadID: &topic, ReplyMarkup: kb})
	if err != nil {
		return fmt.Errorf("post thread header: %w", err)
	}
	h.mu.Lock()
	if h.headers == nil {
		h.headers = map[string]header{}
	}
	h.headers[threadID] = header{chat: chat, message: msg.ID, rendered: rendered(text, kb)}
	h.mu.Unlock()
	if h.Topics != nil {
		if err := h.Topics.PinChatMessage(ctx, chat, msg.ID); err != nil {
			h.log().Warn("failed to pin the thread header", "chat_id", chat, "message_id", msg.ID, "err", err)
		}
	}
	return nil
}

// postHeader posts a new topic's header; a failure only logs, as in Rust.
func (h *Handler) postHeader(ctx context.Context, chat, topic int64, threadID string) {
	if h.Statuses == nil {
		return
	}
	if err := h.PostThreadHeader(ctx, chat, topic, threadID); err != nil {
		h.log().Warn("created a topic but failed to post its header", "chat_id", chat, "topic_id", topic, "err", err)
	}
}

func notModified(err error) bool {
	d, ok := telegram.APIDescription(err)
	return ok && strings.Contains(d, "message is not modified")
}

// RefreshHeader re-renders a header this process posted, for example after
// a worker event of an orchestrator. Unknown threads and unchanged renders
// send nothing.
func (h *Handler) RefreshHeader(ctx context.Context, threadID string) {
	if h.Statuses == nil {
		return
	}
	h.mu.Lock()
	hd, ok := h.headers[threadID]
	h.mu.Unlock()
	if !ok {
		return
	}
	text, kb, err := h.headerView(ctx, hd.chat, threadID)
	if err != nil {
		h.log().Warn("failed to refresh a thread header", "thread_id", threadID, "err", err)
		return
	}
	r := rendered(text, kb)
	if r == hd.rendered {
		return
	}
	if err := h.Telegram.EditMessageText(ctx, hd.chat, hd.message, text, kb); err != nil && !notModified(err) {
		h.log().Warn("failed to refresh a thread header", "thread_id", threadID, "err", err)
		return
	}
	h.mu.Lock()
	h.headers[threadID] = header{chat: hd.chat, message: hd.message, rendered: r}
	h.mu.Unlock()
}

func (h *Handler) headerCallback(ctx context.Context, q *telegram.CallbackQuery) {
	msg := q.Message
	if msg == nil {
		h.answer(ctx, q, "No message context")
		return
	}
	topic := msg.EffectiveThreadID()
	if topic == nil {
		h.answer(ctx, q, "This header is not inside a topic")
		return
	}
	err := func() error {
		t, err := h.Resolver.ResolveThread(ctx, msg.Chat.ID, topic)
		if err != nil {
			return err
		}
		text, kb, err := h.headerView(ctx, msg.Chat.ID, t.ID)
		if err != nil {
			return err
		}
		if err := h.Telegram.EditMessageText(ctx, msg.Chat.ID, msg.ID, text, kb); err != nil && !notModified(err) {
			return err
		}
		h.mu.Lock()
		if hd, ok := h.headers[t.ID]; ok && hd.message == msg.ID {
			h.headers[t.ID] = header{chat: msg.Chat.ID, message: msg.ID, rendered: rendered(text, kb)}
		}
		h.mu.Unlock()
		return nil
	}()
	if err != nil {
		h.log().Warn("failed to refresh a thread header", "chat_id", msg.Chat.ID, "err", err)
		h.answer(ctx, q, "Couldn't refresh thread status")
		return
	}
	h.answer(ctx, q, "Thread status refreshed ✓")
}
