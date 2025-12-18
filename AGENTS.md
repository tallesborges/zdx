# Repository Guidelines

> **See also:** [SPEC.md](./docs/SPEC.md) for product values, contracts, and non-goals.
> SPEC.md is the source of truth for *what ZDX is* and *how it should behave*.
> This file (AGENTS.md) covers *how to develop* the codebase.

## Project Structure & Module Organization

- `src/main.rs`: CLI entrypoint; wires subcommands to implementations.
- `src/cli.rs`: `clap` command/flag definitions (`Cli`, `Commands`, etc.).
- `src/config.rs`: config loading + initialization (`config.toml`), with unit tests.
- `src/paths.rs`: resolves `ZDX_HOME`/default paths (config + sessions).
- `tests/`: integration tests for CLI behavior (`assert_cmd`, `predicates`).

## Build, Test, and Development Commands

- `cargo build`: compile a debug build.
- `cargo build --release`: compile an optimized binary.
- `cargo run -- --help`: run the CLI (args after `--`).
- `cargo test`: run unit + integration tests.
- `cargo fmt`: format the codebase with Rustfmt.
- `cargo clippy`: lint the codebase (optionally add `-- -D warnings` for strict CI-like checks).

Example: `cargo run -- config init` (creates a default config file).

## Coding Style & Naming Conventions

- Rust edition: 2024 (see `Cargo.toml`).
- Formatting: Rustfmt defaults; 4-space indentation (standard Rust style).
- Naming: modules/files `snake_case.rs`, types `UpperCamelCase`, fns/vars `snake_case`.
- Errors: prefer `anyhow::Result` + `Context` at I/O boundaries for actionable messages.

## Testing Guidelines

- Unit tests live next to code (e.g., `src/config.rs`); integration tests in `tests/*.rs`.
- Prefer black-box CLI tests using `assert_cmd::cargo::cargo_bin_cmd!`.
- Naming: `test_<behavior>_<expected>()` and keep tests independent/isolated.

Run a single integration test file: `cargo test --test config_path`.

## Commit & Pull Request Guidelines

- Commit messages generally follow Conventional Commits (e.g., `feat: ...`, `fix: ...`).
- PRs should include: a clear description, repro steps (or example commands), and tests.
- Before opening a PR: run `cargo fmt`, `cargo clippy`, and `cargo test`.

## Security & Configuration Tips

- `ZDX_HOME` controls where config/data are stored; default is `~/.config/zdx`.
- Don’t commit secrets or local configs; use env vars and keep test fixtures synthetic.

## Uncertainty & ambiguity

- If the question is ambiguous or underspecified, explicitly call this out and:
  - Ask up to 1–3 precise clarifying questions, OR
  - Present 2–3 plausible interpretations with clearly labeled assumptions.
- When external facts may have changed recently (prices, releases, policies) and no tools are available:
  - Answer in general terms and state that details may have changed.
- Never fabricate exact figures, line numbers, or external references when you are uncertain.
- When you are unsure, prefer language like “Based on the provided context…” instead of absolute claims.

## Documentation Protocol (SPEC / ROADMAP / PLAN)

ZDX uses three docs with different responsibilities:

- **SPEC.md** = values + contracts + non-goals (source of truth)
- **ROADMAP.md** = high-level versions + outcomes (what/why, not how)
- **PLAN_vX.Y.md** = concrete, commit-sized implementation plan (how)

### Golden rules
1) **SPEC.md wins**. ROADMAP/PLAN must not contradict SPEC values (KISS/YAGNI, terminal-first, YOLO default, engine-first).
2) **Only update SPEC.md when a contract/value changes.** Refactors that don’t change observable behavior do not require SPEC changes.
3) **ROADMAP.md tracks “what’s next”** (features grouped by version). Implementation details belong in PLAN.
4) **PLAN is commit-sized**: each step has a runnable deliverable + at least one test (or a short justification).

---

## When you (the agent/LLM) receive a request

### A) If the request is "Add feature X to the roadmap"
You must:
1) Decide **which version** it belongs to (or propose one).
2) Update **ROADMAP.md** by inserting the item in the correct version section.
3) Check whether the feature changes any SPEC contract. If yes, update **SPEC.md**.
4) (Optional) If asked, create **PLAN_vX.Y.md** with commit-sized steps.

### B) If the request is "Implement feature X"
You must:
1) Identify the target version (from ROADMAP; if missing, propose one and update ROADMAP).
2) Update **SPEC.md** if the implementation changes:
   - CLI surface (commands/flags/output/exit codes)
   - session JSONL schema or paths
   - config keys or resolution rules
   - engine event stream contract
   - tools (name/input/output semantics, timeouts)
   - provider behavior that affects user-visible output
3) Update **ROADMAP.md** to reflect status:
   - move item to “Shipped” or mark partially shipped
4) Generate **PLAN_vX.Y.md** (or update it) as a list of micro-commits:
   - each commit: goal, deliverable, CLI demo command(s), files touched, tests, edge cases

---

## SPEC.md update rules (what qualifies as a “contract” change)

Update SPEC.md when any of the following change:

### CLI contract changes
- adding/removing commands or flags
- changing default behavior of existing flags
- changing stdout/stderr rules
- changing exit codes or meaning

### Persistence contract changes
- changing where config or sessions are stored
- changing JSONL event types or fields
- changing session ID format
- adding schema versioning rules

### Engine contract changes
- adding/removing/changing EngineEvent variants
- changing event semantics (delta/final/tool events)

### Tool contract changes
- new tools, removed tools
- input/output schema changes
- execution context rules (cwd, timeouts)
- error shaping and retry behavior

### Values / non-goals changes
- if a feature conflicts with YOLO default or terminal-first or KISS/YAGNI, SPEC must explicitly document the new stance.

---

## ROADMAP.md update rules

- ROADMAP contains **features/outcomes**, not implementation detail.
- Every roadmap item should be one of:
  - **User-visible UX improvement**
  - **New capability** (tool/command)
  - **Foundational enabler** (engine event stream, core extraction) described as an outcome

When updating ROADMAP:
- Keep items grouped by version (v0.2.x / v0.3.x / …)
- Add a short **one-line goal** per version
- If a feature is shipped, move it under “Shipped” for that version

---

## PLAN_vX.Y.md rules (micro-commit delivery)

When creating/updating a PLAN:
- Each step is ~1 commit and must include:
  - Commit title
  - Goal (1 sentence)
  - Deliverable (observable behavior)
  - CLI demo command(s)
  - Files changed (explicit list)
  - Tests added/updated (explicit list + what they assert)
  - Edge cases covered

Constraints:
- KISS/YAGNI: smallest useful increment
- Offline-testable: use fixtures/mocks for provider
- Don’t introduce new dependencies unless justified

### What counts as a “deliverable” for a PLAN micro-commit?

A PLAN step is valid if the commit leaves the repo in a runnable, coherent state and the change can be verified.

A deliverable MAY be:
- User-visible behavior (a command/flag/output that works for at least one real case), OR
- Internal capability with proof (new module / logic + unit tests + fixtures), OR
- Test harness / mock infra enabling offline tests, OR
- Refactor-only change with evidence (all existing tests pass) and a short justification.

A deliverable MUST include at least one verification path:
- A CLI demo command, OR
- A test command (`cargo test` / `go test` / `pytest` etc.), OR
- A short justification if no new tests are added (e.g., purely mechanical refactor covered by existing tests).

Avoid “half-integrations”:
- Do not expose a CLI command/flag that exists but is non-functional (“TODO”) unless SPEC explicitly allows it.
- Do not change persistence/schema contracts without implementing both write + read paths (or clearly gating it behind a versioned schema rule in SPEC).
- Prefer: internal + tests first, then a single commit that wires the complete vertical slice into the CLI.


---

## Output format requirement when updating docs

When I ask you to add/implement a feature, respond with:

1) **SPEC.md changes** (only if needed)
2) **ROADMAP.md changes** (always if feature affects roadmap)
3) **PLAN_vX.Y.md** (only if I asked for a plan or implementation)

Prefer showing the *full updated sections* (not vague suggestions), so I can copy/paste into files.
