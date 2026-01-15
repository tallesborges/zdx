use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hunk {
    AddFile {
        path: PathBuf,
        contents: String,
    },
    DeleteFile {
        path: PathBuf,
    },
    UpdateFile {
        path: PathBuf,
        move_path: Option<PathBuf>,
        chunks: Vec<UpdateFileChunk>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateFileChunk {
    pub change_context: Option<String>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
    pub is_end_of_file: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    InvalidPatch(String),
    InvalidHunk { message: String, line_number: usize },
}
