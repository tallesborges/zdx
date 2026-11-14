package commands

import (
	"context"
	"time"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Message is one incoming chat message, as the bot hands it over.
type Message struct {
	Chat int64
	// ChatType is Telegram's chat type: private, group, supergroup.
	ChatType string
	IsForum  bool
	// Topic is the message's forum topic (Message.EffectiveThreadID).
	Topic  *int64
	ID     int64
	UserID int64
	// Text is the message text; empty without text.
	Text string
	// Transcript is the first voice transcript, used as staged input when
	// there is no text.
	Transcript string
	// HasMedia: the message carries images or audio. Commands are never
	// read from such a message (Rust handlers), staged input still is.
	HasMedia bool
	// Synthetic: a bot-generated prompt (Mini App feedback, a dispatched
	// seed). It never starts or completes a staged command.
	Synthetic bool
	// ReplyTo is the message an answer quotes (nil for a topic's creation
	// message).
	ReplyTo *int64
}

// InGeneral reports a forum chat's General topic.
func (m Message) InGeneral() bool { return m.IsForum && m.Topic == nil }

// Thread is Rust's view of the conversation (resolve_thread).
type Thread struct {
	// ID is the thread turns run on: the conversation's id after one
	// alias_to hop (effective_thread_id).
	ID string
	// Kind is ordinary, profile or worker_mirror.
	Kind string
	// Profile is the persistent profile of a profile thread.
	Profile string
}

// Thread kinds, as resolve_thread reports them.
const (
	KindOrdinary     = "ordinary"
	KindProfile      = "profile"
	KindWorkerMirror = "worker_mirror"
)

// OrchestratorProfile is the reserved persistent profile of orchestrator
// threads (zdx_engine::subagents::ORCHESTRATOR_SUBAGENT_NAME).
const OrchestratorProfile = "orchestrator"

// Telegram is the Bot API subset commands use. *telegram.Client has these
// methods.
type Telegram interface {
	SendText(ctx context.Context, s telegram.TextSend) (*telegram.Message, error)
	EditMessageText(ctx context.Context, chatID, messageID int64, text string, markup *telegram.InlineKeyboardMarkup) error
	EditMessageReplyMarkup(ctx context.Context, chatID, messageID int64, markup *telegram.InlineKeyboardMarkup) error
	DeleteMessage(ctx context.Context, chatID, messageID int64) error
	AnswerCallbackQuery(ctx context.Context, id string, text *string) error
}

// Topics are the forum Bot API calls (createForumTopic, deleteForumTopic,
// pinChatMessage).
type Topics interface {
	CreateForumTopic(ctx context.Context, chat int64, name string) (topicID int64, err error)
	DeleteForumTopic(ctx context.Context, chat, topic int64) error
	// PinChatMessage pins silently (disable_notification).
	PinChatMessage(ctx context.Context, chat, messageID int64) error
}

// Binding is a chat's resolved workspace, from the Rust-resolved gateway
// settings (`zdx telegram settings`).
type Binding struct {
	// Profile is the bot profile bound to the chat; "" when none.
	Profile string
	// Root is the profile's folder, else the bot's default root.
	Root string
}

// Chats answers per-chat settings. Nothing here is derived in Go: the
// values come from the Rust-resolved settings.
type Chats interface {
	Binding(chat int64) Binding
	// ChatAllowed reports a group chat on the bot allowlist.
	ChatAllowed(chat int64) bool
	// MiniAppURL is the Mini App base URL for the chat, "" when the Mini
	// App is not configured.
	MiniAppURL(chat int64) string
	// OrchestratorChat: the chat's profile opted in to orchestrator topics
	// (telegram.profiles.<name>.orchestrator). The launcher starts there.
	OrchestratorChat(chat int64) bool
}

// Resolver asks Rust what a conversation's thread is. Button handlers use
// it: a callback carries only its message's chat and topic.
type Resolver interface {
	ResolveThread(ctx context.Context, chat int64, topic *int64) (Thread, error)
}

// Dispatcher runs a bot-made message through normal intake, as if the user
// sent it (Rust dispatch_message with a synthetic message). The bot owns
// it; the message must reach the topic's turn even though no update came.
type Dispatcher interface {
	Dispatch(ctx context.Context, m Synthetic)
}

// Synthetic is a message the bot dispatches on the user's behalf.
type Synthetic struct {
	Chat     int64
	ChatType string
	IsForum  bool
	Topic    *int64
	// MessageID is the message the turn's answers quote: the topic id of a
	// seeded topic, the tapped message of a picker or Accept.
	MessageID int64
	UserID    int64
	Text      string
}

// Generator runs the engine's one-shot generations. Each needs the Rust
// `generate` capability, which is not in the protocol yet; an adapter must
// check it and fail with engine.ErrUnsupported, never answer locally. Rust
// picks the model and root (config.tldr_model / handoff_model /
// prompt_builder_model, the thread's root or the chat's).
type Generator interface {
	// TLDR recaps a thread (core::tldr_generation::generate_tldr).
	TLDR(ctx context.Context, chat int64, threadID string) (string, error)
	// Handoff builds the seed of a /handoff topic from the thread and the
	// user's goal (core::handoff_generation::generate_handoff).
	Handoff(ctx context.Context, chat int64, threadID, goal string) (string, error)
	// PromptBuilder drafts a prompt from a short intent
	// (core::prompt_builder_generation::generate_prompt_builder).
	PromptBuilder(ctx context.Context, chat int64, threadID, intent string) (string, error)
}

// StartModel is the model a seeded topic starts with when nothing is picked
// on the staging card (Rust staging::new_topic_model).
type StartModel struct {
	// Pin is the selection the new thread is created with; nil means none:
	// the topic resolves its role's default on every turn.
	Pin *string
	// Label is the display label the model picker shows.
	Label string
}

// Seeds are the Rust operations that open a seeded topic (/handoff, /btw).
type Seeds interface {
	// StartModel resolves the starting model of a topic seeded from source
	// as profile ("" for an ordinary agent). It fails, and nothing is
	// created or written, when the chat's config does not load, the profile
	// model does not resolve, or the source's selection names a mode the
	// new topic's config does not define (fail closed: never the default).
	StartModel(ctx context.Context, chat int64, source, profile string) (StartModel, error)
	// SideQuestionSeed is the /btw seed: a parent-thread header and the
	// labelled question (core::handoff_generation::build_side_thread_seed).
	SideQuestionSeed(ctx context.Context, source, question string) (string, error)
}

// ThreadSetup is the metadata a new topic's thread gets (update_thread on a
// thread Rust has not written yet). Zero fields are left out.
type ThreadSetup struct {
	ThreadID string
	// HandoffFrom is the lineage of a /handoff or /btw thread.
	HandoffFrom string
	// Profile makes the thread a persistent-profile thread.
	Profile string
	// Model is the thread's model selection (a spec or `mode:<name>`).
	Model *string
	// AliasTo makes the topic resume another thread.
	AliasTo string
	// Root is the folder the thread runs in (a worktree).
	Root string
	// PendingTitle: the first turn titles the topic.
	PendingTitle bool
}

// Threads writes thread metadata. Rust owns the files; this goes through
// update_thread.
type Threads interface {
	SetupThread(ctx context.Context, s ThreadSetup) error
}

// TopicBlocker refuses turns in a topic whose orchestrator setup failed
// (Rust abandon_orchestrator_topic / orchestrator_setup_failed). It must
// hold across restarts: the topic has no profile, so its messages would
// otherwise run the default coding agent.
type TopicBlocker interface {
	BlockTopic(ctx context.Context, threadID string) error
}

// CustomCommand is one Markdown command the /commands picker offers.
type CustomCommand struct {
	Name        string
	Description string
	// Source is project, user or builtin.
	Source string
	// Content is the prompt a tap sends.
	Content string
}

// CommandSource lists custom commands for a thread
// (zdx_engine::custom_commands::load_custom_commands, rooted at the thread's
// root or the chat's, never shadowing NativeNames).
type CommandSource interface {
	CustomCommands(ctx context.Context, chat int64, threadID string, native []string) ([]CustomCommand, error)
}

// WorkerCanceller cancels a managed worker (cancel_worker). A nil error
// means the service answered with the worker. A refusal from the service
// must come back as *engine.ServiceError and is shown as refused (never as
// an idle worker: an unmanaged worker may still run); any other error is
// read as "outcome unknown".
type WorkerCanceller interface {
	CancelWorker(ctx context.Context, workerThreadID string) error
}

// Worktrees enables a git worktree for a thread
// (core::worktree::ensure_worktree; CLI `zdx --root <root> worktree ensure
// <thread-id>`, which prints the path).
type Worktrees interface {
	EnsureWorktree(ctx context.Context, root, threadID string) (path string, err error)
}

// Scope is what a model picker targets. Its value is the callback token,
// which differs from the catalog wire for the launcher scopes:
// newthread = "new_thread", neworch = "new_orchestrator".
type Scope string

const (
	// ScopeGeneral edits the chat's default model (/model in General).
	ScopeGeneral Scope = "general"
	// ScopeTopic sets the topic's own model (/model in a topic).
	ScopeTopic Scope = "topic"
	// ScopeNewThread creates an agent topic (launcher 🎛 Custom).
	ScopeNewThread Scope = "newthread"
	// ScopeNewOrchestrator creates an orchestrator topic.
	ScopeNewOrchestrator Scope = "neworch"
	// ScopeStaging picks the model of a staged command's new topic.
	ScopeStaging Scope = "staging"
)

func parseScope(s string) (Scope, bool) {
	switch v := Scope(s); v {
	case ScopeGeneral, ScopeTopic, ScopeNewThread, ScopeNewOrchestrator, ScopeStaging:
		return v, true
	}
	return "", false
}

// PickerQuery asks for the model picker of one scope.
type PickerQuery struct {
	Chat  int64
	Scope Scope
	// ThreadID: ScopeTopic, the topic's effective thread; ScopeStaging, the
	// source thread of the staged command.
	ThreadID string
	// Profile: ScopeStaging, the profile the new topic starts with.
	Profile string
}

// PickerModel is one model the picker offers.
type PickerModel struct {
	// ID is the qualified id (`provider:id`, `provider@account:id`).
	ID string
	// Name is the id without its thinking suffix, for "Select thinking".
	Name string
	// Specs is the spec to pick per thinking level display name
	// (models::format_model_thinking), for every level in ThinkingLevels.
	Specs map[string]string
}

// PickerMode is one model mode the picker offers.
type PickerMode struct {
	Name string
	// Ref is the selection a pick keeps (ModelMode::as_ref_spec).
	Ref string
}

// Picker is everything Rust derives for a model picker. Go only lays it
// out.
type Picker struct {
	// ConfigError: the chat's workspace config does not load; picks are
	// refused with this text (checked_config_for_chat).
	ConfigError string
	// LaunchError: ScopeNewThread/ScopeNewOrchestrator, the role's start
	// config does not resolve (launch_config); picks are refused.
	LaunchError string
	// Models is the chat's subagent_available_models, in order.
	Models []PickerModel
	// ProviderLabels names each provider key (providers::provider_key_label).
	ProviderLabels map[string]string
	// Modes is picker_modes for the scope (empty for launcher scopes).
	Modes []PickerMode
	// Label is the model the header shows: the chat's, the topic's
	// selection label, the role default, or a staged topic's start label,
	// or the "⚠️ …" reason it does not resolve.
	Label string
	// DefaultLabel is the chat's model label (ScopeTopic header).
	DefaultLabel string
	// Overridden: ScopeTopic, the topic has its own model or thinking.
	Overridden bool
	// Override: ScopeTopic, the stored model override.
	Override string
	// Model is the current model spec, for "Model change cancelled".
	Model string
	// Thinking is the current thinking level display name.
	Thinking string
}

// Models are the Rust model operations of the picker and of staged picks.
type Models interface {
	Picker(ctx context.Context, q PickerQuery) (Picker, error)
	// SetTopicModel validates model against the topic's config and stores
	// it on threadID (update_thread with the chat root as config_root).
	// label is the stored selection's label.
	SetTopicModel(ctx context.Context, chat int64, threadID, model string) (label string, err error)
	// ResolveStagingPick resolves a pick against the config a staged
	// command's new topic starts with and returns the selection to keep.
	ResolveStagingPick(ctx context.Context, chat int64, source, profile, model string) (string, error)
}

// ChatDefaults writes a chat's default model (BotContext::set_chat_model:
// the workspace config of the chat's root). No Rust op exists yet.
type ChatDefaults interface {
	SetChatModel(ctx context.Context, chat int64, model string) error
}

// Launch roles.
const (
	RoleAgent        = "agent"
	RoleOrchestrator = "orchestrator"
)

// LaunchMode is one mode on the launcher.
type LaunchMode struct {
	Name string
	// Ref is the selection a tap pins (`mode:<name>`).
	Ref string
	// Primary is the mode's primary spec as configured.
	Primary string
	// Thinking is the mode's thinking level display name.
	Thinking    string
	Description string
}

// LauncherView is what Rust derives for the launcher of one role.
type LauncherView struct {
	// ConfigError: the chat's workspace config does not load.
	ConfigError string
	// LaunchError: the role's default does not resolve; the header warns
	// and new topics of the role are refused.
	LaunchError string
	// Modes are the bot-visible modes (configured, primary available).
	Modes []LaunchMode
	// DefaultMode is the default's `mode:<name>` reference, "" for none.
	DefaultMode string
	// DefaultModel is the default spec without its thinking suffix.
	DefaultModel string
	// DefaultThinking is the default thinking level display name.
	DefaultThinking string
}

// ResumeThread is one thread the launcher's 🔄 Continue offers.
type ResumeThread struct {
	ID string
	// Title is ThreadSummary::display_title.
	Title    string
	Modified *time.Time
}

// Launches are the Rust reads of the launcher.
type Launches interface {
	LauncherView(ctx context.Context, chat int64, role string) (LauncherView, error)
	// Resumable lists the chat project's resumable threads, newest first:
	// top-level, not `telegram-*`, not a managed worker, root equal to the
	// chat's root, at most 8 (launcher resumable_threads).
	Resumable(ctx context.Context, chat int64) ([]ResumeThread, error)
	// ResumeSource checks that source can be resumed (exists, is no
	// `telegram-*` thread, is no alias) and returns its title, "" when it
	// has none.
	ResumeSource(ctx context.Context, source string) (title string, err error)
}

// Usage is a thread's token counts.
type Usage struct {
	Input, Output, CacheRead, CacheWrite uint64
}

// Pricing kinds.
const (
	PricingUnknown      = "unknown"
	PricingSubscription = "subscription"
	PricingPriced       = "priced"
)

// Pricing is the cost line of a status card.
type Pricing struct {
	Kind string
	// Cost and Savings are the thread's total cost and cache savings, in
	// dollars (ModelPricing::cost / cache_savings), for PricingPriced.
	Cost, Savings float64
	// Rates per 1M tokens: input, output, cache read, cache write.
	Input, Output, CacheRead, CacheWrite float64
}

// StatusWorker is one worker of an orchestrator card, newest first.
type StatusWorker struct {
	ThreadID string
	// Title is the worker's title, "" for none.
	Title     string
	Status    string // running, queued, completed, failed, cancelled
	MirrorURL string
}

// ThreadStatus is Rust's status projection of a thread (/status, the
// pinned header). Model sources are default, orchestrator or topic.
type ThreadStatus struct {
	// ConfigError: the card shows only this (the chat's config or the
	// thread root's modes do not load).
	ConfigError string
	// Orchestrator: an orchestrator thread; Workers is its worker list.
	Orchestrator bool
	// Refused is why the stored selection does not resolve here.
	Refused        string
	Model          string
	ModelSource    string
	Mode           string
	Thinking       string
	ThinkingSource string
	ThreadID       string
	// Profile is the chat's bot profile, "" for the fallback. Not in the
	// Rust projection: Handler fills it from Chats.Binding.
	Profile string
	Root    string
	// Branch is the root's git branch, "" when none.
	Branch string
	// ContextLimit is the model's context window, 0 when unknown.
	ContextLimit  uint64
	ContextTokens uint64
	Usage         Usage
	Pricing       Pricing
	Workers       []StatusWorker
	// ParentOrchestrator is the orchestrator thread that owns this one
	// (a worker's parent with the orchestrator profile), "" for none.
	ParentOrchestrator string
}

// Statuses reads the status projection of a thread.
type Statuses interface {
	ThreadStatus(ctx context.Context, chat int64, threadID string) (ThreadStatus, error)
}

var (
	_ Telegram = (*telegram.Client)(nil)
	_ Topics   = (*telegram.Client)(nil)
)
