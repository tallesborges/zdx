use std::sync::atomic::{AtomicBool, Ordering};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

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
        if INTERRUPTED.load(Ordering::SeqCst) {
            // Second Ctrl+C - force exit
            std::process::exit(130);
        }
        INTERRUPTED.store(true, Ordering::SeqCst);
        // Note: Renderer handles printing the interruption message
    })
    .expect("Error setting Ctrl+C handler");
}

/// Checks if an interrupt has been requested.
pub fn is_interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Resets the interrupt flag.
pub fn reset() {
    INTERRUPTED.store(false, Ordering::SeqCst);
}
