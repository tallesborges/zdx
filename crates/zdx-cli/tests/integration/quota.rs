//! `zdx quota` starts the Go quota reader in its own place (exec): the
//! reader gets `quota --root ROOT --zdx-version VERSION [--json]`, this
//! `ZDX_HOME`, and this binary as `ZDX_BIN`, and keeps the PID, so the exit
//! status passes through unchanged. Every test uses a temp home. The
//! exec tests use a fake reader script; the opt-in parity test runs a real
//! `zdx-go` named by `ZDX_GO_TEST_BIN` on an empty home, so no
//! credentials exist and no provider is called.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use predicates::prelude::*;
use serde_json::Value;
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
        out: base.join("quota.out"),
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

/// `zdx --root <root> quota <args>` with only a temp home and a minimal PATH.
fn zdx_quota(sb: &Sandbox, reader: Option<&Path>, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &sb.home)
        .env("ZDX_HOME", &sb.zdx_home)
        .args(["--root", sb.root.to_str().unwrap(), "quota"])
        .args(args)
        .stdin(Stdio::null());
    if let Some(reader) = reader {
        cmd.env("ZDX_GO", reader);
    }
    cmd
}

fn fake_reader(sb: &Sandbox) -> PathBuf {
    let reader = sb.home.join("fake-quota");
    script(
        &reader,
        &format!(
            r#"{{ echo "pid=$$"; for a in "$@"; do echo "arg=$a"; done; echo "home=$ZDX_HOME"; echo "bin=$ZDX_BIN"; echo END; }} > '{}'
exit 5"#,
            sb.out.display()
        ),
    );
    reader
}

fn recorded_args(sb: &Sandbox) -> (String, Vec<String>) {
    let out = std::fs::read_to_string(&sb.out).unwrap();
    assert!(out.ends_with("END\n"), "{out}");
    let args = out
        .lines()
        .filter_map(|line| line.strip_prefix("arg="))
        .map(str::to_string)
        .collect();
    (out, args)
}

#[test]
fn test_quota_execs_the_go_reader_with_root_version_and_json() {
    let sb = sandbox();
    let reader = fake_reader(&sb);

    let mut child = zdx_quota(&sb, Some(&reader), &["--json"]).spawn().unwrap();
    let pid = child.id();
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(5), "the reader's exit status is kept");

    let (out, args) = recorded_args(&sb);
    assert!(
        out.starts_with(&format!("pid={pid}\n")),
        "same process: {out}"
    );
    let root = sb.root.to_str().unwrap();
    assert_eq!(
        args,
        [
            "quota",
            "--root",
            root,
            "--zdx-version",
            env!("CARGO_PKG_VERSION"),
            "--json"
        ],
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
fn test_quota_text_mode_sends_no_json_flag() {
    let sb = sandbox();
    let reader = fake_reader(&sb);
    let status = zdx_quota(&sb, Some(&reader), &[]).status().unwrap();
    assert_eq!(status.code(), Some(5));
    let (out, args) = recorded_args(&sb);
    let root = sb.root.to_str().unwrap();
    assert_eq!(
        args,
        [
            "quota",
            "--root",
            root,
            "--zdx-version",
            env!("CARGO_PKG_VERSION")
        ],
        "{out}"
    );
}

#[test]
fn test_quota_fails_closed_without_a_usable_reader() {
    let sb = sandbox();
    let stderr = |reader: Option<&Path>| {
        let output = zdx_quota(&sb, reader, &["--json"]).output().unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "nothing on stdout");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let installed = stderr(None);
    assert!(installed.contains("zdx-go not found"), "{installed}");
    assert!(
        installed.contains(&sb.home.join(".local/bin/zdx-go").display().to_string()),
        "{installed}"
    );
    assert!(installed.contains("just go-install"), "{installed}");

    let relative = stderr(Some(Path::new("zdx-go")));
    assert!(relative.contains("must be an absolute path"), "{relative}");

    let missing = stderr(Some(&sb.home.join("nope")));
    assert!(missing.contains("zdx-go not found"), "{missing}");

    let zdx = PathBuf::from(env!("CARGO_BIN_EXE_zdx"));
    let itself = stderr(Some(&zdx));
    assert!(
        itself.contains("is this zdx binary, not zdx-go"),
        "{itself}"
    );
}

#[test]
fn test_quota_help_lists_json_flag() {
    crate::fixtures::zdx_cmd()
        .args(["quota", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

/// Opt-in: with `ZDX_GO_TEST_BIN` naming a built `zdx-go`, an empty
/// home reports every provider not logged in, with the JSON the Rust
/// command printed before the move to Go.
#[test]
fn test_quota_real_reader_reports_all_providers_not_logged_in() {
    let Some(bin) = std::env::var_os("ZDX_GO_TEST_BIN").filter(|bin| !bin.is_empty()) else {
        eprintln!("skipped: ZDX_GO_TEST_BIN not set");
        return;
    };
    let sb = sandbox();
    let output = zdx_quota(&sb, Some(Path::new(&bin)), &["--json"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let providers = [
        "claude-cli",
        "openai-codex",
        "google-antigravity",
        "grok-build",
        "opencode-go",
        "muse-code",
    ];
    let entries: Vec<Value> = providers
        .iter()
        .map(|provider| {
            serde_json::json!({"provider": provider, "account": null, "error": "not logged in"})
        })
        .collect();
    let want = serde_json::to_string_pretty(&serde_json::json!({ "providers": entries })).unwrap();
    assert_eq!(stdout, format!("{want}\n"));
}
