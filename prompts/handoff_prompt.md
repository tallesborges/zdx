Extract relevant context from the conversation below for continuing this work. Write from my perspective (first person: "I did...", "I told you...").

Consider what would be useful based on my request below. Questions that might be relevant:
- What did I just do or implement?
- What instructions did I already give you which are still relevant (e.g. follow patterns in the codebase)?
- What files did I already tell you are important or that I am working on (use workspace-relative paths)?
- Did I provide a plan or spec that should be included?
- What important technical details did I discover (APIs, methods, patterns)?
- What caveats, limitations, or open questions did I find?

Extract what matters for the specific request. Don't answer questions that aren't relevant. Pick an appropriate length based on the complexity of the request.

Focus on capabilities and behavior, not file-by-file changes. Avoid excessive implementation details unless critical.

Format: plain text with bullets. No markdown headers, no bold/italic, no code fences. Use workspace-relative paths for files.
Output ONLY the handoff prompt text, nothing else.

<thread>
{{THREAD_CONTENT}}
</thread>

My request:
{{GOAL}}
