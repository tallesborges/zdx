//! The Rust side of jobs (docs/SPEC.md §8 Jobs): one attempt's agent
//! (`zdx jobs attempt`), settings resolution (`zdx jobs resolve`), and
//! `zdx jobs history`. Every other `zdx jobs` subcommand is the Go jobs
//! program (`zdx-jobs`, `apps/shared/cmd/zdx-jobs`), which owns job files,
//! the scheduler, and run bookkeeping, and runs these commands.

use std::ffi::OsString;
use std::io::Read as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use zdx_engine::core::context::{EffectivePrompt, PromptContextInclusion};
use zdx_engine::core::interrupt::InterruptedError;
use zdx_engine::core::thread_persistence::ThreadPersistenceOptions;
use zdx_engine::job_daemon::{self, JobResolution};
use zdx_engine::jobs::records::JobsHome;
use zdx_engine::jobs::{self, JobRunSettings, JobSpec};
use zdx_engine::{config, prompts, subagents};

use super::exec;
use super::go_program::{self, GoProgram};

/// The Go jobs program every other `zdx jobs` subcommand runs:
/// `$ZDX_JOBS` (absolute) or the installed `~/.local/bin/zdx-jobs`.
pub const JOBS: GoProgram = GoProgram {
    name: "zdx-jobs",
    env: "ZDX_JOBS",
    role: "jobs program",
    why: "`zdx jobs` runs the Go jobs program",
    install: "jobs-install",
};

/// Global flags the jobs program receives.
pub struct Delegated<'a> {
    pub root: &'a Path,
    pub system_prompt: Option<&'a str>,
    pub no_thread: bool,
    pub thread: Option<&'a str>,
}

/// Replaces this process with `zdx-jobs --root ROOT [globals] ARGS` (see
/// `go_program` for the environment and the checks).
///
/// # Errors
/// Returns an error, before anything runs, when the program is missing, is
/// not an absolute path, or is this binary; or when the exec fails.
pub fn delegate(globals: &Delegated<'_>, args: Vec<OsString>) -> Result<()> {
    let (mut command, program) = JOBS.command(globals.root)?;
    if let Some(prompt) = globals.system_prompt {
        command.arg("--system-prompt").arg(prompt);
    }
    if globals.no_thread {
        command.arg("--no-thread");
    }
    if let Some(thread) = globals.thread {
        command.arg("--thread").arg(thread);
    }
    command.args(args);
    go_program::exec(command, &program)
}

#[derive(Debug)]
struct PreparedJobRun {
    config: config::Config,
    settings: JobRunSettings,
    effective_prompt: EffectivePrompt,
    tools_override: Option<String>,
    /// Background execution for the run: off when the job's subagent does
    /// not opt in.
    background_commands: bool,
    /// Skills the run's `skill` and `skill_exec` tools may reach: a job
    /// subagent's declared skills, else the configured catalog.
    skill_scope: zdx_engine::skills::SkillScope,
}

/// Jobs are one-shot non-interactive runs, so they carry the exec layer and
/// add the job-specific rules on top of it.
fn job_instruction_layers() -> [&'static str; 2] {
    [
        prompts::EXEC_INSTRUCTION_LAYER,
        prompts::JOB_INSTRUCTION_LAYER,
    ]
}

/// Who is asking for a job run or listing: the caller's root and config
/// (already carrying CLI overrides), plus the raw `--system-prompt` override
/// so a run that reloads config for a declared `root` keeps it.
pub struct Caller<'a> {
    pub root: &'a Path,
    pub config: &'a config::Config,
    pub system_prompt_override: Option<&'a str>,
}

fn job_prompt_context() -> PromptContextInclusion {
    PromptContextInclusion {
        project_context: true,
        memory_index: true,
        skills: true,
    }
}

const ERROR_SUMMARY_MAX_LEN: usize = 400;

/// Prints a job's recent runs (`zdx_engine::jobs::history`), newest first.
pub fn history(name: &str, limit: usize, json: bool) -> Result<()> {
    let runs = jobs::history::recent_runs(&JobsHome::current(), name.trim(), limit, None)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&runs).context("serialize job history")?
        );
    } else {
        println!("{}", jobs::history::prompt_section(name.trim(), &runs));
    }
    Ok(())
}

/// The stdin of `zdx jobs attempt`: the job's declared values and the run's
/// identity, from the jobs program.
#[derive(Debug, Deserialize)]
pub struct AttemptInput {
    #[serde(flatten)]
    spec: JobSpec,
    prompt: String,
    #[serde(default)]
    timeout_secs: Option<u32>,
    run_id: String,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    no_thread: bool,
}

/// What `zdx jobs attempt` writes to its result file.
#[derive(Debug, Serialize)]
struct AttemptResult {
    outcome: &'static str,
    /// False when the run could not be prepared; the jobs program does not
    /// retry it.
    prepared: bool,
    error: Option<String>,
    model: Option<String>,
    root: Option<String>,
}

/// Runs one attempt of a job: resolves and prepares it, runs the agent, and
/// writes the result for the jobs program. `lock_fd` is the job's locked
/// descriptor; it is kept out of the agent's child processes.
///
/// # Errors
/// Returns the attempt's error after the result is written, or an error
/// when the input cannot be read or the result cannot be written.
pub async fn attempt(caller: &Caller<'_>, result_path: &Path, lock_fd: Option<i32>) -> Result<()> {
    if let Some(fd) = lock_fd {
        keep_out_of_children(fd)?;
    }
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("read the attempt input")?;
    let input: AttemptInput = serde_json::from_str(&raw).context("parse the attempt input")?;
    let thread_opts = ThreadPersistenceOptions {
        thread_id: if input.no_thread {
            None
        } else {
            input.thread_id.clone()
        },
        no_save: input.no_thread,
        ..Default::default()
    };
    let history = previous_runs_section(&JobsHome::current(), &input.spec.job, &input.run_id);

    let (result, settings) = match prepare_job_run(caller, &input.spec) {
        Ok(mut prepared) => {
            add_previous_runs(&mut prepared, history.as_deref());
            let root = prepared.settings.root.path.to_string_lossy().to_string();
            let result = Box::pin(exec::run(job_exec_options(
                &prepared,
                &root,
                &thread_opts,
                &input,
            )))
            .await;
            (result, Some(prepared.settings))
        }
        Err(err) => (Err(err.context("prepare job run")), None),
    };
    let interrupted = result
        .as_ref()
        .err()
        .is_some_and(|err| err.downcast_ref::<InterruptedError>().is_some());
    let outcome = match &result {
        Ok(()) => "succeeded",
        Err(_) if interrupted => "interrupted",
        Err(_) => "failed",
    };
    let report = AttemptResult {
        outcome,
        prepared: settings.is_some(),
        error: result.as_ref().err().map(summarize_error),
        model: settings.as_ref().map(|s| s.model.spec.clone()),
        root: settings
            .as_ref()
            .map(|s| s.root.path.to_string_lossy().to_string()),
    };
    std::fs::write(
        result_path,
        serde_json::to_vec(&report).context("serialize the attempt result")?,
    )
    .with_context(|| format!("write the attempt result {}", result_path.display()))?;
    result
}

/// Marks `fd` close-on-exec, so the agent's commands and background jobs do
/// not keep the job locked after this process exits.
#[cfg(unix)]
fn keep_out_of_children(fd: i32) -> Result<()> {
    // SAFETY: fcntl only reads and sets descriptor flags; an fd that is not
    // open returns an error instead of touching memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("mark lock fd {fd} close-on-exec"));
    }
    Ok(())
}

#[cfg(not(unix))]
fn keep_out_of_children(_fd: i32) -> Result<()> {
    Ok(())
}

/// The stdin of `zdx jobs resolve`.
#[derive(Debug, Deserialize)]
struct ResolveInput {
    jobs: Vec<JobSpec>,
}

#[derive(Debug, Serialize)]
struct ResolveOutput {
    /// The daemon context (`--daemon` only).
    daemon: Option<job_daemon::DaemonContextState>,
    results: Vec<JobResolution>,
}

/// Resolves the settings of the jobs on stdin and prints them: inherited
/// from the caller, or from the scheduled daemon's context (`daemon`).
/// `prepare` also renders each run's prompt, as an attempt does.
///
/// # Errors
/// Returns an error when the input cannot be read or `daemon` and `prepare`
/// are combined. A job that does not resolve carries its error instead.
pub fn resolve(caller: &Caller<'_>, daemon: bool, prepare: bool) -> Result<()> {
    if daemon && prepare {
        bail!("--prepare resolves for the caller; it cannot be combined with --daemon");
    }
    let mut raw = String::new();
    std::io::stdin()
        .read_to_string(&mut raw)
        .context("read the jobs to resolve")?;
    let input: ResolveInput = serde_json::from_str(&raw).context("parse the jobs to resolve")?;
    let output = if daemon {
        let state = job_daemon::discover();
        let results = job_daemon::resolve(&state, &input.jobs);
        ResolveOutput {
            daemon: Some(state),
            results,
        }
    } else {
        let results = input
            .jobs
            .iter()
            .map(|job| {
                let settings: Result<JobRunSettings> = if prepare {
                    prepare_job_run(caller, job).map(|prepared| prepared.settings)
                } else {
                    jobs::resolve_run(caller.config, caller.root, job).map(|run| run.settings)
                };
                settings.into()
            })
            .collect();
        ResolveOutput {
            daemon: None,
            results,
        }
    };
    println!(
        "{}",
        serde_json::to_string(&output).context("serialize resolved jobs")?
    );
    Ok(())
}

/// The `exec` options one attempt of a prepared job runs with.
fn job_exec_options<'a>(
    prepared: &'a PreparedJobRun,
    root: &'a str,
    thread_opts: &'a ThreadPersistenceOptions,
    input: &'a AttemptInput,
) -> exec::ExecRunOptions<'a> {
    exec::ExecRunOptions {
        root,
        thread_opts,
        prompt: &input.prompt,
        config: &prepared.config,
        model_override: Some(prepared.settings.model.spec.as_str()),
        effective_prompt_override: Some(&prepared.effective_prompt),
        tool_timeout_override: input.timeout_secs,
        event_filter_override: None,
        stream: false,
        tools_override: prepared.tools_override.as_deref(),
        no_tools: false,
        no_system_prompt: false,
        no_skills: false,
        no_background_commands: !prepared.background_commands,
        subagent: None,
        skill_scope: Some(prepared.skill_scope.clone()),
        activity_kind: Some("job"),
        activity_parent_thread_id: None,
        activity_subagent_name: None,
        steer_file: None,
    }
}

/// What the run is told about the job's earlier runs; `None` when the run
/// log cannot be read (the run goes on without it).
fn previous_runs_section(home: &JobsHome, job: &str, run_id: &str) -> Option<String> {
    match jobs::history::recent_runs(home, job, jobs::history::DEFAULT_RECENT_RUNS, Some(run_id)) {
        Ok(runs) => Some(jobs::history::prompt_section(job, &runs)),
        Err(err) => {
            tracing::warn!(
                err = format!("{err:#}"),
                "Reading the job's previous runs failed"
            );
            None
        }
    }
}

/// Adds the previous-runs section to the run's first runtime context, on
/// both the default and the `subagent:` path.
fn add_previous_runs(prepared: &mut PreparedJobRun, section: Option<&str>) {
    let Some(section) = section else {
        return;
    };
    match prepared.effective_prompt.runtime_context.as_mut() {
        Some(context) => context.add_initial_section(section),
        None => {
            prepared.effective_prompt.runtime_context =
                Some(zdx_engine::core::context::RuntimeContext::with_section(
                    &prepared.settings.root.path,
                    section,
                ));
        }
    }
}

fn prepare_job_run(caller: &Caller<'_>, job: &JobSpec) -> Result<PreparedJobRun> {
    let mut resolved = jobs::resolve_run(caller.config, caller.root, job)?;
    crate::cli::apply_system_prompt_override(&mut resolved.config, caller.system_prompt_override);
    let run_root = resolved.settings.root.path.clone();
    let model = resolved.config.model.clone();
    let instruction_layers = job_instruction_layers();

    // A job run with a subagent follows its definition, like any other
    // subagent run; a job without one keeps background execution.
    let background_commands = resolved
        .subagent
        .as_ref()
        .is_none_or(|definition| definition.background_commands);
    let skill_scope = resolved
        .subagent
        .as_ref()
        .map(|definition| definition.skill_scope(false))
        .unwrap_or_default();
    let (effective_prompt, tools_override) = match resolved.subagent.as_ref() {
        Some(definition) => {
            let (prompt, runtime_context) = subagents::render_prompt_with_layers(
                &resolved.config,
                &run_root,
                definition,
                &model,
                job_prompt_context(),
                &instruction_layers,
            )?;
            (
                EffectivePrompt {
                    prompt: Some(prompt),
                    runtime_context,
                    ..EffectivePrompt::default()
                },
                definition.tools.as_ref().map(|tools| tools.join(",")),
            )
        }
        None => (
            zdx_engine::core::context::build_prompt_with_context_and_layers(
                &resolved.config,
                &run_root,
                &model,
                &instruction_layers,
                job_prompt_context(),
            )
            .context("render job prompt")?,
            None,
        ),
    };

    Ok(PreparedJobRun {
        config: resolved.config,
        settings: resolved.settings,
        effective_prompt,
        tools_override,
        background_commands,
        skill_scope,
    })
}

fn summarize_error(err: &anyhow::Error) -> String {
    let text = format!("{err:#}").replace('\n', " | ");
    if text.len() > ERROR_SUMMARY_MAX_LEN {
        let mut end = ERROR_SUMMARY_MAX_LEN;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &text[..end])
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use zdx_engine::config::{Config, SkillSourceToggles, ThinkingLevel};

    use super::*;

    #[test]
    fn prepare_job_run_uses_effective_model_and_project_context() {
        // Building the prompt materializes bundled skills into $ZDX_HOME.
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Job project note").unwrap();

        let mut config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            thinking_level: ThinkingLevel::Low,
            system_prompt: Some("Base prompt".to_string()),
            ..Default::default()
        };
        config.subagents.enabled = false;
        config.skills.sources = SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        };

        let job = JobSpec {
            job: "demo".to_string(),
            model: Some("openai-codex:gpt-5.4".to_string()),
            subagent: None,
            root: None,
        };

        let prepared = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.clone().unwrap_or_default();

        assert_eq!(prepared.settings.model.spec, "openai-codex:gpt-5.4@low");
        assert_eq!(prepared.settings.model.origin, jobs::SettingOrigin::Job);
        assert_eq!(
            prepared.settings.model.thinking_origin,
            jobs::SettingOrigin::Config
        );
        assert_eq!(prepared.config.model, "openai-codex:gpt-5.4@low");
        assert_eq!(prepared.settings.root.origin, jobs::SettingOrigin::Caller);
        assert!(prompt.contains("Job project note"));
        assert_eq!(prompt.matches(NON_INTERACTIVE_MARKER).count(), 1);
        assert_eq!(prompt.matches(JOB_MARKER).count(), 1);
        assert!(
            initial_context(&prepared).contains("<runtime_context>"),
            "runtime context (skills, memory index) must reach the run"
        );
    }

    /// Both prompt paths get the job's previous runs in the first runtime
    /// context, after the usual snapshot, and a prompt without a snapshot
    /// gets one holding just that section.
    #[test]
    fn previous_runs_reach_the_runtime_context() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = tempdir().unwrap();
        let mut config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            ..Default::default()
        };
        config.skills.sources = no_skill_sources();
        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: None,
            root: None,
        };
        let home = JobsHome::current();
        let section = previous_runs_section(&home, "demo", "current").expect("section");
        assert!(section.contains("No earlier recorded run of `demo`"));

        let mut prepared = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap();
        add_previous_runs(&mut prepared, Some(&section));
        let initial = initial_context(&prepared);
        assert!(initial.starts_with("<runtime_context>"));
        assert!(initial.ends_with(
            "No earlier recorded run of `demo` (`zdx jobs history demo`).\n</runtime_context>"
        ));
        assert_eq!(initial.matches("<runtime_context>").count(), 1);

        prepared.effective_prompt.runtime_context = None;
        add_previous_runs(&mut prepared, Some(&section));
        assert!(initial_context(&prepared).contains("# Previous Runs of This Job"));
    }

    fn test_caller<'a>(config: &'a Config, root: &'a Path) -> Caller<'a> {
        Caller {
            root,
            config,
            system_prompt_override: None,
        }
    }

    fn initial_context(prepared: &PreparedJobRun) -> String {
        prepared
            .effective_prompt
            .runtime_context
            .as_ref()
            .and_then(|context| context.initial.clone())
            .unwrap_or_default()
    }

    const NON_INTERACTIVE_MARKER: &str = "This run is non-interactive";
    const JOB_MARKER: &str = "This run is a ZDX job";

    fn no_skill_sources() -> SkillSourceToggles {
        SkillSourceToggles {
            zdx_user: false,
            zdx_project: false,
            codex_user: false,
            claude_user: false,
            claude_project: false,
            agents_user: false,
            agents_project: false,
        }
    }

    #[test]
    fn prepare_job_run_subagent_keeps_non_interactive_layers_and_context() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("notes.md"), "workspace file").unwrap();
        let subagents_dir = dir.path().join(".zdx").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        std::fs::write(
            subagents_dir.join("helper.md"),
            "---\ndescription: Helper\nmodel: openai-codex:gpt-5.4@high\ntools:\n  - read\n---\nHelper body sentinel.",
        )
        .unwrap();

        let mut config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            ..Default::default()
        };
        config.skills.sources = no_skill_sources();

        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: Some("helper".to_string()),
            root: None,
        };

        let prepared = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.clone().unwrap_or_default();

        assert!(prompt.contains("Helper body sentinel."));
        assert_eq!(prompt.matches(NON_INTERACTIVE_MARKER).count(), 1);
        assert_eq!(prompt.matches(JOB_MARKER).count(), 1);
        assert!(initial_context(&prepared).contains("<runtime_context>"));
        assert_eq!(prepared.tools_override.as_deref(), Some("read"));
        assert_eq!(prepared.settings.model.spec, "openai-codex:gpt-5.4@high");
        assert_eq!(
            prepared.settings.model.origin,
            jobs::SettingOrigin::Subagent
        );
    }

    #[test]
    fn prepare_job_run_uses_declared_root_and_its_config() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let caller = tempdir().unwrap();
        std::fs::write(caller.path().join("AGENTS.md"), "Caller root note").unwrap();
        let declared = tempdir().unwrap();
        std::fs::write(declared.path().join("AGENTS.md"), "Declared root note").unwrap();
        std::fs::create_dir_all(declared.path().join(".zdx")).unwrap();
        std::fs::write(
            declared.path().join(".zdx").join("config.toml"),
            "model = \"openai-codex:gpt-5.4@medium\"\n",
        )
        .unwrap();

        let config = Config {
            model: "anthropic:claude-opus-4-6@high".to_string(),
            ..Default::default()
        };
        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: None,
            root: Some(declared.path().to_string_lossy().to_string()),
        };

        let prepared = prepare_job_run(&test_caller(&config, caller.path()), &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.clone().unwrap_or_default();

        assert_eq!(prepared.settings.root.path, declared.path());
        assert_eq!(prepared.settings.root.origin, jobs::SettingOrigin::Job);
        assert_eq!(prepared.settings.model.spec, "openai-codex:gpt-5.4@medium");
        assert_eq!(prepared.settings.model.origin, jobs::SettingOrigin::Config);
        assert!(prompt.contains("Declared root note"));
        assert!(!prompt.contains("Caller root note"));
    }

    #[test]
    fn prepare_job_run_keeps_cli_system_prompt_with_declared_root() {
        let _home = zdx_engine::test_support::temp_zdx_home();
        let caller = tempdir().unwrap();
        let declared = tempdir().unwrap();
        std::fs::create_dir_all(declared.path().join(".zdx")).unwrap();
        std::fs::write(
            declared.path().join(".zdx").join("config.toml"),
            "system_prompt = \"WORKSPACE-PROMPT\"\n",
        )
        .unwrap();
        let job = JobSpec {
            job: "demo".to_string(),
            model: Some("anthropic:claude-opus-4-6".to_string()),
            subagent: None,
            root: Some(declared.path().to_string_lossy().to_string()),
        };
        let config = Config::default();
        let caller_ctx = Caller {
            root: caller.path(),
            config: &config,
            system_prompt_override: Some("CLI-PROMPT"),
        };

        let prepared = prepare_job_run(&caller_ctx, &job).unwrap();
        let prompt = prepared.effective_prompt.prompt.unwrap_or_default();
        assert!(prompt.contains("CLI-PROMPT"));
        assert!(!prompt.contains("WORKSPACE-PROMPT"));
    }

    #[test]
    fn prepare_job_run_rejects_missing_root() {
        let caller = tempdir().unwrap();
        let missing = caller.path().join("does-not-exist");
        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: None,
            root: Some(missing.to_string_lossy().to_string()),
        };

        let err =
            prepare_job_run(&test_caller(&Config::default(), caller.path()), &job).unwrap_err();
        assert!(
            format!("{err:#}").contains("does not exist or is not a directory"),
            "{err:#}"
        );
    }

    #[test]
    fn validate_fails_for_missing_subagent() {
        let dir = tempdir().unwrap();
        let config = Config::default();
        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: Some("missing-subagent".to_string()),
            root: None,
        };

        let Err(err) = prepare_job_run(&test_caller(&config, dir.path()), &job) else {
            panic!("missing subagent should fail validation");
        };
        assert!(format!("{err:#}").contains("missing-subagent"));
    }

    #[test]
    fn validate_fails_for_subagent_with_unknown_tool() {
        let dir = tempdir().unwrap();
        let subagents_dir = dir.path().join(".zdx").join("subagents");
        std::fs::create_dir_all(&subagents_dir).unwrap();
        std::fs::write(
            subagents_dir.join("broken.md"),
            "---\ndescription: Broken\ntools:\n  - reed\n---\nPrompt",
        )
        .unwrap();

        let config = Config::default();
        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: Some("broken".to_string()),
            root: None,
        };

        let err = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap_err();
        assert!(format!("{err:#}").contains("Unknown tool(s): reed"));
    }

    #[test]
    fn prepare_job_run_rejects_unknown_subagent() {
        let dir = tempdir().unwrap();
        let config = Config {
            model: "anthropic:claude-opus-4-6".to_string(),
            ..Default::default()
        };

        let job = JobSpec {
            job: "demo".to_string(),
            model: None,
            subagent: Some("task".to_string()),
            root: None,
        };

        let err = prepare_job_run(&test_caller(&config, dir.path()), &job).unwrap_err();
        assert!(format!("{err:#}").contains("load subagent 'task'"));
    }
}
