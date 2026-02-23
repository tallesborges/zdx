# Automation Templates

## Scheduled automation

```md
---
schedule: "0 8 * * *"
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
# max_retries: 1
---

# Goal
<one-sentence goal>

# Inputs
- <source 1>
- <source 2>
- Assumptions: <explicit assumptions>

# Execution Steps
1. <step>
2. <step>

# Output Format
- <required sections / limits>

# Empty State
If <nothing to report condition>: `<short message>.`

# Failure Policy
- If a non-critical source fails, continue with available data and state what failed.
```

## Manual-only automation

```md
---
# model: "gemini-cli:gemini-2.5-flash"
# timeout_secs: 900
---

# Goal
<one-sentence goal>

# Inputs
- <source>

# Execution Steps
1. <step>

# Output Format
- <required sections / limits>

# Empty State
<what to return when nothing to do>

# Failure Policy
- <error handling rules>
```
