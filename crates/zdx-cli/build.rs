use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=../../.git/HEAD");

    if let Some(ref_path) = git_head_ref_path() {
        println!("cargo:rerun-if-changed=../../.git/{ref_path}");
    }

    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let git_hash = command_output("git", &["rev-parse", "--short=12", "HEAD"])
        .unwrap_or_else(|| "nogit".to_string());
    let dirty = command_status_success("git", &["diff", "--quiet"]).is_some_and(|clean| !clean);
    let dirty_suffix = if dirty { ".dirty" } else { "" };

    println!("cargo:rustc-env=ZDX_BUILD_ID=build.{epoch_secs}.g{git_hash}{dirty_suffix}");
}

fn git_head_ref_path() -> Option<String> {
    let head = std::fs::read_to_string("../../.git/HEAD").ok()?;
    head.trim().strip_prefix("ref: ").map(str::to_string)
}

fn command_output(command: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(command).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    Some(value.trim().to_string()).filter(|value| !value.is_empty())
}

fn command_status_success(command: &str, args: &[&str]) -> Option<bool> {
    Command::new(command)
        .args(args)
        .status()
        .ok()
        .map(|status| status.success())
}
