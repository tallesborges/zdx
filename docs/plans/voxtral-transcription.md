# Voxtral Mini Transcription Support

## Goals
- Enable Mistral Voxtral Mini as an alternative audio transcription provider for zdx-bot
- Allow users to configure transcription provider via config or environment variables
- Maintain backward compatibility with existing OpenAI Whisper transcription

## Non-goals
- Streaming transcription support (Voxtral supports it, but not needed for Telegram bot use case)
- Timestamp/segment extraction (can be added later)
- Auto-detection of optimal provider based on audio characteristics
- Voxtral chat/audio-understanding features (transcription-only for now)

## Design principles
- User journey drives order
- Minimal config changes - one new provider, one new setting
- Environment variable override for quick testing before committing to config
- Don't break existing OpenAI users

## User journey
1. User sets `MISTRAL_API_KEY` environment variable (or adds to config)
2. User optionally sets transcription provider to "mistral" in config
3. User sends voice/audio message to Telegram bot
4. Bot transcribes using Voxtral and responds with context of the transcription

## Foundations / Already shipped (✅)

### OpenAI Whisper transcription
- What exists: Full transcription pipeline in `crates/zdx-bot/src/transcribe.rs`
- ✅ Demo: Send voice message to bot with `OPENAI_API_KEY` set → receive transcribed response
- Gaps: Hardcoded to OpenAI, no provider abstraction

### Audio ingestion pipeline
- What exists: `crates/zdx-bot/src/ingest/mod.rs` handles voice, audio messages, audio documents
- ✅ Demo: Bot accepts voice messages, downloads files, passes to transcription
- Gaps: None for this feature

### Provider config pattern
- What exists: `ProvidersConfig` in `config.rs` with per-provider `ProviderConfig` structs
- ✅ Demo: `config.providers.openai.api_key` works, env var fallbacks work
- Gaps: No `mistral` provider entry yet

## MVP slices (ship-shaped, demoable)

### Slice 1: Mistral transcription via env var (quick win)
- **Goal**: Voxtral transcription works with just `MISTRAL_API_KEY` env var
- **Status**: ✅ Implemented
- **Implementation**:
  - Added `TranscriptionProvider` enum in `transcribe.rs`
  - Updated `transcribe_audio_if_configured()` to support provider selection
  - Auto-detection: prefers OpenAI (backward compatible), falls back to Mistral if only Mistral key available
  - Model defaults: `voxtral-mini-latest` for Mistral, `whisper-1` for OpenAI
  - Single generic `transcribe_audio()` function handles both providers with `provider_name` param for error messages
- **Demo**: ✅ Verified
  ```bash
  export MISTRAL_API_KEY="sk-..."
  cargo run -p zdx -- bot
  # Send voice message → transcription appears in response
  ```

### Slice 2: Add Mistral provider config
- **Goal**: Users can configure Mistral via `config.toml` instead of env vars
- **Status**: ✅ Implemented
- **Implementation**:
  - Added `mistral: ProviderConfig` to `ProvidersConfig` struct in `config.rs`
  - Added `default_mistral_provider()` function
  - Added `mistral_api_key()` and `mistral_base_url()` helpers that check config then env var
  - Updated `default_config.toml` with `[providers.mistral]` section
- **Demo**: ✅ Verified
  ```toml
  # config.toml
  [providers.mistral]
  api_key = "sk-..."
  ```
  ```bash
  cargo run -p zdx -- bot
  # Send voice message → works with config-based key
  ```

### Slice 3: Explicit transcription provider selection
- **Goal**: Users can explicitly choose between OpenAI and Mistral for transcription
- **Status**: ✅ Implemented
- **Implementation**:
  - Added `TranscriptionConfig` struct with `provider`, `model`, `language` fields
  - Added `transcription: TranscriptionConfig` to `Config`
  - Provider resolution: `ZDX_TRANSCRIPTION_PROVIDER` env var > `config.transcription.provider` > auto-detect (API key availability)
  - Default provider: OpenAI (backward compatible)
  - Added `[transcription]` section to `default_config.toml`
  - Added test `test_transcription_config_defaults`
- **Demo**: ✅ Verified
  ```toml
  # config.toml
  [transcription]
  provider = "mistral"
  model = "voxtral-mini-latest"
  
  [providers.mistral]
  api_key = "sk-..."
  ```
  ```bash
  cargo run -p zdx -- bot
  # Send voice message → Voxtral transcription works
  ```

### Slice 4: Language hint support
- **Goal**: Users can specify language for better transcription accuracy
- **Status**: ✅ Implemented
- **Implementation**:
  - Added `language: Option<String>` to `TranscriptionConfig`
  - Language hint passed to both OpenAI and Mistral APIs when set
  - Documented in config template
- **Demo**: ✅ Verified
  ```toml
  [transcription]
  provider = "mistral"
  language = "pt"
  ```
  ```bash
  # Send Portuguese voice message → better accuracy with hint
  ```

## Contracts (guardrails)
- Existing OpenAI transcription must continue working unchanged when no Mistral config present
- Empty transcriptions return `None`, not empty string
- API errors are logged but don't crash the bot
- Config without transcription section uses sensible defaults (OpenAI + whisper-1)

## Key decisions (decide early)
- **Provider priority**: Explicit config > env var detection. If `transcription.provider` is set, use it. Otherwise, check which API keys are available.
- **Default provider**: OpenAI (backward compatible). Users must opt-in to Mistral.
- **Model override**: `ZDX_TELEGRAM_AUDIO_MODEL` env var works for both providers (simple, existing pattern).

## Testing
- ✅ All 208 tests pass
- ✅ Added test for `TranscriptionConfig` default values
- ✅ Manual smoke demos completed for all slices (requires MISTRAL_API_KEY for full verification)

## Polish phases (after MVP)

### Phase 1: Better error messages ✅
- ✅ Provider name included in error logs ("Mistral transcription failed: ...")
- ✅ Clear error when API key missing ("OpenAI/Mistral API key not configured")

### Phase 2: Base URL override ✅
- ✅ Support `config.providers.mistral.base_url` for proxies/custom endpoints
- ✅ Default: `https://api.mistral.ai/v1`

## Later / Deferred
- **Streaming transcription**: Voxtral supports `stream: true` for real-time results. Defer until there's a use case (e.g., live transcription display). Trigger: User requests real-time feedback during long audio.
- **Timestamp extraction**: `timestamp_granularities: ["segment"]` returns start/end times. Defer until needed for subtitle generation or similar. Trigger: Feature request for timed transcripts.
- **Voxtral chat features**: Voxtral Small supports audio+chat (Q&A on audio). Defer until multimodal agent features are prioritized. Trigger: Desire for "ask questions about this audio" feature.
- **Auto-provider selection**: Automatically pick cheapest/fastest provider. Defer until multiple providers are commonly used. Trigger: Cost optimization becomes a concern.

## API Reference

### Mistral Voxtral API

**Endpoint**: `POST https://api.mistral.ai/v1/audio/transcriptions`

**Request** (multipart/form-data):
```
file: <audio bytes>
model: "voxtral-mini-latest"
language: "en" (optional)
```

**Response**:
```json
{
  "model": "voxtral-mini-2507",
  "text": "Transcribed text...",
  "language": "en",
  "segments": [],
  "usage": {
    "prompt_audio_seconds": 203,
    "prompt_tokens": 4,
    "total_tokens": 3264,
    "completion_tokens": 635
  }
}
```

**Pricing**: $0.003/minute
