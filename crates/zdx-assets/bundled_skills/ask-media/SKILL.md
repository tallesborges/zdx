---
name: ask-media
description: "Understand a local PDF, audio, or video file via `zdx ask-media` (one-shot file understanding: summarize, describe, transcribe, extract, Q&A). Use when the user gives you a specific local file path and asks about its contents — e.g. \"summarize this PDF\", \"what's in this video\", \"what does this document say\", \"describe this clip\", \"pull the numbers out of this file\". Prints the answer as text to stdout."
---

# Ask-Media – File understanding via `zdx ask-media`

Send a local **PDF, audio, or video** file plus a question to a Gemini model and get a **text answer** back on stdout. One-shot: the file is sent inline on each call.

Use this when the user hands you a concrete local file and wants you to read/understand it — summarize a PDF, describe a video, extract data from a document, answer a question about a clip, etc.

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
- Supported inputs: PDF, audio (mp3/wav/ogg/m4a/aac/flac), and video (mp4/mov/mpeg/webm/flv/wmv/3gp).
- Examples: "summarize this PDF", "what's shown in this video", "what does this recording say and what's the tone", "extract the invoice total from this file".

## When NOT to use

- **Images** — sending an image to a model for it to look at is already handled by the normal in-conversation vision path (most models support image input directly). Don't use `ask-media` for images.
- **Plain verbatim transcription** of an audio file → prefer the `transcription` skill (`zdx transcribe`), which is purpose-built for speech-to-text and supports diarization. Use `ask-media` when the user wants understanding/Q&A/summary over audio or video, or any PDF task.
- Non-Gemini models: `ask-media` is **Gemini-only** and errors clearly on any other provider.

## Model

- Default `gemini:gemini-3.5-flash-lite` — fast and cheap, built for document parsing. Good for most PDF/extraction/transcription tasks.
- For harder reasoning over the file, pass `-m gemini:gemini-3.6-flash` (or another Gemini model).

```bash
zdx ask-media clip.mp4 -p "Describe what happens, step by step."
zdx ask-media contract.pdf -p "List every party and their obligations." -m gemini:gemini-3.6-flash
zdx ask-media meeting.m4a -p "Summarize the decisions and action items."
```

## Notes & limits

- **Gemini-only.** Requires a Gemini API key (`GEMINI_API_KEY` or the `gemini` provider configured).
- **Inline size cap ~15 MiB** per file. Larger files are rejected with a clear message (Gemini File API upload is not supported yet).
- **One-shot / stateless:** each call re-sends the file. For a follow-up question, run the command again with the new prompt.
- **Text output only** — the model reads the file and answers in text; it does not produce audio/video/images.
- Read-only: never modifies the source file.
