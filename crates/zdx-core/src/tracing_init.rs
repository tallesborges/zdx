//! Shared tracing initialization for all ZDX binaries.

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{EnvFilter, Layer, fmt};

use crate::config::paths;

/// Options for tracing initialization.
pub struct TracingOptions {
    /// Whether to also log to stderr (disable for TUI mode).
    pub stderr: bool,
}

impl Default for TracingOptions {
    fn default() -> Self {
        Self { stderr: true }
    }
}

/// Initialize tracing with daily rolling file appender + optional stderr.
///
/// Returns guards that must be held alive for the lifetime of the process.
/// Dropping them flushes pending logs.
///
/// File logs go to `~/.zdx/logs/zdx.YYYY-MM-DD.log` at the level set by
/// `ZDX_LOG` env var (default: `info`). Stderr (when enabled) shows `warn+`.
#[must_use]
pub fn init(options: &TracingOptions) -> Vec<WorkerGuard> {
    let log_dir = paths::zdx_home().join("logs");
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::daily(&log_dir, "zdx.log");
    let (file_writer, file_guard) = tracing_appender::non_blocking(file_appender);

    let mut guards = vec![file_guard];

    let file_filter = EnvFilter::try_from_env("ZDX_LOG").unwrap_or_else(|_| EnvFilter::new("info"));

    let file_layer = fmt::layer()
        .compact()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_filter(file_filter);

    if options.stderr {
        let (stderr_writer, stderr_guard) = tracing_appender::non_blocking(std::io::stderr());
        guards.push(stderr_guard);

        let stderr_layer = fmt::layer()
            .compact()
            .with_writer(stderr_writer)
            .with_ansi(true)
            .with_filter(tracing_subscriber::filter::LevelFilter::WARN);

        tracing_subscriber::registry()
            .with(file_layer)
            .with(stderr_layer)
            .init();
    } else {
        tracing_subscriber::registry().with(file_layer).init();
    }

    guards
}
