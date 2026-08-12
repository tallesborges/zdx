> Stage: active. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals

- Model authors replies in plain Markdown; the bot deterministically converts Markdown → Telegram HTML at send time.
- Telegram messages render correctly (bold, italic, inline code, code blocks, links, blockquotes, bullet lists) without depending on the model remembering an HTML contract.
- The same thread opened in the TUI/monitor transcript renders the same content, including code blocks and blockquotes, instead of silently dropping them.
- Stray `<` and `&` in prose stop breaking messages, by construction rather than by post-failure repair.

# Non-goals

- Removing `html::sanitize` (`crates/zdx-bot/src/telegram/html.rs`). It stays, but see Contracts: it is a post-rejection repair, not a pass every message goes through.
- Migrating bot-authored HTML (status message, pickers, launcher, keyboards, retry) to Markdown. Those remain deterministic HTML; the compatibility review only corrected four invalid error wrappers from `<blockquote><code>` to `<pre>`.
- Changing the `<followups>` / `<media>` / `<medias>` control-tag contract or their parsing order.
- Rendering Markdown tables in Telegram chat. Telegram has no table primitive; complex layout keeps going to an HTML artifact file.
- Migrating existing threads that already contain HTML-authored assistant text.
- Enabling new Markdown features in the TUI transcript renderer (`crates/zdx-transcript/src/markdown/parse.rs`).
- Generated forum topic titles (`crates/zdx-bot/src/topic_title.rs:23-36`). Not a message-body path, does not use the Telegram instruction layer, sent via `edit_forum_topic`.
- Entity-aware message splitting for the 4096-char limit. Normal replies keep relying on the prompt's under-3500 guidance.

# Design principles

- User journey drives order.
- Reuse before rebuild: `pulldown-cmark` 0.13 is already a workspace dependency (root `Cargo.toml:74`, used by `zdx-transcript` and `zdx-tui`); the converter is driven off its event stream rather than regex.
- One choke point: all visible model reply text already flows through `parse_final_response` (`crates/zdx-bot/src/handlers/message/media.rs:130-148`).
- Markdown is the storage format; HTML is a per-client rendering detail applied at the edge.
- The converter must emit **Telegram-valid** entity nesting, not just balanced tags. `html::sanitize` repairs syntax only (`telegram/html.rs:37-103`); semantically forbidden but balanced nesting passes through it unchanged, and when `sanitized == input` the ladder skips the retry and resends as plain text with literal tags visible (`telegram/mod.rs:598-610`).

# User journey

1. User sends a message to the bot in a Telegram topic.
2. The agent replies using ordinary Markdown.
3. Telegram shows correctly rendered formatting: bold labels, inline code, fenced code blocks, links, quotes, bullet lists.
4. User later opens the same thread in the TUI/monitor transcript and sees the same content, with code blocks and quotes intact.

# Foundations / Already shipped (✅)

## Single choke point for model reply text

- What exists: `parse_final_response` strips `<followups>` (via `zdx_engine::followups::extract_followups`), strips `<medias>` wrappers, extracts `<media>` paths, then calls `normalize_reply_text` (`media.rs:130-148`). Every visible send/edit path for the final reply consumes its output: cross-topic send (`response.rs:68-86`), status-message edit (`response.rs:91-101`), send fallback (`response.rs:103-140`).
- ✅ Demo: `cargo nextest run -p zdx-bot` — existing `parse_final_response` tests live inline in `crates/zdx-bot/src/handlers/message/mod.rs:314-390`.
- Gaps: `normalize_reply_text` collapses consecutive blank lines globally and trims the payload (`response.rs:149-167`), which is unsafe around code content in either order. The converter must take over block spacing.

## Reactive HTML repair ladder

- What exists: `html::sanitize` escapes stray `<`/`&`, drops unsupported tags, balances crossed/unclosed tags (`telegram/html.rs:37-155`). Wired as HTML → sanitize retry → plain text in `send_message_with_html_fallback` (`telegram/mod.rs:586-610`) and `edit_message_text` (`telegram/mod.rs:648-674`).
- ✅ Demo: `cargo nextest run -p zdx-bot html` — 6 unit tests at `telegram/html.rs:160-212`.
- Gaps: runs **only after Telegram reports a parse error**. Markdown is a valid HTML payload, so it is accepted and rendered literally; the ladder never fires. It also accepts arbitrary attributes on an allowed tag as long as they contain no `<` (`html.rs:110-129`), so the converter must escape its own attributes.

## Telegram-only instruction layer

- What exists: `crates/zdx-assets/instruction_layers/telegram_instruction_layer.md` (82 lines), embedded at `crates/zdx-assets/src/lib.rs:43-44`, re-exported at `crates/zdx-engine/src/prompts.rs:19-20`, installed into `BotContext` at `crates/zdx-bot/src/lib.rs:108-121`, composed into the turn prompt at `crates/zdx-bot/src/handlers/message/turn.rs:109-117` → `crates/zdx-bot/src/agent/mod.rs:95-122`.
- ✅ Demo: no non-bot consumer exists — TUI uses `CHAT_INSTRUCTION_LAYER` (`crates/zdx-tui/src/lib.rs:30-37`), CLI exec uses `EXEC_INSTRUCTION_LAYER` (`crates/zdx-cli/src/modes/exec.rs:19-27`).
- Gaps: the HTML contract to rewrite is not confined to one section — `## Length and formatting` (lines 16-23) and `## Examples` (58-82) carry most of it, but the intro (line 3) and two lines inside `## Detailed answers and file uploads` (47, 50) also assume HTML authoring.

## TUI transcript Markdown renderer

- What exists: `pulldown-cmark` parse + styled-line rendering for headings, bold, italic, inline code, fenced code blocks with language, links, blockquotes, bullet/ordered/nested lists, and tables (`crates/zdx-transcript/src/markdown/parse.rs:411-603`, `692-797`).
- ✅ Demo: open a thread in `just monitor` and confirm a Markdown-authored message renders styled.
- Gaps: parser enables only `Options::ENABLE_TABLES` (`parse.rs:41-45`); `Event::Html`/`InlineHtml` are dropped (`parse.rs:418-425`); soft breaks become a space in the finalized path (`parse.rs:552-565`, used for finalized assistant cells at `zdx-transcript/src/cell.rs:905-926`); strikethrough tags render as plain style (`parse.rs:467-475`); link URLs are discarded (`parse.rs:500-502`).

# MVP phases (ship-shaped, demoable)

## Phase 1: Markdown-first replies (converter + prompt flip, shipped together) — implementation complete 2026-08-11; live demo pending

- **Goal**: the model writes Markdown and both Telegram and the TUI render it correctly. Converter and prompt flip ship as one phase — a pulldown-cmark parse/re-emit is **not** a no-op on today's HTML-authored replies (it interprets their leaked Markdown, decodes and re-encodes entities, and reconstructs whitespace), so a converter-only phase would change output while the prompt still asks for HTML.
- **Scope checklist**:
  - [x] Add `crates/zdx-bot/src/telegram/markdown.rs` exposing `to_telegram_html(&str) -> String`, built on `pulldown_cmark::Parser::new_ext` with `Options::ENABLE_TABLES` only, matching the transcript renderer (`parse.rs:41-45`).
  - [x] Add `pulldown-cmark.workspace = true` to `crates/zdx-bot/Cargo.toml`.
  - [x] Map to Telegram's supported subset: `Strong`→`<b>`, `Emphasis`→`<i>`, `Code`→`<code>`, `Link`→`<a href>`, `BlockQuote`→`<blockquote>`, `CodeBlock`→`<pre><code class="language-x">…</code></pre>` (Telegram's documented form — never a language attribute on `<pre>`; extract and validate the first info-string token, omit on invalid/empty).
  - [x] Enforce Telegram's entity-nesting rules, since `sanitize` cannot repair semantically invalid nesting: flatten nested blockquotes; suspend blockquotes around links, inline code, and code blocks; do not nest formatting inside `code`/`pre`; do not nest links; suppress the outer entity (or close/reopen) for `**bold with `code`**` and `[text with `code`](url)`.
  - [x] Lower what Telegram cannot render: headings → bold line; bullet/ordered items → flat `-` / `N.` lines with two-space-per-level indent for nesting (mirroring `parse.rs:759-760`); `Rule` → separator; tables → plain lines; images and unhandled events → explicit, documented behavior.
  - [x] Define the whitespace contract explicitly and match the finalized transcript path: `SoftBreak` → space (`parse.rs:552-565`), `HardBreak` → newline, paragraph/block end → single blank line, with separator deduplication so blocks never accumulate 3+ newlines. Code indentation and internal blank lines are preserved; the fence-separating terminal newline is omitted.
  - [x] HTML-escape every `Event::Text`, raw HTML event, code payload, and link `href` as an attribute value. Note pulldown already decodes source entities, so text is escaped exactly once. If parser raw-HTML events contain a recognized Telegram tag, bypass conversion for the whole legacy HTML payload.
  - [x] Replace `normalize_reply_text` in the pipeline: feed post-control-tag text straight to the converter and let the converter own spacing. Collapsing blank lines before conversion corrupts fenced code; after conversion it corrupts `<pre>`.
  - [x] Normalize followup button labels (strip inline-code backticks and emphasis markers) for display only, keeping the original value as the synthetic user message (`followups.rs:31-49`, `:136-139`). Required in this phase: once the prompt says "write Markdown", labels will contain backticks.
  - [x] Rewrite `## Length and formatting` (`telegram_instruction_layer.md:16-23`) as Markdown: replace the allowed-tag list, the "no Markdown syntax" ban, the `<code>`-wrapping rule, and both escaping rules (`:22-23`) — escaping becomes the converter's job. Keep the length budget (`:12`), the bullet/section/steps guidance (`:19-21`), and the `filepath:line` rule (`:24`).
  - [x] Rewrite `## Examples` (`:58-82`) in Markdown, including the closing "Avoid" line (`:82`) which currently forbids exactly what we now want.
  - [x] Fix the intro (`:3`): "not a terminal, email, or Markdown renderer" is now false.
  - [x] Fix the two HTML leaks inside `## Detailed answers and file uploads`: `<i>Full details attached ↓</i>` (`:47`) becomes Markdown emphasis, and "wrap it in `<code>`" (`:50`) becomes a backtick reference. The rest of that section (`:40-56`) and all of `## Suggested replies` (`:26-38`) stay as-is — the artifact rule and the `<followups>`/`<media>` control tags are unchanged.
  - [x] Prompt states the exclusions: no tables in chat (use an artifact file), no `~~strikethrough~~` (renders unstyled in the TUI, `parse.rs:467-475`), and include bare URLs where the destination matters (the TUI drops link targets, `parse.rs:500-502`).
  - [x] Unit tests in the `html.rs` style (inline `#[cfg(test)] mod tests`, behavior-named, exact `assert_eq!`) over fixed fixtures: each mapping; nested lists; nested quote; quote containing a code block; bold containing code; link containing code; stray `<` and `&` in prose; a Markdown code span containing a literal `&amp;`; blank lines inside fenced code; legacy HTML-authored replies captured from real threads; idempotency.
- **✅ Demo**: `cargo nextest run -p zdx-bot` passes on the fixture set. Then run `just bot` and ask for a reply containing a fenced code block, a bullet list, inline code, a link, and a quote. Confirm (a) Telegram renders all of it, (b) the persisted thread text is still Markdown, and (c) opening that same thread in `just monitor` shows the code block and quote — the exact content that is invisible today.
- **Risks / failure modes**:
  - Invalid entity nesting silently degrades to plain text with visible tags via the `sanitized == input` path (`telegram/mod.rs:598-610`). The nesting tests are the guard.
  - Whitespace divergence between converter and transcript would defeat the cross-surface goal; the soft-break/paragraph contract above is the guard.
  - The model may emit HTML out of habit during transition. Recognized Telegram HTML triggers the narrow legacy bypass; other raw HTML is escaped literally.
  - Prompt shrinks substantially; re-read the whole layer after editing to confirm no rule was orphaned.

## Phase 2: Model-text paths that bypass the choke point — implementation complete 2026-08-11; live demo pending

- **Goal**: no remaining surface where model-generated text shows raw Markdown. These are already lossy today and Phase 1 does not newly break them (their generators do not consume the Telegram instruction layer), so they follow rather than block.
- **Scope checklist**:
  - [x] `/tldr`: the generator returns plain Markdown (`zdx-engine/src/core/tldr_generation.rs:17-25`) and the bot HTML-escapes it (`handlers/message/commands.rs:635-646`) — convert instead of escape.
  - [x] `/handoff` and `/prompt_builder` previews: generators use no system prompt (`zdx-engine/src/core/handoff_generation.rs:159-176`, `prompt_builder_generation.rs:63-81`) and `suggestion_preview` escapes into a bot-authored `<blockquote>` (`staging.rs:756-777`, sent at `:332-364`) — convert instead of escape.
  - [x] Drop the unconditional outer `<blockquote>` frame in `suggestion_preview` (`staging.rs:770-776`); converted content may already contain a quote and Telegram forbids nesting them. Use a title/separator frame, or flatten quotes inside previews.
  - [x] Convert **before** truncating in both paths. Both currently truncate source Markdown first (`commands.rs:635-655`, `staging.rs:34-36`), which can cut a fence, link, or emphasis mid-construct. Truncate on visible characters without splitting tags or entities.
- **✅ Demo**: run `/tldr`, `/handoff`, and `/prompt_builder` in a live topic, including one case whose generated text contains a Markdown quote and one long enough to hit truncation. All previews render formatted, with no literal backticks and no broken tags.
- **Risks / failure modes**:
  - Truncation on converted HTML must not split an entity or leave an unclosed tag.

# Contracts (guardrails)

- `<followups>`, `<media>`, `<medias>` are stripped before any Markdown parsing, so control tags never reach the converter (order already correct at `media.rs:130-148`).
- The converter's output must be Telegram-valid on its own. `html::sanitize` only runs after a Telegram parse rejection (`telegram/mod.rs:586-610`, `:648-674`) and only repairs tag syntax — it is a net for unexpected input, not part of the normal path.
- The converter escapes its own text, code payloads, and attribute values; `sanitize` permits arbitrary attribute content (`html.rs:110-129`) and will not do it.
- Raw HTML is escaped on the Markdown path. A recognized Telegram tag in a pulldown raw-HTML event selects a whole-message legacy HTML bypass, avoiding Markdown reinterpretation inside `<code>`/`<pre>` while ignoring tag-like text inside Markdown code spans.
- Legacy HTML bypass requires canonical attributes, balanced tags, and Telegram-valid nesting across every raw HTML event; malformed or mixed HTML stays on the escaped Markdown path.
- Converter parser options stay identical to the transcript renderer's. Divergence means content that renders on one surface and not the other, which is the bug being fixed.
- Bot-authored HTML (status, pickers, launcher, retry, keyboards) is never passed through the converter.
- Persisted thread text stays Markdown; conversion happens at the bot edge only.
- Media path extraction keeps tolerating backtick-wrapped paths (`media.rs:parse_media_path`).
- No new dependency: `pulldown-cmark` is already in the workspace.

# Key decisions (decide early)

- **Converter and prompt flip ship together.** A parse/re-emit is not output-preserving for today's HTML-authored replies, so a "wire it inert first" split has no safety value. A narrow parser-event-based legacy HTML bypass remains at the edge because pulldown would otherwise reinterpret Markdown markers inside existing `<code>`/`<pre>` payloads.
- **Emit Telegram-valid nesting in the converter, not downstream.** `sanitize` cannot fix balanced-but-forbidden nesting, and the failure mode is silent plain-text fallback with visible tags.
- **The converter owns block spacing; `normalize_reply_text` leaves the model-text path.** Global blank-line collapsing is unsafe on either side of conversion.
- **Match the transcript renderer's parser options and soft-break semantics** (`ENABLE_TABLES` only, soft break → space). Revisit as a joint upgrade of both surfaces, never one alone.
- **Tables degrade quietly to plain lines** rather than hard-failing; the artifact-file rule already covers layouts that matter.

# Testing

- Unit tests for the converter over fixed fixtures (pure function, `html.rs` style). No integration dir exists for `zdx-bot` and none is needed.
- Fixtures must include legacy HTML-authored replies captured from real threads, so the transition behavior is documented rather than assumed.
- Keep the existing `parse_final_response` tests passing (`handlers/message/mod.rs:314-390`); extend one to assert Markdown input produces HTML output.
- Manual smoke demo per phase, as listed above.
- Commands: `cargo nextest run -p zdx-bot` while iterating, `just ci-fast` for quick lint feedback, `just ci` from repo root as the final gate.

# Polish rounds (after MVP)

## Polish round 1: Formatting fidelity

- Preserve link destinations in a form the TUI can show, given it drops them today.
- Revisit fenced-code language handling once real replies exercise it.
- ✅ Check-in demo: a reply with several links and a language-tagged code block renders well in both Telegram and the monitor.

# Later / Deferred

- Joint upgrade of the transcript renderer and converter to support strikethrough and task lists — revisit if replies actually want them.
- Rendering `Event::Html` in the transcript renderer instead of dropping it — revisit only if some surface still needs to author raw HTML.
- Entity-aware truncation/splitting for normal replies, plus teaching the fallback classifier to recognize "message is too long" alongside parse errors (`telegram/mod.rs:71-77`) — revisit if long replies actually get rejected.
- Stripping Markdown from generated topic titles (`zdx-engine/src/core/title_generation.rs:77-105` strips only quotes/backticks) — revisit if titles show stray markers.
- Backfilling old HTML-authored thread text — revisit only if reading old threads in the TUI becomes a real annoyance.
