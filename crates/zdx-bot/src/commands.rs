#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BotCommand {
    New,
    Restart,
    Status,
    WhereAmI,
    WorktreeCreate,
    Handoff,
    Btw,
    Commands,
    Tldr,
    PromptBuilder,
    ThreadId,
    Threads,
    Launcher,
    Goal,
    GoalClear,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TelegramCommandSpec {
    pub command: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CommandDef {
    command: BotCommand,
    patterns: &'static [&'static str],
    blocks_topic_autocreate: bool,
    telegram_spec: TelegramCommandSpec,
}

const COMMAND_DEFS: &[CommandDef] = &[
    CommandDef {
        command: BotCommand::New,
        patterns: &["/new"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "new",
            description: "Start a new conversation",
        },
    },
    CommandDef {
        command: BotCommand::Restart,
        patterns: &["/restart"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "restart",
            description: "Restart bot and daemon (f = force now, q = when idle)",
        },
    },
    CommandDef {
        command: BotCommand::Status,
        patterns: &["/status"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "status",
            description: "Show thread, model, usage, and pricing",
        },
    },
    CommandDef {
        command: BotCommand::WhereAmI,
        patterns: &["/whereami"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "whereami",
            description: "Show chat ID, topic ID, and profile/cwd binding",
        },
    },
    CommandDef {
        command: BotCommand::WorktreeCreate,
        patterns: &["/worktree create", "/worktree", "/wt"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "worktree",
            description: "Enable worktree for this thread",
        },
    },
    CommandDef {
        command: BotCommand::Handoff,
        patterns: &["/handoff"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "handoff",
            description: "Hand off this topic's context into a new topic",
        },
    },
    CommandDef {
        command: BotCommand::Btw,
        patterns: &["/btw"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "btw",
            description: "Ask a side question in a new topic",
        },
    },
    CommandDef {
        command: BotCommand::Goal,
        patterns: &["/goal"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "goal",
            description: "Work autonomously until a verifier says the goal is done",
        },
    },
    CommandDef {
        command: BotCommand::GoalClear,
        patterns: &["/goal_clear", "/goal-clear", "/goalclear"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "goal_clear",
            description: "Stop the active goal in this topic",
        },
    },
    CommandDef {
        command: BotCommand::Commands,
        patterns: &["/commands"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "commands",
            description: "Show commands available in this project context",
        },
    },
    CommandDef {
        command: BotCommand::Tldr,
        patterns: &["/tldr"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "tldr",
            description: "Recap this topic's conversation",
        },
    },
    CommandDef {
        command: BotCommand::PromptBuilder,
        patterns: &["/prompt-builder", "/prompt_builder", "/promptbuilder"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "prompt_builder",
            description: "Draft a polished prompt from a short intent",
        },
    },
    CommandDef {
        command: BotCommand::ThreadId,
        patterns: &["/threadid", "/thread_id", "/thread-id"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "threadid",
            description: "Show only the thread ID",
        },
    },
    CommandDef {
        command: BotCommand::Threads,
        patterns: &["/threads", "/thread"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "threads",
            description: "Open the Mini App thread viewer",
        },
    },
    CommandDef {
        command: BotCommand::Launcher,
        patterns: &["/launcher", "/menu"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "launcher",
            description: "Show the model launcher (General only)",
        },
    },
];

pub(crate) fn telegram_command_specs() -> Vec<TelegramCommandSpec> {
    let mut specs: Vec<TelegramCommandSpec> =
        COMMAND_DEFS.iter().map(|def| def.telegram_spec).collect();
    specs.push(TelegramCommandSpec {
        command: "model",
        description: "View or change model and thinking",
    });
    specs
}

/// Native bot command names that custom `.md` commands may not shadow in the
/// `/commands` picker (mirrors the TUI's `builtin_names` rule).
pub(crate) fn native_command_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = COMMAND_DEFS
        .iter()
        .map(|def| def.telegram_spec.command)
        .collect();
    names.extend(["model", "cancel"]);
    names
}

pub(crate) fn parse_command(text: &str) -> Option<BotCommand> {
    let trimmed = text.trim();

    if parse_restart_command(trimmed).is_some() {
        return Some(BotCommand::Restart);
    }

    COMMAND_DEFS
        .iter()
        .filter(|def| def.command != BotCommand::Restart)
        .find_map(|def| {
            def.patterns
                .iter()
                .any(|pattern| command_matches(trimmed, pattern))
                .then_some(def.command)
        })
}

/// How `/restart` treats active agent runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RestartMode {
    /// Refuse while any agent run is active (default).
    Gated,
    /// Interrupt active runs and restart now (`f`, `force`, `--force`).
    Force,
    /// Restart automatically once no agent run is active (`q`, `queue`).
    Queued,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RestartCommand {
    pub mode: RestartMode,
}

pub(crate) fn parse_restart_command(text: &str) -> Option<RestartCommand> {
    let mut parts = text.split_whitespace();
    let command = parts.next()?;
    let is_restart = command == "/restart"
        || command
            .strip_prefix("/restart@")
            .is_some_and(|username| !username.is_empty());
    if !is_restart {
        return None;
    }

    let mode = match (parts.next(), parts.next()) {
        (None, None) => RestartMode::Gated,
        (Some("f" | "force" | "--force"), None) => RestartMode::Force,
        (Some("q" | "queue"), None) => RestartMode::Queued,
        _ => return None,
    };
    Some(RestartCommand { mode })
}

pub(crate) fn blocks_topic_autocreate(command: BotCommand) -> bool {
    COMMAND_DEFS
        .iter()
        .find(|def| def.command == command)
        .is_some_and(|def| def.blocks_topic_autocreate)
}

pub(crate) fn is_topic_blocking_command(text: &str) -> bool {
    parse_command(text).is_some_and(blocks_topic_autocreate) || parse_model_command(text).is_some()
}

pub(crate) fn bypasses_queue(text: &str) -> bool {
    matches!(
        parse_command(text),
        Some(
            BotCommand::Status
                | BotCommand::WhereAmI
                | BotCommand::Tldr
                | BotCommand::ThreadId
                | BotCommand::Btw
                | BotCommand::Commands
                | BotCommand::Restart
        )
    )
}

fn command_matches(trimmed_text: &str, command: &str) -> bool {
    if trimmed_text == command {
        return true;
    }

    trimmed_text
        .strip_prefix(command)
        .is_some_and(|stripped| stripped.starts_with('@'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ModelSubcommand {
    Show,
    List,
    Set(String),
    Reset,
}

/// Parses a /model command. Returns None if the text is not a /model command.
pub(crate) fn parse_model_command(text: &str) -> Option<ModelSubcommand> {
    let trimmed = text.trim();
    let without_mention = if trimmed.starts_with("/model@") {
        let rest = trimmed.strip_prefix("/model").unwrap();
        let after_mention = rest.find(' ').map_or("", |i| &rest[i..]);
        format!("/model{after_mention}")
    } else if trimmed == "/model" || trimmed.starts_with("/model ") {
        trimmed.to_string()
    } else {
        return None;
    };

    let parts: Vec<&str> = without_mention.split_whitespace().collect();
    match parts.as_slice() {
        ["/model", "list"] => Some(ModelSubcommand::List),
        ["/model", "set", id, ..] => Some(ModelSubcommand::Set((*id).to_string())),
        ["/model", "reset"] => Some(ModelSubcommand::Reset),
        _ => Some(ModelSubcommand::Show),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        BotCommand, RestartMode, bypasses_queue, command_matches, is_topic_blocking_command,
        parse_command, parse_model_command, parse_restart_command, telegram_command_specs,
    };

    #[test]
    fn parse_basic_commands() {
        assert_eq!(parse_command("/new"), Some(BotCommand::New));
        assert_eq!(parse_command(" /new@zdx_bot "), Some(BotCommand::New));
        assert_eq!(parse_command("/restart"), Some(BotCommand::Restart));
        assert_eq!(parse_command("/restart@zdx_bot please"), None);
        assert_eq!(parse_command("/status"), Some(BotCommand::Status));
        assert_eq!(parse_command(" /status@zdx_bot "), Some(BotCommand::Status));
        assert_eq!(parse_command("/whereami"), Some(BotCommand::WhereAmI));
        assert_eq!(
            parse_command(" /whereami@zdx_bot "),
            Some(BotCommand::WhereAmI)
        );
        assert_eq!(parse_command("/handoff"), Some(BotCommand::Handoff));
        assert_eq!(
            parse_command(" /handoff@zdx_bot "),
            Some(BotCommand::Handoff)
        );
        assert_eq!(parse_command("/handoff please"), None);
    }

    #[test]
    fn parses_restart_modes_strictly() {
        let mode = |text: &str| parse_restart_command(text).map(|cmd| cmd.mode);
        assert_eq!(mode("/restart"), Some(RestartMode::Gated));
        assert_eq!(mode("/restart f"), Some(RestartMode::Force));
        assert_eq!(mode("/restart force"), Some(RestartMode::Force));
        assert_eq!(mode("/restart --force"), Some(RestartMode::Force));
        assert_eq!(mode(" /restart@zdx_bot f "), Some(RestartMode::Force));
        assert_eq!(mode("/restart q"), Some(RestartMode::Queued));
        assert_eq!(mode("/restart queue"), Some(RestartMode::Queued));
        assert_eq!(
            parse_command("/restart@zdx_bot --force"),
            Some(BotCommand::Restart)
        );
        assert_eq!(parse_command("/restart q"), Some(BotCommand::Restart));
        assert_eq!(mode("/restart now"), None);
        assert_eq!(mode("/restart f now"), None);
        assert_eq!(mode("/restart q f"), None);
    }

    #[test]
    fn parse_btw_command() {
        assert_eq!(parse_command("/btw"), Some(BotCommand::Btw));
        assert_eq!(parse_command("/btw@zdx_bot"), Some(BotCommand::Btw));
        // Like /handoff, the question is sent as a follow-up message, not inline.
        assert_eq!(parse_command("/btw what files did we touch"), None);
        // Must not auto-create a topic when sent from General.
        assert!(is_topic_blocking_command("/btw"));
        // Opens a side topic without touching the running thread, so it never
        // waits behind the queue (matches the TUI's `/btw` tab).
        assert!(bypasses_queue("/btw"));
    }

    #[test]
    fn parse_launcher_command_aliases() {
        assert_eq!(parse_command("/launcher"), Some(BotCommand::Launcher));
        assert_eq!(
            parse_command("/launcher@zdx_bot"),
            Some(BotCommand::Launcher)
        );
        assert_eq!(parse_command("/menu"), Some(BotCommand::Launcher));
        assert_eq!(parse_command("/menu@zdx_bot"), Some(BotCommand::Launcher));
        assert!(is_topic_blocking_command("/launcher"));
    }

    #[test]
    fn model_picker_scope_roundtrips() {
        use crate::handlers::message::ModelPickerScope;
        for scope in [
            ModelPickerScope::General,
            ModelPickerScope::Topic,
            ModelPickerScope::NewThread,
        ] {
            assert_eq!(ModelPickerScope::from_data(scope.as_str()), Some(scope));
        }
        assert_eq!(ModelPickerScope::from_data("bogus"), None);
    }

    #[test]
    fn model_pick_callback_data_within_telegram_limit() {
        use crate::handlers::message::ModelPickerScope;
        // Worst case: longest provider name + a two-digit index + newthread scope.
        let provider = "google_antigravity";
        let scope = ModelPickerScope::NewThread.as_str();
        let data = format!("model_pick:{provider}:99:{scope}");
        assert!(
            data.len() <= 64,
            "callback data too long: {data} ({})",
            data.len()
        );
        let data = format!("model_thinking:{provider}:99:{scope}:xhigh");
        assert!(
            data.len() <= 64,
            "callback data too long: {data} ({})",
            data.len()
        );
    }

    #[test]
    fn parse_worktree_command_aliases() {
        assert_eq!(
            parse_command("/worktree create"),
            Some(BotCommand::WorktreeCreate)
        );
        assert_eq!(
            parse_command("/worktree create@zdx_bot"),
            Some(BotCommand::WorktreeCreate)
        );
        assert_eq!(
            parse_command("/worktree@zdx_bot create"),
            Some(BotCommand::WorktreeCreate)
        );
        assert_eq!(
            parse_command("/worktree create@zdx_bot later"),
            Some(BotCommand::WorktreeCreate)
        );
        assert_eq!(parse_command("/wt"), Some(BotCommand::WorktreeCreate));
        assert_eq!(
            parse_command("/wt@zdx_bot"),
            Some(BotCommand::WorktreeCreate)
        );
    }

    #[test]
    fn rejects_non_commands() {
        assert_eq!(parse_command("hello"), None);
        assert_eq!(parse_command("/new please"), None);
        assert_eq!(parse_command("/exit"), None);
        assert_eq!(parse_command("/restart please"), None);
        assert_eq!(parse_command("/worktree please"), None);
    }

    #[test]
    fn blocking_topic_creation_uses_same_parser() {
        assert!(is_topic_blocking_command("/new"));
        assert!(is_topic_blocking_command("/restart@zdx_bot"));
        assert!(is_topic_blocking_command("/status"));
        assert!(is_topic_blocking_command("/whereami"));
        assert!(is_topic_blocking_command("/whereami@zdx_bot"));
        assert!(is_topic_blocking_command("/worktree"));
        assert!(is_topic_blocking_command("/handoff"));
        assert!(is_topic_blocking_command("/model"));
        assert!(is_topic_blocking_command("/model list"));
        assert!(!is_topic_blocking_command("let's chat"));
    }

    #[test]
    fn queue_bypass_is_limited_to_status() {
        assert!(bypasses_queue("/status"));
        assert!(bypasses_queue("/status@zdx_bot"));
        assert!(bypasses_queue("/whereami"));
        assert!(bypasses_queue("/whereami@zdx_bot"));
        assert!(!bypasses_queue("/new"));
        assert!(!bypasses_queue("/model"));
        assert!(!bypasses_queue("/handoff"));
        assert!(bypasses_queue("/tldr"));
        assert_eq!(parse_command("/tldr"), Some(BotCommand::Tldr));
        // Restart must be answered (blocked / queued / forced) while a turn
        // runs, never wait behind it.
        assert!(bypasses_queue("/restart"));
        assert!(bypasses_queue("/restart q"));
        assert!(bypasses_queue("/restart f"));
        // The picker only lists project commands; it never touches the thread.
        assert!(bypasses_queue("/commands"));
        assert!(bypasses_queue("/commands@zdx_bot"));
        assert!(!bypasses_queue("/prompt-builder"));
        assert_eq!(
            parse_command("/prompt-builder"),
            Some(BotCommand::PromptBuilder)
        );
        assert_eq!(
            parse_command("/prompt_builder@zdx_bot"),
            Some(BotCommand::PromptBuilder)
        );
    }

    #[test]
    fn parse_model_commands() {
        assert!(matches!(
            parse_model_command("/model"),
            Some(super::ModelSubcommand::Show)
        ));
        assert!(matches!(
            parse_model_command("/model@zdx_bot list"),
            Some(super::ModelSubcommand::List)
        ));
        assert!(matches!(
            parse_model_command("/model set anthropic:claude-sonnet-4-5"),
            Some(super::ModelSubcommand::Set(_))
        ));
        assert!(matches!(
            parse_model_command("/model reset"),
            Some(super::ModelSubcommand::Reset)
        ));
    }

    #[test]
    fn command_matcher_accepts_bot_mentions_only() {
        assert!(command_matches("/new", "/new"));
        assert!(command_matches("/new@zdx_bot", "/new"));
        assert!(!command_matches("/new anything", "/new"));
    }

    #[test]
    fn telegram_command_specs_are_unique_and_non_empty() {
        let specs = telegram_command_specs();
        assert!(!specs.is_empty());

        let mut names = HashSet::new();
        for spec in specs {
            assert!(!spec.command.trim().is_empty());
            assert!(!spec.description.trim().is_empty());
            assert!(names.insert(spec.command));
        }
    }
}
