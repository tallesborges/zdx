# zdx Mini App (apps/web)

Svelte 5 + Vite + Tailwind v4 frontend for the zdx Telegram Mini App.

Consumes the read-only JSON API served by `crates/zdx-bot/src/server.rs`, which also embeds this
app's build output and serves it at `/app`. This is the Mini App — the old vanilla `app.html` shell
has been removed.

## Where things are

- `build-cached.mjs`: content-fingerprinted build wrapper for `just web-build`; cache metadata lives in `node_modules/.cache/zdx-web-build.json`, never in embedded `dist`
- `build-cached.test.mjs`: temporary-fixture tests for no-op builds, input invalidation, output integrity, and failures
- `src/main.ts`: entrypoint; initializes the Telegram bridge before mounting
- `src/App.svelte`: framed app shell (shell colour behind, rounded surface panel) + drawer host
- `src/app.css`: the whole design system — Tailwind v4 `@theme` tokens
- `src/lib/telegram.ts`: `window.Telegram.WebApp` bridge (scheme, safe areas, haptics, back button, `start_param`, `openTelegramLink`)
- `src/lib/api.ts`: typed fetch client; sends `Authorization: tma <initData>`
- `src/lib/types.ts`: response types mirroring `server.rs` — keep in sync when the Rust structs change
- `src/lib/router.svelte.ts`: query-string router. Canonical: `?view=threads`, `?view=thread&id=…&tab=…`
  and `?view=monitor&section=…`. The bot's legacy `?view=threads&id=…`, `?view=git` and bare
  `startapp=<thread_id>` links are normalized — do not break those. `?view=threads` **with** an `id`
  stays a single-thread link for that reason; only the bare form opens the list.
- `src/lib/transcript.ts`: projects the flat activity stream into a three-level collapsible tree
  (`work` turn → folded `group` → single `tool`) and derives the one-line summaries
- `src/lib/diff.ts`: unified-diff parser with word-level intra-line segmentation
- `src/lib/highlight.ts`: compact line-local syntax tokenizer + span merger (six `--hljs-*` classes)
- `src/lib/markdown.ts`: `marked` + allowlist sanitizer (model output is untrusted)
- `src/lib/workers.ts`: worker roll-up shaping/formatting shared by the thread list, the Agent
  summary and the Workers pane. `failed` is always its own bucket — folding it into `settled` hides
  the one state the reader has to act on. Empty buckets are omitted, and a roll-up whose workers are
  all lineage-only reads `N workers · status unavailable` rather than `N unknown`.
- `src/lib/demo.ts`: **DEV-only** fixtures behind `?demo=1`; dropped from prod builds
- `src/components/`: `Collapse`, `Markdown`, `DiffView`, `Drawer`, `TabStrip`, `Icon`,
  `WorkGroup` (the "Worked for 5m" turn divider), `GroupLine` (folded run), `ToolLine` (one call),
  `ArtifactAttachments` (inline image/audio/file-card rows under a message),
  `ArtifactPreview` (full-screen preview overlay; HTML goes through a sandboxed
  iframe with an opaque origin, never the app's credentials)
- `src/views/ThreadView.svelte`: thread shell — owns the thread fetch + tab strip, renders one pane.
  Shows a Telegram jump button when the response carries `telegram_link`; the link is built
  server-side from the *resolved* id, so it works for `?id=active` too.
- `src/views/TranscriptPane.svelte` / `TrajectoryPane.svelte` / `AgentPane.svelte` / `ChangesPane.svelte` / `WorkersPane.svelte` / `ArtifactsPane.svelte`: the thread tabs.
  `ArtifactsPane` lists the shared `zdx-engine::core::artifacts` projection (`/api/threads/{id}/artifacts`)
  with preview/open, download, and Go to message; `TranscriptPane` renders the same rows inline
  (images, audio players, compact file cards) from the message `artifacts`.
  Artifact bytes come from `/api/threads/{id}/artifacts/file?path=…` as authenticated
  `Blob`s — never as credentialed iframe URLs.
  `TrajectoryPane` renders the shared `zdx-engine::core::thread_trajectory` projection returned by
  `/api/threads/{id}/trajectory`; it owns presentation only, not timing arithmetic. It is mobile-first:
  the bottleneck summary stays readable at phone width, exact spans
  use horizontally scrollable Input/Model/Tools lanes with pinned labels, and selected spans open in a
  bottom sheet. Legacy duration-only events rank as measured durations but are never positioned as
  inferred overlap.
  `ChangesPane` owns a scope selector — **All Changes** / **Uncommitted** / a specific commit.
  `uncommitted` renders the status groups already in `GitResponse`; the history scopes fetch
  `/api/git/scope` and render one flat file list. `All Changes` is the branch's contribution over
  the repo's main branch (PR-diff semantics) plus local edits; the selector subtitle names the base
  (`vs master · 3 commits`) so it is obvious that on main it equals Uncommitted. Untracked files
  keep `kind=untracked` even inside `all`, because they are not part of any diff. The pane takes
  `sharedRepo` and then says so in the header: the diff is resolved from the *thread's repository
  root*, so it is not agent-attributed and several workers on one root see each other's edits.
- `src/views/WorkersPane.svelte`: workers of an orchestrator thread. Rows open the worker in-app;
  a worker with a mirror topic also gets a Telegram jump button. Rows come from two sources and
  **must not be rendered alike**: a `live` row is backed by the in-memory `WorkerManager`, while a
  row with `live: false` was recovered from persisted lineage after a restart and knows only that
  the worker existed. Those render dashed, as `status unavailable`, never as settled.
- `src/views/MonitorView.svelte`: section-aware monitor (`overview` renders everything). Subscription quotas arrive **after** the rest of the dashboard — the server refreshes them off the request path — so render from `subscriptions_status`, not from list length: `pending` shows the section as loading, `stale` shows the values with their age, and the overview quota tile reads `—` rather than `0%` until real windows exist.
- `src/views/ThreadListView.svelte`: recent-thread browser (`?view=threads`). Each row opens the
  thread in-app; threads bound to a Telegram topic also get a jump button that calls
  `openTelegramLink` with the `t.me/c/<internal_id>/<topic_id>` link built server-side. TUI/CLI
  threads and plain DMs have no linkable topic, so they show the in-app open only. A row whose
  `workers` roll-up is non-null is an orchestrator: it gets an `orch` badge and a counts line. Keep
  that line in normal block flow — it is long enough to need two lines on a phone, and `truncate`
  (or a single flex child, which cannot break) clips it mid-word.

## Transcript rendering

Agent activity is hidden by default and expands on demand, in three levels:

1. **`WorkGroup`** — everything between two assistant messages collapses to a rule-flanked
   `Worked for 5m ›` divider. Auto-expands while a tool is running.
2. **`GroupLine`** — consecutive related calls fold into one line: read-only tools become
   `Explored 8 files, 3 searches`, repeated shell calls become `Ran 7 commands`.
3. **`ToolLine`** — a single call, expanding to full arguments and output.

Summaries are derived from real tool payloads in `summarize()`. `edit` carries `old_string`/
`new_string`, so `+N -M` line deltas are computed client-side; `bash` failure is detected from
`output.data.exit_code`, not just `tool_result.ok`. When adding a tool, add a case there.

**Keep activity rows borderless and muted.** The prose answer is the focus; boxes and colour on
every tool call defeat the whole point. Only diff stats and failures get colour.

The transcript opens scrolled to the newest message and follows new content only while the reader is
already at the end; otherwise a floating jump-to-latest button appears with a dot for unseen output.
The scroll effect tracks the node count and reads scroll state via `untrack` — tracking `atBottom`
makes the effect re-run on every scroll event and fight the reader.

Lists rendered from API payloads are keyed by **index**, not by a field value. They are positional
and replaced wholesale on each refresh, so identity keys buy nothing and a duplicate value aborts the
render (a provider can report two quota windows both labelled `weekly`). `App.svelte` wraps the views
in `<svelte:boundary>` so such a failure shows the error instead of a permanent "Loading…".

## Navigation

There is no bottom tab bar. Navigation is a left drawer (shadcn-style
`data-sidebar="group|menu|menu-item"` structure) opened from the header, plus a horizontally
scrollable `TabStrip` inside the thread view.

Thread tabs are data-driven in `ThreadView.svelte` — adding a pane is a one-line entry there plus a
value in `ThreadTab`/`THREAD_TAB_LABELS` and an icon in `TAB_ICONS`. **Artifacts** sits right after
the Thread tab: it is secondary, but the strip scrolls horizontally, so anything appended last
starts offscreen on a phone.

**Workers** is conditional: it renders only when `ThreadResponse.worker_count > 0`, and it is placed
**first** in the strip. The strip scrolls horizontally, so a tab appended last starts offscreen on a
phone — which for an orchestrator's primary object is the same as not shipping it. A deep link to
`?tab=workers` on a thread with no workers falls back to the transcript rather than rendering a tab
that is not in the strip.

`ThreadView` owns **both** the thread and the git fetch. Git lives there rather than in `ChangesPane`
because the tab strip needs to know the tree is dirty before that pane is ever opened, and `AgentPane`
shows the working folder and branch; passing it down keeps it to one request either way. `git status`
shells out, so it refreshes on mount, on manual refresh, and when a turn finishes — never on the 4s
transcript poll.

`AgentPane` mirrors the bot's `/status`: folder, branch, model, thinking, usage totals and **context
occupancy as a percentage**. Two subtleties, both load-bearing:

- Context uses the last usage event that actually carries prompt tokens. Providers emit a prompt
  event (input + cache) and a separate completion event (output only); the plain last event is
  usually ~0 and reads as a meaningless `0%`.
- The percentage needs the model's context window, which no client should hardcode, so `server.rs`
  attaches `context_limit` to each usage event from the model registry.

Icons are inline Lucide paths (ISC) vendored in `Icon.svelte` rather than a dependency — we need a
handful. Stroke width is 1.75, not Lucide's default 2: at 14px the default reads too heavy beside
13px text. Add new glyphs to the `PATHS` registry.

## Design system

The mechanism matters as much as the values: **every colour is a `light-dark()`
pair resolved by `color-scheme`**, which `lib/telegram.ts` sets from Telegram's
`colorScheme`. That means no `dark:` variants anywhere — do not add them.

Fonts: `--font-sans` is the plain system stack, which is also what Telegram
renders natively. `--font-mono` is the platform mono stack; no font files are
vendored.

Token values, rationale and the interaction principles behind the transcript
design live in `.zdx/design-reference/` — local only, gitignored, not shipped.

## Conventions

- Keep `src/lib/types.ts` matching `server.rs` exactly — field names are the JSON keys, no renames.
- The API is read-only and every route is GET. Do not add write calls until the Rust side grows them.
- Poll only when something is actually live (`tool_running`) and only when `document.visibilityState === "visible"`.
  The thread `cursor` counts source events, not visible `activity` rows: private meta and empty events create
  sequence gaps. Never derive the next cursor or a live tool's sequence from `activity.length`; a collision
  makes the delta merge drop the live row.
  **Exception — discovery.** A view whose job is to notice work *appearing* cannot gate its refresh on
  "something is already running", and must not gate the *first* fetch on a page-load count either: a
  thread becomes an orchestrator by spawning its first worker, so `worker_count > 0` from the thread
  load is exactly the condition that is false when discovery matters. `ThreadView` therefore fetches
  and re-fetches workers whenever the Workers **or** Agent tab is open and the document is visible
  (4s while a worker is busy, 10s otherwise), and derives "has workers" from the fetched list as well
  as the count. This reads one small endpoint; it never polls the transcript or shells out to git for
  worker status. Background refresh failures are swallowed like the transcript poll — only the first
  load surfaces an error — and the spinner shows only on that first load, so a zero-worker thread
  keeps reading "No workers yet." instead of blinking.
- Prefer plain CSS custom properties over new Tailwind config; the token layer is the source of truth.
- No `dark:` variants — use the `light-dark()` tokens.
- Syntax highlighting is intentionally line-local and approximate: diff hunks are fragments, so a
  stateful lexer would mis-colour more than it fixes. Add languages to `BY_EXT` in `highlight.ts`
  rather than reaching for a highlighting library.

## Build / run

- `just web-demo` — dev server with fixture data, no bot needed (`?demo=1`)
- `just web-dev` — dev server proxying `/api` to a running `zdx bot` on `:4141`
- `just web-check` — `svelte-check`
- `just web-build` — production build into `apps/web/dist/`; skips Bun install/Vite when web input contents (including configs, lockfile, `.env*`, Bun version, `VITE_*` and `NODE_ENV`) and the complete dist contents match the last successful build. No-op builds preserve dist timestamps so Cargo stays fresh. `node_modules`, `dist`, `.git`, and `AGENTS.md` are excluded from input hashing.
- `bun test apps/web/build-cached.test.mjs` — build cache regression tests (from the workspace root)

Working against the real API in a desktop browser needs signed initData: put a
real one in `apps/web/.env.local` as `ZDX_DEV_INIT_DATA=...`.

## Serving

`crates/zdx-bot/src/server.rs` embeds `apps/web/dist` with `rust-embed`, so **the binary is
self-contained but `dist/` must exist before any cargo build of `zdx-bot`** — a missing folder is a
compile error, not a silent empty UI. `just build-release` and both GitHub workflows run the web
build first; a bare `cargo build` on a fresh clone needs `just web-build` once.

Cache policy: `/app` (the SPA entry) is `no-cache` so new content hashes are picked up;
`/app/assets/*` is `immutable` for a year. That HTTP cache is the only caching layer available —
service workers do not work in Telegram's iOS WebView.

## Maintenance

- Add/move/delete files here: update this file.
- Change the API contract in `server.rs`: update `src/lib/types.ts` and `src/lib/api.ts`.
