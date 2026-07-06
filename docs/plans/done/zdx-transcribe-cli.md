# zdx transcribe CLI (consistency follow-up)

> Source thread: `telegram--1004231855945-topic-1274` (Telegram). Follow-up to `zdx-speak-cli-and-speech-skill.md`. Do this **after** the `zdx speak` plan so both audio directions share the same CLI + skill shape.

# Goals
- Expose the already-shipped speech-to-text engine as a native `zdx transcribe <file>` CLI subcommand (mirroring `zdx imagine` / `zdx speak`), so transcription is usable directly from the terminal and drivable by a skill — consistent with the `speak` direction.
- Optionally add/point a skill that teaches the agent to use `zdx transcribe` for on-demand transcription outside the automatic bot/TUI paths.

# Non-goals
- No change to existing **automatic** transcription behavior in the Telegram bot (voice notes) or TUI voice dictation — they keep using the shared engine unchanged.
- No new transcription providers (OpenAI + Mistral stay the supported set).
- No local/offline ASR (Parakeet) — tracked separately in the ZDX Features backlog.
- No TTS work (that is the sibling `speak` plan).

# Design principles
- User journey drives order: get `zdx transcribe <file>` printing a transcript first; teach it via a skill second.
- Reuse before rebuild: the engine already exists (`zdx-engine::audio::transcribe`); this plan only adds a **thin CLI surface** over it. No engine rewrite.
- Keep CLI glue thin; shared behavior stays in `zdx-engine` (per `crates/zdx-cli/AGENTS.md`).

# User journey
1. From the terminal (or via a skill in the bot), the user runs `zdx transcribe path/to/audio.ogg`.
2. The command resolves the configured provider/model and prints the transcript to stdout.
3. A skill can teach the agent to run it when the user asks to transcribe a specific file/attachment on demand.

# Foundations / Already shipped (✅)

## Shared STT engine
- What exists: `crates/zdx-engine/src/audio/transcribe.rs` — `transcribe_audio_if_configured(config, &TranscriptionConfig, bytes, filename, mime, cancel) -> Result<Option<String>>`, `resolve_model` precedence `ZDX_TRANSCRIPTION_MODEL` > `config.model` > `config.provider` > auto-detect, `const TRANSCRIPTION_PROVIDERS = [OpenAI, Mistral]`, `POST {base_url}/audio/transcriptions`.
- ✅ Demo: a Telegram voice note is transcribed today via this module.

## Consumers already wired (must not regress)
- What exists: bot audio ingestion `crates/zdx-bot/src/ingest/mod.rs` (+ thin wrapper `crates/zdx-bot/src/transcribe.rs`); TUI voice dictation `crates/zdx-tui/src/runtime/handlers/voice.rs`.
- ✅ Demo: Telegram voice note → transcript in prompt; TUI voice hotkey → text inserted into input.

## CLI command pattern to mirror
- What exists: `crates/zdx-cli/src/cli/commands/imagine.rs` (and, once shipped, `speak.rs`) — thin handler, `config::paths::artifact_root()` defaults, `println!` output, integration tests under `crates/zdx-cli/tests/integration/`.
- ✅ Demo: `zdx imagine -p "…" --out …` writes a file and prints its path.

# MVP slices (ship-shaped, demoable)

## Slice 1: `zdx transcribe <file>` CLI subcommand
- **Goal**: A terminal command that transcribes an audio file and prints the text.
- **Scope checklist**:
  - [x] `Commands::Transcribe { file: String, provider: Option<String>, model: Option<String>, language: Option<String> }` in `crates/zdx-cli/src/cli/mod.rs`.
  - [x] Thin handler `crates/zdx-cli/src/cli/commands/transcribe.rs`: read the file bytes + infer mime/filename, build a `TranscriptionConfig` from flags overlaid on `Config.transcription`, call `zdx_engine::audio::transcribe::transcribe_audio_if_configured`, `println!` the transcript; clear message when no provider is configured (`Ok(None)`).
  - [x] Register `pub mod transcribe;` in `cli/commands/mod.rs`; update `crates/zdx-cli/AGENTS.md` "Where things are".
  - [x] Integration test under `crates/zdx-cli/tests/integration/` (arg parsing; missing-file error; no-provider message).
- **✅ Demo**: `zdx transcribe sample.ogg` prints the transcript (or a clear "configure OpenAI/Mistral" message when unset).
- **Risks / failure modes**: none new — reuses the existing engine path.

## Slice 2 (optional): Skill to teach `zdx transcribe`
- **Goal**: On-demand transcription is discoverable by the agent for explicit file/attachment requests.
- **Scope checklist**:
  - [x] Add or extend a skill that teaches `zdx transcribe` usage and when to use it (explicit "transcribe this file" requests), without interfering with automatic bot/TUI transcription. Shipped as bundled skill `crates/zdx-assets/bundled_skills/transcription/SKILL.md`.
- **✅ Demo**: asking the bot to transcribe a specific audio file triggers `zdx transcribe` and returns the text.
- **Risks / failure modes**: skill over-triggering vs the automatic path — constrain the description to explicit file transcription.

# Contracts (guardrails)
- Automatic bot voice-note transcription and TUI voice dictation must not change behavior.
- Provider/model resolution must match the engine's existing precedence (no divergent CLI-only logic).
- Supported providers remain OpenAI + Mistral.

# Key decisions (decide early)
- **Thin CLI over the existing engine** — do not fork transcription logic into the CLI.
- **Flags overlay config** (`--provider`/`--model`/`--language` override `Config.transcription`), consistent with `resolve_model` precedence.

# Testing
- Manual smoke: `zdx transcribe <file>` with OpenAI and with Mistral configured.
- CLI integration tests: arg parsing, missing file, no-provider message.
- Final verification: `just ci` from repo root.

# Polish phases (after MVP)

## Phase 1: Output/format ergonomics
- Optional `--json` output and language auto-detect passthrough, matching other CLI commands' conventions.
- ✅ Check-in demo: `zdx transcribe --json <file>` emits structured output.

# Later / Deferred
- Local/offline ASR backend (Parakeet / `whisper-rs`) — revisit per the ZDX Features "Speech & Transcribe in zdx-core" notes when cost/privacy/offline becomes a priority.
- Diarization / timestamps surface in the CLI — revisit if a concrete need appears.
