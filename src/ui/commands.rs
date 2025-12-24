//! Slash command definitions.
//!
//! This module defines the available slash commands for the TUI command palette.

/// Definition of a slash command.
#[derive(Debug, Clone)]
pub struct SlashCommand {
    /// Primary name (e.g., "clear") - without the leading slash.
    pub name: &'static str,
    /// Aliases (e.g., ["new"]) - without leading slashes.
    pub aliases: &'static [&'static str],
    /// Short description shown in palette.
    pub description: &'static str,
}

impl SlashCommand {
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

/// Available slash commands.
pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand {
        name: "config",
        aliases: &["settings"],
        description: "Open config file in default editor",
    },
    SlashCommand {
        name: "login",
        aliases: &[],
        description: "Login with Anthropic OAuth",
    },
    SlashCommand {
        name: "logout",
        aliases: &[],
        description: "Logout from Anthropic OAuth",
    },
    SlashCommand {
        name: "model",
        aliases: &[],
        description: "Switch model",
    },
    SlashCommand {
        name: "new",
        aliases: &["clear"],
        description: "Start a new conversation",
    },
    SlashCommand {
        name: "quit",
        aliases: &["q", "exit"],
        description: "Exit ZDX",
    },
    SlashCommand {
        name: "thinking",
        aliases: &[],
        description: "Change thinking level",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slash_command_matches_name() {
        let cmd = &SLASH_COMMANDS[4]; // new
        assert!(cmd.matches("new"));
        assert!(cmd.matches("ne"));
        assert!(cmd.matches("NEW")); // case-insensitive
        assert!(!cmd.matches("quit"));
    }

    #[test]
    fn test_slash_command_matches_alias() {
        let cmd = &SLASH_COMMANDS[4]; // new (alias: clear)
        assert!(cmd.matches("clear"));
        assert!(cmd.matches("cle"));
        assert!(cmd.matches("CLEAR")); // case-insensitive
    }

    #[test]
    fn test_slash_command_display_name() {
        let config_cmd = &SLASH_COMMANDS[0];
        assert_eq!(config_cmd.display_name(), "/config (settings)");

        let login_cmd = &SLASH_COMMANDS[1];
        assert_eq!(login_cmd.display_name(), "/login");

        let logout_cmd = &SLASH_COMMANDS[2];
        assert_eq!(logout_cmd.display_name(), "/logout");

        let model_cmd = &SLASH_COMMANDS[3];
        assert_eq!(model_cmd.display_name(), "/model (m)");

        let new_cmd = &SLASH_COMMANDS[4];
        assert_eq!(new_cmd.display_name(), "/new (clear)");

        let quit = &SLASH_COMMANDS[5];
        assert_eq!(quit.display_name(), "/quit (q, exit)");
    }
}
