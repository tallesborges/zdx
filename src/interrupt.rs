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
pub fn init() {
    ctrlc::set_handler(move || {
        if INTERRUPTED.load(Ordering::SeqCst) {
            // Second Ctrl+C - force exit
            std::process::exit(130);
        }
        INTERRUPTED.store(true, Ordering::SeqCst);
        eprintln!("\n^C Interrupted.");
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
