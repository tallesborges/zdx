# Goals
- Accept Telegram DMs that include image or audio attachments.
- Make image content available to the agent during the turn.
- Convert audio to text when possible so the agent can understand it.

# Non-goals
- Any media handling outside Telegram DMs.
- Features unrelated to reading images/audio.

# Design principles
- User journey drives order
- Graceful degradation when media cannot be processed

# User journey
1. User sends a Telegram DM with an image or audio attachment (with or without text).
2. Bot fetches the attachment and stores it locally.
3. Bot injects image content or audio transcript into the agent input.
4. Bot replies in the same DM.

# Foundations / Already shipped (✅)
List capabilities that already exist and should not be rebuilt.

## Telegram DM loop + allowlist
- What exists: polling, DM-only filtering, allowlist gating, reply handling.
- ✅ Demo: send a text DM and get a reply.
- Gaps: none (attachments supported).

## Agent turn pipeline
- What exists: thread persistence, message history, tool execution loop.
- ✅ Demo: send a text DM and see the model respond.
- Gaps: none (image blocks + audio transcripts injected when available).

# MVP slices (ship-shaped, demoable)
Define Slice 1..N in user-journey order.

## Slice 1: Attachment ingestion
- **Goal**: Detect Telegram image/audio attachments and download them with size limits.
- **Scope checklist**:
  - [x] Extend Telegram message parsing to surface attachment metadata
  - [x] Fetch files via Telegram API and store locally
  - [x] Enforce file size limits and skip oversized media
- **✅ Demo**: send an image/audio and verify it is stored locally.
- **Risks / failure modes**:
  - Missing MIME metadata or unsupported formats
  - Download failures or oversized files

## Slice 2: Image to model input
- **Goal**: Attach image content to the user message so vision-capable models can read it.
- **Scope checklist**:
  - [x] Base64 encode images and attach to message content
  - [x] Include a text note referencing the saved file
- **✅ Demo**: send a photo; model replies describing it.
- **Risks / failure modes**:
  - Non-vision models ignore the image content

## Slice 3: Audio transcription
- **Goal**: Produce text from audio attachments when transcription is available.
- **Scope checklist**:
  - [x] Transcribe audio via a configured provider
  - [x] Fallback to a user-visible note when transcription is unavailable
- **✅ Demo**: send a voice note and see transcript-driven response.
- **Risks / failure modes**:
  - Provider errors/timeouts
  - Unsupported audio types

# Contracts (guardrails)
List non-negotiable behaviors that must not regress (derived from Inputs and existing behavior).
- Only accept allowlisted users in private chats.
- Skip oversized media rather than crashing the bot.
- If transcription fails or is unavailable, still respond with a clear fallback note.

# Key decisions (decide early)
List only decisions that would cause rework if postponed (derived from Inputs).
- Where media files are stored locally.
- Which transcription provider/model is used when available.

# Testing
- Manual smoke demos per slice
- Minimal regression tests only for contracts

# Polish phases (after MVP)
Group improvements into phases, each with a ✅ check-in demo.
Limited strictly to scope present in Inputs.

## Phase 1: Configuration knobs
- Add configuration for size limits and transcription settings.
- ✅ Check-in demo: adjust limits/model and verify behavior.

# Later / Deferred
Explicit list of "not now" items + what would trigger revisiting them.
- Outbound media sending if users request it.