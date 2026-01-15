use std::path::PathBuf;

use super::types::{Hunk, ParseError, UpdateFileChunk};

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const END_OF_FILE: &str = "*** End of File";
const ADD_FILE_PREFIX: &str = "*** Add File: ";
const DELETE_FILE_PREFIX: &str = "*** Delete File: ";
const UPDATE_FILE_PREFIX: &str = "*** Update File: ";
const MOVE_TO_PREFIX: &str = "*** Move to: ";

pub fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ParseError> {
    let lines: Vec<String> = patch
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect();

    if lines.first().map(String::as_str) != Some(BEGIN_PATCH) {
        return Err(ParseError::InvalidPatch(
            "Missing *** Begin Patch marker".to_string(),
        ));
    }

    let mut hunks = Vec::new();
    let mut idx = 1;

    while idx < lines.len() {
        let line = lines[idx].as_str();

        if line == END_PATCH {
            return Ok(hunks);
        }

        if line.is_empty() {
            idx += 1;
            continue;
        }

        if let Some(path) = line.strip_prefix(ADD_FILE_PREFIX) {
            let path = path.trim();
            if path.is_empty() {
                return Err(ParseError::InvalidHunk {
                    message: "Add File path cannot be empty".to_string(),
                    line_number: idx + 1,
                });
            }
            idx += 1;
            let mut contents = Vec::new();
            while idx < lines.len() {
                let line = lines[idx].as_str();
                if line == END_PATCH || line.starts_with("*** ") {
                    break;
                }
                if line == END_OF_FILE {
                    idx += 1;
                    break;
                }
                if let Some(rest) = line.strip_prefix('+') {
                    contents.push(rest.to_string());
                    idx += 1;
                    continue;
                }
                return Err(ParseError::InvalidHunk {
                    message: "Add File content lines must start with '+'".to_string(),
                    line_number: idx + 1,
                });
            }
            hunks.push(Hunk::AddFile {
                path: PathBuf::from(path),
                contents: contents.join("\n"),
            });
            continue;
        }

        if let Some(path) = line.strip_prefix(DELETE_FILE_PREFIX) {
            let path = path.trim();
            if path.is_empty() {
                return Err(ParseError::InvalidHunk {
                    message: "Delete File path cannot be empty".to_string(),
                    line_number: idx + 1,
                });
            }
            idx += 1;
            hunks.push(Hunk::DeleteFile {
                path: PathBuf::from(path),
            });
            continue;
        }

        if let Some(path) = line.strip_prefix(UPDATE_FILE_PREFIX) {
            let path = path.trim();
            if path.is_empty() {
                return Err(ParseError::InvalidHunk {
                    message: "Update File path cannot be empty".to_string(),
                    line_number: idx + 1,
                });
            }
            idx += 1;
            let mut move_path = None;
            if idx < lines.len()
                && let Some(target) = lines[idx].strip_prefix(MOVE_TO_PREFIX)
            {
                let target = target.trim();
                if target.is_empty() {
                    return Err(ParseError::InvalidHunk {
                        message: "Move to path cannot be empty".to_string(),
                        line_number: idx + 1,
                    });
                }
                move_path = Some(PathBuf::from(target));
                idx += 1;
            }

            let mut chunks = Vec::new();
            while idx < lines.len() {
                let line = lines[idx].as_str();
                if line == END_PATCH || line.starts_with("*** ") {
                    break;
                }
                if !line.starts_with("@@") {
                    return Err(ParseError::InvalidHunk {
                        message: "Update File chunks must start with @@".to_string(),
                        line_number: idx + 1,
                    });
                }

                let header = line.trim_start_matches("@@");
                let change_context = header
                    .strip_prefix(' ')
                    .map(|s| s.to_string())
                    .filter(|s| !s.is_empty());
                idx += 1;

                let mut old_lines = Vec::new();
                let mut new_lines = Vec::new();
                let mut is_end_of_file = false;

                while idx < lines.len() {
                    let line = lines[idx].as_str();
                    if line == END_OF_FILE {
                        is_end_of_file = true;
                        idx += 1;
                        break;
                    }
                    if line.starts_with("@@") || line.starts_with("*** ") {
                        break;
                    }
                    if let Some(rest) = line.strip_prefix('+') {
                        new_lines.push(rest.to_string());
                        idx += 1;
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix('-') {
                        old_lines.push(rest.to_string());
                        idx += 1;
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix(' ') {
                        old_lines.push(rest.to_string());
                        new_lines.push(rest.to_string());
                        idx += 1;
                        continue;
                    }
                    if line.is_empty() {
                        old_lines.push(String::new());
                        new_lines.push(String::new());
                        idx += 1;
                        continue;
                    }

                    return Err(ParseError::InvalidHunk {
                        message: "Invalid line prefix in Update File hunk".to_string(),
                        line_number: idx + 1,
                    });
                }

                chunks.push(UpdateFileChunk {
                    change_context,
                    old_lines,
                    new_lines,
                    is_end_of_file,
                });
            }

            if chunks.is_empty() {
                return Err(ParseError::InvalidHunk {
                    message: "Update File section requires at least one @@ chunk".to_string(),
                    line_number: idx + 1,
                });
            }

            hunks.push(Hunk::UpdateFile {
                path: PathBuf::from(path),
                move_path,
                chunks,
            });
            continue;
        }

        return Err(ParseError::InvalidHunk {
            message: format!("Unexpected line: {}", line),
            line_number: idx + 1,
        });
    }

    Err(ParseError::InvalidPatch(
        "Missing *** End Patch marker".to_string(),
    ))
}
