# Prompt templates — non-explainer styles

Templates and examples for image types beyond infographics/explainers.

## Photorealistic scene

### Template
```
A photorealistic [shot type] of [subject], [action], set in [environment].
Illuminated by [lighting], creating a [mood] atmosphere. Captured with
[camera/lens details], emphasizing [textures]. [Aspect ratio] format.
```

### Example
```bash
zdx imagine -p "A photorealistic close-up portrait of an elderly Japanese ceramicist with deep, sun-etched wrinkles and a warm, knowing smile. He is carefully inspecting a freshly glazed tea bowl. The setting is his rustic, sun-drenched workshop. Soft, golden hour light streaming through a window, highlighting the fine texture of the clay. Captured with an 85mm portrait lens, soft blurred background (bokeh). Serene and masterful mood." --aspect 3:4
```

## Sticker / illustration

### Template
```
A [style] sticker of a [subject], featuring [characteristics] and a [color palette].
[Line style] and [shading]. Background must be [white/transparent].
```

### Example
```bash
zdx imagine -p "A kawaii-style sticker of a happy red panda wearing a tiny bamboo hat, munching on a green bamboo leaf. Bold, clean outlines, simple cel-shading, vibrant color palette. White background." --aspect 1:1
```

## Text in images

### Template
```
Create a [image type] for [brand] with the text "[text]" in a [font style].
The design should be [style], with a [color scheme].
```

### Example
```bash
zdx imagine -p "A vibrant, eye-catching 'TYPOGRAPHY' design on a textured off-white background. Bold, blocky, extra condensed letters with 3D effect from overlapping layers of bright blue and hot pink, each with a halftone dot pattern, evoking a retro print aesthetic." --aspect 16:9
```

## Product mockup

### Template
```
A high-resolution, studio-lit product photograph of a [product] on a [surface].
[Lighting setup] to [purpose]. Camera angle: [angle] to showcase [feature].
Ultra-realistic, sharp focus on [detail]. [Aspect ratio].
```

### Example
```bash
zdx imagine -p "A high-resolution, studio-lit product photograph of a minimalist ceramic coffee mug in matte black on a polished concrete surface. Three-point softbox setup, soft diffused highlights, no harsh shadows. Slightly elevated 45-degree angle, sharp focus on steam rising from the coffee." --aspect 1:1 --size 2K
```

## Minimalist / negative space

### Template
```
A minimalist composition featuring a single [subject] positioned in the [position]
of the frame. Background: vast, empty [color] canvas. Soft, subtle lighting.
```

### Example
```bash
zdx imagine -p "A minimalist composition featuring a single, delicate red maple leaf positioned in the bottom-right of the frame. The background is a vast, empty off-white canvas, creating significant negative space for text. Soft, diffused lighting from the top left. Square image." --aspect 1:1
```

## Logo

### Example
```bash
zdx imagine -p "Create a modern, minimalist logo for a coffee shop called 'The Daily Grind'. Clean, bold, sans-serif font. Black and white color scheme. Logo in a circle. Use a coffee bean in a clever way." --aspect 1:1
```

## Cinematic landscape

### Example
```bash
zdx imagine -p "A vast alien desert at golden hour, towering crystal formations catching the light, a lone astronaut in the foreground casting a long shadow. Cinematic wide angle, dramatic volumetric lighting, sci-fi concept art." --aspect 16:9 --size 4K
```

## Style keywords reference

| Category | Keywords |
|---|---|
| Photo | photorealistic, cinematic, film noir, vintage, 35mm, studio lighting |
| Art | watercolor, oil painting, pencil sketch, ink illustration, impasto |
| Digital | 3D render, CGI, digital art, low-poly, pixel art, isometric |
| Design | minimalist, flat design, flat vector, blueprint, pop-art |
| Aesthetic | anime, cyberpunk, synthwave, art nouveau, art deco, kawaii |

## Composition terms

close-up, medium shot, wide angle, bird's-eye view, overhead flat lay, 45-degree, centered composition, rule of thirds, symmetrical, leading lines, depth of field, bokeh, tilt-shift.

## Lighting terms

golden hour, Rembrandt lighting, three-point softbox, neon glow, backlit, dramatic chiaroscuro, overcast diffused, soft diffused, studio quality.

## Quality modifiers

`4K`, `8K`, `ultra HD`, `high detail`, `sharp focus`, `professional lighting`, `high-resolution`.

## Prompt inspiration (from Google official sources)

Curated prompts from Google blog posts and API docs.

**Dramatic misty landscape:**
> This aerial shot captures a dramatic, misty landscape, likely a valley or glen, characterized by rolling, verdant hills and a winding river or loch. The photography style leans towards a moody and atmospheric aesthetic, emphasizing the grandeur and isolation of nature. The camera angle is high, looking down into the valley, providing a sweeping panoramic view that highlights the immense scale of the surroundings.

**Magazine cover:**
> A photo of a glossy magazine cover, the minimal blue cover has the large bold words Nano Banana. The text is in a serif font and fills the view. No other text. In front of the text there is a portrait of a person in a sleek and minimal dress. She is playfully holding the number 2, which is the focal point. Put the issue number and "Feb 2026" date in the corner along with a barcode. The magazine is on a shelf against an orange plastered wall, within a designer store.

**Isometric city miniature:**
> Present a clear, 45° top-down isometric miniature 3D cartoon scene of London, featuring its most iconic landmarks and architectural elements. Use soft, refined textures with realistic PBR materials and gentle, lifelike lighting and shadows. Use a clean, minimalistic composition with a soft, solid-colored background.

**Architectural lettering:**
> View of a cozy street in Berlin on a bright sunny day, stark shadows. The old houses are oddly shaped like letters that spell out "BERLIN" Colored in Blue, Red, White and black. The houses still look like houses and the resemblance to letters is subtle.

**Pencil sketch (graphite feel):**
> Create a pencil sketch of a pufferfish nest. Not a clean digital drawing but something with visible pencil strokes and that dusty graphite look.

**Chiaroscuro lighting control:**
> Generate an image with an intense chiaroscuro effect. Introduce harsh, directional light, appearing to come from above and slightly to the left, casting deep, defined shadows across the face. Only slivers of light illuminating the eyes and cheekbones, the rest of the face is in deep shadow.

**Pop-art fashion portrait:**
> Cinematic still, evoking a vibrant, dreamlike quality often found in highly stylized musical dramas or whimsical comedies, with a composition style reminiscent of a master of bold, graphic imagery. The camera is positioned slightly low, looking up at the subject, emphasizing their commanding presence and the dramatic flair of their outfit. The color palette is exceptionally bold and high-contrast, dominated by electric blue and shocking pink, with a bright yellow accent.

**Localized wildlife sign:**
> An intimate, naturalistic cinematic close-up reveals a small, intricately illustrated sign made of recycled material, showing drawings of local birds and flowers. Delicate script below reads: "Native Wildlife: Please Observe from a Distance." Soft, diffused light filters through the leaves of a nearby fern, casting gentle shadows.

**Expressive typography logos:**
> Make 8 minimalistic logos, each is an expressive word, and make letters convey a message or sound visually to express the meaning of this word in a dramatic way. Composition: flat vector rendering of all logos in black on a single white background.
