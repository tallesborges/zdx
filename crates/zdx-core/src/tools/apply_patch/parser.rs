use std::path::PathBuf;

use super::types::{Hunk, ParseError, UpdateFileChunk};

const BEGIN_PATCH: &str = "*** Begin Patch";
const END_PATCH: &str = "*** End Patch";
const END_OF_FILE: &str = "*** End of File";
const ADD_FILE_PREFIX: &str = "*** Add File: ";
const DELETE_FILE_PREFIX: &str = "*** Delete File: ";
const UPDATE_FILE_PREFIX: &str = "*** Update File: ";
const MOVE_TO_PREFIX: &str = "*** Move to: ";

///
/// # Errors
/// Returns an error if the operation fails.
pub fn parse_patch(patch: &str) -> Result<Vec<Hunk>, ParseError> {
    PatchParser::new(patch).parse()
}

struct PatchParser {
    lines: Vec<String>,
    idx: usize,
    hunks: Vec<Hunk>,
}

impl PatchParser {
    fn new(patch: &str) -> Self {
        Self {
            lines: patch
                .lines()
                .map(|line| line.trim_end_matches('\r').to_string())
                .collect(),
            idx: 1,
            hunks: Vec::new(),
        }
    }

    fn parse(mut self) -> Result<Vec<Hunk>, ParseError> {
        if self.lines.first().map(String::as_str) != Some(BEGIN_PATCH) {
            return Err(ParseError::InvalidPatch(
                "Missing *** Begin Patch marker".to_string(),
            ));
        }

        while self.idx < self.lines.len() {
            let line = self.current_line().to_string();
            if line == END_PATCH {
                return Ok(self.hunks);
            }
            if line.is_empty() {
                self.idx += 1;
                continue;
            }

            if let Some(raw_path) = line.strip_prefix(ADD_FILE_PREFIX) {
                self.parse_add_file(raw_path)?;
                continue;
            }
            if let Some(raw_path) = line.strip_prefix(DELETE_FILE_PREFIX) {
                self.parse_delete_file(raw_path)?;
                continue;
            }
            if let Some(raw_path) = line.strip_prefix(UPDATE_FILE_PREFIX) {
                self.parse_update_file(raw_path)?;
                continue;
            }

            return Err(self.invalid_hunk(format!("Unexpected line: {line}")));
        }

        Err(ParseError::InvalidPatch(
            "Missing *** End Patch marker".to_string(),
        ))
    }

    fn parse_add_file(&mut self, raw_path: &str) -> Result<(), ParseError> {
        let file_path = required_path(raw_path)
            .ok_or_else(|| self.invalid_hunk("Add File path cannot be empty".to_string()))?;
        self.idx += 1;

        let mut contents = Vec::new();
        while self.idx < self.lines.len() {
            let line = self.current_line();
            if line == END_PATCH || line.starts_with("*** ") {
                break;
            }
            if line == END_OF_FILE {
                self.idx += 1;
                break;
            }
            if let Some(rest) = line.strip_prefix('+') {
                contents.push(rest.to_string());
                self.idx += 1;
                continue;
            }
            return Err(self.invalid_hunk("Add File content lines must start with '+'".to_string()));
        }

        self.hunks.push(Hunk::AddFile {
            path: PathBuf::from(file_path),
            contents: contents.join("\n"),
        });
        Ok(())
    }

    fn parse_delete_file(&mut self, raw_path: &str) -> Result<(), ParseError> {
        let file_path = required_path(raw_path)
            .ok_or_else(|| self.invalid_hunk("Delete File path cannot be empty".to_string()))?;
        self.idx += 1;
        self.hunks.push(Hunk::DeleteFile {
            path: PathBuf::from(file_path),
        });
        Ok(())
    }

    fn parse_update_file(&mut self, raw_path: &str) -> Result<(), ParseError> {
        let file_path = required_path(raw_path)
            .ok_or_else(|| self.invalid_hunk("Update File path cannot be empty".to_string()))?;
        self.idx += 1;

        let move_path = self.parse_move_to_path()?;
        let chunks = self.parse_update_chunks()?;
        if chunks.is_empty() {
            return Err(self
                .invalid_hunk("Update File section requires at least one @@ chunk".to_string()));
        }

        self.hunks.push(Hunk::UpdateFile {
            path: PathBuf::from(file_path),
            move_path,
            chunks,
        });
        Ok(())
    }

    fn parse_move_to_path(&mut self) -> Result<Option<PathBuf>, ParseError> {
        if self.idx >= self.lines.len() {
            return Ok(None);
        }

        let Some(target) = self.current_line().strip_prefix(MOVE_TO_PREFIX) else {
            return Ok(None);
        };

        let target = required_path(target)
            .ok_or_else(|| self.invalid_hunk("Move to path cannot be empty".to_string()))?;
        let target = target.to_string();
        self.idx += 1;
        Ok(Some(PathBuf::from(target)))
    }

    fn parse_update_chunks(&mut self) -> Result<Vec<UpdateFileChunk>, ParseError> {
        let mut chunks = Vec::new();

        while self.idx < self.lines.len() {
            let line = self.current_line();
            if line == END_PATCH || line.starts_with("*** ") {
                break;
            }
            if !line.starts_with("@@") {
                return Err(self.invalid_hunk("Update File chunks must start with @@".to_string()));
            }
            chunks.push(self.parse_update_chunk()?);
        }

        Ok(chunks)
    }

    fn parse_update_chunk(&mut self) -> Result<UpdateFileChunk, ParseError> {
        let header = self.current_line().trim_start_matches("@@");
        let change_context = header
            .strip_prefix(' ')
            .map(std::string::ToString::to_string)
            .filter(|s| !s.is_empty());
        self.idx += 1;

        let mut old_lines = Vec::new();
        let mut new_lines = Vec::new();
        let mut is_end_of_file = false;

        while self.idx < self.lines.len() {
            let line = self.current_line();
            if line == END_OF_FILE {
                is_end_of_file = true;
                self.idx += 1;
                break;
            }
            if line.starts_with("@@") || line.starts_with("*** ") {
                break;
            }
            if let Some(rest) = line.strip_prefix('+') {
                new_lines.push(rest.to_string());
                self.idx += 1;
                continue;
            }
            if let Some(rest) = line.strip_prefix('-') {
                old_lines.push(rest.to_string());
                self.idx += 1;
                continue;
            }
            if let Some(rest) = line.strip_prefix(' ') {
                old_lines.push(rest.to_string());
                new_lines.push(rest.to_string());
                self.idx += 1;
                continue;
            }
            if line.is_empty() {
                old_lines.push(String::new());
                new_lines.push(String::new());
                self.idx += 1;
                continue;
            }

            return Err(self.invalid_hunk("Invalid line prefix in Update File hunk".to_string()));
        }

        Ok(UpdateFileChunk {
            change_context,
            old_lines,
            new_lines,
            is_end_of_file,
        })
    }

    fn current_line(&self) -> &str {
        self.lines[self.idx].as_str()
    }

    fn invalid_hunk(&self, message: String) -> ParseError {
        ParseError::InvalidHunk {
            message,
            line_number: self.idx + 1,
        }
    }
}

fn required_path(raw_path: &str) -> Option<&str> {
    let file_path = raw_path.trim();
    (!file_path.is_empty()).then_some(file_path)
}
