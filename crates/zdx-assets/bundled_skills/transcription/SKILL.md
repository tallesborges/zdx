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
      --provider <NAME>   openai or mistral (default: auto-detect, OpenAI first)
      --model <MODEL>     Provider-prefixed override, e.g. openai:whisper-1, mistral:voxtral-mini-latest
      --language <LANG>   Language hint, ISO 639-1 (e.g. en, pt)
```

Output: prints the transcript text to stdout (nothing else). If no provider is configured, it prints a short "no provider" notice to stderr and exits without transcribing.

## Using the transcript

The output is **text, not audio** — read it from stdout and use it directly in your reply (quote it, summarize it, or act on it as asked). Do **not** wrap it in a `<media>` tag; that tag is only for sending audio/image files back.

```bash
zdx transcribe "$ZDX_ARTIFACT_DIR/note.ogg"
```

## Providers

- **Auto-detect order: OpenAI first, then Mistral.** OpenAI uses `whisper-1` (`OPENAI_API_KEY`); Mistral uses `voxtral-mini-latest` (`MISTRAL_API_KEY`). This is the reverse of `zdx speak`, which prefers Mistral.
- Pass `--provider openai|mistral` only when the user asks for a specific one or the default isn't configured.
- Voxtral is stronger on diarization/timestamps and multilingual audio; `whisper-1` is a solid general default.

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
zdx transcribe meeting.mp3 --provider mistral --language en
```

## Notes & limits

- Supported providers are **OpenAI + Mistral only** — no local/offline ASR.
- Common audio formats work (ogg/opus, mp3, m4a, wav, flac, aac, webm); the provider infers the format from the file.
- Transcription is read-only: it never modifies the source file.
