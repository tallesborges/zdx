//! Context analysis handler.
//!
//! Builds a per-section breakdown of what the next LLM turn would
//! send (system prompt + tools + messages + per-file AGENTS.md) and emits
//! `UiEvent::ContextResult` carrying a structured [`ContextReport`].
//!
//! Two count layers:
//! - **Characters** — always computed locally, instant, 100% accurate as
//!   raw text length. Default view on overlay open.
//! - **Tokens** — optional, fetched per-section via Anthropic's
//!   `/v1/messages/count_tokens`. Requires an Anthropic API key. Opted
//!   into by the user via `[r] refine` in the overlay.
//!
//! Both counts are bundled into a single [`ContextReport`] so the overlay
//! can toggle between [`DisplayMode::Chars`] and [`DisplayMode::Tokens`]
//! locally — no re-analysis needed once tokens have been fetched.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Result;
use zdx_engine::config::Config;
use zdx_engine::core::agent::{AgentOptions, resolve_active_tools};
use zdx_engine::core::context::build_effective_system_prompt_with_paths_and_instruction_layers;
use zdx_engine::models::ModelOption;
use zdx_engine::providers::anthropic::{AnthropicClient, AnthropicConfig};
use zdx_engine::providers::{ChatMessage, ProviderKind};
use zdx_types::ToolDefinition;

use crate::events::UiEvent;
use crate::tui_instruction_layers;

/// Placeholder user message used as the protocol baseline so we can subtract
/// out Anthropic's automatic role/system framing tokens.
const BASELINE_MESSAGE: &str = ".";

/// Which counting strategy to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnalysisMode {
    /// Char counts only. Always available, instant, no network. Default
    /// path when the overlay is first opened.
    Heuristic,
    /// Char counts AND per-section exact tokens via Anthropic's
    /// `/v1/messages/count_tokens`. Requires an Anthropic API key and a
    /// Claude-family model.
    Exact,
}

/// How the overlay should render an existing report. Toggled locally by
/// the user without re-running the handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayMode {
    Chars,
    Tokens,
}

/// Returns true when the active model could be refined via Anthropic's
/// `count_tokens` endpoint: it's a Claude-family model AND a key is
/// resolvable from config or `ANTHROPIC_API_KEY`.
#[must_use]
pub fn refine_supported(model_id: &str, config: &Config) -> bool {
    let kind = zdx_engine::providers::resolve_provider(model_id).kind;
    let is_claude_family = matches!(kind, ProviderKind::Anthropic | ProviderKind::ClaudeCli);
    if !is_claude_family {
        return false;
    }
    // Mirrors `ProviderKind::resolve_api_key` resolution: config first,
    // then `ANTHROPIC_API_KEY` env var.
    config.providers.anthropic.effective_api_key().is_some()
        || std::env::var("ANTHROPIC_API_KEY")
            .ok()
            .as_ref()
            .is_some_and(|v| !v.trim().is_empty())
}

/// Top-level handler called by the runtime.
pub async fn analyze_context(
    model_id: String,
    config: Config,
    agent_opts: AgentOptions,
    messages: Vec<ChatMessage>,
    mode: AnalysisMode,
) -> UiEvent {
    let result = run(model_id, config, agent_opts, messages, mode)
        .await
        .map_err(|e| e.to_string());
    UiEvent::ContextResult { result }
}

async fn run(
    model_id: String,
    config: Config,
    agent_opts: AgentOptions,
    messages: Vec<ChatMessage>,
    mode: AnalysisMode,
) -> Result<ContextReport> {
    // Resolve the provider kind and bare model name (with any
    // `provider:` prefix stripped). The Anthropic `count_tokens` endpoint
    // accepts the bare model — so a `claude-cli:…` thread (OAuth-routed
    // for chat) can still get exact token counts via an Anthropic API key.
    let selection = zdx_engine::providers::resolve_provider(&model_id);
    let provider_kind = selection.kind;
    let bare_model = selection.model.clone();
    let is_claude_family = matches!(
        provider_kind,
        ProviderKind::Anthropic | ProviderKind::ClaudeCli
    );

    let (display_model, context_limit) = match ModelOption::find_by_id(&model_id) {
        Some(m) => (m.display_name.to_string(), m.context_limit),
        None => (model_id.clone(), 0),
    };

    // Rebuild the system prompt fresh so the count matches what would
    // actually be sent on the next turn (not a possibly-stale cache).
    let instruction_layers = tui_instruction_layers();
    let effective = build_effective_system_prompt_with_paths_and_instruction_layers(
        &config,
        &agent_opts.root,
        &instruction_layers,
        true,
    )?;
    let system_prompt = effective.prompt.unwrap_or_default();
    let agents_paths = effective.loaded_agents_paths.clone();

    let agents_files: Vec<(PathBuf, String)> = agents_paths
        .into_iter()
        .map(|p| {
            let content = std::fs::read_to_string(&p).unwrap_or_default();
            (p, content)
        })
        .collect();

    let tools = resolve_active_tools(&config, &agent_opts, provider_kind);

    // Always compute char counts (instant, 100% accurate as raw chars).
    let chars = compute_char_counts(&system_prompt, &tools, &messages, &agents_files);

    // Optionally fetch exact token counts via Anthropic.
    let tokens = match mode {
        AnalysisMode::Exact if is_claude_family => {
            match build_anthropic_client(&config, &bare_model) {
                Ok(client) => Some(
                    fetch_token_counts(
                        &client,
                        &system_prompt,
                        &tools,
                        &messages,
                        &agents_files,
                        &chars,
                    )
                    .await?,
                ),
                // No API key configured — silently fall back to chars-only.
                // The overlay's `[r] refine` hint is gated on
                // `refine_supported`, so this branch is normally
                // unreachable. Returning `None` keeps the report usable.
                Err(_) => None,
            }
        }
        _ => None,
    };

    Ok(ContextReport {
        display_model,
        context_limit,
        messages_count: messages.len(),
        system_prompt,
        tools,
        chars,
        tokens,
    })
}

fn build_anthropic_client(config: &Config, model: &str) -> Result<AnthropicClient> {
    let anthropic_cfg = AnthropicConfig::from_env(
        model.to_string(),
        // `max_tokens` is required by AnthropicConfig but unused by count_tokens.
        4096,
        config.providers.anthropic.effective_base_url(),
        config.providers.anthropic.effective_api_key(),
        false,
        0,
        None,
    )?;
    Ok(AnthropicClient::new(anthropic_cfg))
}

// ---------------------------------------------------------------------------
// Chars path: instant, local, no network.
// ---------------------------------------------------------------------------

fn compute_char_counts(
    system_prompt: &str,
    tools: &[ToolDefinition],
    messages: &[ChatMessage],
    agents_files: &[(PathBuf, String)],
) -> CharCounts {
    let system = system_prompt.len() as u64;
    let tools_total: u64 = tools
        .iter()
        .map(|t| {
            t.name.len() as u64
                + t.description.len() as u64
                + serde_json::to_string(&t.input_schema).map_or(0, |s| s.len() as u64)
        })
        .sum();
    let messages_total: u64 = messages.iter().map(chat_message_chars).sum();
    let used = system + tools_total + messages_total;

    let mut agents: Vec<(PathBuf, u64)> = agents_files
        .iter()
        .map(|(p, c)| (p.clone(), c.len() as u64))
        .collect();
    agents.sort_by_key(|b| std::cmp::Reverse(b.1));

    CharCounts {
        system,
        tools: tools_total,
        messages: messages_total,
        used,
        agents,
    }
}

fn chat_message_chars(msg: &ChatMessage) -> u64 {
    use zdx_engine::providers::{ChatContentBlock, MessageContent};
    let role = msg.role.len() as u64;
    let body = match &msg.content {
        MessageContent::Text(t) => t.len() as u64,
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .map(|b| match b {
                ChatContentBlock::Text { text, .. } => text.len() as u64,
                ChatContentBlock::Reasoning(r) => r.text.as_deref().map_or(0, |s| s.len() as u64),
                ChatContentBlock::ToolUse { name, input, .. } => {
                    name.len() as u64 + serde_json::to_string(input).map_or(0, |s| s.len() as u64)
                }
                ChatContentBlock::ToolResult(result) => {
                    use zdx_types::{ToolResultBlock, ToolResultContent};
                    match &result.content {
                        ToolResultContent::Text(t) => t.len() as u64,
                        ToolResultContent::Blocks(blocks) => blocks
                            .iter()
                            .map(|b| match b {
                                ToolResultBlock::Text { text } => text.len() as u64,
                                // Images aren't text; report 0 chars. The
                                // Tokens view (via Anthropic) reflects
                                // their real cost.
                                ToolResultBlock::Image { .. } => 0,
                            })
                            .sum(),
                    }
                }
                ChatContentBlock::Image { .. } => 0,
            })
            .sum(),
    };
    role + body
}

// ---------------------------------------------------------------------------
// Tokens path: exact counts via /v1/messages/count_tokens
// ---------------------------------------------------------------------------

async fn fetch_token_counts(
    client: &AnthropicClient,
    system_prompt: &str,
    tools: &[ToolDefinition],
    messages: &[ChatMessage],
    agents_files: &[(PathBuf, String)],
    chars: &CharCounts,
) -> Result<TokenCounts> {
    let baseline_msgs = vec![ChatMessage::user(BASELINE_MESSAGE)];
    // count_tokens requires messages to be non-empty; substitute baseline
    // when the thread has no real conversation yet.
    let real_msgs_or_baseline: Vec<ChatMessage> = if messages.is_empty() {
        baseline_msgs.clone()
    } else {
        messages.to_vec()
    };

    // Fire all calls in parallel via tokio::try_join!.
    //
    // Baseline (Anthropic protocol/role framing):
    //   T_base   = count(messages=[placeholder])
    // Sections (each adds one thing to baseline):
    //   T_sys    = count(system=full,   messages=[placeholder])
    //   T_tools  = count(tools=full,    messages=[placeholder])
    //   T_msgs   = count(messages=real_or_baseline)
    // Ground-truth total:
    //   T_full   = count(system + tools + real_messages)
    let baseline_call = client.count_tokens(&baseline_msgs, &[], None);
    let system_call = client.count_tokens(&baseline_msgs, &[], Some(system_prompt));
    let tools_call = client.count_tokens(&baseline_msgs, tools, None);
    let messages_call = client.count_tokens(&real_msgs_or_baseline, &[], None);
    let total_call = client.count_tokens(&real_msgs_or_baseline, tools, Some(system_prompt));

    // Order per-file requests using the chars vector's path order so the
    // resulting token rows align with the chars rows for the overlay.
    let per_file: Vec<(PathBuf, String)> = chars
        .agents
        .iter()
        .map(|(path, _)| {
            let content = agents_files
                .iter()
                .find(|(p, _)| p == path)
                .map(|(_, c)| c.clone())
                .unwrap_or_default();
            (path.clone(), content)
        })
        .collect();

    let per_file_futures: Vec<_> = per_file
        .iter()
        .map(|(_, content)| {
            let msgs = vec![ChatMessage::user(if content.is_empty() {
                BASELINE_MESSAGE
            } else {
                content
            })];
            async move { client.count_tokens(&msgs, &[], None).await }
        })
        .collect();

    let (t_base, t_sys, t_tools, t_msgs, t_full) = tokio::try_join!(
        baseline_call,
        system_call,
        tools_call,
        messages_call,
        total_call
    )?;

    let per_file_results = futures_util::future::try_join_all(per_file_futures).await?;

    // Subtract baseline framing overhead from each per-section call.
    let system = t_sys.saturating_sub(t_base);
    let tools_total = t_tools.saturating_sub(t_base);
    let messages_total = if messages.is_empty() {
        0
    } else {
        t_msgs.saturating_sub(t_base)
    };

    let agents: Vec<(PathBuf, u64)> = per_file
        .into_iter()
        .zip(per_file_results)
        .map(|((path, content), raw)| {
            let toks = if content.is_empty() {
                0
            } else {
                raw.saturating_sub(t_base)
            };
            (path, toks)
        })
        .collect();

    Ok(TokenCounts {
        system,
        tools: tools_total,
        messages: messages_total,
        used: t_full,
        agents,
    })
}

// ---------------------------------------------------------------------------
// Report data + Markdown rendering
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct CharCounts {
    pub system: u64,
    pub tools: u64,
    pub messages: u64,
    pub used: u64,
    /// Per-AGENTS.md char counts, sorted heaviest first.
    pub agents: Vec<(PathBuf, u64)>,
}

#[derive(Debug, Clone)]
pub struct TokenCounts {
    pub system: u64,
    pub tools: u64,
    pub messages: u64,
    pub used: u64,
    /// Per-AGENTS.md token counts, in the same order as
    /// [`CharCounts::agents`].
    pub agents: Vec<(PathBuf, u64)>,
}

#[derive(Debug, Clone)]
pub struct ContextReport {
    pub display_model: String,
    pub context_limit: u64,
    pub messages_count: usize,
    pub system_prompt: String,
    pub tools: Vec<ToolDefinition>,
    pub chars: CharCounts,
    /// Present once the user has refined via Anthropic `count_tokens`.
    pub tokens: Option<TokenCounts>,
}

impl ContextReport {
    /// True once the exact-token path has been fetched.
    pub fn has_tokens(&self) -> bool {
        self.tokens.is_some()
    }

    /// chars/token density for the whole prompt. None when tokens haven't
    /// been fetched yet (or both totals are zero).
    pub fn ratio(&self) -> Option<f64> {
        let tokens = self.tokens.as_ref()?;
        if tokens.used == 0 {
            return None;
        }
        Some(self.chars.used as f64 / tokens.used as f64)
    }

    pub fn render_markdown(&self, display: DisplayMode) -> String {
        match display {
            DisplayMode::Tokens if self.tokens.is_some() => self.render_tokens_view(),
            // Asked for Tokens but tokens haven't been fetched — fall back
            // to Chars so we never render an empty/half-broken view.
            DisplayMode::Tokens | DisplayMode::Chars => self.render_chars_view(),
        }
    }

    fn render_chars_view(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Context Usage");
        let _ = writeln!(out);
        if self.context_limit > 0 {
            let _ = writeln!(
                out,
                "**Model:** {} · {} tokens context",
                self.display_model,
                format_count(self.context_limit)
            );
        } else {
            let _ = writeln!(out, "**Model:** {}", self.display_model);
        }
        let _ = writeln!(out);

        // Headline: chars only — no context-window % (units mismatch) and
        // no Free row.
        let _ = writeln!(out, "**Chars used:** {}", format_count(self.chars.used));
        let _ = writeln!(out);

        // Density line, shown only after tokens have been refined.
        if let Some(ratio) = self.ratio() {
            let _ = writeln!(
                out,
                "**Density:** {ratio:.2} chars/token (standard heuristic ≈ 4.0)"
            );
            let _ = writeln!(out);
        }

        // Section breakdown
        let _ = writeln!(out, "## Breakdown");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- **System prompt** — {}",
            format_count(self.chars.system),
        );
        if !self.chars.agents.is_empty() {
            let agents_total: u64 = self.chars.agents.iter().map(|(_, n)| *n).sum();
            let _ = writeln!(out, "    - **AGENTS.md** — {}", format_count(agents_total));
            for (path, n) in &self.chars.agents {
                let _ = writeln!(
                    out,
                    "        - `{}` — {}",
                    short_path(path),
                    format_count(*n),
                );
            }
        }
        let _ = writeln!(
            out,
            "- **Built-in tools** — {}",
            format_count(self.chars.tools),
        );
        let _ = writeln!(
            out,
            "- **Messages** — {} · {} turn{}",
            format_count(self.chars.messages),
            self.messages_count,
            if self.messages_count == 1 { "" } else { "s" },
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "_Raw character counts. Press `r` to refine to exact tokens._"
        );
        out
    }

    fn render_tokens_view(&self) -> String {
        let Some(tokens) = self.tokens.as_ref() else {
            // Caller guards against this in `render_markdown`.
            return self.render_chars_view();
        };
        let mut out = String::new();
        let pct = |n: u64| -> String {
            if self.context_limit == 0 {
                "—".to_string()
            } else {
                format!("{:.1}%", (n as f64 / self.context_limit as f64) * 100.0)
            }
        };

        let _ = writeln!(out, "# Context Usage");
        let _ = writeln!(out);
        if self.context_limit > 0 {
            let _ = writeln!(
                out,
                "**Model:** {} · {} tokens context",
                self.display_model,
                format_count(self.context_limit)
            );
        } else {
            let _ = writeln!(out, "**Model:** {}", self.display_model);
        }
        let _ = writeln!(out);

        if self.context_limit > 0 {
            let free = self.context_limit.saturating_sub(tokens.used);
            let _ = writeln!(
                out,
                "**Used:** {} ({})  ·  **Free:** {}",
                format_count(tokens.used),
                pct(tokens.used),
                format_count(free),
            );
        } else {
            let _ = writeln!(out, "**Used:** {}", format_count(tokens.used));
        }
        let _ = writeln!(out);

        if let Some(ratio) = self.ratio() {
            let _ = writeln!(
                out,
                "**Density:** {ratio:.2} chars/token (standard heuristic ≈ 4.0)"
            );
            let _ = writeln!(out);
        }

        // Section breakdown
        let _ = writeln!(out, "## Breakdown");
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "- **System prompt** — {} ({})",
            format_count(tokens.system),
            pct(tokens.system),
        );
        if !tokens.agents.is_empty() {
            let agents_total: u64 = tokens.agents.iter().map(|(_, t)| *t).sum();
            let _ = writeln!(
                out,
                "    - **AGENTS.md** — {} ({})",
                format_count(agents_total),
                pct(agents_total),
            );
            for (path, toks) in &tokens.agents {
                let _ = writeln!(
                    out,
                    "        - `{}` — {}",
                    short_path(path),
                    format_count(*toks),
                );
            }
        }
        let _ = writeln!(
            out,
            "- **Built-in tools** — {} ({})",
            format_count(tokens.tools),
            pct(tokens.tools),
        );
        let _ = writeln!(
            out,
            "- **Messages** — {} ({}) · {} turn{}",
            format_count(tokens.messages),
            pct(tokens.messages),
            self.messages_count,
            if self.messages_count == 1 { "" } else { "s" },
        );
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "_Counts via Anthropic `/v1/messages/count_tokens`. \
             Per-file numbers measure raw content; tiny framing overhead is absorbed into the parent row._"
        );
        out
    }

    pub fn render_system_prompt_markdown(&self) -> String {
        if self.system_prompt.trim().is_empty() {
            "_(empty)_".to_string()
        } else {
            self.system_prompt.trim_end().to_string()
        }
    }

    pub fn render_tools_markdown(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "```json");
        if let Ok(tools) = serde_json::to_string_pretty(&self.tools) {
            let _ = writeln!(out, "{tools}");
        }
        let _ = writeln!(out, "```");
        out
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn short_path(p: &Path) -> String {
    // Show the path relative to the user's home dir when applicable;
    // otherwise show the absolute path. Both are clickable in the TUI.
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if let Ok(stripped) = p.strip_prefix(&home) {
            return format!("~/{}", stripped.display());
        }
    }
    p.display().to_string()
}
