//! Subagent delegation tool.
//!
//! Allows the model to delegate a scoped task to an isolated child `zdx exec`
//! run and return response text only.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition};
use crate::core::events::ToolOutput;
use crate::core::subagent::{ExecSubagentOptions, run_exec_subagent};

/// Returns the tool definition for the `invoke_subagent` tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Invoke_Subagent".to_string(),
        description: "Delegate a scoped task to an isolated child agent run. Best for large or splittable tasks to preserve current context. Provide a focused prompt. Avoid using for trivial tasks you can solve directly. Optional model override is available when needed. Available model overrides are listed in <available_models> inside the <subagents> system prompt block. Returns response text only.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "prompt": {
                    "type": "string",
                    "description": "Task instructions for the delegated subagent. Be specific about expected output."
                },
                "model": {
                    "type": "string",
                    "description": "Optional model override for this subagent run. Use a model from <available_models> in the <subagents> system prompt block."
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

    let requested_model = normalize_optional(input.model.clone());
    let model = requested_model
        .clone()
        .or_else(|| normalize_optional(ctx.model.clone()));
    let Some(model) = model else {
        return ToolOutput::failure(
            "invalid_config",
            "No model available for invoke_subagent",
            Some("Provide input.model or ensure parent model is available".to_string()),
        );
    };

    if let Some(requested_model) = requested_model
        && let Some(err) = validate_model_supported(&requested_model, ctx)
    {
        return err;
    }

    let options = ExecSubagentOptions {
        model: Some(model),
        thinking_level: ctx.thinking_level,
        no_tools: false,
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
        assert!(def.description.contains("<subagents>"));

        let required = def
            .input_schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert!(required.iter().any(|v| v == "prompt"));
        assert!(!required.iter().any(|v| v == "model"));
    }

    #[test]
    fn test_definition_has_no_system_prompt() {
        let def = definition();
        let props = def.input_schema.get("properties").unwrap();
        assert!(props.get("system_prompt").is_none());
        assert!(props.get("model").is_some());
    }

    #[test]
    fn test_input_validation_missing_prompt() {
        let input = json!({"model": "codex:gpt-5.3-codex"});
        let parsed: Result<SubagentInput, _> = serde_json::from_value(input);
        assert!(parsed.is_err());
    }

    #[test]
    fn test_resolve_model_priority() {
        let input = SubagentInput {
            prompt: "task".to_string(),
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
}
