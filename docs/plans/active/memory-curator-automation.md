# memory-curator automation (Slice 6 of proactive memory)

# Goals

- Ship the deferred Slice 6 from `docs/plans/done/proactive-memory-suggestions.md`: a periodic memory indexer that scans recent ZDX threads and **proposes** durable memory items to save to the second brain.
- Deliver as a **bundled automation** (`memory-curator`) so the user can run it immediately with `zdx automations run memory-curator` — no setup, no scaffolding.
- Establish the `crates/zdx-assets/bundled_automations/` pattern (parallel to `bundled_skills/` and `bundled_commands/`) so future first-party automations have a stable shipping channel.

# Non-goals

- The broader architectural reset from the originating Telegram idea (collapse multi-crate workspace, adopt Rig as substrate, delete custom providers). That is multi-PR and explicitly out of scope.
- Automatic writes into `$ZDX_MEMORY_ROOT/Notes/`, `MEMORY.md`, or `.zdx/knowledge/`. v1 only produces a dated review file for the user to file manually.
- A new CLI subcommand (`zdx automations init` / templates). Bundled discovery covers the "instantly runnable" UX without new surface.
- New tool surface. The automation runs against the existing default tool set (`Thread_Search`, `Read_Thread`, `Memory_Search`, `Memory_Get`, `write`, etc.).

# Design principles

- Smallest shippable slice that delivers user-visible value.
- Bundled automations are **manual-only by contract** so future bundled additions cannot silently start running on a user's daemon. Users opt into scheduling by copying the file into `$ZDX_HOME/automations/`.
- User definitions shadow bundled ones with the same file stem — same precedence model as bundled skills / subagents.
- Suggestions, not actions — the user keeps full control over what ends up in their second brain.

# User journey

1. After upgrading, `zdx automations list` shows `memory-curator (bundled) - manual`.
2. User runs `zdx automations run memory-curator`.
3. The agent calls `Thread_Search` to find recent threads, calls `Read_Thread` on each, and dedupes proposed items against existing notes via `Memory_Search`.
4. The agent writes `$ZDX_HOME/memory_suggestions/<YYYY-MM-DD>.md` and prints a compact summary.
5. User reviews the file, files items into NotePlan or `.zdx/knowledge/` themselves.
6. (Optional) User customizes the prompt by copying the bundled file into `$ZDX_HOME/automations/memory-curator.md` (their copy now shadows the bundled one) and adds a `schedule:` for daemon runs.

# Foundations / Already shipped (✅)

- Automation discovery, parsing, manual run, daemon scheduler, run-history log — `crates/zdx-engine/src/automations.rs`, `crates/zdx-cli/src/cli/commands/automations.rs`.
- Bundled-asset embedding pattern (skills + commands) — `crates/zdx-assets/build.rs`, `crates/zdx-assets/src/lib.rs`.
- Tools the automation needs — `Thread_Search`, `Read_Thread`, `Memory_Search`, `Memory_Get`, `write`, `bash`, etc. (already in the default tool set per `crates/zdx-engine/src/tools/mod.rs`).
- Proactive in-chat suggestions plumbing — see `docs/plans/done/proactive-memory-suggestions.md` (Slices 1-5 done; Slice 6 was deferred to here).

# MVP slices (this PR is one slice)

## Slice 1 — Bundled automations + `memory-curator`

- **Goal**: ship the new bundled-automation source and a single bundled `memory-curator` automation that produces dated suggestion files.
- **Scope checklist**:
  - [x] `crates/zdx-assets/build.rs` generates a `bundled_automations_manifest.rs` (third asset manifest).
  - [x] `crates/zdx-assets/src/lib.rs` exposes `BundledAutomationAsset` + `bundled_automation_assets()`.
  - [x] `crates/zdx-engine/src/automations.rs`:
    - [x] Adds `AutomationSource::Bundled` (display label `bundled`).
    - [x] Refactors parsing into `parse_automation_content` (core) + `parse_automation_file` (disk wrapper).
    - [x] Adds `parse_bundled_automation` that uses a synthetic `<bundled>/<relative_path>` display path and **rejects bundled assets with a `schedule`** field.
    - [x] Adds `discover_with_sources(bundled, root, user_dir)` and routes `discover`/`discover_with_user_dir` through it.
    - [x] Bundled assets are merged first; user files overwrite same-stem entries; duplicates within a single source still bail.
  - [x] `crates/zdx-assets/bundled_automations/memory-curator.md` ships a manual-only prompt that uses `Thread_Search`, `Read_Thread`, and `Memory_Search`, writes `$ZDX_HOME/memory_suggestions/<YYYY-MM-DD>.md`, and never auto-writes to user notes.
  - [x] `.zdx/skills/automations/SKILL.md` documents the new bundled source, precedence rules, and manual-only contract.
  - [x] Unit tests in `crates/zdx-engine/src/automations.rs` cover: bundled discovery; user shadowing bundled; bundled-with-schedule rejection; invalid-UTF-8 path context; duplicate-bundled error; the shipped `memory-curator` parses cleanly.
  - [x] Existing CLI integration tests updated: `test_automations_list_includes_bundled_memory_curator` and `test_automations_validate_includes_bundled` replace the old empty-list / count=1 assertions.
- **✅ Demo**:
  - `zdx automations list` → shows `memory-curator (bundled) - manual`.
  - `zdx automations validate` → `Validated N automation(s).` includes `memory-curator`.
  - `zdx automations run memory-curator` → writes `$ZDX_HOME/memory_suggestions/<date>.md` and prints a compact summary (live verification, manual).
- **Risks / failure modes**:
  - Future bundled automations could accidentally include a `schedule` — guarded by the explicit parser rejection.
  - User confusion about precedence — documented in the automations skill and surfaced via the `(bundled)` label in `list`.

# Contracts (guardrails)

- Bundled automations MUST be manual-only; discovery rejects bundled assets with a `schedule:` field.
- User automations MUST shadow bundled automations with the same file stem.
- Duplicate automation names within a single source MUST bail with a clear error.
- The `memory-curator` automation MUST NOT auto-write into `$ZDX_MEMORY_ROOT/Notes/`, `MEMORY.md`, or `.zdx/knowledge/` — it only writes a review file under `$ZDX_HOME/memory_suggestions/`.
- Existing automation behavior (discovery from `$ZDX_HOME/automations/`, daemon scheduling, run history) MUST NOT regress.

# Key decisions (decide early)

- **Shipping shape**: bundled automation discovered alongside user automations beats a new `zdx automations init` CLI surface for v1 — immediate value, no extra commands, easy to migrate later if needed.
- **Daemon safety**: manual-only bundled policy is safer than a config flag for "allow scheduled bundled automations". Users explicitly opt into scheduling by copying the file into their own directory.
- **Destination**: the originating idea left "Notes vs `.zdx/knowledge`" undecided, so v1 stays neutral and emits only a review report. The user files items where they want.

# Testing

- Unit tests in `crates/zdx-engine/src/automations.rs` (6 new tests).
- CLI integration tests updated in `crates/zdx-cli/tests/integration/config_path.rs`.
- Manual smoke test: `zdx automations run memory-curator` against a real `$ZDX_HOME` with at least one recent thread.

# Polish phases (after MVP)

## Phase 1: Use feedback drives prompt tuning

- Adjust suggestion frequency, dedupe sensitivity, and section format based on real review reports.
- Add a `--since` knob or per-run window once we know what feels right.

## Phase 2: Optional auto-file mode

- After enough manual review proves the suggestion quality, consider an opt-in mode that writes a stub note under `$ZDX_MEMORY_ROOT/Notes/_Inbox/` instead of just a suggestion line. Off by default.

# Later / Deferred

- Project-local `.zdx/knowledge/` integration — revisit when the user picks a destination strategy.
- Bundled automation `source` field surfaced in the monitor TUI — defer until there are enough bundled automations to warrant it.
- Cross-thread clustering for higher-quality suggestions — defer until basic curator output proves useful.
