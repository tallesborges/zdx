//! Shared schema and constants for the surface-registered `ask_user_question`
//! tool.
//!
//! This tool is NOT an engine builtin: it is only useful on interactive
//! surfaces that can wait for a human answer, so each surface (Telegram bot,
//! TUI) registers its own handler into its `ToolRegistry` and owns the
//! pending-question mechanics, rendering, and answer routing. Only the pure
//! data shared by every surface lives here.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::core::events::ToolOutput;
use crate::tools::ToolDefinition;

pub const TOOL_NAME: &str = "ask_user_question";

/// `ToolOutputDelta` chunk emitted by surface handlers once the pending
/// question is registered and answerable. Surfaces render the question only
/// after receiving this marker, so answer affordances can never appear before
/// an answer can be accepted.
pub const REGISTERED_MARKER: &str = "question_registered";

#[derive(Debug, Deserialize)]
pub struct QuestionInput {
    pub question: String,
    pub options: Vec<OptionInput>,
}

#[derive(Debug, Deserialize)]
pub struct OptionInput {
    pub label: String,
    #[serde(default)]
    pub description: String,
}

/// Parses and validates tool input, returning a ready-to-send failure
/// `ToolOutput` on invalid input.
///
/// # Errors
/// Returns a `ToolOutput::failure` describing the problem.
pub fn parse_input(input: &Value) -> Result<QuestionInput, ToolOutput> {
    let parsed: QuestionInput = serde_json::from_value(input.clone()).map_err(|err| {
        ToolOutput::failure(
            "invalid_input",
            format!("Invalid ask_user_question input: {err}"),
            None,
        )
    })?;
    if parsed.options.len() < 2 || parsed.options.len() > 5 {
        return Err(ToolOutput::failure(
            "invalid_input",
            "ask_user_question requires 2-5 options",
            None,
        ));
    }
    Ok(parsed)
}

#[must_use]
pub fn definition() -> ToolDefinition {
    ToolDefinition {
        name: TOOL_NAME.to_string(),
        description: "Ask the user one question with tappable answer options, then wait for \
                      their reply. Use this when you are blocked on a decision that is genuinely \
                      the user's to make — clarifying ambiguous instructions, choosing between \
                      approaches, or offering concrete follow-up directions. Do NOT use it for \
                      decisions you can resolve from context or sensible defaults; overusing it \
                      interrupts the user. The user can always type a free-form reply instead of \
                      tapping an option — treat whatever answer comes back as authoritative. If \
                      you recommend an option, put it first and append ' (Recommended)' to its \
                      label. Do not add an 'Other' or 'Something else' option. Ask one question \
                      per call; call again for follow-up questions."
            .to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "question": {
                    "type": "string",
                    "description": "A clear, specific question ending with a question mark."
                },
                "options": {
                    "type": "array",
                    "minItems": 2,
                    "maxItems": 5,
                    "description": "2-5 distinct, meaningful choices.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "label": {
                                "type": "string",
                                "description": "Concise button text (1-5 words)."
                            },
                            "description": {
                                "type": "string",
                                "description": "Optional one-line explanation of trade-offs or implications."
                            }
                        },
                        "required": ["label"]
                    }
                }
            },
            "required": ["question", "options"]
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_input() {
        let input = json!({
            "question": "Pick one?",
            "options": [{"label": "A", "description": "first"}, {"label": "B"}]
        });
        let parsed = parse_input(&input).expect("valid input");
        assert_eq!(parsed.question, "Pick one?");
        assert_eq!(parsed.options.len(), 2);
        assert_eq!(parsed.options[0].description, "first");
        assert_eq!(parsed.options[1].description, "");
    }

    #[test]
    fn rejects_too_few_or_many_options() {
        let one = json!({"question": "Q?", "options": [{"label": "A"}]});
        assert!(parse_input(&one).is_err());

        let six = json!({
            "question": "Q?",
            "options": (0..6).map(|i| json!({"label": format!("O{i}")})).collect::<Vec<_>>()
        });
        assert!(parse_input(&six).is_err());
    }

    #[test]
    fn definition_is_not_a_builtin() {
        let builtin_names = crate::tools::all_tool_names();
        assert!(!builtin_names.contains(&TOOL_NAME.to_string()));
    }
}
