//! Chat command handler: piped stdin runs `exec`; a terminal gets the Go
//! chat TUI (`apps/tui`).

use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::Result;
use zdx_engine::config;
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;

use super::exec;
use super::go_program::{self, GoProgram};

pub async fn run(
    root: &str,
    thread_opts: &ThreadPersistenceOptions,
    config: &config::Config,
    model_override: Option<&str>,
) -> Result<()> {
    if let Some(thread_id) = thread_opts.thread_id.as_deref() {
        zdx_engine::orchestration::ensure_direct_run_allowed(thread_id)?;
    }
    // If stdin is piped, run exec mode instead
    if !std::io::stdin().is_terminal() {
        let mut prompt = String::new();
        std::io::stdin().lock().read_to_string(&mut prompt)?;
        let prompt = prompt.trim();
        if prompt.is_empty() {
            anyhow::bail!("No input provided via pipe");
        }
        return exec::run(exec::ExecRunOptions {
            root,
            thread_opts,
            prompt,
            config,
            model_override,
            effective_prompt_override: None,
            tool_timeout_override: None,
            event_filter_override: None,
            stream: false,
            tools_override: None,
            no_tools: false,
            no_system_prompt: false,
            no_skills: false,
            no_background_commands: false,
            subagent: None,
            skill_scope: None,
            activity_kind: Some("exec"),
            activity_parent_thread_id: None,
            activity_subagent_name: None,
            steer_file: None,
        })
        .await;
    }

    if thread_opts.no_save {
        anyhow::bail!(
            "--no-thread does not apply to the chat TUI: the engine service saves every turn"
        );
    }
    tui(
        Path::new(root),
        thread_opts.thread_id.as_deref(),
        model_override,
    )
}

/// The Go chat TUI bare `zdx` starts: `zdx-go tui`. Its turns run in the
/// engine service.
pub const TUI: GoProgram = GoProgram {
    command: "tui",
    why: "the interactive chat is the Go TUI",
};

/// Replaces this process with the Go chat TUI, as
/// `zdx-go tui --root ROOT [--thread ID] [--model SPEC]`.
///
/// # Errors
/// Returns an error, before anything runs, when the TUI is missing, is not an
/// absolute path, or is this binary; or when the exec fails.
pub fn tui(root: &Path, thread_id: Option<&str>, model: Option<&str>) -> Result<()> {
    let (mut command, program) = TUI.command(root)?;
    if let Some(id) = thread_id {
        command.arg("--thread").arg(id);
    }
    if let Some(model) = model {
        command.arg("--model").arg(model);
    }
    go_program::exec(command, &program)
}
