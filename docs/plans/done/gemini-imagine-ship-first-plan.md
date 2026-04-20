# Goals
- Add support for `gemini-3.1-flash-image-preview` in zdx for image generation.
- Ship a daily-usable flow where a user can run `zdx imagine` and get image files locally.
- Choose and commit to one primary path (CLI command vs skill) early to avoid split effort.

# Non-goals
- Reworking the existing chat/TUI pipeline for image rendering.
- Broad multimodal feature expansion beyond image generation from prompt.
- Building both a full skill path and a full CLI path in parallel.

# Design principles
- User journey drives order
- Ship-first: one complete vertical path before options/polish
- Command-first for repeatable daily use and clear UX (`zdx imagine`)

# User journey
1. User has `GEMINI_API_KEY` configured.
2. User runs `zdx imagine` with a text prompt.
3. zdx calls `gemini-3.1-flash-image-preview` and receives image output.
4. zdx saves image file(s) and prints where they were written.

# MVP command contract
- `-p, --prompt <TEXT>` (required)
- `-o, --out <PATH>` (optional)
- `--model <MODEL_ID>` (optional; default `gemini:gemini-3.1-flash-image-preview`)
- `--aspect <RATIO>` (optional; e.g. `1:1`, `16:9`, `9:16`)
- `--size <SIZE>` (optional; e.g. `1K`, `2K`, `4K`)
- No `--text` flag in MVP. Response modality stays image-focused.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Gemini provider/auth foundation
- What exists: API-key based Gemini integration, request/response plumbing, and HTTP client patterns are already in place.
- ✅ Demo: Existing Gemini text models run successfully through current commands.
- Gaps: No dedicated image-generation command flow yet.

## CLI command routing foundation
- What exists: Existing subcommand architecture and dispatch flow already supports adding new commands cleanly.
- ✅ Demo: Existing commands (`exec`, `threads`, `models`, etc.) route and run through the same command framework.
- Gaps: No `imagine` command currently.

## Model/config foundation
- What exists: Model registry and provider configuration systems are already established.
- ✅ Demo: Models can be selected and resolved via current config/registry flow.
- Gaps: `gemini-3.1-flash-image-preview` is not yet represented as a first-class shipped model entry.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: End-to-end `zdx imagine` MVP (single prompt → image file)
- **Goal**: Deliver first usable image-generation workflow with the new model.
- **Scope checklist**:
  - [ ] Add `imagine` CLI subcommand with `--prompt`, `--out`, `--model`, `--aspect`, and `--size` flags.
  - [ ] Call Gemini `generateContent` using `gemini-3.1-flash-image-preview`.
  - [ ] Send image-focused response modality (no text modality in MVP).
  - [ ] Map `--aspect` and `--size` to Gemini image configuration when provided.
  - [ ] Parse image bytes from response and save to disk.
  - [ ] Print saved file path(s) and fail with actionable error message on API/file errors.
- **✅ Demo**: `zdx imagine -p "<prompt>" -o out.png` produces a valid image file at `out.png`.
- **Risks / failure modes**:
  - API returns text-only due to request config mismatch.
  - Response parsing misses image parts format.
  - File write errors (permissions/path).

## Slice 2: Output reliability + predictable UX
- **Goal**: Make the command reliable for daily repeated usage.
- **Scope checklist**:
  - [ ] Validate `--aspect` and `--size` values in CLI with clear errors.
  - [ ] Handle unexpected/mixed model parts safely while still extracting image output.
  - [ ] Support deterministic naming when output path is omitted.
  - [ ] Add clear exit behavior and stderr diagnostics aligned with existing CLI contracts.
  - [ ] Add guardrails for empty-image responses with explicit failure reason.
- **✅ Demo**: Running `zdx imagine -p "<prompt>"` repeatedly produces valid files with predictable names and clear CLI output.
- **Risks / failure modes**:
  - Inconsistent response shapes across model versions.
  - Ambiguity when multiple image parts are returned.

## Slice 3: Registry/docs contract completion
- **Goal**: Make the feature discoverable and consistent with product contracts.
- **Scope checklist**:
  - [ ] Add `gemini-3.1-flash-image-preview` to shipped model metadata defaults.
  - [ ] Update CLI/spec docs to include `zdx imagine`.
  - [ ] Document required auth/env expectations and basic usage.
- **✅ Demo**: New users can discover command via help/docs and run it without digging into source.
- **Risks / failure modes**:
  - Drift between code behavior and docs/contracts.
  - Pricing/model metadata staleness.

## Slice 4: Minimal regression coverage for command contract
- **Goal**: Protect the user-visible command contract without heavy test scope.
- **Scope checklist**:
  - [ ] Add focused CLI integration tests for argument parsing and failure modes.
  - [ ] Add targeted response parsing tests for image-part extraction.
  - [ ] Keep tests scoped to core command guarantees.
- **✅ Demo**: Contract tests pass and catch regressions in command invocation/output behavior.
- **Risks / failure modes**:
  - Over-testing internals slows iteration.
  - Under-testing parsing contracts allows silent breakage.

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- `zdx imagine` must generate image file output from prompt using Gemini image-preview model.
- `--model`, `--aspect`, and `--size` must be supported in MVP.
- MVP should not require `--text`; image generation works with image-focused output.
- Command output must remain script-friendly (paths/results on stdout, diagnostics on stderr).
- Existing commands (`zdx`, `zdx exec`, automations, threads) must not change behavior.
- Failures must be explicit and actionable (no silent success when no image was generated).

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Primary path: ship a first-class CLI command (`zdx imagine`) as MVP, not a skill-only implementation.
- Initial output contract: single-command image generation with local file save as core success path.
- Flag contract in MVP: support `--model`, `--aspect`, and `--size`; do not include `--text`.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts
- One real API smoke run for final MVP verification (behind env key)
- CLI argument/error-path integration checks for `zdx imagine`

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: CLI ergonomics polish
- Improve help text and examples for common `zdx imagine` usage.
- Improve output messaging for generated file locations.
- ✅ Check-in demo: First-time user can run command from `--help` guidance only.

## Phase 2: Operational polish
- Improve resilience around transient API/network errors.
- Refine deterministic file naming and overwrite behavior messaging.
- ✅ Check-in demo: Repeated runs under normal failure/retry scenarios remain predictable.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Skill-only implementation as primary path — revisit only if command maintenance cost becomes unjustified.
- Any non-image-generation expansion — revisit only after strong repeated usage of `zdx imagine`.