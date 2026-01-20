use std::io;
use std::path::{Path, PathBuf};

const INCLUDE_DIRS: &[&str] = &[
    "docs",
    "crates/zdx-cli/src",
    "crates/zdx-core",
    "crates/zdx-tui",
    "crates/xtask",
];
const EXCLUDE_EXTENSIONS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".gif", ".ico", ".svg", ".woff", ".woff2", ".ttf", ".eot", ".pdf",
    ".zip", ".tar", ".gz", ".exe", ".bin",
];
const EXCLUDE_FILES: &[&str] = &["codebase.txt"];

/// Generate codebase.txt for the entire workspace or specific directories
pub fn run(dirs: Option<Vec<String>>) -> anyhow::Result<()> {
    let current_dir = std::env::current_dir()?;
    let output_file = current_dir.join("codebase.txt");

    println!("Collecting files...");

    // Determine which directories to include
    let dirs_to_include = if let Some(dirs) = dirs {
        if dirs.is_empty() {
            // If empty vector provided, use default include dirs
            INCLUDE_DIRS.iter().map(|&s| s.to_string()).collect()
        } else {
            dirs
        }
    } else {
        // No parameter provided, use default include dirs
        INCLUDE_DIRS.iter().map(|&s| s.to_string()).collect()
    };

    let files = collect_files(&current_dir, &dirs_to_include)?;

    if files.is_empty() {
        anyhow::bail!("No files found to include!");
    }

    println!("Found {} files", files.len());
    println!("Generating {}...", output_file.display());

    let mut output = String::new();

    // Header
    output.push_str("CODEBASE CONTENTS\n");
    output.push_str(&"=".repeat(80));
    output.push_str("\n\nDirectories included:\n");
    for dir in &dirs_to_include {
        output.push_str(&format!("  - {}\n", dir));
    }
    output.push_str(&format!("\nTotal files: {}\n", files.len()));
    output.push('\n');
    output.push_str(&"=".repeat(80));
    output.push_str("\n\n");

    // Add each file
    for file_path in files {
        println!("  Processing: {}", file_path.display());
        match format_file_content(&file_path) {
            Ok(content) => {
                output.push_str(&content);
            }
            Err(e) => {
                eprintln!("  ⚠️  Skipped {}: {}", file_path.display(), e);
            }
        }
    }

    // Write to file
    std::fs::write(&output_file, output)?;

    let file_size = std::fs::metadata(&output_file)?.len();
    println!(
        "\n✓ Generated {} ({:?} bytes)",
        output_file.display(),
        file_size
    );

    Ok(())
}

fn collect_files(root: &Path, dirs_to_include: &[String]) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();

    for dir_name in dirs_to_include {
        let dir_path = root.join(dir_name);
        if !dir_path.exists() {
            eprintln!("Warning: Directory '{}' not found, skipping...", dir_name);
            continue;
        }

        // Recursively walk through directory
        walk_dir(&dir_path, &mut files)?;
    }

    // Sort files for consistent output
    files.sort();
    Ok(files)
}

fn walk_dir(dir_path: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in std::fs::read_dir(dir_path)? {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            // Skip hidden directories
            if path
                .file_name()
                .map(|name| name.to_string_lossy().starts_with('.'))
                .unwrap_or(false)
            {
                continue;
            }
            walk_dir(&path, files)?;
        } else {
            // Check if should exclude
            if !should_exclude_file(&path) {
                files.push(path);
            }
        }
    }
    Ok(())
}

fn should_exclude_file(path: &Path) -> bool {
    // Check extension
    if let Some(ext) = path.extension() {
        let ext_str = format!(".{}", ext.to_string_lossy().to_lowercase());
        if EXCLUDE_EXTENSIONS.iter().any(|&e| e == ext_str) {
            return true;
        }
    }

    // Check filename
    if let Some(file_name) = path.file_name() {
        let name_str = file_name.to_string_lossy();
        if EXCLUDE_FILES.iter().any(|&e| e == name_str) {
            return true;
        }
    }

    false
}

fn format_file_content(file_path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(file_path)?;
    let size_bytes = content.len();

    let mut output = String::new();
    output.push_str(&"=".repeat(80));
    output.push('\n');
    output.push_str(&format!("FILE: {}\n", file_path.display()));
    output.push_str(&format!("SIZE: {} bytes\n", size_bytes));
    output.push_str(&"=".repeat(80));
    output.push_str("\n\n");
    output.push_str(&content);
    output.push_str("\n\n\n"); // Extra newline between files

    Ok(output)
}
