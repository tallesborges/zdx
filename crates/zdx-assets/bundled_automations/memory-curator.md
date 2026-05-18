---
timeout_secs: 600
max_retries: 1
---

# Goal

Read recent ZDX threads and propose durable memory items the user should save to their second brain. Produce a dated suggestion report the user can review and selectively file — never auto-write into the user's notes.

# Inputs

- Recent conversation transcripts available via the `Thread_Search` and `Read_Thread` tools.
- The current memory index and existing notes via `Memory_Search` (collection `note`) and `Memory_Get`.
- The destination for the review report: `$ZDX_HOME/memory_suggestions/<YYYY-MM-DD>.md` (compose the path with `$ZDX_HOME` resolved via `echo $ZDX_HOME` in a `bash` call if needed).

# Execution steps

Run these in order. Continue with whatever succeeded if a later step fails.

1. **Discover recent threads.** Call `Thread_Search` with `limit: 20` and no query, sorted implicitly by recency. If the model can derive a `date_start` for the last 7 days (`YYYY-MM-DD`), pass it. Skip any thread whose id starts with `automation-memory-curator-` (those are prior runs of this automation and would cause feedback loops).
2. **Pick at most 10 distinct threads** that look like real conversations (user/assistant turns, decisions, learnings). Skip empty or trivial ones.
3. **Extract candidates per thread.** For each picked thread call `Read_Thread` with a tight goal, for example:
   > "Extract durable, reusable memory items: stable preferences, decisions made, factual claims about the user, recurring patterns, useful links, lessons learned. Return a JSON array of objects with fields: type ('preference'|'decision'|'fact'|'pattern'|'link'|'learning'), title (≤80 chars), detail (≤2 short sentences), evidence (one short verbatim quote ≤200 chars). Exclude transient/one-off items, secrets, and anything already obviously routine."
4. **Dedupe against existing memory.** For each candidate, call `Memory_Search` with `source: "note"` and `strategy: "hybrid"` using the candidate title as the query. If the top hit looks like the same item, mark it as `existing` and skip from the new-suggestions list; if it's a close variant, mark it as `update` with the existing note pointer. Use `Memory_Get` only when needed to confirm.
5. **Write the review report.** Use the `write` tool to create `$ZDX_HOME/memory_suggestions/<YYYY-MM-DD>.md`. If the file already exists for today, overwrite it with the regenerated contents (do not append). Format:

   ```markdown
   # Memory Curator — <YYYY-MM-DD>

   _Source threads scanned: N. New suggestions: K. Likely updates: U. Already-known: E._

   ## New suggestions

   ### 1. [type] Title
   - **Detail:** ...
   - **Evidence:** "<short quote>" — thread `<thread_id>`
   - **Proposed destination:** `MEMORY.md` index pointer **or** new note under `$ZDX_MEMORY_ROOT/Notes/<folder>/<title>.md`

   ## Likely updates

   ### 1. [type] Title
   - **Existing note:** `<path>` (qmd docid `<#abc123>`)
   - **What changed / what to merge:** ...

   ## Skipped (already known)

   - [type] Title — matches `<existing note path>`
   ```

6. **Print a compact summary** in the final response: counts, top three new suggestions by line, and the absolute path of the review file. Keep it under ~20 lines so the run thread stays scannable.

# Tool surface

You have access to: `Thread_Search`, `Read_Thread`, `Memory_Search`, `Memory_Get`, `write`, `read`, `bash`, plus the standard exploration tools. Prefer the structured tools over shelling out.

# Output format

- Review file: `$ZDX_HOME/memory_suggestions/<YYYY-MM-DD>.md` (always written, even when empty).
- Final agent message: short summary as described in step 6.

# Empty state

If no threads were found or no durable items were extracted, still write the review file with body:

```markdown
# Memory Curator — <YYYY-MM-DD>

_No new memory suggestions today._
```

Then in the final agent message: `No new memory suggestions today. Wrote <path>.`

# Failure policy

- If `Thread_Search` fails or returns nothing, log the cause inline and emit the empty-state report.
- If `Read_Thread` fails on a specific thread, skip that thread and continue with the others; note the skip in the report under a `## Errors` section.
- If `Memory_Search` is unavailable, emit suggestions without dedupe and note `[memory dedupe unavailable]` in the report header.
- If the `write` tool fails, still emit the summary in the final message and surface the write error.

# Non-goals

- Do NOT auto-write into `$ZDX_MEMORY_ROOT/Notes/`, `MEMORY.md`, or `.zdx/knowledge/` directories. The user reviews and files items themselves.
- Do NOT delete or edit existing notes. This automation only reads and suggests.
- Do NOT include secrets, credentials, tokens, or anything that looks like a personal identifier the user did not already publish in their notes.
