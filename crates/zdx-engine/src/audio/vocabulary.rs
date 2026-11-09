//! Custom transcription vocabulary from `$ZDX_HOME/vocabulary.txt`.
//!
//! One entry per line (a term, a name, or a short description of the audio);
//! blank lines and lines starting with `#` are skipped. Callers can add files
//! for one run; their entries come first. Entries that differ only in case
//! count once, and the first spelling wins. The file is read on every call, so
//! an edit applies to the next transcription without a restart.

use std::collections::HashSet;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::paths;

/// The global vocabulary file.
#[must_use]
pub fn global_path() -> PathBuf {
    paths::zdx_home().join("vocabulary.txt")
}

/// Loads the vocabulary: entries from `extra_files` first (in order), then
/// the global file. A missing global file means no global entries.
///
/// # Errors
/// Returns an error if a file cannot be read, or an extra file is missing.
pub fn load(extra_files: &[PathBuf]) -> Result<Vec<String>> {
    let mut lines = Vec::new();
    for path in extra_files {
        let text = fs::read_to_string(path)
            .with_context(|| format!("read vocabulary file {}", path.display()))?;
        lines.extend(parse(&text));
    }
    lines.extend(read_optional(&global_path())?);
    Ok(dedupe(lines))
}

fn read_optional(path: &Path) -> Result<Vec<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(parse(&text).collect()),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(Vec::new()),
        Err(err) => Err(err).with_context(|| format!("read vocabulary file {}", path.display())),
    }
}

fn parse(text: &str) -> impl Iterator<Item = String> + '_ {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
}

fn dedupe(lines: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    lines
        .into_iter()
        .filter(|line| seen.insert(line.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extra_files_come_first_and_first_spelling_wins() {
        let home = crate::test_support::temp_zdx_home();
        fs::write(
            home.path().join("vocabulary.txt"),
            "# global terms\nTermA\n\n  shared term  \n",
        )
        .unwrap();
        let extra = home.path().join("meeting.txt");
        fs::write(&extra, "Shared Term\nTermB\ntermb\n").unwrap();

        assert_eq!(
            load(std::slice::from_ref(&extra)).unwrap(),
            ["Shared Term", "TermB", "TermA"]
        );
        assert_eq!(load(&[]).unwrap(), ["TermA", "shared term"]);
    }

    #[test]
    fn missing_global_file_is_empty_and_missing_extra_file_fails() {
        let home = crate::test_support::temp_zdx_home();
        assert!(load(&[]).unwrap().is_empty());
        assert!(load(&[home.path().join("missing.txt")]).is_err());
    }
}
