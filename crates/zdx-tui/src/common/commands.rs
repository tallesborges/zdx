//! Command definitions for the command palette.
//!
//! This module defines the available commands for the TUI command palette.

/// Definition of a command.
#[derive(Debug, Clone)]
pub struct Command {
    /// Primary name (e.g., "clear") - without the leading slash.
    pub name: &'static str,
    /// Aliases (e.g., `["new"]`) - without leading slashes.
    pub aliases: &'static [&'static str],
    /// Short description shown in palette.
    pub description: &'static str,
    /// Category for grouping in the palette (e.g., "thread", "config", "auth").
    pub category: &'static str,
    /// Keyboard shortcut hint (e.g., "Ctrl s").
    pub shortcut: Option<&'static str>,
}

impl Command {
    /// Returns true if this command matches the given filter (case-insensitive).
    /// Matches against name, aliases, and category.
    pub fn matches(&self, filter: &str) -> bool {
        let filter_lower = filter.to_lowercase();
        self.name.to_lowercase().contains(&filter_lower)
            || self.category.to_lowercase().contains(&filter_lower)
            || self
                .aliases
                .iter()
                .any(|a| a.to_lowercase().contains(&filter_lower))
    }

    /// Returns the display name with aliases, e.g., "new (clear)".
    pub fn display_name(&self) -> String {
        if self.aliases.is_empty() {
            self.name.to_string()
        } else {
            format!("{} ({})", self.name, self.aliases.join(", "))
        }
    }
}

/// Available commands.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "config",
        aliases: &["settings"],
        description: "Open config file in default editor",
        category: "config",
        shortcut: None,
    },
    Command {
        name: "copy-id",
        aliases: &["copyid"],
        description: "Copy current thread ID to clipboard",
        category: "thread",
        shortcut: None,
    },
    Command {
        name: "debug",
        aliases: &["perf", "status"],
        description: "Toggle debug/performance status line",
        category: "debug",
        shortcut: None,
    },
    Command {
        name: "handoff",
        aliases: &[],
        description: "Start new thread with context from current",
        category: "thread",
        shortcut: None,
    },
    Command {
        name: "login",
        aliases: &[],
        description: "Authenticate with the active provider",
        category: "auth",
        shortcut: None,
    },
    Command {
        name: "logout",
        aliases: &[],
        description: "Clear auth for the active provider",
        category: "auth",
        shortcut: None,
    },
    Command {
        name: "rename",
        aliases: &[],
        description: "Rename the current thread",
        category: "thread",
        shortcut: None,
    },
    Command {
        name: "model",
        aliases: &[],
        description: "Switch model",
        category: "model",
        shortcut: None,
    },
    Command {
        name: "models",
        aliases: &["models-config"],
        description: "Open models config in default editor",
        category: "config",
        shortcut: None,
    },
    Command {
        name: "skills",
        aliases: &["skill"],
        description: "Browse and install skills",
        category: "skills",
        shortcut: None,
    },
    Command {
        name: "new",
        aliases: &["clear"],
        description: "Start a new thread",
        category: "thread",
        shortcut: None,
    },
    Command {
        name: "quit",
        aliases: &["q", "exit"],
        description: "Exit ZDX",
        category: "app",
        shortcut: None,
    },
    Command {
        name: "threads",
        aliases: &["history"],
        description: "Browse and switch threads",
        category: "thread",
        shortcut: None,
    },
    Command {
        name: "worktree",
        aliases: &["wt"],
        description: "Create/switch to a per-thread git worktree",
        category: "git",
        shortcut: None,
    },
    Command {
        name: "root-new",
        aliases: &["root"],
        description: "Start a new thread from the original project root",
        category: "thread",
        shortcut: None,
    },
    Command {
        name: "thinking",
        aliases: &[],
        description: "Change thinking level",
        category: "model",
        shortcut: Some("Ctrl+T"),
    },
    Command {
        name: "timeline",
        aliases: &[],
        description: "Jump to a thread turn",
        category: "thread",
        shortcut: None,
    },
];

pub fn command_available(command: &Command, model_id: &str) -> bool {
    if command.name == "thinking" {
        return zdx_core::models::model_supports_reasoning(model_id);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find_command(name: &str) -> &'static Command {
        COMMANDS.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn test_command_matches_name() {
        let cmd = find_command("new");
        assert!(cmd.matches("new"));
        assert!(cmd.matches("ne"));
        assert!(cmd.matches("NEW")); // case-insensitive
        assert!(!cmd.matches("quit"));
    }

    #[test]
    fn test_command_matches_alias() {
        let cmd = find_command("new");
        assert!(cmd.matches("clear"));
        assert!(cmd.matches("cle"));
        assert!(cmd.matches("CLEAR")); // case-insensitive
    }

    #[test]
    fn test_command_matches_category() {
        let cmd = find_command("new");
        assert!(cmd.matches("thread")); // category
        assert!(cmd.matches("THREAD")); // case-insensitive

        let cmd = find_command("login");
        assert!(cmd.matches("auth")); // category
    }

    #[test]
    fn test_command_display_name() {
        assert_eq!(find_command("config").display_name(), "config (settings)");
        assert_eq!(find_command("copy-id").display_name(), "copy-id (copyid)");
        assert_eq!(find_command("debug").display_name(), "debug (perf, status)");
        assert_eq!(find_command("handoff").display_name(), "handoff");
        assert_eq!(find_command("login").display_name(), "login");
        assert_eq!(find_command("logout").display_name(), "logout");
        assert_eq!(find_command("rename").display_name(), "rename");
        assert_eq!(find_command("model").display_name(), "model");
        assert_eq!(
            find_command("models").display_name(),
            "models (models-config)"
        );
        assert_eq!(find_command("skills").display_name(), "skills (skill)");
        assert_eq!(find_command("new").display_name(), "new (clear)");
        assert_eq!(find_command("quit").display_name(), "quit (q, exit)");
        assert_eq!(find_command("threads").display_name(), "threads (history)");
        assert_eq!(find_command("worktree").display_name(), "worktree (wt)");
        assert_eq!(find_command("root-new").display_name(), "root-new (root)");
        assert_eq!(find_command("thinking").display_name(), "thinking");
        assert_eq!(find_command("timeline").display_name(), "timeline");
    }
}
