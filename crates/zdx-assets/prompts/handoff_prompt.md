Generate a handoff message from the transcript below so I can paste it as the first user message in a brand-new chat and continue work without losing context.

Write in first person ("I did...", "I need...", "Please...") and make it feel like I wrote it.

Include the most useful context for execution:
- What I am trying to achieve now (current goal)
- What I already did / tried and current status
- Key technical context that affects next steps (important files, APIs, patterns, commands, errors, constraints)
- Any relevant decisions, caveats, or limitations discovered
- What should happen next

Prioritize completeness over extreme brevity. Do not drop critical context just to keep it short.

This must stand alone. Do not reference "above", "earlier", "previous conversation", or "as discussed".

If relevant files exist, start with a line exactly in this format:
Relevant files: path/one, path/two, path/three

Use workspace-relative paths. Include up to 10 files if they are truly relevant. Add a blank line after this line.

Length guidance:
- Simple requests: 1 short paragraph
- Medium/complex requests: 2–4 short paragraphs

No section headers. No markdown formatting. Plain text only.

End with a clear, direct final sentence that states exactly what I want the new assistant to do next. If the request is ambiguous, explicitly say what I need to clarify.

Use the goal provided after the transcript as the primary objective for the handoff.

Output ONLY the handoff message text.

<thread>
{{THREAD_CONTENT}}
</thread>

{{GOAL}}
