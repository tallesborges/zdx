//! `zdx monitor`: the read-only Go dashboard (`apps/monitor`).

use std::path::Path;

use anyhow::Result;

use super::go_program::{self, GoProgram};

/// The Go monitor `zdx monitor` starts: `zdx-go monitor`.
pub const MONITOR: GoProgram = GoProgram {
    command: "monitor",
    why: "`zdx monitor` runs the Go monitor",
};

/// `zdx monitor [-- ARGS]`: replaces this process with the Go monitor, as
/// `zdx-go monitor --root ROOT ARGS` (see `go_program` for the environment and
/// the checks).
///
/// # Errors
/// Returns an error, before anything runs, when the monitor is missing, is
/// not an absolute path, or is this binary; or when the exec fails.
pub fn run(root: &Path, args: &[String]) -> Result<()> {
    let (mut command, monitor) = MONITOR.command(root)?;
    command.args(args);
    go_program::exec(command, &monitor)
}
