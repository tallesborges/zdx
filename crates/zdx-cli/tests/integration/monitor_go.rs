//! `zdx monitor` starts the Go monitor in its own place (exec): the monitor
//! gets `--root ROOT` and the extra arguments, this `ZDX_HOME`, and this
//! binary as `ZDX_BIN`, and keeps the PID, so signals and the exit status
//! pass through unchanged. Every test uses a fake monitor script and a temp
//! home; none starts the real monitor.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::{TempDir, tempdir};

struct Sandbox {
    _dir: TempDir,
    home: PathBuf,
    zdx_home: PathBuf,
    root: PathBuf,
    out: PathBuf,
}

fn sandbox() -> Sandbox {
    let dir = tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let home = base.join("home");
    let zdx_home = base.join("zdx-home");
    let root = base.join("project");
    for path in [&home, &zdx_home, &root] {
        std::fs::create_dir_all(path).unwrap();
    }
    Sandbox {
        out: base.join("monitor.out"),
        _dir: dir,
        home,
        zdx_home,
        root,
    }
}

fn script(path: &Path, body: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `zdx --root <root> monitor <args>` with only a temp home and a minimal
/// PATH.
fn zdx_monitor(sb: &Sandbox, monitor: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &sb.home)
        .env("ZDX_HOME", &sb.zdx_home)
        .args(["--root", sb.root.to_str().unwrap(), "monitor"])
        .args(args)
        .stdin(Stdio::null());
    if let Some(monitor) = monitor {
        cmd.env("ZDX_MONITOR", monitor);
    }
    cmd
}

fn wait_for(path: &Path) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(path)
            && text.ends_with("END\n")
        {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "fake monitor never wrote {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn test_monitor_execs_the_go_monitor_with_root_and_args() {
    let sb = sandbox();
    let monitor = sb.home.join("fake-monitor");
    script(
        &monitor,
        &format!(
            r#"{{ echo "pid=$$"; for a in "$@"; do echo "arg=$a"; done; echo "home=$ZDX_HOME"; echo "bin=$ZDX_BIN"; echo END; }} > '{}'
exit 5"#,
            sb.out.display()
        ),
    );

    let mut child = zdx_monitor(&sb, Some(&monitor), &["--", "--cli-fallback"])
        .spawn()
        .unwrap();
    let pid = child.id();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(5), "the monitor's exit status is kept");

    let out = wait_for(&sb.out);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], format!("pid={pid}"), "same process: {out}");
    let args: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("arg="))
        .collect();
    let root = sb.root.to_str().unwrap();
    assert_eq!(args, ["--root", root, "--cli-fallback"], "{out}");
    assert!(
        out.contains(&format!("home={}\n", sb.zdx_home.display())),
        "{out}"
    );
    let zdx = PathBuf::from(env!("CARGO_BIN_EXE_zdx"))
        .canonicalize()
        .unwrap();
    assert!(out.contains(&format!("bin={}\n", zdx.display())), "{out}");
}

#[test]
fn test_monitor_passes_signals_through() {
    let sb = sandbox();
    let monitor = sb.home.join("fake-monitor");
    script(
        &monitor,
        &format!(
            r#"trap '{{ echo "pid=$$"; echo END; }} > "{out}"; exit 43' TERM
echo ready > "{out}.ready"
while :; do sleep 0.05; done"#,
            out = sb.out.display()
        ),
    );

    let mut child = zdx_monitor(&sb, Some(&monitor), &[]).spawn().unwrap();
    let pid = child.id();
    let ready = PathBuf::from(format!("{}.ready", sb.out.display()));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "fake monitor did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: `pid` is the child spawned above, still running.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(43), "the signal reaches the monitor");
    let out = wait_for(&sb.out);
    assert!(out.starts_with(&format!("pid={pid}\n")), "{out}");
}

#[test]
fn test_monitor_fails_closed_without_a_usable_binary() {
    let sb = sandbox();
    let stderr = |monitor: Option<&Path>| {
        let output = zdx_monitor(&sb, monitor, &[]).output().unwrap();
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let installed = stderr(None);
    assert!(installed.contains("zdx-monitor not found"), "{installed}");
    assert!(
        installed.contains(&sb.home.join(".local/bin/zdx-monitor").display().to_string()),
        "{installed}"
    );
    assert!(installed.contains("just monitor-install"), "{installed}");

    let relative = stderr(Some(Path::new("zdx-monitor")));
    assert!(relative.contains("must be an absolute path"), "{relative}");

    let missing = stderr(Some(&sb.home.join("nope")));
    assert!(missing.contains("zdx-monitor not found"), "{missing}");

    let zdx = PathBuf::from(env!("CARGO_BIN_EXE_zdx"));
    let itself = stderr(Some(&zdx));
    assert!(
        itself.contains("is this zdx binary, not the monitor"),
        "{itself}"
    );
}
