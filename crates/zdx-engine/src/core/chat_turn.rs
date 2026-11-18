//! Model-facing text of a chat-surface turn, shared by every chat surface
//! (the Telegram bot and gateway) through the engine socket and the
//! `zdx telegram` CLI twins:
//!
//! - [`compose_user_text`]: one incoming chat message (reply context, text,
//!   transcripts, attachment notes) as the user text a turn records.
//! - [`reply_parts`]: a turn's final answer split into the chat text, the
//!   `<followups>` suggestions, and the `<media>` files to send, each with
//!   the artifact identity the Mini App lists.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::artifacts::{self, ArtifactKind};

/// The message an incoming one replies to, when it has text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepliedTo {
    pub message_id: i64,
    /// Sent by a bot (this bot, or a job posting through its account).
    #[serde(default)]
    pub from_bot: bool,
    /// The part the user quoted, else the message text or caption.
    pub text: String,
    /// `text` is the part the user selected, not the whole message.
    #[serde(default)]
    pub quoted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioInput {
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript: Option<String>,
    /// What the clip belongs to (Mini App feedback notes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// How an image reaches the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ImageDelivery {
    /// Sent inline; `downscaled` when the model gets a smaller copy.
    Inline {
        #[serde(default)]
        downscaled: bool,
    },
    /// Saved only; `reason` says why the model cannot see it inline.
    SavedOnly {
        reason: String,
        /// The saved file fits `ask_media`'s type and size limits.
        #[serde(default)]
        ask_media: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageInput {
    pub path: PathBuf,
    pub delivery: ImageDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentInput {
    pub file_name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum VideoStatus {
    Saved {
        path: PathBuf,
    },
    /// Not downloaded: over the media-understanding limit.
    TooLarge {
        size_bytes: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VideoInput {
    /// Human label of the media kind ("Video", "Video note", ...).
    pub kind: String,
    #[serde(flatten)]
    pub status: VideoStatus,
}

/// One incoming chat message, as [`compose_user_text`] takes it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<RepliedTo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub audios: Vec<AudioInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub documents: Vec<DocumentInput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub videos: Vec<VideoInput>,
    /// Attachments that could not be loaded, as prompt sentences.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notices: Vec<String>,
}

/// The replied-to message, placed before the user's words.
fn reply_to_block(reply: &RepliedTo) -> String {
    let sender = if reply.from_bot {
        "the bot (or another run posting through it)"
    } else {
        "a user"
    };
    let what = if reply.quoted {
        "quotes this part of"
    } else {
        "replies to"
    };
    format!(
        "<reply_to>\nThe user's message below {what} message {} in this chat, sent by {sender}:\n{}\n</reply_to>",
        reply.message_id, reply.text
    )
}

/// The user text a chat turn records for one incoming message.
#[must_use]
pub fn compose_user_text(input: &UserInput) -> String {
    let mut parts = Vec::new();
    if let Some(reply) = input.reply_to.as_ref() {
        parts.push(reply_to_block(reply));
    }
    if let Some(text) = input.text.as_ref()
        && !text.trim().is_empty()
    {
        parts.push(text.clone());
    }

    for audio in &input.audios {
        // A labelled clip (Mini App feedback) names the note it belongs to,
        // so the transcript stays attached to it.
        let prefix = audio
            .label
            .as_ref()
            .map_or_else(String::new, |label| format!("{label}: "));
        if let Some(transcript) = &audio.transcript {
            if !prefix.is_empty() {
                parts.push(format!("{prefix}transcript"));
            }
            parts.push(crate::providers::wrap_voice_transcript(transcript));
            parts.push(format!(
                "(Audio file saved at {} — use the `ask_media` tool on it if you need more than the transcript.)",
                audio.path.display()
            ));
        } else {
            parts.push(format!(
                "{prefix}Audio attachment saved at {} (transcription unavailable).",
                audio.path.display()
            ));
        }
    }

    for image in &input.images {
        let path = image.path.display();
        let (prefix, subject) = match &image.label {
            Some(label) => (format!("{label}: "), "image attachment"),
            None => (String::new(), "Image attachment"),
        };
        let text = match &image.delivery {
            ImageDelivery::Inline { downscaled: false } => format!("{subject} saved at {path}."),
            ImageDelivery::Inline { downscaled: true } => format!(
                "{subject} saved at {path}. You see a downscaled copy; the saved file is the full-resolution original — use `ask_media` on it if you need fine detail."
            ),
            ImageDelivery::SavedOnly {
                reason,
                ask_media: true,
            } => format!(
                "{subject} saved at {path}, but it was not attached for you to see: {reason}. Use the `ask_media` tool on the saved file to inspect it."
            ),
            ImageDelivery::SavedOnly {
                reason,
                ask_media: false,
            } => format!(
                "{subject} saved at {path}, but it was not attached for you to see: {reason}. `ask_media` cannot read this file either; ask the user to resend it as a PNG or JPEG."
            ),
        };
        parts.push(format!("{prefix}{text}"));
    }

    for doc in &input.documents {
        parts.push(format!(
            "Document attachment '{}' saved at {} — use the `ask_media` tool on it to read its contents.",
            doc.file_name,
            doc.path.display()
        ));
    }

    for video in &input.videos {
        parts.push(match &video.status {
            VideoStatus::Saved { path } => format!(
                "{} attachment saved at {} — use the `ask_media` tool on it to watch it.",
                video.kind,
                path.display()
            ),
            #[allow(clippy::cast_precision_loss)]
            VideoStatus::TooLarge { size_bytes } => format!(
                "{} attachment was not downloaded: it is {:.1} MiB and the limit is {} MiB. Ask the user for a shorter or compressed clip.",
                video.kind,
                *size_bytes as f64 / (1024.0 * 1024.0),
                crate::media::MAX_INLINE_BYTES / (1024 * 1024)
            ),
        });
    }

    parts.extend(input.notices.iter().cloned());

    if parts.is_empty() {
        "User sent an attachment.".to_string()
    } else {
        parts.join("\n\n")
    }
}

/// Longest edge of an image the model sees inline.
pub const INLINE_IMAGE_EDGE: u32 = 1568;
/// Largest inline image, in bytes.
pub const INLINE_IMAGE_BYTES: usize = 1024 * 1024;
/// Image types every provider takes inline.
const SUPPORTED_IMAGE_MIMES: &[&str] = &["image/jpeg", "image/png", "image/gif", "image/webp"];

/// An incoming image made ready for a turn: how the model gets it, and the
/// inline bytes when it gets them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedImage {
    pub delivery: ImageDelivery,
    /// The image the model sees inline (`submit.images`), when it is inline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<crate::orchestration::updates::InputImage>,
}

/// Shrinks an image to the inline limits, or explains why the model cannot
/// see it. An image the decoder cannot read (GIF, for one) is sent as-is when
/// it is a type providers accept and already within the byte limit.
#[must_use]
pub fn prepare_inline_image(local_path: &Path, bytes: &[u8]) -> PreparedImage {
    use base64::Engine as _;
    let encode = |data: &[u8]| base64::engine::general_purpose::STANDARD.encode(data);
    if let Some((resized, mime_type)) =
        crate::images::downscale_to_fit(bytes, INLINE_IMAGE_EDGE, INLINE_IMAGE_BYTES)
    {
        return PreparedImage {
            delivery: ImageDelivery::Inline { downscaled: true },
            image: Some(crate::orchestration::updates::InputImage {
                mime_type: mime_type.to_string(),
                data: encode(&resized),
            }),
        };
    }

    let detected = infer::get(bytes).map(|kind| kind.mime_type());
    let reason = match detected {
        Some(mime) if SUPPORTED_IMAGE_MIMES.contains(&mime) => {
            if bytes.len() <= INLINE_IMAGE_BYTES {
                return PreparedImage {
                    delivery: ImageDelivery::Inline { downscaled: false },
                    image: Some(crate::orchestration::updates::InputImage {
                        mime_type: mime.to_string(),
                        data: encode(bytes),
                    }),
                };
            }
            format!(
                "it is {} and could not be shrunk below the {} inline limit",
                format_mib(bytes.len() as u64),
                format_mib(INLINE_IMAGE_BYTES as u64)
            )
        }
        Some(mime) => format!("the model cannot take {mime} images directly"),
        None => "its image format was not recognized".to_string(),
    };

    PreparedImage {
        delivery: ImageDelivery::SavedOnly {
            reason,
            ask_media: crate::media::can_ask_media(local_path, bytes.len() as u64),
        },
        image: None,
    }
}

#[allow(clippy::cast_precision_loss)]
fn format_mib(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

const MEDIA_BLOCK_OPEN: &str = "<medias>";
const MEDIA_BLOCK_CLOSE: &str = "</medias>";
const MEDIA_TAG_OPEN: &str = "<media";
const MEDIA_TAG_CLOSE: &str = "</media>";

/// How a chat sends a media file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendAs {
    Photo,
    Voice,
    Audio,
    Video,
    Document,
}

/// One `<media>` file of a reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyMedia {
    pub path: PathBuf,
    /// The Mini App's key for this file (deep links, `Open artifact`).
    pub artifact_key: String,
    pub kind: ArtifactKind,
    pub send_as: SendAs,
    /// The path's lexical form opens the same file; when false, the file
    /// sent is not the one an artifact link resolves to.
    pub lexical_is_physical: bool,
    /// An absolute path to an existing regular file: only these are sent.
    pub sendable: bool,
}

/// A final answer split for a chat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyParts {
    /// The reply Markdown, without `<followups>` and `<media>` blocks.
    pub text: String,
    pub followups: Vec<String>,
    /// Each file once, in reply order.
    pub media: Vec<ReplyMedia>,
}

/// Splits a final answer into the chat text, its follow-up suggestions, and
/// its media files.
#[must_use]
pub fn reply_parts(final_text: &str) -> ReplyParts {
    let (without_followups, followups) = crate::followups::extract_followups(final_text);
    let without_wrappers = without_followups
        .replace(MEDIA_BLOCK_OPEN, "")
        .replace(MEDIA_BLOCK_CLOSE, "");
    let (text, raw_media) = extract_media_tags(&without_wrappers);
    let mut media = Vec::new();
    let mut seen = HashSet::new();
    for raw in raw_media {
        let Some(path) = artifacts::clean_media_path(&raw).map(PathBuf::from) else {
            continue;
        };
        let lexical_is_physical = artifacts::lexical_path_is_physical(&path);
        let artifact_key = artifacts::artifact_key(&path);
        // Spellings that resolve to the same place share the artifact key;
        // a lexical form that opens another file keeps its own spelling.
        let identity = if lexical_is_physical {
            format!("key:{artifact_key}")
        } else {
            format!("path:{}", path.display())
        };
        if !seen.insert(identity) {
            continue;
        }
        media.push(ReplyMedia {
            kind: artifacts::kind_for_path(&path),
            send_as: send_as(&path),
            sendable: path.is_absolute()
                && std::fs::metadata(&path).is_ok_and(|meta| meta.is_file()),
            artifact_key,
            lexical_is_physical,
            path,
        });
    }
    ReplyParts {
        text,
        followups,
        media,
    }
}

/// The Telegram method that fits a file: photo, voice note, audio,
/// video, or document.
#[must_use]
pub fn send_as(path: &Path) -> SendAs {
    let ext = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "webp" => SendAs::Photo,
        "ogg" | "oga" | "opus" => SendAs::Voice,
        "mp3" | "m4a" | "wav" | "aac" | "flac" => SendAs::Audio,
        "mp4" | "mov" | "m4v" | "webm" => SendAs::Video,
        _ => SendAs::Document,
    }
}

fn extract_media_tags(input: &str) -> (String, Vec<String>) {
    let mut cleaned = String::new();
    let mut media_values = Vec::new();
    let mut cursor = 0;

    while let Some(start_rel) = input[cursor..].find(MEDIA_TAG_OPEN) {
        let start = cursor + start_rel;
        let Some(after_tag_name) = input.as_bytes().get(start + MEDIA_TAG_OPEN.len()) else {
            break;
        };
        // Skip accidental matches such as "<medias>".
        if *after_tag_name == b's' {
            let skip_to = start + MEDIA_TAG_OPEN.len();
            cleaned.push_str(&input[cursor..skip_to]);
            cursor = skip_to;
            continue;
        }

        cleaned.push_str(&input[cursor..start]);

        let Some(open_end_rel) = input[start..].find('>') else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let open_end = start + open_end_rel;
        let open_tag = &input[start..=open_end];

        if open_tag.ends_with("/>") {
            cursor = open_end + 1;
            continue;
        }

        let content_start = open_end + 1;
        let Some(close_rel) = input[content_start..].find(MEDIA_TAG_CLOSE) else {
            cleaned.push_str(&input[start..]);
            cursor = input.len();
            break;
        };
        let content_end = content_start + close_rel;
        let inner = input[content_start..content_end].trim();
        if !inner.is_empty() {
            media_values.push(inner.to_string());
        }

        cursor = content_end + MEDIA_TAG_CLOSE.len();
    }

    if cursor < input.len() {
        cleaned.push_str(&input[cursor..]);
    }

    (cleaned, media_values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reply_parts_strips_blocks_and_keeps_each_file_once() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("out.png");
        std::fs::write(&file, b"png").unwrap();
        let text = format!(
            "Done.\n<medias><media>{0}</media><media>{0}</media><media>/nope/x.pdf</media></medias>\n<followups><followup>Next?</followup></followups>",
            file.display()
        );
        let parts = reply_parts(&text);
        assert_eq!(parts.text.trim(), "Done.");
        assert_eq!(parts.followups, ["Next?"]);
        assert_eq!(parts.media.len(), 2);
        assert_eq!(parts.media[0].send_as, SendAs::Photo);
        assert!(parts.media[0].sendable);
        assert_eq!(parts.media[0].kind, ArtifactKind::Image);
        assert!(!parts.media[1].sendable);
        assert_eq!(parts.media[1].send_as, SendAs::Document);
    }

    #[test]
    fn send_as_routes_video_and_leaves_other_video_on_document() {
        for ext in ["mp4", "mov", "m4v", "webm", "MP4"] {
            assert_eq!(
                send_as(Path::new(&format!("clip.{ext}"))),
                SendAs::Video,
                "{ext}"
            );
        }
        for ext in ["mkv", "avi", "mpg", "ogv", "pdf"] {
            assert_eq!(
                send_as(Path::new(&format!("clip.{ext}"))),
                SendAs::Document,
                "{ext}"
            );
        }
    }

    #[test]
    fn user_text_keeps_reply_context_and_attachment_notes() {
        let text = compose_user_text(&UserInput {
            reply_to: Some(RepliedTo {
                message_id: 7,
                from_bot: true,
                text: "earlier".to_string(),
                quoted: false,
            }),
            text: Some("how much?".to_string()),
            audios: vec![AudioInput {
                path: "/tmp/a.oga".into(),
                transcript: Some("hello".to_string()),
                label: None,
            }],
            ..UserInput::default()
        });
        assert!(text.starts_with("<reply_to>\nThe user's message below replies to message 7"));
        assert!(text.contains("how much?"));
        assert!(text.contains("/tmp/a.oga"));
        assert_eq!(
            compose_user_text(&UserInput::default()),
            "User sent an attachment."
        );
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut std::io::Cursor::new(&mut buf), image::ImageFormat::Png)
            .unwrap();
        buf
    }

    fn prompt_for(path: &Path, delivery: ImageDelivery) -> String {
        compose_user_text(&UserInput {
            images: vec![ImageInput {
                path: path.to_path_buf(),
                delivery,
                label: None,
            }],
            ..UserInput::default()
        })
    }

    #[test]
    fn a_large_image_is_sent_downscaled_with_its_aspect_ratio() {
        use base64::Engine as _;
        // Small in pixels on purpose: image work is slow in a debug build.
        let path = Path::new("/tmp/screen.png");
        let prepared = prepare_inline_image(path, &png(3200, 100));
        assert_eq!(
            prepared.delivery,
            ImageDelivery::Inline { downscaled: true }
        );
        let sent = base64::engine::general_purpose::STANDARD
            .decode(&prepared.image.unwrap().data)
            .unwrap();
        assert!(sent.len() <= INLINE_IMAGE_BYTES);
        let copy = image::load_from_memory(&sent).unwrap();
        assert_eq!((copy.width(), copy.height()), (INLINE_IMAGE_EDGE, 49));
        assert!(prompt_for(path, prepared.delivery).contains("You see a downscaled copy"));
    }

    #[test]
    fn images_the_model_cannot_take_are_saved_with_guidance() {
        let mut heic = vec![0, 0, 0, 0x18];
        heic.extend_from_slice(b"ftypheic\0\0\0\0mif1heic");
        heic.resize(64, 0);
        let path = Path::new("/tmp/IMG_1.HEIC");
        let prepared = prepare_inline_image(path, &heic);
        assert!(prepared.image.is_none());
        let prompt = prompt_for(path, prepared.delivery);
        assert!(
            prompt.contains("not attached for you to see: the model cannot take image/heif images directly. Use the `ask_media` tool"),
            "{prompt}"
        );

        let svg = b"<svg xmlns='http://www.w3.org/2000/svg'/>";
        let path = Path::new("/tmp/logo.svg");
        let prepared = prepare_inline_image(path, svg);
        assert!(prepared.image.is_none());
        let prompt = prompt_for(path, prepared.delivery);
        assert!(
            prompt.contains(
                "its image format was not recognized. `ask_media` cannot read this file either"
            ),
            "{prompt}"
        );
    }
}
