---
description: Investigate with Oracle until root-cause agreement, then fix
---
Investigate this bug thoroughly and coordinate with Oracle until you both converge on the likely root cause. Once both of you agree, immediately implement the fix.

Rules:
- Do not jump to implementation before root cause agreement.
- First inspect the codebase, logs, traces, related files, and recent changes.
- Gather concrete evidence before deciding anything.
- Ask Oracle to propose root-cause hypotheses based on the evidence.
- Critically evaluate Oracle's hypotheses yourself.
- If you do not agree with Oracle's leading hypothesis, or the evidence is still weak, continue investigating and ask Oracle again.
- Repeat until:
  1. you and Oracle converge on the most likely root cause, or
  2. there are 2–3 strong competing hypotheses that need my decision, or
  3. you hit a real blocker due to missing information.
- As soon as you and Oracle agree on the likely root cause, implement the fix.
- After implementing, review the fix for regressions, missed edge cases, and codebase convention issues.
- Prefer concrete verification when possible.
- Minimize assumptions throughout.

Loop:
inspect → ask Oracle for hypotheses → evaluate Oracle's hypotheses → gather more evidence if needed → repeat → agree → fix → review

Oracle's job:
- propose and rank root-cause hypotheses
- explain supporting evidence
- highlight missing evidence and alternatives

Your job:
- verify whether Oracle's hypotheses actually explain the bug
- reject weak or unsupported hypotheses
- keep investigating until you personally agree
- once agreement is reached, fix it and validate the result

At the end, give me:
- the agreed root cause
- the evidence for it
- what was fixed
- what was verified
- any remaining risks or uncertainty

Bug / context:
[paste issue, symptoms, logs, error, or ticket here]
