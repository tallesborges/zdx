use std::fs;

use predicates::prelude::*;
use serde_json::Value;
use tempfile::tempdir;

#[test]
fn test_config_path_command() {
    let dir = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .args(["config", "path"])
        .assert()
        .success()
        .stdout(predicate::str::contains("config.toml"));
}

#[test]
fn test_config_init_creates_file() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    assert!(!config_path.exists());

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created config at"));

    assert!(config_path.exists());

    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("model ="));
    assert!(contents.contains("# max_tokens ="));
}

#[test]
fn test_config_init_fails_if_exists() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");

    fs::write(&config_path, "# existing config").unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", dir.path())
        .args(["config", "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));
}

#[test]
fn test_config_help_shows_subcommands() {
    crate::fixtures::zdx_cmd()
        .args(["config", "--help"])
        .assert()
        .success()
        .stdout(predicate::str::contains("path"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("get"))
        .stdout(predicate::str::contains("set"))
        .stdout(predicate::str::contains("unset"));
}

#[test]
fn test_config_get_set_unset() {
    let zdx_home = tempdir().unwrap();
    let isolated_cwd = tempdir().unwrap();

    // Init config
    config_cmd(&zdx_home, &isolated_cwd, &["config", "init"])
        .assert()
        .success();

    // Set model
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "set", "model", "test-model-1"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("Set model"));

    // Get model
    config_cmd(&zdx_home, &isolated_cwd, &["config", "get", "model"])
        .assert()
        .success()
        .stdout(predicate::str::contains("test-model-1"));

    // Unset a key
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "unset", "bash_foreground_bound_secs"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("Unset bash_foreground_bound_secs"));

    // Set array elements generically (e.g. model modes)
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "set", "model_modes.0.name", "fast"],
    )
    .assert()
    .success();

    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &[
            "config",
            "set",
            "model_modes.0.primary",
            "google-antigravity:gemini-3.8-flash-high@high",
        ],
    )
    .assert()
    .success();

    // Plain get on array field works without --json
    config_cmd(&zdx_home, &isolated_cwd, &["config", "get", "model_modes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("fast"))
        .stdout(predicate::str::contains("gemini-3.8-flash-high"));

    // Plain get on indexed field
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "get", "model_modes.0.primary"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "google-antigravity:gemini-3.8-flash-high@high",
    ));

    // Unset array element
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "unset", "model_modes.0"],
    )
    .assert()
    .success()
    .stdout(predicate::str::contains("Unset model_modes.0"));
}

#[test]
fn test_config_set_rejects_removed_keys() {
    let zdx_home = tempdir().unwrap();
    let isolated_cwd = tempdir().unwrap();

    config_cmd(&zdx_home, &isolated_cwd, &["config", "init"])
        .assert()
        .success();

    // The removed `thinking` key is rejected instead of silently ignored
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "set", "model_modes.0.thinking", "high"],
    )
    .assert()
    .failure()
    .stderr(predicate::str::contains("model_modes[0].thinking"))
    .stderr(predicate::str::contains("model spec"));

    // The removed `[[favorites]]` list is rejected and points at model modes
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "set", "favorites.0.alias", "Fast"],
    )
    .assert()
    .failure()
    .stderr(predicate::str::contains("`favorites` was removed"))
    .stderr(predicate::str::contains("model_modes"));

    // The removed top-level `thinking_level` key is rejected too
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &["config", "set", "thinking_level", "high"],
    )
    .assert()
    .failure()
    .stderr(predicate::str::contains("thinking_level"))
    .stderr(predicate::str::contains("model spec"));

    // Validation rejects invalid schema writes
    config_cmd(
        &zdx_home,
        &isolated_cwd,
        &[
            "config",
            "set",
            "bash_foreground_bound_secs",
            "not_a_number",
        ],
    )
    .assert()
    .failure()
    .stderr(predicate::str::contains("modified config would be invalid"));
}

/// Builds a `config` CLI invocation isolated to the given home and cwd.
fn config_cmd(
    zdx_home: &tempfile::TempDir,
    cwd: &tempfile::TempDir,
    args: &[&str],
) -> assert_cmd::Command {
    let mut cmd = crate::fixtures::zdx_cmd();
    cmd.current_dir(cwd.path()).env("ZDX_HOME", zdx_home.path());
    cmd.args(args);
    cmd
}

#[test]
fn test_bot_init_updates_global_telegram_config() {
    let zdx_home = tempdir().unwrap();
    let config_path = zdx_home.path().join("config.toml");

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .args([
            "bot",
            "init",
            "--bot-token",
            "123456:abc",
            "--user-id",
            "42",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Saved Telegram bot settings"));

    assert!(config_path.exists());
    let contents = fs::read_to_string(&config_path).unwrap();
    assert!(contents.contains("[telegram]"));
    assert!(contents.contains("bot_token = \"123456:abc\""));
    assert!(contents.contains("allowlist_user_ids = [42]"));
}

#[test]
fn test_bot_command_fails_when_token_is_missing() {
    let zdx_home = tempdir().unwrap();
    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        // A temp HOME too: `zdx bot` would otherwise exec an installed
        // ~/.local/bin/zdx-go if the token check ever stopped first.
        .env("HOME", zdx_home.path())
        .env_remove("ZDX_GO")
        .env_remove("ZDX_TELEGRAM_BOT_TOKEN")
        .env_remove("TELEGRAM_BOT_TOKEN")
        .args(["bot"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("telegram.bot_token"));
}

#[test]
fn test_telegram_command_requires_bot_token() {
    let zdx_home = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .env("ZDX_HOME", zdx_home.path())
        .env_remove("ZDX_TELEGRAM_BOT_TOKEN")
        .env_remove("TELEGRAM_BOT_TOKEN")
        .args([
            "telegram",
            "create-topic",
            "--chat-id",
            "-100123",
            "--name",
            "Test Topic",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("Telegram bot token is required"));
}

#[test]
fn test_mcp_servers_reports_missing_config() {
    let root = tempdir().unwrap();

    let output = crate::fixtures::zdx_cmd()
        .args(["--root", root.path().to_str().unwrap(), "mcp", "servers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["config_exists"], Value::Bool(false));
    assert_eq!(parsed["servers"], Value::Array(Vec::new()));
}

#[test]
fn test_mcp_servers_reports_invalid_config() {
    let root = tempdir().unwrap();
    fs::write(root.path().join(".mcp.json"), "not json").unwrap();

    let output = crate::fixtures::zdx_cmd()
        .args(["--root", root.path().to_str().unwrap(), "mcp", "servers"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let parsed: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["config_exists"], Value::Bool(true));
    assert_eq!(parsed["servers"], Value::Array(Vec::new()));
    assert_eq!(
        parsed["diagnostics"][0]["kind"],
        Value::String("config_error".to_string())
    );
}

#[test]
fn test_mcp_call_rejects_invalid_json() {
    let root = tempdir().unwrap();

    crate::fixtures::zdx_cmd()
        .args([
            "--root",
            root.path().to_str().unwrap(),
            "mcp",
            "call",
            "demo",
            "tool",
            "--json",
            "not-json",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("parse --json input"));
}

/// `set-setting` checks the change against every layer before it writes, a
/// dry run writes nothing, the JSON names the file and which workspaces read
/// it, and `reset-setting` removes the key again. `workspaces --json` then
/// lists each mode of `model_modes`.
#[test]
fn test_config_set_and_reset_setting() {
    let home = tempdir().unwrap();
    let zdx_home = home.path().join(".zdx");
    let project = home.path().join("proj");
    fs::create_dir_all(&zdx_home).unwrap();
    fs::create_dir_all(project.join(".zdx")).unwrap();
    fs::write(
        zdx_home.join("config.toml"),
        "model = \"openai:gpt-6@high\"\n\n[[model_modes]]\nname = \"fast\"\ndescription = \"quick\"\nprimary = \"openai:gpt-6@low\"\n",
    )
    .unwrap();
    let overlay = project.join(".zdx").join("config.toml");
    let zdx = |args: &[&str]| {
        let mut cmd = crate::fixtures::zdx_cmd();
        cmd.env("ZDX_HOME", &zdx_home)
            .env("HOME", home.path())
            .current_dir(&project)
            .args(args);
        cmd
    };
    let json = |args: &[&str]| -> Value {
        serde_json::from_slice(&zdx(args).assert().success().get_output().stdout).unwrap()
    };

    zdx(&["config", "set-setting", "model", "mode:nope", "--dry-run"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("names no configured model mode"));
    let dry = json(&[
        "config",
        "set-setting",
        "model",
        "mode:fast",
        "--dry-run",
        "--json",
    ]);
    assert_eq!(dry["changed"], true);
    assert_eq!(dry["reach"]["reads"], serde_json::json!(["proj"]));
    assert!(!overlay.exists(), "a dry run writes nothing");

    let wrote = json(&["config", "set-setting", "model", "mode:fast", "--json"]);
    assert_eq!(wrote["setting"]["value"], "mode:fast");
    assert_eq!(wrote["setting"]["resolves_to"], "openai:gpt-6@low");
    assert_eq!(
        fs::read_to_string(&overlay).unwrap().trim(),
        "model = \"mode:fast\""
    );

    zdx(&["config", "set-setting", "subagents.enabled", "maybe"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("`on` or `off`"));

    let reset = json(&["config", "reset-setting", "model", "--json"]);
    assert_eq!(reset["changed"], true);
    assert_eq!(reset["setting"]["value"], "openai:gpt-6@high");
    assert_eq!(reset["setting"]["source"]["layer"], "global");

    let modes = r#"[{"name":"fast","description":"quick","primary":"openai:gpt-6@medium","alternatives":[]}]"#;
    json(&[
        "config",
        "set-setting",
        "model_modes",
        modes,
        "--global",
        "--json",
    ]);
    let global = fs::read_to_string(zdx_home.join("config.toml")).unwrap();
    assert!(
        global.contains("primary = \"openai:gpt-6@medium\""),
        "{global}"
    );

    let reports = json(&["config", "workspaces", "--json"]);
    let setting = reports[0]["settings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|setting| setting["key"] == "model_modes")
        .unwrap()
        .clone();
    assert_eq!(setting["modes"][0]["name"], "fast");
    assert_eq!(setting["modes"][0]["thinking"], "medium");
}
