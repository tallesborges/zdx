# Image Support Implementation Plan

**Project/feature:** Add image reading support to zdx's `read` tool, enabling the agent to read image files (JPEG, PNG, GIF, WebP) and send them to Claude for vision analysis.

**Existing state:**
- `read` tool exists (`src/tools/read.rs`) - text-only, 50KB truncation
- Anthropic provider exists (`src/providers/anthropic/`) - handles text, thinking, tool_use, tool_result blocks
- `ChatContentBlock` enum has: `Thinking`, `Text`, `ToolUse`, `ToolResult` variants
- `ToolResult` has `content: String` - **CRITICAL: must become array for images**
- `ToolOutput` envelope returns JSON data to the model

**Constraints:**
- Rust edition 2024
- Must use file magic bytes for MIME detection (not file extension)
- Supported formats: JPEG, PNG, GIF, WebP (Anthropic's supported formats)
- Base64 encoding for image data
- New dependency: `infer` crate for magic byte detection
- **3.75MB client-side image size limit** (Anthropic API limit is ~5MB for base64, raw expands ~33%)

**Success looks like:** Agent can read an image file with the `read` tool and Claude receives it as a vision-capable image block.

---

## ⚠️ Critical Architecture Issue (from Gemini review)

**Problem:** Current `ToolResult` uses `content: String`, but Anthropic API requires `content` to be an **array of content blocks** when including images:

```json
// REQUIRED format for tool_result with image:
{
  "type": "tool_result",
  "tool_use_id": "...",
  "content": [
    { "type": "text", "text": "Read image file [image/png]" },
    { "type": "image", "source": { "type": "base64", "media_type": "image/png", "data": "..." } }
  ]
}
```

**Current code (broken for images):**
```rust
// src/tools/mod.rs
pub struct ToolResult {
    pub content: String,  // ❌ Cannot hold image blocks
    ...
}

// src/providers/anthropic/client.rs  
ApiContentBlock::ToolResult {
    content: String,      // ❌ Serializes as string, not array
    ...
}
```

**Solution:** Slice 3 must refactor `ToolResult.content` to support `Vec<ContentBlock>` or an enum.

---

# Goals
- Agent can read image files (JPEG, PNG, GIF, WebP) using the `read` tool
- Images are detected by file magic bytes, not extension
- Images are base64-encoded and sent to Claude as image content blocks
- Claude can analyze/describe images read by the agent

# Non-goals
- User-attached images in prompts (separate feature)
- Image rendering in TUI (separate feature)
- Image size limits/resizing (defer to later)
- PDF or other document formats

# Design principles
- **User journey drives order**: read tool → provider integration → demo
- **Ship-first**: minimal working path, no premature abstractions
- **Content-based detection**: magic bytes, not file extensions
- **Existing patterns**: follow pi-mono's proven approach

# User journey
1. User asks agent to analyze an image file
2. Agent calls `read` tool with image path
3. Tool detects image via magic bytes, reads binary, base64-encodes
4. Tool returns `ImageContent` block alongside text description
5. Provider converts `ImageContent` to Anthropic API format
6. Claude receives and analyzes the image
7. User sees Claude's analysis

# Foundations / Already shipped (✅)

## Read tool infrastructure
- **What exists:** `src/tools/read.rs` with path resolution, error handling, truncation
- **✅ Demo:** `cargo test --package zdx -- tools::read`
- **Gaps:** Only handles text files via `fs::read_to_string`

## Anthropic provider
- **What exists:** Full streaming SSE parser, content block handling, cache control
- **✅ Demo:** `cargo test --package zdx -- providers::anthropic`
- **Gaps:** No `Image` variant in `ChatContentBlock` or `ApiContentBlock`

## Tool output envelope
- **What exists:** `ToolOutput::success(json!({...}))` pattern
- **✅ Demo:** Tool tests use this pattern
- **Gaps:** Returns JSON, need to also return structured image content for API

---

# MVP slices (ship-shaped, demoable)

## Slice 1: MIME detection from magic bytes ✅
- **Goal:** Detect image MIME type from file content, not extension
- **Scope checklist:**
  - [x] Add `infer` crate to `Cargo.toml` for magic byte detection
  - [x] Implement `detect_image_mime(path: &Path) -> Option<String>` in `src/tools/read.rs` (reads first ~4KB, detects JPEG/PNG/GIF/WebP)
  - [x] Unit tests: detect each format, return None for text files
- **✅ Demo:**
  ```bash
  cargo test --package zdx -- tools::read::tests::test_detect
  # Tests pass for .jpg, .png, .gif, .webp detection
  # Test passes for .txt returning None
  ```
- **Failure modes / guardrails:**
  - File doesn't exist → return None (let caller handle)
  - File too small to detect → return None
  - Unsupported image format → return None

## Slice 2: Read tool returns image content ✅
- **Goal:** Read tool detects images and returns them as base64
- **Scope checklist:**
  - [x] Add `ImageContent` struct to `src/core/events.rs`: `{ mime_type: String, data: String }`
  - [x] Update `ToolOutput` or add new return type for image content
  - [x] Modify `read::execute()` to:
    1. First check MIME type via `mime::detect_image_mime()`
    2. If image: check size ≤ 3.75MB, read binary, base64-encode, return `ImageContent`
    3. If not image: existing text path
  - [x] **Add 3.75MB size limit** for images (Anthropic API ~5MB limit for base64, raw files expand ~33%)
  - [x] Update tool description to mention image support
  - [x] Tests: read image file returns base64 data
  - [x] Test: image > 3.75MB returns error
- **✅ Demo:**
  ```bash
  cargo test --package zdx -- tools::read::tests::test_read_image
  # Tests: test_read_image_returns_base64, test_read_image_returns_correct_metadata,
  #        test_read_image_too_large_returns_error, test_read_text_file_no_image_content
  ```
- **Failure modes / guardrails:**
  - Image file doesn't exist → existing path_error
  - Image read fails → read_error with context
  - Image > 3.75MB → `image_too_large` error with size info

## Slice 3: Refactor ToolResult for mixed content + Image variant ✅
- **Goal:** ToolResult supports array of content blocks (text + image), add Image variant
- **Scope checklist:**
  - [x] **Refactor `ToolResult.content`** in `src/tools/mod.rs`:
    - Change from `content: String` to `content: ToolResultContent`
    - `ToolResultContent` = enum: `Text(String)` | `Blocks(Vec<ToolResultBlock>)`
    - `ToolResultBlock` = enum: `Text { text: String }` | `Image { mime_type: String, data: String }`
  - [x] Update `ToolResult::from_output()` to handle image content
  - [x] Add `ToolResult::with_image()` constructor for testing
  - [x] **Refactor `ApiContentBlock::ToolResult`** in `src/providers/anthropic/client.rs`:
    - Change `content: String` to `content: ApiToolResultContent`
    - Serialize as array when contains image: `[{type: "text", ...}, {type: "image", ...}]`
    - Serialize as string when text-only (backwards compatible)
  - [x] Update `ApiMessage::from_chat_message()` to convert `ChatContentBlock::ToolResult` with images
  - [x] Update `session.rs` to use new `ToolResultContent::Text` wrapper
  - [x] **Add test**: Verify exact JSON structure for tool_result with image matches Anthropic spec
- **✅ Demo:**
  ```bash
  cargo test --package zdx -- providers::anthropic::tests::test_tool_result
  # Verify: content is array, image block has correct source structure
  ```
- **Failure modes / guardrails:**
  - Backwards compatible: text-only tool results still work ✅
  - Invalid mime_type → API will reject (acceptable for MVP)
  - Empty data → API will reject (acceptable for MVP)

## Slice 4: End-to-end verification ✅
- **Goal:** Verify the full image flow works from read tool to API
- **Note:** The implementation was completed as part of Slice 3. This slice is just verification.
- **Scope checklist:**
  - [x] `read::execute()` returns `ToolOutput::success_with_image()` (done in Slice 2)
  - [x] `execute_tool()` calls `ToolResult::from_output()` which handles images (done in Slice 3)
  - [x] `ApiMessage::from_chat_message()` converts to correct API format (done in Slice 3)
  - [x] Manual smoke test with real API call
- **✅ Demo:**
  ```bash
  # Manual test with real API:
  cargo run -- "read the file test.png and describe what you see"
  # Claude should describe the image contents
  ```
- **Failure modes / guardrails:**
  - API rejects image → error event propagates to UI (existing error handling)
  - Image > 5MB (after base64) may hit API limits → 3.75MB client limit prevents this

---

# Contracts (guardrails)
1. **Magic byte detection**: MIME type determined by file content, never by extension
2. **Supported formats only**: JPEG, PNG, GIF, WebP - others return as text/error
3. **Base64 encoding**: All image data transmitted as base64 strings
4. **Anthropic format**: Tool result with image must be array: `[{type: "text"}, {type: "image", source: {...}}]`
5. **3.75MB size limit**: Images larger than 3.75MB rejected with actionable error
6. **Backwards compatible**: Text-only tool results continue to work unchanged

# Key decisions (decide early)

| Decision | Choice | Rationale |
|----------|--------|-----------|
| MIME detection library | `infer` crate | Lightweight, well-maintained, reads first bytes only |
| ToolResult content type | `enum { Text(String), Blocks(Vec<Block>) }` | Anthropic API requires array for images, string for text-only |
| Image size limit | 3.75MB client-side | Anthropic ~5MB base64 limit, raw expands ~33% (5MB ÷ 1.33 ≈ 3.75MB) |
| Return format | Text description + Image block | Match pi-mono: `"Read image file [mime_type]"` + image data |
| Backwards compatibility | Text-only results remain as strings | Avoids breaking existing tool result handling |

# Testing
- **Slice 1:** Unit tests for MIME detection in `src/tools/read.rs`
- **Slice 2:** Unit test for read tool image path + size limit enforcement
- **Slice 3:** Unit test for serialization to Anthropic format - **verify exact JSON structure**
- **Slice 4:** Manual smoke test with real API call

## Critical test case (Slice 3)
```rust
#[test]
fn test_tool_result_with_image_serializes_as_array() {
    // Given a ToolResult with image content
    let result = ToolResult::with_image(
        "toolu_123",
        "Read image file [image/png]",
        "image/png",
        "base64data...",
    );
    
    // When serialized to API format
    let api_block = ApiContentBlock::from(&result);
    let json = serde_json::to_value(&api_block).unwrap();
    
    // Then content is an array with text and image blocks
    let content = json.get("content").unwrap();
    assert!(content.is_array());
    assert_eq!(content.as_array().unwrap().len(), 2);
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image");
    assert_eq!(content[1]["source"]["type"], "base64");
}
```

# Risks

| Risk | Mitigation |
|------|------------|
| Large base64 strings cause memory pressure | 3.75MB limit, monitor in polish phase |
| Serde serialization of large strings is slow | Accept for MVP, optimize if measured |
| API rejects images silently | Add logging for API error responses |
| Breaking existing tool results | Keep String path for text-only, test backwards compat |

# Polish phases (after MVP)

## Phase 1: Error messages and edge cases
- [ ] Better error messages for unsupported image formats
- [ ] Handle corrupted image files gracefully
- [ ] Add image size to tool output metadata
- **✅ Check-in:** Error messages are actionable

## Phase 2: Performance
- [ ] Stream large images instead of loading fully into memory
- [ ] Consider image size limits/warnings
- **✅ Check-in:** 10MB image doesn't OOM

# Later / Deferred
- **User-attached images in prompts**: Trigger = user requests `/attach` or drag-drop
- **TUI image rendering**: Trigger = user wants inline image preview (Kitty/iTerm2)
- **Image resizing**: Trigger = API frequently rejects large images
- **PDF/document support**: Trigger = user requests document analysis
- **Multiple images per read**: Trigger = user wants to read image directories
