> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

> **Status 2026-07-21:** Phase 1 implemented + verified live. New `crates/zdx-providers/src/alibaba/image.rs` (`AlibabaImageClient`), re-exported via `alibaba.rs`; `imagine.rs` has a `ProviderKind::Alibaba` branch; `--size` maps CLI tokens (512px/1K/2K → `W*H`). `just ci-fast` + tests pass.
>
> **3.0 access unblocked 2026-08-05:** `qwen-image-3.0-pro` and `qwen-image-3.0` are now both listed for the account and generate successfully on the sync `multimodal-generation` endpoint (`dashscope-intl`). Verified end-to-end through `zdx imagine` for text-to-image and editing at `--size 1K`; `512px`/`1K`/`2K` all confirmed for both ids. No client changes were needed — the model string passes through, so this was docs + defaults only. (Prior state: 403 Access denied while the model was in invitation-only preview.) Edit model `qwen-image-edit-plus` is valid (requires 1–3 images).
>
> **Phase 2 (partial):** The `imagine` skill (`crates/zdx-assets/bundled_skills/imagine/SKILL.md`) now documents `alibaba:qwen-image-3.0-pro` (preferred), `alibaba:qwen-image-3.0`, and `alibaba:qwen-image-2.0-pro` (generate + edit, `--size` 512px/1K/2K). Registry image-capability flag + `zdx models` image filter NOT added (still no programmatic image-model listing).

# Goals
- Support **Qwen-Image** (Alibaba DashScope, targeting Qwen-Image-3.0) in `zdx imagine` for BOTH:
  - **Text-to-image**: `zdx imagine --model alibaba:<qwen-image-model> "<prompt>"`
  - **Image editing** (parity with the existing OpenAI/Gemini `--source` support): `zdx imagine --model alibaba:<qwen-image-edit-model> --source in.png "<edit instruction>"`
- Reuse the `ALIBABA_API_KEY` already wired for the Alibaba chat provider.

# Non-goals
- Chat/streaming use of Qwen-Image (image-only; not a `StreamingProvider`).
- Wan video and the **async** `text2image/image-synthesis` tier (`qwen-image`/`qwen-image-plus`) — deferred; the sync `multimodal-generation` endpoint covers both generate + edit without polling.
- China endpoints (`dashscope.aliyuncs.com`).
- Registering Qwen-Image in the chat model picker (image models run via `zdx imagine --model`).

# Design principles
- User journey drives order: one model, generate → save, then edit → save.
- **Reuse before rebuild**: reuse `imagine.rs` dispatch, `--source` loading (already present), the `GenerateImageResponse`/file-writing pipeline, and the `ProviderKind::Alibaba` API key. Add only a DashScope image client.
- **Prefer the sync `multimodal-generation` endpoint** — it handles text-to-image AND editing in one uniform request shape, is synchronous (no poll loop), and takes base64 data URIs for local files.
- Keep the image path separate from the chat client (different base URL `/api/v1`, different request/response).

# User journey
1. User sets `ALIBABA_API_KEY` (already wired).
2. Generate: `zdx imagine --model alibaba:qwen-image-2.0-pro "a newspaper front page ..."`.
3. Edit: `zdx imagine --model alibaba:qwen-image-edit-plus --source photo.jpg "make the dress red"`.
4. zdx calls the sync endpoint, gets result image URL(s), downloads them, writes files, prints paths.

# Verified facts (Alibaba Model Studio docs, 2026-06/07)
- Image models are **DashScope-native only** (no `compatible-mode`); base `https://dashscope-intl.aliyuncs.com/api/v1`; auth `Authorization: Bearer <ALIBABA/DASHSCOPE key>`.
- **Sync `multimodal-generation` (primary — does BOTH generate + edit):**
  - `POST /services/aigc/multimodal-generation/generation` (no async header). "Asynchronous interfaces are not supported."
  - Models: `qwen-image-2.0-pro`, `qwen-image-2.0`, `qwen-image-edit-max`, `qwen-image-edit-plus` (support 1–6 output images; edit takes 1–3 input images).
  - Body: `{ "model": "...", "input": { "messages": [ { "role": "user", "content": [ {"image": "<url|base64>"}, ..., {"text": "<prompt or edit instruction>"} ] } ] }, "parameters": { "n": 1, "size": "1328*1328", "negative_prompt": " ", "prompt_extend": true, "watermark": false } }`.
  - Text-to-image = `content` is just `[{"text": ...}]`; editing = prepend 1–3 `{"image": ...}` entries.
  - Input images: public URL, OSS URL, or **base64 data URI** `data:<mime>;base64,<data>` (≤10MB; JPG/PNG/WEBP/etc.) — base64 fits local `--source` files.
  - Response: result image **URL(s)** under `output.choices[].message.content[].image` → must download bytes.
  - Size uses `WIDTH*HEIGHT` (star), e.g. `1024*1024`.
- **Async `text2image/image-synthesis` (deferred):** `qwen-image`/`qwen-image-plus`, submit + poll `/tasks/{id}` — generation only, no editing. Not needed for MVP.
- **Unknown to confirm (Phase 0):** the exact DashScope model id(s) for Qwen-Image-3.0 and its edit variant, and that they are on the sync `multimodal-generation` endpoint.

# Foundations / Already shipped (✅)
## `zdx imagine` pipeline + `--source`
- What exists: `crates/zdx-cli/src/cli/commands/imagine.rs` — `run()` dispatches by `provider_selection.kind` (Gemini/OpenAI/OpenAICodex, else bail); `load_source_images()` already reads `--source` files into `{mime_type, data}`; providers return images (bytes) + `text_parts`; `resolve_output_paths()` + `fs::write` save + print.
- ✅ Demo: `zdx imagine --model gemini:... --source a.png "..."` edits and writes a file.
- Gaps: no Alibaba branch; DashScope returns a **URL** (must download) and wants source images as **base64 data URIs**.

## Alibaba API key wiring
- What exists: `ProviderKind::Alibaba` + `config.providers.alibaba` (env `ALIBABA_API_KEY`).
- Gaps: `providers.alibaba.effective_base_url()` is the compatible-mode URL — wrong for images; the image client uses its own `/api/v1` base URL and only borrows the key.

## Per-provider image client precedent
- What exists: `crates/zdx-providers/src/{openai/image_generation.rs, gemini/api.rs}` — `generate_images()` + request build + response parse + tests. All synchronous; none download a URL.

# MVP phases (ship-shaped, demoable)

## Phase 0: Confirm model ids + endpoint (spike)
- **Goal**: Pin the Qwen-Image-3.0 generate + edit model ids and confirm the sync `multimodal-generation` endpoint.
- **Scope checklist**:
  - [ ] With `ALIBABA_API_KEY`, `curl` the sync endpoint to confirm a working generate model id (e.g. `qwen-image-2.0-pro` or a `qwen-image-3.0*` id) and an edit model id (`qwen-image-edit-plus`/`-max`).
  - [ ] Confirm result image URL location and `size`/`n`/`prompt_extend` param names.
- **✅ Demo**: raw `curl` returns a result image URL for both a generate and an edit request.
- **Risks**: if 3.0 is async-only, use `qwen-image-2.0-pro`/`qwen-image-edit-*` for the sync MVP and revisit async separately.

## Phase 1: DashScope sync image client — generate + edit (end-to-end)
- **Goal**: `zdx imagine --model alibaba:<model> "..."` generates, and `--source` edits, writing files.
- **Scope checklist**:
  - [ ] Promote `crates/zdx-providers/src/alibaba.rs` → `alibaba/{mod.rs, image.rs}` (mod.rs re-exports the existing chat client); add `AlibabaImageClient` + `AlibabaImageGenerationOptions { size, n, source_images, negative_prompt, prompt_extend }` + `AlibabaGenerateImageResponse { images, text_parts }`.
  - [ ] `generate_images()`: build the `multimodal-generation` body (source images → base64 `data:` URIs prepended to `content`, then the `{text}`), POST synchronously, parse `output.choices[].message.content[].image` URL(s), **download** each URL → bytes (mime from content-type/extension).
  - [ ] Image base URL: own constant `https://dashscope-intl.aliyuncs.com/api/v1` + env `ALIBABA_IMAGE_BASE_URL`; key via `ProviderKind::Alibaba.resolve_api_key()`.
  - [ ] Size mapping `1024x1024` → `1024*1024`.
  - [ ] Export types from `zdx-providers` + engine facade.
  - [ ] `imagine.rs`: add `ProviderKind::Alibaba => generate_alibaba_images(...)` arm; map source images (already loaded) into base64; update the bail message.
  - [ ] Tests: request build for generate vs edit (content ordering), base64 data-URI formatting, size mapping, response URL extraction, error/`FAILED` surfacing.
- **✅ Demo**:
  - Generate: `zdx imagine --model alibaba:qwen-image-2.0-pro "a red bicycle"` prints a valid saved image path.
  - Edit: `zdx imagine --model alibaba:qwen-image-edit-plus --source bike.png "make it blue"` writes the edited image.
- **Risks / failure modes**:
  - Result URL download is a second network hop; handle failure clearly.
  - Large base64 source images (10MB cap) — validate size before send.
  - Don't reuse the chat compatible-mode base URL.

## Phase 2: Ergonomics + registry entry
- **Goal**: Discoverable + documented + validated inputs.
- **Scope checklist**:
  - [ ] Register the Qwen-Image generate + edit model ids in the registry with an image capability flag (mirror gemini/openai image entries).
  - [ ] `-n`/multiple outputs if trivial (endpoint supports 1–6); validate `--size` tokens and source count (1–3 for edit).
  - [ ] Update the `imagine` skill/docs with the Alibaba generate + edit examples.
- **✅ Demo**: help/skill list Qwen-Image; invalid `--size`/too many sources give clear errors.

# Contracts (guardrails)
- Existing Gemini/OpenAI/Codex image generation + editing unchanged.
- Chat `alibaba:` provider unchanged (image path fully separate).
- Bounded network behavior; no secrets logged.

# Key decisions (decide early)
- **Endpoint**: use the **sync `multimodal-generation`** endpoint as the single path for both generate and edit (no async polling). Async `text2image` is deferred.
- **Model ids** (Phase 0): confirm the 3.0 generate + edit ids; fall back to `qwen-image-2.0-pro` / `qwen-image-edit-plus` if 3.0 isn't on the sync endpoint yet.
- **Base URL**: dedicated image base URL (`/api/v1`) + `ALIBABA_IMAGE_BASE_URL`, key borrowed from the Alibaba chat provider.
- **Module layout**: promote `alibaba.rs` → `alibaba/{mod.rs,image.rs}` so chat + image live together.

# Testing
- Unit: generate vs edit request build, base64 data-URI formatting, size mapping, response URL parse, error surfacing (no live calls).
- Manual smoke: one live generate + one live edit per Phase 1.

# Polish rounds (after MVP)
## Polish round 1: async tier + multi-image fusion
- Add the async `text2image/image-synthesis` path for `qwen-image`/`qwen-image-plus` (submit + poll) if a pure-gen model is wanted.
- Multi-image fusion (2–3 source images) niceties + `negative_prompt`/`prompt_extend` flags.
- ✅ Check-in demo: fuse two source images into one output.

# Later / Deferred
- Wan text-to-video / image-to-video.
- China endpoints.
- OSS temporary-URL upload path (only base64 for now).
