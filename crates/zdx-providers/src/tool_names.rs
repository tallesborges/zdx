//! Tool names as a provider sees them on the wire.
//!
//! The engine names a tool with its lowercase name. A provider's
//! [`ToolNamePolicy`] turns that into the wire name. [`ToolNames`] applies it
//! to the declared tools and to the tool calls replayed from history, and maps
//! the names of streamed tool calls back. A wire name that no declared tool
//! has passes through unchanged, so the engine's unknown-tool error still
//! fires.

use std::collections::HashMap;

use zdx_types::{ProviderError, StreamEvent, ToolDefinition};

/// Longest tool name the Responses API accepts.
const MAX_WIRE_NAME_LEN: usize = 64;

/// How a provider names tools on the wire.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolNamePolicy {
    /// The lowercase name.
    #[default]
    Lowercase,
    /// The lowercase name after a fixed prefix.
    Prefixed(&'static str),
}

impl ToolNamePolicy {
    fn wire_name(self, name: &str) -> String {
        let name = name.to_ascii_lowercase();
        match self {
            Self::Lowercase => name,
            Self::Prefixed(prefix) => format!("{prefix}{name}"),
        }
    }
}

/// The two-way name table of one request.
#[derive(Debug, Clone, Default)]
pub struct ToolNames {
    policy: ToolNamePolicy,
    /// Wire name → engine name, for every declared tool.
    from_wire: HashMap<String, String>,
}

impl ToolNames {
    /// Builds the table for the tools a request declares.
    ///
    /// Every policy maps distinct lowercase names to distinct wire names, so
    /// two tools never share one.
    ///
    /// # Errors
    /// Returns a request error when a wire name is longer than the API
    /// accepts.
    pub fn build(tools: &[ToolDefinition], policy: ToolNamePolicy) -> Result<Self, ProviderError> {
        let mut from_wire = HashMap::new();
        for tool in tools {
            let name = tool.name.to_ascii_lowercase();
            let wire = policy.wire_name(&name);
            if wire.len() > MAX_WIRE_NAME_LEN {
                return Err(ProviderError::request(format!(
                    "tool `{name}` is sent as `{wire}`, longer than {MAX_WIRE_NAME_LEN} characters"
                )));
            }
            from_wire.insert(wire, name);
        }
        Ok(Self { policy, from_wire })
    }

    /// The wire name of `name`, declared or not (a tool call in history may
    /// name a tool this request no longer declares).
    #[must_use]
    pub fn to_wire(&self, name: &str) -> String {
        self.policy.wire_name(name)
    }

    /// The engine name of a streamed tool call's `wire` name.
    #[must_use]
    pub fn from_wire(&self, wire: &str) -> String {
        self.from_wire
            .get(wire)
            .cloned()
            .unwrap_or_else(|| wire.to_string())
    }

    /// Maps the tool name of a streamed tool call back to the engine name.
    #[must_use]
    pub fn restore(&self, event: StreamEvent) -> StreamEvent {
        match event {
            StreamEvent::ContentBlockStart {
                index,
                block_type,
                id,
                name: Some(name),
                data,
                id_origin,
            } => StreamEvent::ContentBlockStart {
                index,
                block_type,
                id,
                name: Some(self.from_wire(&name)),
                data,
                id_origin,
            },
            other => other,
        }
    }

    /// Whether names change on the wire beyond lowercasing.
    #[must_use]
    pub fn renames(&self) -> bool {
        self.policy != ToolNamePolicy::Lowercase
    }
}

#[cfg(test)]
mod tests {
    use zdx_types::ContentBlockType;

    use super::*;

    fn tool(name: &str) -> ToolDefinition {
        ToolDefinition {
            name: name.to_string(),
            description: "d".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    fn start(name: &str) -> StreamEvent {
        StreamEvent::ContentBlockStart {
            index: 0,
            block_type: ContentBlockType::ToolUse,
            id: Some("call_1".to_string()),
            name: Some(name.to_string()),
            data: None,
            id_origin: None,
        }
    }

    fn started_name(event: &StreamEvent) -> Option<&str> {
        match event {
            StreamEvent::ContentBlockStart { name, .. } => name.as_deref(),
            _ => None,
        }
    }

    #[test]
    fn a_prefix_maps_requests_history_and_stream() {
        let names = ToolNames::build(
            &[tool("Bash"), tool("Web_Search")],
            ToolNamePolicy::Prefixed("_"),
        )
        .unwrap();
        assert_eq!(names.to_wire("Bash"), "_bash");
        // History may name a tool this request does not declare.
        assert_eq!(names.to_wire("read"), "_read");
        assert_eq!(
            started_name(&names.restore(start("_web_search"))),
            Some("web_search")
        );
        // An unknown wire name passes through for the engine to refuse.
        assert_eq!(started_name(&names.restore(start("_nope"))), Some("_nope"));
    }

    #[test]
    fn lowercase_keeps_names() {
        let names = ToolNames::build(&[tool("Bash")], ToolNamePolicy::Lowercase).unwrap();
        assert!(!names.renames());
        assert_eq!(names.to_wire("Bash"), "bash");
        assert_eq!(started_name(&names.restore(start("bash"))), Some("bash"));
    }

    #[test]
    fn a_too_long_wire_name_is_refused() {
        let long = "x".repeat(MAX_WIRE_NAME_LEN);
        let err = ToolNames::build(&[tool(&long)], ToolNamePolicy::Prefixed("_")).unwrap_err();
        assert!(!err.is_retryable());
        assert!(err.message.contains("longer than 64"), "{}", err.message);
    }
}
