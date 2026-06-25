# zdx-providers

LLM provider implementations extracted from `zdx-core`.

## Layout

- `src/lib.rs` — crate root: module declarations, `ProviderKind`, `ProviderSelection`, `resolve_provider()`, `ProviderBuildContext`
- `src/shared.rs` — provider-agnostic helpers (`resolve_api_key`, `resolve_base_url`, `merge_system_prompt`, `USER_AGENT`); re-exports value types from `zdx-types`
- `src/oauth.rs` — OAuth token storage/retrieval (Claude CLI, OpenAI Codex, Gemini CLI)
- `src/anthropic/` — Anthropic Messages API + Claude CLI OAuth provider
- `src/openai/` — OpenAI Responses/Chat Completions/image generation API + Codex OAuth provider
- `src/gemini/` — Google Gemini API + Gemini CLI OAuth + Antigravity OAuth providers
- `src/openrouter.rs`, `src/deepseek.rs`, `src/mistral.rs`, `src/moonshot.rs`, `src/stepfun.rs`, `src/xiaomi.rs`, `src/minimax.rs`, `src/zai.rs`, `src/xai.rs` — thin OpenAI-compatible providers
- `src/opencode_go.rs` — meta-provider that routes to inner clients based on model registry hints
- `src/debug_metrics.rs`, `src/debug_trace.rs` — debug/tracing wrappers for provider streams
- `src/text_tool_parser.rs`, `src/thinking_parser.rs` — SSE stream content parsers

## Conventions

- Pure value types (DTOs, enums) belong in `zdx-types`, not here.
- This crate must NOT depend on `zdx-engine` (no circular deps).
- `zdx-engine` re-exports everything via a thin `providers.rs` facade.
- Provider routing hints (e.g. for the opencode-go meta-provider) are passed as `api_hint: Option<String>` parameters — model registry lookups happen in the caller (`zdx-engine`).
