//! Central artifact model shared by every ZDX surface.
//!
//! An artifact is a file a thread produced (anything under its
//! `$ZDX_HOME/artifacts/threads/<id>/` directory) or a file the agent sent to
//! the user (an absolute `<media>` path in the thread transcript). The two
//! sources are merged on the canonical absolute path, so a file that was both
//! generated and sent appears once with `source: "both"` and the sequences of
//! the messages that referenced it. Files that were never sent still appear
//! (`source: "generated"`, no message link); sent files that are no longer on
//! disk still appear (`exists: false`) so the tab can explain the gap instead
//! of silently hiding them.
//!
//! Surfaces render from these types and never re-derive kinds, MIME types, or
//! media-tag parsing themselves.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Serialize;

use crate::config::paths;
use crate::core::thread_persistence::{ThreadEvent, load_thread_events};

/// Maximum files listed for one thread; the walk stops collecting after this.
const MAX_ARTIFACT_FILES: usize = 500;
/// Maximum directory depth below the thread artifact dir.
const MAX_WALK_DEPTH: usize = 6;

/// How an artifact is previewed. `other` covers everything else (PDF, text,
/// video, archives): the Mini App offers download, not a preview.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactKind {
    Image,
    Audio,
    Html,
    Other,
}

impl ArtifactKind {
    /// Preview kind for a download. HTML counts as its own kind so the client
    /// can route it to the sandboxed full-screen preview.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Html => "html",
            Self::Other => "other",
        }
    }
}

/// Where an artifact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ArtifactSource {
    Generated,
    Sent,
    Both,
}

impl ArtifactSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Sent => "sent",
            Self::Both => "both",
        }
    }
}

/// Inline attachment on a transcript message: the subset of [`Artifact`] the
/// conversation needs without a second request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactAttachment {
    pub name: String,
    /// Absolute path, passed back to the download endpoint as `?path=`.
    pub path: String,
    pub mime: String,
    pub kind: ArtifactKind,
    pub size_bytes: u64,
    pub exists: bool,
}

/// One row of the thread Artifacts tab.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Artifact {
    pub name: String,
    /// Absolute path, passed back to the download endpoint as `?path=`.
    pub path: String,
    /// Path relative to the thread artifact dir using `/` separators, when the
    /// file lives inside it. `None` for sent files stored elsewhere.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rel: Option<String>,
    pub mime: String,
    pub kind: ArtifactKind,
    pub size_bytes: u64,
    pub exists: bool,
    pub source: ArtifactSource,
    /// Sequences of the transcript messages that referenced this file, in
    /// order. Empty for generated-but-never-sent files. The first entry backs
    /// the tab's "Go to message" jump.
    pub message_sequences: Vec<usize>,
    /// RFC3339 mtime of the file on disk, when it exists.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified_at: Option<String>,
}

/// Thread artifact directory (`$ZDX_HOME/artifacts/threads/<id>`, or
/// `.../scratch` when `thread_id` is missing/blank).
pub fn artifact_dir_for_thread(thread_id: Option<&str>) -> PathBuf {
    paths::artifact_dir_for_thread(thread_id)
}

/// Same as [`artifact_dir_for_thread`] anchored at an explicit home (tests).
pub fn artifact_dir_for_thread_with_zdx_home(zdx_home: &Path, thread_id: Option<&str>) -> PathBuf {
    let root = zdx_home.join("artifacts");
    match thread_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(id) => root.join("threads").join(id),
        None => root.join("scratch"),
    }
}

/// MIME type inferred from the file extension. Unknown extensions fall back
/// to `application/octet-stream` so every artifact stays downloadable.
pub fn mime_for_path(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("svg") => "image/svg+xml",
        Some("ogg" | "oga" | "opus") => "audio/ogg",
        Some("mp3") => "audio/mpeg",
        Some("m4a") => "audio/mp4",
        Some("wav") => "audio/wav",
        Some("aac") => "audio/aac",
        Some("flac") => "audio/flac",
        Some("html" | "htm") => "text/html",
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("json") => "application/json",
        Some("mp4") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

/// Preview kind for a path: images and audio render inline, HTML gets the
/// sandboxed full-screen preview, everything else is download-only.
pub fn kind_for_path(path: &Path) -> ArtifactKind {
    let mime = mime_for_path(path);
    if mime.starts_with("image/") {
        ArtifactKind::Image
    } else if mime.starts_with("audio/") {
        ArtifactKind::Audio
    } else if mime == "text/html" {
        ArtifactKind::Html
    } else {
        ArtifactKind::Other
    }
}

/// Absolute `<media>` paths referenced by one transcript message, deduped in
/// order. Mirrors the bot's send path: only absolute paths are valid, and the
/// `<medias>` wrapper itself is not a tag.
pub fn extract_media_paths(text: &str) -> Vec<String> {
    const TAG_OPEN: &str = "<media";
    const TAG_CLOSE: &str = "</media>";

    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(start_rel) = text[cursor..].find(TAG_OPEN) {
        let start = cursor + start_rel;
        let Some(next) = text.as_bytes().get(start + TAG_OPEN.len()) else {
            break;
        };
        if *next == b's' {
            cursor = start + TAG_OPEN.len();
            continue;
        }
        let Some(open_end_rel) = text[start..].find('>') else {
            break;
        };
        let open_end = start + open_end_rel;
        if text[start..=open_end].ends_with("/>") {
            cursor = open_end + 1;
            continue;
        }
        let content_start = open_end + 1;
        let Some(close_rel) = text[content_start..].find(TAG_CLOSE) else {
            break;
        };
        let inner = text[content_start..content_start + close_rel].trim();
        if let Some(path) = clean_media_path(inner)
            && !out.contains(&path)
        {
            out.push(path);
        }
        cursor = content_start + close_rel + TAG_CLOSE.len();
    }
    out
}

fn clean_media_path(raw: &str) -> Option<String> {
    let candidate = raw
        .trim()
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim_end_matches([',', ';'])
        .trim();
    if candidate.starts_with('/') {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// Lexically normalizes an absolute path (resolves `.`/`..` without touching
/// the filesystem) so transcript references dedupe against walked files.
fn normalize_absolute(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

fn modified_rfc3339(path: &Path) -> Option<String> {
    let modified: std::time::SystemTime = fs::metadata(path).ok()?.modified().ok()?;
    let datetime: chrono::DateTime<chrono::Utc> = modified.into();
    Some(datetime.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn attachment_for_absolute(path: &Path) -> ArtifactAttachment {
    let meta = fs::metadata(path).ok().filter(std::fs::Metadata::is_file);
    ArtifactAttachment {
        name: file_name_of(path),
        path: path.to_string_lossy().into_owned(),
        mime: mime_for_path(path).to_string(),
        kind: kind_for_path(path),
        size_bytes: meta.as_ref().map_or(0, std::fs::Metadata::len),
        exists: meta.is_some(),
    }
}

/// Inline attachments for one transcript message's raw text.
pub fn attachments_for_message(text: &str) -> Vec<ArtifactAttachment> {
    extract_media_paths(text)
        .iter()
        .map(|raw| attachment_for_absolute(&normalize_absolute(Path::new(raw))))
        .collect()
}

#[derive(Default)]
struct DiskEntry {
    path: PathBuf,
    rel: Option<String>,
    size_bytes: u64,
    modified_at: Option<String>,
}

fn walk_artifact_dir(dir: &Path) -> Vec<DiskEntry> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = vec![(dir.to_path_buf(), 0)];
    while let Some((current, depth)) = stack.pop() {
        if out.len() >= MAX_ARTIFACT_FILES || depth > MAX_WALK_DEPTH {
            continue;
        }
        let Ok(entries) = fs::read_dir(&current) else {
            continue;
        };
        let mut names: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        names.sort();
        for path in names {
            if out.len() >= MAX_ARTIFACT_FILES {
                break;
            }
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                continue;
            }
            if meta.is_dir() {
                stack.push((path, depth + 1));
            } else if meta.is_file() {
                let rel = path.strip_prefix(dir).ok().map(|rel| {
                    rel.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/")
                });
                let modified_at = modified_rfc3339(&path);
                out.push(DiskEntry {
                    path,
                    rel,
                    size_bytes: meta.len(),
                    modified_at,
                });
            }
        }
    }
    out
}

/// Absolute media path -> message sequences that referenced it, in order.
fn media_sequences(events: &[ThreadEvent]) -> BTreeMap<String, Vec<usize>> {
    let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (sequence, event) in events.iter().enumerate() {
        let ThreadEvent::Message { text, .. } = event else {
            continue;
        };
        for raw in extract_media_paths(text) {
            let key = normalize_absolute(Path::new(&raw))
                .to_string_lossy()
                .into_owned();
            map.entry(key).or_default().push(sequence);
        }
    }
    for sequences in map.values_mut() {
        sequences.sort_unstable();
        sequences.dedup();
    }
    map
}

/// Lists a thread's artifacts: files in its artifact dir merged with files the
/// transcript sent, deduped on the canonical absolute path. Never fails on a
/// missing thread or artifact dir; both simply contribute nothing.
///
/// # Errors
/// Currently infallible (missing inputs contribute nothing), but returns
/// `Result` to leave room for fallible index reads without breaking callers.
pub fn list_thread_artifacts(thread_id: &str) -> Result<Vec<Artifact>> {
    let dir = artifact_dir_for_thread(Some(thread_id));
    let disk = if dir.is_dir() {
        walk_artifact_dir(&dir)
    } else {
        Vec::new()
    };
    let events = load_thread_events(thread_id).unwrap_or_default();
    Ok(list_with(&dir, disk, &media_sequences(&events)))
}

fn list_with(
    dir: &Path,
    disk: Vec<DiskEntry>,
    refs: &BTreeMap<String, Vec<usize>>,
) -> Vec<Artifact> {
    let mut by_key: HashMap<String, Artifact> = HashMap::new();

    for entry in disk {
        let key = normalize_absolute(&entry.path)
            .to_string_lossy()
            .into_owned();
        let sequences = refs.get(&key).cloned().unwrap_or_default();
        let key_path = Path::new(&key);
        let mime = mime_for_path(key_path).to_string();
        let kind = kind_for_path(key_path);
        let name = file_name_of(key_path);
        by_key.insert(
            key.clone(),
            Artifact {
                name,
                path: key,
                rel: entry.rel,
                mime,
                kind,
                size_bytes: entry.size_bytes,
                exists: true,
                source: if sequences.is_empty() {
                    ArtifactSource::Generated
                } else {
                    ArtifactSource::Both
                },
                message_sequences: sequences,
                modified_at: entry.modified_at,
            },
        );
    }

    for (key, sequences) in refs {
        if by_key.contains_key(key) {
            continue;
        }
        let path = normalize_absolute(Path::new(key));
        let attachment = attachment_for_absolute(&path);
        let rel = path.strip_prefix(dir).ok().map(|rel| {
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/")
        });
        by_key.insert(
            key.clone(),
            Artifact {
                name: attachment.name,
                path: key.clone(),
                rel,
                mime: attachment.mime,
                kind: attachment.kind,
                size_bytes: attachment.size_bytes,
                exists: attachment.exists,
                source: ArtifactSource::Sent,
                message_sequences: sequences.clone(),
                modified_at: modified_rfc3339(&path),
            },
        );
    }

    let mut artifacts: Vec<Artifact> = by_key.into_values().collect();
    artifacts.sort_by(|a, b| {
        b.modified_at
            .cmp(&a.modified_at)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    artifacts.truncate(MAX_ARTIFACT_FILES);
    artifacts
}

/// Resolves a download `?path=` value for a thread to a file on disk.
///
/// Relative values stay inside the thread artifact dir (traversal rejected).
/// Absolute values must either live under that dir or have been sent in this
/// thread's transcript, so the endpoint cannot become an arbitrary file read.
///
/// # Errors
/// Returns an error when the path is empty, escapes the artifact dir, is an
/// absolute path the thread never sent, or does not resolve to a file on disk.
pub fn resolve_artifact_for_download(
    thread_id: &str,
    path_param: &str,
) -> Result<(PathBuf, String)> {
    let raw = path_param.trim();
    if raw.is_empty() || raw.contains('\0') {
        bail!("Invalid artifact path");
    }

    let dir = artifact_dir_for_thread(Some(thread_id));
    let candidate = if raw.starts_with('/') {
        let normalized = normalize_absolute(Path::new(raw));
        let under_dir = normalized.starts_with(&dir);
        if !under_dir {
            let events = load_thread_events(thread_id)
                .with_context(|| format!("load thread `{thread_id}`"))?;
            let refs = media_sequences(&events);
            if !refs.contains_key(&normalized.to_string_lossy().into_owned()) {
                bail!("Artifact is not part of this thread");
            }
        }
        normalized
    } else {
        if raw.starts_with('\\') || Path::new(raw).is_absolute() {
            bail!("Invalid artifact path");
        }
        let joined = dir.join(raw);
        let normalized = normalize_absolute(&joined);
        if !normalized.starts_with(&dir) {
            bail!("Invalid artifact path");
        }
        normalized
    };

    let meta = fs::metadata(&candidate)
        .with_context(|| format!("read artifact `{}`", candidate.display()))?;
    if !meta.is_file() {
        bail!("Artifact is not a file");
    }
    let mime = mime_for_path(&candidate).to_string();
    Ok((candidate, mime))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;

    use super::*;
    use crate::core::thread_persistence::Thread;

    #[test]
    fn artifact_dir_uses_threads_and_scratch() {
        let _home = crate::test_support::temp_zdx_home();
        let dir = artifact_dir_for_thread(Some("thread-1"));
        assert!(dir.ends_with(Path::new("artifacts/threads/thread-1")));
        let scratch = artifact_dir_for_thread(None);
        assert!(scratch.ends_with(Path::new("artifacts/scratch")));
        let blank = artifact_dir_for_thread(Some("   "));
        assert!(blank.ends_with(Path::new("artifacts/scratch")));
    }

    #[test]
    fn extracts_only_absolute_media_paths_deduped() {
        let text = "done <medias><media>/tmp/a.png</media><media>/tmp/a.png</media><media>relative/b.png</media></medias> tail";
        assert_eq!(extract_media_paths(text), vec!["/tmp/a.png".to_string()]);
        assert!(extract_media_paths("no tags here").is_empty());
        assert!(extract_media_paths("<medias></medias>").is_empty());
    }

    #[test]
    fn classifies_preview_kinds() {
        assert_eq!(kind_for_path(Path::new("a.png")), ArtifactKind::Image);
        assert_eq!(kind_for_path(Path::new("a.ogg")), ArtifactKind::Audio);
        assert_eq!(kind_for_path(Path::new("a.mp3")), ArtifactKind::Audio);
        assert_eq!(kind_for_path(Path::new("a.html")), ArtifactKind::Html);
        assert_eq!(kind_for_path(Path::new("a.pdf")), ArtifactKind::Other);
        assert_eq!(mime_for_path(Path::new("a.pdf")), "application/pdf");
    }

    #[test]
    fn lists_generated_files_and_merges_sent_refs() {
        let _home = crate::test_support::temp_zdx_home();
        let thread_id = format!("artifacts-{}", uuid::Uuid::new_v4());
        let dir = artifact_dir_for_thread(Some(&thread_id));
        fs::create_dir_all(&dir).unwrap();

        let generated = dir.join("report.html");
        let mut file = fs::File::create(&generated).unwrap();
        writeln!(file, "<h1>hi</h1>").unwrap();
        let unlinked = dir.join("notes.txt");
        fs::write(&unlinked, "scratch").unwrap();
        let outside = artifact_dir_for_thread(None).join(format!("outside-{thread_id}.png"));
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        fs::write(&outside, "fake-png").unwrap();

        let mut thread = Thread::with_id(thread_id.clone()).unwrap();
        thread
            .append(&ThreadEvent::assistant_message(format!(
                "done <media>{}</media> and <media>{}</media>",
                generated.display(),
                outside.display()
            )))
            .unwrap();

        let artifacts = list_thread_artifacts(&thread_id).unwrap();
        assert_eq!(artifacts.len(), 3);

        let both = artifacts
            .iter()
            .find(|a| a.path == normalize_absolute(&generated).to_string_lossy())
            .unwrap();
        assert_eq!(both.source, ArtifactSource::Both);
        assert_eq!(both.kind, ArtifactKind::Html);
        assert_eq!(both.message_sequences, vec![1]);
        assert!(both.rel.as_deref() == Some("report.html"));

        let sent = artifacts
            .iter()
            .find(|a| a.path == normalize_absolute(&outside).to_string_lossy())
            .unwrap();
        assert_eq!(sent.source, ArtifactSource::Sent);
        assert_eq!(sent.message_sequences, vec![1]);

        let unlinked_row = artifacts.iter().find(|a| a.name == "notes.txt").unwrap();
        assert_eq!(unlinked_row.source, ArtifactSource::Generated);
        assert!(unlinked_row.message_sequences.is_empty());
    }

    #[test]
    fn download_rejects_traversal_and_unrelated_absolute_paths() {
        let _home = crate::test_support::temp_zdx_home();
        let thread_id = format!("artifacts-dl-{}", uuid::Uuid::new_v4());
        let dir = artifact_dir_for_thread(Some(&thread_id));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");
        fs::write(&file, "hi").unwrap();

        let mut thread = Thread::with_id(thread_id.clone()).unwrap();
        thread
            .append(&ThreadEvent::assistant_message("hi"))
            .unwrap();

        assert!(resolve_artifact_for_download(&thread_id, "../other.txt").is_err());
        assert!(resolve_artifact_for_download(&thread_id, "/etc/hostname").is_err());
        assert!(resolve_artifact_for_download(&thread_id, "a.txt").is_ok());
        assert!(resolve_artifact_for_download(&thread_id, &file.to_string_lossy()).is_ok());
    }
}
