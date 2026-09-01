# zdx Mini App (apps/web)

Svelte 5 + Vite + Tailwind v4 frontend for the zdx Telegram Mini App.

Consumes the read-only JSON API served by `crates/zdx-bot/src/server.rs`, which also embeds this
app's build output and serves it at `/app`. This is the Mini App — the old vanilla `app.html` shell
has been removed.

## Where things are

- `src/main.ts`: entrypoint; initializes the Telegram bridge before mounting
- `src/App.svelte`: framed app shell (shell colour behind, rounded surface panel) + drawer host
- `src/app.css`: the whole design system — Tailwind v4 `@theme` tokens
- `src/lib/telegram.ts`: `window.Telegram.WebApp` bridge (scheme, safe areas, haptics, back button, `start_param`)
- `src/lib/api.ts`: typed fetch client; sends `Authorization: tma <initData>`
- `src/lib/types.ts`: response types mirroring `server.rs` — keep in sync when the Rust structs change
- `src/lib/router.svelte.ts`: query-string router. Canonical: `?view=thread&id=…&tab=…` and
  `?view=monitor&section=…`. Also accepts the bot's legacy `?view=threads`, `?view=git` and bare
  `startapp=<thread_id>` links and normalizes them — do not break those.
- `src/lib/transcript.ts`: projects the flat activity stream into a three-level collapsible tree
  (`work` turn → folded `group` → single `tool`) and derives the one-line summaries
- `src/lib/diff.ts`: unified-diff parser with word-level intra-line segmentation
- `src/lib/highlight.ts`: compact line-local syntax tokenizer + span merger (six `--hljs-*` classes)
- `src/lib/markdown.ts`: `marked` + allowlist sanitizer (model output is untrusted)
- `src/lib/demo.ts`: **DEV-only** fixtures behind `?demo=1`; dropped from prod builds
- `src/components/`: `Collapse`, `Markdown`, `DiffView`, `Drawer`, `TabStrip`, `Icon`,
  `WorkGroup` (the "Worked for 5m" turn divider), `GroupLine` (folded run), `ToolLine` (one call)
- `src/views/ThreadView.svelte`: thread shell — owns the thread fetch + tab strip, renders one pane
- `src/views/TranscriptPane.svelte` / `AgentPane.svelte` / `ChangesPane.svelte`: the thread tabs
- `src/views/MonitorView.svelte`: section-aware monitor (`overview` renders everything)

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
value in `ThreadTab`/`THREAD_TAB_LABELS` and an icon in `TAB_ICONS`. A "Files" tab is intentionally
absent: it needs a repo file-listing endpoint that `server.rs` does not expose yet.

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
- Prefer plain CSS custom properties over new Tailwind config; the token layer is the source of truth.
- No `dark:` variants — use the `light-dark()` tokens.
- Syntax highlighting is intentionally line-local and approximate: diff hunks are fragments, so a
  stateful lexer would mis-colour more than it fixes. Add languages to `BY_EXT` in `highlight.ts`
  rather than reaching for a highlighting library.

## Build / run

- `just web-demo` — dev server with fixture data, no bot needed (`?demo=1`)
- `just web-dev` — dev server proxying `/api` to a running `zdx bot` on `:4141`
- `just web-check` — `svelte-check`
- `just web-build` — production build into `apps/web/dist/`

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
