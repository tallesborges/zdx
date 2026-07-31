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

/// Dependencies whose `debug`/`trace` output drowns out ZDX's own events
/// (HTTP/2 frames, connection pooling, TLS, gitignore walking). Pinned to
/// `warn` so `ZDX_LOG=debug` shows ZDX's debug events, not h2 frame dumps.
/// An explicit directive for the same target in `ZDX_LOG` replaces the floor,
/// so `ZDX_LOG=debug,h2=debug` still works when the transport is suspect.
const NOISY_DEPENDENCY_TARGETS: &[&str] = &[
    "h2",
    "hyper",
    "hyper_util",
    "reqwest",
    "rustls",
    "tungstenite",
    "tokio_tungstenite",
    "ignore",
    "globset",
];

/// Build the file-layer filter: a `warn` floor for noisy dependencies, then
/// `ZDX_LOG` (default `info`) appended so an explicit directive for the same
/// target replaces the floor.
fn file_filter() -> EnvFilter {
    let requested = std::env::var("ZDX_LOG").unwrap_or_else(|_| "info".to_string());
    build_file_filter(&requested)
}

fn build_file_filter(requested: &str) -> EnvFilter {
    let floor = NOISY_DEPENDENCY_TARGETS
        .iter()
        .map(|target| format!("{target}=warn"))
        .collect::<Vec<_>>()
        .join(",");
    EnvFilter::new(format!("{floor},{requested}"))
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

    let file_layer = fmt::layer()
        .compact()
        .with_writer(file_writer)
        .with_ansi(false)
        .with_filter(file_filter());

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_requests_keep_noisy_dependencies_at_warn() {
        let rendered = build_file_filter("debug").to_string();
        assert!(rendered.contains("h2=warn"), "{rendered}");
        assert!(rendered.contains("hyper=warn"), "{rendered}");
        assert!(rendered.contains("debug"), "{rendered}");
    }

    #[test]
    fn explicit_dependency_directive_is_not_overridden() {
        let rendered = build_file_filter("debug,h2=debug").to_string();
        assert!(!rendered.contains("h2=warn"), "{rendered}");
        assert!(rendered.contains("h2=debug"), "{rendered}");
        // Unmentioned noisy deps are still pinned.
        assert!(rendered.contains("rustls=warn"), "{rendered}");
    }

    #[test]
    fn a_directive_for_one_dependency_does_not_unpin_a_similarly_named_one() {
        // `hyper_util` and `hyper` are distinct targets: asking for one must not
        // silently lift the floor on the other.
        let rendered = build_file_filter("debug,hyper_util=debug").to_string();
        assert!(rendered.contains("hyper=warn"), "{rendered}");
        assert!(rendered.contains("hyper_util=debug"), "{rendered}");
    }
}
