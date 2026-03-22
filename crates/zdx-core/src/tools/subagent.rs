//! Subagent delegation tool.
//!
//! Allows the model to delegate a scoped task to an isolated child `zdx exec`
//! run and return response text only.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::context::{PromptContextInclusion, build_prompt_with_context_and_layers};
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::subagents::{self, SubagentSummary};

/// Returns the tool definition for the `invoke_subagent` tool.
pub fn definition() -> ToolDefinition {
    definition_with_subagents(&[])
}

/// Returns the tool definition enriched with available named subagents.
pub fn definition_with_subagents(subagents: &[SubagentSummary]) -> ToolDefinition {
    ToolDefinition {
        name: "Invoke_Subagent".to_string(),
        description: build_description(subagents),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Task instructions for the delegated subagent. Be specific about expected output."
                },
                "subagent": {
                    "type": "string",
                    "description": "Optional named subagent configuration. When omitted, uses the default base system prompt behavior."
                }
            },
            "required": ["prompt"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct SubagentInput {
    prompt: String,
    subagent: Option<String>,
}

/// Executes the `invoke_subagent` tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
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
    let definition = match resolve_subagent_definition(&ctx.root, input.subagent.clone()) {
        Ok(definition) => definition,
        Err(err) => return err,
    };

    let model = match resolve_execution_model(definition.as_ref(), &config, ctx) {
        Ok(model) => model,
        Err(err) => return err,
    };

    let system_prompt = match build_system_prompt(&config, &ctx.root, definition.as_ref(), &model) {
        Ok(prompt) => prompt,
        Err(err) => return err,
    };

    let options = build_exec_options(definition.as_ref(), ctx, model, system_prompt);

    match run_exec_subagent(&ctx.root, &prompt, &options).await {
        Ok(response) => ToolOutput::success(Value::String(response)),
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

fn resolve_subagent_definition(
    root: &std::path::Path,
    requested: Option<String>,
) -> Result<Option<subagents::SubagentDefinition>, ToolOutput> {
    let requested = normalize_optional(requested);
    match requested.as_deref() {
        Some(name) => subagents::load_by_name(root, name)
            .map(Some)
            .map_err(|err| {
                ToolOutput::failure(
                    "invalid_input",
                    format!("Unknown subagent '{name}'"),
                    Some(err.to_string()),
                )
            }),
        None => Ok(None),
    }
}

fn resolve_execution_model(
    definition: Option<&subagents::SubagentDefinition>,
    config: &crate::config::Config,
    ctx: &ToolContext,
) -> Result<String, ToolOutput> {
    let model = match definition.and_then(|definition| definition.model.clone()) {
        Some(model) => {
            if let Some(err) = validate_model_supported(&model, ctx) {
                return Err(err);
            }
            model
        }
        None => normalize_optional(ctx.model.clone()).unwrap_or_else(|| config.model.clone()),
    };

    if model.trim().is_empty() {
        return Err(ToolOutput::failure(
            "invalid_config",
            "No model available for invoke_subagent",
            Some("Ensure a parent/default model is available in config".to_string()),
        ));
    }

    Ok(model)
}

fn build_system_prompt(
    config: &crate::config::Config,
    root: &std::path::Path,
    definition: Option<&subagents::SubagentDefinition>,
    model: &str,
) -> Result<String, ToolOutput> {
    let result = match definition {
        Some(definition) => subagents::render_prompt(
            config,
            root,
            definition,
            model,
            &[],
            false,
            PromptContextInclusion::default(),
        )
        .map_err(|err| {
            ToolOutput::failure(
                "invalid_input",
                format!("Failed to render subagent '{}'", definition.name),
                Some(err.to_string()),
            )
        }),
        None => build_prompt_with_context_and_layers(
            config,
            root,
            model,
            &[],
            false,
            PromptContextInclusion::default(),
        )
        .map(|effective| effective.prompt.unwrap_or_default())
        .map_err(|err| {
            ToolOutput::failure(
                "invalid_input",
                "Failed to build default subagent prompt",
                Some(err.to_string()),
            )
        }),
    }?;

    Ok(result)
}

fn build_exec_options(
    definition: Option<&subagents::SubagentDefinition>,
    ctx: &ToolContext,
    model: String,
    system_prompt: String,
) -> ExecSubagentOptions {
    ExecSubagentOptions {
        model: Some(model),
        system_prompt: Some(system_prompt),
        thinking_level: definition
            .and_then(|definition| definition.thinking_level)
            .or(ctx.thinking_level),
        no_tools: false,
        no_system_prompt: false,
        tools_override: definition.and_then(|definition| definition.tools.clone()),
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: ctx.timeout,
    }
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn build_description(subagents: &[SubagentSummary]) -> String {
    let mut description = "Delegate a scoped task to an isolated child agent run. Best for large or splittable tasks to preserve current context. Provide a focused prompt. Avoid using for trivial tasks you can solve directly. Use `subagent` to select a named configuration. Returns response text only.".to_string();

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
    let available: Vec<String> = ctx
        .subagent_available_models
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect();

    let allowed = available
        .iter()
        .any(|available| available.eq_ignore_ascii_case(model));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_definition_schema() {
        let def = definition();
        assert_eq!(def.name, "Invoke_Subagent");
        assert!(def.description.contains("Use `subagent`"));

        let required = def
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
        assert!(!required.iter().any(|v| v == "model"));
        assert!(!required.iter().any(|v| v == "subagent"));
    }

    #[test]
    fn test_definition_has_no_system_prompt() {
        let def = definition();
        let props = def.input_schema.get("properties").unwrap();
        assert!(props.get("system_prompt").is_none());
        assert!(props.get("subagent").is_some());
        assert!(props.get("model").is_none());
    }

    #[test]
    fn test_input_validation_missing_prompt() {
        let input = json!({"subagent": "coder"});
        let parsed: Result<SubagentInput, _> = serde_json::from_value(input);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_resolve_model_priority() {
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());

        assert_eq!(
            normalize_optional(ctx.model.clone()).as_deref(),
            Some("openai:gpt-5.2")
        );
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
    fn test_validate_model_supported_fails_when_available_models_empty() {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        assert!(validate_model_supported("openai:gpt-5.2", &ctx).is_some());
    }

    #[test]
    fn test_build_description_includes_available_subagents() {
        let desc = build_description(&[
            SubagentSummary {
                name: "coder".to_string(),
                description: "Coding helper".to_string(),
            },
            SubagentSummary {
                name: "researcher".to_string(),
                description: "Research helper".to_string(),
            },
        ]);

        assert!(desc.contains("coder (Coding helper)"));
        assert!(desc.contains("researcher (Research helper)"));
    }
}
