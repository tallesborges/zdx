//! Chat command handler.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;

use anyhow::{Context, Result};
use zdx_engine::config;
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;

use super::exec;
use crate::modes;

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
        })
        .await;
    }

    let root_path = PathBuf::from(root);
    let mut config = config.clone();
    // Loaded from the process cwd; a mode means what `--root` defines, the
    // layers the TUI reloads from.
    let cwd = std::env::current_dir().context("determine current directory")?;
    config.use_modes_of(&cwd, &root_path)?;
    if let Some(model) = model_override {
        config.apply_model_selection(model).context("-m")?;
    }

    let thread = thread_opts.resolve(&root_path).context("resolve thread")?;

    modes::run_interactive_chat(&config, thread, root_path, model_override)
        .await
        .context("interactive chat failed")?;

    Ok(())
}
