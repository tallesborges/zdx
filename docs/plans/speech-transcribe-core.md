# Goals
- Unified `transcribe` module in zdx-core: any consumer (bot, cli, tui) can transcribe audio bytes → text
- `speech` tool in zdx-core: agent can invoke TTS to generate audio files (OpenAI TTS API)
- Bot migrates from inline transcription to the shared zdx-core module
- Speech skill continues to instruct the agent when to generate audio; the tool is the execution mechanism

# Non-goals
- Streaming audio playback in TUI
- Custom voice creation / voice cloning
- Non-OpenAI TTS providers (Mistral TTS, ElevenLabs, etc.)
- Real-time speech-to-text (live mic input)
- Audio format conversion (rely on API output formats)

# Design principles
- User journey drives order
- Extract before adding: move existing bot transcription logic to core first, then build speech on top
- Keep it simple: raw reqwest + serde, no SDK wrappers
- UI-agnostic core: no Telegram-specific types in zdx-core modules

# User journey
1. User sends an audio message to the bot → bot transcribes it automatically using zdx-core
2. User asks the agent (bot/cli/tui) to "read this aloud" or "generate audio for X" → agent invokes the speech tool → audio file is saved and path returned
3. Bot sends the generated audio file back to the user; CLI/TUI prints the file path

# Foundations / Already shipped (✅)

## Bot transcription (inline)
- What exists: `crates/zdx-bot/src/transcribe.rs` — full OpenAI/Mistral Whisper transcription via multipart upload. Config-driven model resolution with env var override. Used by `ingest/mod.rs` to auto-transcribe every audio attachment.
- ✅ Demo: send a voice message to the bot → get transcript in agent context
- Gaps: logic is bot-specific, references `config.telegram.transcription`, not reusable from cli/tui

## Speech skill (external Python)
- What exists: `~/.zdx/skills/speech/SKILL.md` instructs the agent to run `scripts/text_to_speech.py` (Python, OpenAI SDK). Generates MP3/WAV files.
- ✅ Demo: ask agent "generate speech for hello world" → runs Python script → MP3 file
- Gaps: requires Python + openai package; not a native tool; no structured tool output

## Tool system
- What exists: `crates/zdx-core/src/tools/` — each tool is a module with `definition() -> ToolDefinition` and `execute(input, ctx) -> ToolOutput`. Tools registered in `mod.rs`.
- ✅ Demo: agent uses Web_Search, Grep, etc.
- Gaps: none relevant

# MVP slices (ship-shaped, demoable)

## Slice 1: Extract transcribe module to zdx-core
- **Goal**: Move transcription logic from bot to core so any consumer can call it
- **Scope checklist**:
  - [ ] Create `crates/zdx-core/src/audio/mod.rs` with `pub mod transcribe;`
  - [ ] Create `crates/zdx-core/src/audio/transcribe.rs` — extract `transcribe_audio_if_configured`, `transcribe_audio`, `resolve_model`, `TranscriptionRequest` from bot. Replace `config.telegram.transcription` access with a simple `TranscribeConfig` struct param (model, language)
  - [ ] Add public `TranscribeConfig` that bot can construct from its telegram config
  - [ ] Update `crates/zdx-bot/src/transcribe.rs` to be a thin wrapper calling `zdx_core::audio::transcribe::transcribe(config, bytes, filename, mime)`
  - [ ] Update `crates/zdx-core/AGENTS.md` with new files
- **✅ Demo**: bot still transcribes audio messages exactly as before (regression check: send voice message)
- **Risks / failure modes**:
  - Config shape mismatch — mitigate by keeping `TranscribeConfig` minimal (provider kind, model, language, api_key, base_url)

## Slice 2: Speech tool in zdx-core
- **Goal**: Agent can invoke a `Speech` tool to generate audio via OpenAI TTS API
- **Scope checklist**:
  - [ ] Create `crates/zdx-core/src/audio/speech.rs` — OpenAI TTS via reqwest (POST to `/audio/speech`, JSON body with `model`, `input`, `voice`, `instructions`, `response_format`)
  - [ ] Create `crates/zdx-core/src/tools/speech.rs` — tool definition + execute function
    - Input schema: `text` (required), `voice` (optional, default "cedar"), `instructions` (optional), `filename` (optional)
    - Output: saves audio file to working dir, returns path in ToolOutput
  - [ ] Register tool in `crates/zdx-core/src/tools/mod.rs`
  - [ ] Update `crates/zdx-core/AGENTS.md`
- **✅ Demo**: in CLI/TUI, ask agent "generate speech saying hello world" → tool invoked → MP3 file created → path shown
- **Risks / failure modes**:
  - OpenAI API key not set → clear error message ("Speech requires OPENAI_API_KEY")
  - Large text input → API has 4096 char limit; return error for now, chunking deferred

## Slice 3: Bot sends speech audio back
- **Goal**: When the agent generates speech in bot context, send the audio file to the Telegram chat
- **Scope checklist**:
  - [ ] Add `send_audio` method to `TelegramClient` (POST sendAudio with file upload)
  - [ ] In bot agent/handler flow, detect speech tool output (file path ending in audio extension) and send as audio message
  - [ ] Update `crates/zdx-bot/AGENTS.md`
- **✅ Demo**: tell bot "say hello in audio" → bot generates speech → sends audio file in chat
- **Risks / failure modes**:
  - File too large for Telegram (50MB limit) — unlikely for TTS, but add size check

# Contracts (guardrails)
- Bot audio transcription must keep working identically after Slice 1 (same providers, same config keys, same behavior)
- Speech tool must require `OPENAI_API_KEY`; fail clearly if missing
- Speech tool output must be a valid audio file at the returned path
- Core modules must remain UI-agnostic (no Telegram types)

# Key decisions (decide early)
- **TranscribeConfig shape**: flat struct with `model: Option<String>`, `language: Option<String>` — provider/key resolution stays in core using existing `Config` (passed directly)
- **Speech output location**: use `ToolContext.working_dir` + generated filename (e.g., `speech_<hash>.mp3`)
- **Speech model default**: `gpt-4o-mini-tts-2025-12-15` (matches skill default)
- **Voice default**: `cedar` (matches skill default)

# Testing
- Manual smoke demos per slice
- Slice 1: send voice message to bot, verify transcript unchanged
- Slice 2: `just run`, ask agent to generate speech, verify file exists and plays
- Slice 3: tell bot to speak, verify audio appears in chat
- No automated tests initially (early-stage alpha convention)

# Polish phases (after MVP)

## Phase 1: Speech skill alignment
- Update speech skill SKILL.md to prefer the native tool over the Python script
- Remove Python script dependency from skill instructions
- ✅ Check-in demo: speech skill triggers native tool instead of Python CLI

## Phase 2: Transcribe as a tool
- Add a `Transcribe` tool so the agent can explicitly transcribe audio files (not just auto-transcribe in bot)
- Useful for CLI workflows: "transcribe this audio file"
- ✅ Check-in demo: in CLI, ask agent to transcribe a local .mp3 file

# Later / Deferred
- **Non-OpenAI TTS providers** — revisit if users request alternatives or cost becomes a concern
- **Audio chunking for long text** — revisit when users hit the 4096 char limit
- **Streaming TTS** — revisit if real-time playback is needed in TUI
- **Audio format conversion** — revisit if consumers need formats the API doesn't support
- **Batch speech generation** — revisit if bot/automation use cases emerge
