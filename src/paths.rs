//! Path resolution for ZDX configuration and data directories.
//!
//! ZDX_HOME resolution order:
//! 1. ZDX_HOME environment variable (if set)
//! 2. ~/.config/zdx (default)

use std::path::PathBuf;

/// Returns the ZDX home directory.
///
/// Checks ZDX_HOME env var first, falls back to ~/.config/zdx
pub fn zdx_home() -> PathBuf {
    if let Ok(home) = std::env::var("ZDX_HOME") {
        return PathBuf::from(home);
    }

    dirs::home_dir()
        .map(|h| h.join(".config").join("zdx"))
        .expect("Could not determine home directory")
}

/// Returns the path to the config.toml file.
pub fn config_path() -> PathBuf {
    zdx_home().join("config.toml")
}

/// Returns the path to the sessions directory.
#[allow(dead_code)]
pub fn sessions_dir() -> PathBuf {
    zdx_home().join("sessions")
}
