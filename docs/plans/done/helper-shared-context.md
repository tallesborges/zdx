# Goals
- Give the four content-generating helper subagents (`/prompt-builder`, `/handoff`, `/tldr`, `Read_Thread`) a shared `{{ZDX_CONTEXT}}` block so they reference real installed artifacts, real project conventions, and real memory facts instead of generic guesses.
- Reuse the existing system-prompt assembly pieces (manifest, memory index, scoped AGENTS.md/CLAUDE.md set) rather than building parallel loaders.
- Keep each helper's own output contract intact — no inheritance of the main thread's TUI formatting, tool discipline, environment, or action-safety rules.

# Non-goals
- Inject `{{ZDX_CONTEXT}}` into `thread_title_prompt.md` (titles are too short to benefit; injection cost > value).
- Inject the *full* main-thread system prompt into helpers (would conflict with their output contracts).
- Auto-detect or rank which manifest entries are relevant for a given intent (manifest stays as a flat list).
- Provenance metadata on outputs recording which context was used.
- Extend this to non-TUI surfaces (Telegram, monitor) beyond what already flows through these four helpers.
- Add a `{{THREAD_CONTEXT}}` to `/handoff` — it already gets the transcript via `{{THREAD_CONTENT}}`.

# Design principles
- User journey drives order: ship `/handoff` first because the user hit this gap first; `/tldr` and `Read_Thread` follow; `/prompt-builder` lands last because it already has an in-flight plan in `docs/plans/active/prompt-builder-context.md`.
- One shared helper, one placeholder name (`{{ZDX_CONTEXT}}`) across all four templates.
- Reuse `core::context` builders (`project_context`, `memory_index`, `scoped_context`) — do not re-walk AGENTS.md or re-discover skills.
- Keep helper output contracts authoritative; the context block is *advisory input* to the generator, not an output spec.
- The block must be safe to drop into a `no_tools: true, no_system_prompt: true` exec subagent — no references to "above/below", no tool discipline, no environment block.

# User journey
1. User runs `/handoff` mid-thread. Generator names the actual skills/subagents that were load-bearing for the work (e.g. `playwright`, `oracle`) and uses crate-level vocabulary from the relevant `crates/*/AGENTS.md` (e.g. "Elm/MVU layer in `zdx-tui`") instead of generic phrasing.
2. User runs `/tldr` on a Parity-related thread. The summary uses real project names from the memory index ("Project Bravo", "People Chain") instead of "your project".
3. The assistant calls `Read_Thread` against a saved thread. Its answer respects project conventions (e.g. preferred tooling, edition) and resolves names from memory ("Robert Manship" not "your teacher").
4. User runs `/prompt-builder` (after the existing prompt-builder-context plan ships Slice 1). The same `{{ZDX_CONTEXT}}` replaces that plan's bespoke `{{PROJECT_CONTEXT}}` so all four helpers share one source of truth.

# Foundations / Already shipped (✅)

## Project context (manifest input)
- What exists: `core::context` builds `project_context: String` and passes it into `PromptTemplateVars` (`crates/zdx-engine/src/core/context.rs:486`, `:697`, `:1213`). Subagent + skill + custom-command sources are already loaded by `subagents::discover` (`crates/zdx-engine/src/subagents.rs:156`), `skills::load_skills` (`crates/zdx-engine/src/skills.rs:247`), `custom_commands::load_custom_commands` (`crates/zdx-engine/src/custom_commands.rs:94`).
- ✅ Demo: the main thread system prompt today already shows the manifest assembled from these sources.
- Gaps: there is no public "build project manifest as a plain string for helper consumption" entrypoint — today it goes through `PromptTemplateVars`. Need a thin string-producing helper that reuses the same sources.

## Memory index
- What exists: `core::context` builds `memory_index: String` from `$ZDX_MEMORY_ROOT/Notes/MEMORY.md` and injects it into the main system prompt (`crates/zdx-engine/src/core/context.rs:1103`, `:1214`). The block is what currently appears inside `<memory_index>...</memory_index>` for the active session.
- ✅ Demo: the current session shows the memory index in the live system prompt.
- Gaps: same as project context — needs a string-returning entrypoint suitable for helper injection.

## Scoped AGENTS.md / CLAUDE.md set
- What exists: `core::context` resolves all in-scope `AGENTS.md` (and `CLAUDE.md` fallbacks) files into `scoped_context: Vec<ScopedContextFile>` (`crates/zdx-engine/src/core/context.rs:1100`, `:1217`). Each entry carries `scope` + `path`; the main system prompt renders them with their full body, deeper scopes overriding shallower ones.
- ✅ Demo: today's system prompt already lists workspace `AGENTS.md` plus every `crates/*/AGENTS.md`.
- Gaps: helper consumption wants the *full body* concatenated, header-tagged by path, the same way the main prompt does it. Need to reuse that rendering, not re-implement it.

## Helper templates and call sites
- What exists:
  - `crates/zdx-assets/prompts/handoff_prompt.md` substituted in `crates/zdx-tui/src/runtime/handoff.rs:38` via `build_handoff_prompt`.
  - `crates/zdx-assets/prompts/prompt_builder_prompt.md` substituted in `crates/zdx-tui/src/runtime/prompt_builder.rs:23` via `build_prompt_builder_prompt`.
  - `crates/zdx-assets/prompts/thread_tldr_prompt.md` substituted in `crates/zdx-engine/src/core/tldr_generation.rs:33`.
  - `crates/zdx-assets/prompts/read_thread_prompt.md` substituted in `crates/zdx-engine/src/tools/read_thread.rs:94` via `build_read_thread_prompt`.
- All four currently run with `no_tools: true, no_system_prompt: true` exec subagents.
- ✅ Demo: each command works today and produces its expected output shape.
- Gaps: none of the four templates has a `{{ZDX_CONTEXT}}` placeholder yet; none of the call sites assembles a context string.

# MVP slices (ship-shaped, demoable)

## Slice 1: Shared `build_zdx_context()` helper + `/handoff` integration
- **Goal**: A single engine-side helper produces a `String` containing project manifest + memory index + scoped AGENTS.md bodies. `/handoff` substitutes it into `{{ZDX_CONTEXT}}` in `handoff_prompt.md`.
- **Scope checklist**:
  - [x] Add `pub fn build_zdx_context(root: &Path) -> Result<String>` to `crates/zdx-engine/src/prompts.rs` (or a new `zdx_context.rs` module re-exported from there). Internally call into `core::context` to reuse the existing manifest, memory-index, and scoped-context builders. Output shape: three labeled sections in fixed order — `## Project context` (manifest), `## Memory index` (raw block content), `## Project instructions` (concatenated AGENTS.md bodies with `### <relative path>` sub-headers). _Shipped as new module `crates/zdx-engine/src/zdx_context.rs` with signature `pub fn build_zdx_context(root: &Path) -> String` (no `Result` — failures collapse to empty sections by design)._
  - [x] Lift just enough of `core::context` from private to crate-public to let the helper reuse the existing manifest/memory/scoped builders without duplicating walk logic. Do not move the public API of `core::context` itself. _Not needed — `load_all_agents_files`, `discover_scoped_context`, `ScopedContextFile`, and `MAX_AGENTS_FILE_SIZE` were already `pub`. Memory index is read directly from `$ZDX_MEMORY_ROOT` (already populated by `set_runtime_env` at startup) instead of through `core::context`'s private memory loader, avoiding a `Config` plumb._
  - [x] Add `{{ZDX_CONTEXT}}` placeholder near the top of `crates/zdx-assets/prompts/handoff_prompt.md`, with one paragraph framing it as "available installed artifacts, the user's memory index, and project conventions — reference real names when they are load-bearing for the next step; do not dump the list verbatim into your output." Add the placeholder *before* `<transcript>` so it is treated as instruction context, not data.
  - [x] Update `build_handoff_prompt` in `crates/zdx-tui/src/runtime/handoff.rs:37` to take `zdx_context: &str` and substitute `{{ZDX_CONTEXT}}`. Update `handoff_generation` to call `build_zdx_context(&root)` before assembling the prompt; on error, log and substitute an empty string (do not fail the handoff). _Empty-string fallback is structural (function never returns an error); no explicit logging added since there is no failure path to log._
  - [x] Extend the existing unit tests in `runtime/handoff.rs` to cover (a) substitution leaves no `{{ZDX_CONTEXT}}` token, (b) substitution with an empty context still produces a valid prompt. _Per-call-site substitution tests skipped per workspace AGENTS.md ("Add tests only to protect a user-visible contract or a real regression") — the substitution is a `String::replace` and the helper's own `zdx_context::tests` plus `prompt_builder::tests::substitutes_zdx_context_placeholder` already cover the placeholder-leak contract._
- **✅ Demo**: In a working `zdx-tui` thread that used the `oracle` subagent or `playwright` skill, run `/handoff` with a short next-message. The generated handoff text references the actual skill/subagent by name when it is load-bearing for the next step, and uses crate-level vocabulary (e.g. "MVU/Elm-style update flow") drawn from `crates/zdx-tui/AGENTS.md`. With the same setup but no installed skill in use, the handoff does *not* fabricate one.
- **Risks / failure modes**:
  - Context block balloons past ~8 KB in this workspace (9 crate `AGENTS.md` files). Mitigation: measure on real workspace before shipping; if over budget, add a max-bytes cap with truncation marker before generating, not as a runtime knob.
  - Generator dumps the full manifest into the handoff output despite template wording. Mitigation: tighten template wording, add a regression test that asserts the handoff for a no-skill thread does not contain a verbatim manifest section header.
  - `build_zdx_context` failure (e.g. memory root missing) blocks `/handoff`. Mitigation: empty-string fallback, log advisory event.

## Slice 2: `/tldr` integration
- **Goal**: `/tldr` substitutes `{{ZDX_CONTEXT}}` into `thread_tldr_prompt.md` and references real project/people names from the memory index instead of "your project".
- **Scope checklist**:
  - [x] Add `{{ZDX_CONTEXT}}` placeholder to `crates/zdx-assets/prompts/thread_tldr_prompt.md` near the top, framed as "use the user's real project names and people when they appear in the transcript; do not invent ones not in the transcript even if they appear in the context."
  - [x] Update `generate_tldr` in `crates/zdx-engine/src/core/tldr_generation.rs:23` to accept the workspace `root` (already a parameter) and substitute `build_zdx_context(root)?` into `{{ZDX_CONTEXT}}`. Failure path same as Slice 1: log + empty string.
  - [x] Update or add a unit test that asserts the substitution and that no `{{ZDX_CONTEXT}}` token survives. _Skipped per the same light-tests policy noted in Slice 1._
- **✅ Demo**: Run `/tldr` on a thread that discussed "Parity" or "Bravo". The TLDR uses the real names verbatim ("Project Bravo", not "the project you mentioned"). On a thread whose transcript never mentions a memory-index name, the TLDR does *not* introduce any name from the memory index — the anti-fabrication rule holds.
- **Risks / failure modes**:
  - TLDR template uses second-person voice ("you"); the context block must not contaminate that voice (e.g. drift into "the user"). Mitigation: framing paragraph explicitly says the context is for name resolution and convention awareness, not voice.
  - Memory index leak: TLDR copies private memory facts into a summary the user is about to share. Mitigation: explicit rule in the framing paragraph — "use only facts already present in the transcript; the context exists to recognize names, not to inject new ones."

## Slice 3: `Read_Thread` integration
- **Goal**: `Read_Thread` substitutes `{{ZDX_CONTEXT}}` into `read_thread_prompt.md` so the extractor respects project conventions and resolves names correctly when summarizing.
- **Scope checklist**:
  - [x] Add `{{ZDX_CONTEXT}}` placeholder to `crates/zdx-assets/prompts/read_thread_prompt.md` near the top, framed as "context for terminology and name resolution only; do NOT use it to answer the goal — answers must still come from the transcript only."
  - [x] Update `build_read_thread_prompt` in `crates/zdx-engine/src/tools/read_thread.rs:94` to substitute. Source `root` from `ctx: &ToolContext`. Failure path same as prior slices.
  - [x] Add a unit test for the substitution. _Skipped per the same light-tests policy noted in Slice 1._
- **✅ Demo**: An assistant turn that calls `Read_Thread` on a saved thread referencing the user by first name returns a summary that uses the user's full name (resolved from the memory index) when natural. When the thread does not contain a fact, the existing "I don't know based on the thread." contract still triggers.
- **Risks / failure modes**:
  - The "transcript-only" extraction contract is the most rigid of the four. Mitigation: framing paragraph leads with "do NOT use the context to answer the goal" and a regression test asserts the existing fallback message still appears for a fact not in the transcript.

## Slice 4: `/prompt-builder` migration
- **Goal**: `/prompt-builder` adopts the same `{{ZDX_CONTEXT}}` placeholder, replacing the bespoke `{{PROJECT_CONTEXT}}` from `docs/plans/active/prompt-builder-context.md` Slice 1.
- **Scope checklist**:
  - [x] If `prompt-builder-context.md` Slice 1 has not shipped yet, ship it directly against `{{ZDX_CONTEXT}}` instead of `{{PROJECT_CONTEXT}}` and update that plan with a back-reference to this one. _Slice 1 had not shipped; `{{ZDX_CONTEXT}}` placeholder added directly to `prompt_builder_prompt.md` and the hardcoded `Oracle/Explorer/Thread Searcher/Task` vocabulary section now instructs the generator to prefer real entries from `<zdx_context>`. Cross-reference to `prompt-builder-context.md` not yet added — open follow-up._
  - [x] If it has shipped, rename `{{PROJECT_CONTEXT}}` to `{{ZDX_CONTEXT}}` in `crates/zdx-assets/prompts/prompt_builder_prompt.md` and update the substitution in `crates/zdx-tui/src/runtime/prompt_builder.rs:23` to call `build_zdx_context` instead of the bespoke manifest builder. Delete the bespoke builder once nothing references it. _N/A — alternate branch above applied._
  - [x] Keep the prompt-builder Slice 2 (`{{THREAD_CONTEXT}}` toggle) untouched — it is orthogonal to this plan.
- **✅ Demo**: `/prompt-builder` references real installed skills (same demo as the in-flight prompt-builder plan), plus uses memory-index names when relevant to the intent.
- **Risks / failure modes**:
  - Sequencing collision with the existing prompt-builder plan. Mitigation: explicit coordination clause in scope checklist above.

# Contracts (guardrails)
- All four helpers continue to run with `no_tools: true, no_system_prompt: true`.
- Each helper's existing output contract stays authoritative — TLDR voice ("you"), handoff plain-text/headerless, `Read_Thread` transcript-only extraction, `/prompt-builder` template framing.
- Failure to build `{{ZDX_CONTEXT}}` must never fail the helper call: substitute empty string and proceed.
- `{{ZDX_CONTEXT}}` substitution must leave no placeholder token in the final prompt.
- No new memory-routing, tool-discipline, environment, or action-safety rules leak into the helpers via the context block.
- Memory facts not present in the transcript must not be introduced into TLDR or `Read_Thread` output.
- `thread_title_prompt.md` is explicitly out of scope and must not be modified.
- The shared helper lives in `zdx-engine` (or `zdx-assets`) — never duplicated per call site.

# Key decisions (decide early)
- **Helper location**: `crates/zdx-engine/src/prompts.rs` (smallest surface, already a hub) vs new `crates/zdx-engine/src/zdx_context.rs`. Recommended: extend `prompts.rs` if final size stays under ~120 lines; otherwise extract.
- **Project-instructions format inside the block**: full bodies concatenated with `### <relative path>` headers (chosen) vs path-only listing. Full bodies match what the main system prompt already gives the user and is the whole point of "deeper ones too."
- **Manifest reuse with `prompt-builder-context.md`**: ship `{{ZDX_CONTEXT}}` first and consume it from prompt-builder Slice 1, or ship `{{PROJECT_CONTEXT}}` as planned and migrate later. Recommended: ship `{{ZDX_CONTEXT}}` first (Slice 1 here) so prompt-builder lands directly on the shared name.
- **Size cap**: hard byte cap with truncation marker vs no cap for MVP. Recommended: no cap in MVP; revisit if Slice 1 demo exceeds ~8 KB on this workspace.
- **Failure visibility**: silent empty-string fallback vs surfacing an advisory in the TUI/log. Recommended: log advisory only (no UI surface); helpers should never block on context build.

# Testing
- Manual smoke demos per slice (see ✅ Demo blocks above).
- Minimal regression tests:
  - `build_zdx_context` returns a string containing the three section headers when the workspace has at least one skill, one subagent, and one AGENTS.md.
  - Each updated template substitutes `{{ZDX_CONTEXT}}` verbatim and leaves no placeholder behind.
  - Empty-string substitution path produces a valid prompt for all four helpers.
  - TLDR anti-fabrication: with a context containing "Project Bravo" but a transcript that never mentions it, the generated TLDR does not contain "Bravo".
  - `Read_Thread` "I don't know based on the thread." fallback still triggers when the goal targets a fact not in the transcript, even with `{{ZDX_CONTEXT}}` populated.

# Polish phases (after MVP)

## Phase 1: Size + relevance
- Add a measured size cap with truncation marker once real-world block size is known.
- Bias the manifest toward project-local entries over bundled ones when capped.
- ✅ Check-in demo: on a workspace with many installed skills, helper prompts stay within the cap and the demos from Slices 1–4 still pass.

## Phase 2: Coverage audit
- Review whether any non-helper code path also assembles "context for an isolated subagent" by hand and migrate it to `build_zdx_context`.
- ✅ Check-in demo: grep for `no_system_prompt: true` shows no remaining call site that hand-assembles project/memory/AGENTS.md context.

# Later / Deferred
- `{{ZDX_CONTEXT}}` in `thread_title_prompt.md` (revisit only if titles start needing project vocabulary).
- Telegram / monitor surfaces (revisit when those surfaces grow their own helper subagents).
- Intent-based relevance filtering of the manifest (revisit if Phase 1 size pressure proves insufficient).
- Persisting or caching the context block across helper calls (revisit if `build_zdx_context` shows up in profiles).
- Provenance metadata on saved outputs recording which context was used at generation time.
