---
name: ask-media
description: "Understand a local image, PDF, audio, or video file via `zdx ask-media` (one-shot file understanding: describe, summarize, transcribe, extract, Q&A). Use when the user gives you a specific local file path and asks about its contents, or when the active model cannot read that media type itself — e.g. \"what's in this screenshot\", \"summarize this PDF\", \"what's in this video\", \"read the error in this image\", \"pull the numbers out of this file\". Prints the answer as text to stdout."
---

# Ask-Media – File understanding via `zdx ask-media`

Send a local **image, PDF, audio, or video** file plus a question to a Gemini model and get a **text answer** back on stdout. One-shot: the file is re-sent on each call.

You write the question, so ask for exactly what you need — not a generic description. A targeted prompt ("read the stack trace and the failing file path") beats "describe this image" every time.

## CLI reference

```
zdx ask-media <FILE> -p "<question>" [OPTIONS]

Options:
  -p, --prompt <PROMPT>   The question/instruction to run against the file (required)
  -m, --model <MODEL>     Gemini provider:model id (default: gemini:gemini-3.5-flash-lite)
      --json              Emit {file, model, answer} JSON instead of plain text
```

Output: prints the model's text answer to stdout (nothing else). Read it and use it directly in your reply — do **not** wrap it in a `<media>` tag (that tag is only for sending files back).

```bash
zdx ask-media "$ZDX_ARTIFACT_DIR/report.pdf" -p "Summarize the key findings in 3 bullets."
```

## When to use

- The user gives you a specific local file path and asks about its contents.
- **The active model cannot read that media type itself.** Models without image input (for example `deepseek:deepseek-v4-flash`) never receive attached images: the image is replaced by a note telling you to come here. The file path is in the surrounding text or tool output — call `ask-media` on it with a question shaped by what the user actually asked.
- Supported inputs: images (png/jpg/webp/gif/heic), PDF, audio (mp3/wav/ogg/m4a/aac/flac), and video (mp4/mov/mpeg/webm/flv/wmv/3gp).

## When NOT to use

- The media is **already visible to the active model** — a vision-capable model receives attached images directly, so just answer. Only reach for `ask-media` when the content did not reach you.
- **Plain verbatim transcription** of an audio file → prefer the `transcription` skill (`zdx transcribe`), which is purpose-built for speech-to-text and supports diarization. Use `ask-media` when the user wants understanding/Q&A/summary over audio or video.
- Non-Gemini models: `ask-media` is **Gemini-only** and errors clearly on any other provider. If no Gemini key is configured, say the file can't be inspected instead of guessing at its contents.

## Model

- Default `gemini:gemini-3.5-flash-lite` — fast and cheap, built for document parsing. Good for most image/PDF/extraction tasks.
- For harder reasoning over the file, pass `-m gemini:gemini-3.6-flash` (or another Gemini model).

```bash
zdx ask-media screenshot.png -p "What error message is shown, and which file does it point to?"
zdx ask-media clip.mp4 -p "Describe what happens, step by step."
zdx ask-media contract.pdf -p "List every party and their obligations." -m gemini:gemini-3.6-flash
zdx ask-media meeting.m4a -p "Summarize the decisions and action items."
```

## Notes & limits

- **Gemini-only.** Requires a Gemini API key (`GEMINI_API_KEY` or the `gemini` provider configured).
- **Inline size cap ~15 MiB** per file. Larger files are rejected with a clear message (Gemini File API upload is not supported yet).
- **One-shot / stateless:** each call re-sends the file. For a follow-up question, run the command again with the new prompt — that is the cheap way to drill in after a first pass.
- **Text output only** — the model reads the file and answers in text; it does not produce audio/video/images.
- Read-only: never modifies the source file.
