//! Subagent delegation tool.
//!
//! Allows the model to delegate a scoped task to an isolated child `zdx exec`
//! run and return response text only.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, ToolSet, toolset_tool_names};
use crate::core::context::PromptContextInclusion;
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, SubagentStreamSink, run_exec_subagent_holding};
use crate::providers::{ProviderKind, resolve_provider};
use crate::subagents::{self, SubagentDefinition, SubagentSummary};

/// Returns the tool definition for the `invoke_subagent` tool.
pub fn definition() -> ToolDefinition {
    definition_with_subagents(&[], None)
}

/// Returns the tool definition enriched with available named subagents.
///
/// `allowed` restricts the advertised catalog: when it is `Some`, only the
/// listed subagents appear in the `subagent` enum. `subagent` is always
/// required — delegation always names the agent it targets.
pub fn definition_with_subagents(
    subagents: &[SubagentSummary],
    allowed: Option<&[String]>,
) -> ToolDefinition {
    let subagents: Vec<SubagentSummary> = match allowed {
        Some(allowed) => subagents
            .iter()
            .filter(|summary| {
                allowed
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&summary.name))
            })
            .cloned()
            .collect(),
        None => subagents.to_vec(),
    };
    let valid_subagents = supported_subagent_names(&subagents);
    ToolDefinition {
        name: "Invoke_Subagent".to_string(),
        description: build_description(&subagents, allowed),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Self-contained instructions for the delegated subagent. Include the goal, relevant context, constraints/non-goals, file paths, expected output, and verification when useful. The child does not share your full parent reasoning/history, so do not rely on implicit context."
                },
                "subagent": {
                    "type": "string",
                    "description": build_subagent_field_description(&subagents),
                    "enum": valid_subagents
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for this invocation (`provider:model[@thinking][@fast]`, or `mode:<name>` for a configured model mode). Set only when the user explicitly requests it; otherwise omit it to preserve the named subagent profile. Must resolve to an available subagent model."
                },
                "thread_id": {
                    "type": "string",
                    "description": "Continue an earlier run of the same subagent instead of starting from zero: pass the `child_thread_id` an earlier invoke_subagent call returned. The child resumes that thread with its full history and the same artifact directory, so the prompt needs only the new request. The thread must belong to this subagent and this working directory, and must not be running. Omit it to start a new child."
                }
            },
            "required": ["prompt", "subagent"],
            "additionalProperties": false
        }),
    }
}

/// Adds the background behavior to an `invoke_subagent` definition, for a run
/// with background execution: the `background` option, and, when the run has
/// a handoff bound, the automatic handoff of a normal call. Without
/// background execution the schema has neither.
pub fn add_background_option(
    definition: &mut ToolDefinition,
    handoff_bound: Option<std::time::Duration>,
) {
    let normal_call = match handoff_bound {
        Some(bound) => format!(
            " Without `background`, the call waits for the subagent and returns its answer if it \
             finishes within {secs} seconds. A subagent still running after {secs} seconds is not \
             stopped: it is moved to the background and the call returns backgrounded: true with a \
             bg_id and child_thread_id instead of the answer. It is the same run and keeps \
             working, so do NOT invoke it again; follow it with background_output and stop it \
             with background_kill.",
            secs = bound.as_secs()
        ),
        None => " Without `background`, the call blocks until the subagent finishes and returns \
                 its answer."
            .to_string(),
    };
    definition.description.push_str(&normal_call);
    definition.description.push_str(
        " With `background: true` it returns at once with a bg_id and child_thread_id instead, \
         so this turn can keep working: follow the subagent with background_output (wait_secs \
         with wait_for: \"exit\" returns as soon as its result is ready) and stop it with \
         background_kill. A subagent in the background is stopped when this turn ends, so \
         collect its result before you finish.",
    );
    if let Some(properties) = definition
        .input_schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    {
        properties.insert(
            "background".to_string(),
            json!({
                "type": "boolean",
                "description": "Run the subagent without blocking this turn. Returns a bg_id and child_thread_id at once; read progress and the result with background_output and stop it with background_kill. It never outlives this turn: it is stopped when the turn ends."
            }),
        );
    }
}

#[derive(Debug, Deserialize)]
struct SubagentInput {
    prompt: String,
    subagent: Option<String>,
    model: Option<String>,
    thread_id: Option<String>,
}

/// Executes the `invoke_subagent` tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input_value = input;
    if !ctx.subagents_enabled {
        return ToolOutput::failure(
            "disabled",
            "invoke_subagent is disabled in config",
            Some("Set [subagents].enabled = true to enable subagent delegation".to_string()),
        );
    }

    let (input, prompt) = match parse_input(input) {
        Ok(value) => value,
        Err(err) => return err,
    };
    let config = ctx.config.clone().unwrap_or_default();
    let definition = match resolve_subagent_definition(
        &ctx.root,
        input.subagent.clone(),
        ctx.allowed_subagents.as_deref(),
    ) {
        Ok(definition) => definition,
        Err(err) => return err,
    };

    let definition = &definition;

    let resume = match normalize_optional(input.thread_id.clone()) {
        Some(id) => match validate_resume_thread(&ctx.root, definition, &id) {
            Ok(()) => Some(id),
            Err(err) => return err,
        },
        None => None,
    };

    let (model, thinking_level) =
        match resolve_execution_model(definition, &config, ctx, input.model.as_deref()) {
            Ok(resolved) => resolved,
            Err(err) => return err,
        };
    let prompt = build_delegated_prompt(
        &prompt,
        child_has_read_thread_access(definition, &config, &model),
        ctx.current_thread_id.as_deref(),
    );

    let system_prompt = match build_system_prompt(&config, &ctx.root, definition, &model) {
        Ok(prompt) => prompt,
        Err(err) => return err,
    };

    let options = build_exec_options(definition, ctx, model, thinking_level, system_prompt);

    if super::wants_background(input_value) {
        return start_background(definition, ctx, &prompt, options, resume.as_deref()).await;
    }

    // Relay the child's tool activity live to the parent's `invoke_subagent`
    // cell when the engine wired an event sender + tool id for this call.
    let stream = match (ctx.event_sender.clone(), ctx.tool_use_id.clone()) {
        (Some(sender), Some(parent_tool_id)) => Some(SubagentStreamSink {
            sender,
            parent_tool_id,
        }),
        _ => None,
    };

    if let Some(bound) = automatic_handoff_bound(ctx) {
        return run_managed(
            definition,
            ctx,
            &prompt,
            options,
            super::subagent_job::Launch::After(bound),
            stream,
            resume.as_deref(),
        )
        .await;
    }

    run_blocking(ctx, &prompt, options, stream, resume.as_deref()).await
}

/// Runs the child and waits for it to exit.
///
/// The child's process owns its thread lock and the cleanup of a new thread
/// it never wrote to: both are released when the child exits, also when this
/// call is dropped first and its lease is still stopping the child.
async fn run_blocking(
    ctx: &ToolContext,
    prompt: &str,
    mut options: ExecSubagentOptions,
    stream: Option<SubagentStreamSink>,
    resume: Option<&str>,
) -> ToolOutput {
    let ChildThread {
        id,
        new_thread,
        lock,
    } = match ChildThread::open(&ctx.root, &options, resume) {
        Ok(child) => child,
        Err(err) => return err,
    };
    options.thread_id = Some(id.clone());
    let hold = (lock, UnusedThreadCleanup(new_thread.clone()));
    let result = run_exec_subagent_holding(
        &ctx.root,
        prompt,
        &options,
        ctx.cancel_token.clone(),
        stream,
        hold,
    )
    .await;
    let output = subagent_tool_output(
        result,
        ctx.cancel_token
            .as_ref()
            .is_some_and(tokio_util::sync::CancellationToken::is_cancelled),
    );
    super::subagent_job::child_answer(output, &id, new_thread.as_deref())
}

/// Removes a thread created for a call when dropped, if the child never wrote
/// to it.
struct UnusedThreadCleanup(Option<std::path::PathBuf>);

impl Drop for UnusedThreadCleanup {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            super::subagent_job::remove_unused_child_thread(path);
        }
    }
}

/// Starts `definition` as a background subagent owned by the current turn.
async fn start_background(
    definition: &SubagentDefinition,
    ctx: &ToolContext,
    prompt: &str,
    options: ExecSubagentOptions,
    resume: Option<&str>,
) -> ToolOutput {
    if !ctx.background_execution.is_enabled() {
        return ToolOutput::failure(
            "background_disabled",
            "background: true is not available: this run has no background execution. Call \
             invoke_subagent without background; it returns when the subagent finishes.",
            None,
        );
    }
    // Checked before anything is created, so a call that cannot start leaves
    // nothing behind.
    if let Some(refusal) = super::subagent_job::cannot_start(ctx) {
        return refusal;
    }

    run_managed(
        definition,
        ctx,
        prompt,
        options,
        super::subagent_job::Launch::Immediate,
        None,
        resume,
    )
    .await
}

/// When a normal call hands its subagent off to the background: the run's
/// `Bash` handoff bound, when the run has background execution, a surface
/// that keeps relocated work alive, a nonzero bound, and an owning turn.
/// `None` means the call blocks until the child exits, as before.
fn automatic_handoff_bound(ctx: &ToolContext) -> Option<std::time::Duration> {
    match ctx.background_execution {
        super::BackgroundExecution::Enabled {
            handoff_bound: Some(bound),
        } if super::subagent_job::cannot_start(ctx).is_none() => Some(bound),
        _ => None,
    }
}

/// Runs `definition` as a managed child owned by the current turn, against
/// its thread: created up front so a handle can name it, or the continued
/// thread `resume`.
async fn run_managed(
    definition: &SubagentDefinition,
    ctx: &ToolContext,
    prompt: &str,
    mut options: ExecSubagentOptions,
    launch: super::subagent_job::Launch,
    live: Option<SubagentStreamSink>,
    resume: Option<&str>,
) -> ToolOutput {
    let child = match ChildThread::open(&ctx.root, &options, resume) {
        Ok(child) => child,
        Err(err) => return err,
    };
    options.thread_id = Some(child.id.clone());

    super::subagent_job::run(
        super::subagent_job::SubagentJob {
            root: &ctx.root,
            prompt,
            options,
            subagent: &definition.name,
            child_thread_id: child.id,
            child_thread_path: child.new_thread,
            thread_lock: std::sync::Arc::new(child.lock),
        },
        ctx,
        launch,
        live,
    )
    .await
}

/// The thread one child run uses, locked for that run.
struct ChildThread {
    id: String,
    /// The thread file when this call created it; `None` when continuing.
    new_thread: Option<std::path::PathBuf>,
    lock: super::subagent_job::ChildThreadLock,
}

impl ChildThread {
    /// Creates and locks a new child thread, or locks the validated thread
    /// `resume`. A thread that another run is using fails with `thread_busy`.
    fn open(
        root: &std::path::Path,
        options: &ExecSubagentOptions,
        resume: Option<&str>,
    ) -> Result<Self, ToolOutput> {
        if let Some(id) = resume {
            let lock = super::subagent_job::lock_child_thread(id)?;
            // Runs that are not invoke_subagent children (a TUI session, a
            // manual `zdx exec --thread`) do not take the lock; their
            // active-run marker shows them.
            if crate::agent_activity::list_active()
                .iter()
                .any(|run| run.thread_id.as_deref() == Some(id))
            {
                return Err(super::subagent_job::thread_busy(id));
            }
            return Ok(Self {
                id: id.to_string(),
                new_thread: None,
                lock,
            });
        }
        let thread = create_child_thread(root, options).map_err(|err| {
            ToolOutput::failure(
                "thread_error",
                "Failed to create the subagent's thread",
                Some(format!("{err:#}")),
            )
        })?;
        let path = thread.path().clone();
        match super::subagent_job::lock_child_thread(&thread.id) {
            Ok(lock) => Ok(Self {
                id: thread.id.clone(),
                new_thread: Some(path),
                lock,
            }),
            Err(err) => {
                super::subagent_job::remove_unused_child_thread(&path);
                Err(err)
            }
        }
    }
}

/// Checks that `id` names a thread an earlier call of `definition` created in
/// this working directory, before anything runs against it.
fn validate_resume_thread(
    root: &std::path::Path,
    definition: &SubagentDefinition,
    id: &str,
) -> Result<(), ToolOutput> {
    let mismatch = |message: String| Err(ToolOutput::failure("thread_mismatch", message, None));
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(ToolOutput::failure(
            "invalid_input",
            format!("thread_id '{id}' is not a thread id"),
            Some("Pass the child_thread_id an earlier invoke_subagent call returned.".to_string()),
        ));
    }
    let summary = match crate::core::thread_persistence::read_thread_summary(id) {
        Ok(Some(summary)) => summary,
        Ok(None) => {
            return Err(ToolOutput::failure(
                "thread_not_found",
                format!("Subagent thread {id} does not exist"),
                Some(
                    "Pass a child_thread_id an earlier invoke_subagent call returned, or omit \
                     thread_id to start a new child."
                        .to_string(),
                ),
            ));
        }
        Err(err) => {
            return Err(ToolOutput::failure(
                "thread_error",
                format!("Failed to read subagent thread {id}"),
                Some(format!("{err:#}")),
            ));
        }
    };
    if summary.origin_kind.as_deref() != Some("subagent") {
        return mismatch(format!(
            "Thread {id} is not a subagent run; only a child_thread_id returned by \
             invoke_subagent can be continued"
        ));
    }
    let name = &definition.name;
    if summary.subagent_name.as_deref() != Some(name.as_str()) {
        return mismatch(format!(
            "Thread {id} belongs to subagent '{}', not '{name}'",
            summary.subagent_name.as_deref().unwrap_or("unknown")
        ));
    }
    if let Some(thread_root) = summary.root_path.as_deref()
        && !same_dir(std::path::Path::new(thread_root), root)
    {
        return mismatch(format!(
            "Thread {id} runs in {thread_root}, not in this run's working directory {}",
            root.display()
        ));
    }
    Ok(())
}

fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Creates the tagged child thread a background subagent runs against.
fn create_child_thread(
    root: &std::path::Path,
    options: &ExecSubagentOptions,
) -> anyhow::Result<crate::core::thread_persistence::Thread> {
    let mut thread = crate::core::thread_persistence::Thread::new_with_root(root)?;
    thread.set_origin(
        options.thread_origin_kind.clone(),
        options.thread_parent_id.clone(),
        options.thread_subagent_name.clone(),
    );
    // Writes the meta line, so the thread exists on disk before the child
    // opens it by id.
    thread.set_root_path(root)?;
    Ok(thread)
}

fn subagent_tool_output(result: anyhow::Result<String>, cancelled: bool) -> ToolOutput {
    match result {
        Ok(response) => ToolOutput::success(Value::String(response)),
        Err(_) if cancelled => ToolOutput::canceled("Interrupted by user"),
        Err(err) => ToolOutput::failure(
            "execution_failed",
            "Subagent execution failed",
            Some(err.to_string()),
        ),
    }
}

fn parse_input(input: &Value) -> Result<(SubagentInput, String), ToolOutput> {
    let input: SubagentInput = serde_json::from_value(input.clone()).map_err(|e| {
        ToolOutput::failure(
            "invalid_input",
            "Invalid input for invoke_subagent tool",
            Some(format!("Parse error: {e}")),
        )
    })?;

    let prompt = input.prompt.trim().to_string();
    if prompt.is_empty() {
        return Err(ToolOutput::failure(
            "invalid_input",
            "prompt cannot be empty",
            None,
        ));
    }

    Ok((input, prompt))
}

fn build_delegated_prompt(
    prompt: &str,
    can_consult_origin_thread: bool,
    origin_thread_id: Option<&str>,
) -> String {
    if !can_consult_origin_thread {
        return prompt.to_string();
    }

    match origin_thread_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(thread_id) => format!(
            "This task was delegated from thread {thread_id}. If important context is missing or ambiguous, consult that thread before making assumptions.\n\n{prompt}"
        ),
        None => prompt.to_string(),
    }
}

fn child_has_read_thread_access(
    definition: &SubagentDefinition,
    config: &crate::config::Config,
    model: &str,
) -> bool {
    effective_child_tool_names(definition, config, model)
        .into_iter()
        .any(|tool| tool.eq_ignore_ascii_case("read_thread"))
}

fn effective_child_tool_names(
    definition: &SubagentDefinition,
    config: &crate::config::Config,
    model: &str,
) -> Vec<String> {
    if let Some(tools) = definition.tools.clone() {
        return tools;
    }

    let provider = resolve_provider(model).kind;
    let provider_config = config.providers.get(provider);
    if provider_config.tools.is_some() {
        let all_tool_names = crate::tools::all_tool_names();
        let all_tool_names_refs: Vec<&str> = all_tool_names
            .iter()
            .map(std::string::String::as_str)
            .collect();
        return provider_config
            .filter_tools(&all_tool_names_refs)
            .into_iter()
            .map(str::to_string)
            .collect();
    }

    let tool_set = if matches!(provider, ProviderKind::OpenAI | ProviderKind::OpenAICodex) {
        ToolSet::OpenAICodex
    } else {
        ToolSet::Default
    };
    toolset_tool_names(tool_set)
}

fn resolve_subagent_definition(
    root: &std::path::Path,
    requested: Option<String>,
    allowed: Option<&[String]>,
) -> Result<SubagentDefinition, ToolOutput> {
    let Some(requested) = normalize_optional(requested) else {
        return Err(ToolOutput::failure(
            "invalid_input",
            "subagent is required",
            Some("Name the subagent to delegate to; see the `subagent` field.".to_string()),
        ));
    };
    subagents::resolve_named(root, &requested, allowed).map_err(|err| {
        ToolOutput::failure(
            "invalid_input",
            format!("Subagent '{requested}' is not available"),
            Some(err.to_string()),
        )
    })
}

fn resolve_execution_model(
    definition: &SubagentDefinition,
    config: &crate::config::Config,
    ctx: &ToolContext,
    model_override: Option<&str>,
) -> Result<(String, crate::config::ThinkingLevel), ToolOutput> {
    let mut resolved = config.clone();
    let mut requires_validation = false;
    if let Some(level) = ctx.thinking_level {
        resolved.thinking_level = level;
    }
    if let Some(model) = normalize_optional(ctx.model.clone()) {
        resolved.apply_model_spec(&model);
    }
    // Each layer may name a `mode:<name>` instead of a spec, resolved against
    // this run's config at every invocation. An absent layer inherits; an
    // explicit mode that is not configured fails the delegation instead of
    // running another layer's model (and possibly another account).
    for (source, model) in [
        ("definition", definition.model.as_deref()),
        (
            "override",
            config
                .subagents
                .overrides
                .get(&definition.name)
                .and_then(|over| over.model.as_deref()),
        ),
        (
            "invocation",
            model_override
                .map(str::trim)
                .filter(|model| !model.is_empty()),
        ),
    ] {
        let Some(model) = model else { continue };
        if let Err(err) =
            subagents::apply_subagent_model(&mut resolved, &definition.name, source, model)
        {
            return Err(ToolOutput::failure(
                "unknown_model_mode",
                format!("{err:#}"),
                Some(
                    "Use a configured mode or a model id, or fix the subagent's model setting."
                        .to_string(),
                ),
            ));
        }
        requires_validation = true;
    }
    let thinking_level = resolved.thinking_level;
    let model = resolved.model;

    if model.trim().is_empty() {
        return Err(ToolOutput::failure(
            "invalid_config",
            "No model available for invoke_subagent",
            Some("Ensure a parent/default model is available in config".to_string()),
        ));
    }
    if requires_validation && let Some(err) = validate_model_supported(&model, ctx) {
        return Err(err);
    }

    Ok((model, thinking_level))
}

fn build_system_prompt(
    config: &crate::config::Config,
    root: &std::path::Path,
    definition: &SubagentDefinition,
    model: &str,
) -> Result<String, ToolOutput> {
    subagents::render_prompt(
        config,
        root,
        definition,
        model,
        PromptContextInclusion::default(),
    )
    .map_err(|err| {
        ToolOutput::failure(
            "invalid_input",
            format!("Failed to render subagent '{}'", definition.name),
            Some(err.to_string()),
        )
    })
}

fn build_exec_options(
    definition: &SubagentDefinition,
    ctx: &ToolContext,
    model: String,
    thinking_level: crate::config::ThinkingLevel,
    system_prompt: String,
) -> ExecSubagentOptions {
    let subagent_name = definition.name.clone();
    ExecSubagentOptions {
        model: Some(model),
        system_prompt: Some(system_prompt),
        thinking_level: Some(thinking_level),
        no_tools: false,
        no_system_prompt: false,
        no_background_commands: !definition.background_commands,
        tools_override: definition.tools.clone(),
        event_filter: Some(vec![
            "turn_finished".to_string(),
            "tool_started".to_string(),
            "tool_input_completed".to_string(),
            "tool_completed".to_string(),
        ]),
        timeout: ctx.timeout,
        activity_kind: Some("subagent".to_string()),
        activity_parent_thread_id: ctx.current_thread_id.clone(),
        activity_subagent_name: Some(subagent_name.clone()),
        thread_origin_kind: Some("subagent".to_string()),
        thread_parent_id: ctx.current_thread_id.clone(),
        thread_subagent_name: Some(subagent_name),
        thread_id: None,
        steer_file: None,
    }
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn supported_subagent_names(subagents: &[SubagentSummary]) -> Vec<String> {
    let mut names: Vec<String> = Vec::with_capacity(subagents.len());
    for subagent in subagents {
        if !names.iter().any(|name| name == &subagent.name) {
            names.push(subagent.name.clone());
        }
    }
    names
}

fn build_subagent_field_description(subagents: &[SubagentSummary]) -> String {
    let valid = supported_subagent_names(subagents)
        .into_iter()
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "Required named subagent. Delegation always targets one of: {valid}. Skill names are invalid unless they are also listed here."
    )
}

fn build_description(subagents: &[SubagentSummary], allowed: Option<&[String]>) -> String {
    let mut description = if allowed.is_some() {
        "Delegate a scoped task to an isolated child agent run. Each subagent works within the tools and rules of its own definition, as its description states. This agent may only reach the subagents listed in the `subagent` field, which is required. Child runs are self-contained and do not share your full parent reasoning or implicit context, so every important decision, relevant detail, file path, constraint, non-goal, and acceptance criterion must be made explicit in the prompt. Provide a focused prompt with the goal, relevant context, constraints/non-goals, file paths, and expected output. When multiple subtasks are independent, call `invoke_subagent` multiple times in the same response to run them in parallel. Trust but verify: if a child reports claims you plan to rely on, inspect the key evidence before reporting success. The optional `model` spec is a user-controlled override for that invocation only; never change a named subagent's default autonomously. Returns the subagent's final answer as `result` with its `child_thread_id`, not its transcript. To continue the same child later with its full history (for example to revise its work), pass that id as `thread_id`.".to_string()
    } else {
        "Delegate a scoped task to an isolated child agent run. Each subagent works within the tools and rules of its own definition, as its description states. `explorer` and `oracle` are read-only: they research, read, and analyze, but never edit files or change state, so their implementation work stays in this run. `subagent` is required — use `explorer` for multi-round local exploration, repo understanding, or thread-history discovery, and `oracle` for deep diagnosis, debugging dead ends, architecture, or tradeoff analysis. Exact one-hop reads or exact string/symbol lookups are direct work and should not be delegated. When multiple investigations are independent, call `invoke_subagent` multiple times in the same response to run them in parallel; `explorer` is especially appropriate for fan-out across separate search areas. Child runs are self-contained and do not share your full parent reasoning or implicit context, so every important decision, relevant detail, file path, constraint, non-goal, and acceptance criterion must be made explicit in the prompt. Provide a focused prompt with the goal, relevant context, constraints/non-goals, file paths, and expected output. Trust but verify: if a child reports claims you plan to rely on, inspect the key evidence before relying on it. The optional `model` spec is a user-controlled override for that invocation only; never change a named subagent's default autonomously. Returns the subagent's final answer as `result` with its `child_thread_id`, not its transcript. To continue the same child later with its full history (for example to revise its work), pass that id as `thread_id`. Skill names are invalid unless they are also listed as supported subagents.".to_string()
    };

    if !subagents.is_empty() {
        let listed = subagents
            .iter()
            .map(|subagent| format!("{} ({})", subagent.name, subagent.description))
            .collect::<Vec<_>>()
            .join(", ");
        description.push_str(" Available named subagents: ");
        description.push_str(&listed);
        description.push('.');
    }

    description
}

fn validate_model_supported(model: &str, ctx: &ToolContext) -> Option<ToolOutput> {
    let requested = canonical_model_id(model);
    let available: Vec<String> = ctx
        .subagent_available_models
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();
    let canonical_available: Vec<String> = available
        .iter()
        .map(|value| canonical_model_id(value))
        .collect();

    let allowed = canonical_available
        .iter()
        .any(|available| available.eq_ignore_ascii_case(&requested));
    if allowed {
        return None;
    }

    let details = if available.is_empty() {
        "No available subagent models. Enable at least one provider with models in config."
            .to_string()
    } else {
        format!("Available models: {}", available.join(", "))
    };

    Some(ToolOutput::failure(
        "model_not_supported",
        format!("Model '{model}' is not supported for invoke_subagent"),
        Some(details),
    ))
}

fn canonical_model_id(model: &str) -> String {
    let selection = resolve_provider(model);
    let provider = crate::providers::oauth::account_cache_key(
        selection.kind.id(),
        selection.account.as_deref(),
    );
    format!("{provider}:{}", selection.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_subagent_maps_to_cancelled_tool_output() {
        let output = subagent_tool_output(Err(anyhow::anyhow!("Subagent cancelled")), true);
        assert!(matches!(output, ToolOutput::Canceled { .. }));
    }

    #[test]
    fn test_build_delegated_prompt_without_thread_id_returns_original_prompt() {
        assert_eq!(
            build_delegated_prompt("diagnose this", true, None),
            "diagnose this"
        );
    }

    #[test]
    fn test_build_delegated_prompt_without_read_thread_access_returns_original_prompt() {
        assert_eq!(
            build_delegated_prompt("diagnose this", false, Some("thread-123")),
            "diagnose this"
        );
    }

    #[test]
    fn test_build_delegated_prompt_with_thread_id_prefixes_prompt() {
        let prompt = build_delegated_prompt("diagnose this", true, Some("thread-123"));
        assert!(prompt.starts_with(
            "This task was delegated from thread thread-123. If important context is missing or ambiguous, consult that thread before making assumptions."
        ));
        assert!(prompt.ends_with("\n\ndiagnose this"));
    }

    #[test]
    fn test_child_has_read_thread_access_for_explicit_subagent_tools() {
        let definition = subagents::SubagentDefinition {
            name: "oracle".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("oracle.md"),
            source: subagents::SubagentSource::BuiltIn,
            allowed_subagents: None,
            model: None,
            thinking_level: None,
            tools: Some(vec!["read".to_string(), "read_thread".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            background_commands: false,
            prompt_body: "body".to_string(),
        };

        assert!(child_has_read_thread_access(
            &definition,
            &crate::config::Config::default(),
            "anthropic:claude-sonnet-4-5"
        ));
    }

    #[test]
    fn test_child_has_read_thread_access_for_explicit_subagent_tools_without_read_thread() {
        let definition = subagents::SubagentDefinition {
            name: "helper".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("helper.md"),
            source: subagents::SubagentSource::BuiltIn,
            allowed_subagents: None,
            model: None,
            thinking_level: None,
            tools: Some(vec!["read".to_string(), "glob".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            background_commands: false,
            prompt_body: "body".to_string(),
        };

        assert!(!child_has_read_thread_access(
            &definition,
            &crate::config::Config::default(),
            "anthropic:claude-sonnet-4-5"
        ));
    }

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Invoke_Subagent");
        assert!(def.description.contains("`subagent` is required"));
        assert!(def.description.contains("Skill names are invalid"));

        let required = def
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
        assert!(required.iter().any(|v| v == "subagent"));
        assert!(!required.iter().any(|v| v == "model"));
        assert!(!required.iter().any(|v| v == "thinking_level"));
    }

    #[test]
    fn test_definition_has_no_system_prompt() {
        let def = definition();
        let props = def.input_schema.get("properties").unwrap();
        assert!(props.get("system_prompt").is_none());
        assert!(props.get("subagent").is_some());
        assert!(props.get("model").is_some());
        assert!(props.get("thinking_level").is_none());
    }

    #[test]
    fn test_input_validation_missing_prompt() {
        let input = json!({"subagent": "coder"});
        let parsed: Result<SubagentInput, _> = serde_json::from_value(input);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_input_validation_accepts_invocation_overrides() {
        let input = json!({
            "prompt": "inspect this",
            "subagent": "explorer",
            "model": "openai-codex:gpt-5.6-luna@high"
        });
        let parsed: SubagentInput = serde_json::from_value(input).unwrap();

        assert_eq!(
            parsed.model.as_deref(),
            Some("openai-codex:gpt-5.6-luna@high")
        );
    }

    #[test]
    fn test_resolve_model_prefers_invocation_override() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());
        ctx.subagent_available_models = vec![
            "openai:gpt-5.5".to_string(),
            "openai-codex:gpt-5.6-luna".to_string(),
        ];
        let definition = subagents::SubagentDefinition {
            name: "explorer".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("explorer.md"),
            source: subagents::SubagentSource::BuiltIn,
            allowed_subagents: None,
            model: Some("openai:gpt-5.5@low".to_string()),
            thinking_level: Some(crate::config::ThinkingLevel::Low),
            tools: Some(vec!["read".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            background_commands: false,
            prompt_body: "body".to_string(),
        };

        assert_eq!(
            resolve_execution_model(
                &definition,
                &crate::config::Config::default(),
                &ctx,
                Some("openai-codex:gpt-5.6-luna@high")
            )
            .unwrap(),
            (
                "openai-codex:gpt-5.6-luna@high".to_string(),
                crate::config::ThinkingLevel::High
            )
        );
    }

    fn mode_config(name: &str, primary: &str) -> crate::config::Config {
        crate::config::Config {
            model_modes: vec![crate::config::ModelMode {
                name: name.to_string(),
                description: "tier".to_string(),
                primary: primary.to_string(),
                alternatives: Vec::new(),
                thinking: crate::models::ModelSpec::parse(primary)
                    .thinking
                    .unwrap_or_default(),
            }],
            ..Default::default()
        }
    }

    fn explorer_definition(model: Option<&str>) -> subagents::SubagentDefinition {
        subagents::SubagentDefinition {
            name: "explorer".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("explorer.md"),
            source: subagents::SubagentSource::BuiltIn,
            allowed_subagents: None,
            model: model.map(str::to_string),
            thinking_level: None,
            tools: Some(vec!["read".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            background_commands: false,
            prompt_body: "body".to_string(),
        }
    }

    /// A `mode:<name>` override resolves to that mode's primary spec, taking
    /// its thinking level with it.
    #[test]
    fn test_mode_ref_override_resolves_to_primary() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["gemini:gemini-3.8-flash".to_string()];
        let mut config = mode_config("fast", "gemini:gemini-3.8-flash@low");
        config.subagents.overrides.insert(
            "explorer".to_string(),
            crate::config::SubagentOverride {
                model: Some("mode:fast".to_string()),
                thinking_level: None,
            },
        );

        assert_eq!(
            resolve_execution_model(&explorer_definition(None), &config, &ctx, None).unwrap(),
            (
                "gemini:gemini-3.8-flash@low".to_string(),
                crate::config::ThinkingLevel::Low
            )
        );
    }

    /// A `mode:<name>` invocation argument resolves the same way and is still
    /// validated against the available model list.
    #[test]
    fn test_mode_ref_invocation_is_resolved_and_validated() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["claude-cli:claude-opus-5".to_string()];
        let config = mode_config("smart", "claude-cli:claude-opus-5@high");

        assert_eq!(
            resolve_execution_model(
                &explorer_definition(None),
                &config,
                &ctx,
                Some("mode:smart")
            )
            .unwrap(),
            (
                "claude-cli:claude-opus-5@high".to_string(),
                crate::config::ThinkingLevel::High
            )
        );

        // Same ref, but the resolved model is not available to subagents.
        ctx.subagent_available_models = vec!["openai:gpt-5.5".to_string()];
        let err = resolve_execution_model(
            &explorer_definition(None),
            &config,
            &ctx,
            Some("mode:smart"),
        )
        .expect_err("unavailable resolved model must fail");
        assert!(matches!(
            err,
            ToolOutput::Failure { error, .. } if error.code == "model_not_supported"
        ));
    }

    /// An explicit layer naming an undefined mode fails the delegation with
    /// an actionable error instead of running another layer's model, in every
    /// layer. An absent layer still inherits.
    #[test]
    fn test_unknown_mode_ref_fails_instead_of_falling_through() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["openai:gpt-5.5".to_string()];
        ctx.model = Some("openai:gpt-5.5@high".to_string());
        let config = mode_config("fast", "gemini:gemini-3.8-flash@low");
        let with_override = |model: &str| {
            let mut config = config.clone();
            config.subagents.overrides.insert(
                "explorer".to_string(),
                crate::config::SubagentOverride {
                    model: Some(model.to_string()),
                    thinking_level: None,
                },
            );
            config
        };
        let failure = |result: Result<(String, crate::config::ThinkingLevel), ToolOutput>| {
            serde_json::to_string(&result.unwrap_err()).unwrap()
        };

        let text = failure(resolve_execution_model(
            &explorer_definition(Some("openai:gpt-5.5@medium")),
            &with_override("mode:nope"),
            &ctx,
            None,
        ));
        assert!(text.contains("unknown_model_mode"), "{text}");
        assert!(
            text.contains("subagent 'explorer' override model"),
            "{text}"
        );
        assert!(text.contains("configured: fast"), "{text}");

        let text = failure(resolve_execution_model(
            &explorer_definition(Some("mode:gone")),
            &config,
            &ctx,
            None,
        ));
        assert!(
            text.contains("subagent 'explorer' definition model"),
            "{text}"
        );

        let text = failure(resolve_execution_model(
            &explorer_definition(None),
            &config,
            &ctx,
            Some("mode:typo"),
        ));
        assert!(
            text.contains("subagent 'explorer' invocation model"),
            "{text}"
        );

        // No layer at all: the parent's model is inherited.
        assert_eq!(
            resolve_execution_model(&explorer_definition(None), &config, &ctx, None).unwrap(),
            (
                "openai:gpt-5.5@high".to_string(),
                crate::config::ThinkingLevel::High
            )
        );
    }

    /// A mode override is resolved from the config at each invocation, so an
    /// edited mode applies to the next one.
    #[test]
    fn test_mode_ref_override_follows_mode_edits() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec![
            "gemini:gemini-3.8-flash".to_string(),
            "openai:gpt-5.5".to_string(),
        ];
        let mut config = mode_config("fast", "gemini:gemini-3.8-flash@low");
        config.subagents.overrides.insert(
            "explorer".to_string(),
            crate::config::SubagentOverride {
                model: Some("mode:fast".to_string()),
                thinking_level: None,
            },
        );
        let first = resolve_execution_model(&explorer_definition(None), &config, &ctx, None);
        assert_eq!(first.unwrap().0, "gemini:gemini-3.8-flash@low");

        config.model_modes[0].primary = "openai:gpt-5.5@medium".to_string();
        config.model_modes[0].thinking = crate::config::ThinkingLevel::Medium;
        let next = resolve_execution_model(&explorer_definition(None), &config, &ctx, None);
        assert_eq!(
            next.unwrap(),
            (
                "openai:gpt-5.5@medium".to_string(),
                crate::config::ThinkingLevel::Medium
            )
        );
    }

    #[test]
    fn test_build_exec_options_carries_resolved_thinking() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.thinking_level = Some(crate::config::ThinkingLevel::Low);
        let definition = subagents::SubagentDefinition {
            name: "explorer".to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from("explorer.md"),
            source: subagents::SubagentSource::BuiltIn,
            allowed_subagents: None,
            model: None,
            thinking_level: Some(crate::config::ThinkingLevel::Medium),
            tools: Some(vec!["read".to_string()]),
            skills: None,
            auto_loaded_skills: None,
            background_commands: false,
            prompt_body: "body".to_string(),
        };

        let options = build_exec_options(
            &definition,
            &ctx,
            "openai-codex:gpt-5.6-luna@high".to_string(),
            crate::config::ThinkingLevel::High,
            "system".to_string(),
        );

        assert_eq!(
            options.thinking_level,
            Some(crate::config::ThinkingLevel::High)
        );
    }

    /// The child runs without background execution unless the definition
    /// opts in, whatever the parent run allows.
    #[test]
    fn test_build_exec_options_carries_background_policy() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        let build = |definition: &subagents::SubagentDefinition| {
            build_exec_options(
                definition,
                &ctx,
                "openai:gpt-5.2".to_string(),
                crate::config::ThinkingLevel::Low,
                "system".to_string(),
            )
        };

        let default = explorer_definition(None);
        assert!(build(&default).no_background_commands);

        let opted_in = subagents::SubagentDefinition {
            background_commands: true,
            ..explorer_definition(None)
        };
        assert!(!build(&opted_in).no_background_commands);
    }

    fn thread_files() -> Vec<std::path::PathBuf> {
        std::fs::read_dir(crate::config::paths::threads_dir())
            .map(|entries| entries.flatten().map(|entry| entry.path()).collect())
            .unwrap_or_default()
    }

    fn background_ctx(root: &std::path::Path) -> ToolContext {
        ToolContext::new(root.to_path_buf(), None).with_background_execution(
            crate::tools::BackgroundExecution::Enabled {
                handoff_bound: None,
            },
        )
    }

    async fn start_in(ctx: &ToolContext) -> ToolOutput {
        let definition = explorer_definition(None);
        let options = build_exec_options(
            &definition,
            ctx,
            "openai:gpt-5.2".to_string(),
            crate::config::ThinkingLevel::Low,
            "system".to_string(),
        );
        start_background(&definition, ctx, "task", options, None).await
    }

    /// A background call that cannot have an owner is refused before its
    /// child thread is created, so it leaves nothing behind.
    #[tokio::test]
    async fn background_without_an_owner_creates_no_thread() {
        let home = crate::test_support::temp_zdx_home();
        let ctx = background_ctx(home.path());

        let output = start_in(&ctx).await;
        let (code, _, _) = output.error_info().expect("refused");
        assert_eq!(
            code,
            if cfg!(unix) {
                "no_owner"
            } else {
                "unsupported"
            }
        );
        assert!(thread_files().is_empty(), "{:?}", thread_files());
    }

    /// A launch that fails after the child thread was created removes that
    /// thread, because the child never wrote to it.
    #[cfg(unix)]
    #[tokio::test]
    async fn failed_background_launch_removes_its_empty_thread() {
        let home = crate::test_support::temp_zdx_home();
        // The background registry directory cannot be created: the launch
        // fails with an I/O error after the thread exists.
        std::fs::create_dir_all(home.path().join("run")).unwrap();
        std::fs::write(home.path().join("run").join("background"), "not a dir").unwrap();
        let (_guard, tasks) = crate::tools::subagent_job::TurnTasksGuard::new();
        let mut ctx = background_ctx(home.path());
        ctx.turn_tasks = Some(tasks);

        let output = start_in(&ctx).await;
        let (code, _, _) = output.error_info().expect("launch fails");
        assert_eq!(code, "io_error");
        assert!(thread_files().is_empty(), "{:?}", thread_files());
    }

    /// A normal call hands off only with background execution, a handoff
    /// bound, and an owning turn; otherwise it blocks as before.
    #[test]
    fn automatic_handoff_needs_policy_bound_and_owner() {
        let bound = std::time::Duration::from_secs(5);
        let (_guard, tasks) = crate::tools::subagent_job::TurnTasksGuard::new();
        let with = |execution, owner: bool| {
            let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None)
                .with_background_execution(execution);
            if owner {
                ctx.turn_tasks = Some(std::sync::Arc::clone(&tasks));
            }
            automatic_handoff_bound(&ctx)
        };
        let enabled = |handoff_bound| crate::tools::BackgroundExecution::Enabled { handoff_bound };

        let expected = if cfg!(unix) { Some(bound) } else { None };
        assert_eq!(with(enabled(Some(bound)), true), expected);
        assert_eq!(with(enabled(Some(bound)), false), None, "no owning turn");
        assert_eq!(with(enabled(None), true), None, "no bound (0, or surface)");
        assert_eq!(
            with(crate::tools::BackgroundExecution::Disabled, true),
            None,
            "no background execution"
        );
    }

    /// The schema states the behavior the run has: the handoff and its bound
    /// when there is one, a blocking call otherwise; `background` either way.
    #[test]
    fn background_option_text_matches_the_handoff_bound() {
        let mut with_bound = definition();
        add_background_option(&mut with_bound, Some(std::time::Duration::from_secs(45)));
        let text = &with_bound.description;
        assert!(text.contains("finishes within 45 seconds"), "{text}");
        assert!(text.contains("moved to the background"), "{text}");
        assert!(text.contains("backgrounded: true"), "{text}");
        assert!(text.contains("do NOT invoke it again"), "{text}");
        assert!(text.contains("With `background: true`"), "{text}");
        assert!(
            with_bound.input_schema["properties"]
                .get("background")
                .is_some()
        );

        let mut without_bound = definition();
        add_background_option(&mut without_bound, None);
        let text = &without_bound.description;
        assert!(
            text.contains("blocks until the subagent finishes"),
            "{text}"
        );
        assert!(!text.contains("moved to the background"), "{text}");
        assert!(text.contains("With `background: true`"), "{text}");
    }

    /// Cleanup deletes only a thread that holds nothing but its meta line.
    #[test]
    fn unused_child_thread_cleanup_keeps_history() {
        let home = crate::test_support::temp_zdx_home();
        let options = ExecSubagentOptions {
            thread_origin_kind: Some("subagent".to_string()),
            ..ExecSubagentOptions::default()
        };

        let empty = create_child_thread(home.path(), &options).unwrap();
        assert!(empty.path().exists());
        crate::tools::subagent_job::remove_unused_child_thread(empty.path());
        assert!(!empty.path().exists());

        let mut used = create_child_thread(home.path(), &options).unwrap();
        used.append(&crate::core::thread_persistence::ThreadEvent::user_message(
            "real history",
        ))
        .unwrap();
        crate::tools::subagent_job::remove_unused_child_thread(used.path());
        assert!(used.path().exists(), "a thread with history must be kept");
    }

    #[test]
    fn test_parent_model_does_not_require_supported_list_match() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());
        ctx.subagent_available_models = vec!["codex:gpt-5.3-codex".to_string()];

        let chosen = normalize_optional(ctx.model.clone());
        assert_eq!(chosen.as_deref(), Some("openai:gpt-5.2"));
    }

    #[test]
    fn test_validate_model_supported() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["codex:gpt-5.3-codex".to_string()];

        assert!(validate_model_supported("codex:gpt-5.3-codex", &ctx).is_none());
        assert!(validate_model_supported("openai:gpt-5.2", &ctx).is_some());
    }

    #[test]
    fn test_validate_model_supported_accepts_provider_alias_equivalence() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.subagent_available_models = vec!["openai-codex:gpt-5.3-codex".to_string()];

        assert!(validate_model_supported("codex:gpt-5.3-codex", &ctx).is_none());
    }

    #[test]
    fn test_validate_model_supported_fails_when_available_models_empty() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        assert!(validate_model_supported("openai:gpt-5.2", &ctx).is_some());
    }

    #[test]
    fn test_build_description_includes_available_subagents() {
        let desc = build_description(
            &[
                SubagentSummary {
                    name: "coder".to_string(),
                    description: "Coding helper".to_string(),
                },
                SubagentSummary {
                    name: "researcher".to_string(),
                    description: "Research helper".to_string(),
                },
            ],
            None,
        );

        assert!(desc.contains("coder (Coding helper)"));
        assert!(desc.contains("researcher (Research helper)"));
        assert!(desc.contains("`explorer` and `oracle` are read-only"));
        assert!(desc.contains("use `explorer` for multi-round local exploration"));
        assert!(desc.contains("`oracle` for deep diagnosis"));
        assert!(!desc.contains("`task`"));
        assert!(desc.contains("Child runs are self-contained"));
        assert!(desc.contains("do not share your full parent reasoning"));
        assert!(desc.contains("acceptance criterion"));
        assert!(desc.contains("Skill names are invalid"));
    }

    #[test]
    fn test_subagent_field_description_lists_valid_values() {
        let description = build_subagent_field_description(&[
            SubagentSummary {
                name: "explorer".to_string(),
                description: "Exploration".to_string(),
            },
            SubagentSummary {
                name: "oracle".to_string(),
                description: "Deep reasoning".to_string(),
            },
        ]);

        assert!(description.contains("Delegation always targets one of: `explorer`, `oracle`"));
        assert!(description.contains("Required named subagent"));
        assert!(description.contains("Skill names are invalid"));
    }

    #[test]
    fn omitted_subagent_is_rejected() {
        let failure = resolve_subagent_definition(std::path::Path::new("."), None, None)
            .expect_err("omitted subagent must fail");

        assert!(format!("{failure:?}").contains("subagent is required"));
    }

    #[test]
    fn task_alias_is_no_longer_resolvable() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            resolve_subagent_definition(root.path(), Some("task".to_string()), None).is_err(),
            "`task` must not resolve to any subagent"
        );
    }

    #[test]
    fn restricted_schema_lists_only_allowed_subagents_and_requires_the_field() {
        let allowed = vec!["explorer".to_string()];
        let definition = definition_with_subagents(
            &[
                SubagentSummary {
                    name: "explorer".to_string(),
                    description: "Read-only exploration".to_string(),
                },
                SubagentSummary {
                    name: "oracle".to_string(),
                    description: "Deep reasoning".to_string(),
                },
            ],
            Some(&allowed),
        );

        let schema = &definition.input_schema;
        let enum_values = schema["properties"]["subagent"]["enum"]
            .as_array()
            .expect("subagent enum");
        assert_eq!(enum_values, &vec![json!("explorer")]);

        let required = schema["required"].as_array().expect("required");
        assert!(required.contains(&json!("subagent")));
        assert!(!definition.description.contains("oracle (Deep reasoning)"));
    }

    #[test]
    fn restricted_selection_rejects_disallowed_and_omitted_subagents() {
        let root = tempfile::tempdir().unwrap();
        let allowed = vec!["explorer".to_string()];

        let omitted = resolve_subagent_definition(root.path(), None, Some(&allowed));
        assert!(omitted.is_err(), "omitted subagent must be rejected");

        for denied in ["task", "oracle"] {
            let result =
                resolve_subagent_definition(root.path(), Some(denied.to_string()), Some(&allowed));
            assert!(result.is_err(), "{denied} must be rejected");
        }
    }

    #[test]
    fn unrestricted_schema_requires_a_named_subagent() {
        let definition = definition_with_subagents(&[], None);
        let required = definition.input_schema["required"]
            .as_array()
            .expect("required");
        assert!(required.contains(&json!("subagent")));
        assert!(
            definition.input_schema["properties"]["subagent"]["enum"]
                .as_array()
                .expect("enum")
                .is_empty()
        );
    }

    fn named(name: &str) -> SubagentDefinition {
        subagents::SubagentDefinition {
            name: name.to_string(),
            description: "desc".to_string(),
            path: std::path::PathBuf::from(format!("{name}.md")),
            source: subagents::SubagentSource::Project,
            allowed_subagents: None,
            model: None,
            thinking_level: None,
            tools: None,
            skills: None,
            auto_loaded_skills: None,
            background_commands: false,
            prompt_body: "body".to_string(),
        }
    }

    /// A thread as a finished `invoke_subagent` call leaves it: tagged with
    /// its subagent, with some history.
    fn subagent_thread(root: &std::path::Path, origin: Option<&str>, subagent: &str) -> String {
        let options = ExecSubagentOptions {
            thread_origin_kind: origin.map(str::to_string),
            thread_subagent_name: Some(subagent.to_string()),
            ..Default::default()
        };
        let thread = create_child_thread(root, &options).unwrap();
        let mut content = std::fs::read_to_string(thread.path()).unwrap();
        content.push_str("{\"type\":\"message\",\"role\":\"user\",\"text\":\"task\"}\n");
        std::fs::write(thread.path(), content).unwrap();
        thread.id.clone()
    }

    fn error_code(result: Result<(), ToolOutput>) -> String {
        let output = result.expect_err("refused");
        let (code, _, _) = output.error_info().expect("failure");
        code.to_string()
    }

    /// Continuing is allowed only for an existing thread that the same
    /// subagent ran in the same working directory.
    #[test]
    fn resume_validates_the_thread_before_running() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let helper = named("helper");

        let id = subagent_thread(root.path(), Some("subagent"), "helper");
        assert!(validate_resume_thread(root.path(), &helper, &id).is_ok());

        assert_eq!(
            error_code(validate_resume_thread(
                root.path(),
                &helper,
                "missing-thread"
            )),
            "thread_not_found"
        );
        assert_eq!(
            error_code(validate_resume_thread(root.path(), &helper, "../escape")),
            "invalid_input"
        );
        assert_eq!(
            error_code(validate_resume_thread(root.path(), &named("other"), &id)),
            "thread_mismatch"
        );
        assert_eq!(
            error_code(validate_resume_thread(other_root.path(), &helper, &id)),
            "thread_mismatch"
        );
        let helper_run = subagent_thread(root.path(), Some("helper:title"), "helper");
        assert_eq!(
            error_code(validate_resume_thread(root.path(), &helper, &helper_run)),
            "thread_mismatch"
        );
    }

    /// One child thread runs at most one child at a time: a second call is
    /// refused while the first holds the thread, and allowed afterwards.
    #[test]
    fn a_running_thread_cannot_be_continued_twice() {
        let _home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let id = subagent_thread(root.path(), Some("subagent"), "helper");
        let options = ExecSubagentOptions::default();

        let first = ChildThread::open(root.path(), &options, Some(&id)).expect("free");
        assert!(
            first.new_thread.is_none(),
            "a continued thread is never removed"
        );
        let second = ChildThread::open(root.path(), &options, Some(&id))
            .err()
            .expect("busy");
        assert_eq!(second.error_info().unwrap().0, "thread_busy");

        drop(first);
        assert!(ChildThread::open(root.path(), &options, Some(&id)).is_ok());
    }

    /// An answer names the thread that holds it; a failure names it only
    /// while the thread exists, and a new thread the child never wrote to is
    /// removed.
    #[test]
    fn child_answers_carry_their_thread() {
        let home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let id = subagent_thread(root.path(), Some("subagent"), "helper");
        let answer =
            crate::tools::subagent_job::child_answer(ToolOutput::success(json!("done")), &id, None);
        assert_eq!(
            serde_json::to_value(&answer).unwrap()["data"],
            json!({ "result": "done", "child_thread_id": id })
        );

        let failed = crate::tools::subagent_job::child_answer(
            ToolOutput::failure("execution_failed", "Subagent execution failed", None),
            &id,
            None,
        );
        let (_, _, details) = failed.error_info().unwrap();
        assert!(details.unwrap_or_default().contains(&id));

        let empty = create_child_thread(root.path(), &ExecSubagentOptions::default()).unwrap();
        let failed = crate::tools::subagent_job::child_answer(
            ToolOutput::failure("execution_failed", "Subagent execution failed", None),
            &empty.id,
            Some(empty.path()),
        );
        let (_, _, details) = failed.error_info().unwrap();
        assert!(details.is_none(), "{details:?}");
        assert!(
            !home
                .path()
                .join("threads")
                .join(format!("{}.jsonl", empty.id))
                .exists()
        );
    }

    /// A dropped blocking call does not free its thread early: the lock stays
    /// with the child until the lease has stopped it, so an immediate
    /// continuation is refused, and the next one after the exit runs.
    #[cfg(unix)]
    #[tokio::test]
    async fn dropped_call_keeps_the_thread_locked_until_the_child_exits() {
        use zdx_tools::process_supervisor::SupervisedCommand;

        let home = crate::test_support::temp_zdx_home();
        let root = tempfile::tempdir().unwrap();
        let id = subagent_thread(root.path(), Some("subagent"), "helper");
        let options = ExecSubagentOptions::default();
        let pidfile = home.path().join("child.pid");

        let ChildThread { lock, .. } =
            ChildThread::open(root.path(), &options, Some(&id)).expect("free");
        // A child that ignores TERM, so stopping it takes the whole grace.
        let mut command = SupervisedCommand::new("/bin/sh");
        command
            .args([
                "-c",
                "trap '' TERM; echo $$ > \"$1\"; while :; do sleep 0.1; done",
                "fixture",
                pidfile.to_str().unwrap(),
            ])
            .stdin_null();
        let mut child = command
            .spawn(std::time::Duration::from_millis(800))
            .await
            .expect("spawn fixture");
        child.keep_until_exit((lock, UnusedThreadCleanup(None)));
        let pid = loop {
            if let Ok(raw) = std::fs::read_to_string(&pidfile)
                && let Ok(pid) = raw.trim().parse::<i32>()
            {
                break pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        };

        // The call is dropped: its lease starts stopping the child.
        drop(child);
        let busy = ChildThread::open(root.path(), &options, Some(&id))
            .err()
            .expect("the stopping child still holds the thread");
        assert_eq!(busy.error_info().unwrap().0, "thread_busy");

        let resumed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                if let Ok(child) = ChildThread::open(root.path(), &options, Some(&id)) {
                    break child;
                }
                tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the thread was never released");
        drop(resumed);
        assert!(
            unsafe { libc::kill(pid, 0) } != 0,
            "the thread was released while the child was alive"
        );
    }
}
