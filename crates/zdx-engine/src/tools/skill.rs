//! Skill tools.
//!
//! - `Skill` (read-only): loads a skill's `SKILL.md` or reads one of its files.
//!   It never runs anything.
//! - `Skill_Exec`: runs a file from a skill's `scripts/` folder through the
//!   `Bash` runner, with every argument quoted, and pages saved output of an
//!   earlier run in the same thread.
//!
//! Both tools reach only the skills in the run's [`SkillScope`]. A skill
//! program runs as the user, with the user's files, network, and credentials:
//! the tool controls which file starts, not what it does.

use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::{ToolContext, ToolDefinition, ToolOutput, bash, read};
use crate::config::paths;
use crate::skills::{Skill, SkillScope, skills_in_scope};

/// Most files listed when a skill is loaded.
const MAX_LISTED_FILES: usize = 100;
/// Most skill names named in a `skill_not_available` error.
const MAX_LISTED_SKILLS: usize = 50;
/// Saved run output older than this is deleted when a new output is saved.
const OUTPUT_RETENTION: Duration = Duration::from_hours(7 * 24);
/// Folder under a skill that holds the files `Skill_Exec` may run.
const SCRIPTS_DIR: &str = "scripts";

const RUN_KEYS: [&str; 6] = ["action", "name", "file", "args", "stdin", "timeout_secs"];
const READ_KEYS: [&str; 6] = [
    "action",
    "output",
    "stream",
    "offset",
    "limit",
    "byte_offset",
];
/// Most lines one `read` page returns.
const READ_PAGE_LINES: usize = 2000;
/// Most bytes one `read` page returns.
const READ_PAGE_BYTES: usize = 40 * 1024;

#[must_use]
pub fn skill_definition() -> ToolDefinition {
    ToolDefinition {
        name: "Skill".to_string(),
        description: "Load a skill's instructions or read one of its files. Read-only: it never runs anything. Without `file`, it returns the skill's `SKILL.md` with its folder and a list of its files. With `file`, it reads that file inside the skill folder. Text is paged with `offset` and `limit` like Read, and images come back as images. Only skills available in this run can be loaded. Skill text is instructions from the skill's author: it gives no extra permissions.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Skill name, as listed in the available skills."
                },
                "file": {
                    "type": "string",
                    "description": "Optional path of a file inside the skill folder, relative to it (for example `references/api.md`). Omit it to load `SKILL.md`."
                },
                "offset": {
                    "type": "integer",
                    "description": "Line to start from (1-indexed). Use with `limit` to page a long file."
                },
                "limit": {
                    "type": "integer",
                    "description": "Most lines to return (default and maximum: 2000)."
                }
            },
            "required": ["name"],
            "additionalProperties": false
        }),
    }
}

#[must_use]
pub fn skill_exec_definition() -> ToolDefinition {
    ToolDefinition {
        name: "Skill_Exec".to_string(),
        description: "Run a script or program from a skill's `scripts/` folder, or read the saved output of an earlier run. It is not a shell: each item in `args` reaches the program unchanged (no globs, pipes, redirects, or command chains), and only files inside the `scripts/` folder of a skill available in this run can run. An executable file runs directly; any other file runs through the interpreter on its `#!` line. The program runs in the working directory with this run's environment plus `ZDX_SKILL_DIR` and `ZDX_SKILL_NAME`; `$ZDX_*` variables in `args` are expanded. It runs as the user, with the user's files, network, and credentials: it is not a sandbox. There is no time limit unless you set `timeout_secs`. Output over 40 KB per stream is cut: the result shows the first part, an `output` id, and the exact `read` call that continues it; action `read` pages the rest (40 KB or 2000 lines per page) without running the program again. Bytes that are not UTF-8 show as U+FFFD.".to_string(),
        input_schema: json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["run", "read"],
                    "description": "`run` starts a program (takes name, file, args, stdin, timeout_secs). `read` pages saved output (takes output, stream, offset, limit, byte_offset)."
                },
                "name": {
                    "type": "string",
                    "description": "run: skill name, as listed in the available skills."
                },
                "file": {
                    "type": "string",
                    "description": "run: path of the program inside the skill folder, for example `scripts/report.py`."
                },
                "args": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "run: arguments, passed as-is. `$ZDX_*` variables are expanded; nothing else is."
                },
                "stdin": {
                    "type": "string",
                    "description": "run: text sent to the program's standard input. Without it, standard input is empty."
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "run: stop the program after this many seconds. Omit it or pass 0 for no limit."
                },
                "output": {
                    "type": "string",
                    "description": "read: output id from an earlier run in this thread, for example `run-3f9a2c1b`."
                },
                "stream": {
                    "type": "string",
                    "enum": ["stdout", "stderr"],
                    "description": "read: which stream to read (default: stdout)."
                },
                "offset": {
                    "type": "integer",
                    "description": "read: line to start from (1-indexed)."
                },
                "limit": {
                    "type": "integer",
                    "description": "read: most lines to return (default and maximum: 2000). A page also stops at 40 KB."
                },
                "byte_offset": {
                    "type": "integer",
                    "description": "read: byte to start from inside the `offset` line, to continue a line longer than one page. Use the `next_byte_offset` a result gives."
                }
            },
            "required": ["action"],
            "additionalProperties": false
        }),
    }
}

// -- Skill (read-only) --

/// Executes the `Skill` tool.
pub async fn execute_skill(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let Some(object) = input.as_object() else {
        return invalid_input("Skill input must be an object.");
    };
    if let Some(key) = object
        .keys()
        .find(|key| !["name", "file", "offset", "limit"].contains(&key.as_str()))
    {
        return invalid_input(format!(
            "`{key}` is not a Skill parameter (it takes: name, file, offset, limit)."
        ));
    }
    let name = match required_string(object, "name") {
        Ok(name) => name,
        Err(output) => return output,
    };
    let file = match optional_string(object, "file") {
        Ok(file) => file,
        Err(output) => return output,
    };

    let ctx_for_lookup = ctx.clone();
    let paging = paging_fields(object);
    super::execute_blocking(ctx.timeout, move || {
        load_or_read(&ctx_for_lookup, &name, file.as_deref(), &paging)
    })
    .await
}

fn load_or_read(
    ctx: &ToolContext,
    name: &str,
    file: Option<&str>,
    paging: &Map<String, Value>,
) -> ToolOutput {
    let skill = match find_skill(ctx, name) {
        Ok(skill) => skill,
        Err(output) => return output,
    };
    let base = match canonical_dir(&skill.base_dir) {
        Ok(base) => base,
        Err(output) => return output,
    };
    let (target, relative) = match file {
        None => match skill.file_path.canonicalize() {
            Ok(path) => (path, "SKILL.md".to_string()),
            Err(err) => {
                return ToolOutput::failure(
                    "read_error",
                    format!("Cannot open SKILL.md of skill '{name}'"),
                    Some(err.to_string()),
                );
            }
        },
        Some(file) => match resolve_inside(&base, &base, file, name) {
            Ok(path) => (path, file.trim().to_string()),
            Err(output) => return output,
        },
    };

    let mut read_input = paging.clone();
    read_input.insert(
        "file_path".to_string(),
        Value::String(target.to_string_lossy().into_owned()),
    );
    let mut output = read::execute(&Value::Object(read_input), &ctx.as_leaf());
    if let ToolOutput::Success { data, .. } = &mut output
        && let Some(data) = data.as_object_mut()
    {
        data.remove("file_path");
        data.remove("resolved_file_path");
        data.insert("skill".to_string(), Value::String(skill.name.clone()));
        data.insert("file".to_string(), Value::String(relative));
        rewrite_page_warning(data);
        if file.is_none() {
            data.insert(
                "directory".to_string(),
                Value::String(base.to_string_lossy().into_owned()),
            );
            let (files, total) = list_skill_files(&base);
            data.insert("files_total".to_string(), Value::from(total));
            if total > files.len() {
                data.insert("files_truncated".to_string(), Value::Bool(true));
            }
            data.insert(
                "files".to_string(),
                Value::Array(files.into_iter().map(Value::String).collect()),
            );
        }
    }
    output
}

/// Replaces `Read`'s paging warning, which points at other tools, with one
/// that names only this tool's own parameters.
fn rewrite_page_warning(data: &mut Map<String, Value>) {
    let Some(warning) = data.get("warning").and_then(Value::as_str) else {
        return;
    };
    let number = |key: &str| data.get(key).and_then(Value::as_u64).unwrap_or(0);
    let (offset, shown, total) = (
        number("offset"),
        number("lines_shown"),
        number("total_lines"),
    );
    let last = (offset + shown).saturating_sub(1);
    let next = offset + shown;
    let text = if warning.contains("mid-line") {
        format!(
            "This page stopped at about 40 KB inside line {last}, which is longer than a page; the rest of that line is not shown. Continue after it with offset={next}."
        )
    } else {
        format!("Showing lines {offset}-{last} of {total}. Continue with offset={next}.")
    };
    data.insert("warning".to_string(), Value::String(text));
}

/// Lists the skill's files as `/`-separated paths relative to `base`, sorted,
/// without `SKILL.md`, hidden entries, `__pycache__`, or `node_modules`.
/// Returns at most [`MAX_LISTED_FILES`] paths and the total count.
fn list_skill_files(base: &Path) -> (Vec<String>, usize) {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>, depth: usize) {
        if depth > 8 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || name == "__pycache__" || name == "node_modules" {
                continue;
            }
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                walk(&path, base, out, depth + 1);
            } else if let Ok(relative) = path.strip_prefix(base) {
                let relative = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if relative != "SKILL.md" {
                    out.push(relative);
                }
            }
        }
    }

    let mut files = Vec::new();
    walk(base, base, &mut files, 0);
    files.sort();
    let total = files.len();
    files.truncate(MAX_LISTED_FILES);
    (files, total)
}

// -- Skill_Exec --

/// Executes the `Skill_Exec` tool.
pub async fn execute_skill_exec(input: &Value, ctx: &ToolContext) -> ToolOutput {
    let Some(object) = input.as_object() else {
        return invalid_input("Skill_Exec input must be an object.");
    };
    let action = match object.get("action") {
        Some(Value::String(action)) => action.trim().to_ascii_lowercase(),
        Some(_) => return invalid_input("`action` must be \"run\" or \"read\"."),
        None => return invalid_input("`action` is required: \"run\" or \"read\"."),
    };
    let (allowed, other_action, other_keys): (&[&str], &str, &[&str]) = match action.as_str() {
        "run" => (&RUN_KEYS, "read", &READ_KEYS),
        "read" => (&READ_KEYS, "run", &RUN_KEYS),
        other => {
            return invalid_input(format!(
                "Unknown action \"{other}\": use \"run\" or \"read\"."
            ));
        }
    };
    if let Some(key) = object.keys().find(|key| !allowed.contains(&key.as_str())) {
        let hint = if other_keys.contains(&key.as_str()) {
            format!(" It belongs to action \"{other_action}\".")
        } else {
            String::new()
        };
        return invalid_input(format!(
            "`{key}` is not a parameter of action \"{action}\" (it takes: {}).{hint}",
            allowed[1..].join(", ")
        ));
    }

    if action == "read" {
        let ctx = ctx.clone();
        let object = object.clone();
        return super::execute_blocking(ctx.timeout, move || read_saved_output(&ctx, &object))
            .await;
    }
    run(object, ctx).await
}

/// A validated `run` request, ready to hand to the `Bash` runner.
struct RunPlan {
    skill: String,
    file: String,
    command: String,
    timeout_secs: Option<u64>,
    /// Holds the stdin file until the program has finished.
    stdin_file: Option<tempfile::NamedTempFile>,
}

async fn run(object: &Map<String, Value>, ctx: &ToolContext) -> ToolOutput {
    let plan = {
        let ctx = ctx.clone();
        let object = object.clone();
        match tokio::task::spawn_blocking(move || plan_run(&ctx, &object)).await {
            Ok(Ok(plan)) => plan,
            Ok(Err(output)) => return output,
            Err(_) => {
                return ToolOutput::failure("panic", "Skill_Exec setup panicked", None);
            }
        }
    };

    let mut bash_input = json!({ "command": plan.command });
    if let Some(timeout) = plan.timeout_secs {
        bash_input["timeout_secs"] = Value::from(timeout);
    }
    let leaf = ctx.as_leaf();
    let deadline = ctx.timeout;
    let output = super::run_with_output_deltas(ctx, move |output_tx| async move {
        bash::execute_without_background(&bash_input, &leaf, deadline, output_tx).await
    })
    .await;
    drop(plan.stdin_file);

    let thread_id = ctx.current_thread_id.clone();
    let (skill, file) = (plan.skill, plan.file);
    match tokio::task::spawn_blocking(move || {
        finish_run(output, thread_id.as_deref(), &skill, &file)
    })
    .await
    {
        Ok(output) => output,
        Err(_) => ToolOutput::failure("panic", "Skill_Exec result handling panicked", None),
    }
}

fn plan_run(ctx: &ToolContext, object: &Map<String, Value>) -> Result<RunPlan, ToolOutput> {
    let name = required_string(object, "name")?;
    let file = required_string(object, "file")?;
    let args = parse_args(object.get("args"))?;
    let timeout_secs = whole_number(object, "timeout_secs")?;
    let stdin = match object.get("stdin") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return Err(invalid_input("`stdin` must be a string.")),
    };

    let skill = find_skill(ctx, &name)?;
    let base = canonical_dir(&skill.base_dir)?;
    let scripts = base.join(SCRIPTS_DIR).canonicalize().map_err(|_missing| {
        ToolOutput::failure(
            "not_runnable",
            format!("Skill '{name}' has no `{SCRIPTS_DIR}/` folder, so it has nothing to run."),
            None,
        )
    })?;
    if !scripts.starts_with(&base) {
        return Err(ToolOutput::failure(
            "path_not_allowed",
            format!("The `{SCRIPTS_DIR}/` folder of '{name}' resolves outside the skill folder."),
            Some(format!("It points to {}", scripts.display())),
        ));
    }
    let program = resolve_inside(&base, &scripts, &file, &name)?;
    let mut command_line = launch_argv(&program, &file)?;

    let skill_dir = base.to_string_lossy().into_owned();
    for arg in args {
        if arg.contains('\0') {
            return Err(invalid_input("`args` cannot contain NUL characters."));
        }
        command_line.push(expand_zdx_vars(&arg, |var| match var {
            "ZDX_SKILL_DIR" => Some(skill_dir.clone()),
            "ZDX_SKILL_NAME" => Some(skill.name.clone()),
            _ => std::env::var(var).ok(),
        }));
    }

    let stdin_file = stdin
        .map(|text| {
            let mut file = tempfile::Builder::new()
                .prefix("zdx-skill-stdin-")
                .tempfile()
                .map_err(|err| {
                    ToolOutput::failure("io_error", "Cannot prepare stdin", Some(err.to_string()))
                })?;
            std::io::Write::write_all(&mut file, text.as_bytes()).map_err(|err| {
                ToolOutput::failure("io_error", "Cannot prepare stdin", Some(err.to_string()))
            })?;
            Ok::<_, ToolOutput>(file)
        })
        .transpose()?;

    let mut command = format!(
        "export ZDX_SKILL_DIR={} ZDX_SKILL_NAME={}; exec",
        sh_quote(&skill_dir),
        sh_quote(&skill.name)
    );
    for part in &command_line {
        command.push(' ');
        command.push_str(&sh_quote(part));
    }
    if let Some(stdin_file) = &stdin_file {
        command.push_str(" < ");
        command.push_str(&sh_quote(&stdin_file.path().to_string_lossy()));
    }

    Ok(RunPlan {
        skill: skill.name,
        file: file.trim().to_string(),
        command,
        timeout_secs,
        stdin_file,
    })
}

/// How the program starts: directly when it is executable, otherwise through
/// the interpreter on its `#!` line, read the way this host's kernel reads it
/// for an executable file (see [`shebang_words`]).
fn launch_argv(program: &Path, file: &str) -> Result<Vec<String>, ToolOutput> {
    let path = program.to_string_lossy().into_owned();
    if is_executable(program) {
        return Ok(vec![path]);
    }
    let Some(line) = shebang_line(program) else {
        return Err(ToolOutput::failure(
            "not_runnable",
            format!(
                "`{file}` is not executable and has no `#!` line, so there is no way to start it."
            ),
            None,
        ));
    };
    let mut argv = shebang_words(&line);
    if !Path::new(&argv[0]).is_absolute() {
        return Err(ToolOutput::failure(
            "not_runnable",
            format!("The `#!` line of `{file}` must name an absolute interpreter path."),
            Some(format!("Found: #!{line}")),
        ));
    }
    argv.push(path);
    Ok(argv)
}

/// Splits a `#!` line (without `#!`, trimmed, not empty) into the interpreter
/// and its arguments, the way this host's kernel does, so a script starts the
/// same way whether or not it has the executable bit.
///
/// Linux passes everything after the interpreter as one argument
/// (`#!/usr/bin/env -S uv run` gives `env` the single argument `-S uv run`).
/// macOS and the BSDs split it on whitespace. Neither handles quotes.
fn shebang_words(line: &str) -> Vec<String> {
    if cfg!(target_os = "linux") {
        match line.split_once([' ', '\t']) {
            Some((interpreter, rest)) if !rest.trim().is_empty() => {
                vec![interpreter.to_string(), rest.trim().to_string()]
            }
            Some((interpreter, _)) => vec![interpreter.to_string()],
            None => vec![line.to_string()],
        }
    } else {
        line.split_whitespace().map(str::to_string).collect()
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).is_ok_and(|meta| meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    false
}

/// The interpreter line without `#!`, trimmed, when the file starts with one.
fn shebang_line(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = [0_u8; 512];
    let mut file = fs::File::open(path).ok()?;
    let read = file.read(&mut head).ok()?;
    let head = &head[..read];
    let rest = head.strip_prefix(b"#!")?;
    let end = rest
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(rest.len());
    let line = std::str::from_utf8(&rest[..end]).ok()?.trim();
    (!line.is_empty()).then(|| line.to_string())
}

/// Replaces `$ZDX_NAME` and `${ZDX_NAME}` with `lookup(name)`. Other text,
/// other variables, and unknown `ZDX_` variables stay as written.
fn expand_zdx_vars(input: &str, lookup: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let (braced, body) = match after.strip_prefix('{') {
            Some(body) => (true, body),
            None => (false, after),
        };
        let len = body
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(body.len());
        let name = &body[..len];
        let closed = !braced || body[len..].starts_with('}');
        let value = (name.starts_with("ZDX_") && closed)
            .then(|| lookup(name))
            .flatten();
        if let Some(value) = value {
            out.push_str(&value);
            rest = &body[len + usize::from(braced)..];
        } else {
            out.push('$');
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// Quotes `value` as one word for `/bin/sh`.
fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Moves output that the `Bash` runner cut into the thread's store and points
/// the result at it.
fn finish_run(
    mut output: ToolOutput,
    thread_id: Option<&str>,
    skill: &str,
    file: &str,
) -> ToolOutput {
    let ToolOutput::Success { data, .. } = &mut output else {
        return output;
    };
    let Some(data) = data.as_object_mut() else {
        return output;
    };
    data.insert("skill".to_string(), Value::String(skill.to_string()));
    data.insert("file".to_string(), Value::String(file.to_string()));

    let spills: Vec<(&str, PathBuf)> = ["stdout", "stderr"]
        .into_iter()
        .filter_map(|stream| {
            data.remove(&format!("{stream}_file"))
                .and_then(|path| path.as_str().map(PathBuf::from))
                .map(|path| (stream, path))
        })
        .collect();
    if spills.is_empty() {
        return output;
    }

    let saved = thread_id.and_then(output_store).and_then(|store| {
        prune_old_outputs(store.parent()?);
        allocate_run_dir(&store).ok()
    });
    let Some((id, run_dir)) = saved else {
        for (_, path) in &spills {
            let _ = fs::remove_file(path);
        }
        data.insert("output_saved".to_string(), Value::Bool(false));
        data.insert(
            "message".to_string(),
            Value::String(
                "Output was cut at 40 KB and the rest was not kept: this run has no saved thread."
                    .to_string(),
            ),
        );
        return output;
    };

    let mut notes = Vec::new();
    for (stream, spill) in spills {
        let target = run_dir.join(format!("{stream}.txt"));
        if fs::rename(&spill, &target).is_err() {
            let copied = fs::copy(&spill, &target).is_ok();
            let _ = fs::remove_file(&spill);
            if !copied {
                continue;
            }
        }
        let Ok((next, total_lines)) = preview_end(&target) else {
            continue;
        };
        data.insert(format!("{stream}_total_lines"), Value::from(total_lines));
        data.insert(format!("{stream}_next_offset"), Value::from(next.line));
        if next.byte > 0 {
            data.insert(format!("{stream}_next_byte_offset"), Value::from(next.byte));
        }
        notes.push(format!(
            "{stream} was cut at 40 KB; read the rest with {}",
            next.read_call(&id, stream)
        ));
    }
    if notes.is_empty() {
        let _ = fs::remove_dir(&run_dir);
        data.insert("output_saved".to_string(), Value::Bool(false));
        return output;
    }
    data.insert("output".to_string(), Value::String(id));
    data.insert("message".to_string(), Value::String(notes.join(". ")));
    output
}

/// A position in saved output: a 1-indexed line and a byte offset in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Position {
    line: usize,
    byte: usize,
}

impl Position {
    fn read_call(self, id: &str, stream: &str) -> String {
        let mut call =
            json!({"action": "read", "output": id, "stream": stream, "offset": self.line});
        if self.byte > 0 {
            call["byte_offset"] = Value::from(self.byte);
        }
        call.to_string()
    }
}

/// Where the run result's 40 KB preview of a saved stream ends, and the
/// stream's line count.
fn preview_end(path: &Path) -> std::io::Result<(Position, usize)> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(bash::MAX_OUTPUT_BYTES);
    fs::File::open(path)?
        .take(bash::MAX_OUTPUT_BYTES as u64 + 4)
        .read_to_end(&mut head)?;
    let cut = zdx_tools::utf8_cut_point(&head, bash::MAX_OUTPUT_BYTES);
    let shown = &head[..cut];
    let newlines = count_newlines(shown);
    let line_start = shown
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |pos| pos + 1);
    let next = Position {
        line: newlines + 1,
        byte: cut - line_start,
    };
    Ok((next, count_file_lines(path)?))
}

/// The length of `bytes` without a UTF-8 character cut off at its end.
/// Bytes that are not UTF-8 are kept as they are.
fn complete_utf8_prefix(bytes: &[u8]) -> usize {
    let len = bytes.len();
    let lead = (len.saturating_sub(4)..len)
        .rev()
        .find(|&pos| bytes[pos] & 0xC0 != 0x80);
    let Some(lead) = lead else {
        return len;
    };
    let width = match bytes[lead] {
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    };
    if lead + width > len { lead } else { len }
}

fn count_newlines(bytes: &[u8]) -> usize {
    bytes.split(|byte| *byte == b'\n').count() - 1
}

/// Counts lines without loading the file: newlines, plus a last line that
/// has none.
fn count_file_lines(path: &Path) -> std::io::Result<usize> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut lines = 0;
    let mut last = b'\n';
    loop {
        let chunk = reader.fill_buf()?;
        let Some(&end) = chunk.last() else { break };
        lines += count_newlines(chunk);
        last = end;
        let len = chunk.len();
        reader.consume(len);
    }
    Ok(lines + usize::from(last != b'\n'))
}

/// One page of saved output.
struct Page {
    content: Vec<u8>,
    lines_shown: usize,
    /// Where the next page starts, when output remains.
    next: Option<Position>,
}

/// Reads up to `limit` lines from `start`, at most [`READ_PAGE_BYTES`] bytes,
/// without loading the file. A line longer than the page is cut (never inside
/// a UTF-8 character) and continues at `next.byte` of the same line.
fn read_page(path: &Path, start: Position, limit: usize) -> std::io::Result<Page> {
    let mut reader = BufReader::new(fs::File::open(path)?);
    let mut line = 1;
    while line < start.line {
        if reader.skip_until(b'\n')? == 0 {
            return Ok(Page {
                content: Vec::new(),
                lines_shown: 0,
                next: None,
            });
        }
        line += 1;
    }

    let mut skip = start.byte;
    while skip > 0 {
        let chunk = reader.fill_buf()?;
        let len = chunk
            .iter()
            .take(skip)
            .position(|byte| *byte == b'\n')
            .unwrap_or_else(|| chunk.len().min(skip));
        if len == 0 {
            break;
        }
        reader.consume(len);
        skip -= len;
    }

    let first_byte = start.byte - skip;
    let mut content = Vec::new();
    let mut line = start.line;
    let mut byte = first_byte;
    let mut full_lines = 0;
    let mut at_eof = false;
    while full_lines < limit {
        let room = READ_PAGE_BYTES - content.len();
        if room == 0 {
            break;
        }
        let read = (&mut reader)
            .take(room as u64)
            .read_until(b'\n', &mut content)?;
        if read == 0 {
            at_eof = true;
            break;
        }
        if content.last() == Some(&b'\n') {
            full_lines += 1;
            line += 1;
            byte = 0;
            continue;
        }
        if read < room {
            // The last line has no newline and the file ends here.
            byte += read;
            at_eof = true;
            break;
        }
        // The page is full inside a line: keep whole characters only and
        // leave the rest of a cut character for the next page. Earlier reads
        // ended at a newline, so the cut is inside this read.
        let keep = complete_utf8_prefix(&content);
        let dropped = content.len() - keep;
        content.truncate(keep);
        reader.seek_relative(-i64::try_from(dropped).unwrap_or(0))?;
        byte += read - dropped;
        break;
    }
    let more = !at_eof && !reader.fill_buf()?.is_empty();
    let partial_line = byte > if line == start.line { first_byte } else { 0 };
    Ok(Page {
        content,
        lines_shown: full_lines + usize::from(partial_line),
        next: more.then_some(Position { line, byte }),
    })
}

fn read_saved_output(ctx: &ToolContext, object: &Map<String, Value>) -> ToolOutput {
    let id = match required_string(object, "output") {
        Ok(id) => id,
        Err(output) => return output,
    };
    if !is_output_id(&id) {
        return invalid_input(format!(
            "`output` must be an id from an earlier run, like \"run-3f9a2c1b\" (got \"{id}\")."
        ));
    }
    let stream = match object.get("stream") {
        None | Some(Value::Null) => "stdout",
        Some(Value::String(stream)) if stream == "stdout" || stream == "stderr" => stream.as_str(),
        Some(_) => return invalid_input("`stream` must be \"stdout\" or \"stderr\"."),
    };
    let Some(store) = ctx.current_thread_id.as_deref().and_then(output_store) else {
        return ToolOutput::failure(
            "output_not_found",
            "This run has no saved thread, so it has no saved outputs.",
            None,
        );
    };
    let path = store.join(&id).join(format!("{stream}.txt"));
    if !path.is_file() {
        return ToolOutput::failure(
            "output_not_found",
            format!("No saved {stream} for {id} in this thread."),
            Some(
                "Saved outputs belong to the thread whose run created them and are deleted after 7 days. Only a stream that was cut is saved.".to_string(),
            ),
        );
    }

    let number = |key: &str| {
        whole_number(object, key)
            .map(|value| value.map(|n| usize::try_from(n).unwrap_or(usize::MAX)))
    };
    let (offset, limit, byte_offset) =
        match (number("offset"), number("limit"), number("byte_offset")) {
            (Ok(offset), Ok(limit), Ok(byte)) => (
                offset.unwrap_or(1).max(1),
                limit.unwrap_or(READ_PAGE_LINES).clamp(1, READ_PAGE_LINES),
                byte.unwrap_or(0),
            ),
            (Err(output), _, _) | (_, Err(output), _) | (_, _, Err(output)) => return output,
        };
    let start = Position {
        line: offset,
        byte: byte_offset,
    };
    let read = read_page(&path, start, limit)
        .and_then(|page| count_file_lines(&path).map(|total| (page, total)));
    let (page, total_lines) = match read {
        Ok(read) => read,
        Err(err) => {
            return ToolOutput::failure(
                "read_error",
                format!("Cannot read saved {stream} of {id}"),
                Some(err.to_string()),
            );
        }
    };
    let mut data = json!({
        "output": id,
        "stream": stream,
        "content": String::from_utf8_lossy(&page.content),
        "offset": offset,
        "lines_shown": page.lines_shown,
        "total_lines": total_lines,
        "truncated": page.next.is_some(),
    });
    if byte_offset > 0 {
        data["byte_offset"] = Value::from(byte_offset);
    }
    if let Some(next) = page.next {
        data["next_offset"] = Value::from(next.line);
        if next.byte > 0 {
            data["next_byte_offset"] = Value::from(next.byte);
        }
        data["message"] = Value::String(format!(
            "More output remains; read it with {}",
            next.read_call(&id, stream)
        ));
    }
    ToolOutput::success(data)
}

/// An output id: `run-` and 8 lowercase hex digits.
fn is_output_id(id: &str) -> bool {
    id.strip_prefix("run-").is_some_and(|hex| {
        hex.len() == 8 && hex.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The current thread's output folder: `$ZDX_HOME/run/skill-exec/<thread>/`.
/// `None` for an id that is not a plain thread id (the rule the orchestrator
/// applies to thread ids).
fn output_store(thread_id: &str) -> Option<PathBuf> {
    let valid = !thread_id.is_empty()
        && thread_id.len() <= 200
        && !thread_id.starts_with('.')
        && thread_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    valid.then(|| {
        paths::zdx_home()
            .join("run")
            .join("skill-exec")
            .join(thread_id)
    })
}

/// Creates a new `run-<8 hex>` folder in `store`. Ids are random, so an id
/// from a deleted run is not given to a new one.
fn allocate_run_dir(store: &Path) -> std::io::Result<(String, PathBuf)> {
    loop {
        fs::create_dir_all(store)?;
        let id = format!("run-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        let dir = store.join(&id);
        match fs::create_dir(&dir) {
            Ok(()) => return Ok((id, dir)),
            // Taken, or another save pruned the empty thread folder in between.
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::NotFound
                ) => {}
            Err(err) => return Err(err),
        }
    }
}

/// Deletes saved outputs older than [`OUTPUT_RETENTION`] in every thread's
/// folder under `root`, and thread folders left empty.
fn prune_old_outputs(root: &Path) {
    let Ok(threads) = fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for thread in threads.flatten() {
        let thread_dir = thread.path();
        let Ok(runs) = fs::read_dir(&thread_dir) else {
            continue;
        };
        for run in runs.flatten() {
            let expired = run
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > OUTPUT_RETENTION);
            if expired {
                let _ = fs::remove_dir_all(run.path());
            }
        }
        let _ = fs::remove_dir(&thread_dir);
    }
}

// -- Shared --

fn find_skill(ctx: &ToolContext, name: &str) -> Result<Skill, ToolOutput> {
    let config = ctx.config.clone().unwrap_or_default();
    let skills = skills_in_scope(&config, &ctx.root, &ctx.skill_scope);
    if let Some(skill) = skills.iter().find(|skill| skill.name == name) {
        return Ok(skill.clone());
    }
    let mut names: Vec<&str> = skills.iter().map(|skill| skill.name.as_str()).collect();
    names.sort_unstable();
    let details = if names.is_empty() {
        "No skills are available in this run.".to_string()
    } else {
        let more = names.len().saturating_sub(MAX_LISTED_SKILLS);
        names.truncate(MAX_LISTED_SKILLS);
        let mut list = format!("Available skills: {}", names.join(", "));
        if more > 0 {
            let _ = std::fmt::Write::write_fmt(&mut list, format_args!(" (and {more} more)"));
        }
        list
    };
    let scope = match &ctx.skill_scope {
        SkillScope::Discovered => "",
        SkillScope::Named(_) => " This run may use only the skills its profile declares.",
    };
    Err(ToolOutput::failure_with_details(
        "skill_not_available",
        format!("Skill '{name}' is not available in this run.{scope}"),
        details,
    ))
}

fn canonical_dir(dir: &Path) -> Result<PathBuf, ToolOutput> {
    dir.canonicalize().map_err(|err| {
        ToolOutput::failure(
            "read_error",
            format!("Cannot open skill folder {}", dir.display()),
            Some(err.to_string()),
        )
    })
}

/// Resolves `file` (relative to the skill folder `base`) to a regular file
/// whose real path, after following every link, is inside `within`.
fn resolve_inside(
    base: &Path,
    within: &Path,
    file: &str,
    skill: &str,
) -> Result<PathBuf, ToolOutput> {
    let file = file.trim();
    if file.is_empty() {
        return Err(invalid_input("`file` cannot be empty."));
    }
    let requested = Path::new(file);
    if requested.is_absolute() || file.starts_with('~') || file.starts_with('$') {
        return Err(invalid_input(format!(
            "`file` must be a path relative to the skill folder, like `scripts/run.py` (got `{file}`)."
        )));
    }
    let outside = || {
        let place = if within == base {
            "the skill folder".to_string()
        } else {
            format!("the skill's `{SCRIPTS_DIR}/` folder")
        };
        ToolOutput::failure(
            "path_not_allowed",
            format!("`{file}` is outside {place} of '{skill}'."),
            None,
        )
    };
    let resolved = match base.join(requested).canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(ToolOutput::failure(
                "not_found",
                format!("Skill '{skill}' has no file `{file}`."),
                None,
            ));
        }
        Err(err) => {
            return Err(ToolOutput::failure(
                "read_error",
                format!("Cannot open `{file}` in skill '{skill}'"),
                Some(err.to_string()),
            ));
        }
    };
    if !resolved.starts_with(within) {
        return Err(outside());
    }
    if !resolved.is_file() {
        return Err(ToolOutput::failure(
            "not_a_file",
            format!("`{file}` in skill '{skill}' is not a file."),
            None,
        ));
    }
    Ok(resolved)
}

fn paging_fields(object: &Map<String, Value>) -> Map<String, Value> {
    ["offset", "limit"]
        .into_iter()
        .filter_map(|key| {
            object
                .get(key)
                .filter(|value| !value.is_null())
                .map(|value| (key.to_string(), value.clone()))
        })
        .collect()
}

/// Reads `args`. Besides a JSON array of strings, it takes the same array
/// sent as one string (`"[\"comment\"]"`), which models sometimes produce;
/// the `grep` and `web_search` list fields accept that form too.
fn parse_args(value: Option<&Value>) -> Result<Vec<String>, ToolOutput> {
    let items = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items.clone(),
        Some(Value::String(text)) => match serde_json::from_str::<Value>(text.trim()) {
            Ok(Value::Array(items)) => items,
            _ => {
                return Err(invalid_input(format!(
                    "`args` must be an array of strings, one item per argument, like [\"comment\", \"--label\", \"two words\"]. It was the string {}.",
                    Value::String(text.clone())
                )));
            }
        },
        Some(other) => {
            return Err(invalid_input(format!(
                "`args` must be an array of strings, like [\"comment\"]. It was {other}."
            )));
        }
    };
    items
        .into_iter()
        .map(|item| match item {
            Value::String(text) => Ok(text),
            other => Err(invalid_input(format!(
                "Every item in `args` must be a string; one was {other}. Quote numbers too, like [\"--count\", \"3\"]."
            ))),
        })
        .collect()
}

/// Reads an optional whole number that may also come as a numeric string
/// (`"30"`), as the other tools accept.
fn whole_number(object: &Map<String, Value>, key: &str) -> Result<Option<u64>, ToolOutput> {
    let parsed = match object.get(key) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.trim().parse::<u64>().ok(),
        Some(_) => None,
    };
    parsed
        .map(Some)
        .ok_or_else(|| invalid_input(format!("`{key}` must be a whole number, 0 or more.")))
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, ToolOutput> {
    match object.get(key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Ok(value.trim().to_string()),
        Some(Value::String(_)) => Err(invalid_input(format!("`{key}` cannot be empty."))),
        Some(_) => Err(invalid_input(format!("`{key}` must be a string."))),
        None => Err(invalid_input(format!("`{key}` is required."))),
    }
}

fn optional_string(object: &Map<String, Value>, key: &str) -> Result<Option<String>, ToolOutput> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(invalid_input(format!("`{key}` must be a string."))),
    }
}

fn invalid_input(message: impl Into<String>) -> ToolOutput {
    ToolOutput::failure("invalid_input", message, None)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    use super::*;
    use crate::config::{Config, SkillSourceToggles};

    struct Fixture {
        _home: crate::test_support::TestZdxHomeGuard,
        root: tempfile::TempDir,
        config: Config,
    }

    impl Fixture {
        fn new() -> Self {
            let home = crate::test_support::temp_zdx_home();
            let root = tempfile::tempdir().unwrap();
            let mut config = Config::default();
            config.skills.sources = SkillSourceToggles {
                zdx_project: true,
                ..Default::default()
            };
            Self {
                _home: home,
                root,
                config,
            }
        }

        fn skill_dir(&self, name: &str) -> PathBuf {
            self.root.path().join(".zdx").join("skills").join(name)
        }

        fn add_skill(&self, name: &str) -> PathBuf {
            let dir = self.skill_dir(name);
            fs::create_dir_all(dir.join("references")).unwrap();
            fs::write(
                dir.join("SKILL.md"),
                format!("---\nname: {name}\ndescription: Demo skill.\n---\n\n# {name} body\n"),
            )
            .unwrap();
            fs::write(
                dir.join("references").join("guide.md"),
                "line one\nline two\n",
            )
            .unwrap();
            fs::write(dir.join(".hidden"), "secret").unwrap();
            dir
        }

        fn ctx(&self, thread: Option<&str>) -> ToolContext {
            ToolContext::new(self.root.path().to_path_buf(), None)
                .with_config(&self.config)
                .with_current_thread_id(thread)
        }
    }

    #[cfg(unix)]
    fn write_script(dir: &Path, name: &str, body: &str, executable: bool) {
        use std::os::unix::fs::PermissionsExt;
        let scripts = dir.join("scripts");
        fs::create_dir_all(&scripts).unwrap();
        let path = scripts.join(name);
        fs::write(&path, body).unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }

    fn error_code(output: &ToolOutput) -> String {
        output
            .error_info()
            .map(|(code, _, _)| code.to_string())
            .unwrap_or_default()
    }

    fn error_message(output: &ToolOutput) -> String {
        output
            .error_info()
            .map(|(_, message, _)| message.to_string())
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn load_returns_instructions_folder_and_files() {
        let fx = Fixture::new();
        fx.add_skill("demo");

        let output = execute_skill(&json!({"name": "demo"}), &fx.ctx(None)).await;
        let data = output.data().expect("load succeeds");
        assert!(data["content"].as_str().unwrap().contains("# demo body"));
        assert_eq!(data["file"], "SKILL.md");
        assert_eq!(data["files"], json!(["references/guide.md"]));
        assert_eq!(data["files_total"], 1);
        assert!(data.get("file_path").is_none());
        let directory = data["directory"].as_str().unwrap();
        assert_eq!(
            Path::new(directory),
            fx.skill_dir("demo").canonicalize().unwrap()
        );
    }

    #[tokio::test]
    async fn read_pages_a_file_inside_the_skill() {
        let fx = Fixture::new();
        fx.add_skill("demo");

        let output = execute_skill(
            &json!({"name": "demo", "file": "references/guide.md", "offset": 2, "limit": 1}),
            &fx.ctx(None),
        )
        .await;
        let data = output.data().expect("read succeeds");
        assert_eq!(data["content"], "line two\n");
        assert_eq!(data["file"], "references/guide.md");
        assert!(data.get("files").is_none());

        let first = execute_skill(
            &json!({"name": "demo", "file": "references/guide.md", "limit": 1}),
            &fx.ctx(None),
        )
        .await;
        let warning = first.data().unwrap()["warning"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(warning, "Showing lines 1-1 of 2. Continue with offset=2.");
    }

    #[tokio::test]
    async fn read_returns_images_as_images() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        let image = image::RgbImage::from_pixel(2, 2, image::Rgb([10, 20, 30]));
        image.save(dir.join("assets.png")).unwrap();

        let output = execute_skill(
            &json!({"name": "demo", "file": "assets.png"}),
            &fx.ctx(None),
        )
        .await;
        let ToolOutput::Success { image, .. } = output else {
            panic!("image read failed: {output:?}");
        };
        assert!(image.is_some());
    }

    #[tokio::test]
    async fn files_outside_the_skill_folder_are_refused() {
        let fx = Fixture::new();
        fx.add_skill("demo");
        fs::write(fx.root.path().join("outside.txt"), "nope").unwrap();

        let escape = execute_skill(
            &json!({"name": "demo", "file": "../../../outside.txt"}),
            &fx.ctx(None),
        )
        .await;
        assert_eq!(error_code(&escape), "path_not_allowed");

        let absolute = execute_skill(
            &json!({"name": "demo", "file": fx.root.path().join("outside.txt")}),
            &fx.ctx(None),
        )
        .await;
        assert_eq!(error_code(&absolute), "invalid_input");

        #[cfg(unix)]
        {
            symlink(
                fx.root.path().join("outside.txt"),
                fx.skill_dir("demo").join("link.txt"),
            )
            .unwrap();
            let linked =
                execute_skill(&json!({"name": "demo", "file": "link.txt"}), &fx.ctx(None)).await;
            assert_eq!(error_code(&linked), "path_not_allowed");
        }
    }

    #[tokio::test]
    async fn only_skills_in_scope_are_reachable() {
        let fx = Fixture::new();
        fx.add_skill("demo");
        fx.add_skill("other");

        let named = fx
            .ctx(None)
            .with_skill_scope(SkillScope::Named(vec!["other".to_string()]));
        let refused = execute_skill(&json!({"name": "demo"}), &named).await;
        assert_eq!(error_code(&refused), "skill_not_available");
        assert!(
            execute_skill(&json!({"name": "other"}), &named)
                .await
                .is_ok()
        );

        let none = fx.ctx(None).with_skill_scope(SkillScope::Named(Vec::new()));
        let refused = execute_skill(&json!({"name": "other"}), &none).await;
        assert_eq!(error_code(&refused), "skill_not_available");

        let mut disabled = fx.config.clone();
        disabled.skills.enabled = false;
        let ctx = ToolContext::new(fx.root.path().to_path_buf(), None).with_config(&disabled);
        let refused = execute_skill(&json!({"name": "demo"}), &ctx).await;
        assert_eq!(error_code(&refused), "skill_not_available");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_passes_args_and_stdin_without_a_shell() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        write_script(
            &dir,
            "echo.sh",
            "#!/bin/sh\nfor arg in \"$@\"; do printf '[%s]\\n' \"$arg\"; done\nprintf 'skill=%s\\n' \"$ZDX_SKILL_NAME\"\ncat\n",
            true,
        );
        let marker = fx.root.path().join("injected");

        let output = execute_skill_exec(
            &json!({
                "action": "run",
                "name": "demo",
                "file": "scripts/echo.sh",
                "args": [
                    "two words",
                    "it's",
                    format!("; touch {}", marker.display()),
                    "$(id)",
                    "*",
                    "$ZDX_SKILL_NAME",
                    "$HOME"
                ],
                "stdin": "from stdin\n"
            }),
            &fx.ctx(Some("thread-a")),
        )
        .await;
        let data = output.data().expect("run succeeds");
        assert_eq!(data["exit_code"], 0);
        let stdout = data["stdout"].as_str().unwrap();
        assert_eq!(
            stdout,
            format!(
                "[two words]\n[it's]\n[; touch {}]\n[$(id)]\n[*]\n[demo]\n[$HOME]\nskill=demo\nfrom stdin\n",
                marker.display()
            )
        );
        assert!(!marker.exists(), "an argument must never run as a command");
        assert_eq!(data["skill"], "demo");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_uses_the_shebang_of_a_non_executable_script() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        write_script(&dir, "hello.sh", "#!/bin/sh\necho hello\n", false);
        write_script(&dir, "plain.txt", "echo nope\n", false);

        let ctx = fx.ctx(None);
        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/hello.sh"}),
            &ctx,
        )
        .await;
        assert_eq!(output.data().unwrap()["stdout"], "hello\n");

        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/plain.txt"}),
            &ctx,
        )
        .await;
        assert_eq!(error_code(&output), "not_runnable");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_only_starts_files_inside_scripts() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        write_script(&dir, "ok.sh", "#!/bin/sh\necho ok\n", true);
        fs::write(
            dir.join("references").join("tool.sh"),
            "#!/bin/sh\necho no\n",
        )
        .unwrap();
        symlink("/bin/sh", dir.join("scripts").join("shell")).unwrap();

        let ctx = fx.ctx(None);
        for file in [
            "references/tool.sh",
            "SKILL.md",
            "scripts/shell",
            "scripts/../SKILL.md",
        ] {
            let output = execute_skill_exec(
                &json!({"action": "run", "name": "demo", "file": file}),
                &ctx,
            )
            .await;
            assert_eq!(error_code(&output), "path_not_allowed", "{file}");
        }

        let refused = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/ok.sh"}),
            &ctx.clone()
                .with_skill_scope(SkillScope::Named(vec!["other".to_string()])),
        )
        .await;
        assert_eq!(error_code(&refused), "skill_not_available");
    }

    #[tokio::test]
    async fn run_and_read_parameters_do_not_mix() {
        let fx = Fixture::new();
        let ctx = fx.ctx(Some("thread-a"));

        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/x", "output": "run-1"}),
            &ctx,
        )
        .await;
        assert_eq!(error_code(&output), "invalid_input");
        assert!(error_message(&output).contains("belongs to action \"read\""));

        let output = execute_skill_exec(
            &json!({"action": "read", "output": "run-1", "args": []}),
            &ctx,
        )
        .await;
        assert!(error_message(&output).contains("belongs to action \"run\""));

        let output = execute_skill_exec(&json!({"name": "demo"}), &ctx).await;
        assert!(error_message(&output).contains("`action` is required"));

        let output = execute_skill_exec(&json!({"action": "read", "output": "../x"}), &ctx).await;
        assert_eq!(error_code(&output), "invalid_input");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn large_output_is_saved_for_the_thread_and_paged() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        write_script(
            &dir,
            "big.sh",
            "#!/bin/sh\ni=1\nwhile [ $i -le 3000 ]; do echo \"line $i padded to make the output larger than forty kilobytes\"; i=$((i+1)); done\n",
            true,
        );

        let owner = fx.ctx(Some("thread-a"));
        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/big.sh"}),
            &owner,
        )
        .await;
        let data = output.data().expect("run succeeds").clone();
        assert_eq!(data["stdout_truncated"], true);
        let id = data["output"].as_str().unwrap().to_string();
        assert!(is_output_id(&id), "{id}");
        assert_eq!(data["stdout_total_lines"], 3000);
        assert!(data.get("stdout_file").is_none());
        let next = data["stdout_next_offset"].as_u64().unwrap();
        assert!(data["message"].as_str().unwrap().contains(&id));

        let page = execute_skill_exec(
            &json!({"action": "read", "output": id, "offset": next, "limit": 2}),
            &owner,
        )
        .await;
        let page = page.data().expect("read succeeds");
        assert!(
            page["content"]
                .as_str()
                .unwrap()
                .starts_with(&format!("line {next} padded"))
        );
        assert_eq!(page["output"], id.as_str());
        assert_eq!(page["stream"], "stdout");
        assert_eq!(page["lines_shown"], 2);
        assert_eq!(page["next_offset"], next + 2);
        assert!(page.get("file_path").is_none());

        let other_thread = fx.ctx(Some("thread-b"));
        let refused =
            execute_skill_exec(&json!({"action": "read", "output": id}), &other_thread).await;
        assert_eq!(error_code(&refused), "output_not_found");

        let again = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/big.sh"}),
            &owner,
        )
        .await;
        assert_ne!(again.data().unwrap()["output"], id.as_str());

        // A thread id with dots is a valid thread id and keeps its output.
        let dotted = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/big.sh"}),
            &fx.ctx(Some("thread.with.dots")),
        )
        .await;
        assert!(dotted.data().unwrap()["output"].is_string());

        let no_thread = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/big.sh"}),
            &fx.ctx(None),
        )
        .await;
        let data = no_thread.data().unwrap();
        assert_eq!(data["output_saved"], false);
        assert!(data.get("output").is_none());
        assert!(data.get("stdout_file").is_none());
    }

    /// Models sometimes send `args` as a string that holds the JSON array, and
    /// numbers as strings. Both forms run; any other string is refused with an
    /// error that shows the expected shape.
    #[cfg(unix)]
    #[tokio::test]
    async fn args_sent_as_a_json_string_still_run() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        write_script(
            &dir,
            "echo.sh",
            "#!/bin/sh\nfor arg in \"$@\"; do printf '[%s]\\n' \"$arg\"; done\n",
            true,
        );
        let ctx = fx.ctx(None);

        let output = execute_skill_exec(
            &json!({
                "action": "run", "name": "demo", "file": "scripts/echo.sh",
                "args": "[\"comment\", \"two words\"]", "timeout_secs": "30"
            }),
            &ctx,
        )
        .await;
        assert_eq!(output.data().unwrap()["stdout"], "[comment]\n[two words]\n");

        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/echo.sh", "args": "comment --help"}),
            &ctx,
        )
        .await;
        assert_eq!(error_code(&output), "invalid_input");
        let message = error_message(&output);
        assert!(message.contains("like [\"comment\""), "{message}");
        assert!(message.contains("\"comment --help\""), "{message}");

        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/echo.sh", "args": ["--count", 3]}),
            &ctx,
        )
        .await;
        assert!(error_message(&output).contains("Quote numbers too"));
    }

    #[test]
    fn expands_only_zdx_variables() {
        let lookup = |name: &str| match name {
            "ZDX_ARTIFACT_DIR" => Some("/art".to_string()),
            _ => None,
        };
        assert_eq!(
            expand_zdx_vars(
                "$ZDX_ARTIFACT_DIR/a.png ${ZDX_ARTIFACT_DIR}x $HOME $ZDX_NONE ${ZDX_ARTIFACT_DIR $",
                lookup
            ),
            "/art/a.png /artx $HOME $ZDX_NONE ${ZDX_ARTIFACT_DIR $"
        );
    }

    /// A line far longer than one page is read in full, page by page, with
    /// the byte continuation the results give.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_line_longer_than_a_page_is_read_in_full() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        write_script(
            &dir,
            "wide.sh",
            "#!/bin/sh\nhead -c 100000 /dev/zero | tr '\\0' 'x'\necho\necho tail-line\n",
            true,
        );
        let ctx = fx.ctx(Some("thread-a"));
        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/wide.sh"}),
            &ctx,
        )
        .await;
        let data = output.data().unwrap().clone();
        let id = data["output"].as_str().unwrap().to_string();
        assert_eq!(data["stdout_total_lines"], 2);
        assert_eq!(data["stdout_next_offset"], 1);
        let mut byte = data["stdout_next_byte_offset"].as_u64().unwrap();
        let mut text = data["stdout"].as_str().unwrap().to_string();
        let mut offset = 1;
        loop {
            let page = execute_skill_exec(
                &json!({"action": "read", "output": id, "offset": offset, "byte_offset": byte}),
                &ctx,
            )
            .await;
            let page = page.data().expect("page").clone();
            text.push_str(page["content"].as_str().unwrap());
            if page["truncated"] == false {
                break;
            }
            offset = page["next_offset"].as_u64().unwrap();
            byte = page["next_byte_offset"].as_u64().unwrap_or(0);
        }
        assert_eq!(text, format!("{}\ntail-line\n", "x".repeat(100_000)));
    }

    #[test]
    fn pages_never_split_a_character() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.txt");
        let line = "é".repeat(READ_PAGE_BYTES * 3 / 4);
        fs::write(&path, format!("{line}\n")).unwrap();

        let page = read_page(&path, Position { line: 1, byte: 0 }, 10).unwrap();
        assert_eq!(page.content.len(), READ_PAGE_BYTES);
        let next = page.next.unwrap();
        assert_eq!(
            next,
            Position {
                line: 1,
                byte: READ_PAGE_BYTES
            }
        );
        let rest = read_page(&path, next, 10).unwrap();
        assert!(std::str::from_utf8(&rest.content).is_ok());
        assert!(rest.next.is_none());

        let odd = "a".to_string() + &line;
        fs::write(&path, &odd).unwrap();
        let page = read_page(&path, Position { line: 1, byte: 0 }, 10).unwrap();
        assert!(std::str::from_utf8(&page.content).is_ok());
        assert_eq!(page.content.len(), READ_PAGE_BYTES - 1);
        assert_eq!(count_file_lines(&path).unwrap(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_scripts_folder_that_links_outside_the_skill_is_refused() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        symlink("/bin", dir.join("scripts")).unwrap();

        let output = execute_skill_exec(
            &json!({"action": "run", "name": "demo", "file": "scripts/sh", "args": ["-c", "echo ran"]}),
            &fx.ctx(None),
        )
        .await;
        assert_eq!(error_code(&output), "path_not_allowed");
    }

    #[test]
    fn shebang_words_follow_the_host_kernel() {
        let words = shebang_words("/usr/bin/env -S uv run --script");
        if cfg!(target_os = "linux") {
            assert_eq!(words, ["/usr/bin/env", "-S uv run --script"]);
        } else {
            assert_eq!(words, ["/usr/bin/env", "-S", "uv", "run", "--script"]);
        }
        assert_eq!(shebang_words("/bin/sh"), ["/bin/sh"]);
    }

    /// A non-executable script starts the way it would with the executable
    /// bit on this host.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_shebang_with_arguments_matches_the_executable_start() {
        let fx = Fixture::new();
        let dir = fx.add_skill("demo");
        let body = "#!/bin/sh -e\necho ok\n";
        write_script(&dir, "plain.sh", body, false);
        write_script(&dir, "exec.sh", body, true);
        let ctx = fx.ctx(None);
        for file in ["scripts/plain.sh", "scripts/exec.sh"] {
            let output = execute_skill_exec(
                &json!({"action": "run", "name": "demo", "file": file}),
                &ctx,
            )
            .await;
            assert_eq!(output.data().unwrap()["stdout"], "ok\n", "{file}");
        }
    }

    #[test]
    fn output_ids_are_random_hex() {
        assert!(is_output_id("run-3f9a2c1b"));
        for id in [
            "run-",
            "run-1",
            "run-3F9A2C1B",
            "../run-3f9a2c1b",
            "run-3f9a2c1b/../x",
            "run-3f9a2c1bb",
        ] {
            assert!(!is_output_id(id), "{id}");
        }
        let store = tempfile::tempdir().unwrap();
        let (first, _) = allocate_run_dir(store.path()).unwrap();
        let (second, _) = allocate_run_dir(store.path()).unwrap();
        assert!(is_output_id(&first) && is_output_id(&second));
        assert_ne!(first, second);
    }
}
