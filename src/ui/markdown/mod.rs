//! Markdown parsing and rendering for assistant responses.
//!
//! This module provides:
//! - `render_markdown()`: Parse markdown text into styled lines
//! - `wrap_styled_spans()`: Wrap styled spans while preserving styles across line breaks
//! - `MarkdownStreamCollector`: Streaming markdown accumulator with incremental rendering
//!
//! Uses pulldown-cmark for parsing. Falls back to plain text if parsing fails.

mod parse;
mod stream;
mod wrap;

pub use parse::render_markdown;
#[allow(unused_imports)]
pub use stream::{MarkdownStreamCollector, render_markdown_streaming};
#[allow(unused_imports)]
pub use wrap::{WrapOptions, wrap_styled_spans};
