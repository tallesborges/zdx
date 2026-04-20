# Telegram Screenshot & Media Response

## Goals
- Agent can take screenshots when asked
- Bot can send media files as Telegram responses (not just text)
  - Images (png/jpg/webp)
  - Documents (pdf and other files)

## Non-goals
- Window/app-specific screenshot targeting (polish phase)
- Multiple monitor selection (polish phase)
- Image editing/annotation
- OCR or image analysis of screenshots
- Receiving/processing inbound Telegram media in this plan (separate flow)

## Design principles
- User journey drives order
- Minimal scope per slice
- KISS MVP: path-first file handoff before structured image blocks
- Route by file type: image -> `sendPhoto`, everything else -> `sendDocument`

## User journey
1. User asks bot: "take a screenshot"
2. Bot captures the screen
3. Bot sends screenshot back to Telegram chat as media

Secondary journey:
1. User asks bot to generate/export a file (e.g., PDF)
2. Tool returns local file path
3. Bot sends file to Telegram chat as document media

## Foundations / Already shipped (✅)

### Path handoff
- What exists: tools/skills can print saved file path(s)
- ✅ Demo: capture helpers return absolute output path(s)
- Gaps: bot flow does not yet parse media path(s) and route to the correct Telegram media API

### Telegram client
- What exists: `TelegramClient` with `send_message`, `download_file`
- ✅ Demo: Bot sends text replies
- Gaps: No media upload methods (`sendPhoto`, `sendDocument`)

### Screenshot skill
- What exists: `~/.zdx/skills/screenshot/` with cross-platform capture helpers
- ✅ Demo: skill can capture and print output file path(s)
- Gaps: Bot flow does not yet wire skill output path(s) into Telegram media send

## MVP slices (ship-shaped, demoable)

## Slice 1: Reuse existing screenshot skill
- **Goal**: Use existing screenshot skill + generic file-path contract for media upload (no new core tool)
- **Scope checklist**:
  - [ ] Reuse `~/.zdx/skills/screenshot/` (do not add `crates/zdx-core/src/tools/screenshot.rs`)
  - [ ] Capture via skill helper (typically `scripts/take_screenshot.py --mode temp`) and collect returned image path(s)
  - [ ] Define KISS output contract for MVP:
    - required XML wrapper: `<medias> ... </medias>`
    - entries: `<media>/absolute/path</media>` or `<media path="/absolute/path"/>`
    - first valid path is primary
  - [ ] Keep MVP behavior simple: full-screen request path first; window/region targeting stays optional
- **✅ Demo**: Run `zdx exec -p "take a screenshot"` → skill captures image and returns path
- **Risks / failure modes**:
  - Skill scripts may require OS permissions (macOS Screen Recording)
  - Multiple monitor captures can yield multiple files; MVP should send first file and log when more exist
  - Headless/server environments have no display (return clear error)

## Slice 2: Telegram media upload primitives
- **Goal**: TelegramClient can send both photos and documents
- **Scope checklist**:
  - [ ] Add `send_photo` method to `TelegramClient`
  - [ ] Add `send_document` method to `TelegramClient` (for pdf + generic files)
  - [ ] Use Telegram `sendPhoto` API with **multipart/form-data** (not JSON)
  - [ ] Use Telegram `sendDocument` API with **multipart/form-data**
  - [ ] Add path-based helper: read file bytes from `media_path` before upload
  - [ ] Detect file name + mime type from path (fallback to `application/octet-stream`)
  - [ ] Define consistent method args for both uploads (`chat_id`, media bytes, optional caption, `reply_to_message_id`, `message_thread_id`)
  - [ ] Ensure topic/reply support matches Bot API (`message_thread_id`, and migrate to `reply_parameters` when needed)
  - [ ] Use `reqwest::multipart::Form` for file upload
- **✅ Demo**: Unit test or manual test sending photo + PDF to Telegram chat
- **Risks / failure modes**:
  - Telegram upload limits differ by method:
    - `sendPhoto`: up to 10 MB (plus dimension constraints)
    - `sendDocument`: up to 50 MB
  - File read/path errors before upload (validate exists and readable)

## Slice 3: Agent media response flow
- **Goal**: When agent returns media path(s), bot sends file via proper Telegram media API
- **Scope checklist**:
  - [ ] In `handlers/message.rs`, change `run_agent_turn_with_persist` to return `messages` (currently dropped)
  - [ ] Parse media file path(s) from `<medias>` XML block using the Slice 1 output contract
  - [ ] Validate path exists, is regular file, and is readable
  - [ ] Route by file type:
    - image-like (`.png/.jpg/.jpeg/.webp`) -> `send_photo`
    - everything else (including `.pdf`) -> `send_document`
  - [ ] Still send text response if present
- **✅ Demo**:
  - Ask bot "take a screenshot" in Telegram → receive photo
  - Ask bot to send a generated PDF path → receive document
- **Risks / failure modes**:
  - Malformed XML block should fail closed (no unintended upload)
  - Multiple media paths in one response (send first, log warning for now)
  - Unsupported/unknown extensions need safe fallback (`send_document`)

## Contracts (guardrails)
- Text-only responses must continue to work unchanged
- Screenshot/file failures return clear error message, don't crash bot
- No new core screenshot tool for MVP; capture is delegated to existing screenshot skill
- No dependency on `ToolResultBlock::Image` in MVP
- Only allow local absolute file paths for MVP (no remote URL fetch in this slice)
- If URL mode is added later, note Bot API limits: 5 MB photos / 20 MB other by URL; `sendDocument` URL is currently reliable for `.pdf` and `.zip`
- Update `docs/SPEC.md` for Telegram media-response behavior

## Key decisions (decide early)
- Reuse existing screenshot skill instead of adding a new core tool
- KISS path-first integration for MVP (path parsing + file upload)
- Full screen capture flow first for MVP (window/region targeting optional)
- Route images through `sendPhoto`; route PDFs/other files through `sendDocument`

## Open questions
- Should MVP send only the first detected media path, or all paths in order?
- Should we preflight file size and fail fast (`>10MB` for photo route, `>50MB` for document route)?

## Testing
- Manual smoke demos per slice
- Unit tests for `send_photo` and `send_document` with mock server (optional)
- Integration tests:
  - screenshot path is parsed and uploaded via `send_photo`
  - PDF path is parsed and uploaded via `send_document`

## Polish phases (after MVP)

## Phase 1: Cross-platform & targeting
- Expand use of skill targeting options (`--app`, `--window-name`, `--region`, `--active-window`)
- Monitor selection behavior for multi-monitor setups
- ✅ Check-in demo: Capture specific window on supported platforms via skill flags

## Phase 2: Media handling improvements
- Compress/resize large screenshots before sending
- Support sending multiple media files in sequence
- Add caption with timestamp/context
- ✅ Check-in demo: Large screenshot auto-compressed and PDF still uploads successfully

## Later / Deferred
- **Structured media blocks**: migrate from path parsing to typed media blocks for stronger contracts
- **Richer media routing**: add `sendAudio` / `sendVideo` where beneficial (instead of defaulting to `sendDocument`)
- **Windows support**: Would require different capture tool, revisit if user requests
- **Screen recording/video**: Different feature, out of scope
- **Image annotation**: Add text/arrows to screenshots, complex UI needed
- **OCR integration**: Extract text from screenshots, separate feature
