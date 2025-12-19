# Repository Guidelines

> **See also:** [SPEC.md](./docs/SPEC.md) for product values, contracts, and non-goals.  
> SPEC.md is the source of truth for *what ZDX is* and *how it should behave*.  
> This file (AGENTS.md) covers *how to develop* the codebase.  
>
> **Decision log:** `docs/adr/` contains Architecture Decision Records (ADRs) explaining *why* we made notable choices.

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

### Testing philosophy (contract-first, surgical)

ZDX tests exist to **prevent breaking changes** and **lock in user-visible contracts** (SPEC/CLI/persistence/tool loop).
We do **not** write tests to maximize line/branch coverage.

Before adding a test, you must be able to answer in one sentence:

> **What contract would silently break if this test didn't exist?**

If you can't point to a SPEC contract, a CLI behavior, a persisted format, a prior bug/regression, or a genuinely tricky parser edge case, **do not add the test**.

### What we test (high leverage)

Prefer **black-box** tests (integration/CLI) over unit tests when possible.

We *do* test:
- **CLI contracts:** stdout/stderr shape, exit codes, `--format json` envelope, command/flag behavior.
- **Persistence contracts:** session JSONL schema, paths (`ZDX_HOME`), migration/version rules (if any).
- **Provider/tool loop contracts:** tool_use → tool_result cycles, event stream sequencing (via mock server/fixtures).
- **Tricky parsing/streaming logic:** e.g., SSE parsing, incremental JSON, UTF-8 boundary handling.
  - These should be covered by **a small number of fixtures** that exercise the important edges.

### What we avoid (low leverage / "test bloat")

We avoid tests that:
- Mirror implementation details (tests that read like the code's `if/else` tree).
- Assert internal/private helper behavior when the same behavior can be validated via a public API or CLI.
- Enumerate "all permutations" (e.g., whitespace variants, redundant precedence cases) unless there was a real regression.
- Require fragile setup (global state, timing dependencies, `sleep`, real network).
- Force serial execution (`--test-threads=1`) just to mutate process-global state.

**Rule of thumb:** if a test would fail after a refactor that does not change observable behavior, it's probably testing the wrong thing.

### Env vars and other global state (important)

Avoid mutating process-wide environment variables in unit tests (`std::env::set_var/remove_var`), because it:
- couples unrelated tests,
- can become flaky under parallel test execution,
- often leads to "serial test" footguns.

Prefer instead:
- **Integration tests** that spawn the CLI as a subprocess and set env vars on the `Command` (`assert_cmd` supports this).
- Tests that validate behavior via a **mock server base URL** (e.g., the CLI hits the mock endpoint when `ANTHROPIC_BASE_URL` is set), rather than unit-testing internal resolution branches.

If you *must* test env behavior in-process, keep it to **one minimal test** and scope changes with a guard pattern. Do not add a pile of permutation tests.

### Fixture discipline

- Use fixtures to cover **representative** cases, not exhaustive ones.
- Prefer **1–3 end-to-end fixture tests** over 10 micro-tests that each assert one tiny field.
- Add new fixtures only when they catch a regression or cover a new contract.

### Test review checklist (required)

When adding or changing tests, ensure:
- ✅ The test protects a SPEC/CLI/persistence/tool contract (cite the section or link an issue).
- ✅ The test asserts **observable behavior**, not internal structure.
- ✅ The test is minimal: one happy path + one failure path (or one regression) is usually enough.
- ✅ No new test depends on process-global env mutation unless unavoidable.
- ✅ There isn't already a test covering the same failure mode.

**Example smell:** Testing every permutation of env/config/default resolution for a base URL via a private helper.
**Preferred:** One integration test that runs the CLI with `ANTHROPIC_BASE_URL` pointing at a mock server and asserts the request arrives there.

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

## Documentation Protocol (SPEC / ROADMAP / PLAN / ADR)

ZDX uses four docs with different responsibilities:

- **SPEC.md** = values + contracts + non-goals (**source of truth**)
- **docs/ROADMAP.md** = optional priorities list (what/why, not how)
- **docs/plans/plan_*.md** = concrete, commit-sized implementation plan (how)
- **docs/adr/*.md** = **Architecture Decision Records** (why we chose a path, tradeoffs, consequences)

### Golden rules
1) **SPEC.md wins.** ROADMAP/PLAN/ADR must not contradict SPEC values (KISS/YAGNI, terminal-first, YOLO default, engine-first).
2) **Only update SPEC.md when a contract/value changes.** Refactors that don’t change observable behavior do not require SPEC changes.
3) **docs/ROADMAP.md is optional.** If it exists, it tracks “what’s next” with minimal maintenance (no implementation detail).
4) **PLAN is commit-sized.** Each step has a runnable deliverable + at least one test (or a short justification).
5) **ADRs are for durable decisions** (the “why”), not for restating SPEC/PLAN:
   - ADRs should not re-document CLI/schema contracts (that belongs in SPEC).
   - ADRs should not contain task lists (that belongs in PLAN).

### ADR rules
Write an ADR when you make a decision that is:
- Hard to reverse (format/storage/layout), OR
- Has meaningful tradeoffs (perf vs simplicity, strict vs flexible), OR
- Likely to be questioned later (“why not X?”), OR
- Impacts multiple modules or future features.

Conventions:
- Location: `docs/adr/`
- Filename: `NNNN-short-slug.md` (e.g., `0001-session-format-jsonl.md`)
- Status: `Proposed | Accepted | Superseded by ADR-XXXX`
- Prefer **superseding** an ADR over rewriting history.

Linking:
- SPEC sections affected by a decision should link to the ADR (“see ADR-0001”).
- PLAN steps may reference ADRs for rationale.
- PRs/commits should reference ADR numbers when relevant.

---

## When you (the AI assistant) receive a request

## Docs and process: Fast path vs Contract path

Most changes should follow the **Fast path**. Only use the **Contract path** when a user-visible contract changes.

### Fast path (default)
Use this when:
- change is internal/refactor, or
- feature is additive and does not change existing CLI/persistence/tool contracts.

Rules:
- You may ship without touching SPEC/ROADMAP/ADR.
- Add tests only if they catch a real regression risk (see Testing Guidelines).
- Updating `docs/ROADMAP.md` is optional and can be done later in batch.

### Contract path (only when needed)
Use this when the change modifies a contract in SPEC:
- CLI commands/flags/output/exit codes
- session schema/paths/schema_version rules
- tool schemas/envelopes/timeouts
- provider behavior that affects user-visible output
- engine event stream semantics

Rules:
- Update `docs/SPEC.md` with the minimal contract delta.
- Add a surgical test that would have caught an accidental contract break.
- Write an ADR only for decisions that are hard to reverse.
- Updating `docs/ROADMAP.md` is optional unless the change affects priorities/sequence.

### A) If the request is "Add feature X to the roadmap / priorities"
You should:
1) Add the item to `docs/ROADMAP.md` (if the repo is using it), placing it under **Now / Next / Later**.
2) Check whether the feature changes any SPEC contract. If yes, update `docs/SPEC.md`.
3) If adding the feature requires a notable design choice/tradeoff, create a short ADR in `docs/adr/`.
4) If asked, create a plan doc under `docs/plans/` (or update the current plan) with commit-sized steps.

### B) If the request is "Implement feature X"
You should:
1) Follow **Fast path** vs **Contract path** above.
2) Update `docs/SPEC.md` if (and only if) the implementation changes:
   - CLI surface (commands/flags/output/exit codes)
   - session JSONL schema or paths
   - config keys or resolution rules
   - engine event stream contract
   - tools (name/input/output semantics, timeouts)
   - provider behavior that affects user-visible output
3) If implementation requires a non-trivial decision (format choice, interface boundary, error model, storage rules), create/update an ADR in `docs/adr/` and link it from SPEC/PLAN as appropriate.
4) Update `docs/ROADMAP.md` only if it helps keep priorities accurate (optional).
5) If asked, generate/update a `docs/plans/plan_<short_slug>.md` file as a list of micro-commits:
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

- `docs/ROADMAP.md` contains **features/outcomes**, not implementation detail.
- Every roadmap item should be one of:
  - **User-visible UX improvement**
  - **New capability** (tool/command)
  - **Foundational enabler** (engine event stream, core extraction) described as an outcome

When updating ROADMAP:
- Prefer **Now / Next / Later** (or similar) over version micro-buckets.
- Keep “Now” short (e.g., max 3 items).
- If a feature is shipped, move it under “Shipped”.

---

## docs/plans/plan_*.md rules (micro-commit delivery)

When creating/updating a PLAN:
- Each step is ~1 commit and must include:
  - Commit title
  - Goal (1 sentence)
  - Deliverable (observable behavior)
  - CLI demo command(s)
  - Files changed (explicit list)
  - Tests added/updated (explicit list + what they assert)
  - Edge cases covered

**Important:** A PLAN step does **not** require *new* tests if existing tests already cover the behavior.
Never add a low-signal test just to satisfy the checklist. In that case, explicitly say:
- which existing tests cover it, or
- why the change is refactor-only and covered by `cargo test`.

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

0) **ADR changes** (only if needed / created / superseded)
1) **docs/SPEC.md changes** (only if needed)
2) **docs/ROADMAP.md changes** (only if updated / needed)
3) **docs/plans/plan_*.md** (only if I asked for a plan or implementation)

Prefer showing the *full updated sections* (not vague suggestions), so I can copy/paste into files.
