//! `zdx bot` starts the Go gateway in its own place (exec): the gateway gets
//! the bot service argv, this `ZDX_HOME`, and this binary as `ZDX_BIN`, and
//! keeps the PID, so signals and the exit status pass through unchanged.
//! Every test uses a fake gateway script and a temp home; none starts a real
//! gateway or contacts Telegram.

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
    // `zdx bot` checks the Telegram settings before it starts the gateway.
    std::fs::write(
        zdx_home.join("config.toml"),
        "[telegram]\nallowlist_user_ids = [42]\n",
    )
    .unwrap();
    Sandbox {
        out: base.join("gateway.out"),
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

/// `zdx --root <root> bot <args>` with only a temp home, a minimal PATH, and
/// a dummy bot token as its only credential.
fn zdx_bot(sb: &Sandbox, gateway: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &sb.home)
        .env("ZDX_HOME", &sb.zdx_home)
        .env("ZDX_TELEGRAM_BOT_TOKEN", "123456:dummy")
        .args(["--root", sb.root.to_str().unwrap(), "bot"])
        .args(args)
        .stdin(Stdio::null());
    if let Some(gateway) = gateway {
        cmd.env("ZDX_GATEWAY", gateway);
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
            "fake gateway never wrote {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn test_bot_execs_the_gateway_with_the_service_argv() {
    let sb = sandbox();
    let gateway = sb.home.join("fake-gateway");
    script(
        &gateway,
        &format!(
            r#"{{ echo "pid=$$"; for a in "$@"; do echo "arg=$a"; done; echo "home=$ZDX_HOME"; echo "bin=$ZDX_BIN"; echo END; }} > '{}'
exit 7"#,
            sb.out.display()
        ),
    );

    let mut child = zdx_bot(&sb, Some(&gateway), &["--", "--web-dist", "/dist dir"])
        .spawn()
        .unwrap();
    let pid = child.id();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(7), "the gateway's exit status is kept");

    let out = wait_for(&sb.out);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], format!("pid={pid}"), "same process: {out}");
    let args: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.strip_prefix("arg="))
        .collect();
    let root = sb.root.to_str().unwrap();
    assert_eq!(
        args,
        ["--root", root, "bot", "--web-dist", "/dist dir"],
        "{out}"
    );
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
fn test_bot_keeps_an_explicit_zdx_bin_and_passes_signals_through() {
    let sb = sandbox();
    let gateway = sb.home.join("fake-gateway");
    let engine = sb.home.join("zdx");
    script(
        &gateway,
        &format!(
            r#"trap '{{ echo "pid=$$"; echo "bin=$ZDX_BIN"; echo END; }} > "{out}"; exit 42' TERM
echo ready > "{out}.ready"
while :; do sleep 0.05; done"#,
            out = sb.out.display()
        ),
    );

    let mut child = zdx_bot(&sb, Some(&gateway), &[])
        .env("ZDX_BIN", &engine)
        .spawn()
        .unwrap();
    let pid = child.id();
    let ready = PathBuf::from(format!("{}.ready", sb.out.display()));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready.exists() {
        assert!(Instant::now() < deadline, "fake gateway did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
    // SAFETY: `pid` is the child spawned above, still running.
    unsafe { libc::kill(pid as i32, libc::SIGTERM) };
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(42), "restart exit code passes through");

    let out = wait_for(&sb.out);
    assert!(out.starts_with(&format!("pid={pid}\n")), "{out}");
    assert!(
        out.contains(&format!("bin={}\n", engine.display())),
        "{out}"
    );
}

#[test]
fn test_bot_fails_closed_without_a_usable_gateway() {
    let sb = sandbox();
    let stderr = |gateway: Option<&Path>| {
        let output = zdx_bot(&sb, gateway, &[]).output().unwrap();
        assert!(!output.status.success());
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let installed = stderr(None);
    assert!(installed.contains("zdx-gateway not found"), "{installed}");
    assert!(
        installed.contains(&sb.home.join(".local/bin/zdx-gateway").display().to_string()),
        "{installed}"
    );
    assert!(installed.contains("just gateway-install"), "{installed}");

    let relative = stderr(Some(Path::new("zdx-gateway")));
    assert!(relative.contains("must be an absolute path"), "{relative}");

    let missing = stderr(Some(&sb.home.join("nope")));
    assert!(missing.contains("zdx-gateway not found"), "{missing}");

    let zdx = PathBuf::from(env!("CARGO_BIN_EXE_zdx"));
    let itself = stderr(Some(&zdx));
    assert!(itself.contains("is this zdx binary"), "{itself}");

    // Without a bot token nothing starts, not even a usable gateway.
    let gateway = sb.home.join("fake-gateway");
    script(&gateway, &format!("touch '{}'", sb.out.display()));
    let output = zdx_bot(&sb, Some(&gateway), &[])
        .env_remove("ZDX_TELEGRAM_BOT_TOKEN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let no_token = String::from_utf8_lossy(&output.stderr);
    assert!(no_token.contains("telegram.bot_token"), "{no_token}");
    assert!(!sb.out.exists(), "the gateway must not run");
}
