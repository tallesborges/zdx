# zdx speak CLI + speech skill migration

> Source thread: `telegram--1004231855945-topic-1274` (Telegram). This is the plan to work on **now**. The sibling plan `zdx-transcribe-cli.md` covers the consistent `zdx transcribe` follow-up.

# Goals
- Add a native `zdx speak` CLI subcommand (mirroring `zdx imagine`) that turns text into a spoken audio file using a configured provider.
- Deliver the audio in the Telegram bot as a **native voice note** (OGG/Opus, waveform + playback speed), not a plain audio file or document.
- Support two providers, provider-based like STT: **OpenAI** and **Mistral/Voxtral**, with **Mistral as the default** (cheap, works well for the user).
- Migrate the existing `speech` skill so it teaches the agent to use `zdx speak` (drop the bundled Python OpenAI CLI); the skill lives inside ZDX and is the trigger layer for the bot.

# Non-goals
- No agent-callable `speak` tool and no `/speak` chat command as the primary mechanism — the trigger is CLI + skill (the `imagine` pattern).
- No TUI text-to-speech (the user explicitly does not want it).
- No changes to transcription (`zdx transcribe` is a separate plan).
- No streaming TTS, no voice cloning, no per-word timing.
- No always-on / auto-speak of every reply.

# Design principles
- User journey drives order: get a playable file out of `zdx speak` first, then make it a real Telegram voice note, then swap in the default provider, then wire the skill.
- Reuse before rebuild: mirror `zdx-engine::audio::transcribe` for the engine core and `crates/zdx-cli/src/cli/commands/imagine.rs` for the CLI surface. Reuse the existing `<media>` bot pipeline and `post_multipart` sender.
- Keep CLI glue thin; shared behavior lives in `zdx-engine` (per `crates/zdx-cli/AGENTS.md`).
- Deterministic voice-note format via ffmpeg (available on the user's machine); graceful fallback when ffmpeg is missing.

# User journey
1. In the Telegram bot, the user asks for audio (e.g. "manda a pronúncia de *triage* em áudio").
2. The `speech` skill loads (description match) and teaches the agent to run `zdx speak "triage" --out <path>.ogg` via bash.
3. The agent emits the produced file as `<media>…</media>` in its final reply.
4. The bot sends it with `sendVoice`, so the user gets a voice note with waveform and speed control.

# Foundations / Already shipped (✅)

## STT engine module (mirror source for the TTS core)
- What exists: `crates/zdx-engine/src/audio/transcribe.rs` — `transcribe_audio_if_configured(...)`, `resolve_model(...)` with precedence `ZDX_TRANSCRIPTION_MODEL` > `config.model` > `config.provider` > auto-detect, `const TRANSCRIPTION_PROVIDERS = [OpenAI, Mistral]`, `OperationCancelled`, `POST {base_url}/audio/transcriptions`, provider `resolve_api_key`/`resolve_base_url`. Module wired via `crates/zdx-engine/src/audio/mod.rs` (`pub mod transcribe;`).
- ✅ Demo: send a Telegram voice note → it is transcribed and included in the prompt.
- Gaps: no TTS counterpart yet.

## `imagine` CLI pattern (mirror source for the `speak` command)
- What exists: `Commands::Imagine { prompt, out, model, ... }` in `crates/zdx-cli/src/cli/mod.rs:163`; thin handler `crates/zdx-cli/src/cli/commands/imagine.rs` → `resolve_provider(model_input)`, dispatch per provider, write bytes to `config::paths::artifact_root()` when `--out` omitted, `println!("{}", path.display())`.
- ✅ Demo: `zdx imagine -p "a cat" --out /tmp/cat.png` writes the file and prints its path.

## Bot `<media>` output pipeline
- What exists: `parse_final_response` / `extract_media_tags` in `crates/zdx-bot/src/handlers/message.rs` parse `<media>/abs/path</media>` (and `<medias>…`). Dispatch loop: `is_image_path` → `send_photo_from_path`, else → `send_document_from_path` (`message.rs:~1586`). Senders `send_photo`/`send_document` (+ `_from_path`) built on `post_multipart(method, form)` in `crates/zdx-bot/src/telegram/mod.rs` (`send_photo` ~717, `send_document` ~768, `post_multipart` ~839).
- ✅ Demo: a reply ending with `<media>/tmp/x.png</media>` sends the image to Telegram.
- Gaps: `.ogg` currently falls through to `send_document_from_path` (arrives as a file, not a voice note).

## Provider config + base URL/key resolution
- What exists: `crates/zdx-providers/src/lib.rs` OpenAI (`OPENAI_API_KEY`/`OPENAI_BASE_URL`, `https://api.openai.com/v1`) and Mistral (`MISTRAL_API_KEY`/`MISTRAL_BASE_URL`, `https://api.mistral.ai/v1`); `TranscriptionConfig` in `crates/zdx-engine/src/config.rs:~395` exposed as `Config.transcription`, with a commented `[transcription]` block in `crates/zdx-assets/default_config.toml:~50`.
- ✅ Demo: transcription already resolves provider/key/base_url from config + env.

# MVP slices (ship-shaped, demoable)

## Slice 1: Engine `audio::speak` core (OpenAI, MP3)
- **Goal**: One shared function that synthesizes speech bytes from text, mirroring `transcribe.rs`.
- **Scope checklist**:
  - [ ] New `crates/zdx-engine/src/audio/speak.rs`; add `pub mod speak;` to `audio/mod.rs`.
  - [ ] `SpeechConfig { provider: Option<String>, model: Option<String>, voice: Option<String>, format: Option<String> }` in `config.rs`, exposed as `Config.speech`; commented `[speech]` block in `default_config.toml` mirroring `[transcription]`.
  - [ ] `const SPEECH_PROVIDERS = [OpenAI, Mistral]`; defaults: OpenAI `gpt-4o-mini-tts`, voice default (e.g. `coral`), format `mp3`.
  - [ ] `resolve_model` precedence mirrors STT: `ZDX_SPEECH_MODEL` > `config.model` > `config.provider` > auto-detect.
  - [ ] `synthesize_speech(config, &SpeechConfig, text, cancel) -> Result<SpeechAudio { bytes, mime, ext }>`: `POST {base_url}/audio/speech` JSON `{model, input, <voice-key>, response_format}`, `bearer_auth`, reuse `resolve_api_key`/`resolve_base_url` + `OperationCancelled`/`tokio::select!`. Check `status().is_success()` **before** reading the body; on error include the JSON/text body. OpenAI returns **raw audio bytes** (`response.bytes()`).
  - [ ] **Voice param key differs by provider**: OpenAI uses `voice`, Mistral uses `voice_id`. In Slice 1 (OpenAI-only) send `voice`; the provider branch in Slice 4 sets the correct key. Do not hardcode a single key for both.
  - [ ] Guards: reject empty text and cap input length.
  - [ ] Unit tests: `resolve_model` precedence; error-body surfaced on non-200.
- **✅ Demo**: a unit test / tiny harness calls `synthesize_speech("triage")` with `OPENAI_API_KEY` and gets non-empty MP3 bytes.
- **Risks / failure modes**: OpenAI TTS response is binary, not JSON — do not call `.json()` on success (Mistral differs; see Slice 4).

## Slice 2: `zdx speak` CLI subcommand
- **Goal**: Terminal-usable command that writes an audio file and prints its path, mirroring `imagine`.
- **Scope checklist**:
  - [ ] `Commands::Speak { text, out: Option<String>, model: Option<String>, voice: Option<String>, format: Option<String> }` in `cli/mod.rs`.
  - [ ] Thin handler `crates/zdx-cli/src/cli/commands/speak.rs` → `zdx_engine::audio::speak::synthesize_speech`; default output dir `config::paths::artifact_root()` (`speech/` subdir), `println!` the path.
  - [ ] Register `pub mod speak;` in `cli/commands/mod.rs`; update `crates/zdx-cli/AGENTS.md` "Where things are".
  - [ ] Integration test under `crates/zdx-cli/tests/integration/` (mock/skip network; assert arg parsing + empty-text rejection).
- **✅ Demo**: `zdx speak "triage" --out /tmp/triage.mp3` writes a playable MP3 and prints the path.
- **Risks / failure modes**: none new; follows the imagine handler shape.

## Slice 3: Voice note — ffmpeg transcode + bot `send_voice` (the money slice)
- **Goal**: `zdx speak` produces a Telegram-ready OGG/Opus by default, and the bot sends it as a voice note.
- **Scope checklist**:
  - [ ] Default `format = "ogg"` (Opus): after MP3 synthesis, transcode via ffmpeg (`-c:a libopus`) to `.ogg`. Keep the transcode helper in `zdx-engine` (so the CLI stays thin); `--format mp3` skips transcode.
  - [ ] Fallback: if ffmpeg is not on PATH, keep MP3 and log a note (voice-note UX downgrades to `sendAudio`).
  - [ ] Bot: add `send_voice` / `send_voice_from_path` in `telegram/mod.rs` (Telegram `sendVoice`, `voice` multipart part) mirroring `send_document`; and `send_audio` for the MP3 fallback (`sendAudio`).
  - [ ] Bot: add `is_audio_path` in `handlers/message.rs`; dispatch `.ogg/.oga/.opus` → `send_voice_from_path`, `.mp3/.m4a/.wav` → `send_audio`, else fall through to document.
  - [ ] Unit tests: `is_audio_path` routing; ffmpeg-missing fallback path.
- **✅ Demo**: a bot reply ending with `<media>/…/triage.ogg</media>` arrives in Telegram as a **voice note** with waveform + speed control.
- **Risks / failure modes**: Telegram `sendVoice` requires OGG/Opus specifically — ffmpeg guarantees it; fallback prevents hard failure when ffmpeg is absent.

## Slice 4: Mistral/Voxtral provider + make it the default
- **Goal**: Support Voxtral TTS and default to it.
- **Scope checklist**:
  - [ ] In `synthesize_speech`, branch response decoding by provider: **Mistral returns JSON base64 `{"audio_data": "..."}`** (not raw bytes) — decode it. OpenAI stays raw bytes.
  - [ ] **Request body key differs**: OpenAI uses `voice`, Mistral uses `voice_id`. Set the correct key in the provider branch (see Slice 1 note). Endpoint is `/v1/audio/speech` for both.
  - [ ] Default model for Mistral `voxtral-mini-tts-2603` (verify current slug against Mistral docs at build time); pin a real default `voice_id` per provider (OpenAI e.g. `coral`; Mistral preset slug from the Voxtral voice list — do not leave it unset, Voxtral requires a valid `voice_id`).
  - [ ] Auto-detect order makes Mistral the default when `MISTRAL_API_KEY` is set.
  - [ ] Confirm Mistral supports `response_format: "mp3"` so the ffmpeg step is unchanged (confirmed in docs; request the non-stream JSON shape, not the SSE `text/event-stream` variant).
  - [ ] Unit test: base64 decode path.
- **✅ Demo**: with only `MISTRAL_API_KEY` set, `zdx speak "triage"` produces audio via Voxtral; `--provider openai` still works.
- **Risks / failure modes**: Voxtral response shape + `voice_id` param differ from OpenAI — this is why it is a dedicated slice, not assumed drop-in. Mistral's TTS endpoint also runs **content moderation and rejects some inputs with `403`** — the "surface error body on non-200" logic from Slice 1 covers it; keep the body in the error message.

## Slice 5: Migrate the `speech` skill to `zdx speak`
- **Goal**: The skill becomes ZDX-native guidance that drives the CLI; drop the Python dependency.
- **Scope checklist**:
  - [ ] Rewrite the `speech` skill (`~/.zdx/skills/speech/SKILL.md`) to teach `zdx speak` invocation, when to use it (only when the user asks for audio/pronunciation), voice options, and to emit the output path as `<media>`.
  - [ ] Remove reliance on `scripts/text_to_speech.py` / `OPENAI_API_KEY`-only assumptions; keep the useful voice-direction guidance as best-effort (OpenAI `instructions`; Voxtral may ignore).
- **✅ Demo**: in the bot, "manda a pronúncia de *triage* em áudio" → skill loads → `zdx speak` runs → user receives a voice note end-to-end.
- **Risks / failure modes**: skill under/over-triggering — constrain the description to explicit audio requests.

# Contracts (guardrails)
- Existing `<media>` image/document sending must not regress; audio routing is additive.
- ffmpeg missing must degrade to `sendAudio`/MP3, never hard-fail the reply.
- Empty or oversized input text is rejected before any provider call.
- No TUI TTS surface is added.
- Bot/TUI transcription behavior is untouched by this plan.

# Key decisions (decide early)
- **Trigger = CLI + skill** (not tool, not `/speak` command). Matches `imagine`; keeps the toolset unchanged.
- **Voice-note format = OGG/Opus via ffmpeg**, MP3 intermediate; `sendAudio` fallback.
- **Default provider = Mistral/Voxtral**; OpenAI available via `--provider`/config. Both use `POST /v1/audio/speech` but diverge: OpenAI takes `voice` + returns raw bytes; Mistral takes `voice_id` + returns JSON base64 `audio_data`.
- **Core lives in `zdx-engine::audio::speak`**; CLI handler stays thin.

# Testing
- Manual smoke demo per slice (see ✅ Demo lines).
- Unit tests: `resolve_model` precedence, error-body surfacing, base64 decode (Mistral), `is_audio_path` routing, ffmpeg-missing fallback.
- CLI integration test for arg parsing + empty-text rejection.
- Final verification: `just ci` from repo root.

# Polish phases (after MVP)

## Phase 1: Voice direction passthrough
- Pass style/voice `instructions` for OpenAI TTS from the skill; document Voxtral behavior.
- ✅ Check-in demo: same text spoken with two distinct styles.

## Phase 2: Voice presets
- Named voice presets selectable via `--voice` and taught by the skill.
- ✅ Check-in demo: `zdx speak "..." --voice <preset>` changes the voice audibly.

# Later / Deferred
- Agent-callable `speak` tool — revisit if natural-language audio requests need tighter integration than the skill provides.
- TUI TTS / playback — revisit only if the user changes their mind.
- Streaming TTS and voice cloning — revisit when latency or custom voices matter.
- OGG/Opus without ffmpeg (direct provider Opus output) — revisit if removing the ffmpeg dependency becomes valuable.
