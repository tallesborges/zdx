#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BotCommand {
    New,
    Rebuild,
    WorktreeCreate,
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
        command: BotCommand::Rebuild,
        patterns: &["/rebuild"],
        blocks_topic_autocreate: true,
        telegram_spec: TelegramCommandSpec {
            command: "rebuild",
            description: "Rebuild and restart the bot",
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
];

pub(crate) fn telegram_command_specs() -> Vec<TelegramCommandSpec> {
    let mut specs: Vec<TelegramCommandSpec> =
        COMMAND_DEFS.iter().map(|def| def.telegram_spec).collect();
    specs.push(TelegramCommandSpec {
        command: "model",
        description: "View or change the AI model",
    });
    specs
}

pub(crate) fn parse_command(text: &str) -> Option<BotCommand> {
    let trimmed = text.trim();

    COMMAND_DEFS.iter().find_map(|def| {
        def.patterns
            .iter()
            .any(|pattern| command_matches(trimmed, pattern))
            .then_some(def.command)
    })
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
        BotCommand, command_matches, is_topic_blocking_command, parse_command, parse_model_command,
        telegram_command_specs,
    };

    #[test]
    fn parse_new_and_rebuild_commands() {
        assert_eq!(parse_command("/new"), Some(BotCommand::New));
        assert_eq!(parse_command(" /new@zdx_bot "), Some(BotCommand::New));
        assert_eq!(parse_command("/rebuild"), Some(BotCommand::Rebuild));
        assert_eq!(
            parse_command("/rebuild@zdx_bot please"),
            Some(BotCommand::Rebuild)
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
        assert_eq!(parse_command("/rebuild please"), None);
        assert_eq!(parse_command("/worktree please"), None);
    }

    #[test]
    fn blocking_topic_creation_uses_same_parser() {
        assert!(is_topic_blocking_command("/new"));
        assert!(is_topic_blocking_command("/rebuild@zdx_bot"));
        assert!(is_topic_blocking_command("/worktree"));
        assert!(is_topic_blocking_command("/model"));
        assert!(is_topic_blocking_command("/model list"));
        assert!(!is_topic_blocking_command("let's chat"));
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
