//! Markdown parsing and rendering for assistant responses.
//!
//! This module provides:
//! - `render_markdown()`: Parse markdown text into styled lines
//! - `render_markdown_streaming()`: Incremental streaming markdown rendering
//!
//! Uses pulldown-cmark for parsing. Falls back to plain text if parsing fails.

mod parse;
mod stream;
mod wrap;

pub use parse::render_markdown;
pub use stream::render_markdown_streaming;
