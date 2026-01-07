# OpenAI Codex OAuth + Responses Provider (Ship-First Plan)

Reference implementation: https://github.com/badlogic/pi-mono/pull/451

# Goals
- Add an OpenAI Codex provider that uses the Responses API
- Enable OAuth authentication for the new provider
- Support streaming responses with tool loop execution

# Non-goals
- Additional providers beyond OpenAI Codex
- UI/UX changes not required for OAuth or streaming
- Non-essential provider features outside Responses + streaming + tool loop

# Design principles
- User journey drives order
- Ship-first
- Demoable slices

# User journey
1) User logs in with OpenAI Codex OAuth
2) User runs a prompt against the new provider
3) User sees streaming output
4) User sees tool calls execute and the response completes

# Foundations / Already shipped (✅)
- What exists: OAuth credential cache and provider-specific OAuth helpers
  - ✅ Demo: Log in/out for the existing OAuth provider and verify credentials are stored/cleared
  - Gaps: No OpenAI Codex OAuth flow
- What exists: Streaming provider + tool loop wiring for a shipped provider
  - ✅ Demo: Run a prompt and observe streaming output and tool execution
  - Gaps: No OpenAI Codex provider implementation

# MVP slices (ship-shaped, demoable)

## Slice 1: OpenAI Codex OAuth flow (✅)
- Goal: User can authenticate and store OpenAI Codex OAuth credentials
- Scope checklist:
  - [x] Add OpenAI Codex OAuth endpoints, client ID, scopes, and PKCE flow
  - [x] Add CLI login/logout wiring for OpenAI Codex
  - [x] Store credentials in the existing OAuth cache
- ✅ Demo: Run login, confirm credentials saved, run logout, confirm credentials removed
- Risks / failure modes:
  - OAuth code parsing fails
  - Token exchange or refresh fails

## Slice 2: Minimal Responses streaming (✅)
- Goal: User can send a prompt and receive streaming text output
- Scope checklist:
  - [x] Build Responses API request payloads
  - [x] Implement streaming response parsing for text deltas
  - [x] Wire provider selection to the new implementation
- ✅ Demo: Run a prompt and observe streamed output to completion
- Risks / failure modes:
  - Stream parsing breaks on partial events
  - Errors are not surfaced clearly

## Slice 3: Tool loop integration (✅)
- Goal: Tool calls during streaming are executed and responses continue
- Scope checklist:
  - [x] Parse tool-call events from the stream
  - [x] Execute tools and send tool results back to the provider
  - [x] Resume streaming until completion
- ✅ Demo: Run a prompt that triggers a tool and see tool execution + continued output
- Risks / failure modes:
  - Tool-call schema mismatches
  - Tool result formatting rejected by the API

## Slice 4: OAuth refresh handling (✅)
- Goal: Daily-usable auth without frequent re-login
- Scope checklist:
  - [x] Refresh access tokens when expired
  - [x] Define fallback behavior when refresh fails
- ✅ Demo: Simulate an expired token and verify refresh path succeeds or fails cleanly
- Risks / failure modes:
  - Refresh invalidates stored credentials
  - Incorrect auth headers lead to silent failures

# Contracts (guardrails)
- OAuth credentials remain cached in the existing location and format
- Interactive mode remains full-screen and does not emit transcript to stdout
- Non-interactive mode keeps assistant output on stdout and diagnostics on stderr

# Key decisions (decide early)
- OAuth login flow: local callback vs manual paste
- Streaming parsing strategy for Responses events
- Tool-call mapping between provider and tool loop

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)

## Phase 1: Reliability hardening (pending)
- ✅ Check-in demo: Long stream completes without errors
- Scope: Retry/timeout handling and clearer auth errors

## Phase 2: Auth UX cleanup (pending)
- ✅ Check-in demo: Login/logout flows are smooth and understandable
- Scope: Prompt clarity and error messaging

# Later / Deferred
- Additional providers beyond OpenAI Codex
- Advanced provider features not required for Responses + streaming + tool loop
- Revisit if usage demands more provider capabilities
