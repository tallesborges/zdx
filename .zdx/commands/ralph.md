---
description: Execute an approved plan slice-by-slice (Explorer + Oracle per slice)
---
Execute this approved plan autonomously.

Rules:
- Treat each slice in the plan as a checkpoint.
- Work on only one slice at a time.
- Before starting a slice, use Explorer to gather any missing local codebase context needed for that slice.
- Do not make avoidable assumptions. If the codebase can answer something, inspect it first.
- When a slice is implemented, update the plan/status for that slice immediately (for example: mark it done, note what changed, and record any important deviations or follow-ups).
- After finishing each slice, stop implementation and ask Oracle to review that slice before moving on.
- Oracle review must check for:
  - bugs
  - regressions
  - missed edge cases
  - violations of codebase conventions
  - anything that makes the slice unsafe to build on
- If Oracle finds real issues, fix them before moving to the next slice.
- Re-run Oracle review after major fixes if needed.
- Only proceed to the next slice when the current slice is in a good state.
- Keep going slice by slice until:
  1. all slices are implemented and reviewed,
  2. you hit a real blocker that needs my decision,
  3. you need missing external information.
- Minimize assumptions. Inspect the codebase before deciding.
- Prefer concrete verification when possible.

Execution loop:
slice → Explorer context check → implement → update slice status → Oracle review → fix if needed → verify → next slice

Requirements:
- Use Explorer for context gathering.
- Use Oracle for review.
- Prefer concrete verification when possible.
- Keep the plan/status in sync with reality as you progress.

At the end, give me:
- what was implemented per slice
- how each slice status was updated
- what Explorer clarified for each slice
- what Oracle found for each slice
- what issues were fixed
- any remaining risks or blockers

Here is the approved plan:
[paste plan here]
