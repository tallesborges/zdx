//! `zdx git status|scope|diff`: the Git views as JSON (`zdx-go git`,
//! `apps/shared/gitview/cli`), the same bodies the Mini App Git routes
//! answer. The views live in the shared Go package `gitview`; this command
//! only starts it in its own place (see `go_program`). For `--thread`, the
//! Go side asks this binary (`ZDX_BIN`) for the `thread_root` view.

use std::ffi::OsString;
use std::path::Path;

use anyhow::Result;

use super::go_program::{self, GoProgram};

/// The Git views `zdx git` starts: `zdx-go git`.
pub const GIT: GoProgram = GoProgram {
    command: "git",
    why: "`zdx git` runs the Go Git views",
};

/// Replaces this process with `zdx-go git --root ROOT ARGS`.
///
/// # Errors
/// Returns an error, before anything runs, when `zdx-go` is missing, is not
/// an absolute path, or is this binary; or when the exec fails.
pub fn run(root: &Path, args: Vec<OsString>) -> Result<()> {
    let (mut command, program) = GIT.command(root)?;
    command.args(args);
    go_program::exec(command, &program)
}
