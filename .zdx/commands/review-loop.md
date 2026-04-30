---
description: Multi-pass Oracle reviews on the implementation, fix valid issues
---
Review the implementation I just finished.

Rules:
- Do not assume it is correct just because it compiles or seems complete.
- Run multiple Oracle review passes, each with a different focus.
- After each review pass:
  1. summarize the findings,
  2. decide whether they are real issues,
  3. fix the valid issues,
  4. continue to the next review pass.
- Only stop when all review passes are done and no important issues remain.

Review passes:
1. Bug review
   - Look for logic bugs, broken flows, missing edge cases, incorrect assumptions.
2. Regression review
   - Look for things this implementation may have broken in nearby behavior.
3. Convention / quality review
   - Look for codebase convention mismatches, weak structure, unclear naming, or unsafe patterns.
4. Final synthesis review
   - Re-check whether the result is actually good enough to ship.

Execution loop:
review pass → judge findings → fix valid issues → next review pass

Important:
- Be skeptical.
- Minimize false positives.
- Do not nitpick unless it affects correctness, maintainability, or project conventions in a meaningful way.
- Prefer concrete verification when possible.

At the end, give me:
- what each review pass found
- what was fixed after each pass
- any remaining risks or unresolved concerns
