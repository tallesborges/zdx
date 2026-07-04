# Goals
- Turn ideas captured in the "ZDX Features" note into Git-tracked, reviewable plans without manual copy-paste.
- Ship a manual-trigger ZDX automation that reads the backlog note, picks an un-planned idea, and opens a GitHub PR that adds a new ship-first plan file under `docs/plans/active/`.
- Never touch the user's main working tree or any code — plan generation happens on a dedicated branch/worktree and lands only via PR the user accepts or closes.
- Give the backlog a way to become action (plans) instead of history-only notes.

# Non-goals
- Implementing the plans (that is the separate "Autonomous Plan Implementer" idea).
- Editing or writing code files — output is plan markdown only.
- Auto-merging PRs (the user always reviews and accepts).
- Rewriting or reorganizing the "ZDX Features" note structure.
- Building new engine/Rust functionality — this reuses existing automation + bash + `gh` infrastructure.
- Semantic/embedding-based duplicate detection (MVP uses simple text/grep matching).

# Design principles
- User journey drives order.
- Read-first, write-via-PR-only: the automation reads the note and code but only ever mutates a throwaway worktree + branch, never the primary checkout.
- Idempotent: an idea that already has a plan or an open plan PR must not get a duplicate.
- Always deliver a visible result: a PR link, or an explicit "nothing to plan" message.
- Absolute paths over ambient cwd: the automation targets the zdx repo by absolute path so it works regardless of the run root.
- Reuse the `ship-first-plan` skill for the actual plan content and format.

# User journey
1. The user dumps feature ideas into the "ZDX Features" note over time.
2. The user (or a later schedule) triggers `zdx automations run zdx-features-maintainer`.
3. The automation reads the note, lists candidate ideas, and filters out ideas already shipped or already covered by a plan.
4. It selects the top un-planned idea, generates a ship-first plan for it on a dedicated branch/worktree, and opens a PR.
5. The user reviews the PR and merges (accept) or closes (reject) — the main working tree was never touched.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Automations system (user-defined)
- What exists: user automations are markdown + YAML frontmatter files discovered from `<ZDX_HOME>/automations/*.md` (`crates/zdx-engine/src/automations.rs:15-20`, `:57-64`). Supported frontmatter keys: `schedule`, `model`, `subagent`, `timeout_secs`, `max_retries` (`crates/zdx-engine/src/automations.rs:51-58`, `deny_unknown_fields`). CLI: `zdx automations validate` / `run <name>` / `runs` / `daemon` (`.zdx/skills/automations/SKILL.md`).
- ✅ Demo: `zdx automations list` shows the new automation after the file is created; `zdx automations validate` passes.
- Gaps: no bundled/system automation for this; it will be a user automation file.

## Automation runtime cwd + root
- What exists: an automation's tool cwd/root equals the resolved CLI root (`crates/zdx-cli/src/cli/mod.rs:735-743`, `:1084-1113`; `crates/zdx-cli/src/cli/commands/exec.rs:54-76`; bash runs in `ctx.root` at `crates/zdx-tools/src/bash.rs:290-294`). Frontmatter has no cwd field.
- ✅ Demo: running with `--root <zdx repo>` makes bash/tool calls operate in that repo.
- Gaps: to be root-independent, the prompt must use absolute paths (`git -C <repo>`, absolute note path) instead of relying on cwd.

## Git worktree infrastructure
- What exists: native worktree helper creates `<repo parent>/.zdx/worktrees/<repo>-<hash>/<id>` with branch `zdx/<id>` via `git worktree add` (`crates/zdx-engine/src/core/worktree.rs:15-51`, `:77-96`, `:148-187`); exposed as `zdx --worktree <id>` and a `/worktree` command. Plain `git worktree add` via the bash tool is also available.
- ✅ Demo: `git -C <repo> worktree add <path> -b <branch> HEAD` creates an isolated tree without disturbing the main checkout.
- Gaps: none for MVP; the automation can use plain `git worktree add` for a custom `plan/<slug>` branch.

## GitHub CLI + plan format + plan skill
- What exists: `gh` is the sanctioned GitHub tool (global AGENTS rule); `docs/plans/active/*.md` is the live-plan location with a consistent `# Goals / # Non-goals / # MVP slices` structure; the `ship-first-plan` skill defines the exact plan template and save path `docs/plans/active/<slug>.md`.
- ✅ Demo: existing files like `docs/plans/active/recall-tool-canonical-notes-threads.md` show the target format; `gh pr create` opens PRs.
- Gaps: none.

## "ZDX Features" note access
- What exists: canonical note at `$ZDX_MEMORY_ROOT/Notes/20-29 Development & Tech/21.02 ZDX/ZDX Features.md`; entries are `### Title` + `` `Status` — description ... Thread: `...` `` blocks. Direct `read` on the canonical path is deterministic; `Memory_Search`/`Memory_Get` cover fuzzy discovery (`crates/zdx-engine/src/tools/memory_get.rs:15-17`).
- ✅ Demo: reading the file yields the parseable idea list.
- Gaps: none.

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Single-idea plan PR (manual, safe on repeat runs)
- **Goal**: One command turns the first eligible un-planned idea into a PR that adds a ship-first plan file, on an isolated branch, without touching the main tree — and re-running never creates a duplicate.
- **Scope checklist**:
  - [ ] Create `~/.zdx/automations/zdx-features-maintainer.md` (frontmatter: `model` set to a capable long-context model, `timeout_secs`, no `schedule` = manual-only).
  - [ ] **Preflight (fail fast, before any branch/worktree)**: assert the zdx repo absolute path exists and is a git repo; the note path is readable; `gh auth status` OK; push access via non-mutating `git -C <repo> ls-remote origin`; resolve the PR base branch explicitly via `gh repo view --json defaultBranchRef`. On any failure, print a visible failure summary and exit.
  - [ ] **Concurrency lock**: acquire an atomic lock (e.g. `mkdir /tmp/zdx-features-maintainer.lock`) before selection; release on exit. If locked, report and exit.
  - [ ] Read the ZDX Features note by absolute path; list candidate ideas (`### Title` + status line); skip ideas whose status is clearly `Shipped`/`Done`/`Implemented`.
  - [ ] **Deterministic selection**: pick the **first eligible** idea in note order (no ranking in MVP). Derive a stable `<slug>` from the title.
  - [ ] **Minimal dedup preflight (per selected idea)**: skip and fall through to the next candidate if any of these already exist — `docs/plans/active/<slug>.md`, `docs/plans/done/<slug>.md`, an open PR (`gh pr list --head plan/<slug>`), or a remote branch (`git -C <repo> ls-remote --heads origin plan/<slug>`).
  - [ ] Create an isolated worktree **outside the main checkout** (e.g. under `$TMPDIR`), on a new branch: `git -C <repo> worktree add <tmp path> -b plan/<slug> <base>`.
  - [ ] Generate a ship-first plan for that idea following the `ship-first-plan` template; write it to `docs/plans/active/<slug>.md` **inside the worktree**.
  - [ ] **Plans-only assertion**: stage explicitly (`git -C <wt> add docs/plans/active/<slug>.md`); assert `git -C <wt> diff --cached --name-only` is exactly that one file and `git -C <wt> status --porcelain` shows nothing unexpected; abort visibly otherwise. All mutating git uses `git -C <wt>` (never the main repo).
  - [ ] Commit, `git -C <wt> push -u origin plan/<slug>`, then `gh pr create --repo <owner>/<repo> --base <base> --head plan/<slug>` with a body summarizing the idea + note back-link.
  - [ ] **Safe cleanup (finally/trap)**: always remove the worktree if created; on pre-push failure delete the local branch; on post-push/pre-PR failure leave the remote branch and print the exact `gh pr create` recovery command. Report the PR URL (or failure) in the run output.
- **✅ Demo**: `zdx automations run zdx-features-maintainer` produces a PR containing exactly one new `docs/plans/active/<slug>.md`; `git status` on the main checkout is clean; **running it a second time does not create a duplicate PR** and reports the idea as already planned.
- **Risks / failure modes**:
  - `gh`/git auth failure → caught in preflight, exits before creating anything.
  - Branch/file/PR collision on repeat run → prevented by the minimal dedup preflight.
  - Worktree left behind on crash → trap-based cleanup + documented manual `git worktree prune`.
  - Model writes stray files → caught by the staged-diff assertion (abort).

## Slice 2: Robust dedup + already-implemented guard
- **Goal**: Beyond the exact slug/branch match in Slice 1, avoid planning ideas that are semantically covered by an existing plan or already implemented.
- **Scope checklist**:
  - [ ] Fuzzy-match the idea against `docs/plans/` (active + done) by title/keywords, not just exact slug.
  - [ ] Match against open PR titles via `gh pr list`, not only the `plan/<slug>` head branch.
  - [ ] Quick implemented-signal scan: grep the codebase/docs for obvious markers the idea already exists; if strong, skip and note why.
  - [ ] When any check is uncertain, still create the plan PR (conservative — the user reviews).
- **✅ Demo**: An idea whose plan exists under a differently-named file, or whose note status is `Shipped`, is skipped with a logged reason; a genuinely-new idea still gets a PR.
- **Risks / failure modes**:
  - False "already implemented" → conservative default: when unsure, create the plan PR anyway.

## Slice 3: Batch pass (one PR per idea, capped)
- **Goal**: Process several un-planned ideas in one run, capped to a small N, each as its own PR.
- **Scope checklist**:
  - [ ] Iterate the top N un-planned ideas (default N small, e.g. 3).
  - [ ] Each idea gets its own branch/worktree/plan/PR (isolated failures don't abort the batch).
  - [ ] Run output summarizes: created PRs (with URLs), skipped ideas (with reasons), failures.
- **✅ Demo**: A backlog with 3 un-planned ideas yields 3 independent PRs in one run; a failure on one idea still produces PRs for the others.
- **Risks / failure modes**:
  - PR spam → the small cap and dedup guard (Slice 2) contain it.

# Contracts (guardrails)
- MUST NOT commit to or modify the main working checkout — all mutating git runs use `git -C <worktree>` on a `plan/<slug>` branch; the worktree lives outside the repo.
- MUST NOT modify any file outside `docs/plans/active/` in the generated PR — enforced by a staged-diff assertion (`git diff --cached --name-only` == the single plan file), not prompt wording alone.
- MUST NOT modify the "ZDX Features" note in MVP (read-only source).
- MUST NOT auto-merge or push to the base branch — output is always a PR for human review; PR base is resolved explicitly (`gh repo view --json defaultBranchRef`).
- MUST be idempotent from Slice 1: exact slug/plan-file/branch/PR collisions are checked before creating anything; no duplicate plan file or PR for an idea already planned.
- MUST always produce a visible result: PR URL(s), or an explicit "No new ideas to plan." message; never a blank run.
- MUST use `gh` for all GitHub operations and clean up worktrees it creates (trap/finally), even on failure.
- MUST serialize runs with an atomic lock so concurrent manual/scheduled runs can't race on the same idea/branch.

# Key decisions (decide early)
- **Repo targeting**: hardcode the zdx repo absolute path in the automation prompt and use `git -C <repo>` + absolute note path, so the automation is independent of the run root/daemon cwd (user confirmed: "as we have the path, it's enough").
- **Selection**: deterministic **first-eligible in note order** for MVP; value-ranking deferred to Phase 1. No per-run target argument — the CLI is only `zdx automations run <name>` (`crates/zdx-cli/src/cli/mod.rs:439-443`; prompt-only payload at `crates/zdx-cli/src/cli/commands/automations.rs:223-249`), so per-idea targeting would need an engine/CLI change and is out of scope.
- **Worktree location + branch**: plain `git worktree add` with branch `plan/<slug>`; worktree created **outside the main checkout** (e.g. `$TMPDIR`) and removed on exit. (Alternative: native `zdx --worktree`, but its `zdx/<id>` branch naming and root-resolution coupling fit interactive sessions better than a headless plan job.)
- **One PR per idea vs batched into one PR**: one PR per idea (easiest to review/accept/reject independently). Batching deferred.
- **Trigger**: manual-only first (no `schedule`); add a schedule only after robust dedup (Slice 2) is trusted.
- **Model**: set an explicit capable long-context model (note reading + code scanning + plan authoring); do not rely on a fast/cheap default.

# Testing
- Manual smoke demos per slice (see each slice's ✅ Demo).
- `zdx automations validate` must pass after creating/editing the automation file.
- Regression check for contracts: after a run, `git status` in the main checkout is clean and no unexpected files changed outside `docs/plans/active/` in the PR branch.

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.

## Phase 1: Selection quality + PR body richness
- Rank ideas by value/effort signals from the note status/age; richer PR bodies (idea summary, related plans, note back-link).
- ✅ Check-in demo: PRs open in a sensible priority order with a body that a reviewer can act on without opening the note.

## Phase 2: Scheduled operation
- Add a `schedule` for periodic runs once dedup is trusted; ensure the run reports cleanly when there is nothing new.
- ✅ Check-in demo: a scheduled run creates PRs only for genuinely new ideas and otherwise reports "No new ideas to plan."

# Later / Deferred
- Editing the "ZDX Features" note to mark ideas as `Planned` / link the PR back — deferred (keep note read-only until write-back is clearly wanted). Trigger: user asks for backlog status sync.
- Semantic/embedding duplicate detection over plans and code — deferred until simple grep dedup proves insufficient.
- Auto-implementing accepted plans — out of scope here; belongs to the "Autonomous Plan Implementer" idea.
- Batching multiple ideas into a single PR — revisit if one-PR-per-idea becomes noisy.
