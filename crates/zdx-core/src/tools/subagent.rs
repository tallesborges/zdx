//! Subagent delegation tool.
//!
//! Allows the model to delegate a scoped task to an isolated child `zdx exec`
//! run and return response text only.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::context::PromptContextInclusion;
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};
use crate::subagents::{self, SubagentSummary};

const DEFAULT_SUBAGENT_NAME: &str = "general_assistant";

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
                    "description": "Optional named subagent configuration. Defaults to 'general_assistant'."
                },
                "model": {
                    "type": "string",
                    "description": "Deprecated optional model override. Used only when the selected subagent does not define its own model. Use a model from <available_models> in the <subagents> system prompt block."
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
    model: Option<String>,
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

    let input: SubagentInput = match serde_json::from_value(input.clone()) {
        Ok(i) => i,
        Err(e) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for invoke_subagent tool",
                Some(format!("Parse error: {e}")),
            );
        }
    };

    let prompt = input.prompt.trim().to_string();
    if prompt.is_empty() {
        return ToolOutput::failure("invalid_input", "prompt cannot be empty", None);
    }

    let config = ctx.config.clone().unwrap_or_default();
    let requested_subagent = normalize_optional(input.subagent.clone())
        .unwrap_or_else(|| DEFAULT_SUBAGENT_NAME.to_string());
    let definition = match subagents::load_by_name(&ctx.root, &requested_subagent) {
        Ok(definition) => definition,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Unknown subagent '{requested_subagent}'"),
                Some(err.to_string()),
            );
        }
    };

    let requested_model = normalize_optional(input.model.clone());
    let model = match definition.model.clone() {
        Some(model) => {
            if let Some(err) = validate_model_supported(&model, ctx) {
                return err;
            }
            model
        }
        None => match requested_model.clone() {
            Some(model) => {
                if let Some(err) = validate_model_supported(&model, ctx) {
                    return err;
                }
                model
            }
            None => normalize_optional(ctx.model.clone()).unwrap_or_else(|| config.model.clone()),
        },
    };

    if model.trim().is_empty() {
        return ToolOutput::failure(
            "invalid_config",
            "No model available for invoke_subagent",
            Some("Provide input.model or ensure parent model is available".to_string()),
        );
    }

    let system_prompt = match subagents::render_prompt(
        &config,
        &ctx.root,
        &definition,
        &model,
        None,
        false,
        PromptContextInclusion::default(),
    ) {
        Ok(prompt) => prompt,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                format!("Failed to render subagent '{requested_subagent}'"),
                Some(err.to_string()),
            );
        }
    };

    let options = ExecSubagentOptions {
        model: Some(model),
        system_prompt: Some(system_prompt),
        thinking_level: definition.thinking_level.or(ctx.thinking_level),
        no_tools: false,
        no_system_prompt: false,
        tools_override: definition.tools.clone(),
        event_filter: Some(vec!["turn_finished".to_string()]),
        timeout: ctx.timeout,
    };

    match run_exec_subagent(&ctx.root, &prompt, &options).await {
        Ok(response) => ToolOutput::success(Value::String(response)),
        Err(err) => ToolOutput::failure(
            "execution_failed",
            "Subagent execution failed",
            Some(err.to_string()),
        ),
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
        assert!(props.get("model").is_some());
    }

    #[test]
    fn test_input_validation_missing_prompt() {
        let input = json!({"subagent": "general_assistant", "model": "codex:gpt-5.3-codex"});
        let parsed: Result<SubagentInput, _> = serde_json::from_value(input);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_resolve_model_priority() {
        let input = SubagentInput {
            prompt: "task".to_string(),
            subagent: Some("general_assistant".to_string()),
            model: Some("codex:gpt-5.3-codex".to_string()),
        };
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());

        assert_eq!(
            normalize_optional(input.model.clone())
                .or_else(|| normalize_optional(ctx.model.clone()))
                .as_deref(),
            Some("codex:gpt-5.3-codex")
        );
    }

    #[test]
    fn test_implicit_model_does_not_require_supported_list_match() {
        let input = SubagentInput {
            prompt: "task".to_string(),
            subagent: Some("general_assistant".to_string()),
            model: None,
        };
        let mut ctx = ToolContext::new(std::path::PathBuf::from("."), None);
        ctx.model = Some("openai:gpt-5.2".to_string());
        ctx.subagent_available_models = vec!["codex:gpt-5.3-codex".to_string()];

        let requested_model = normalize_optional(input.model.clone());
        assert!(requested_model.is_none());

        let chosen = requested_model
            .clone()
            .or_else(|| normalize_optional(ctx.model.clone()));
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
                name: "general_assistant".to_string(),
                description: "General helper".to_string(),
            },
            SubagentSummary {
                name: "automation_assistant".to_string(),
                description: "Headless helper".to_string(),
            },
        ]);

        assert!(desc.contains("general_assistant (General helper)"));
        assert!(desc.contains("automation_assistant (Headless helper)"));
    }
}
