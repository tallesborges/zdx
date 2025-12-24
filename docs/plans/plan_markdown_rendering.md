# Ship-First Markdown Rendering Implementation Plan

## Validation Summary

**Status:** ✅ **APPROVED WITH REVISIONS** (Validated by Gemini 3 Pro + GPT-5.2 Codex)

**Key Findings:**
- ✅ Plan is directionally sound and follows good incremental delivery practices
- ⚠️ **CRITICAL:** Styled text wrapping is a blocking architecture issue (added to Slice 1)
- ✅ pulldown-cmark is the correct dependency choice
- ✅ Slice ordering is optimal (finalized → streaming → polish)
- ⚠️ Several edge cases and guardrails added based on validation feedback

**Consensus Corrections:**
1. **Remove heuristic** (Decision D1): Always parse assistant content as markdown
2. **Add styled wrapping** to Slice 1 (prerequisite for all other slices)
3. **Document clipping** behavior for code blocks in MVP
4. **Avoid textwrap** crate (conflicts with "no new deps" constraint)
5. **Update AGENTS.md** when adding `src/ui/markdown.rs` (repo requirement)

---

## Inputs

**Project/feature:** Add markdown rendering to zdx-cli assistant responses. Currently shows plain text; want styled headings, code blocks, lists, emphasis, and links to improve readability of technical responses.

**Existing state:**
- Ratatui-based TUI with reducer pattern (state.rs, update.rs, view.rs)
- Transcript model: HistoryCell → StyledLine → StyledSpan
- Plain text wrapping with unicode-width support
- Streaming via AssistantDelta events
- Terminal safety/restore already working (terminal.rs)
- Style enum for semantic styles

**Constraints:**
- Must use existing StyledLine/StyledSpan architecture (no rewrite)
- Must integrate with current streaming (delta-based)
- Must maintain width-agnostic transcript model
- Must use pulldown-cmark (proven, same as codex)
- No new dependencies beyond pulldown-cmark
- Terminal restore must not regress

**Success looks like:**
Daily-usable markdown viewing in TUI: headings stand out, code blocks are readable (monospace, no wrap), lists are indented, **bold** and *italic* work, links are visible. Streaming markdown updates smoothly without flicker.

---

## Research Summary: openai/codex Markdown Implementation

**Core architecture:**
- Uses `pulldown-cmark` for parsing markdown into events
- Uses `ratatui` for terminal rendering (same as zdx-cli!)
- `MarkdownStyles` struct defines ratatui styles for different elements
- `render_markdown_text_with_width` function converts markdown → styled ratatui spans/lines
- `MarkdownStreamCollector` buffers deltas and commits only complete lines (newline-delimited)

**Styled elements:**
- Headings (H1: bold+underline, H2: bold, H3-H6: styled)
- Code blocks (cyan, no wrapping to preserve formatting)
- Inline code (cyan)
- Emphasis (italic), Strong (bold), Strikethrough
- Lists (indented, markers styled)
- Blockquotes (green, indented)
- Links (cyan+underline, URL shown in parens)

**Wrapping strategy:**
- Plain text and inline elements wrap normally
- Code blocks never wrap (preserve whitespace)
- Lists/blockquotes maintain indent across wrapped lines
- ⚠️ **Note:** We will NOT use `textwrap` crate (conflicts with "no new deps"); instead, we'll implement styled wrapping in-house using existing unicode-width logic

---

# Goals
- User sees formatted markdown in assistant responses (headings, code, lists, emphasis, links)
- Streaming markdown accumulates and renders incrementally (no flash of unstyled content)
- Code blocks preserve formatting (no word wrap, monospace)
- Wrapping respects markdown structure (don't break mid-element)
- Copy existing patterns from openai/codex (proven, reduces risk)

# Non-goals
- Tables, images, HTML (defer until proven daily need)
- Syntax highlighting in code blocks (defer; monospace + color is enough for MVP)
- Custom markdown extensions
- Markdown editor/input (responses only)
- Rewriting existing transcript/wrapping architecture

# Design principles
- **User journey drives order:** Input arrives → parses → renders → user sees styled output → streaming updates → polish
- **Ship-first:** Get markdown visible early; optimize later
- **Copy proven patterns:** Use openai/codex structure (MarkdownStyles, render function, stream collector)
- **YAGNI:** Only implement elements the LLM actually outputs (headings, code, lists, emphasis)
- **Preserve existing architecture:** Extend StyledLine/StyledSpan, don't replace
- **Reducer pattern:** Markdown rendering is pure function (state → styled lines)

# User journey
1. User sends prompt
2. Assistant streams response with markdown syntax (`# Heading`, `` `code` ``, `**bold**`)
3. Engine emits AssistantDelta events with raw markdown
4. TUI accumulates markdown in transcript cell
5. Renderer parses markdown → styled spans → wraps at current width → displays
6. User sees formatted output (headings bold, code blocks preserved, lists indented)
7. User scrolls/resizes → re-renders at new width, formatting stays correct

# Foundations / Already shipped (✅)

## Terminal safety/restore
- **What exists:** `src/ui/terminal.rs` handles raw mode entry/exit, alt-screen, panic hooks, clean restore
- ✅ **Demo:** Run `zdx`, Ctrl+C → terminal restores correctly; trigger panic → terminal still restores
- **Gaps:** None (verified working)

## Transcript model (width-agnostic)
- **What exists:** `HistoryCell` → `display_lines(width)` → `Vec<StyledLine>` → `Vec<StyledSpan>`
- ✅ **Demo:** Resize terminal during session → lines re-wrap correctly without data loss
- **Gaps:** None (wrapping, caching, styles all working for plain text)

## Streaming infrastructure
- **What exists:** `AssistantDelta` events → `append_assistant_delta()` → `is_streaming` flag → cursor indicator
- ✅ **Demo:** Send prompt → see streaming text appear with cursor → finalize → cursor disappears
- **Gaps:** None (delta accumulation works)

## Wrapping with unicode-width
- **What exists:** `wrap_text()` and `break_word_by_width()` handle CJK, emoji, zero-width chars
- ✅ **Demo:** Send message with CJK/emoji → wraps at correct display columns
- **Gaps:** None (unicode handling solid)

## Style system
- **What exists:** `Style` enum → view.rs converts to ratatui styles
- ✅ **Demo:** User/assistant/tool messages have different colors
- **Gaps:** Need to add markdown-specific styles (heading, code, link, etc.)

## Reducer pattern
- **What exists:** `update.rs` mutates state, `view.rs` renders read-only, `effects.rs` defines side effects
- ✅ **Demo:** All UI interactions go through update reducer → state changes → view re-renders
- **Gaps:** None (architecture in place)

---

# 🚨 Critical Architecture Issue: Styled Text Wrapping

**Identified by:** Gemini 3 Pro + GPT-5.2 Codex validation

**The Problem:**
The current `wrap_text(&str, width) -> Vec<String>` operates on plain text, but markdown rendering produces `Vec<StyledSpan>` (text + style pairs). You cannot:
1. Join styled spans into plain text → **loses style information**
2. Wrap the plain text → **style boundaries are lost**
3. Re-apply styles after wrapping → **impossible to know where styles were**

**Impact:** Without styled wrapping, you'll either render unwrapped (overflow/clipped) or flatten to plain text (lose all markdown styling). This affects **every slice** and is fundamental to correctness.

**Solution:** Implement `wrap_styled_spans()` in Slice 1 as a prerequisite for all other slices.

## Styled Wrapping Requirements

**Must handle:**
1. Unicode-width preservation (CJK, emoji, zero-width chars)
2. Breaking mid-span while preserving style for each fragment
3. Word boundaries when possible
4. Whitespace preservation for inline code (no collapsing)
5. Soft breaks (`Event::SoftBreak` → space) vs hard breaks (`Event::HardBreak` → newline)
6. Hanging indents for lists/blockquotes

**Implementation approach:**
```rust
/// Options for wrapping styled spans with hanging indents
#[derive(Debug, Clone)]
pub struct WrapOptions {
    pub width: usize,
    pub first_prefix: Vec<StyledSpan>,  // e.g., "- " for list bullet
    pub rest_prefix: Vec<StyledSpan>,   // e.g., "  " for continuation lines
}

/// Wrap styled spans while preserving styles across line breaks
fn wrap_styled_spans(spans: &[StyledSpan], opts: &WrapOptions) -> Vec<StyledLine>
```

**Key design decisions:**
- Reuses existing `break_word_by_width()` from `transcript.rs` for unicode handling
- When a span is split across lines, creates new spans with the same `Style` for fragments
- Inline code (`Style::CodeInline`) preserves whitespace runs (no `split_whitespace()`)
- Normal text collapses whitespace and wraps on word boundaries
- Hard breaks (`\n` in span.text) trigger immediate line flush
- Prefixes support hanging indents for lists/blockquotes

**Integration:**
- Paragraphs: `wrap_styled_spans(&spans, &WrapOptions { width, first_prefix: vec![], rest_prefix: vec![] })`
- List items: First line has bullet prefix, continuation lines indent to align with text
- Blockquotes: Both lines have `"> "` prefix or indent
- Code blocks: Skip wrapping entirely, emit lines as-is

**Reference implementation:** Provided by GPT-5.2 (see session 019b511b-6b31-7cc1-b0c3-7dd7f7b65f15)

---

# MVP slices (ship-shaped, demoable)

## Slice 1: Markdown parsing (non-streaming, finalized only)
**Goal:** Parse finalized assistant responses as markdown, render styled inline elements (bold, italic, inline code). Implement styled text wrapping as foundation for all other slices.

**Status:** ✅ **COMPLETE**

**Scope checklist:**
- [x] Add `pulldown-cmark = "0.11"` to Cargo.toml
- [x] Create `src/ui/markdown.rs`
- [x] **[CRITICAL]** Implement `wrap_styled_spans(spans: &[StyledSpan], opts: &WrapOptions) -> Vec<StyledLine>`
  - [x] Add `WrapOptions` struct with `width`, `first_prefix`, `rest_prefix` fields
  - [x] Extend existing unicode-width logic from `transcript.rs` (reuse `break_word_by_width()`)
  - [x] Handle breaking mid-span: split span into fragments, preserve style on each fragment
  - [x] Respect word boundaries for normal text (use `split_whitespace()`)
  - [x] Preserve whitespace for inline code (`Style::CodeInline` → no whitespace collapsing)
  - [x] Handle hard breaks: `\n` in `span.text` triggers line flush
  - [x] Support hanging indents via `first_prefix` vs `rest_prefix`
- [x] Implement `render_markdown(text: &str, width: usize) -> Vec<StyledLine>`
  - [x] Parse markdown with `pulldown_cmark::Parser::new()`
  - [x] Handle events: `Text`, `Code`, `Emphasis`, `Strong`, `SoftBreak`, `HardBreak`
  - [x] `Event::SoftBreak` → emit space span
  - [x] `Event::HardBreak` → emit `\n` in span text (triggers flush in wrapper)
  - [x] `Event::Html` → skip or render as plain text (avoid terminal injection)
  - [x] Convert events to `Vec<StyledSpan>`, then call `wrap_styled_spans()`
- [x] Add `Style` enum variants: `CodeInline`, `Emphasis`, `Strong`
- [x] Hook into `HistoryCell::Assistant::display_lines()` — **always** use `render_markdown()` (no heuristic)
- [x] Graceful degradation: if `pulldown_cmark` parsing fails → fall back to plain text rendering
- [x] **Update `AGENTS.md`** when adding `src/ui/markdown.rs` (required by repo rules)

**Implementation notes:**
- Also implemented Slice 2 (code blocks) and Slice 3 (headings) and Slice 4 (lists) as part of the same pass
- Slice 6 (links and blockquotes) also included
- Added view.rs style mappings for all markdown styles
- Tests added in `src/ui/markdown.rs`

**Additional fixes applied:**
- Fixed extra blank lines between list items (paragraphs inside lists don't add trailing blank line)
- Fixed missing spaces around inline code (preserve leading/trailing whitespace in text spans)
- Added subtle ```` ``` ```` fence markers with `Style::CodeFence` (dark gray)
- Show language identifier on opening fence (e.g., ```` ```rust ````)
- Fixed empty line before closing fence (trim trailing newlines from code content)

**✅ Demo:**
```bash
# Run zdx, send prompt that triggers markdown response
cargo run --
> Explain Rust borrowing

# Expect to see:
# - **bold** words in bold
# - *italic* words in italic
# - `code` in monospace/cyan
# - Plain text for everything else
```

**Failure modes / guardrails:**
- If markdown parser fails → fall back to plain text rendering
- If unknown event type → skip (don't crash)
- No streaming yet, so no incomplete parse issues

---

## Slice 2: Code blocks (preserve formatting, no wrap)
**Goal:** Render fenced code blocks with monospace style, no word wrapping, preserve indentation.

**Status:** ✅ **COMPLETE** (implemented as part of Slice 1)

**Scope checklist:**
- [x] Handle `pulldown_cmark::Event::Start(Tag::CodeBlock(...))` and `End(TagEnd::CodeBlock)`
- [x] Track `in_code_block` state during event processing
- [x] When in code block: collect text into separate lines, **skip `wrap_styled_spans()`** entirely
- [x] Preserve whitespace exactly (avoid `split_whitespace()` or any normalization)
- [x] Add `Style::CodeBlock` variant
- [x] Ensure code blocks have indent/prefix for visual separation (e.g., indent by 2 spaces)
- [x] Add `Style::CodeFence` for subtle ```` ``` ```` markers (dark gray)
- [x] Show language identifier on opening fence (e.g., ```` ```rust ````)
- [x] Trim trailing newlines to avoid empty line before closing fence
- [ ] **Document MVP limitation:** Long code lines will be **clipped** by ratatui (no horizontal scroll until Phase 2)
  - Consider adding visual `…` indicator at edge for clipped lines

**✅ Demo:**
```bash
cargo run --
> Show me a Rust function

# Expect to see:
# ```rust
# fn example() {
#     let x = 42;
# }
# ```
# With proper indentation, monospace, cyan color, no wrapping even if long lines
```

**Failure modes / guardrails:**
- Long code lines **will be clipped** (ratatui's default behavior) until Phase 2 adds horizontal scroll
- Consider adding `…` (dim/gray) at the edge for visual feedback that content is hidden
- For MVP: preserving code formatting is more important than fitting width

---

## Slice 3: Headings
**Goal:** Style headings (H1-H6) to stand out visually.

**Status:** ✅ **COMPLETE** (implemented as part of Slice 1)

**Scope checklist:**
- [x] Handle `Start(Tag::Heading { level, .. })` and `End(TagEnd::Heading(level))`
- [x] Add `Style::H1`, `Style::H2`, `Style::H3` variants (H4-H6 can share H3 style for MVP)
- [x] H1: bold + underline, H2: bold, H3: italic or dim
- [x] Emit heading content as styled spans, wrap using `wrap_styled_spans()` with empty prefixes
- [x] Add spacing via explicit blank `StyledLine`s (not embedded `\n`) to keep width-agnostic model consistent

**✅ Demo:**
```bash
cargo run --
> Explain async/await

# Expect to see:
# # Big Heading      <- bold + underline
# ## Subheading      <- bold
# ### Smaller        <- styled differently
```

**Failure modes / guardrails:**
- Nested headings (rare) → handle by current level
- Very long heading text → wraps normally (headings are short in practice)

---

## Slice 4: Lists (ordered, unordered, indented)
**Goal:** Render lists with bullets/numbers and proper indentation.

**Status:** ✅ **COMPLETE** (implemented as part of Slice 1)

**Dependencies:** Requires `wrap_styled_spans()` from Slice 1 for hanging indents.

**Scope checklist:**
- [x] Handle `Start(Tag::List(None))` (unordered) and `Start(Tag::List(Some(start)))` (ordered)
- [x] Handle `Start(Tag::Item)` and `End(TagEnd::Item)`
- [x] Track indent level for nested lists
- [x] Emit list markers: `•` or `-` for unordered, `1.`, `2.` for ordered
- [x] Indent list items (e.g., 2 spaces per level)
- [x] Use `wrap_styled_spans()` with `WrapOptions`:
  - `first_prefix`: bullet/number + space (e.g., `[StyledSpan { text: "- ", style: Style::ListBullet }]`)
  - `rest_prefix`: spaces to align with text start (e.g., `[StyledSpan { text: "  ", style: Style::Assistant }]`)
- [x] **Note:** Current `render_prefixed_content()` only supports single style + `&str`; it won't work for mixed inline styles inside list items

**✅ Demo:**
```bash
cargo run --
> Give me a checklist for Rust setup

# Expect to see:
# - Install Rust
# - Set up editor
#   - VSCode
#   - Vim
# - Write hello world
```

**Failure modes / guardrails:**
- Deeply nested lists (rare) → limit visual indent to avoid running out of horizontal space
- Mixed list types → handle state stack correctly

---

## Slice 5: Streaming markdown (incremental parsing)
**Goal:** Stream markdown deltas and commit complete elements as they arrive. Based on openai/codex `MarkdownStreamCollector`.

**Scope checklist:**
- [ ] Create `MarkdownStreamCollector` in `src/ui/markdown.rs`
- [ ] `push_delta(delta: &str)` accumulates into buffer
- [ ] `commit_complete_lines(width: usize) -> Vec<StyledLine>` parses buffer, returns lines ending in `\n`
- [ ] `finalize(width: usize) -> Vec<StyledLine>` renders all remaining buffered content
- [ ] In `HistoryCell::Assistant`, store raw markdown + committed line count
- [ ] In `display_lines()`, if streaming → use collector to render committed prefix only
- [ ] Cursor appears after committed content
- [ ] **Caching concern:** Ensure streaming cells remain **non-cacheable** (content changes every delta)
- [ ] **Buffer strategy:** Commit on hard boundaries (newline, code fence close, blank line) but add max buffered size fallback to avoid delaying long paragraphs

**✅ Demo:**
```bash
cargo run --
> Explain closures

# Expect to see:
# - Markdown elements appear as complete lines arrive
# - Code blocks don't render until closing ``` arrives
# - No flash of unstyled content
# - Streaming cursor appears after last committed line
```

**Failure modes / guardrails:**
- Incomplete markdown (e.g., only `**bold` without closing `**`) → buffer until closing delimiter or finalize
- Very long streamed response → committed lines cache, only re-parse uncommitted buffer
- If parse fails mid-stream → fall back to plain text for uncommitted portion

---

## Slice 6: Links and blockquotes
**Goal:** Render links (cyan, underlined, show URL) and blockquotes (indented, styled).

**Status:** ⚠️ **PARTIALLY COMPLETE** (implemented as part of Slice 1)

**Dependencies:** Requires `wrap_styled_spans()` from Slice 1 for proper wrapping.

**Scope checklist:**
- [x] Handle `Start(Tag::Link { dest_url, .. })` → render link text styled
  - [ ] Handle nested emphasis inside link text (preserve styles) - **TODO: URL display**
  - [ ] Truncate very long URLs (e.g., middle truncation: `https://example.com/very...long/path`) - **TODO**
- [x] Add `Style::Link` variant
- [x] Handle `Start(Tag::BlockQuote)` → indent content, add blockquote style
- [x] Add `Style::BlockQuote` variant
- [x] Use `wrap_styled_spans()` with appropriate prefixes to preserve indentation across wrapped lines

**Note:** Link URL display not yet implemented - link text is styled but URL is not shown in parentheses.

**✅ Demo:**
```bash
cargo run --
> Tell me about Rust docs

# Expect to see:
# See [the book](https://doc.rust-lang.org/book/)
# Rendered as:
# See the book (https://doc.rust-lang.org/book/)  <- cyan + underline

# > This is a quote
# Rendered as:
# > This is a quote  <- indented, green/dim
```

**Failure modes / guardrails:**
- Very long URLs → may wrap, that's okay
- Nested blockquotes → handle indent stack

---

# Contracts (guardrails)

**Must not regress:**
1. Terminal restore on panic/Ctrl+C (terminal.rs)
2. Wrapping respects unicode display width (CJK, emoji)
3. Streaming cursor appears/disappears correctly
4. Resize triggers re-wrap without data loss
5. Plain text messages still render correctly (backward compat)
6. Finalized cells are cacheable (don't re-parse markdown every frame)

**New contracts (markdown-specific):**
1. Code blocks never word-wrap (preserve formatting)
2. If markdown parse fails → fall back to plain text (graceful degradation)
3. Streaming markdown commits only complete elements (no half-rendered code blocks)
4. Markdown rendering is pure function (no side effects, no global state)
5. Styled wrapping preserves unicode display width (CJK, emoji)
6. Inline code preserves whitespace (no `split_whitespace()` collapsing)

---

# Missing Guardrails / Edge Cases

**Identified by:** GPT-5.2 Codex validation

These edge cases must be handled to ensure correct rendering:

## Text Processing
1. **Inline code with spaces/backticks:**
   - `split_whitespace()` will collapse spaces and break literal formatting
   - **Fix:** Inline code (`Style::CodeInline`) must preserve spaces and wrap character-by-character within the span

2. **Soft vs hard breaks:**
   - `Event::SoftBreak` should become a space in the current style
   - `Event::HardBreak` should emit a newline or trigger line flush
   - **Fix:** Must be explicit in event handling to match markdown expectations

3. **Whitespace normalization:**
   - Normal text should collapse multiple spaces to single space
   - Inline code should preserve exact whitespace
   - **Fix:** `preserve_ws` flag based on `Style::CodeInline`

## Nested/Mixed Styles
4. **Emphasis inside links or list items:**
   - Must avoid losing style when splitting spans across lines
   - **Fix:** `wrap_styled_spans()` creates new spans with same style for fragments

5. **List item continuation lines:**
   - Wrapped lines must align with text start (not under the bullet)
   - **Fix:** Hanging indent via `WrapOptions.rest_prefix`

## HTML/Security
6. **HTML events:**
   - `pulldown-cmark` emits `Event::Html` for raw HTML
   - **Fix:** Skip or render as plain text to avoid terminal injection surprises

## Streaming
7. **Streaming code fences:**
   - Ensure no partial fence renders as normal text
   - **Fix:** Buffer until closing ``` arrives

8. **Markdown state persistence:**
   - pulldown-cmark parsers are stateful
   - **Fix:** Re-parse active buffer on every frame with fresh parser instance; cache finalized lines

## Long Content
9. **Very long URLs:**
   - May dominate the TUI if extremely long (200+ chars)
   - **Fix:** Truncate middle of long URLs: `https://example.com/very...long/path`

10. **Deep nesting:**
    - Lists/blockquotes nested 5+ levels may run out of horizontal space
    - **Fix:** Limit visual indent or wrap aggressively

---

# Key decisions (decide early)

## D1: When to parse as markdown vs plain text?
**Decision:** Always parse all assistant content as markdown.

**Rationale:**
- pulldown-cmark handles plain text efficiently (just emits `Text` events)
- No heuristic needed (avoids false positives/negatives like "$2/lb" or "I bought 5 lbs @ #2")
- Simpler implementation, consistent behavior
- Graceful degradation: if parse fails → fall back to plain text

**Why this changed:** Original plan had heuristic to detect markdown syntax. Validation feedback (Gemini + GPT-5.2) showed this is error-prone and unnecessary.

---

## D2: Where does markdown parsing happen?
**Decision:** In `HistoryCell::display_lines()` for finalized cells, in new `MarkdownStreamCollector` for streaming cells.

**Why:** Keeps transcript model UI-agnostic (stores raw text); rendering logic lives in UI layer.

---

## D3: How to handle incomplete markdown during streaming?
**Decision:** Copy openai/codex approach:
- Buffer deltas until logical delimiter (newline for lines, closing tag for blocks)
- Commit only complete elements
- Cursor appears after last committed line
- On finalize, render all remaining buffer

**Why:** Proven approach; avoids flash of unstyled → styled content.

---

## D4: Code block horizontal overflow?
**Decision:** MVP: Let long code lines overflow off-screen (user scrolls). Future: Add horizontal scroll or truncate with `...`.

**Why:** Preserving code formatting is more important than fitting width; horizontal scroll is a polish feature.

---

## D5: Markdown styles in Style enum?
**Decision:** Add variants to existing `Style` enum:
```rust
pub enum Style {
    // ... existing ...
    CodeInline,
    CodeBlock,
    Emphasis,
    Strong,
    H1, H2, H3,
    Link,
    BlockQuote,
    ListBullet,
    ListNumber,
}
```

**Why:** Keeps styles semantic; view.rs maps to ratatui styles; easy to tweak colors later.

---

## D6: How to implement styled wrapping?
**Decision:** Implement `wrap_styled_spans()` in-house, reusing existing `break_word_by_width()` from `transcript.rs`.

**Approach:**
- No new dependencies (avoid `textwrap` crate)
- `WrapOptions` struct with `width`, `first_prefix`, `rest_prefix` for hanging indents
- When span doesn't fit, split into fragments with same style
- Inline code (`Style::CodeInline`) preserves whitespace, wraps character-by-character
- Normal text collapses whitespace, wraps on word boundaries
- Hard breaks (`\n` in span.text) trigger immediate line flush

**Reference:** GPT-5.2 implementation in session 019b511b-6b31-7cc1-b0c3-7dd7f7b65f15

**Why:** Avoids dependency bloat, maintains consistency with existing unicode-width handling, provides full control over styling.

---

# Testing

**Manual smoke demos per slice:**
- Each slice includes ✅ Demo section above
- Run after completing slice, verify expected output
- Keep test prompts in `tests/fixtures/markdown_samples.md` for regression

**Minimal regression tests:**
- `tests/markdown_render_test.rs`:
  - `test_wrap_styled_spans_basic()` — wrap plain spans across lines, preserves styles
  - `test_wrap_styled_spans_mid_span_break()` — break in middle of styled span → fragments have same style
  - `test_wrap_styled_spans_inline_code_whitespace()` — inline code preserves spaces
  - `test_wrap_styled_spans_hanging_indent()` — list prefixes work correctly
  - `test_wrap_styled_spans_hard_break()` — `\n` in span triggers line flush
  - `test_inline_code()` — `code` → cyan span
  - `test_bold_italic()` — `**bold**` and `*italic*` → styled spans
  - `test_soft_hard_breaks()` — soft break → space, hard break → newline
  - `test_code_block_no_wrap()` — fenced block → no word wrap, all lines preserved
  - `test_code_block_whitespace()` — code blocks preserve exact whitespace
  - `test_heading_styles()` — `# H1`, `## H2` → correct style variants
  - `test_list_indent()` — unordered/ordered lists → correct markers + hanging indent
  - `test_fallback_to_plain()` — malformed markdown → plain text output (no crash)
  - `test_html_events_skipped()` — HTML in markdown → skipped or plain text
  - `test_streaming_commit_lines()` — push deltas → only complete lines returned
  - `test_streaming_cache()` — streaming cells not cacheable, finalized cells cacheable
- Run with `cargo test markdown`

---

# MVP Limitations (Document Clearly)

These limitations are acceptable for MVP and will be addressed in polish phases:

1. **Code block clipping:** Long code lines (>terminal width) will be clipped by ratatui
   - No horizontal scroll until Phase 2
   - Consider visual `…` indicator for UX feedback

2. **No syntax highlighting:** Code blocks render in monospace with single color
   - Defer until proven daily need (requires `syntect` or similar)

3. **No tables/images/HTML:** Not implemented
   - Rare in LLM responses
   - Defer until user reports "I can't read the table"

4. **Long paragraphs delay during streaming:** Buffering until newline means long paragraphs appear all at once
   - Mitigated by max buffer size fallback in Slice 5

5. **Deep nesting visual limit:** Lists/blockquotes nested 5+ levels may exceed terminal width
   - Rare in practice
   - Wrap aggressively if encountered

---

# Risk Assessment

**Ranked from highest to lowest risk:**

## 🔴 HIGHEST RISK: Styled wrapping across spans and indentation
- **Why:** Affects every slice and is fundamental to correctness
- **Impact:** Without it, markdown rendering is broken (clipped or loses styles)
- **Mitigation:** Implement in Slice 1 before any other work; use GPT-5.2's reference implementation

## 🟡 MODERATE RISK: Streaming markdown chunking
- **Why:** Incomplete structures can misrender or flicker
- **Impact:** Bad UX (flash of unstyled content, half-rendered code blocks)
- **Mitigation:** Buffer until hard boundaries (newline, code fence close); max buffer fallback

## 🟡 MODERATE RISK: Performance/caching
- **Why:** Re-parsing markdown every frame is expensive
- **Impact:** Slow rendering on large transcripts
- **Mitigation:** Cache finalized cells, only re-parse uncommitted buffer for streaming cells

## 🟢 LOWER RISK: Markdown edge cases
- **Why:** Mostly cosmetic issues (inline code spacing, nested styles)
- **Impact:** Minor rendering glitches in specific scenarios
- **Mitigation:** Handle explicit cases in guardrails list

---

# Polish phases (after MVP)

## Phase 1: Streaming performance (after Slice 5 ships)
**Goal:** Reduce re-parsing overhead during streaming.
- Cache parsed markdown structure for committed lines
- Only re-parse uncommitted buffer
- ✅ **Check-in demo:** Stream 1000-line markdown response → no visible lag, smooth cursor updates

---

## Phase 2: Horizontal scroll for code blocks (after Slice 2 ships + user feedback)
**Goal:** Let user scroll horizontally to see long code lines.
- Add horizontal offset state for code blocks
- Keybinding: `→/←` or `h/l` when in code block
- ✅ **Check-in demo:** Long code line → scroll right → see rest of line

---

## Phase 3: Copy respects markdown structure (after selection ships)
**Goal:** When user copies code block, preserve indentation and formatting.
- Copy uses raw markdown lines (not wrapped spans)
- ✅ **Check-in demo:** Select code block → copy → paste into editor → formatting preserved

---

# Later / Deferred

**Syntax highlighting in code blocks:**
- **Why deferred:** Requires `syntect` or similar; large dependency, scope creep
- **Trigger:** User feedback shows code is hard to read without highlighting (not expected for MVP)

**Tables, images, HTML:**
- **Why deferred:** Rare in LLM responses; complex rendering
- **Trigger:** User reports "I can't read the table in response"

**Markdown in user input:**
- **Why deferred:** Users type plain text; markdown is for assistant responses
- **Trigger:** Feature request for formatted input (unlikely)

**Custom markdown extensions (e.g., callouts, admonitions):**
- **Why deferred:** pulldown-cmark doesn't support by default; niche need
- **Trigger:** LLM starts outputting custom syntax (not observed yet)

**Live markdown preview while typing:**
- **Why deferred:** MVP is response-only; editor mode is separate feature
- **Trigger:** User requests "live preview while I type"

---

**End of plan.**
