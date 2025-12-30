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

    /// Returns the display name with aliases, e.g., "clear (new)".
    pub fn display_name(&self) -> String {
        if self.aliases.is_empty() {
            format!("/{}", self.name)
        } else {
            format!("/{} ({})", self.name, self.aliases.join(", "))
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
        name: "handoff",
        aliases: &[],
        description: "Start new session with context from current",
    },
    Command {
        name: "login",
        aliases: &[],
        description: "Login with Anthropic OAuth",
    },
    Command {
        name: "logout",
        aliases: &[],
        description: "Logout from Anthropic OAuth",
    },
    Command {
        name: "model",
        aliases: &[],
        description: "Switch model",
    },
    Command {
        name: "new",
        aliases: &["clear"],
        description: "Start a new conversation",
    },
    Command {
        name: "quit",
        aliases: &["q", "exit"],
        description: "Exit ZDX",
    },
    Command {
        name: "thinking",
        aliases: &[],
        description: "Change thinking level",
    },
];

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
        assert_eq!(find_command("config").display_name(), "/config (settings)");
        assert_eq!(find_command("handoff").display_name(), "/handoff");
        assert_eq!(find_command("login").display_name(), "/login");
        assert_eq!(find_command("logout").display_name(), "/logout");
        assert_eq!(find_command("model").display_name(), "/model");
        assert_eq!(find_command("new").display_name(), "/new (clear)");
        assert_eq!(find_command("quit").display_name(), "/quit (q, exit)");
        assert_eq!(find_command("thinking").display_name(), "/thinking");
    }
}
