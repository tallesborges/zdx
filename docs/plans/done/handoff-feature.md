# Handoff Feature - Implementation Plan

## Summary

Handoff allows users to start a new session with an AI-generated prompt that captures relevant context from the current session. Uses the subagent pattern (spawning `zdx exec`) to generate the handoff prompt.

## User Journey

1. User has an ongoing session with thread history
2. User opens command palette and selects `/handoff`
3. User enters goal text (e.g., "implement caching")
4. System shows "Generating handoff..." status
5. Subagent reads current session, generates focused prompt
6. Generated prompt appears in input textarea
7. User reviews/edits, presses Enter
8. New session starts with the crafted prompt

## Design Principles

- **KISS**: Reuse existing infrastructure (subagent via `zdx exec`, session persistence)
- **No new tools**: Subagent uses `bash` + `zdx sessions show` to read context
- **Ship-first**: Minimal slices, no polish until proven

---

# MVP Slices

## Slice 1: `/handoff` command in palette

- **Goal**: Add `/handoff` command that accepts a goal argument
- **Scope**:
  - [x] Add `handoff` command to `COMMANDS` 
  - [x] Command requires active session (error if none)
  - [x] Palette shows input field for goal after command selection
  - [x] On submit: trigger handoff generation effect
- **Demo**: Open palette → `/handoff` → type "implement caching" → see "Generating..."
- **Status**: ✅ Implemented

## Slice 2: Subagent-based generation

- **Goal**: Spawn subagent to generate handoff prompt
- **Scope**:
  - [x] Get current session ID
  - [x] Spawn: `zdx --no-save exec -p "<generation prompt>"`
  - [x] Generation prompt instructs subagent to:
    - Read session via `zdx sessions show <id>`
    - Generate focused handoff prompt for the goal
    - Include relevant context, decisions, files
    - Output ONLY the prompt text
  - [x] Capture stdout as generated prompt
  - [x] Handle errors (timeout, empty output)
- **Demo**: `/handoff implement caching` → waits → generated prompt captured
- **Status**: ✅ Implemented

## Slice 3: Draft to input + new session

- **Goal**: Show generated prompt in input, create new session on submit
- **Scope**:
  - [x] Set input textarea content to generated prompt
  - [x] Show status: "Handoff ready. Edit and press Enter."
  - [x] On submit:
    - [x] Create new session (reuse `/new` logic)
    - [x] Send the prompt as first message
  - [x] On Esc: clear input, cancel handoff
- **Demo**: Generated prompt in input → edit → Enter → new session → agent responds
- **Status**: ✅ Implemented

---

# Implementation Notes

## Generation Prompt Template

```
Read session {session_id} using this command:
zdx sessions show {session_id}

Based on that session, generate a focused handoff prompt for the goal: "{goal}"

Include:
- Relevant context and decisions made
- Key files or code discussed
- The specific goal/direction

Output ONLY the handoff prompt text, nothing else. The prompt should be 
written as if the user is starting a fresh thread with a new agent.
```

## Subprocess Spawning

```rust
let output = Command::new(std::env::current_exe()?)
    .args(["exec", "-p", &generation_prompt, "--no-save"])
    .current_dir(&root)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .output()?;

let generated = String::from_utf8_lossy(&output.stdout).trim().to_string();
```

---

# Contracts

- `/new` behavior unchanged
- Normal message submission unchanged
- Session JSONL format unchanged
- `/handoff` requires active session (error if none)
- `/handoff` with empty goal shows error
- Esc cancels handoff and clears input
- Subagent failure shows error, preserves goal for retry

# Testing

- **Manual smoke**:
  - Slice 1: `/handoff` appears in palette, accepts goal input
  - Slice 2: Subagent spawns, generates prompt (check with simple goal)
  - Slice 3: Prompt in input → submit → new session works

---

# Deferred

| Item | Trigger |
|------|---------|
| Streaming generation progress | Generation feels too slow |
| Configurable generation prompt | Default prompt insufficient |
| Handoff chain tracking | Need to debug handoff history |
