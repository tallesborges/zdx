//! `zdx quota`: the Go quota reader (`zdx-go quota`, `apps/shared/quota/cli`).
//!
//! The quota implementation lives in the shared Go module, so the CLI, the
//! Mini App, and the monitor read quotas the same way. This command only
//! starts it in its own place (see `go_program`). The reader never runs
//! `zdx`, so there is no recursion.

use std::path::Path;

use anyhow::Result;

use super::go_program::{self, GoProgram};

/// The Go quota reader `zdx quota` starts: `zdx-go quota`.
pub const QUOTA: GoProgram = GoProgram {
    command: "quota",
    why: "`zdx quota` runs the Go quota reader",
};

/// `zdx quota [--json]`: replaces this process with the Go quota reader, as
/// `zdx-go quota --root ROOT --zdx-version VERSION [--json]`. The version is
/// this binary's, sent as the `zdx/<version>` user agent where the
/// providers expect it.
///
/// # Errors
/// Returns an error, before anything runs, when the reader is missing, is
/// not an absolute path, or is this binary; or when the exec fails.
pub fn run(root: &Path, json: bool) -> Result<()> {
    let (mut command, program) = QUOTA.command(root)?;
    command.args(["--zdx-version", env!("CARGO_PKG_VERSION")]);
    if json {
        command.arg("--json");
    }
    go_program::exec(command, &program)
}
