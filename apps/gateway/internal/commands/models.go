package commands

import (
	"context"
	"strconv"
	"strings"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// ThinkingLevels are the picker's levels in Rust's order
// (ThinkingLevel::all, display names).
var ThinkingLevels = []string{"off", "low", "medium", "high", "xhigh", "max"}

// chatDefaultUnavailable answers /model in General while no Rust op writes
// a chat's default model.
const chatDefaultUnavailable = "Changing the chat's default model is not available in this gateway build yet. Use /model inside a topic."

func launchScope(s Scope) (role string, ok bool) {
	switch s {
	case ScopeNewThread:
		return RoleAgent, true
	case ScopeNewOrchestrator:
		return RoleOrchestrator, true
	}
	return "", false
}

func scopeForRole(role string) Scope {
	if role == RoleOrchestrator {
		return ScopeNewOrchestrator
	}
	return ScopeNewThread
}

// providers are the unique provider keys of models, in first-seen order.
func providers(models []PickerModel) []string {
	var out []string
	seen := map[string]bool{}
	for _, m := range models {
		p, _, _ := strings.Cut(m.ID, ":")
		if !seen[p] {
			seen[p] = true
			out = append(out, p)
		}
	}
	return out
}

func providerModels(models []PickerModel, provider string) []PickerModel {
	var out []PickerModel
	for _, m := range models {
		if strings.HasPrefix(m.ID, provider+":") {
			out = append(out, m)
		}
	}
	return out
}

func providerLabel(p Picker, key string) string {
	if l, ok := p.ProviderLabels[key]; ok && l != "" {
		return l
	}
	return key
}

func chunk(buttons []telegram.InlineKeyboardButton, n int) [][]telegram.InlineKeyboardButton {
	var rows [][]telegram.InlineKeyboardButton
	for i := 0; i < len(buttons); i += n {
		rows = append(rows, buttons[i:min(i+n, len(buttons))])
	}
	return rows
}

// ProviderKeyboard is the picker's first step: modes (where a pick changes
// an existing selection), providers, and Cancel or Back.
func ProviderKeyboard(p Picker, scope Scope) *telegram.InlineKeyboardMarkup {
	var rows [][]telegram.InlineKeyboardButton
	if scope == ScopeGeneral || scope == ScopeTopic || scope == ScopeStaging {
		var modes []telegram.InlineKeyboardButton
		for _, m := range p.Modes {
			modes = append(modes, telegram.CallbackButton("⚡ "+m.Name, "model_mode:"+m.Name+":"+string(scope)))
		}
		rows = append(rows, chunk(modes, 3)...)
	}
	var provs []telegram.InlineKeyboardButton
	for _, key := range providers(p.Models) {
		provs = append(provs, telegram.CallbackButton(providerLabel(p, key), "model_provider:"+key+":"+string(scope)))
	}
	rows = append(rows, chunk(provs, 3)...)
	exit := "✖ Cancel"
	if scope == ScopeNewThread || scope == ScopeNewOrchestrator || scope == ScopeStaging {
		exit = "← Back"
	}
	rows = append(rows, []telegram.InlineKeyboardButton{telegram.CallbackButton(exit, "model_cancel:"+string(scope))})
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

// ModelsKeyboard lists one provider's models, two per row.
func ModelsKeyboard(p Picker, provider string, scope Scope) *telegram.InlineKeyboardMarkup {
	var buttons []telegram.InlineKeyboardButton
	for i, m := range providerModels(p.Models, provider) {
		display := m.ID
		if parts := strings.Split(m.ID, ":"); len(parts) > 1 {
			display = parts[1]
		}
		buttons = append(buttons, telegram.CallbackButton(display, "model_pick:"+provider+":"+strconv.Itoa(i)+":"+string(scope)))
	}
	rows := chunk(buttons, 2)
	rows = append(rows,
		[]telegram.InlineKeyboardButton{telegram.CallbackButton("← Back", "model_back:"+string(scope))},
		[]telegram.InlineKeyboardButton{telegram.CallbackButton("✖ Cancel", "model_cancel:"+string(scope))},
	)
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

// ThinkingKeyboard is the picker's last step; the current level is ticked.
func ThinkingKeyboard(provider string, index int, scope Scope, current string) *telegram.InlineKeyboardMarkup {
	var buttons []telegram.InlineKeyboardButton
	for _, level := range ThinkingLevels {
		label := level
		if level == current {
			label = "✅ " + level
		}
		buttons = append(buttons, telegram.CallbackButton(label, "model_thinking:"+provider+":"+strconv.Itoa(index)+":"+string(scope)+":"+level))
	}
	rows := chunk(buttons, 2)
	rows = append(rows, []telegram.InlineKeyboardButton{telegram.CallbackButton("← Back", "model_provider:"+provider+":"+string(scope))})
	return &telegram.InlineKeyboardMarkup{InlineKeyboard: rows}
}

// PickerHeader is the picker text on its first step (and on Back). Labels
// are escaped: a "⚠️ <reason>" label may carry error text.
func PickerHeader(p Picker, scope Scope, picked *string) string {
	switch scope {
	case ScopeNewThread:
		return "Pick a model for a new thread. Default: <code>" + esc(p.Label) + "</code>"
	case ScopeNewOrchestrator:
		return "Pick a model for a new 🧭 orchestrator (other topics keep theirs). Default: <code>" + esc(p.Label) + "</code>"
	case ScopeStaging:
		text := "Pick the model for the new topic (this thread keeps its own). Starts on: <code>" + esc(p.Label) + "</code>"
		if picked != nil {
			text += "\nPicked: <code>" + esc(*picked) + "</code>"
		}
		return text
	case ScopeTopic:
		text := "Current model: <code>" + esc(p.Label) + "</code>"
		if p.Override != "" {
			text += "\nCurrent override: <code>" + esc(p.Override) + "</code>"
		}
		return text
	}
	return "Current model: <code>" + esc(p.Label) + "</code>"
}

// showHeader is the text /model and /model list open the picker with.
func showHeader(p Picker, scope Scope) string {
	if scope == ScopeTopic && p.Overridden {
		return "Current model: <code>" + esc(p.Label) + "</code> (topic override)\nDefault: <code>" + esc(p.DefaultLabel) + "</code>"
	}
	return "Current model: <code>" + esc(p.Label) + "</code>"
}

func (h *Handler) modelCommand(ctx context.Context, m Message, t Thread, mc ModelCommand) bool {
	if h.Models == nil || (mc.Action != ModelShow && mc.Action != ModelList) {
		return false
	}
	scope := ScopeTopic
	if m.InGeneral() {
		if h.Defaults == nil {
			h.reply(ctx, m, chatDefaultUnavailable, nil)
			return true
		}
		scope = ScopeGeneral
	}
	q := PickerQuery{Chat: m.Chat, Scope: scope}
	if scope == ScopeTopic {
		q.ThreadID = t.ID
	}
	p, err := h.Models.Picker(ctx, q)
	if err != nil {
		h.reply(ctx, m, "❌ "+esc(err.Error()), nil)
		return true
	}
	h.reply(ctx, m, showHeader(p, scope), ProviderKeyboard(p, scope))
	return true
}

// pickerTarget is what a picker callback acts on.
type pickerTarget struct {
	chat, message int64
	topic         *int64
	// thread is the topic's effective thread (ScopeTopic, ScopeStaging).
	thread string
	// card is the live staged command (ScopeStaging).
	card StagingCard
}

func (h *Handler) query(t pickerTarget, scope Scope) PickerQuery {
	q := PickerQuery{Chat: t.chat, Scope: scope}
	switch scope {
	case ScopeTopic:
		q.ThreadID = t.thread
	case ScopeStaging:
		q.ThreadID, q.Profile = t.thread, t.card.Profile
	}
	return q
}

// modelCallback handles model_provider:, model_pick:, model_mode:,
// model_thinking:, model_back: and model_cancel:.
func (h *Handler) modelCallback(ctx context.Context, q *telegram.CallbackQuery, data string) bool {
	kind, rest, _ := strings.Cut(data, ":")
	switch kind {
	case "model_provider", "model_pick", "model_mode", "model_thinking", "model_back", "model_cancel":
	default:
		return false
	}
	msg := q.Message
	if msg == nil {
		h.answer(ctx, q, "No message context")
		return true
	}
	scope, ok := callbackScope(kind, rest)
	if !ok {
		h.answer(ctx, q, "")
		return true
	}
	if _, launch := launchScope(scope); launch && h.Launches == nil {
		h.answer(ctx, q, "Not available in this gateway build yet")
		return true
	}
	if scope == ScopeGeneral && h.Defaults == nil {
		h.answer(ctx, q, "Not available in this gateway build yet")
		return true
	}
	target := pickerTarget{chat: msg.Chat.ID, message: msg.ID, topic: msg.EffectiveThreadID()}
	if scope == ScopeTopic || scope == ScopeStaging {
		t, err := h.Resolver.ResolveThread(ctx, target.chat, target.topic)
		if err != nil {
			h.log().Warn("failed to resolve a picker's thread", "chat_id", target.chat, "err", err)
			h.answer(ctx, q, "Couldn't read this topic; try again")
			return true
		}
		target.thread = t.ID
	}
	if scope == ScopeStaging {
		card, live := h.Card(target.thread)
		if !live {
			h.edit(ctx, target.chat, target.message, "⚠️ Session expired — re-run the command.", nil)
			h.answer(ctx, q, "Session expired — re-run the command")
			return true
		}
		target.card = card
	}
	if kind == "model_cancel" {
		h.pickerCancel(ctx, q, target, scope)
		return true
	}
	p, err := h.Models.Picker(ctx, h.query(target, scope))
	if err != nil {
		h.log().Warn("failed to read the model picker", "chat_id", target.chat, "err", err)
		h.answer(ctx, q, truncateRunes(err.Error(), 200))
		return true
	}
	switch kind {
	case "model_provider":
		provider, _, _ := strings.Cut(rest, ":")
		h.edit(ctx, target.chat, target.message, "Select a <b>"+esc(providerLabel(p, provider))+"</b> model:", ModelsKeyboard(p, provider, scope))
		h.answer(ctx, q, "")
	case "model_back":
		h.edit(ctx, target.chat, target.message, PickerHeader(p, scope, target.card.Model), ProviderKeyboard(p, scope))
		h.answer(ctx, q, "")
	case "model_pick":
		h.pickModel(ctx, q, target, scope, p, rest)
	case "model_thinking":
		h.pickThinking(ctx, q, target, scope, p, rest)
	case "model_mode":
		name, _, _ := cutLast(rest, ":")
		for _, m := range p.Modes {
			if strings.EqualFold(strings.TrimSpace(m.Name), strings.TrimSpace(name)) {
				h.applyPick(ctx, q, target, scope, p, m.Ref)
				return true
			}
		}
		h.answer(ctx, q, "Mode no longer configured")
	}
	return true
}

func cutLast(s, sep string) (before, after string, ok bool) {
	i := strings.LastIndex(s, sep)
	if i < 0 {
		return s, "", false
	}
	return s[:i], s[i+len(sep):], true
}

// callbackScope reads the scope of picker data, field by field like the
// Rust parsers (a provider key never holds ':', a mode name may not).
func callbackScope(kind, rest string) (Scope, bool) {
	var s string
	switch kind {
	case "model_provider":
		_, s, _ = strings.Cut(rest, ":")
	case "model_back", "model_cancel":
		s = rest
	case "model_thinking":
		head, _, ok := cutLast(rest, ":")
		if !ok {
			return "", false
		}
		_, s, _ = cutLast(head, ":")
	default:
		_, s, _ = cutLast(rest, ":")
	}
	return parseScope(s)
}

// indexed reads `provider:index` (the provider may hold `@account`).
func indexed(p Picker, s string) (PickerModel, string, int, string) {
	provider, idx, ok := cutLast(s, ":")
	if !ok {
		return PickerModel{}, "", 0, "Invalid model selection"
	}
	i, err := strconv.Atoi(idx)
	if err != nil || i < 0 {
		return PickerModel{}, "", 0, "Invalid model selection"
	}
	models := providerModels(p.Models, provider)
	if i >= len(models) {
		return PickerModel{}, "", 0, "Model no longer available"
	}
	return models[i], provider, i, ""
}

func (h *Handler) pickModel(ctx context.Context, q *telegram.CallbackQuery, t pickerTarget, scope Scope, p Picker, rest string) {
	head, _, _ := cutLast(rest, ":")
	m, provider, i, problem := indexed(p, head)
	if problem != "" {
		h.answer(ctx, q, problem)
		return
	}
	h.edit(ctx, t.chat, t.message, "Select thinking for <code>"+esc(m.Name)+"</code>:", ThinkingKeyboard(provider, i, scope, p.Thinking))
	h.answer(ctx, q, "")
}

func (h *Handler) pickThinking(ctx context.Context, q *telegram.CallbackQuery, t pickerTarget, scope Scope, p Picker, rest string) {
	head, level, _ := cutLast(rest, ":")
	head, _, _ = cutLast(head, ":") // drop the scope
	m, _, _, problem := indexed(p, head)
	spec, known := m.Specs[strings.ToLower(strings.TrimSpace(level))]
	if problem != "" || !known {
		if problem == "" {
			problem = "Model no longer available"
		}
		h.answer(ctx, q, problem)
		return
	}
	h.applyPick(ctx, q, t, scope, p, spec)
}

// applyPick applies a finished pick (a spec or a `mode:<name>` reference)
// to what the scope targets.
func (h *Handler) applyPick(ctx context.Context, q *telegram.CallbackQuery, t pickerTarget, scope Scope, p Picker, model string) {
	if p.ConfigError != "" {
		h.answer(ctx, q, p.ConfigError)
		return
	}
	if role, ok := launchScope(scope); ok {
		if p.LaunchError != "" {
			h.answer(ctx, q, p.LaunchError)
			return
		}
		_, err := h.launchTopic(ctx, t.chat, model, role)
		h.answerLaunched(ctx, q, role, err)
		h.renderLauncher(ctx, t.chat, t.message, role)
		return
	}
	switch scope {
	case ScopeStaging:
		selection, err := h.Models.ResolveStagingPick(ctx, t.chat, t.thread, t.card.Profile, model)
		if err != nil {
			h.answer(ctx, q, err.Error())
			return
		}
		if h.FinishModelPick(ctx, t.thread, t.chat, t.message, &selection) {
			h.answer(ctx, q, "New topic: "+selection)
			return
		}
		h.edit(ctx, t.chat, t.message, "⚠️ Session expired — re-run the command.", nil)
		h.answer(ctx, q, "Session expired — re-run the command")
	case ScopeGeneral:
		text := "✅ Default model set to <code>" + esc(model) + "</code>."
		if err := h.Defaults.SetChatModel(ctx, t.chat, model); err != nil {
			text = "❌ Failed to save model: " + esc(err.Error())
		}
		h.edit(ctx, t.chat, t.message, text, nil)
		h.answer(ctx, q, "")
	case ScopeTopic:
		text := ""
		if label, err := h.Models.SetTopicModel(ctx, t.chat, t.thread, model); err != nil {
			text = "❌ " + esc(err.Error())
		} else {
			text = "✅ Model set to <code>" + esc(label) + "</code> for this topic."
		}
		h.edit(ctx, t.chat, t.message, text, nil)
		h.answer(ctx, q, "")
	}
}

func (h *Handler) pickerCancel(ctx context.Context, q *telegram.CallbackQuery, t pickerTarget, scope Scope) {
	defer h.answer(ctx, q, "")
	if role, ok := launchScope(scope); ok {
		h.renderLauncher(ctx, t.chat, t.message, role)
		return
	}
	if scope == ScopeStaging {
		if !h.FinishModelPick(ctx, t.thread, t.chat, t.message, nil) {
			h.edit(ctx, t.chat, t.message, "⚠️ Session expired — re-run the command.", nil)
		}
		return
	}
	p, err := h.Models.Picker(ctx, h.query(t, scope))
	if err != nil {
		h.log().Warn("failed to read the model picker", "chat_id", t.chat, "err", err)
		h.edit(ctx, t.chat, t.message, "Model change cancelled.", nil)
		return
	}
	h.edit(ctx, t.chat, t.message, "Model change cancelled. Current model: <code>"+esc(p.Model)+"</code>", nil)
}

// openStagingPicker is a staging card's `🎛 Model`.
func (h *Handler) openStagingPicker(ctx context.Context, q *telegram.CallbackQuery, threadID string, card StagingCard) {
	msg := q.Message
	p, err := h.Models.Picker(ctx, PickerQuery{Chat: msg.Chat.ID, Scope: ScopeStaging, ThreadID: threadID, Profile: card.Profile})
	if err != nil {
		h.log().Warn("failed to open the staging model picker", "err", err)
		h.answer(ctx, q, truncateRunes(err.Error(), 200))
		return
	}
	h.edit(ctx, msg.Chat.ID, msg.ID, PickerHeader(p, ScopeStaging, card.Model), ProviderKeyboard(p, ScopeStaging))
	h.answer(ctx, q, "")
}
