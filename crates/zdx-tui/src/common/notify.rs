//! Turn-completion notifications for the TUI: an OSC 9 desktop notification
//! and optional cmux sidebar integration (status pill + progress bar). All
//! best-effort.

use std::io::Write;
use std::sync::OnceLock;

/// Per-instance status-pill key, so multiple zdx sharing one cmux workspace
/// each get their own pill. Derived from the pane (`CMUX_SURFACE_ID`), falling
/// back to the process id.
fn cmux_status_key() -> &'static str {
    static KEY: OnceLock<String> = OnceLock::new();
    KEY.get_or_init(|| {
        derive_status_key(
            std::env::var("CMUX_SURFACE_ID").ok().as_deref(),
            std::process::id(),
        )
    })
}

fn derive_status_key(surface_id: Option<&str>, pid: u32) -> String {
    let id = surface_id
        .and_then(|s| s.split('-').next())
        .map(str::to_ascii_lowercase)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| pid.to_string());
    format!("zdx-{id}")
}

/// Writes an OSC sequence (`ESC ] <code> ; <payload> BEL`) to stdout, which the
/// terminal interprets rather than renders. Unsupported terminals ignore it.
fn write_osc(code: u8, payload: &str) {
    let mut stdout = std::io::stdout();
    let _ = write!(stdout, "\x1b]{code};{payload}\x07");
    let _ = stdout.flush();
}

/// Emits an OSC 9 desktop notification.
pub fn emit_osc9(message: &str) {
    write_osc(9, message);
}

/// Sets the terminal window/tab title via OSC 0, which sets both the icon name
/// and window title so most terminals show it in the tab.
pub fn set_term_title(title: &str) {
    write_osc(0, title);
}

/// Sets this instance's cmux sidebar status pill (e.g. `◐ · fix auth bug`).
/// Fire-and-forget, so a missing `cmux` binary is a silent no-op.
pub fn cmux_set_status(value: impl Into<String>) {
    let value = value.into();
    cmux_spawn(vec!["set-status".into(), cmux_status_key().into(), value]);
}

/// Clears this instance's cmux sidebar status pill.
pub fn cmux_clear_status() {
    cmux_spawn(vec!["clear-status".into(), cmux_status_key().into()]);
}

/// Clears this instance's cmux status pill on shutdown using a detached
/// `std::process` spawn, so cleanup still runs as the tokio runtime tears down
/// (a fire-and-forget `tokio::spawn` would be dropped unpolled at exit). Returns
/// once the child is launched without waiting, so a missing `cmux` binary or an
/// unresponsive socket is a silent, non-blocking no-op.
pub fn cmux_clear_status_on_exit() {
    let _ = std::process::Command::new("cmux")
        .args(["clear-status", cmux_status_key()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Sets the cmux sidebar progress bar (`value` clamped to `0.0..=1.0`).
pub fn cmux_set_progress(value: f64, label: String) {
    let value = value.clamp(0.0, 1.0);
    cmux_spawn(vec![
        "set-progress".into(),
        format!("{value:.2}"),
        "--label".into(),
        label,
    ]);
}

/// Clears the cmux sidebar progress bar.
pub fn cmux_clear_progress() {
    cmux_spawn(vec!["clear-progress".into()]);
}

/// Runs a `cmux` subcommand fire-and-forget on the async runtime, ignoring the
/// outcome so a missing `cmux` binary is a silent no-op.
fn cmux_spawn(args: Vec<String>) {
    tokio::spawn(async move {
        let _ = tokio::process::Command::new("cmux")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;
    });
}

#[cfg(test)]
mod tests {
    use super::derive_status_key;

    #[test]
    fn status_key_uses_short_surface_id() {
        assert_eq!(
            derive_status_key(Some("8E1DB164-9B6A-43EB-8523-72E3426FCABE"), 42),
            "zdx-8e1db164"
        );
    }

    #[test]
    fn status_key_falls_back_to_pid() {
        assert_eq!(derive_status_key(None, 42), "zdx-42");
        assert_eq!(derive_status_key(Some(""), 42), "zdx-42");
    }
}
