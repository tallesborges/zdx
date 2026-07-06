---
name: record-demos
description: Record zdx TUI/CLI demos as GIFs, screenshots, or videos with VHS for the README. Use when the user asks to record or regenerate a zdx demo, add a feature clip, or says "record the TUI", "regenerate demo.gif", "make a GIF of the tabs feature", "screenshot the context overlay", or "add a demo for a feature". Covers the locked recording settings, the NO_COLOR gotcha, tape layout under docs/tapes, and rendering into docs/assets.
---

# Record zdx demos (VHS)

Record zdx as GIFs / PNGs / MP4 using [VHS](https://github.com/charmbracelet/vhs). Tapes are declarative and deterministic (headless `ttyd` + `ffmpeg`, no visible window), so the same `.tape` regenerates the same clip every time.

Two things, both committed to this repo:
- **Tapes + assets = project content.** Feature recordings live in `docs/tapes/*.tape` and render into `docs/assets/*.gif|png`, embedded in `README.md`.
- **This skill = the how.** It holds the recipe and gotchas. It contains no GIFs.

## Prerequisites (check once)

```sh
which vhs ffmpeg || brew install vhs   # vhs pulls in ttyd; ffmpeg needed for gif/mp4
```

Build a zdx binary to record against. The repo uses a **shared target dir** (`.cargo/config.toml` → `target-dir = "../.zdx/cargo-target"`), so the binary is **not** under `target/`:

```sh
cargo build -p zdx --bin zdx                 # debug; or: just build-release
BIN_DIR="$(cd ../.zdx/cargo-target/debug && pwd)"   # release/ for build-release
"$BIN_DIR/zdx" --version                     # sanity check
```

## Critical gotchas (encode these in every tape)

1. **`unset NO_COLOR`** in the hidden setup, before launching zdx. The agent runtime exports `NO_COLOR=1`, which makes the whole TUI monochrome. This is the #1 mistake — always unset it.
2. **Quote all paths** in `Output` / `Screenshot`. `$ZDX_ARTIFACT_DIR` contains `--`, which breaks VHS's unquoted-path parser.
3. **Put `$BIN_DIR` on PATH** in the hidden setup so `zdx` resolves as a bare command.
4. **ttyd startup race**: an occasional `could not open ttyd: ERR_CONNECTION_REFUSED` — just re-run the same tape.
5. **Quit the TUI** with `Ctrl+C` twice (or the `quit` command).
6. **Real turns need a live model call** (~40s with Opus [high]). Don't pad the end with a long fixed `Sleep` — that leaves a "frozen" tail on the final frame. Instead end the turn with `Wait+Screen /marker/` (see gotcha 8). Tools run inline (no approval gate in this setup).
7. **Colors show during real turns**, not on the launch screen. The empty launch screen is mostly white/gray; diffs (green/red), tool output, syntax highlighting, and the context-% indicator only light up once the agent responds.
8. **End real turns with `Wait+Screen /marker/`, and the marker must be output-only.** `Wait+Screen` matches *anything currently on screen — including your typed prompt*, which stays visible in the conversation. So a marker copied from the prompt fires instantly (empty recording). Pick text that appears only after completion — e.g. a distinctive word the model will produce in its answer but that isn't in your prompt, or the turn-summary bar (`Wait+Screen@60s /tool ·/`). Add a small `Sleep 1s` after the Wait so the final frame reads.

## Locked recording settings

These produce a readable, framed, full-color clip. Treat them as the default; tune per clip only when needed.

| Setting | Value | Why |
|---|---|---|
| `Width` / `Height` | `1600` × `1000` | Enough columns/rows for the TUI to breathe |
| `FontSize` | `20` | Readable after Telegram/GitHub downscaling |
| `Theme` | `Dracula` | Vivid ANSI palette (swap to taste; zdx uses named 16-colors) |
| `WindowBar` | `Colorful` | macOS-style title bar with traffic lights |
| `Margin` / `MarginFill` | `60` / `#5B4B8A` | Framed backdrop |
| `BorderRadius` | `12` | Rounded window corners |
| `TypingSpeed` | `45ms` | Natural typing pace |

Bigger widget → raise `Width`/`Height`. Crisper text → raise `FontSize`. More/less color → change `Theme`.

## Tape template

Copy this into `docs/tapes/<name>.tape` and edit the body. `$OUT` and `$BIN_DIR` must be expanded to absolute paths before running (VHS does not expand shell vars).

```tape
Output "docs/assets/<name>.gif"

Set Shell bash
Set FontSize 20
Set Width 1600
Set Height 1000
Set Padding 24
Set Theme "Dracula"
Set WindowBar Colorful
Set Margin 60
Set MarginFill "#5B4B8A"
Set BorderRadius 12
Set TypingSpeed 45ms

Hide
Type "unset NO_COLOR; export PATH=<BIN_DIR>:$PATH; clear"
Enter
Show

# --- body: drive zdx here ---
Type "zdx"
Sleep 500ms
Enter
Sleep 3s
# ... Type / Enter / Sleep to demo the feature ...
Ctrl+C
Sleep 400ms
Ctrl+C
Sleep 600ms
```

Add `Screenshot "docs/assets/<name>.png"` at any frame to also emit a still.

**Real-turn variant** — replace the body with a prompt + `Wait+Screen` so there's no frozen tail. Use the completion status bar as the marker (it appears only when the turn finishes and never in your prompt):

```tape
Type "Read the root Cargo.toml and list the crate names as a bullet list."
Sleep 800ms
Enter
Wait+Screen@90s /tool ·/    # turn-summary bar ("N tool · Xs · ..."); output-only
Sleep 1s
```

If the turn uses no tools, pick another output-only marker (a distinctive word the model will emit that is absent from your prompt line). Never reuse text from the prompt — it stays visible in the conversation and fires `Wait` instantly.

## Workflow

### 1. Preview in the artifact dir
Render scratch cuts to `$ZDX_ARTIFACT_DIR` first (point `Output` there), iterate on timing/content, and view the result before touching `docs/assets/`.

### 2. Finalize into the repo
Once it looks right, point `Output` (and any `Screenshot`) at `docs/assets/<name>.gif|png`, render, and embed in `README.md`:

```sh
vhs docs/tapes/<name>.tape
```

**Success criteria:** the clip is full-color, readable, framed, and shows the intended feature; the file lands in `docs/assets/`.

### 3. Add a new feature clip
Copy the template to `docs/tapes/<feature>.tape`, script the keystrokes for that feature (find the real keybindings in `crates/zdx-tui/src` first — command palette is `Ctrl+O`), preview, finalize, embed. One tape per feature.

## Notes

- `Output` extension picks the format: `.gif`, `.mp4`, `.webm`, or `.png` (single frame).
- **Render both `.gif` and `.mp4`** for each clip (same tape, two `Output` lines or two runs). Use **GIF in the README** (GitHub autoplays it inline), and **MP4 for sharing/previews** (Telegram, chat, social).
- **Quality:** GIF is capped at 256 colors + dithered, and Telegram re-encodes GIFs hard, so shared GIFs look poor. MP4 is true-color H.264 and Telegram plays it natively at full quality — always prefer MP4 when sending a clip to someone.
- The overview `demo.gif` (top of README) is just another tape: launch + one real turn.
- Keep tapes short; long clips make large GIFs. Trim sleeps to the minimum that reads well.
