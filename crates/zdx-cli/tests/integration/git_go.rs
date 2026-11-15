//! `zdx git status|scope|diff` starts the Go Git views in its own place
//! (exec): `zdx-go git --root ROOT <command> [--thread ID] ARGS`, with this
//! `ZDX_HOME` and this binary as `ZDX_BIN`. The argv tests use a fake
//! program; the opt-in test runs a built `zdx-go` named by
//! `ZDX_GO_TEST_BIN` on a fixture repository and an empty home.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        out: base.join("git.out"),
        _dir: dir,
        home,
        zdx_home,
        root,
    }
}

/// `zdx --root <root> git <args>` with only a temp home and a minimal PATH.
fn zdx_git(sb: &Sandbox, program: &Path, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_zdx"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &sb.home)
        .env("ZDX_HOME", &sb.zdx_home)
        .env("ZDX_GO", program)
        .args(["--root", sb.root.to_str().unwrap(), "git"])
        .args(args)
        .stdin(Stdio::null());
    cmd
}

fn recorded_args(sb: &Sandbox) -> Vec<String> {
    std::fs::read_to_string(&sb.out)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("arg="))
        .map(str::to_string)
        .collect()
}

#[test]
fn test_git_execs_zdx_go_with_the_command_and_its_flags() {
    let sb = sandbox();
    let program = sb.home.join("fake-zdx-go");
    std::fs::write(
        &program,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do echo \"arg=$a\"; done > '{}'\nexit 3\n",
            sb.out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let root = sb.root.to_str().unwrap();
    let cases: [(&[&str], &[&str]); 4] = [
        (&["status"], &["status"]),
        (&["scope"], &["scope", "all"]),
        (
            &["scope", "--thread", "t1", "abc1234"],
            &["scope", "--thread", "t1", "abc1234"],
        ),
        (
            &[
                "diff", "--kind", "commit", "--commit", "abc1234", "--", "-odd",
            ],
            &[
                "diff", "--kind", "commit", "--commit", "abc1234", "--", "-odd",
            ],
        ),
    ];
    for (args, expected) in cases {
        let status = zdx_git(&sb, &program, args).status().unwrap();
        assert_eq!(status.code(), Some(3), "the exit status is kept: {args:?}");
        let mut want = vec!["git".to_string(), "--root".to_string(), root.to_string()];
        want.extend(expected.iter().map(|arg| (*arg).to_string()));
        assert_eq!(recorded_args(&sb), want, "{args:?}");
    }
}

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn json(output: &std::process::Output) -> Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap()
}

/// Opt-in: with `ZDX_GO_TEST_BIN` naming a built `zdx-go`, `zdx git`
/// answers the Mini App bodies for the repository at `--root`, and for a
/// thread it asks this binary for the `thread_root` view (the service is
/// not running in the sandbox) and falls back to `--root`.
#[test]
fn test_real_zdx_go_answers_git_views() {
    let Some(bin) = std::env::var_os("ZDX_GO_TEST_BIN").filter(|bin| !bin.is_empty()) else {
        eprintln!("skipped: ZDX_GO_TEST_BIN not set");
        return;
    };
    let sb = sandbox();
    git(&sb.root, &["init", "-q", "-b", "main"]);
    std::fs::write(sb.root.join("a.txt"), "one\n").unwrap();
    git(&sb.root, &["add", "a.txt"]);
    git(&sb.root, &["commit", "-q", "-m", "first"]);
    std::fs::write(sb.root.join("a.txt"), "two\n").unwrap();
    std::fs::write(sb.root.join("new.txt"), "new\n").unwrap();
    let program = PathBuf::from(bin);

    let status = json(&zdx_git(&sb, &program, &["status"]).output().unwrap());
    assert_eq!(status["repository"]["root"], sb.root.to_str().unwrap());
    assert_eq!(status["repository"]["branch"], "main");
    assert_eq!(status["files"]["unstaged"][0]["path"], "a.txt");
    assert_eq!(status["files"]["untracked"][0]["path"], "new.txt");
    assert_eq!(status["commits"][0]["subject"], "first");

    let diff = json(
        &zdx_git(&sb, &program, &["diff", "--kind", "unstaged", "a.txt"])
            .output()
            .unwrap(),
    );
    assert!(diff["content"].as_str().unwrap().contains("+two"), "{diff}");

    let scope = json(&zdx_git(&sb, &program, &["scope"]).output().unwrap());
    assert_eq!(scope["scope"], "all");

    let thread = zdx_git(&sb, &program, &["status", "--thread", "no-such-thread"])
        .output()
        .unwrap();
    let thread = json(&thread);
    assert_eq!(thread["repository"]["source"], "bot_root");
    assert_eq!(thread["repository"]["thread_id"], "no-such-thread");

    let refused = zdx_git(&sb, &program, &["diff", "--kind", "bogus", "a.txt"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
}
