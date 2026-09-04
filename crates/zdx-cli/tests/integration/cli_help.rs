use predicates::prelude::*;

#[test]
fn test_version_includes_build_id() {
    crate::fixtures::zdx_cmd()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("+build."));
}

#[test]
fn test_help_shows_all_commands() {
    crate::fixtures::zdx_cmd()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("exec"))
        .stdout(predicate::str::contains("imagine"))
        .stdout(predicate::str::contains("speak"))
        .stdout(predicate::str::contains("mcp"))
        .stdout(predicate::str::contains("automations"))
        .stdout(predicate::str::contains("threads"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--thinking"));
}

#[test]
fn test_mcp_help_shows_subcommands() {
    crate::fixtures::zdx_cmd()
        .args(["mcp", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("servers"))
        .stdout(predicate::str::contains("auth"))
        .stdout(predicate::str::contains("logout"))
        .stdout(predicate::str::contains("tools"))
        .stdout(predicate::str::contains("schema"))
        .stdout(predicate::str::contains("call"));
}

#[test]
fn test_mcp_call_help_shows_json_flag() {
    crate::fixtures::zdx_cmd()
        .args(["mcp", "call", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_imagine_help_shows_flags() {
    crate::fixtures::zdx_cmd()
        .args(["imagine", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--prompt"))
        .stdout(predicate::str::contains("--out"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--aspect"))
        .stdout(predicate::str::contains("--size"));
}

#[test]
fn test_imagine_rejects_invalid_aspect_ratio() {
    crate::fixtures::zdx_cmd()
        .args(["imagine", "-p", "test", "--aspect", "2:1"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid aspect ratio"));
}

#[test]
fn test_imagine_rejects_invalid_image_size() {
    crate::fixtures::zdx_cmd()
        .args(["imagine", "-p", "test", "--size", "8K"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Invalid image size"));
}

#[test]
fn test_speak_help_shows_flags() {
    crate::fixtures::zdx_cmd()
        .args(["speak", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--out"))
        .stdout(predicate::str::contains("--model"))
        .stdout(predicate::str::contains("--voice"))
        .stdout(predicate::str::contains("--format"));
}

#[test]
fn test_speak_requires_text_argument() {
    crate::fixtures::zdx_cmd()
        .args(["speak"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("<TEXT>"));
}

#[test]
fn test_speak_rejects_empty_text() {
    crate::fixtures::zdx_cmd()
        .args(["speak", "   "])
        .assert()
        .failure()
        .stderr(predicate::str::contains("empty text"));
}

#[test]
fn test_threads_help_shows_subcommands() {
    crate::fixtures::zdx_cmd()
        .args(["threads", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("show"))
        .stdout(predicate::str::contains("inspect"))
        .stdout(predicate::str::contains("resume"))
        .stdout(predicate::str::contains("export"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("tools"));
}

#[test]
fn test_threads_inspect_help_shows_thread_id() {
    crate::fixtures::zdx_cmd()
        .args(["threads", "inspect", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("THREAD_ID"));
}

#[test]
fn test_memory_help_shows_subcommands() {
    crate::fixtures::zdx_cmd()
        .args(["memory", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("index"))
        .stdout(predicate::str::contains("search"));
}

#[test]
fn test_threads_tools_help_shows_flags() {
    crate::fixtures::zdx_cmd()
        .args(["threads", "tools", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("--failed"))
        .stdout(predicate::str::contains("--json"));
}

#[test]
fn test_automations_help_shows_subcommands() {
    crate::fixtures::zdx_cmd()
        .args(["automations", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("runs"))
        .stdout(predicate::str::contains("validate"))
        .stdout(predicate::str::contains("run"));
}

#[test]
fn test_bot_help_shows_init_subcommand() {
    crate::fixtures::zdx_cmd()
        .args(["bot", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("profile"));
}

#[test]
fn test_daemon_help_shows_poll_interval() {
    crate::fixtures::zdx_cmd()
        .args(["automations", "daemon", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("poll-interval-secs"));
}
