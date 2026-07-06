---
name: speech
description: "Generate spoken audio from text via `zdx speak` (text-to-speech). Use when the user explicitly asks for audio/voice output — narration, voiceover, a spoken clip, a pronunciation read (e.g. \"send the pronunciation of X as audio\", \"manda em áudio\"), or an accessibility read. Outputs an OGG/Opus voice note by default. Not for transcription (that is automatic)."
---

# Speech – Text-to-Speech via `zdx speak`

Turn text into a spoken audio file using ZDX's native `zdx speak` command. In the Telegram bot, the audio is delivered as a real **voice note** (waveform + playback speed).

Only use this skill when the user **explicitly** asks for audio/voice output. For normal replies, answer with text.

## CLI reference

```
zdx speak <TEXT> [OPTIONS]

Options:
  -o, --out <PATH>        Output audio path (default: $ZDX_HOME/artifacts/speech/speech-<timestamp>.ogg)
      --provider <NAME>   mistral (default), openai, gemini, or xai
      --model <MODEL>     Provider-prefixed override, e.g. openai:gpt-4o-mini-tts, mistral:voxtral-mini-tts-latest, gemini:gemini-3.1-flash-tts-preview
      --voice <VOICE>     Provider-specific voice (see below)
      --format <FORMAT>   ogg (default, voice note) | mp3 | opus | aac | flac | wav | pcm
```

Output: prints the saved file path to stdout.

## Delivering audio (important)

To send the audio to the user in the bot, **emit the output path as a `<media>` tag** at the end of your reply:

```
<media>/absolute/path/to/speech-XXXX.ogg</media>
```

- `.ogg`/`.oga`/`.opus` → sent as a **voice note** (`sendVoice`)
- `.mp3`/`.m4a`/`.wav` → sent as an audio message (`sendAudio`)

Prefer the default `.ogg` so the user gets a proper voice note. When `$ZDX_ARTIFACT_DIR` is set, write there:

```bash
zdx speak "Hello there" --out "$ZDX_ARTIFACT_DIR/greeting.ogg"
```

## Providers & voices

- **Default: Mistral / Voxtral** (`voxtral-mini-tts-latest`). Default voice `en_paul_neutral`. Other presets include `en_paul_happy`, `en_paul_sad`, `en_paul_excited`, `en_paul_frustrated`, `gb_oliver_neutral`, `gb_jane_sarcasm` (expressive, multilingual).
- **OpenAI** (`gpt-4o-mini-tts`), via `--provider openai`. Default voice `coral` (also `marin`, `cedar`, `alloy`, `nova`, etc.).
- **Gemini** (`gemini-3.1-flash-tts-preview`), via `--provider gemini`. Default voice `Kore` (also `Puck`, `Charon`, `Aoede`, `Zephyr`, etc.). Outputs OGG/WAV. Slow preview model — prefer `gemini:gemini-2.5-flash-preview-tts` for speed/cost.
- **xAI Grok** (`grok-tts`), via `--provider xai`. Default voice `eve` (also `ara`, `leo`, `rex`, `sal`). Cheapest + fast; supports inline speech tags like `[laugh]`, `[sigh]`, `<whisper>`. English by default.
- Auto-detect prefers Mistral (`MISTRAL_API_KEY`), then OpenAI (`OPENAI_API_KEY`), then Gemini (`GEMINI_API_KEY`), then xAI (`XAI_API_KEY`). Gemini and xAI are opt-in via `--provider`.

Pick a provider/voice only when the user asks for a specific one; otherwise the defaults are fine.

## When to use

- The user explicitly asks for audio: "read this aloud", "send it as audio", "voiceover for…", "manda em áudio".
- Pronunciation requests: "how do you pronounce X" → speak the word/phrase so the user can hear it.
- Do **not** auto-generate audio for ordinary answers, and do not use this for transcription (voice notes are transcribed automatically).

## Examples

**Pronunciation read:**
```bash
zdx speak "triage — /ˈtriːɑːʒ/" --out "$ZDX_ARTIFACT_DIR/triage.ogg"
```

**Short narration with OpenAI:**
```bash
zdx speak "Welcome to the demo. Today we'll show how it works." --provider openai --voice coral --out "$ZDX_ARTIFACT_DIR/intro.ogg"
```

**Plain MP3 (not a voice note):**
```bash
zdx speak "Chapter one." --out "$ZDX_ARTIFACT_DIR/chapter-1.mp3"
```

## Notes & limits

- Input must be ≤ 4096 characters per request; split longer text into multiple calls.
- The default OGG/Opus output requires `ffmpeg` on PATH; without it, `zdx speak` falls back to MP3 automatically.
- Mistral's TTS endpoint applies content moderation and may reject some inputs with a `403`; the error surfaces the reason.
- Disclose to end users that the voice is AI-generated when relevant.
