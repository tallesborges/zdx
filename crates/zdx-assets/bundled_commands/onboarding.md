---
description: Onboard to a project or library through source-grounded explanation
---
Onboard me to this project or library using a DeepWiki-style workflow, then explain it in a way that helps me understand and work with it.

Rules:
- Optimize for human learning, not for creating durable agent instructions.
- If the target project/library is ambiguous, ask one focused question before starting.
- For non-trivial requests, use `todo_write` to track discovery, synthesis, and delivery. Treat a request as non-trivial if it spans multiple project areas, requires multiple Explorer calls, or involves external docs. For small single-pass requests, skip todo tracking.
- Inspect the real project, docs, examples, tests, manifests, and entrypoints before explaining.
- Prefer Explorer for broad read-only discovery. Use multiple Explorer calls in parallel when the project has clearly separate areas to inspect.
- Use Oracle only when synthesis is difficult, the architecture is unclear, or there are competing interpretations.
- Do not use Task unless I explicitly ask for edits or a saved artifact.
- Do not modify files unless I explicitly ask for edits.
- Do not create a skill, note, wiki, scratch markdown, or documentation file unless I explicitly ask for a saved artifact.
- Keep the final answer at the level I asked for: overview if I ask overview, deeper guide if I ask deeper.
- For large or unfamiliar projects, after initial exploration, provide an in-message checkpoint: areas inspected, authoritative docs/examples found, likely architecture/workflows, gaps/uncertainties, and next investigation focus.

Workflow:
1. Clarify the target and learning goal if needed.
2. Explore raw facts:
   - repository structure
   - README/docs
   - manifests and dependency files
   - entrypoints
   - examples and tests
   - important modules/packages/crates
   - run/build/test commands when obvious from project files
3. Research meaning from those facts:
   - what the project is for
   - main mental model
   - architecture and major boundaries
   - key workflows and data/control flow
   - main public APIs, CLI surfaces, or integration points
   - conventions, gotchas, and things to read first
4. Compose a learning-oriented explanation:
   - start with the shortest useful summary
   - explain the project in progressive layers
   - connect concepts to concrete files or examples when helpful
   - include a suggested reading path or next practice task when useful

Role separation:
- Explorer's role: gather source-grounded facts and likely files/flows.
- Oracle's role: help synthesize architecture only when the evidence is complex or ambiguous.
- Task's role: none by default; use only if I explicitly ask for edits or a saved study artifact.
- Your role: decide what matters for my learning goal and explain it clearly.

At the end, give me:
- what the project/library is
- the core mental model
- the main architecture/workflows
- the most important files/modules/docs to read
- how to run/use it if discoverable
- gotchas or uncertainties
- a recommended next learning step