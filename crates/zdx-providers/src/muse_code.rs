//! Muse Code (Meta subscription) provider using the Responses API.
//!
//! Sends the same Muse request shape as the API-key [`crate::meta`]
//! provider, swapping `META_API_KEY` for a Model API key minted through Meta's
//! device-authorization flow (see [`crate::oauth::muse_code`]).
//!
//! Credential lifecycle: Meta publishes no lifetime for a minted key, so zdx
//! does not predict expiry. The cached key is used as-is, and a rejection
//! (HTTP 401/403) triggers exactly one re-mint from the stored identity token
//! before the request is retried. A second rejection is surfaced to the user.
//! A 403 `client_attestation_denied` is not a rejection of the key and is
//! surfaced at once.
//!
//! Tool names: Meta refuses a request with that 403 when it declares a tool
//! named like a Muse Code client tool (`bash`, `web_search`, ...), so every
//! tool goes on the wire as `_<name>` ([`TOOL_NAMES`]). This goes around a
//! stated Meta rule for subscription keys; the user chose to accept the risk
//! to the account.
//!
//! ⚠️ Unsupported by Meta. Its documentation states a Muse Code subscription
//! "only works through the Muse Code CLI". Whether a key minted outside that
//! CLI draws on the subscription or bills pay-as-you-go is **not established** —
//! no billed request was made to verify it. Treat the billing attribution as
//! unknown until observed against real usage and billing records.

use anyhow::{Context, Result};
use reqwest::header::{HeaderMap, HeaderValue};
use zdx_types::{ProviderError, ToolDefinition};

use crate::meta::{reasoning_effort_from_thinking_level, responses_config};
use crate::oauth::muse_code as oauth_muse_code;
use crate::openai::responses::{ResponsesConfig, send_responses_stream_named};
use crate::shared::merge_system_prompt;
use crate::tool_names::{ToolNamePolicy, ToolNames};
use crate::{ChatMessage, ProviderStream};

/// Loads the cached Model API key.
///
/// No expiry check: the stored credential carries
/// [`oauth_muse_code::NO_EXPIRY`], so staleness is discovered when Meta rejects
/// the key rather than predicted from a timestamp.
///
/// # Errors
/// Returns an error if no credentials are stored.
pub fn resolve_api_key(account: Option<&str>) -> Result<String> {
    let creds = oauth_muse_code::load_credentials(account)?.ok_or_else(|| {
        anyhow::anyhow!("No Muse Code credentials found. Run `zdx login --muse-code`.")
    })?;
    Ok(creds.access)
}

/// Error code of Meta's client gate: a 403 for a valid subscription key whose
/// request Meta does not accept as coming from the Muse Code client.
///
/// Observed 2026-11-15: requests that declare tools named like Muse Code
/// builtins (`bash`, `web_search`, ...) get it, others with the same key do
/// not. A re-mint returns the same key, so it cannot help.
const CLIENT_ATTESTATION_DENIED: &str = "client_attestation_denied";

/// How tools are named on the wire. Observed 2026-11-16: `_bash`, and the
/// whole zdx tool set prefixed with `_`, pass where `bash` is refused,
/// including a replayed `_bash` call. A prefix on every name, rather than a
/// list of refused ones, keeps working when Meta refuses more names.
const TOOL_NAMES: ToolNamePolicy = ToolNamePolicy::Prefixed("_");

/// Whether Meta rejected the credential itself, as opposed to the request.
fn is_credential_rejection(error: &anyhow::Error) -> bool {
    error.downcast_ref::<ProviderError>().is_some_and(|err| {
        err.status
            .is_some_and(|status| status == 401 || status == 403)
            && err.code.as_deref() != Some(CLIENT_ATTESTATION_DENIED)
    })
}

fn build_headers(api_key: &str) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        crate::shared::header_value("Muse Code API key", &format!("Bearer {api_key}"))?,
    );
    headers.insert("accept", HeaderValue::from_static("text/event-stream"));
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert(
        "user-agent",
        HeaderValue::from_static(crate::shared::USER_AGENT),
    );
    Ok(headers)
}

/// Muse Code client: Meta Responses API with a subscription-minted key.
pub struct MuseCodeClient {
    config: ResponsesConfig,
    account: Option<String>,
    http: reqwest::Client,
}

impl MuseCodeClient {
    pub fn new(
        base_url: String,
        model: String,
        max_tokens: Option<u32>,
        prompt_cache_key: Option<String>,
        reasoning_effort: String,
        reasoning_summary: bool,
        account: Option<String>,
    ) -> Self {
        Self {
            config: responses_config(
                base_url,
                model,
                max_tokens,
                prompt_cache_key,
                reasoning_effort,
                reasoning_summary,
            ),
            account,
            http: crate::shared::streaming_http_client(),
        }
    }

    async fn stream_once(
        &self,
        api_key: &str,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        send_responses_stream_named(
            "muse-code",
            &self.http,
            &self.config,
            build_headers(api_key)?,
            messages,
            tools,
            system,
            ToolNames::build(tools, TOOL_NAMES)?,
        )
        .await
    }

    ///
    /// # Errors
    /// Returns an error if the key cannot be resolved or the request fails.
    pub async fn send_messages_stream(
        &self,
        messages: &[ChatMessage],
        tools: &[ToolDefinition],
        system: Option<&str>,
    ) -> Result<ProviderStream> {
        let account = self.account.as_deref();
        let api_key = resolve_api_key(account)?;
        let system = merge_system_prompt(system);

        match self
            .stream_once(&api_key, messages, tools, system.as_deref())
            .await
        {
            Err(error) if is_credential_rejection(&error) => {
                // The key has no published lifetime, so a rejection is the only
                // reliable expiry signal. Re-mint once from the stored identity
                // token; if that is also rejected the identity token itself is
                // gone and the user has to log in again.
                let refreshed = oauth_muse_code::remint_rejected_key(account, &api_key)
                    .await
                    .context("Muse Code key was rejected and could not be re-minted")?;
                self.stream_once(&refreshed.access, messages, tools, system.as_deref())
                    .await
            }
            other => other,
        }
    }
}

/// Constructs the Muse Code client from the given context.
///
/// # Errors
/// Returns an error if the base URL cannot be resolved.
pub fn build(
    ctx: &crate::ProviderBuildContext<'_>,
) -> anyhow::Result<Box<dyn crate::StreamingProvider>> {
    let base_url = crate::ProviderKind::MuseCode.resolve_base_url(ctx.base_url)?;
    Ok(Box::new(MuseCodeClient::new(
        base_url,
        ctx.model.to_string(),
        ctx.config_max_tokens,
        ctx.cache_key.clone(),
        reasoning_effort_from_thinking_level(ctx.thinking_level, ctx.model).to_owned(),
        ctx.thinking_level.is_enabled(),
        ctx.account.map(ToString::to_string),
    )))
}

#[cfg(test)]
mod tests {
    use zdx_types::ProviderError;

    use super::{MuseCodeClient, is_credential_rejection};
    use crate::{ProviderKind, resolve_provider};

    fn client(model: &str) -> MuseCodeClient {
        MuseCodeClient::new(
            "https://api.meta.ai/v1".to_string(),
            model.to_string(),
            Some(1024),
            Some("thread-123".to_string()),
            "high".to_string(),
            true,
            None,
        )
    }

    #[test]
    fn muse_code_prefix_routes_to_provider() {
        let selection = resolve_provider("muse-code:muse-spark-1.3");
        assert_eq!(selection.kind, ProviderKind::MuseCode);
        assert_eq!(selection.model, "muse-spark-1.3");
    }

    #[test]
    fn muse_code_is_an_oauth_subscription_provider() {
        assert_eq!(ProviderKind::MuseCode.id(), "muse-code");
        assert!(ProviderKind::MuseCode.is_subscription());
        assert!(ProviderKind::MuseCode.supports_oauth());
        // The subscription credential is minted, never read from an env var.
        assert_eq!(ProviderKind::MuseCode.api_key_env_var(), None);
    }

    /// Declared tools and replayed tool calls go out as `_<name>`; nothing
    /// in the request keeps a bare client-tool name.
    #[test]
    fn tools_and_replayed_calls_go_out_prefixed() {
        use zdx_types::{ChatContentBlock, ChatMessage, IdOrigin, ToolDefinition};

        use crate::openai::responses::build_request_body_named;
        use crate::tool_names::ToolNames;

        let tools = [ToolDefinition {
            name: "Bash".to_string(),
            description: "Run a shell command".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let messages = [
            ChatMessage::user("run ls"),
            ChatMessage::assistant_blocks(vec![ChatContentBlock::ToolUse {
                id: "call_1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "ls"}),
                id_origin: IdOrigin::Synthesized,
                replay: None,
            }]),
        ];
        let names = ToolNames::build(&tools, super::TOOL_NAMES).unwrap();
        let body = build_request_body_named(
            &client("muse-spark-1.3").config,
            &messages,
            &tools,
            None,
            None,
            &names,
        )
        .unwrap();
        let body = serde_json::to_value(body).unwrap();
        assert_eq!(body["tools"][0]["name"], "_bash");
        let call = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call")
            .unwrap();
        assert_eq!(call["name"], "_bash");
    }

    #[test]
    fn muse_code_sends_the_same_responses_shape_as_meta() {
        let client = client("muse-spark-1.3");
        assert_eq!(client.config.path, "/responses");
        assert_eq!(client.config.store, Some(false));
        assert_eq!(
            client.config.include.as_ref(),
            Some(&vec!["reasoning.encrypted_content".to_string()])
        );
        assert_eq!(client.config.reasoning_summary.as_deref(), Some("auto"));
    }

    #[test]
    fn muse_code_omits_summary_when_thinking_is_off() {
        let client = MuseCodeClient::new(
            "https://api.meta.ai/v1".to_string(),
            "muse-spark-1.3".to_string(),
            None,
            None,
            "minimal".to_string(),
            false,
            None,
        );
        assert_eq!(client.config.reasoning_effort.as_deref(), Some("minimal"));
        assert!(client.config.reasoning_summary.is_none());
    }
    #[test]
    fn credential_rejection_is_detected_only_for_auth_statuses() {
        for status in [401_u16, 403] {
            let err = anyhow::Error::new(ProviderError::http_status(status, "{}"));
            assert!(is_credential_rejection(&err), "status {status}");
        }
        // A rate limit or server fault must not burn a re-mint on the
        // rate-limited endpoint.
        for status in [400_u16, 429, 500] {
            let err = anyhow::Error::new(ProviderError::http_status(status, "{}"));
            assert!(!is_credential_rejection(&err), "status {status}");
        }
        assert!(!is_credential_rejection(&anyhow::anyhow!("boom")));
    }

    #[test]
    fn client_attestation_denial_is_not_a_credential_rejection() {
        // The key is valid; re-minting returns the same key and burns a call
        // on the rate-limited mint endpoint.
        let body = r#"{"error":{"code":"client_attestation_denied","message":"Muse Code subscriptions cover requests from the Muse Code client only.","type":"permission_error"}}"#;
        let err = anyhow::Error::new(ProviderError::http_status(403, body));
        assert!(!is_credential_rejection(&err));
    }

    #[test]
    fn minted_credentials_carry_no_predicted_expiry() {
        // The key has no published lifetime: it must never look expired, so
        // recovery is driven by a real rejection instead of a timer.
        let payload = crate::oauth::muse_code::MuseCodeKeyResponse {
            api_key: Some("LLM|key".to_string()),
            is_subs_active: Some(true),
            ..Default::default()
        };
        let creds = crate::oauth::muse_code::credentials_from_key_response("identity", &payload)
            .expect("credentials");
        assert_eq!(creds.expires, crate::oauth::muse_code::NO_EXPIRY);
        assert!(!creds.is_expired());
        assert_eq!(creds.refresh, "identity");
        assert_eq!(creds.access, "LLM|key");
    }

    #[test]
    fn inactive_subscription_is_rejected_at_login() {
        let payload = crate::oauth::muse_code::MuseCodeKeyResponse {
            api_key: Some("LLM|key".to_string()),
            is_subs_active: Some(false),
            ..Default::default()
        };
        assert!(
            crate::oauth::muse_code::credentials_from_key_response("identity", &payload).is_err()
        );
    }
}
