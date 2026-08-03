//! Turn-completion notifications for the TUI: an OSC 9 desktop notification
//! and terminal title updates. All best-effort.

use std::io::Write;

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
