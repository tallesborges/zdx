use std::sync::OnceLock;

use tempfile::TempDir;

/// Returns a single process-wide isolated `ZDX_HOME` for tests.
///
/// Sets `ZDX_HOME` to one temp dir exactly once for the whole test binary and
/// leaks it for the process lifetime. Every test that depends on
/// `ZDX_HOME`-derived paths must funnel through this helper so the value stays
/// stable and never touches the real `~/.zdx`. Sharing one home is what keeps
/// tests from order-dependent divergence: independent setters that each pointed
/// `ZDX_HOME` at their own temp dir would flip the live value depending on
/// scheduling.
pub(crate) fn temp_zdx_home() -> &'static TempDir {
    static ZDX_HOME: OnceLock<TempDir> = OnceLock::new();
    ZDX_HOME.get_or_init(|| {
        let temp = TempDir::new().unwrap();
        // SAFETY: tests are single-process; funneling every `ZDX_HOME`-dependent
        // test through this one setter keeps the value stable for the binary.
        unsafe {
            std::env::set_var("ZDX_HOME", temp.path());
        }
        temp
    })
}
