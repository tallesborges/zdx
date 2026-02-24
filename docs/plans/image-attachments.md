# Image Attachments in TUI

## Status: Slice 1 shipped ✅

## Goals
- Attach images to messages via drag-and-drop (file path paste)
- Show `[Image #N]` placeholders in input area with full placeholder UX
- Send images as `ChatContentBlock::Image` blocks with `<attached_image>` context tags
- Show `📎 N image(s)` indicator + `[Image N]` references in transcript
- Per-thread image numbering so the model can reference images across turns

## What shipped (Slice 1)

### Drag-and-drop image attachment
- Detects image file paths on paste (`.png`, `.jpg`, `.jpeg`, `.gif`, `.webp`)
- Unescapes shell-escaped paths (`\ ` → ` `, `\(` → `(`, `\)` → `)`)
- Reads file, validates size (max 20MB), base64 encodes
- Shows `[Image #N]` placeholder in input

### Placeholder UX (shared with paste placeholders)
- Cursor jumping: Left/Right arrows jump over placeholder as atomic unit
- Cursor snapping: Up/Down arrows snap cursor to placeholder end if it lands inside
- Backspace on `]`: deletes entire placeholder and removes the image
- Styled rendering: image placeholders rendered with same cyan bold style as paste placeholders
- Unified `all_placeholder_strings()` method feeds both paste and image placeholders to navigation/render

### Message sending
- `ChatMessage::user_with_images()` creates multi-block message:
  - `<attached_image path="...">` text block per image (gives model source context, uses clean unescaped path)
  - `image` block with base64 data
  - User text block with `[Image N]` references preserved (placeholders replaced with `[Image N]` on submit)
- Image counter is **per-thread**: increments across messages, resets only on `/new` or handoff submit
  - `InputMutation::ResetImageCounter` emitted by `new_thread_reset_mutations()` and handoff submit

### Transcript display
- `📎 N image(s)` indicator line above user message content
- `[Image N]` references in message text
- `image_count` field on `HistoryCell::User` for cache invalidation

### Files changed
- `crates/zdx-core/src/providers/shared.rs` — `ChatMessage::user_with_images()`
- `crates/zdx-tui/src/features/input/state.rs` — `PendingImage`, attach/take/clear/sync, placeholder navigation for images, `reset_image_counter()`
- `crates/zdx-tui/src/features/input/update.rs` — `is_image_path()`, paste detection, `build_send_effects` with images
- `crates/zdx-tui/src/features/input/render.rs` — unified placeholder collection via `all_placeholder_strings()`
- `crates/zdx-tui/src/features/transcript/cell.rs` — `image_count` field, `📎` indicator rendering
- `crates/zdx-tui/src/effects.rs` — `UiEffect::AttachImage`
- `crates/zdx-tui/src/mutations.rs` — `InputMutation::AttachImage`, `InputMutation::ResetImageCounter`
- `crates/zdx-tui/src/runtime/mod.rs` — `AttachImage` handler, `read_and_encode_image()`, clean path storage
- `crates/zdx-tui/src/update.rs` — paste returns effects
- `crates/zdx-tui/src/overlays/command_palette.rs` — `ResetImageCounter` in new thread mutations
- `crates/zdx-tui/src/overlays/file_picker.rs` — `ResetImageCounter` in apply

## Later / Deferred
- Clipboard image paste (Cmd+V after screenshot) — needs raw RGBA→PNG encoding (`arboard` returns RGBA pixels)
- Image preview overlay (click/Enter on placeholder to see image metadata)
- Image thumbnails in terminal (sixel/kitty protocol)
- Thread persistence for image references (currently images are not persisted in thread files)
