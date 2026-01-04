use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static INTERRUPT_NOTIFY: OnceLock<tokio::sync::Notify> = OnceLock::new();

#[derive(Debug)]
pub struct InterruptedError;

impl std::fmt::Display for InterruptedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Interrupted")
    }
}

impl std::error::Error for InterruptedError {}

/// Initializes the Ctrl+C handler.
///
/// The handler sets an interrupt flag only; it does not print anything.
/// The renderer is responsible for printing the interruption message.
/// This keeps stdout/stderr ownership in the renderer (per SPEC §10).
pub fn init() {
    ctrlc::set_handler(move || {
        trigger_ctrl_c();
    })
    .expect("Error setting Ctrl+C handler");
}

fn notify_waiters() {
    INTERRUPT_NOTIFY
        .get_or_init(tokio::sync::Notify::new)
        .notify_waiters();
}

/// Triggers an interrupt via Ctrl+C, force-exiting on a second Ctrl+C.
pub fn trigger_ctrl_c() {
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        // Second interrupt - force exit.
        // Restore terminal first since process::exit() bypasses Drop handlers.
        let _ = crate::modes::tui::terminal::restore_terminal();
        std::process::exit(130);
    }
    notify_waiters();
}

/// Checks if an interrupt has been requested.
pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Waits until an interrupt is triggered.
pub async fn wait_for_interrupt() {
    loop {
        if is_interrupted() {
            return;
        }
        INTERRUPT_NOTIFY
            .get_or_init(tokio::sync::Notify::new)
            .notified()
            .await;
    }
}

/// Resets the interrupt flag.
pub fn reset() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}
