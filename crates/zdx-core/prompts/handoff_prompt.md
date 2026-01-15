Extract relevant context from the conversation below for continuing this work. Write from my perspective (first person: "I did...", "I told you...").

Consider what would be useful based on my request below. Questions that might be relevant:
- What did I just do or implement?
- What instructions did I already give you which are still relevant (e.g. follow patterns in the codebase)?
- What files did I already tell you are important or that I am working on (use workspace-relative paths)?
- Did I provide a plan or spec that should be included?
- What important technical details did I discover (APIs, methods, patterns)?
- What caveats or limitations did I find?

Extract what matters for the specific request. Don't answer questions that aren't relevant. Pick an appropriate length based on the complexity of the request.

Focus on capabilities and behavior, not file-by-file changes. Avoid excessive implementation details unless critical.

This handoff will be used as the very first message in a fresh chat. It must stand alone and be actionable. Do not reference "above", "previous conversation", or "earlier". Assume the user is handing off to continue execution, so include only the minimum context needed to act on the latest request.

If relevant files exist, start the message with a line that begins "Relevant files:" followed by up to 5 workspace-relative paths separated by commas (no bullets). Add a blank line after that line before the paragraphs.

Write a short first-person handoff that feels like I wrote it. Use 1–2 short paragraphs only (no labels, no section headers). Keep it concise and actionable. Summarize what this work is about, what I already did, and any important findings or limitations.

End with a final sentence that makes the user's latest request actionable (rewrite for clarity if needed). Do not prefix it with "My request" or similar. If the request is unclear, say so and note what I would ask.

Format: plain text. No markdown headers, no bold/italic, no code fences.
Output ONLY the handoff prompt text, nothing else.

<thread>
{{THREAD_CONTENT}}
</thread>

{{GOAL}}
