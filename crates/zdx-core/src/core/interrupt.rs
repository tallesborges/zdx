use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::Notify;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);
static TERMINATE: AtomicBool = AtomicBool::new(false);
static INTERRUPT_NOTIFY: OnceLock<Notify> = OnceLock::new();
static RESTORE_HOOK: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

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
///
/// Also registers SIGTERM and SIGHUP handlers for graceful shutdown.
/// Unlike Ctrl+C (which may cancel a running agent), these signals
/// always trigger an unconditional quit.
///
/// # Panics
/// Panics if registering the Ctrl+C handler fails.
pub fn init() {
    ctrlc::set_handler(move || {
        trigger_ctrl_c();
    })
    .expect("Error setting Ctrl+C handler");

    // Register SIGTERM and SIGHUP to set the terminate flag.
    // These are catchable signals (unlike SIGKILL) that should cause a clean exit.
    #[cfg(unix)]
    {
        use signal_hook::consts::{SIGHUP, SIGTERM};

        // SAFETY: These closures only set an AtomicBool, which is async-signal-safe.
        unsafe {
            signal_hook::low_level::register(SIGTERM, || {
                TERMINATE.store(true, Ordering::SeqCst);
            })
            .expect("Error registering SIGTERM handler");
            signal_hook::low_level::register(SIGHUP, || {
                TERMINATE.store(true, Ordering::SeqCst);
            })
            .expect("Error registering SIGHUP handler");
        }
    }
}

fn notify_waiters() {
    INTERRUPT_NOTIFY.get_or_init(Notify::new).notify_waiters();
}

/// Triggers an interrupt via Ctrl+C, force-exiting on a second Ctrl+C.
pub fn trigger_ctrl_c() {
    if INTERRUPTED.swap(true, Ordering::SeqCst) {
        // Second interrupt - force exit.
        // Restore terminal first since process::exit() bypasses Drop handlers.
        if let Some(hook) = RESTORE_HOOK.get() {
            hook();
        }
        std::process::exit(130);
    }
    notify_waiters();
}

/// Checks if an interrupt has been requested.
pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Checks if a terminate signal (SIGTERM/SIGHUP) was received.
///
/// Unlike `is_interrupted()`, this always means "quit now" regardless of agent state.
pub fn should_terminate() -> bool {
    TERMINATE.load(Ordering::SeqCst)
}

/// Waits until an interrupt is triggered.
pub async fn wait_for_interrupt() {
    loop {
        if is_interrupted() {
            return;
        }
        INTERRUPT_NOTIFY.get_or_init(Notify::new).notified().await;
    }
}

/// Resets the interrupt flag.
pub fn reset() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}

/// Registers a restore hook called on the second Ctrl+C before exit.
///
/// Typically used by the TUI to restore terminal state.
pub fn set_restore_hook<F>(hook: F)
where
    F: Fn() + Send + Sync + 'static,
{
    let _ = RESTORE_HOOK.set(Box::new(hook));
}
