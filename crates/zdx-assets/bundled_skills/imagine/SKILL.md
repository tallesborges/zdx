---
name: imagine
description: "Generate and edit images. Saves the image and prints the exact path it wrote. Use when the user wants any visual creative output, an image generated from a description, or wants to modify an existing image in any way — editing, transforming, or adding visual elements."
---

# Imagine – Image Generation & Editing via `zdx imagine`

Generate images from text prompts or edit existing images using Gemini, OpenAI, or Alibaba (Qwen-Image) image models. Supports text-to-image generation, image editing (inpainting, outpainting, style transfer), and multi-image composition.

## CLI reference

```
zdx imagine --prompt <PROMPT> [OPTIONS]

Options:
  -p, --prompt <PROMPT>   Text prompt (required)
  -s, --source <IMAGE>    Source image for editing (repeatable for multi-image)
  -o, --out <PATH>        Output path — written exactly as given (default: $ZDX_HOME/artifacts/image-<timestamp>.<ext>)
      --model <MODEL>     Model override (default: gemini:gemini-3.1-flash-image-preview)
      --aspect <RATIO>    Aspect ratio (Gemini only; see table below)
      --size <SIZE>       512px | 1K | 2K | 4K (Gemini default 1K; openai: lets the API pick; Codex rejects it)
      --format <FORMAT>   png | jpeg | webp | gif | svg (openai: only)
      --quality <Q>       low | medium | high | xhigh | max | auto (openai: only)
      --complexity <L>    low | medium | high (openai: only; API default high)
      --frames <N>        GIF frame count (openai: only, with --format gif)
      --frame-ms <MS>     GIF delay per frame, 20-2000 (openai: only, with --format gif)
      --markup <PNG>      Transparent PNG annotation layer for the first --source (openai: only)
```

Output: prints the exact saved file path(s) to stdout. `--out` is honored literally; the model picks the image format (often JPEG for Gemini), so a file named `.png` may actually hold JPEG bytes. That is fine — zdx reads images by content, not extension. Use `--out` to control the path and always view/attach the printed path. For `openai:` output, zdx detects the real type from the bytes. Without `--out` it picks the matching extension. A warning on stderr means the API returned a different type than `--format` asked for (for example PNG instead of GIF); tell the user, and do not rename the file to the requested type.

When running inside zdx (TUI, bot, or CLI), `$ZDX_ARTIFACT_DIR` is the preferred output location. Pass it via `--out`:

```bash
zdx imagine -p "..." --out "$ZDX_ARTIFACT_DIR/descriptive-name.png"
```

If no `--out` is given, images default to `$ZDX_HOME/artifacts/`.

## Provider guidance

- **Default:** If you do not pass `--model`, `zdx imagine` uses `gemini:gemini-3.1-flash-image-preview`.
- **OpenAI API key path (`openai:`):** `openai:gpt-image-2.6-flare` (fast iteration, the default choice) and `openai:gpt-image-2.6-sunburst` (detailed work, more consistent edits). Calls the OpenAI Images API, billed per token. Requires `OPENAI_API_KEY`. This is the only path with model selection, `--format`, `--quality`, `--complexity`, GIF frames, and `--markup`. Older OpenAI image models (`gpt-image-2`, `gpt-image-2.5-*`) are no longer documented for `openai:`.
- **OpenAI Codex path (`openai-codex:gpt-image-2`):** runs on the ChatGPT subscription, no per-request cost. `gpt-image-2` is the only accepted model name, and it is a backend alias: the backend picks the image engine, size, and quality, and never reports which model ran. Do not tell the user which model Codex used. `--size`, `--aspect`, `@thinking`/`@fast`, other model IDs (including 2.6), and all `openai:`-only flags fail on this path. `zdx imagine` prints a warning on stderr. Generation was live-verified; edits with `-s` follow upstream Codex CLI and are not live-tested.
- **Alibaba (Qwen-Image) support:** `zdx imagine` supports `alibaba:qwen-image-3.0-pro`, `alibaba:qwen-image-3.0`, and `alibaba:qwen-image-2.0-pro` — combined text-to-image + editing models on Alibaba DashScope. Requires `ALIBABA_API_KEY`. Use them when the user asks for Qwen/Alibaba image generation.
- **Preferred Alibaba path:** Use `alibaba:qwen-image-3.0-pro`. It is the strongest of the three at dense layouts, small text (~10px), and multilingual typography. Fall back to `alibaba:qwen-image-3.0` for cheaper/faster runs, and to `alibaba:qwen-image-2.0-pro` only when explicitly asked.
- **Gemini vs OpenAI/Alibaba:** `--aspect` currently works with Gemini only; `openai:` and Alibaba take `--size` instead, and OpenAI Codex takes neither.
- **OpenAI size support:** `openai:` sends `1K`=1024x1024, `2K`=2048x2048, `4K`=3840x2160, and omits size without `--size` so the API picks it. GPT Image 2.6 size limits are not documented, so 2K and 4K can fail. `openai-codex:` rejects `--size`; the backend picks the size (a probe got 1536x1024). `512px` is Gemini-only.
- **Alibaba size support:** All Alibaba Qwen-Image models support `512px`, `1K`, and `2K` (not `4K`).

### Recommended model selection

- Use **Gemini** by default unless the user asks for OpenAI or there is a specific reason to use it.
- Use **`openai-codex:gpt-image-2`** for plain OpenAI requests on the subscription, when the exact model and settings do not matter.
- Use **`openai:gpt-image-2.6-flare`** (or `-sunburst` for detailed edits) when the user asks for GPT Image 2.6, an explicit model or size, a GIF, an SVG, a markup-guided edit, or `--complexity`/`--quality` control. It costs money per request. zdx sends what you ask for but does not check which model ran or the output size, and this path is tested only offline.

### OpenAI examples

**Subscription (Codex):**
```bash
zdx imagine -p "A cinematic photo of a red fox in falling snow" --model openai-codex:gpt-image-2
```

**GPT Image 2.6 (API key; billed, explicit model):**
```bash
zdx imagine -p "A cinematic photo of a red fox in falling snow" --model openai:gpt-image-2.6-flare --quality low --size 1K
```

**Quick edit:** low complexity suits focused edits; use medium or high for scenes with many elements or dense text.
```bash
zdx imagine -p "Add a neon sign above the doorway, keep the rest unchanged" -s street.png --model openai:gpt-image-2.6-flare --complexity low
```

**Markup-guided edit:** draw circles or sketches on a separate transparent PNG with the same size as the first source image. Do not flatten it onto the photo.
```bash
zdx imagine -p "Make the circled vase blue, keep everything else the same" -s kitchen.png --markup circle-overlay.png --model openai:gpt-image-2.6-flare
```

**Animated GIF:** describe the subject, the motion, and the loop. `--size` is the size of each frame.
```bash
zdx imagine -p "A small owl blinks and waves one wing, then returns to the first pose for a seamless loop" --model openai:gpt-image-2.6-sunburst --format gif --frames 6 --frame-ms 150 --out "$ZDX_ARTIFACT_DIR/owl.gif"
```

**SVG (generation only, no --source):** for icons, logos, and flat illustrations.
```bash
zdx imagine -p "A flat bakery logo: a golden loaf above the words Field & Flour" --model openai:gpt-image-2.6-flare --format svg --out "$ZDX_ARTIFACT_DIR/logo.svg"
```

**Unverified OpenAI limits:** GIF and SVG output may need preview access on the API organization. The docs disagree on the GIF frame range (2-16 or 2-64) and on GIF output for edits. If the API rejects a request, report its error; do not retry with guessed values. The SVG is static vector XML: `Read` cannot show it as an image.

### Alibaba (Qwen-Image) examples

**Text-to-image:**
```bash
zdx imagine -p "A cinematic photo of a red fox in falling snow" --model alibaba:qwen-image-3.0-pro --size 1K
```

**Editing (same model, add source image):**
```bash
zdx imagine -p "Make the jacket bright red, keep everything else the same" -s person.png --model alibaba:qwen-image-3.0-pro --size 1K
```

### Modes

| Mode | Usage | Description |
|------|-------|-------------|
| **Text → Image** | `-p "prompt"` | Generate from scratch |
| **Edit single image** | `-p "prompt" -s image.png` | Inpaint, outpaint, style transfer, modify |
| **Multi-image compose** | `-p "prompt" -s a.png -s b.png` | Combine images into new scene |

## Output location

Images are saved to `$ZDX_ARTIFACT_DIR` when set, otherwise `$ZDX_HOME/artifacts/`. Use `--out` to set the exact path:

```bash
zdx imagine -p "..." --out "$ZDX_ARTIFACT_DIR/descriptive-name.png"
```

## Core prompting principle

> **Describe the scene, don't just list keywords.** A narrative, descriptive paragraph will almost always produce a better, more coherent image than a list of disconnected words.

## Infographic & visual explainer prompts

### Template: Concept explainer

```
Create a [style] infographic that explains [concept/topic]. Show [key elements]
with clear labels and [visual metaphor or analogy]. The layout should be
[layout description]. Use [color palette] and [typography style].
[Aspect ratio].
```

### Template: Process / how-it-works

```
A [style] diagram showing how [process/system] works, step by step.
Label each stage clearly: [stage 1], [stage 2], [stage 3].
Use [visual style: arrows, flow, numbered panels]. [Color scheme].
[Aspect ratio].
```

### Template: Comparison / versus

```
A [style] side-by-side comparison of [thing A] vs [thing B].
Each side shows [key differences] with clear labels.
Use [visual contrast: split layout, color coding]. [Aspect ratio].
```

### Template: Anatomy / breakdown

```
A [style] annotated breakdown of [subject], with labeled parts showing
[components/layers]. [Drawing style: technical, Da Vinci sketch, blueprint,
cutaway, cross-section]. Notes in English. [Aspect ratio].
```

### Template: Timeline / history

```
A [style] visual timeline of [topic], from [start] to [end].
Key milestones labeled with dates and brief descriptions.
[Layout: horizontal, vertical, spiral]. [Color palette]. [Aspect ratio].
```

### Template: Mental model / analogy

```
A [style] visual that explains [abstract concept] using the analogy of
[concrete thing]. Show how [mapping between concept and analogy].
Clear labels connecting the analogy to the real concept. [Aspect ratio].
```

### Tips for better explainer images

- **State the educational goal** — "explain X to someone unfamiliar" produces more accessible results than just naming the topic.
- **Describe the layout** — triptych, grid, flowchart, numbered panels, split-screen. The model follows layout instructions well.
- **Use analogies and metaphors** — "explain photosynthesis as a recipe" or "explain TCP as a postal system" produces creative, memorable visuals.
- **Specify label and text placement** — "with bold labels", "numbered steps", "annotated arrows pointing to each part".
- **Choose a visual style** that fits the content: blueprint for technical, watercolor for organic, flat vector for clean/modern, sketch for conceptual.
- **Use 16:9 or 3:2** for most explainers (good for horizontal layouts with room for labels).

### Example prompts

**How something works:**
```bash
zdx imagine -p "A colorful, educational infographic explaining how a CPU executes an instruction. Show the fetch-decode-execute cycle as a circular flow diagram with labeled stages. Each stage has a small illustration: memory fetching data, decoder breaking it down, ALU computing. Flat vector style, vibrant colors on dark background." --aspect 16:9
```

**Concept analogy:**
```bash
zdx imagine -p "A whimsical illustrated infographic explaining the human immune system as a medieval castle defense. White blood cells as knights, antibodies as archers on walls, the skin as castle walls, fever as pouring boiling oil. Labeled annotations connecting each metaphor to the real biology. Colorful storybook illustration style." --aspect 16:9
```

**Anatomy / breakdown:**
```bash
zdx imagine -p "Da Vinci style anatomical sketch of a dissected Monarch butterfly. Detailed drawings of the head, wings, and legs on textured parchment with handwritten notes in English explaining each part." --aspect 1:1
```

**Comparison:**
```bash
zdx imagine -p "A clean, modern split-screen comparison of REST vs GraphQL APIs. Left side shows REST with multiple endpoint arrows, right side shows GraphQL with a single endpoint and flexible query. Color-coded: blue for REST, purple for GraphQL. Flat design, bold labels, white background." --aspect 16:9
```

**Process / step-by-step:**
```bash
zdx imagine -p "A vibrant infographic explaining photosynthesis as a recipe from a colorful kids' cookbook. Show the 'ingredients' (sunlight, water, CO2) going into a plant 'kitchen' and the 'finished dish' (sugar/energy) coming out. Numbered steps, playful illustrations, bright colors. Suitable for a 4th grader." --aspect 16:9
```

**Timeline:**
```bash
zdx imagine -p "An illustrated horizontal timeline of the history of programming languages, from Fortran (1957) to Rust (2015). Each milestone shows the language name, year, and a small icon representing its key innovation. Retro-futuristic style with muted earth tones and clean typography." --aspect 21:9
```

**Mental model:**
```bash
zdx imagine -p "A visual explanation of Git branching using a subway map analogy. The main branch is the main line, feature branches are branch lines that split off and merge back. Commits are stations. Labeled with Git terms (main, feature, merge, rebase). Clean, colorful transit map style." --aspect 16:9
```

## Aspect ratio and resolution

| Ratio | Use case |
|---|---|
| `1:1` | Icons, logos, social media posts |
| `3:4` / `4:3` | Portraits / Classic photography |
| `2:3` / `3:2` | Print photography |
| `9:16` / `16:9` | Stories, vertical / Banners, widescreen, **infographics** |
| `21:9` | Cinematic ultrawide, **timelines** |
| `4:5` / `5:4` | Instagram portrait / landscape |
| `1:4` / `4:1` | Ultra-tall / Ultra-wide strips |
| `1:8` / `8:1` | Extreme panoramic strips |

**Resolution:** Default is 1K. Use `--size` only when you need higher resolution.

## Image editing (--source)

Use `--source` / `-s` to provide one or more input images for editing. The prompt describes what to change.

### Single-image editing

**Inpainting (add/modify elements):**
```bash
zdx imagine -p "Add a small knitted wizard hat on the cat's head" -s cat.png
```

**Inpainting (remove elements):**
```bash
zdx imagine -p "Remove the person from the background, fill with natural scenery" -s photo.png
```

**Style transfer:**
```bash
zdx imagine -p "Transform this photograph into Van Gogh's Starry Night style. Preserve the composition but render with swirling impasto brushstrokes and deep blues and bright yellows." -s city.jpg
```

**Outpainting / aspect change:**
```bash
zdx imagine -p "Recreate this image as a cinematic ultrawide banner, extending the background naturally" -s hero.png --aspect 21:9
```

**Color/mood adjustment:**
```bash
zdx imagine -p "Make this scene a warm golden hour sunset, keeping all subjects the same" -s photo.jpg
```

### Multi-image composition

Provide multiple `--source` flags to combine images into a new scene:

**Group composition:**
```bash
zdx imagine -p "An office group photo of these people, making funny faces" \
  -s person1.png -s person2.png -s person3.png --aspect 5:4
```

**Product mockup:**
```bash
zdx imagine -p "Create a professional e-commerce fashion photo of this model wearing this dress" \
  -s model.jpg -s dress.jpg
```

**Composite with reference:**
```bash
zdx imagine -p "Place this logo in the bottom-right corner of this banner image" \
  -s banner.png -s logo.png
```

### Editing prompt tips

- **Be specific about what to change** — "change only the blue sofa to brown leather" works better than "change the sofa".
- **Describe what to preserve** — "keep the rest of the room exactly the same, preserving style and lighting".
- **Don't reconstruct the original** — use direct editing instructions, not a description of the whole image.
- **For detail preservation** — describe critical details (face, logo) in the prompt alongside the edit instruction.
- **Supported formats:** PNG, JPEG, GIF, WebP. Total request must be under 20MB (all source images combined).

## Other prompt styles

For non-explainer use cases, see `references/prompt-templates.md` for templates and examples covering:
- Photorealistic scenes
- Stickers & illustrations
- Text & typography in images
- Product mockups
- Minimalist / negative space
- Cinematic landscapes
- Logos

## General prompting tips

- **Be specific about the subject** — name the subject, action, environment, context.
- **Declare the style explicitly** — without a style keyword the model guesses.
- **Use narrative descriptions** — paint a picture with words, not keyword lists.
- **Iterate quickly** — small wording changes produce big differences. Tweak rather than rewrite.

## Workflow tips

- Always view the generated image after creation to verify quality.
- Images are saved to `$ZDX_ARTIFACT_DIR` when set. Use `--out` for descriptive filenames; the path is written exactly as given.
- If the model returns no images, check the prompt for policy violations or try rephrasing.
- Use `--size 2K` or `4K` only when you specifically need higher resolution.
- For iterative refinement, keep the base prompt and tweak one element at a time.
- For editing, use `--source` with a direct edit instruction — don't describe the whole image.
- Multi-image composition works best with 2–5 source images. More images = more ambiguity.
