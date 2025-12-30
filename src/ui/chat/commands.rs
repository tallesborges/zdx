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

    #[test]
    fn test_command_matches_name() {
        let cmd = &COMMANDS[4]; // new
        assert!(cmd.matches("new"));
        assert!(cmd.matches("ne"));
        assert!(cmd.matches("NEW")); // case-insensitive
        assert!(!cmd.matches("quit"));
    }

    #[test]
    fn test_command_matches_alias() {
        let cmd = &COMMANDS[4]; // new (alias: clear)
        assert!(cmd.matches("clear"));
        assert!(cmd.matches("cle"));
        assert!(cmd.matches("CLEAR")); // case-insensitive
    }

    #[test]
    fn test_command_display_name() {
        let config_cmd = &COMMANDS[0];
        assert_eq!(config_cmd.display_name(), "/config (settings)");

        let login_cmd = &COMMANDS[1];
        assert_eq!(login_cmd.display_name(), "/login");

        let logout_cmd = &COMMANDS[2];
        assert_eq!(logout_cmd.display_name(), "/logout");

        let model_cmd = &COMMANDS[3];
        assert_eq!(model_cmd.display_name(), "/model");

        let new_cmd = &COMMANDS[4];
        assert_eq!(new_cmd.display_name(), "/new (clear)");

        let quit = &COMMANDS[5];
        assert_eq!(quit.display_name(), "/quit (q, exit)");
    }
}
