//! Process liveness and identity primitives shared by the PID file, agent
//! activity, and background process trackers.
//!
//! `kill(pid, 0)` alone is not a liveness check: it succeeds for zombies, whose
//! PIDs linger in the process table until the parent reaps them. A supervisor
//! that never reaps (or is itself stopped) therefore looks like it still owns a
//! live service, which wedges start/stop flows. Every check here treats a
//! zombie as dead.

use std::io;

/// Whether a PID refers to a live process.
pub enum Liveness {
    /// Running (or stopped), and not a zombie.
    Alive,
    /// Gone, or a zombie awaiting reap by its parent.
    Dead,
    /// Exists but its state is not inspectable (e.g. owned by another user).
    Unverifiable,
}

/// Returns true only when `pid` is a live, non-zombie process.
///
/// Processes whose state cannot be inspected are reported as not alive.
#[must_use]
pub fn is_alive(pid: u32) -> bool {
    matches!(liveness(pid), Liveness::Alive)
}

/// Classifies `pid` as alive, dead (including unreaped zombies), or opaque.
#[cfg(unix)]
#[must_use]
pub fn liveness(pid: u32) -> Liveness {
    if unsafe { libc::kill(pid as libc::pid_t, 0) } != 0 {
        return match io::Error::last_os_error().raw_os_error() {
            Some(libc::EPERM) => Liveness::Unverifiable,
            // ESRCH (no such process) or anything else → treat as dead.
            _ => Liveness::Dead,
        };
    }
    // Signalable, so the PID exists — but a zombie is signalable too.
    proc_state(pid)
}

#[cfg(not(unix))]
#[must_use]
pub fn liveness(_pid: u32) -> Liveness {
    Liveness::Unverifiable
}

/// Distinguishes a real process from an unreaped zombie.
///
/// A zombie has no task info left, so `proc_pidinfo` rejects it with `ESRCH`
/// even though `kill(pid, 0)` still succeeds. `EPERM` means the process is
/// live but owned by another user.
#[cfg(target_os = "macos")]
fn proc_state(pid: u32) -> Liveness {
    match bsdinfo(pid) {
        Ok(_) => Liveness::Alive,
        Err(Some(libc::ESRCH)) => Liveness::Dead,
        Err(_) => Liveness::Unverifiable,
    }
}

#[cfg(target_os = "linux")]
fn proc_state(pid: u32) -> Liveness {
    // Field 3 of /proc/<pid>/stat is the state char. The command (field 2) may
    // contain spaces and parens, so split after the final ')'.
    let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Liveness::Dead,
        Err(_) => return Liveness::Unverifiable,
    };
    let Some((_, rest)) = stat.rsplit_once(')') else {
        return Liveness::Unverifiable;
    };
    match rest.split_whitespace().next() {
        Some("Z") => Liveness::Dead,
        Some(_) => Liveness::Alive,
        None => Liveness::Unverifiable,
    }
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
fn proc_state(_pid: u32) -> Liveness {
    Liveness::Alive
}

/// Process-group id of `pid`, or `None` when unreadable.
#[cfg(unix)]
#[must_use]
pub fn current_pgid(pid: u32) -> Option<i32> {
    let r = unsafe { libc::getpgid(pid as libc::pid_t) };
    if r < 0 { None } else { Some(r) }
}

#[cfg(not(unix))]
#[must_use]
pub fn current_pgid(_pid: u32) -> Option<i32> {
    None
}

/// OS start-time identity of `pid`, used to defeat PID reuse.
///
/// Microseconds since the Unix epoch on macOS; platform starttime on Linux.
#[cfg(target_os = "macos")]
#[must_use]
pub fn current_birth(pid: u32) -> Option<u64> {
    let info = bsdinfo(pid).ok()?;
    Some(info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec)
}

#[cfg(target_os = "linux")]
#[must_use]
pub fn current_birth(pid: u32) -> Option<u64> {
    // Field 22 of /proc/<pid>/stat is starttime (clock ticks since boot).
    // The command (field 2) may contain spaces/parens, so split after ')'.
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let rest = stat.rsplit_once(')')?.1;
    rest.split_whitespace().nth(19)?.parse::<u64>().ok()
}

#[cfg(all(unix, not(target_os = "macos"), not(target_os = "linux")))]
#[must_use]
pub fn current_birth(_pid: u32) -> Option<u64> {
    None
}

#[cfg(not(unix))]
#[must_use]
pub fn current_birth(_pid: u32) -> Option<u64> {
    None
}

/// Reads BSD task info for `pid`, returning the failing `errno` on error.
#[cfg(target_os = "macos")]
fn bsdinfo(pid: u32) -> Result<libc::proc_bsdinfo, Option<i32>> {
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let n = unsafe {
        libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            0,
            std::ptr::from_mut(&mut info).cast::<libc::c_void>(),
            size,
        )
    };
    if n == size {
        Ok(info)
    } else {
        Err(io::Error::last_os_error().raw_os_error())
    }
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;
    use std::thread::sleep;
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn unreaped_child_is_not_alive() {
        let mut child = Command::new("true").spawn().expect("spawn child");
        let pid = child.id();

        // Wait for the child to become a zombie: it has exited, but nothing has
        // called wait() on it yet, so its PID stays in the process table.
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && is_alive(pid) {
            sleep(Duration::from_millis(10));
        }

        assert_eq!(
            unsafe { libc::kill(pid as libc::pid_t, 0) },
            0,
            "kill(pid, 0) still succeeds for a zombie — the trap this guards"
        );
        assert!(!is_alive(pid), "an unreaped zombie must not read as alive");

        child.wait().expect("reap child");
    }

    #[test]
    fn live_process_is_alive() {
        assert!(is_alive(std::process::id()));
    }
}
