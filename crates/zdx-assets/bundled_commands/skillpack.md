---
description: Turn a project or library into an AI-usable skill package
---
Analyze this project or library and create a reusable skill package so future AI agents can use it correctly.

Rules:
- Optimize for future agent execution, not for human learning.
- If the target project/library or desired skill scope is ambiguous, ask one focused question before starting.
- For non-trivial requests, use `todo_write` to track discovery, synthesis, design, writing, and validation. Treat a request as non-trivial if it spans multiple project areas, requires multiple Explorer calls, involves external docs, or will produce/update files. For small single-pass requests, skip todo tracking.
- Inspect the real project, docs, examples, tests, manifests, and entrypoints before drafting the skill.
- Prefer Explorer for broad read-only discovery. Use multiple Explorer calls in parallel when the project has clearly separate areas to inspect.
- Use Oracle only when the correct skill boundary, API usage pattern, or architecture interpretation is unclear.
- Use Task only after the skill boundary, target location, and outline are confirmed, and only for the scoped write/validate phase. Pass Task the confirmed file paths, content plan, validation command, and constraints.
- Do not overwrite an existing skill or reference file without explicit confirmation.
- Keep the skill focused. If the project contains several unrelated workflows, propose multiple smaller skills instead of one broad skill.
- Put large reference material in supporting files next to `SKILL.md`; keep `SKILL.md` concise and actionable.
- Use existing skill conventions and validate the final skill if you save it.
- For large or unfamiliar projects, after initial exploration, provide an in-message checkpoint: areas inspected, authoritative docs/examples found, likely usage patterns, gaps/uncertainties, and next investigation focus.

Workflow:
1. Clarify the skill target if needed:
   - what project/library/tool should become skill-usable
   - who will use the skill: future ZDX agents
   - whether the skill should be project-local or global
   - the main tasks the skill should enable
2. Explore raw facts:
   - repository structure
   - README/docs
   - package/crate manifests
   - public APIs, CLI commands, config files, and examples
   - tests that show expected usage
   - setup, run, build, and verification commands
   - common errors, constraints, and gotchas
3. Research agent-usable meaning:
   - when the skill should trigger
   - what inputs the agent needs
   - exact usage workflows
   - authoritative references to cite/read
   - commands or code snippets future agents should reuse
   - validation steps that prove correct usage
   - non-goals and unsafe assumptions
4. Design the skill package:
   - decide whether this should be one skill or several smaller skills; prefer multiple skills when triggers, setup, commands, or user goals differ materially
   - propose a kebab-case skill name
   - propose the target location: project `.zdx/skills/<name>/` or user `$ZDX_HOME/skills/<name>/`
   - outline `SKILL.md` plus any supporting reference files
   - ask for confirmation before writing if the location or scope is not already explicit
5. Write and validate when confirmed:
   - create or update `SKILL.md`
   - add supporting references/examples only when they materially help future agents
   - validate the skill using the repo's skill validation path when available
   - fix validation errors before declaring done

Role separation:
- Explorer's role: gather source-grounded usage facts, examples, APIs, commands, and docs.
- Oracle's role: help decide skill boundaries or resolve ambiguous usage patterns.
- Task's role: create/update files and run validation after the skill design is confirmed.
- Your role: convert the findings into a minimal, reliable skill package for future AI use.

At the end, give me:
- the skill name and target location
- what tasks the skill enables
- the key references/examples included
- what files were created or changed
- validation result, if saved
- anything intentionally left out or still uncertain