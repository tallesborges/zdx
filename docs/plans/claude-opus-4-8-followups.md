# Claude Opus 4.8 follow-ups

## Mid-conversation system messages

- Support Anthropic `role: "system"` messages inside `messages` immediately after a user turn.
- Decide how ZDX should model late instructions without disturbing the top-level system prompt and cache breakpoints.
- Add request-shape tests for valid placement and persisted-thread replay.

## Refusal `stop_details`

- Extend Anthropic SSE parsing to preserve documented refusal `stop_details` when `stop_reason = "refusal"`.
- Surface refusal category in notices and thread logs.
- Add fixtures for refusal categories once the docs/API examples are stable.

## Anthropic fast mode

- Add `speed: "fast"` plus beta header `fast-mode-2026-02-01` for supported Claude API Opus models.
- Extend fast-mode provider toggles beyond OpenAI without changing unsupported providers.
- Keep it disabled by default because access is research-preview/waitlist-gated and premium-priced.