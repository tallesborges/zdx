# zdx Mini App (apps/web)

Svelte 5 + Vite + Tailwind v4 frontend for the zdx Telegram Mini App.

Consumes the JSON API served by `crates/zdx-bot/src/server.rs` (read-only except artifact feedback), which also embeds this
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
- `src/lib/annotate.ts`: artifact annotation — the bridge injected into previewed HTML (its source is a string here; it runs after the doctype, removes itself, builds messages with object spread rather than page-patchable helpers, and only answers parent requests; text selection is no exception: in Browse the annotator asks for it every 400 ms and only the user's Add note turns an answer into a draft), validation of its untrusted `postMessage` events (the per-load nonce filters stray messages; it does not make a report trusted; page strings are cut with `headUnits`/`tailUnits`, never inside a surrogate pair, and made `wellFormed`: a lone surrogate in the JSON makes the server refuse the whole submission), mark geometry, drawing sketch rendering, media helpers, and the feedback payload (mirrors `zdx_engine::core::artifact_feedback::FeedbackSubmission`). No `$lib` imports, so `annotate.test.mjs` can load it under `bun test`
- `src/lib/versionMatch.ts`: Update to a newer version of an open artifact: the `inspect` request built from the notes' own selectors, the conservative per-note checks (element with markup, text quote with context, drawing boxes and width, byte-identical image copy, page), `rangeRequests` for the text notes' matched words, which the bridge measures (`range-rects`) so a moved note gets its real box, all-or-nothing `carryOver` that keeps `carriedFrom`. Pure; no `$lib` imports; tested by `version-match.test.mjs` (`bun test apps/web/version-match.test.mjs`). `ArtifactAnnotator` polls `api.artifactVersion` every 30 s and on focus/visibility while the preview is open, checks the new page in a hidden second frame with its own bridge nonce, starting only on the answer to `settle`, sent after that frame's load event and answered once its fonts are ready (the bridge's early `ready` is ignored there; 8 s limit), and accepts a measured box only when the measured words equal the quote (`acceptMeasured`), and never reloads or rebinds notes without the user's Update; Stay on this version and Update anyway never remove a note
- `src/lib/draftStore.ts`: unsent annotation drafts (media as Blobs) in IndexedDB, one record per Telegram user, thread, and artifact path, with the annotated SHA-256 and the submission id; in-memory when IndexedDB is unavailable, and `saveDraftRecord` returns `false` so the UI never claims the notes are saved. It resolves only when the transaction commits: `ArtifactAnnotator` shows `saving` until then (`data-storage` on the status text; browser tests wait for `saved` before reloading), and only the latest write sets the status
- `annotate.test.mjs`: bridge injection, message-validation, and text-note tests (`bun test apps/web/annotate.test.mjs`)
- `src/lib/imageMarks.ts`: image artifact annotation view geometry (formats, limits, and orientation are the server's: the image is the engine's canonical copy, shown as is): `fitRect` (contain, letterboxed), an image-relative `ImageView` (zoom + image point at the stage centre) clamped by `viewRect`, `zoomAround`/`panBy`, stage↔image mapping, and stroke clamping/thinning, `revealBounds` (smallest pan that brings the edited mark back into view after the stage resizes; zoom unchanged), `badgePosition` (number clamped to the image box, not the stage, so it never sits away from its mark). Marks are fractions of the canonical copy, never stage pixels; that is what keeps them attached through zoom, resize, and rotation. No `$lib` imports
- `image-marks.test.mjs`: letterbox, zoom/pan clamping, resize/rotation, stroke clipping, and image payload tests (`bun test apps/web/image-marks.test.mjs`)
- `src/lib/types.ts`: response types mirroring `server.rs` — keep in sync when the Rust structs change
- `src/lib/router.svelte.ts`: query-string router. Canonical: `?view=threads`, `?view=thread&id=…&tab=…`
  and `?view=monitor&section=…` (sections include `workspaces`). The bot's legacy `?view=threads&id=…`, `?view=git` and bare
  `startapp=<thread_id>` links are normalized — do not break those. `?view=threads` **with** an `id`
  stays a single-thread link for that reason; only the bare form opens the list. A file's `Open artifact`
  link is `startapp=a_<key>_<thread_id>`, read from initData `start_param`, else the `tgWebAppStartParam`
  query parameter (parsed in `src/lib/artifactLink.ts`, no `$lib` imports, tested by
  `artifact-link.test.mjs`): it opens the thread's Artifacts tab, and `ArtifactsPane` takes the pending key
  once (`router.takeLinkedArtifact`, which also drops `artifact=<key>` from the URL) and opens that row's
  preview, or a notice when the key is not listed or the file is gone. The pending link is a plain field,
  not `$state`: changing `router.current` re-runs `ThreadView`'s load (its props are getters on the route),
  which remounts the pane and would drop the preview. Only explicit thread ids, never `active`.
- `src/lib/transcript.ts`: projects the flat activity stream into a three-level collapsible tree
  (`work` turn → folded `group` → single `tool`) and derives the one-line summaries
- `src/lib/diff.ts`: unified-diff parser with word-level intra-line segmentation
- `src/lib/highlight.ts`: compact line-local syntax tokenizer + span merger (six `--hljs-*` classes), used by the diff view and text artifact previews
- `src/lib/markdown.ts`: `marked` + allowlist sanitizer (model output and Markdown artifacts are untrusted). It parses with `DOMParser`, never `innerHTML` on a live-document element: that starts image loads and fires their `onerror` before the sanitizer runs (verified in Chromium)
- `src/lib/textPreview.ts`: text artifact preview limits (fetch up to 10 MiB, render the first 256 KiB, syntax colours up to 3000 lines), UTF-8 decoding of the cut (NUL byte = not text), and line splitting. No `$lib` imports; tested by `text-preview.test.mjs` (`bun test apps/web/text-preview.test.mjs`)
- `src/lib/time.ts`: quota reset formatting — `resets in 2h 14m` countdown plus a local clock time
  (`today 14:30`, `tomorrow 09:00`, `Thu 14:30`). Both take `now` explicitly.
- `src/lib/run.ts`: formatting for the shared live run status (`formatSecs`, `runAges`)
- `src/lib/usage.ts`: model and context occupancy read from a thread's usage events (`lastUsage`, `contextOccupancy`), shared by the Agent tab and the menu's thread card
- `src/lib/monitorSummary.ts`: Monitor figures shown in more than one place (`servicesLabel`, `peakQuota`, `jobFailed`); all cover all of zdx
- `src/lib/currentThread.svelte.ts`: what the open `ThreadView` already fetched (title, live `run`, worker count and list, git, model, context %), published for the menu's thread card so opening the menu costs no request for it. Unfetched parts stay null and the card leaves them out
- `src/lib/artifactList.ts`: Artifacts tab order (newest / oldest / name; rows without a time last) and local day groups and labels. No `$lib` imports; tested by `artifact-list.test.mjs` (`bun test apps/web/artifact-list.test.mjs`)
- `src/lib/workers.ts`: worker roll-up shaping/formatting shared by the thread list, the Agent
  summary and the Workers pane. `failed` is always its own bucket — folding it into `settled` hides
  the one state the reader has to act on. Empty buckets are omitted, and a roll-up whose workers are
  all lineage-only reads `N workers · status unavailable` rather than `N unknown`.
- `src/lib/demo.ts`: **DEV-only** fixtures behind `?demo=1`; dropped from prod builds. Artifact bytes can be
  replaced per path from a browser test with `window.__zdxDemoArtifacts = { [path]: Blob }`
- `src/components/`: `Collapse`, `Markdown`, `DiffView`, `TextView` (text and code with line numbers, highlighting, and a Wrap toggle that defaults on for plain text), `Drawer`, `TabStrip`, `Icon`,
  `RunStatusLine` (the running turn's label, turn age, last-event and heartbeat ages under the thread header),
  `WorkGroup` (the "Worked for 5m" turn divider), `GroupLine` (folded run), `ToolLine` (one call),
  `ArtifactAttachments` (inline image/audio/file-card rows under a message),
  `ArtifactPreview` (full-screen preview overlay; the `markdown` kind renders through `Markdown` with a Preview/Source switch, the `text` kind through `TextView`; HTML goes through a sandboxed
  iframe with an opaque origin, never the app's credentials; an image goes to the annotator as its
  canonical copy from `api.artifactImage` (`/artifacts/image`; identity from `X-Zdx-*` headers); a `422`
  (format, or over the 25 MiB / 40 MP limits on originals) shows the original as a passive `<img>` with the
  server's reason, plus a line when unsent notes for it are kept in the draft store),
  `ImageStage` (the image branch of the annotator: the `<img>` placed by `viewRect`, an SVG overlay that
  redraws marks from image fractions on every view change, pans the `focus` mark (the one being edited, or unfinished strokes) back into view when the stage resizes, moves Fit to the other top corner when a mark or number is under it, pinch/pan/double-tap/wheel zoom, Tap → pin,
  Draw → strokes; a second finger always turns the gesture into a pinch and drops the stroke in progress),
  `ArtifactAnnotator` (HTML and image artifacts; the editor, notes list, drafts, stale handling, and Send
  are shared, and for images the stage is `ImageStage`, with no bridge, pins instead of the element tree,
  the server's source hash as the version (the copy is not hashed here), the copy's decode checked against
  the named size, and the copy's hash and size sent as `image`. When the original is larger than the copy, a line says the
  user annotates an optimized copy (sizes from `sourceWidth`/`sourceHeight`). Drafts store that frame (`DraftRecord.frame`);
  reopening on another copy is stale. Snapshot `$state` objects before IndexedDB or the payload (a proxy
  fails the structured clone and the draft silently stays in memory only). For HTML: Browse / Tap / Draw modes
  (Browse keeps native text selection; a docked bar at the bottom of the stage, over the frame rather
  than resizing it, shows the quote and Add note, and holds through the tap even if the frame drops
  its selection when focus leaves it), the Tap selection
  sheet (highlight + dimming drawn by the bridge, a local ancestor/child tree addressed by
  bridge-local node ids that are only valid until the next answer, Select parent, and an explicit
  Comment on this; the bridge walks the tree with prototype getters captured before page scripts
  run, the parser drops repeated node ids, and tree lists are keyed by index), the note editor
  with intent, comment, voice, and images, the notes list, stale-version and lost-bridge banners, and
  Send). Keep `sandbox="allow-scripts"` without `allow-same-origin`: the app never reads the page
  DOM, it talks to the injected bridge. In Tap and Draw the parent covers the frame and owns input,
  so page handlers never fire; the frame only answers requests the parent is waiting for, so a page
  script cannot create a draft or change a selection. Never use `scrollIntoView` for page or sheet
  reveals: it also scrolls the app around the frame (seen in Chromium); scroll the specific box. Send the frame geometry, ids, and mode only — never comments, media,
  or ids of the thread. Snapshot state before `postMessage` (`$state` proxies cannot be cloned, and
  the throw takes down the whole view). Drafts hold Blobs, not object URLs, so they can be stored;
  the component owns the URLs. Toolbar mode changes go through `requestMode`, which asks before leaving Draw with unfinished strokes; Send is disabled while they exist (never cleared silently). A send freezes the drafts (no edit, delete, new note, or mode change)
  and success removes only the submitted ids, so the remembered `submission_id` always names exactly
  what the server got. `ArtifactPreview` asks before closing over an open unsaved note (`unsaved`
  binding, `confirmAction`).
- `src/views/ThreadView.svelte`: thread shell — owns the thread fetch + tab strip, renders one pane.
  Shows a Telegram jump button when the response carries `telegram_link`; the link is built
  server-side from the *resolved* id, so it works for `?id=active` too.
- `src/views/TranscriptPane.svelte` / `TrajectoryPane.svelte` / `AgentPane.svelte` / `ChangesPane.svelte` / `WorkersPane.svelte` / `ArtifactsPane.svelte`: the thread tabs.
  `ArtifactsPane` lists the shared `zdx-engine::core::artifacts` projection (`/api/threads/{id}/artifacts`),
  newest first as sent, under day headers with the time on each row and an optional sort chip (`lib/artifactList.ts`),
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
- `src/views/MonitorView.svelte`: section-aware monitor (`overview` renders everything). Subscription quotas arrive **after** the rest of the dashboard — the server refreshes them off the request path — so render from `subscriptions_status`, not from list length: `pending` shows the section as loading, `stale` shows the values with their age, and the overview quota tile reads `—` rather than `0%` until real windows exist. Each quota window shows its `resets_at` as a countdown and a local time, computed against a client `now` ticked with the 30s poll — the snapshot can be minutes old, so never measure against its fetch time. A window without `resets_at` shows no reset line rather than a guess. The Jobs section renders the shared `zdx_engine::jobs::JobSummary` list (`jobs`, plus `jobs_error` when jobs cannot be listed at all): schedule, status, next run, and last outcome come from the server; the view only formats them, and a row opens its last run's thread when it has one.
- `src/views/WorkspacesPane.svelte`: `?view=monitor&section=workspaces` — workspace chips, then each model setting with its origin (`set here` highlighted, `from <ancestor>`, `global`, `default`) and, for own-overlay overrides, what a reset falls back to. Read-only; `MonitorView` fetches `/api/workspaces` for this section instead of `/api/monitor`, does not poll it, and `overview` does not include it.
- `src/views/ThreadListView.svelte`: recent-thread browser (`?view=threads`). Each row opens the
  thread in-app; threads bound to a Telegram topic also get a jump button that calls
  `openTelegramLink` with the `t.me/c/<internal_id>/<topic_id>` link built server-side. TUI/CLI
  threads and plain DMs have no linkable topic, so they show the in-app open only. A row whose
  `workers` roll-up is non-null is an orchestrator: it gets an `orch` badge and a counts line. Keep
  that line in normal block flow — it is long enough to need two lines on a phone, and `truncate`
  (or a single flex child, which cannot break) clips it mid-word. An `All` / `Workers` chip row
  filters server-side (`api.threads("workers")` → `/api/threads?kind=workers`); rows with
  `is_worker` get a `worker` badge. The worker rule lives in the engine — never re-derive it here
  from `parent`/`origin` fields.

## Transcript rendering

Agent activity is hidden by default and expands on demand, in three levels:

1. **`WorkGroup`** — everything between two assistant messages collapses to a rule-flanked
   `Worked for 5m ›` divider. Auto-expands while a tool is running.
2. **`GroupLine`** — consecutive related calls fold into one line: read-only tools become
   `Explored 8 files, 3 searches`, repeated shell calls become `Ran 7 commands`.
3. **`ToolLine`** — a single call, expanding to full arguments and output.
   A `tool_running` row with `completed_at` is a call that finished while its round is still running;
   `ToolLine` shows its outcome and duration until the persisted `tool_use`/`tool_result` replace it.

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
scrollable `TabStrip` inside the thread view. The drawer does not list thread tabs (the strip has
them). It shows, in order: a "This thread" card while a thread view is open (from
`currentThread`: run state, model and context %, branch and changes when git was fetched, the live
run label, and the first three workers, failed first), the three most recent other threads, and
"All of zdx" (one full-width row per Monitor section: Overview, Active agents, Services, Quota,
Jobs, Workspaces, Config; figures only from `/api/monitor`, none for Workspaces and Config). Keep the scope labels: Monitor figures are global, the card is the thread's own. Opening
the drawer reads `/api/threads` and `/api/monitor` once (and the workers once when the view has not
loaded them); it never polls.

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
- The API is read-only except `POST /api/threads/{id}/artifacts/feedback` (`api.sendArtifactFeedback`). Do not add other write calls until the Rust side grows them. Write calls take the concrete thread id (`ThreadView` passes `artifactThreadId`: for a legacy `?id=active` link, the first concrete id the thread load resolved, never `active`).
- Poll only when something is actually live (`run` set, a `tool_running` row, or a pending delivery) and only when `document.visibilityState === "visible"`. Gate on `run`, not on `tool_running` alone: between tools the model thinks or waits, and a view that stops polling then misses the next tool.
- Live run status (`RunStatus`: state, label, ages) is derived on the server by `zdx_engine::agent_activity`; `src/lib/run.ts` only formats it. Never label a run stuck from its ages: a quiet build is healthy, and a fresh heartbeat shows only that the process is alive.
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
