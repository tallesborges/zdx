# Goals
- A live, terminal-native "cockpit" the user reads in a nvim/LazyVim split while working, no context switch.
- Give the user a clear vision of their day: what needs action / open loops, what they're working on now, what they finished today, where their attention should go — plus proactive insights the AI can see that the user might be missing.
- One agent-owned file `cockpit.md` refreshed on a schedule, holding that vision.
- One user-owned file `actions.md` where the user replies in free-form natural language; the AI interprets intent against full context, does the reasonable thing (archive, draft a reply, complete a task, snooze, etc. — not a fixed command set), and reports outcomes back into `cockpit.md`.
- Zero-clobber concurrency: the user can edit freely in nvim while the loop runs.

# Non-goals
- Native ZDX TUI dashboard (this is the north star, deferred — see Later).
- Sub-minute / true real-time refresh.
- Single-file "agent zone / your zone" merge layout (rejected in brainstorm: two writers on one file).
- Telegram supergroup / automation-routing reorg and morning-report retirement (related but separate; see Later).
- In-place Google Docs editing, Sheets, Drive workflows.

# Design principles
- User journey drives order.
- Single-writer per file: **agent-owned** files (`cockpit.md`, `state.json`) are written only by the agent; the **user-owned** file (`actions.md`) is written only by the user. The user reads `cockpit.md` but never edits it (the agent will overwrite it) — notes go in `actions.md`.
- Atomic writes: agent-owned files are written to a temp file in the same dir and renamed, so nvim `autoread` never sees a half-written file.
- Idempotency + lock: every answered item is acted on exactly once, tracked in an agent-owned ledger; a run lock prevents overlapping runs from double-acting.
- Stable anchors gate mutations: cockpit items carry stable IDs; the rule is **no stable ID → no mutation** (unless the target is uniquely obvious and the action is low-risk).
- Open intent, not a command grammar: after the anchor, the user writes natural-language intent; the AI interprets it with full context. No fixed verb list to memorize.
- Safe by default: the AI only acts on clearly-answered items. Ambiguous or high-stakes intent is proposed back in `cockpit.md`, not executed. No email is auto-sent (replies become drafts).
- Ugly-but-functional first: plain Markdown, one automation, dogfood daily before adding polish.

# User journey
1. User opens `cockpit.md` (left split) and sees the day's vision: what needs action / open loops, what's in progress, what's done, where attention should go, and AI insights.
2. User responds in `actions.md` (right split) in plain language next to the items they care about, saves.
3. On the next run the AI interprets those responses, acts, and reports outcomes back into `cockpit.md`.
4. `cockpit.md` keeps refreshing on a schedule so the vision stays live while the user works.

# Foundations / Already shipped (✅)
## Automation runtime
- What exists: single-file automations in `$ZDX_HOME/automations/*.md` with `schedule` (cron), `model`, `timeout_secs`, `max_retries`. CLI: `zdx automations validate | run <name> | runs [name] | daemon`. Daemon polls (default 30s).
- ✅ Demo: `zdx automations list` shows existing automations; `zdx automations validate` passes.
- Gaps: none for this feature.

## Google data via `gog`
- What exists: `gog gmail messages search '<query>' --account <addr> --json --no-input`, `gog gmail drafts create`, `gog gmail send --reply-to-message-id`, calendar events, per-account via `--account` / `GOG_ACCOUNT`. Proven in `morning-report.md`.
- ✅ Demo: `gog gmail messages search 'in:inbox is:unread' --max 5 --json --no-input` returns messages.
- Gaps: Parity + Opala accounts must be added with `gog auth add` before multi-account works (Key decision).

## Apple Reminders + ZDX threads
- What exists: `apple-reminders` scripts for today/overdue/complete; `Thread_Search` + `Read_Thread` for "what I did today" (pattern proven in `daily-review.md`).
- ✅ Demo: `python3 $ZDX_HOME/skills/apple-reminders/scripts/reminders.py today --json` returns tasks.

# MVP slices (ship-shaped, demoable)

## Slice 1: Cockpit generator (read-only situational awareness)
- **Goal**: A `live-cockpit` automation writes `cockpit.md` with the day's situation. No actions yet — pure read.
- **Scope checklist**:
  - [ ] Create `$ZDX_HOME/automations/live-cockpit.md` (Artifact pattern).
  - [ ] Write to a fixed path `$ZDX_HOME/cockpit/cockpit.md`, created if missing.
  - [ ] Write atomically (temp file in same dir + `mv`).
  - [ ] Sections: `Now` (local time + hours left in workday), `Needs action / open loops`, `In progress` (what I'm working on), `Done today` (from ZDX threads / completed reminders), `Attention` (calendar + important email, prioritized), `Insights` (proactive things the AI sees that I might be missing).
  - [ ] Every actionable item carries a compact stable ID and enough source metadata to act on later: `email:<account>:<gmail_message_id>`, `reminder:<reminder_id>`, `calendar:<event_id>`, `thread:<thread_id>`. Show it inline, e.g. `[E-personal-abc123]`.
  - [ ] Seed a short `actions.md` template (anchor + free-text example) but DO NOT process it yet.
  - [ ] Empty state: still write the file with "Nothing urgent right now."
- **✅ Demo**: `zdx automations run live-cockpit` → `cockpit.md` exists, has a fresh timestamp, lists today's calendar + important email; open in nvim and confirm `:e`/autoread reloads.
- **Risks / failure modes**:
  - Half-written file if not atomic → mitigated by temp+rename contract.
  - Source failure (calendar/gmail) → degrade: write the section with `[<source> unavailable]`.

## Slice 2: Open reasoning loop (interpret `actions.md`)
- **Goal**: The same automation, before regenerating the cockpit, reads the user's anchored free-form responses in `actions.md`, interprets intent with full context, acts each item exactly once, and reports outcomes.
- **Scope checklist**:
  - [ ] Add an agent-owned idempotency ledger `$ZDX_HOME/cockpit/state.json`: per answered item track `{anchor id, response fingerprint, inferred intent, status (processed|skipped|needs_clarification|failed), external result id (e.g. Gmail draftId), run timestamp}`.
  - [ ] Add a run lock (`mkdir`/`flock` on `$ZDX_HOME/cockpit/.live-cockpit.lock`); if held, skip the action phase and note "previous run still active".
  - [ ] Response format: `[<anchor id>] <free-text intent>` (e.g. `[E-personal-abc123] archive this`, `[E-parity-def456] draft a reply saying I'll review Monday`). Text stays natural language; the anchor makes the target unambiguous.
  - [ ] On run, phase 0: read `actions.md` + `state.json`, process only items whose (anchor + fingerprint) is not already `processed`. **No stable anchor → no mutation.**
  - [ ] Act via the right skill (archive/label via gog, complete via apple-reminders, `reply` → **Gmail draft**, snooze); record result in `state.json`.
  - [ ] Ambiguous or high-stakes intent → do NOT act; surface a clarifying line back in `cockpit.md` and mark `needs_clarification`.
  - [ ] Append results to a `Recently done` section in `cockpit.md` ("✅ archived …", "📝 draft created for …", "❓ need clarification on …").
  - [ ] Never modify `actions.md` (single-writer contract).
  - [ ] Empty state: nothing new/unprocessed → skip phase 0 silently.
- **✅ Demo**: write `[E-...] archive the invoice email` and `[E-...] draft a reply to Torsten saying I'll review tomorrow`, run automation → invoice archived, one Gmail draft created, both reported and recorded in `state.json`; run again → no duplicate action (idempotent); a vague/anchorless note produces a clarifying question instead of a wrong action.
- **Risks / failure modes**:
  - Duplicate actions on re-runs → mitigated by `state.json` ledger + fingerprint.
  - Overlapping runs → mitigated by run lock.
  - Misinterpreting intent → keep mutations conservative; anything unclear becomes a question.
  - Sending real email by mistake → mitigated: reply = draft-only in MVP.

## Slice 3: Multi-account email
- **Goal**: `Important email` covers all three accounts (personal + Parity + Opala), tagged by source.
- **Scope checklist**:
  - [ ] Ensure `gog auth add` done for Parity + Opala (Key decision / prerequisite).
  - [ ] Loop the gmail search per account with `--account`; tag each item `[personal] / [parity] / [opala]`.
  - [ ] If an account is unauthed/unreachable, note `[<account> unavailable]` and continue.
- **✅ Demo**: `cockpit.md` shows important email from all three accounts, each tagged; killing one account's auth degrades to a noted gap, others still render.
- **Risks / failure modes**:
  - Rate/latency with 3 accounts × frequent runs → tune `--max` and cadence.

## Slice 4: Live refresh
- **Goal**: The cockpit updates itself while the user works — cadence ramped up conservatively after measuring.
- **Scope checklist**:
  - [ ] Start read-only scheduled runs at `*/15` during work hours only (e.g. `*/15 8-19 * * 1-5`); measure avg run time, token cost, and `gog` latency.
  - [ ] Only after the action loop + ledger + lock are proven, move toward `*/5`. Keep cadence trivial to change.
  - [ ] Confirm the automations daemon runs it (`zdx automations daemon`); rely on the run lock for overlap safety.
  - [ ] Set a fast/cheap `model` to keep frequent-run cost low.
- **✅ Demo**: with the daemon running, watch `cockpit.md` timestamp advance on schedule; a run that overruns the interval does not double-act (lock holds).
- **Risks / failure modes**:
  - Cost/API rate from frequent LLM runs across 3 accounts → 15-min work-hours default first, cheap model, ramp only after measuring.

# Contracts (guardrails)
- Agent writes ONLY agent-owned files (`cockpit.md`, `state.json`); it MUST NOT edit `actions.md`. The user writes ONLY `actions.md` and MUST NOT edit `cockpit.md`.
- Agent-owned files are always written atomically (temp + rename), never streamed in place.
- Idempotency: each answered item is acted on exactly once, enforced via the `state.json` ledger (anchor + response fingerprint). Re-runs never repeat a completed action.
- A run lock prevents overlapping runs from acting twice.
- No mutation without a stable anchor (unless the target is uniquely obvious and low-risk). Ambiguous/high-stakes intent is surfaced as a question, never guessed into an action.
- No email is sent in MVP — `reply` produces a draft. Archiving and task-completion are the only mutations.
- Every run produces a visible `cockpit.md` even in the empty state.
- On any source failure, degrade and note the gap; never abort the whole run.

# Key decisions (decide early)
- **File location**: default `$ZDX_HOME/cockpit/{cockpit.md,actions.md}`. Change now if you want them inside a project dir instead.
- **Refresh cadence**: ramp conservatively — manual → `*/15` work-hours read-only → `*/5` only after cost/latency measured and the action loop is proven. Every-minute is out.
- **Idempotency store**: agent-owned `state.json` (doesn't break single-writer — user isn't a writer). Chosen over embedding the ledger in `cockpit.md` for robustness.
- **Anchors**: cockpit items get stable IDs in Slice 1 so Slice 2 mutations aren't built on mutable prose.
- **One automation, two phases** (execute actions → regenerate cockpit) vs. two separate automations. Plan assumes one — simpler, no coordination.
- **reply = draft in MVP**, promote to real send only after trust (avoids headless mis-sends).
- **Interpretation vs. commands**: resolved toward open free-form interpretation (no fixed verb grammar). Safety comes from "ask when unsure + draft-only email", not from a rigid syntax.
- **Multi-account prerequisite**: Parity + Opala must be added via `gog auth add` before Slice 3.
- **Model**: pick a fast/cheap model for frequent runs.

# Testing
- Manual smoke demo per slice (see each ✅ Demo).
- `zdx automations validate` after creating/editing the automation.
- No new Rust tests — this is an automation + Markdown files, not repo code. (Native dashboard in Later would add tests.)

# Polish phases (after MVP)
## Phase 1: Sharper awareness + actions
- Prioritize by hours-left; smarter attention ranking; promote trusted replies from draft to send; richer insight generation across all context.
- ✅ Check-in demo: a day view that reflows as hours pass; the AI flags something genuinely useful the user hadn't noticed.

## Phase 2: Delivery/routing cleanup
- Decide `cockpit` vs `morning-report` overlap; retire/trim morning-report if the cockpit covers it.
- ✅ Check-in demo: no duplicate morning-report + cockpit content.

# Later / Deferred
- **Native ZDX dashboard (north star)**: a real TUI in `zdx-tui`/`zdx-monitor` reading the same structured data. Trigger: the Markdown cockpit is proven daily-useful and its format is stable.
- **Telegram supergroup reorg** (separate ZDX-dev vs Daily groups). Trigger: user decides to act on the group split discussed in brainstorm.
- **Opala project actions** beyond email. Trigger: Opala work actually starts.

# Oracle review (addendum)
Reviewed before building. Verdict: read-only cockpit is feasible; the headless action loop was unsafe as first written. Key catch: because the agent never edits `actions.md`, there was no way to tell "already processed" from "new", so a 5-min loop could re-create drafts / repeat mutations. Accepted changes, now folded in above:
- Idempotency ledger (`state.json`) + response fingerprints; act-exactly-once.
- Run lock to prevent overlapping-run double-acting.
- Stable item IDs in Slice 1; "no anchor → no mutation" for Slice 2.
- `cockpit.md` is read-only for the user (was ambiguous).
- Conservative cadence ramp (manual → `*/15` work-hours → `*/5`), not `*/5` by default.
Not adopted: none rejected — all recommendations incorporated.
