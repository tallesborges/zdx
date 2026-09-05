//! Ask-media tool.
//!
//! One-shot understanding of a local image, PDF, audio, or video file: reads
//! the file, sends it inline to a Gemini model with a question, and returns the
//! model's text answer.
//!
//! Read-only by construction — it reads one file and returns text. Shares its
//! core with the `zdx ask-media` CLI (`crate::media::ask_media`), so agents
//! without a shell get the same capability.

use serde::Deserialize;
use serde_json::{Value, json};

use super::{ToolContext, ToolDefinition, resolve_existing_path};
use crate::core::events::ToolOutput;
use crate::media::{DEFAULT_ASK_MEDIA_MODEL, ask_media};

/// Returns the tool definition for the ask-media tool.
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: "Ask_Media".to_string(),
        description: "Understand a local image, PDF, audio, or video file: describe, summarize, transcribe, extract, or answer questions about its contents. Use when the user points at a media file and asks what is in it, or when the file's content did not reach you (for example an attachment the active model cannot read, or a voice note whose transcript is not enough). You write the `prompt`, so ask for exactly what you need — a targeted question like `read the stack trace and the failing file path` beats `describe this image`. Supported inputs: PDF, plain text, images (png/jpg/webp/gif/heic), audio (mp3/wav/ogg/m4a/aac/flac), and video (mp4/mov/mpeg/webm/flv/wmv/3gp), up to 15 MiB. Read-only: it reads the file and returns text, and never modifies it. Stateless — each call re-sends the file, so ask a follow-up question by calling again. Do not use it for media already visible to you, and prefer the transcription tooling for plain verbatim speech-to-text. Requires a configured Gemini provider; it fails clearly when the model is not Gemini."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "file": {
                    "type": "string",
                    "description": "Path to the local media file. Relative paths resolve from the current working directory. Supports $VAR/${VAR} env vars and a leading ~."
                },
                "prompt": {
                    "type": "string",
                    "description": "The question or instruction to run against the file. Be specific about what to extract; vague prompts return vague answers."
                },
                "model": {
                    "type": "string",
                    "description": "Optional Gemini model override (e.g. `gemini:gemini-3.6-flash` for harder reasoning over the file). Defaults to a fast, cheap document-parsing model. Must be a Gemini model."
                }
            },
            "required": ["file", "prompt"],
            "additionalProperties": false
        }),
    }
}

#[derive(Debug, Deserialize)]
struct AskMediaInput {
    file: String,
    prompt: String,
    model: Option<String>,
}

/// Executes the ask-media tool and returns a structured envelope.
pub async fn execute(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let input: AskMediaInput = match serde_json::from_value(input.clone()) {
        Ok(value) => value,
        Err(err) => {
            return ToolOutput::failure(
                "invalid_input",
                "Invalid input for ask_media tool",
                Some(format!("Parse error: {err}")),
            );
        }
    };

    let prompt = input.prompt.trim();
    if prompt.is_empty() {
        return ToolOutput::failure("invalid_input", "prompt cannot be empty", None);
    }

    let file = input.file.trim();
    if file.is_empty() {
        return ToolOutput::failure("invalid_input", "file cannot be empty", None);
    }

    let resolved = match resolve_existing_path(file, &ctx.root) {
        Ok(resolved) => resolved,
        Err(output) => return output,
    };

    let model = input
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(DEFAULT_ASK_MEDIA_MODEL);

    let config = ctx.config.clone().unwrap_or_default();

    match ask_media(&resolved.resolved_path, prompt, model, &config).await {
        Ok(answer) => ToolOutput::success(Value::String(answer)),
        Err(err) => ToolOutput::failure(
            "execution_failed",
            "Ask media failed",
            Some(format!("{err:#}")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolContext {
        ToolContext::new(std::env::temp_dir(), None)
    }

    #[test]
    fn definition_requires_file_and_prompt() {
        let definition = definition();
        assert_eq!(definition.name, "Ask_Media");

        let required = definition.input_schema["required"]
            .as_array()
            .expect("required");
        assert!(required.contains(&json!("file")));
        assert!(required.contains(&json!("prompt")));
        assert!(!required.contains(&json!("model")));
    }

    #[tokio::test]
    async fn rejects_blank_prompt_and_file() {
        let blank_prompt = execute(&json!({ "file": "a.png", "prompt": "  " }), &ctx()).await;
        assert!(!blank_prompt.is_ok());

        let blank_file = execute(&json!({ "file": "  ", "prompt": "what is this" }), &ctx()).await;
        assert!(!blank_file.is_ok());
    }

    #[tokio::test]
    async fn reports_a_missing_file_without_calling_the_provider() {
        let output = execute(
            &json!({
                "file": "definitely-missing-media-file.png",
                "prompt": "what is this"
            }),
            &ctx(),
        )
        .await;

        assert!(!output.is_ok());
    }

    #[tokio::test]
    async fn rejects_a_non_gemini_model() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("sample.png");
        std::fs::write(&file, b"not a real png").unwrap();

        let output = execute(
            &json!({
                "file": file.to_string_lossy(),
                "prompt": "what is this",
                "model": "anthropic:claude-opus-4-6"
            }),
            &ToolContext::new(dir.path().to_path_buf(), None),
        )
        .await;

        assert!(!output.is_ok());
        let rendered = serde_json::to_string(&output).unwrap();
        assert!(rendered.contains("Gemini"), "unexpected output: {rendered}");
    }
}
