---
description: capture a convention or learning from this thread into AGENTS.md
---
Capture a durable learning, convention, or gotcha from this thread into the project's `AGENTS.md` (or `CLAUDE.md`, whichever the project uses).

Phase 1 — Identify the learning (silent):
- Scan the thread for the insight worth persisting: a non-obvious convention, a "we tried X, the idiomatic way is Y" moment, a gotcha that bit us, or a rule the user ratified by correcting the assistant.
- If multiple plausible candidates exist, list them in one short message and ask which to save. Otherwise proceed silently.

Phase 2 — Locate the target file:
- Find the most relevant `AGENTS.md` / `CLAUDE.md` in scope: nearest one walking up from the affected files, or the repo root if the learning is repo-wide.
- If the learning belongs to a nested package or workspace, save it in that package's file, not the root.

Phase 3 — Draft the entry:
- One or two lines. Bold lead-in, then the rule.
- Frame as a principle, not the specific instance from the thread. The thread's example is just the trigger, not the wording.
- Every line must pass: "would removing this make the agent get something wrong?" If no, drop it.
- Place it in the most fitting existing section. Create a new section only if none of the existing ones fit.

Phase 4 — Apply:
- Show the user the exact bullet you plan to add and the section it goes in, before editing.
- After confirmation, edit the file. Do not silently rephrase neighbouring bullets.

Rules:
- Do not duplicate guidance already covered. If similar advice exists, propose tightening the existing bullet instead of adding a new one.
- Do not invent context the thread doesn't support. The entry must be traceable to something that actually happened in this thread.
- Keep the wording generic enough to apply beyond the specific case that triggered it. If the user pushes back as "too strict / not generic enough", iterate before saving.

At the end, give me:
- the file path edited
- the exact bullet added (or the diff if you tightened an existing one)
- which section it landed in
- which thread message the learning was sourced from