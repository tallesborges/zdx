// Package commands handles the bot's slash commands, the staged commands
// (/handoff, /btw, /prompt_builder) and the /commands picker, ported from
// the Rust bot: crates/zdx-bot/src/commands.rs, handlers/message/commands.rs,
// staging.rs and command_picker.rs.
//
// It decides nothing the engine owns. Thread kinds and profiles, models,
// generated text, custom command discovery and seeded-thread metadata come
// from injected Rust operations (see deps.go). A command whose dependency is
// not wired is not handled, so the bot answers it as unsupported instead of
// faking it.
//
// The package does not import internal/bot: the bot adapts its own types to
// Message, Thread and the interfaces here.
package commands

import (
	"strings"
	"unicode"

	"github.com/tallesborges/zdx/apps/gateway/internal/telegram"
)

// Command is a built-in bot command (Rust BotCommand). /model and /queue
// have their own parsers.
type Command string

const (
	CmdNew           Command = "new"
	CmdRestart       Command = "restart"
	CmdStatus        Command = "status"
	CmdWhereAmI      Command = "whereami"
	CmdWorktree      Command = "worktree"
	CmdHandoff       Command = "handoff"
	CmdBtw           Command = "btw"
	CmdCommands      Command = "commands"
	CmdTldr          Command = "tldr"
	CmdPromptBuilder Command = "prompt_builder"
	CmdThreadID      Command = "threadid"
	CmdThreads       Command = "threads"
	CmdLauncher      Command = "launcher"
)

type commandDef struct {
	command     Command
	patterns    []string
	description string
}

// commandDefs is Rust's COMMAND_DEFS, in its order. Every entry blocks topic
// auto-creation; the menu name is the Command value.
var commandDefs = []commandDef{
	{CmdNew, []string{"/new"}, "Start a new conversation"},
	{CmdRestart, []string{"/restart"}, "Restart bot and daemon (f = force now, q = when idle)"},
	{CmdStatus, []string{"/status"}, "Show thread, model, usage, and pricing"},
	{CmdWhereAmI, []string{"/whereami"}, "Show chat ID, topic ID, and profile/cwd binding"},
	{CmdWorktree, []string{"/worktree create", "/worktree", "/wt"}, "Enable worktree for this thread"},
	{CmdHandoff, []string{"/handoff"}, "Hand off this topic's context into a new topic"},
	{CmdBtw, []string{"/btw"}, "Ask a side question in a new topic"},
	{CmdCommands, []string{"/commands"}, "Show commands available in this project context"},
	{CmdTldr, []string{"/tldr"}, "Recap this topic's conversation"},
	{CmdPromptBuilder, []string{"/prompt-builder", "/prompt_builder", "/promptbuilder"}, "Draft a polished prompt from a short intent"},
	{CmdThreadID, []string{"/threadid", "/thread_id", "/thread-id"}, "Show only the thread ID"},
	{CmdThreads, []string{"/threads", "/thread"}, "Open the Mini App thread viewer"},
	{CmdLauncher, []string{"/launcher", "/menu"}, "New agent or orchestrator thread on a chosen model (General only)"},
}

// Parse recognises a built-in command: the exact command, or the command
// addressed to a bot (`/status@bot`). Arguments make it no command, except
// for /restart's modes.
func Parse(text string) (Command, bool) {
	t := strings.TrimSpace(text)
	if _, ok := ParseRestart(t); ok {
		return CmdRestart, true
	}
	for _, def := range commandDefs {
		if def.command == CmdRestart {
			continue
		}
		for _, p := range def.patterns {
			if telegram.CommandMatches(t, p) {
				return def.command, true
			}
		}
	}
	return "", false
}

// RestartMode is how /restart treats active agent runs.
type RestartMode string

const (
	// RestartGated refuses while any agent run is active.
	RestartGated RestartMode = "gated"
	// RestartForce interrupts active runs (`f`, `force`, `--force`).
	RestartForce RestartMode = "force"
	// RestartQueued restarts once no agent run is active (`q`, `queue`).
	RestartQueued RestartMode = "queued"
)

// ParseRestart parses /restart strictly: one optional mode word.
func ParseRestart(text string) (RestartMode, bool) {
	parts := strings.Fields(text)
	if len(parts) == 0 || !isCommandWord(parts[0], "/restart") {
		return "", false
	}
	switch {
	case len(parts) == 1:
		return RestartGated, true
	case len(parts) > 2:
		return "", false
	}
	switch parts[1] {
	case "f", "force", "--force":
		return RestartForce, true
	case "q", "queue":
		return RestartQueued, true
	}
	return "", false
}

// isCommandWord: word is command, or command@<non-empty bot name>.
func isCommandWord(word, command string) bool {
	if word == command {
		return true
	}
	name, ok := strings.CutPrefix(word, command+"@")
	return ok && name != ""
}

// ModelAction is a /model subcommand.
type ModelAction string

const (
	// ModelShow is /model (and any form not listed below): the picker.
	ModelShow ModelAction = "show"
	// ModelList is /model list, which also shows the picker.
	ModelList  ModelAction = "list"
	ModelSet   ModelAction = "set"
	ModelReset ModelAction = "reset"
)

// ModelCommand is a parsed /model command. Model is set for ModelSet.
type ModelCommand struct {
	Action ModelAction
	Model  string
}

// ParseModel parses a /model command like Rust parse_model_command.
func ParseModel(text string) (ModelCommand, bool) {
	t := strings.TrimSpace(text)
	var rest string
	switch {
	case strings.HasPrefix(t, "/model@"):
		after := strings.TrimPrefix(t, "/model")
		if i := strings.IndexByte(after, ' '); i >= 0 {
			rest = after[i:]
		}
	case t == "/model" || strings.HasPrefix(t, "/model "):
		rest = strings.TrimPrefix(t, "/model")
	default:
		return ModelCommand{}, false
	}
	parts := strings.Fields(rest)
	switch {
	case len(parts) == 1 && parts[0] == "list":
		return ModelCommand{Action: ModelList}, true
	case len(parts) >= 2 && parts[0] == "set":
		return ModelCommand{Action: ModelSet, Model: parts[1]}, true
	case len(parts) == 1 && parts[0] == "reset":
		return ModelCommand{Action: ModelReset}, true
	}
	return ModelCommand{Action: ModelShow}, true
}

// ParseQueue is `/queue <text>`: the text to queue as a normal turn, never
// steered into the running one. false for an empty /queue or other text.
func ParseQueue(text string) (string, bool) {
	t := strings.TrimLeftFunc(text, unicode.IsSpace)
	command, rest := t, ""
	if i := strings.IndexFunc(t, unicode.IsSpace); i >= 0 {
		command, rest = t[:i], t[i:]
	}
	rest = strings.TrimSpace(rest)
	return rest, isCommandWord(command, "/queue") && rest != ""
}

// BypassesQueue reports commands answered while the topic's turn runs:
// they never touch the running thread.
func BypassesQueue(text string) bool {
	c, ok := Parse(text)
	if !ok {
		return false
	}
	switch c {
	case CmdStatus, CmdWhereAmI, CmdTldr, CmdThreadID, CmdBtw, CmdCommands, CmdRestart:
		return true
	}
	return false
}

// BlocksTopicAutocreate reports a command sent in General that must not open
// a new topic (every built-in command and /model).
func BlocksTopicAutocreate(text string) bool {
	if _, ok := Parse(text); ok {
		return true
	}
	_, ok := ParseModel(text)
	return ok
}

// MenuCommands is the native `/` menu (setMyCommands), in Rust's order.
func MenuCommands() []telegram.BotCommand {
	out := make([]telegram.BotCommand, 0, len(commandDefs)+2)
	for _, def := range commandDefs {
		out = append(out, telegram.BotCommand{Command: string(def.command), Description: def.description})
	}
	return append(out,
		telegram.BotCommand{Command: "model", Description: "View or change model and thinking"},
		telegram.BotCommand{Command: "queue", Description: "Queue a message instead of adding it to the running turn"},
	)
}

// NativeNames are the command names custom `.md` commands may not shadow in
// the /commands picker. The Rust command discovery takes them as input.
func NativeNames() []string {
	out := make([]string, 0, len(commandDefs)+3)
	for _, def := range commandDefs {
		out = append(out, string(def.command))
	}
	return append(out, "model", "cancel", "queue")
}
