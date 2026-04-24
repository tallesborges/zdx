# Goals
- Enable `zdx imagine --source ...` for `openai:gpt-image-2` and `openai-codex:gpt-image-2`.
- Reuse the existing OpenAI Responses API image-generation path for edits instead of adding the multipart image-edits endpoint.
- Keep Gemini image editing working while sharing local source-image loading across Gemini, OpenAI, and OpenAI Codex.
- Preserve the existing text-to-image behavior and SSE parser behavior for OpenAI/Codex.

# Non-goals
- Switching OpenAI/Codex image editing to `/v1/images/edits`.
- Adding image masks, inpainting-specific flags, or provider-specific edit UI beyond existing `--source`.
- Adding automatic prompt rewriting unless live testing proves it is required.
- Moving all image source types into `zdx-types` in the MVP.

# Design principles
- User journey drives order
- Ship-first: make one real `--source` edit command work before adding polish.
- Reuse the shipped Responses API path in `crates/zdx-providers/src/openai/image_generation.rs:32-59`.
- Keep provider entrypoints thin because OpenAI and Codex already call the shared builder/parser at `crates/zdx-providers/src/openai/api.rs:134-159` and `crates/zdx-providers/src/openai/codex.rs:184-218`.

# User journey
1. User has OpenAI API-key auth or OpenAI Codex OAuth auth configured.
2. User runs `zdx imagine --model openai-codex:gpt-image-2 --source input.png -p "<edit instruction>"`.
3. zdx loads the local source image and MIME type using the same path/MIME rules Gemini already uses at `crates/zdx-cli/src/cli/commands/imagine.rs:80-100`.
4. zdx sends a Responses API request with prompt text plus `input_image` data URL content blocks.
5. The hosted `image_generation` tool runs in edit mode and streams image events.
6. zdx parses final image output, writes the image file, and prints the path as it already does in `crates/zdx-cli/src/cli/commands/imagine.rs:51-68`.

# Foundations / Already shipped (✅)

## OpenAI Responses image-generation helper
- What exists: `OpenAIImageGenerationOptions`, request construction, model aliasing, and SSE parser live in `crates/zdx-providers/src/openai/image_generation.rs:12-59` and `crates/zdx-providers/src/openai/image_generation.rs:102-148`.
- ✅ Demo: Existing text-to-image works through `openai:gpt-image-2` and `openai-codex:gpt-image-2` using the hosted `image_generation` tool.
- Gaps: Options only include `size`; request content only includes `input_text`, so source images cannot be sent yet.

## Shared OpenAI/Codex provider entrypoints
- What exists: OpenAI API-key image generation calls `build_image_generation_request` and `parse_image_generation_sse_response` at `crates/zdx-providers/src/openai/api.rs:134-159`.
- What exists: OpenAI Codex image generation calls the same shared helper at `crates/zdx-providers/src/openai/codex.rs:184-218`.
- ✅ Demo: One provider helper change affects both OpenAI API-key and Codex flows.
- Gaps: Neither entrypoint passes source image data because the shared options type does not expose it yet.

## Gemini source-image CLI loading
- What exists: `generate_gemini_images` normalizes each `--source` path, infers MIME by extension, reads bytes, and builds Gemini `SourceImage` values at `crates/zdx-cli/src/cli/commands/imagine.rs:80-100`.
- ✅ Demo: `zdx imagine --model gemini:... --source input.png -p "<edit instruction>"` uses this path.
- Gaps: Loading is inline and Gemini-specific, while OpenAI/Codex currently reject `--source` in `openai_family_image_options` at `crates/zdx-cli/src/cli/commands/imagine.rs:183-199`.

## OpenAI image SSE parser
- What exists: The parser collects partial previews and final images, then prefers final images over partial fallback at `crates/zdx-providers/src/openai/image_generation.rs:102-134`.
- ✅ Demo: Existing parser tests cover done items, partial fallback, and final-over-partial behavior at `crates/zdx-providers/src/openai/image_generation.rs:286-428`.
- Gaps: No parser changes are expected for edit support unless live edit responses use a new event shape.

# MVP slices (ship-shaped, demoable)

## Slice 1: Add OpenAI source-image options and edit request shape
- **Goal**: Build a valid Responses API edit request for both OpenAI and Codex without touching CLI behavior yet.
- **Scope checklist**:
  - [ ] Add `OpenAIImageInput { mime_type: String, data: Vec<u8> }` beside `OpenAIImageGenerationOptions` in `crates/zdx-providers/src/openai/image_generation.rs:12-17`.
  - [ ] Extend `OpenAIImageGenerationOptions` with `source_images: Vec<OpenAIImageInput>`.
  - [ ] Change `build_image_generation_request` at `crates/zdx-providers/src/openai/image_generation.rs:32-59` to build a mutable content array.
  - [ ] Keep the first content item as `{ "type": "input_text", "text": prompt }`.
  - [ ] For each source image, append `{ "type": "input_image", "image_url": "data:<mime>;base64,<data>" }`.
  - [ ] When `source_images` is non-empty, add `"action": "edit"` to the `image_generation` tool object.
  - [ ] Leave generation requests unchanged when `source_images` is empty.
- **✅ Demo**: A unit test proves `build_image_generation_request("gpt-image-2", ..., source_images=[png])` emits prompt text, a data URL `input_image`, `tools[0].action == "edit"`, and `tool_choice.type == "image_generation"`.
- **Risks / failure modes**:
  - Public `/responses` or private `/codex/responses` may require a different `input_image` field name than `image_url`.
  - `action: "edit"` may be rejected if the backend expects `auto`; adjust only after a live schema error.

## Slice 2: Share CLI source-image loading across providers
- **Goal**: Remove Gemini-only source loading so all supported image providers can consume `--source` consistently.
- **Scope checklist**:
  - [ ] Add a private CLI-local `LoadedSourceImage { mime_type: String, data: Vec<u8> }` in `crates/zdx-cli/src/cli/commands/imagine.rs`.
  - [ ] Extract the path normalization, MIME inference, and `fs::read` logic from `crates/zdx-cli/src/cli/commands/imagine.rs:80-100` into `load_source_images(source: &[String]) -> Result<Vec<LoadedSourceImage>>`.
  - [ ] Update `generate_gemini_images` to map `LoadedSourceImage` into `Gemini SourceImage`.
  - [ ] Keep existing Gemini behavior and error messages for unsupported formats and unreadable files.
- **✅ Demo**: Gemini `--source` still works, and source-image loading code exists in one helper used before provider-specific mapping.
- **Risks / failure modes**:
  - Error context changes could make CLI failures less actionable.
  - Accidentally moving provider-specific types into CLI helper could make future providers harder to add.

## Slice 3: Enable `--source` for OpenAI and OpenAI Codex
- **Goal**: Make the user-visible edit journey work through the existing `zdx imagine` command.
- **Scope checklist**:
  - [ ] Remove the `--source` rejection from `openai_family_image_options` at `crates/zdx-cli/src/cli/commands/imagine.rs:183-190`.
  - [ ] Keep the `--aspect` rejection because OpenAI/Codex currently use `--size` only at `crates/zdx-cli/src/cli/commands/imagine.rs:191-196`.
  - [ ] Map loaded source images into `OpenAIImageInput` for `OpenAIImageGenerationOptions`.
  - [ ] Preserve default OpenAI image size behavior from `DEFAULT_OPENAI_RESPONSES_IMAGE_SIZE` at `crates/zdx-cli/src/cli/commands/imagine.rs:17`.
  - [ ] Preserve OpenAI size mapping in `openai_family_image_size` at `crates/zdx-cli/src/cli/commands/imagine.rs:203-211`.
- **✅ Demo**: `zdx imagine --model openai-codex:gpt-image-2 --source input.png --size 1K -p "Edit the provided image by changing only the background"` writes an output image path.
- **Risks / failure modes**:
  - Large local images may produce slow or rejected JSON requests; do not add new size validation in MVP unless the API returns a clear required limit.
  - Multiple `--source` files may be accepted by request shape but model quality may vary.

## Slice 4: Verify API behavior and prompt guidance
- **Goal**: Confirm the edit request works against both provider backends and identify whether prompt wording needs documentation or code changes.
- **Scope checklist**:
  - [ ] Run targeted provider tests for request JSON and existing SSE parser behavior.
  - [ ] Live smoke `openai-codex:gpt-image-2` first because Codex was already verified for hosted image generation.
  - [ ] Live smoke `openai:gpt-image-2` through the public `/responses` path.
  - [ ] Test delta-style edit prompts such as “Change only the background; preserve subject, pose, clothing, and framing.”
  - [ ] If the model ignores sources, test explicit wording such as “Use the attached/source image as the base image.”
  - [ ] Only add automatic prompt rewriting if repeated live tests show user prompts are consistently insufficient.
- **✅ Demo**: Both `openai-codex:gpt-image-2` and `openai:gpt-image-2` produce edited images from the same local source image and delta prompt.
- **Risks / failure modes**:
  - API accepts request but returns only text; existing CLI already fails with model text at `crates/zdx-cli/src/cli/commands/imagine.rs:43-48`.
  - API streams a new event shape; parser may need a narrowly scoped addition in `crates/zdx-providers/src/openai/image_generation.rs:102-148`.

# Contracts (guardrails)
- Existing OpenAI/Codex text-to-image must keep using the Responses hosted `image_generation` tool through `build_image_generation_request` at `crates/zdx-providers/src/openai/image_generation.rs:32-59`.
- Existing parser behavior must continue preferring final images over partial previews as implemented at `crates/zdx-providers/src/openai/image_generation.rs:102-134`.
- `gpt-image-2` must continue mapping to the Responses orchestration model through `responses_model_for_image_generation` at `crates/zdx-providers/src/openai/image_generation.rs:62-68`.
- `--aspect` remains unsupported for OpenAI/Codex until there is an explicit provider mapping.
- `--size 1K`, `2K`, and `4K` mappings must remain as implemented in `crates/zdx-cli/src/cli/commands/imagine.rs:203-211`.
- CLI output remains script-friendly: generated file paths are printed to stdout after file writes in `crates/zdx-cli/src/cli/commands/imagine.rs:51-68`.

# Key decisions (decide early)
- Use Responses `input_image` content blocks with base64 data URLs for both OpenAI API-key and OpenAI Codex.
- Use `image_url` as the initial data URL field name for `input_image`; revisit only if live API errors prove Codex/public Responses require a different shape.
- Set hosted tool `action` to `"edit"` when source images are present; test `"auto"` only if `"edit"` is rejected.
- Keep `OpenAIImageInput` provider-local for MVP instead of moving Gemini `SourceImage` into `zdx-types`.
- Keep prompt text user-controlled in MVP; investigate prompt wording through smoke tests before adding any automatic prompt wrapper.

# Testing
- Manual smoke demos per slice.
- Minimal regression tests only for contracts.
- Provider unit test for generation request shape remaining unchanged with `source_images: []`.
- Provider unit test for edit request shape with `input_text`, `input_image` data URL, and `tools[0].action == "edit"`.
- Existing SSE parser tests should continue passing after options/request changes.
- Targeted local checks before live smoke:
  - `cargo test -p zdx-providers image_generation_request`
  - `cargo test -p zdx-cli`

# Polish phases (after MVP)

## Phase 1: Error and schema hardening
- Improve API error context if public `/responses` and private `/codex/responses` diverge on `input_image` field names or tool action values.
- Add a parser fixture only if live edit responses expose a new final-image event shape.
- ✅ Check-in demo: A failed schema request surfaces the exact provider error, and successful edit responses still write one final image.

## Phase 2: Source-image limits and UX
- Add user-facing limits only if live API behavior exposes a concrete file-size or count constraint.
- Keep validation at the CLI/provider boundary, not in shared parser code.
- ✅ Check-in demo: Oversized or unsupported source files fail before request send with a clear message.

## Phase 3: Shared image input type cleanup
- If more providers need the same source-image shape, move the plain `{ mime_type, data }` type to a shared crate following provider crate conventions.
- Avoid this until duplication creates real maintenance cost.
- ✅ Check-in demo: Gemini/OpenAI/Codex still compile and source loading maps through one shared value type.

# Later / Deferred
- Multipart `/v1/images/edits`: revisit only if Responses edit support fails for either OpenAI or Codex.
- Image masks or inpainting flags: revisit when there is a user-visible CLI contract for masks.
- Automatic prompt rewriting: revisit only if live tests prove delta prompts are consistently mishandled.
- TUI image-edit workflow: revisit separately from the `zdx imagine` CLI path.