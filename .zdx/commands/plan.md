---
description: Plan in a loop with Oracle (draft → review → revise until agreement)
---
Create an implementation plan for this request in an iterative loop with Oracle.

Rules:
- Do not settle for the first draft.
- First inspect the codebase and gather the context you need.
- Minimize assumptions. If something can be checked in the codebase, inspect it instead of guessing.
- If the request is ambiguous or important information is missing, ask focused follow-up questions before finalizing the plan.
- Draft the plan yourself first.
- Then ask Oracle to review the draft plan and bring:
  - challenges to your assumptions
  - missing context
  - architectural risks
  - sequencing problems
  - alternative solutions
  - better implementation ideas if relevant
- Critically evaluate Oracle's suggestions yourself.
- If you and Oracle do not yet agree that the plan is strong enough, revise the plan and loop again.
- Repeat until:
  1. both you and Oracle are confident in the plan,
  2. there are open questions that require my input,
  3. or you hit a real blocker due to missing information.
- Keep the plan practical, explicit, and grounded in the real codebase.

Planning loop:
inspect → ask follow-ups if needed → draft → Oracle review → revise → repeat until agreement

Oracle's role:
- validate the plan
- challenge weak assumptions
- suggest better sequencing or safer solutions
- point out missing context and alternative approaches

Your role:
- create the draft
- judge Oracle's ideas critically
- improve the plan until you personally agree it is strong enough

At the end, provide:
- the final plan
- the key assumptions
- what Oracle changed or improved
- any remaining open questions or risks
