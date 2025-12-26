Goals
- Add Anthropic OAuth login to zdx with local token cache in `~/.zdx/oauth.json` and provider wiring.
- Ship a minimal CLI login flow first, then TUI `/login`, keeping core UI-agnostic and reducer-driven.
- Update contracts in `docs/SPEC.md` to allow OAuth token caching while keeping API keys env-only.

Non-goals
- No `ANTHROPIC_OAUTH_TOKEN` env var support.
- No multi-provider OAuth.
- No web UI or browser-embedded auth.
- No redesign of transcript rendering or tool system beyond login needs.

User journey
- start → input → submit → see output → stream → scroll → tools → selection/copy → polish.

MVP Slices
0) Terminal safety + restore (only if raw/alt-screen is used)
- [x] Verify whether TUI uses raw mode/alt-screen; if yes, add a guard to always restore on panic/interrupt.
- [x] Add a smoke path to enter/exit TUI safely.
- ✅ Demo: start TUI, trigger Ctrl+C/panic, terminal restores cleanly.
- Failure modes: terminal stuck in raw/alt-screen; cleanup not run on panic.
- **DONE**: TUI already has panic hook + Drop impl that restores terminal.

1) OAuth core + CLI login/logout
- [x] Add `zdx login --anthropic` and `zdx logout --anthropic`.
- [x] Implement OAuth core: open auth URL, accept pasted token/code; store token in `~/.zdx/oauth.json` (0600 perms).
- [x] Document/decide whether we accept pasted OAuth token only or exchange code; keep it minimal and explicit.
- ✅ Demo: login writes token cache; logout clears it.
- Failure modes: invalid token/code, file permission errors, malformed JSON, network errors.
- **DONE**: `src/core/oauth.rs` + login/logout commands in main.rs. Tests in `tests/login_logout.rs`.

2) Provider wiring + first prompt
- [x] Load OAuth token from `~/.zdx/oauth.json` and prefer it over `ANTHROPIC_API_KEY`.
- [x] Detect OAuth token type and set Anthropic OAuth headers accordingly (`Authorization: Bearer` for OAuth vs `x-api-key` for API key).
- [x] Include required `anthropic-beta: oauth-2025-04-20` header for OAuth requests.
- [x] Auto-refresh expired OAuth tokens; fallback to API key if refresh fails.
- [x] Run a prompt and verify streaming output works with OAuth token.
- ✅ Demo: login → prompt → streaming response.
- Failure modes: header rejection, token prefix mismatch, streaming regression.
- **DONE**: `AnthropicConfig::resolve_auth()` checks OAuth first, falls back to API key. `AuthType` enum controls header selection. OAuth requires beta header.

3) TUI `/login` flow (reducer pattern)
- [x] Add events: `LoginRequested`, `AuthCodeEntered`, `LoginSucceeded`, `LoginFailed`, `LoginCancelled`.
- [x] Minimal overlay or prompt input for token/code; avoid complex UI.
- [x] Ensure `update(state, event)` is the only state mutator; render reads state only.
- ✅ Demo: `/login` in TUI, paste token/code, see success, continue chat.
- Failure modes: keybinding conflicts, stuck overlay, focus/scroll regression.
- **DONE**: Added `/login` slash command, login overlay with reducer pattern, async token exchange.

Contracts (guardrails)
- UI-agnostic core: OAuth logic + token storage live in core/config, not TUI.
- Reducer pattern for UI: update(state, event) mutates state; render reads state only.
- Tokens stored with 0600 perms; never logged; API keys remain env-only.
- OAuth token precedence: `~/.zdx/oauth.json` > `ANTHROPIC_API_KEY`.

Key decisions
- Auth flow minimalism: accept pasted OAuth token (or code exchange only if required/confirmed).
- Token cache format/path: `~/.zdx/oauth.json` with provider map.
- Keybinding/focus for `/login` overlay avoids existing editor shortcuts.
- Backpressure: login flow must not block streaming loop or event dispatch.

Testing
- Integration test: `zdx login/logout` writes/removes token cache.
- Manual: run a streaming prompt using OAuth token.
- Avoid process-global env mutation; set env on spawned commands if needed.

Polish phases
- Add `zdx auth status` and user-friendly error messages.
- Improve prompts and success copy (include token cache path, never token).

Later / Deferred
- Multi-provider OAuth.
- Web UI login.
- Token refresh/expiry handling beyond re-login.

Gemini validation
- PASS
