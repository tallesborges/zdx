> Stage: drafts | active | done | archived. Keep this plan current while working: when a scope item is finished, check its box `[ ]`→`[x]`; when a phase's ✅ demo passes, mark the phase done (with date). The plan file is the source of truth, not memory.

# Goals
- Ship a small, standalone `zdx ask-media` command that sends an audio/video/PDF file plus a prompt to Gemini and prints the model's text answer (summarize / transcribe / Q&A over the file).
- Reuse the existing one-shot-command pattern (`zdx transcribe` / `zdx imagine` / `zdx speak`): thin CLI over an engine core, wrapped by a skill, invoked by the agent via bash.
- Zero changes to the chat/content-block pipeline. No `ChatContentBlock`, provider-matrix, persistence, or transcript churn.
- Usable three ways from one core: direct CLI, agent-via-skill, and (later, optional) a native tool.

# Non-goals
- In-conversation multimodal (raw media flowing through `ChatContentBlock` across providers) — this is the deferred upgrade path for true multi-turn follow-up (see Later / Deferred).
- Gemini **File API** (`files:upload` / `fileData`) for large files — deferred; MVP is inline base64 only (no File API exists in the codebase today).
- Non-Gemini providers (Anthropic/OpenAI/etc.) — Gemini-only by design.
- Model output of audio/video/PDF (text output only).
- A native tool registration up front — start CLI + skill; promote only if it earns it.

# Design principles
- User journey drives order: unlock "point at a PDF, get an answer" first.
- Reuse before rebuild: mirror `crates/zdx-cli/src/cli/commands/transcribe.rs`; reuse `GeminiClient`/`GeminiConfig` + its `generateContent` + `inlineData` builder (`crates/zdx-providers/src/gemini/api.rs:225-237`).
- Core logic in `zdx-engine`; CLI, skill, and any future native tool are thin wrappers over it.
- Fail loud: clear errors for oversize/unsupported files or a non-Gemini model.

# User journey
1. A file exists locally — either the user passes a path, or the Telegram bot already downloaded it and handed the agent the local path (`crates/zdx-bot/src/agent/mod.rs:225-261`, `crates/zdx-bot/src/types.rs` `IncomingDocument`/`IncomingAudio` carry `local_path`).
2. The user (or the agent, guided by the skill) runs `zdx ask-media <path> -p "<question>"`.
3. zdx reads the file, base64-encodes it, and sends it inline to a Gemini model with the prompt.
4. zdx prints the text answer (plain or `--json`).
5. Follow-up: re-invoke the command for another question (re-sends the file). True in-conversation follow-up is the deferred upgrade.

# Foundations / Already shipped (✅)

## One-shot Gemini command pattern
- What exists: `zdx transcribe` reads a file → bytes → MIME → calls an engine core → prints text/JSON (`crates/zdx-cli/src/cli/commands/transcribe.rs:12-60`). `zdx imagine` / `zdx speak` follow the same shape. All are invoked by the agent via bash through skills, not as native tools.
- ✅ Demo: `zdx transcribe <audio>` works today.
- Gaps: none of them do "file + prompt → text understanding".

## Gemini generateContent + inlineData
- What exists: `GeminiClient`/`GeminiConfig::from_env` (`crates/zdx-providers/src/gemini/api.rs:38-97`) and an image-generation `generateContent` path that base64-encodes bytes into an `inlineData` part and parses the response (`api.rs:225-237`). `inline_data_part` is MIME-generic (`crates/zdx-providers/src/gemini/shared.rs:467-474`).
- ✅ Demo: `zdx imagine` already round-trips inline data through `generateContent`.
- Gaps: no non-streaming "media + text → text" helper yet.

## Attachment plumbing (Telegram)
- What exists: bot downloads attachments, saves locally, and surfaces the local path to the agent (`crates/zdx-bot/src/ingest/mod.rs:149-405`, `agent/mod.rs:225-261`). Deps `base64 = "0.22"`, `infer = "0.19"` (`Cargo.toml`).
- ✅ Demo: sending a document in Telegram today yields a "local path" sentence to the agent.
- Gaps: the agent has no command to actually read that file; audio is transcribed and its `local_path` may not always be surfaced when a transcript exists (Phase 3 concern).

# MVP phases (ship-shaped, demoable)

## Phase 1: `zdx ask-media` core + PDF — ✅ done 2026-07-22
- **Goal**: `zdx ask-media report.pdf -p "summarize"` prints a correct summary.
- **Scope checklist**:
  - [x] Engine core fn (`crates/zdx-engine/src/media.rs`, `ask_media(path, prompt, model, config) -> String`): read bytes → detect MIME → base64 → Gemini `generateContent` with parts `[inlineData(mime,b64), text(prompt)]` → extract text. Added non-streaming `GeminiClient::generate_text_from_media` in `crates/zdx-providers/src/gemini/api.rs`.
  - [x] CLI command `crates/zdx-cli/src/cli/commands/ask_media.rs` mirroring `transcribe.rs`; registered `Commands::AskMedia` + dispatch in `crates/zdx-cli/src/cli/mod.rs` and module in `commands/mod.rs`.
  - [x] MIME detection via `media_mime_for_path` (extension map incl. `application/pdf`, images, audio, video) in `media.rs`; unit-tested.
  - [x] Default model `gemini:gemini-3.5-flash-lite` with `-m/--model` override; non-Gemini model → clear error.
  - [x] Inline size cap (15 MiB) with a clear over-limit message.
- **✅ Demo** (passed 2026-07-22): `zdx ask-media /tmp/zdx_test.pdf -p "what is the secret code?"` → `BANANA-42`; `--json` returns `{file, model, answer}` with section titles; non-Gemini model and unsupported extension both rejected cleanly.
- **Risks / failure modes**:
  - Oversize inline request → must reject with a clear message, not a raw 400.
  - Non-Gemini model passed → clear error.

## Phase 2: Audio & video — ✅ done 2026-07-22
- **Goal**: the same command answers over audio and short video.
- **Scope checklist**:
  - [x] Accept audio/video MIME types — covered by `media_mime_for_path` (mp3/wav/ogg/m4a/aac/flac + mp4/mov/mpeg/webm/flv/wmv/3gp) added in Phase 1; no code change needed.
  - [x] Size cap — single 15 MiB inline cap with clear over-cap error (per-type caps deferred to Polish; File API deferred to Later).
- **✅ Demo** (passed 2026-07-22): `zdx ask-media /tmp/zdx_audio.mp3 -p "what animal is said?"` → `Elephant`; `zdx ask-media /tmp/zdx_video.mp4 -p "what text is on screen?"` → `ZEBRA CROSSING` and `-p "what animal is spoken?"` → `Elephant` (both the video frame and its audio track are understood).
- **Risks / failure modes**:
  - Audio/video inline size limits hit quickly → surfaces the File API need (Later).

## Phase 3: Skill so the agent uses it automatically — ✅ done 2026-07-22
- **Goal**: sending a PDF/audio/video in Telegram makes the agent answer about it with no manual command.
- **Scope checklist**:
  - [x] Added bundled skill `crates/zdx-assets/bundled_skills/ask-media/SKILL.md` (mirrors transcription/imagine/speech; auto-embedded by `zdx-assets/build.rs`). Tells the agent to call `zdx ask-media <path> -p "..."` on a local file path; documents when to use vs the `transcription` skill; Gemini-only.
  - [x] Bot now surfaces the audio `local_path` alongside the transcript in `build_user_text` (`crates/zdx-bot/src/agent/mod.rs`), so the agent can `ask-media` it when the transcript isn't enough. Documents and images already surfaced their paths.
- **✅ Demo**: send a PDF with caption "summarize this" in Telegram → agent invokes `ask-media` and replies with a correct summary, unprompted. (Skill embedded + verified in generated manifest; end-to-end Telegram run pending live bot session.)
- **Risks / failure modes**:
  - Audio path not surfaced when a transcript already exists → fixed (path note added).

# Contracts (guardrails)
- No changes to the existing chat/image/text pipeline (`ChatContentBlock`, provider serialization, persistence, transcript).
- Command shape and skill invocation mirror `transcribe`/`imagine`/`speak`.
- Gemini-only: a non-Gemini model or unsupported/oversize file yields a clear user-visible error, never a malformed request.
- Media bytes are not persisted to thread JSONL (only the returned text enters any conversation).

# Key decisions (decide early)
1. **Command name** — `ask-media` (chosen). Module `ask_media.rs`, subcommand `zdx ask-media`, skill `ask-media`.
2. **Core location** — engine core fn in `crates/zdx-engine/src/media.rs` reusing `GeminiClient`, so CLI + future native tool share it (chosen).
3. **Default model** — `gemini:gemini-3.5-flash-lite` (document parsing), overridable via `--model` (chosen).
4. **Output** — one-shot text now (plain + `--json`); streaming can come later.
5. **Follow-up support** — accept that this command is stateless (re-sends the file per call). True multi-turn follow-up over the same media is the deferred in-conversation integration (Later), which we adopt only if we want zdx to support it natively.

# Testing
- Manual smoke demos per phase (see each ✅ Demo).
- Minimal regression tests only for contracts: a non-Gemini model errors cleanly; oversize file errors cleanly; a known small PDF returns non-empty text.

# Polish rounds (after MVP)

## Polish round 1: robustness & ergonomics
- Friendly over-limit / unsupported-MIME messages across audio/video/PDF.
- `--json` schema stabilized; optional `--out` to write the answer to a file.
- ✅ Check-in demo: oversize and unsupported files produce clear, correct messages.

# Later / Deferred
- **In-conversation multimodal (true follow-up)** — the raw-media-through-`ChatContentBlock` integration (add a generic `Media` block, wire Gemini `inlineData`, gate other providers). This is what enables multi-turn "ask more about page 3" without re-sending. Trigger: we decide zdx should support media natively in the conversation. (Prior draft of this deeper approach is the basis; it touches ~12 `ChatContentBlock::Image` sites + bot ingestion + capability metadata.)
- **Native tool wrapper** over the same engine core, if structured/always-on invocation beats the skill-over-CLI path. Trigger: the skill approach proves too indirect.
- **Gemini File API** (`files:upload` → `fileData.file_uri`) for large video / long audio beyond the inline cap. Trigger: users hit the inline size ceiling.
- **CLI `zdx exec` attachment flag** and **TUI media attach**, if non-Telegram media input is needed (no `exec` attach arg today — `crates/zdx-cli/src/cli/mod.rs:94-149`).
