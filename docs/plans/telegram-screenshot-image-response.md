# Telegram Screenshot & Image Response

## Goals
- Agent can take screenshots when asked
- Bot can send images as Telegram responses (not just text)

## Non-goals
- Window/app-specific screenshot targeting (polish phase)
- Multiple monitor selection (polish phase)
- Image editing/annotation
- OCR or image analysis of screenshots

## Design principles
- User journey drives order
- Minimal scope per slice
- Reuse existing infrastructure (ToolResultContent already supports images)

## User journey
1. User asks bot: "take a screenshot"
2. Bot captures the screen
3. Bot sends screenshot image back to Telegram chat

## Foundations / Already shipped (✅)

### Tool system with image support
- What exists: `ToolResultContent::Blocks` supports image content blocks
- ✅ Demo: See `crates/zdx-core/src/tools/mod.rs` ToolResultBlock::Image
- ✅ **Already works**: `read` tool returns images via `ToolOutput::success_with_image` (reuse this pattern)

### Telegram client
- What exists: `TelegramClient` with `send_message`, `download_file`
- ✅ Demo: Bot sends text replies
- Gaps: No `sendPhoto` method

### Bash tool
- What exists: Can run arbitrary shell commands including `screencapture`
- ✅ Demo: `bash` tool works
- Gaps: Returns text output, not image content

## MVP slices (ship-shaped, demoable)

## Slice 1: Screenshot tool (core)
- **Goal**: Add screenshot tool that captures screen and returns base64 image
- **Scope checklist**:
  - [ ] Create `crates/zdx-core/src/tools/screenshot.rs`
  - [ ] Define `Screenshot` tool with simple schema (no parameters for MVP)
  - [ ] Use `screencapture -x -t png /tmp/screenshot.png` (macOS) or `import -window root` (Linux)
  - [ ] Read file, base64 encode, return as `ToolOutput` with image
  - [ ] Register in `ToolRegistry::builtins()` and `all_tool_names()`
  - [ ] Add to `ToolSet::Default` tool list
- **✅ Demo**: Run `zdx exec -p "take a screenshot"` → tool executes, returns image data
- **Risks / failure modes**:
  - Non-macOS systems need different capture command (detect OS)
  - Headless server has no display (return error gracefully)

## Slice 2: Telegram sendPhoto
- **Goal**: TelegramClient can send photos to chats
- **Scope checklist**:
  - [ ] Add `send_photo` method to `TelegramClient`
  - [ ] Use Telegram `sendPhoto` API with **multipart/form-data** (not JSON)
  - [ ] Decode base64 image data to bytes before sending
  - [ ] Accept `chat_id`, `photo_data: &[u8]`, `caption: Option<&str>`, `reply_to_message_id`, `message_thread_id`
  - [ ] Use `reqwest::multipart::Form` for file upload
- **✅ Demo**: Unit test or manual test sending image to Telegram chat
- **Risks / failure modes**:
  - Telegram has 10MB photo limit (add check/error)
  - Base64 decode errors (validate before send)

## Slice 3: Agent image response flow
- **Goal**: When agent produces image output, bot sends it as photo
- **Scope checklist**:
  - [ ] In `handlers/message.rs`, change `run_agent_turn_with_persist` to return `messages` (currently dropped)
  - [ ] Extract images from `ToolResult::content` blocks (check for `ToolResultBlock::Image`)
  - [ ] Alternatively: consume `ToolCompleted` events during agent turn to capture images
  - [ ] If image present: decode base64, call `send_photo` with image bytes
  - [ ] Still send text response if present
- **✅ Demo**: Ask bot "take a screenshot" in Telegram → receive photo
- **Risks / failure modes**:
  - Multiple images in one response (send first, log warning for now)
  - Large screenshots may timeout (compress or resize if needed)
  - Image bytes not persisted to thread log (acceptable for MVP)

## Contracts (guardrails)
- Text-only responses must continue to work unchanged
- Screenshot failures return clear error message, don't crash bot
- Screenshot tool in `ToolSet::Default` but can be excluded via provider `tools` config
- Update `AGENTS.md` and `docs/SPEC.md` when adding new tool

## Key decisions (decide early)
- macOS-only for MVP (screencapture), Linux support in polish
- Full screen capture only for MVP (no window targeting)
- PNG format (lossless, reasonable size)

## Open questions
- Should screenshot enforce size cap (like read's 3.75MB) before base64 encoding?
- Consider Telegram's 10MB photo limit vs PNG screenshot size on high-DPI displays

## Testing
- Manual smoke demos per slice
- Unit test for `send_photo` with mock server (optional)
- Integration test: screenshot tool returns valid PNG base64

## Polish phases (after MVP)

## Phase 1: Cross-platform & targeting
- Linux support via `import` (ImageMagick) or `gnome-screenshot`
- Window targeting parameter: `{ "target": "window" | "screen" | "selection" }`
- Monitor selection for multi-monitor setups
- ✅ Check-in demo: Screenshot specific window on both macOS and Linux

## Phase 2: Image handling improvements
- Compress large screenshots before sending
- Support sending multiple images in sequence
- Add caption with timestamp/context
- ✅ Check-in demo: Large screenshot auto-compressed, sends successfully

## Later / Deferred
- **Windows support**: Would require different capture tool, revisit if user requests
- **Screen recording/video**: Different feature, out of scope
- **Image annotation**: Add text/arrows to screenshots, complex UI needed
- **OCR integration**: Extract text from screenshots, separate feature
