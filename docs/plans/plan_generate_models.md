# Generate Models Registry (On-Demand Generator Binary) — Ship-First Plan

# Goals
- Generate a deterministic Rust model registry from `https://models.dev/api.json`, on-demand (no `build.rs`).
- Use the generated registry for the interactive model picker (replace the hardcoded `AVAILABLE_MODELS`).
- Make “update + verify up-to-date” easy for humans and CI.

# Non-goals
- Fetching models at runtime (no network dependency while using `zdx`).
- Supporting providers beyond what ZDX can actually call today (start with Anthropic-only filtering).
- Perfect metadata parity with models.dev (cost/context/etc can come later).

# Design principles
- User journey drives order
- Ship-first: ugly-but-usable first, refactor later
- Reducer for UI changes: `update(state, event)`; render reads state only
- Deterministic outputs: stable ordering + stable formatting
- No build-time network: generation is explicit and opt-in
- No stdout noise while TUI active; generator logs to stderr

# User journey
1) Start: developer runs a single command to update models.
2) Input: optionally choose provider/output path/source URL.
3) Submit: tool fetches + parses models.dev JSON.
4) See output: writes a generated Rust file (or reports “up to date”).
5) Stream: prints progress/errors to stderr (safe to paste into issues).
6) Scroll/navigate: review changes via `git diff` (generated file is readable).
7) Tools: CI runs a “check” mode to fail if generation is stale.
8) Selection/copy: user picks a model in the TUI from the generated list.
9) Markdown/polish: nicer naming/grouping/search later.

# Foundations / Already shipped (✅)
List any capabilities that already exist and should not be rebuilt.

- Terminal safety/restore in TUI (`crossterm` alt-screen + raw mode + panic hook)
  - ✅ Demo (how to verify quickly): run `cargo run --` then force exit (Ctrl+C / panic path) and confirm terminal returns to normal (echo, cursor, scroll).
  - Gaps (only if any): none expected (verify only).
- Existing model picker overlay + config persistence (`model` field)
  - ✅ Demo (how to verify quickly): run `zdx`, open model picker (slash command `/model`), change selection, restart and confirm persisted model.
  - Gaps (only if any): model list is hardcoded (`src/ui/tui.rs`).
- HTTP + JSON deps already present (`reqwest`, `serde`, `serde_json`, `wiremock`)
  - ✅ Demo (how to verify quickly): `cargo test`.
  - Gaps (only if any): generator binary + fixtures not present.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: On-demand generator binary (fetch → normalize → generate) ✅
- Goal: `cargo run --bin generate_models` writes a deterministic Rust file under `src/`.
- Scope checklist:
  - [x] Add `src/bin/generate_models.rs` with a small CLI (`--provider anthropic`, `--out`, `--url`, `--check`)
  - [x] Fetch `models.dev/api.json` (allow override via `--url` / env var)
  - [x] Parse only fields needed for picker: `id`, display name, tool-capable flag, deprecated status
  - [x] Filter to provider(s) ZDX supports today (Anthropic first) and tool-capable models
  - [x] Generate stable output (sorted, rustfmt-friendly, header comment, no timestamps)
  - [x] Write to a single committed file (e.g. `src/models_generated.rs`)
- ✅ Demo:
  - Run `cargo run --bin generate_models -- --provider anthropic`
  - See `Wrote src/models_generated.rs` and a clean `cargo build`
- Failure modes / guardrails:
  - Network failure → clear error message; non-zero exit
  - Schema drift → parse errors point to provider/model path
  - Non-determinism → enforce sorted iteration + stable formatting

## Slice 2: Wire generated registry into the app (no UX regression) ✅
- Goal: TUI model picker uses generated data instead of hardcoded constants.
- Scope checklist:
  - [x] Add a small runtime module (e.g. `src/models.rs`) defining `ModelOption` and `AVAILABLE_MODELS`
  - [x] `include!` the generated file (or `pub const` from generated file) and update `src/ui/tui.rs` to reference it
  - [x] Keep UI changes reducer-friendly (events update picker state; render reads state)
- ✅ Demo:
  - Run `cargo run --` and open `/model`; list matches generated file
  - Pick a model; it persists to config and is used for requests
- Failure modes / guardrails:
  - Generated list empty → show an in-UI error and fall back to current configured model
  - Model not present (user config) → picker defaults to first entry but does not overwrite config until user confirms

## Slice 3: “Check mode” for CI + simple docs
- Goal: Prevent stale generated models from silently drifting.
- Scope checklist:
  - [ ] Implement `--check` to compare expected output vs existing file (no write on success)
  - [ ] Document the workflow in `README.md` or `docs/SPEC.md`-adjacent doc: “how to update models”
  - [ ] Add a CI-friendly command snippet (even if CI isn’t wired yet)
- ✅ Demo:
  - `cargo run --bin generate_models -- --check` exits 0 when up to date, non-zero when not
- Failure modes / guardrails:
  - Different line endings / formatting → generator owns formatting; compare exact bytes for simplicity

## Slice 4: Minimal regression tests (protect contracts only)
- Goal: Lock determinism + filtering so MVP stays stable.
- Scope checklist:
  - [ ] Add an integration test using `wiremock` serving a fixture JSON
  - [ ] Test: generator output is deterministic and includes only expected filtered models
  - [ ] Test: `--check` fails when file differs
- ✅ Demo:
  - `cargo test` passes offline (no real network)
- Failure modes / guardrails:
  - Flaky tests → no real HTTP; only `wiremock` + fixtures

# Contracts (guardrails)
List 3–7 non-negotiable rules that must not regress.

- Generator is on-demand only; `cargo build` never requires network.
- Generated file is deterministic (same input JSON → same bytes).
- `--check` is authoritative and CI-safe (no writes, correct exit codes).
- TUI never prints transcript to stdout while active (keep existing contract).
- Model picker never breaks terminal restore (panic/ctrl-c still restores).

# Key decisions (decide early)
List the decisions that prevent rewrites.

- Output shape: generate `ModelOption { id, display_name }` only vs richer metadata (start minimal; expand later).
- Filtering rules: Anthropic-only + `tool_call == true` + skip `deprecated` (and what to do if models.dev lacks fields).
- Where generated code lives: `src/models_generated.rs` committed vs `include!` from `OUT_DIR` (choose committed for now).
- Backpressure/perf in picker: if model list grows large, decide whether to add type-to-filter (defer until needed).
- Keybindings + focus: ensure picker navigation doesn’t conflict with existing scroll/input keys (verify with long list).

# Testing
- Manual smoke demos per slice (commands + TUI checks listed above).
- Minimal regression tests only for the contracts: determinism, filtering, `--check` behavior, offline tests.

# Polish phases (after MVP)
Group improvements into phases, each with ✅ check-in demo.

- Phase 1 (✅ demo: picker still fast): add incremental search in picker + grouping by provider/family.
- Phase 2 (✅ demo: richer UI): show context window / max output / reasoning badge if available.
- Phase 3 (✅ demo: nicer diffs): generator emits sorted, wrapped formatting and a summary table to stderr.

# Later / Deferred
Explicit list of “not now” items + what would trigger revisiting them.

- Multiple providers (trigger: add another provider client in `src/providers/`).
- Runtime model discovery/caching (trigger: users want “latest models” without repo changes).
- `xtask` workspace split to keep generator deps isolated (trigger: dependency bloat or build times become noticeable).
- Auto-refresh in CI with PR bot (trigger: frequent upstream model changes causing churn).

