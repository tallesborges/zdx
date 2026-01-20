//! Command definitions for the command palette.
//!
//! This module defines the available commands for the TUI command palette.

/// Definition of a command.
#[derive(Debug, Clone)]
pub struct Command {
    /// Primary name (e.g., "clear") - without the leading slash.
    pub name: &'static str,
    /// Aliases (e.g., ["new"]) - without leading slashes.
    pub aliases: &'static [&'static str],
    /// Short description shown in palette.
    pub description: &'static str,
}

impl Command {
    /// Returns true if this command matches the given filter (case-insensitive).
    /// Matches against name and all aliases.
    pub fn matches(&self, filter: &str) -> bool {
        let filter_lower = filter.to_lowercase();
        self.name.to_lowercase().contains(&filter_lower)
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
    },
    Command {
        name: "copy-id",
        aliases: &["copyid"],
        description: "Copy current thread ID to clipboard",
    },
    Command {
        name: "debug",
        aliases: &["perf", "status"],
        description: "Toggle debug/performance status line",
    },
    Command {
        name: "handoff",
        aliases: &[],
        description: "Start new thread with context from current",
    },
    Command {
        name: "login",
        aliases: &[],
        description: "Authenticate with the active provider",
    },
    Command {
        name: "logout",
        aliases: &[],
        description: "Clear auth for the active provider",
    },
    Command {
        name: "rename",
        aliases: &[],
        description: "Rename the current thread",
    },
    Command {
        name: "model",
        aliases: &[],
        description: "Switch model",
    },
    Command {
        name: "models",
        aliases: &["models-config"],
        description: "Open models config in default editor",
    },
    Command {
        name: "new",
        aliases: &["clear"],
        description: "Start a new thread",
    },
    Command {
        name: "quit",
        aliases: &["q", "exit"],
        description: "Exit ZDX",
    },
    Command {
        name: "threads",
        aliases: &["history"],
        description: "Browse and switch threads",
    },
    Command {
        name: "thinking",
        aliases: &[],
        description: "Change thinking level",
    },
    Command {
        name: "timeline",
        aliases: &[],
        description: "Jump to a thread turn",
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
        assert_eq!(find_command("new").display_name(), "new (clear)");
        assert_eq!(find_command("quit").display_name(), "quit (q, exit)");
        assert_eq!(find_command("threads").display_name(), "threads (history)");
        assert_eq!(find_command("thinking").display_name(), "thinking");
        assert_eq!(find_command("timeline").display_name(), "timeline");
    }
}
