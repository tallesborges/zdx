---
name: imagine
description: "Generate and edit images. Outputs PNG files. Use when the user wants any visual creative output, an image generated from a description, or wants to modify an existing image in any way — editing, transforming, or adding visual elements."
---

# Imagine – Image Generation & Editing via `zdx imagine`

Generate images from text prompts or edit existing images using Gemini image models. Supports text-to-image generation, image editing (inpainting, outpainting, style transfer), and multi-image composition.

## CLI reference

```
zdx imagine --prompt <PROMPT> [OPTIONS]

Options:
  -p, --prompt <PROMPT>   Text prompt (required)
  -s, --source <IMAGE>    Source image for editing (repeatable for multi-image)
  -o, --out <PATH>        Output path (default: $ZDX_HOME/artifacts/image-<timestamp>.png)
      --model <MODEL>     Gemini model (default: gemini-3.1-flash-image-preview)
      --aspect <RATIO>    Aspect ratio (see table below)
      --size <SIZE>       512px | 1K (default) | 2K | 4K
```

Output: prints the saved file path(s) to stdout.

When running inside zdx (TUI, bot, or CLI), `$ZDX_ARTIFACT_DIR` is the preferred output location. Pass it via `--out`:

```bash
zdx imagine -p "..." --out "$ZDX_ARTIFACT_DIR/descriptive-name.png"
```

If no `--out` is given, images default to `$ZDX_HOME/artifacts/`.

### Modes

| Mode | Usage | Description |
|------|-------|-------------|
| **Text → Image** | `-p "prompt"` | Generate from scratch |
| **Edit single image** | `-p "prompt" -s image.png` | Inpaint, outpaint, style transfer, modify |
| **Multi-image compose** | `-p "prompt" -s a.png -s b.png` | Combine images into new scene |

## Output location

Images are saved to `$ZDX_ARTIFACT_DIR` when set, otherwise `$ZDX_HOME/artifacts/`. Use `--out` to specify the path:

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
- Images are saved to `$ZDX_ARTIFACT_DIR` when set. Use `--out` for descriptive filenames.
- If the model returns no images, check the prompt for policy violations or try rephrasing.
- Use `--size 2K` or `4K` only when you specifically need higher resolution.
- For iterative refinement, keep the base prompt and tweak one element at a time.
- For editing, use `--source` with a direct edit instruction — don't describe the whole image.
- Multi-image composition works best with 2–5 source images. More images = more ambiguity.
