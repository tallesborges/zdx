---
name: transcription
description: "Transcribe an audio file to text via `zdx transcribe` (speech-to-text). Use when the user explicitly hands you a specific audio file/path/attachment and asks for its text — e.g. \"transcribe this file\", \"what does this recording say\", or \"get the text from this audio\"."
---

# Transcription – Speech-to-Text via `zdx transcribe`

Turn an audio file into text using ZDX's native `zdx transcribe` command. The transcript is printed to stdout.

Only use this skill when the user **explicitly** points at a specific audio file to transcribe on demand. Incoming Telegram voice notes and the TUI voice-dictation hotkey are **already transcribed automatically** — don't use this skill for those.

## CLI reference

```
zdx transcribe <FILE> [OPTIONS]

Options:
      --model <MODEL>     provider:model id (e.g. elevenlabs:scribe_v2, mistral:voxtral-mini-latest)
                          or a bare provider name (openai, mistral, xai, elevenlabs) for its default
      --language <LANG>   Language hint, ISO 639-1 (e.g. en, pt)
      --diarize           Label speakers (Mistral/Voxtral or ElevenLabs only)
      --json              Emit JSON (text + diarized segments) instead of plain text
      --list-models       List the supported transcription models (with --json for machine output)
```

Output: prints the transcript text to stdout (nothing else). If no provider is configured, it prints a short "no provider" notice to stderr and exits without transcribing.

## Using the transcript

The output is **text, not audio** — read it from stdout and use it directly in your reply (quote it, summarize it, or act on it as asked). Do **not** wrap it in a `<media>` tag; that tag is only for sending audio/image files back.

```bash
zdx transcribe "$ZDX_ARTIFACT_DIR/note.ogg"
```

## Providers

- **Auto-detect order: OpenAI → Mistral → xAI → ElevenLabs.** OpenAI uses `whisper-1` (`OPENAI_API_KEY`); Mistral uses `voxtral-mini-latest` (`MISTRAL_API_KEY`); xAI uses `grok-stt` (`XAI_API_KEY`); ElevenLabs uses `scribe_v2` (`ELEVENLABS_API_KEY`). This is the reverse of `zdx speak`, which prefers Mistral.
- Run `zdx transcribe --list-models` to see the supported models and which have an API key configured.
- To pick a specific provider, pass `--model <provider>` (default model) or `--model <provider>:<model>` (exact model). There is no separate `--provider` flag.
- Voxtral and ElevenLabs Scribe v2 both support **speaker diarization** (`--diarize`); OpenAI `whisper-1` and xAI do not. ElevenLabs Scribe v2 is state-of-the-art accuracy across 90+ languages; `whisper-1` is a solid general default.

## Diarization & JSON

- `--diarize` labels who spoke, formatted as `[mm:ss] Speaker N: ...` blocks. Only **Mistral (Voxtral)** and **ElevenLabs** support it; requesting it for OpenAI/xAI errors out.
- `--json` emits `{ "text": ..., "segments": [{ "speaker", "start", "end", "text" }] }` for programmatic use (e.g. meeting tools). Combine with `--diarize` to populate `segments`.
- Voxtral returns segment-level speakers; ElevenLabs returns word-level speakers that zdx groups into contiguous speaker segments.

```bash
zdx transcribe meeting.m4a --model elevenlabs --diarize
zdx transcribe meeting.m4a --model mistral:voxtral-mini-latest --diarize --json > meeting.json
```

## When to use

- The user gives you a concrete audio file/path/attachment and wants its words: "transcribe this", "what does this audio say", "pull the text out of this recording".
- Do **not** trigger for ordinary voice-note messages or TUI dictation — those are transcribed automatically before you ever see them.

## Examples

**Transcribe a file (auto provider):**
```bash
zdx transcribe /path/to/recording.ogg
```

**Force Mistral with a language hint:**
```bash
zdx transcribe meeting.mp3 --model mistral --language en
```

## Notes & limits

- Supported providers are **OpenAI, Mistral, xAI, and ElevenLabs** — no local/offline ASR.
- Common audio formats work (ogg/opus, mp3, m4a, wav, flac, aac, webm); the provider infers the format from the file.
- Transcription is read-only: it never modifies the source file.
