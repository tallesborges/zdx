//! Shared transcript display model and rendering.
//!
//! Extracted from `zdx-tui` so non-interactive consumers (e.g. the monitor
//! transcript overlay) can render thread transcripts with the same formatting
//! (markdown, wrapping, tool pairing) without depending on the interactive TUI
//! crate. The interactive pieces (selection, lazy virtualization, viewport
//! state) stay in `zdx-tui`.

mod build;
mod cell;
mod convert;
pub mod markdown;
mod reasoning;
mod style;
pub mod text;
mod tool_detail;
mod wrap;

pub use build::build_transcript_from_events;
pub use cell::{CellId, ChildToolEntry, ChildToolState, HistoryCell, ToolState, tool_command_text};
pub use convert::{
    cells_to_lines_with_offsets, convert_style, convert_styled_line, wrap_line_to_width,
};
pub use reasoning::reasoning_display_text;
pub use style::{Style, StyledLine, StyledSpan};
pub use tool_detail::{ToolDetailBody, tool_detail_body, tool_state_color, tool_state_glyph};
pub use wrap::WrapCache;
