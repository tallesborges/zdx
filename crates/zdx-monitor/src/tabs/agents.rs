use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use zdx_engine::agent_activity;
use zdx_engine::config::paths;
use zdx_engine::core::thread_persistence;

use crate::app::{MonitorApp, copy_text, lines_text};
use crate::ui::{SELECTED_BG, centered_rect, spinner_frame_now, truncate_chars};

pub struct ActiveAgentInfo {
    pub pid: u32,
    pub surface: String,
    pub thread_id: String,
    /// Full (un-truncated) thread id used to locate the transcript file.
    /// `None` for tracked runs that don't persist a thread.
    pub full_thread_id: Option<String>,
    /// Originating thread id when this run was spawned by another run. Used to
    /// nest child runs under their parent in the Active Agents tree.
    pub parent_thread_id: Option<String>,
    /// Tree connector prefix (e.g. `"├─ "`) computed from the parent/child
    /// layout; empty for top-level runs.
    pub tree_prefix: String,
    pub model: String,
    pub provider: String,
    /// Named OAuth account serving this run (`None` = default account).
    pub account: Option<String>,
    pub thinking: String,
    pub uptime: String,
    pub kind: Option<String>,
    pub subagent_name: Option<String>,
    /// Name of the currently executing tool call (`"bash"`), when the run is
    /// inside a tool round. `None` between tool rounds.
    pub current_tool: Option<String>,
    /// Coarse run phase from the marker (`waiting`/`thinking`/`answering`/
    /// `retrying`), shown when no tool is running.
    pub phase: Option<String>,
}

/// State for the Active Agents transcript overlay (drill-in on `Enter`).
pub struct AgentOverlayState {
    /// Thread id captured when the overlay was opened (never re-derived from
    /// the live selection).
    pub thread_id: String,
    /// Header label, e.g. `provider:model@thinking abc12345`.
    pub title: String,
    /// Rendered transcript lines (formatted markdown via `zdx-transcript`).
    pub lines: Vec<Line<'static>>,
    /// Cells backing `lines`, kept so the tool pane can render a tool's detail.
    pub cells: Vec<zdx_transcript::HistoryCell>,
    /// Tool calls in display order, with the line each one's header sits on.
    pub tools: Vec<ToolRef>,
    /// Thinking blocks in display order, one entry per thinking cell.
    pub thinking: Vec<ThinkingRef>,
    /// Ordinals of the thinking blocks the user expanded. Cells are rebuilt
    /// from disk on every refresh, so expansion lives here and is re-applied
    /// on each load rather than on the cell.
    pub expanded_thinking: HashSet<usize>,
    /// Currently highlighted tool call (`tool_use_id`), for drill-in.
    pub tool_selected: Option<String>,
    /// Open tool detail pane, if any.
    pub tool_pane: Option<ToolPaneState>,
    /// Manual top-line scroll offset. `None` follows the newest content.
    pub scroll: Option<usize>,
    /// The captured run is no longer active (marker gone). Presentation only —
    /// reads continue while the overlay is open.
    pub ended: bool,
    /// No thread id was available for this run.
    pub unavailable: bool,
    /// Last-seen transcript file size, to skip reparsing unchanged files.
    pub file_len: u64,
    /// Last-seen transcript file mtime, to skip reparsing unchanged files.
    pub file_mtime: Option<SystemTime>,
    /// Tool-use ids of the run's in-flight tools at the last render, so the
    /// overlay rebuilds when a tool starts/finishes even though the JSONL
    /// hasn't changed yet.
    pub running_sig: Vec<String>,
    /// Coarse phase of the run from its activity marker, shown in the title.
    /// Refreshed every tick; `None` once the run ends.
    pub run_phase: Option<String>,
    /// Name of the first in-flight tool, shown in the title with precedence
    /// over `run_phase` (a stale `thinking` would mislead during tool rounds).
    pub running_tool: Option<String>,
    /// Width the transcript was last rendered at (re-render on resize).
    pub width: usize,
}

impl AgentOverlayState {
    /// Largest top-line offset that still fills a `page`-row viewport.
    pub fn max_scroll(&self, page: usize) -> usize {
        self.lines.len().saturating_sub(page)
    }

    /// First transcript line shown in a `page`-row viewport. `scroll == None`
    /// follows the newest content, so it resolves to the bottom.
    ///
    /// Rendering and row→line hit-testing must agree on this, or clicks land on
    /// the wrong line; both go through here.
    pub fn top_line(&self, page: usize) -> usize {
        let max = self.max_scroll(page);
        self.scroll.unwrap_or(max).min(max)
    }

    /// Index into `tools` of the highlighted tool, if it is still present.
    pub fn selected_tool_index(&self) -> Option<usize> {
        let id = self.tool_selected.as_deref()?;
        self.tools.iter().position(|t| t.tool_use_id == id)
    }

    /// The cell for a `tool_use_id`, looked up live so a running tool's pane
    /// picks up new output on each refresh.
    pub fn tool_cell(&self, tool_use_id: &str) -> Option<&zdx_transcript::HistoryCell> {
        self.cells.iter().find(|cell| {
            matches!(cell, zdx_transcript::HistoryCell::Tool { tool_use_id: id, .. } if id == tool_use_id)
        })
    }
}

/// A tool call in the rendered transcript.
pub struct ToolRef {
    pub tool_use_id: String,
    /// Index in `AgentOverlayState::lines` of this tool's header row.
    pub line: usize,
    /// One past this tool's last rendered row, excluding any separator.
    /// Clicks anywhere in `line..end` target this tool.
    pub end: usize,
}

/// A thinking block in the rendered transcript.
pub struct ThinkingRef {
    /// Position among the window's thinking cells. Used instead of a cell
    /// index because cells are rebuilt on refresh and the window trims from
    /// the front once a thread exceeds `TRANSCRIPT_MAX_CELLS`.
    pub ordinal: usize,
    /// Index in `AgentOverlayState::lines` of this block's header row.
    pub line: usize,
    /// One past this block's last rendered row, excluding any separator.
    /// Clicks anywhere in `line..end` toggle this block.
    pub end: usize,
}

/// State for the tool detail pane opened from the transcript overlay.
pub struct ToolPaneState {
    /// Tool identity, not a cell index: cells are rebuilt on every refresh, and
    /// the pane re-reads the live cell so running tools keep updating.
    pub tool_use_id: String,
    /// Top-line offset; clamped at render time against the wrapped body.
    pub scroll: usize,
}

pub(crate) fn load_active_agents() -> Vec<ActiveAgentInfo> {
    let flat: Vec<ActiveAgentInfo> = agent_activity::list_active()
        .into_iter()
        .map(|r| {
            let short_thread = r
                .thread_id
                .as_deref()
                .map_or("-", |id| if id.len() > 8 { &id[..8] } else { id })
                .to_string();
            let current_tool = r.current_tools.first().map(|tool| tool.name.clone());
            ActiveAgentInfo {
                pid: r.pid,
                surface: r.surface.unwrap_or_else(|| "-".to_string()),
                thread_id: short_thread,
                full_thread_id: r.thread_id.filter(|id| !id.is_empty()),
                parent_thread_id: r.parent_thread_id.filter(|id| !id.is_empty()),
                tree_prefix: String::new(),
                model: r.model.unwrap_or_else(|| "-".to_string()),
                provider: r.provider.unwrap_or_else(|| "-".to_string()),
                account: r.account,
                thinking: r.thinking.unwrap_or_else(|| "-".to_string()),
                uptime: agent_activity::uptime_since(&r.started_at),
                kind: r.kind,
                subagent_name: r.subagent_name,
                current_tool,
                phase: r.phase,
            }
        })
        .collect();
    arrange_agent_tree(flat)
}

/// Reorders active-agent runs into a parent → child tree (depth-first,
/// preserving the natural start order among siblings) and fills each row's
/// `tree_prefix` with box-drawing connectors. A run nests under another when
/// its `parent_thread_id` matches that run's `full_thread_id`; runs whose
/// parent is absent become top-level roots.
fn arrange_agent_tree(flat: Vec<ActiveAgentInfo>) -> Vec<ActiveAgentInfo> {
    let n = flat.len();
    if n == 0 {
        return flat;
    }

    let mut id_to_idx: HashMap<&str, usize> = HashMap::new();
    for (i, a) in flat.iter().enumerate() {
        if let Some(id) = a.full_thread_id.as_deref() {
            id_to_idx.entry(id).or_insert(i);
        }
    }

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut roots: Vec<usize> = Vec::new();
    for (i, a) in flat.iter().enumerate() {
        let parent = a
            .parent_thread_id
            .as_deref()
            .and_then(|p| id_to_idx.get(p).copied())
            .filter(|&p| p != i);
        match parent {
            Some(p) => children[p].push(i),
            None => roots.push(i),
        }
    }

    let mut order: Vec<usize> = Vec::with_capacity(n);
    let mut prefixes: Vec<String> = vec![String::new(); n];
    let mut visited = vec![false; n];
    for (i, &root) in roots.iter().enumerate() {
        dfs_agent_tree(
            root,
            &children,
            "",
            i + 1 == roots.len(),
            0,
            &mut order,
            &mut prefixes,
            &mut visited,
        );
    }

    let mut slots: Vec<Option<ActiveAgentInfo>> = flat.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(n);
    for idx in order {
        if let Some(mut a) = slots[idx].take() {
            a.tree_prefix = std::mem::take(&mut prefixes[idx]);
            out.push(a);
        }
    }
    // Cycle safety: append any run not reached by the DFS in natural order.
    for slot in &mut slots {
        if let Some(a) = slot.take() {
            out.push(a);
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn dfs_agent_tree(
    idx: usize,
    children: &[Vec<usize>],
    ancestor_prefix: &str,
    is_last: bool,
    depth: usize,
    order: &mut Vec<usize>,
    prefixes: &mut [String],
    visited: &mut [bool],
) {
    if visited[idx] {
        return;
    }
    visited[idx] = true;

    let connector = if depth == 0 {
        ""
    } else if is_last {
        "└─ "
    } else {
        "├─ "
    };
    prefixes[idx] = format!("{ancestor_prefix}{connector}");
    order.push(idx);

    let child_prefix = if depth == 0 {
        String::new()
    } else {
        format!("{ancestor_prefix}{}", if is_last { "   " } else { "│  " })
    };
    let kids = &children[idx];
    for (i, &kid) in kids.iter().enumerate() {
        dfs_agent_tree(
            kid,
            children,
            &child_prefix,
            i + 1 == kids.len(),
            depth + 1,
            order,
            prefixes,
            visited,
        );
    }
}

/// Path to a thread's transcript JSONL.
pub(crate) fn transcript_path(id: &str) -> PathBuf {
    paths::threads_dir().join(format!("{id}.jsonl"))
}

/// Max transcript cells kept for rendering (bounds render size, not I/O).
/// Applied after building cells so `tool_use`/`tool_result` pairs never split.
const TRANSCRIPT_MAX_CELLS: usize = 200;

/// Reads a thread transcript and renders it to formatted ratatui lines using
/// the shared `zdx-transcript` renderer (markdown, wrapping, tool pairing).
/// Also returns the cells, the tool rows, and the thinking rows found in them,
/// so the overlay can drill into a tool call or expand a thinking block.
/// `expanded` re-applies the user's thinking expansions to the fresh cells.
/// Best-effort; a missing file yields no lines.
fn read_thread_transcript(
    id: &str,
    width: usize,
    expanded: &HashSet<usize>,
    running: &[agent_activity::ActiveToolCall],
) -> (
    Vec<zdx_transcript::HistoryCell>,
    Vec<Line<'static>>,
    Vec<ToolRef>,
    Vec<ThinkingRef>,
) {
    let events = thread_persistence::load_thread_events(id).unwrap_or_default();
    let all_cells = zdx_transcript::build_transcript_from_events(&events);
    let start = all_cells.len().saturating_sub(TRANSCRIPT_MAX_CELLS);
    let mut cells = all_cells[start..].to_vec();
    append_running_tool_cells(&mut cells, running);
    apply_thinking_expansion(&mut cells, expanded);
    let (lines, tools, thinking) = render_cells(&cells, width);
    (cells, lines, tools, thinking)
}

/// Appends a synthetic running-tool cell for each in-flight tool from the
/// run's activity marker. Uses the marker's full input when it was stored;
/// otherwise rebuilds a display input as `{primary_key: summary}`. The cell's
/// start time comes from the marker so elapsed time survives rebuilds. Skips
/// ids already persisted (a completed round lands in the JSONL just before
/// the marker entry clears).
fn append_running_tool_cells(
    cells: &mut Vec<zdx_transcript::HistoryCell>,
    running: &[agent_activity::ActiveToolCall],
) {
    for tool in running {
        let already_persisted = cells.iter().any(|cell| {
            matches!(cell, zdx_transcript::HistoryCell::Tool { tool_use_id, .. } if *tool_use_id == tool.id)
        });
        if already_persisted {
            continue;
        }
        let name = tool.name.to_ascii_lowercase();
        let input = if tool.input.is_null() {
            zdx_transcript::primary_input_key(&name)
                .filter(|_| !tool.summary.is_empty())
                .map_or_else(
                    || serde_json::json!({}),
                    |key| serde_json::json!({ key: tool.summary }),
                )
        } else {
            tool.input.clone()
        };
        let mut cell = zdx_transcript::HistoryCell::tool_running(tool.id.clone(), name, input);
        if let zdx_transcript::HistoryCell::Tool {
            started_at,
            output_delta,
            ..
        } = &mut cell
        {
            if let Ok(marker_start) = chrono::DateTime::parse_from_rfc3339(&tool.started_at) {
                *started_at = marker_start.with_timezone(&chrono::Utc);
            }
            if !tool.output_tail.is_empty() {
                *output_delta = Some(tool.output_tail.clone());
            }
        }
        cells.push(cell);
    }
}

/// Marker record for the active run bound to `thread_id`, if any.
fn active_run_for(thread_id: &str) -> Option<agent_activity::RunRecord> {
    if thread_id.is_empty() {
        return None;
    }
    agent_activity::list_active()
        .into_iter()
        .find(|r| r.thread_id.as_deref() == Some(thread_id))
}

/// Applies the tracked expansions to freshly built cells, which always arrive
/// collapsed.
fn apply_thinking_expansion(cells: &mut [zdx_transcript::HistoryCell], expanded: &HashSet<usize>) {
    let mut ordinal = 0usize;
    for cell in cells {
        if let zdx_transcript::HistoryCell::Thinking { is_collapsed, .. } = cell {
            *is_collapsed = !expanded.contains(&ordinal);
            ordinal += 1;
        }
    }
}

/// Renders cells to lines plus the tool and thinking rows they occupy.
fn render_cells(
    cells: &[zdx_transcript::HistoryCell],
    width: usize,
) -> (Vec<Line<'static>>, Vec<ToolRef>, Vec<ThinkingRef>) {
    let (lines, offsets) = zdx_transcript::cells_to_lines_with_offsets(cells, width.max(1));
    let total = lines.len();
    let mut tools = Vec::new();
    let mut thinking = Vec::new();
    for (idx, cell) in cells.iter().enumerate() {
        match cell {
            zdx_transcript::HistoryCell::Tool { tool_use_id, .. } => tools.push(ToolRef {
                tool_use_id: tool_use_id.clone(),
                line: offsets[idx],
                end: cell_end_row(cells, &offsets, total, idx),
            }),
            zdx_transcript::HistoryCell::Thinking { .. } => thinking.push(ThinkingRef {
                ordinal: thinking.len(),
                line: offsets[idx],
                end: cell_end_row(cells, &offsets, total, idx),
            }),
            _ => {}
        }
    }
    (lines, tools, thinking)
}

/// One past the last row belonging to `cells[idx]`, excluding the separator
/// blank lines that follow it. Consecutive tool/thinking cells have no
/// separator at all, so this must come from the shared gap rule.
fn cell_end_row(
    cells: &[zdx_transcript::HistoryCell],
    offsets: &[usize],
    total: usize,
    idx: usize,
) -> usize {
    offsets.get(idx + 1).map_or(total, |next| {
        next.saturating_sub(zdx_transcript::gap_after(&cells[idx], cells.get(idx + 1)))
    })
}

/// Number of visible transcript rows in the full-screen overlay.
pub(crate) fn agent_overlay_page_size(app: &MonitorApp) -> usize {
    (app.terminal_height.saturating_sub(2) as usize).max(1)
}

/// Opens the transcript overlay for the currently selected active agent.
pub(crate) fn open_agent_overlay(app: &mut MonitorApp) {
    let Some(a) = app.active_agents.get(app.selected_index) else {
        return;
    };
    let title = format!("{}:{}@{} {}", a.provider, a.model, a.thinking, a.thread_id);
    let width = app.terminal_width.saturating_sub(2) as usize;
    match a.full_thread_id.clone() {
        Some(id) => {
            let mut state = AgentOverlayState {
                thread_id: id,
                title,
                lines: Vec::new(),
                cells: Vec::new(),
                tools: Vec::new(),
                thinking: Vec::new(),
                expanded_thinking: HashSet::new(),
                tool_selected: None,
                tool_pane: None,
                scroll: None,
                ended: false,
                unavailable: false,
                file_len: 0,
                file_mtime: None,
                running_sig: Vec::new(),
                run_phase: None,
                running_tool: None,
                width,
            };
            load_transcript_into(&mut state);
            app.agent_overlay = Some(state);
        }
        None => {
            app.agent_overlay = Some(AgentOverlayState {
                thread_id: String::new(),
                title,
                lines: vec![Line::from("transcript unavailable (no thread id)")],
                cells: Vec::new(),
                tools: Vec::new(),
                thinking: Vec::new(),
                expanded_thinking: HashSet::new(),
                tool_selected: None,
                tool_pane: None,
                scroll: None,
                ended: false,
                unavailable: true,
                file_len: 0,
                file_mtime: None,
                running_sig: Vec::new(),
                run_phase: None,
                running_tool: None,
                width,
            });
        }
    }
}

/// File length + mtime used to detect transcript changes between ticks.
/// Missing/unreadable file collapses to `(0, None)`.
fn transcript_file_fingerprint(path: &Path) -> (u64, Option<SystemTime>) {
    fs::metadata(path).map_or((0, None), |m| (m.len(), m.modified().ok()))
}

/// (Re)loads the transcript for an open overlay and records file len/mtime.
pub(crate) fn load_transcript_into(state: &mut AgentOverlayState) {
    let path = transcript_path(&state.thread_id);
    let (len, mtime) = transcript_file_fingerprint(&path);
    state.file_len = len;
    state.file_mtime = mtime;
    let run = active_run_for(&state.thread_id);
    state.run_phase = run.as_ref().and_then(|r| r.phase.clone());
    state.running_tool = run
        .as_ref()
        .and_then(|r| r.current_tools.first())
        .map(|t| t.name.to_ascii_lowercase());
    let running = run.map(|r| r.current_tools).unwrap_or_default();
    state.running_sig = running
        .iter()
        .map(|t| format!("{}:{}", t.id, t.output_tail.len()))
        .collect();
    let (cells, lines, tools, thinking) = read_thread_transcript(
        &state.thread_id,
        state.width,
        &state.expanded_thinking,
        &running,
    );
    state.cells = cells;
    state.lines = lines;
    state.tools = tools;
    state.thinking = thinking;
    // A tool trimmed out of the window can no longer be highlighted or shown.
    if state.selected_tool_index().is_none() {
        state.tool_selected = None;
    }
    if let Some(pane) = &state.tool_pane
        && state.tool_cell(&pane.tool_use_id).is_none()
    {
        state.tool_pane = None;
    }
}

/// Timed-tick refresh for the open transcript overlay. Skips reparsing when the
/// file and render width are unchanged. Marks the run ended when its marker
/// disappears, but keeps reading (the final assistant message may still be
/// persisting).
pub(crate) fn refresh_agent_overlay(app: &mut MonitorApp) {
    let width = app.terminal_width.saturating_sub(2) as usize;
    let Some(state) = app.agent_overlay.as_mut() else {
        return;
    };
    if state.unavailable {
        return;
    }
    state.ended = !app
        .active_agents
        .iter()
        .any(|a| a.full_thread_id.as_deref() == Some(state.thread_id.as_str()));

    let path = transcript_path(&state.thread_id);
    let (len, mtime) = transcript_file_fingerprint(&path);
    let run = active_run_for(&state.thread_id);
    state.run_phase = run.as_ref().and_then(|r| r.phase.clone());
    state.running_tool = run
        .as_ref()
        .and_then(|r| r.current_tools.first())
        .map(|t| t.name.to_ascii_lowercase());
    let running_sig: Vec<String> = run
        .map(|r| {
            r.current_tools
                .iter()
                .map(|t| format!("{}:{}", t.id, t.output_tail.len()))
                .collect()
        })
        .unwrap_or_default();
    if len == state.file_len
        && mtime == state.file_mtime
        && width == state.width
        && running_sig == state.running_sig
    {
        return;
    }
    state.file_len = len;
    state.file_mtime = mtime;
    state.width = width;
    load_transcript_into(state);
}

/// Highlights the tool `step` positions away from the current one and scrolls it
/// into view. With nothing highlighted yet, starts at the newest tool.
fn move_agent_overlay_tool(state: &mut AgentOverlayState, step: isize, page: usize) {
    if state.tools.is_empty() {
        return;
    }
    let last = state.tools.len() - 1;
    let next = match state.selected_tool_index() {
        Some(cur) => {
            let cur = cur as isize;
            (cur + step).clamp(0, last as isize) as usize
        }
        None => last,
    };
    let tool = &state.tools[next];
    state.tool_selected = Some(tool.tool_use_id.clone());

    // Keep the highlighted header comfortably inside the viewport.
    let target = tool.line.saturating_sub(page / 3);
    state.scroll = Some(target.min(state.max_scroll(page)));
}

/// Highlights a tool and opens its detail pane.
fn open_tool_pane(state: &mut AgentOverlayState, tool_use_id: String) {
    state.tool_selected = Some(tool_use_id.clone());
    state.tool_pane = Some(ToolPaneState {
        tool_use_id,
        scroll: 0,
    });
}

/// Opens the tool detail pane for the highlighted tool, selecting the newest
/// tool first when nothing is highlighted yet.
fn open_agent_overlay_tool_pane(state: &mut AgentOverlayState, page: usize) {
    if state.selected_tool_index().is_none() {
        move_agent_overlay_tool(state, 0, page);
    }
    let Some(idx) = state.selected_tool_index() else {
        return;
    };
    let tool_use_id = state.tools[idx].tool_use_id.clone();
    open_tool_pane(state, tool_use_id);
}

/// Opens the tool detail pane for the tool under a click row in the
/// full-screen transcript overlay. No-op when the row isn't over a tool.
///
/// The overlay covers the whole frame, so row 0 is its top border and content
/// row `n` is at screen row `n + 1`.
fn open_tool_pane_at_row(state: &mut AgentOverlayState, row: u16, page: usize) {
    let Some(content_row) = row.checked_sub(1).map(usize::from).filter(|r| *r < page) else {
        return;
    };
    let line = state.top_line(page) + content_row;
    let Some(tool) = state.tools.iter().find(|t| (t.line..t.end).contains(&line)) else {
        return;
    };
    let tool_use_id = tool.tool_use_id.clone();
    open_tool_pane(state, tool_use_id);
}

/// Routes a left click in the transcript overlay: a thinking block toggles,
/// otherwise a tool's rows open its detail pane.
pub(crate) fn handle_overlay_click(state: &mut AgentOverlayState, row: u16, page: usize) {
    if toggle_thinking_at_row(state, row, page) {
        return;
    }
    open_tool_pane_at_row(state, row, page);
}

/// Expands or collapses the thinking block under a clicked overlay row.
///
/// Returns `true` when a block was toggled, so the caller can fall through to
/// tool hit-testing otherwise. The clicked block keeps its screen row, since
/// expanding shifts every line below it.
fn toggle_thinking_at_row(state: &mut AgentOverlayState, row: u16, page: usize) -> bool {
    let Some(content_row) = row.checked_sub(1).map(usize::from).filter(|r| *r < page) else {
        return false;
    };
    let line = state.top_line(page) + content_row;
    let Some(block) = state
        .thinking
        .iter()
        .find(|t| (t.line..t.end).contains(&line))
    else {
        return false;
    };

    let ordinal = block.ordinal;
    if !state.expanded_thinking.remove(&ordinal) {
        state.expanded_thinking.insert(ordinal);
    }
    rerender_transcript(state);

    if let Some(block) = state.thinking.iter().find(|t| t.ordinal == ordinal) {
        let anchored = block.line.saturating_sub(content_row);
        state.scroll = Some(anchored.min(state.max_scroll(page)));
    }
    true
}

/// Expands every thinking block, or collapses them all when any are open.
fn toggle_all_thinking(state: &mut AgentOverlayState) {
    if state.expanded_thinking.is_empty() {
        state.expanded_thinking = (0..state.thinking.len()).collect();
    } else {
        state.expanded_thinking.clear();
    }
    rerender_transcript(state);
}

/// Re-renders the overlay from the cells already in memory. Used by the
/// thinking toggles, which change only how existing cells display and must not
/// re-read the transcript file on a keystroke or click.
fn rerender_transcript(state: &mut AgentOverlayState) {
    apply_thinking_expansion(&mut state.cells, &state.expanded_thinking);
    let (lines, tools, thinking) = render_cells(&state.cells, state.width);
    state.lines = lines;
    state.tools = tools;
    state.thinking = thinking;
}

/// Handles a key while the tool detail pane is open. Scroll offsets are clamped
/// at render time, where the wrapped body height is known.
fn handle_tool_pane_key(pane: &mut ToolPaneState, key: KeyCode, page_size: usize) -> bool {
    match key {
        KeyCode::Esc | KeyCode::Char('q') => return false,
        KeyCode::Char('j') | KeyCode::Down => pane.scroll = pane.scroll.saturating_add(1),
        KeyCode::Char('k') | KeyCode::Up => pane.scroll = pane.scroll.saturating_sub(1),
        KeyCode::PageDown => pane.scroll = pane.scroll.saturating_add(page_size),
        KeyCode::PageUp => pane.scroll = pane.scroll.saturating_sub(page_size),
        KeyCode::Home => pane.scroll = 0,
        KeyCode::Char('G') | KeyCode::End => pane.scroll = usize::MAX,
        _ => {}
    }
    true
}

/// Handles a key while the transcript overlay is open.
pub(crate) fn handle_agent_overlay_key(app: &mut MonitorApp, key: KeyCode) {
    if matches!(key, KeyCode::Char('y' | 'Y')) {
        if let Some((text, done)) = agent_overlay_copy_payload(app, key) {
            copy_text(app, &text, &done);
        }
        return;
    }
    let page = agent_overlay_page_size(app);
    let Some(state) = app.agent_overlay.as_mut() else {
        return;
    };
    if let Some(pane) = state.tool_pane.as_mut() {
        if !handle_tool_pane_key(pane, key, page) {
            state.tool_pane = None;
        }
        return;
    }
    let max_offset = state.max_scroll(page);
    // `None` means following the newest content; step from the bottom.
    let cur = state.top_line(page);
    match key {
        KeyCode::Esc | KeyCode::Char('q') => app.agent_overlay = None,
        KeyCode::Tab | KeyCode::Char('n') => move_agent_overlay_tool(state, 1, page),
        KeyCode::BackTab | KeyCode::Char('p') => move_agent_overlay_tool(state, -1, page),
        KeyCode::Enter => open_agent_overlay_tool_pane(state, page),
        KeyCode::Char('t') => toggle_all_thinking(state),
        KeyCode::Char('j') | KeyCode::Down => state.scroll = Some((cur + 1).min(max_offset)),
        KeyCode::Char('k') | KeyCode::Up => state.scroll = Some(cur.saturating_sub(1)),
        KeyCode::PageDown => state.scroll = Some((cur + page).min(max_offset)),
        KeyCode::PageUp => state.scroll = Some(cur.saturating_sub(page)),
        KeyCode::Home => state.scroll = Some(0),
        KeyCode::Char('G') | KeyCode::End => state.scroll = None,
        _ => {}
    }
}

/// Copy payload for the transcript overlay: `y` yields the highlighted (or
/// open) tool's primary command, `Y` its full detail body. `None` when no
/// tool is highlighted.
fn agent_overlay_copy_payload(app: &MonitorApp, key: KeyCode) -> Option<(String, String)> {
    let state = app.agent_overlay.as_ref()?;
    let tool_use_id = state
        .tool_pane
        .as_ref()
        .map(|pane| pane.tool_use_id.clone())
        .or_else(|| state.tool_selected.clone())?;
    let cell = state.tool_cell(&tool_use_id)?;
    match key {
        KeyCode::Char('y') => {
            let zdx_transcript::HistoryCell::Tool { name, input, .. } = cell else {
                return None;
            };
            let text = zdx_transcript::tool_command_text(&name.to_ascii_lowercase(), input);
            (!text.is_empty()).then(|| (text, "Copied command".to_string()))
        }
        KeyCode::Char('Y') => {
            let text = lines_text(&zdx_transcript::tool_detail_body(cell).lines);
            (!text.is_empty()).then(|| (text, "Copied tool detail".to_string()))
        }
        _ => None,
    }
}

pub(crate) fn render_active_agents(f: &mut Frame, app: &MonitorApp, area: Rect) {
    /// Fixed width of the status column after the PID (`⚙ bash`, `◌ waiting`).
    const STATUS_COL: usize = 12;
    if app.active_agents.is_empty() {
        let p = Paragraph::new(" No active agent runs")
            .style(Style::default().fg(Color::DarkGray))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Active Agents"),
            );
        f.render_widget(p, area);
        return;
    }

    let inner_width = area.width.saturating_sub(2) as usize;
    let items: Vec<ListItem> = app
        .active_agents
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let role = a.kind.as_deref().unwrap_or(&a.surface);
            let role_label = if let Some(name) = a.subagent_name.as_deref() {
                format!("{role}:{name}")
            } else {
                role.to_string()
            };
            let (status, status_color) = a.current_tool.as_deref().map_or_else(
                || {
                    (
                        a.phase
                            .as_deref()
                            .map_or_else(String::new, |phase| format!("◌ {phase}")),
                        Color::DarkGray,
                    )
                },
                |tool| {
                    let glyph = zdx_transcript::tool_state_glyph(
                        &zdx_transcript::ToolState::Running,
                        spinner_frame_now(),
                    );
                    (format!("{glyph} {tool}"), Color::Yellow)
                },
            );
            let status = format!("{:<STATUS_COL$}", truncate_chars(&status, STATUS_COL));
            let pre = format!(" {}● PID {} ", a.tree_prefix, a.pid);
            let mid = format!(" {} model:", truncate_chars(&role_label, 18));
            let suffix = format!(" thread:{} up {}", a.thread_id, a.uptime);
            let model_width = inner_width.saturating_sub(
                pre.chars().count() + STATUS_COL + mid.chars().count() + suffix.chars().count(),
            );
            let model_desc = format!(
                "{}:{}@{}",
                zdx_engine::providers::oauth::account_cache_key(&a.provider, a.account.as_deref()),
                a.model,
                a.thinking
            );
            let model = truncate_chars(&model_desc, model_width);
            let (style, status_style) = if i == app.selected_index {
                (
                    Style::default().fg(Color::Green).bg(SELECTED_BG),
                    Style::default().fg(status_color).bg(SELECTED_BG),
                )
            } else {
                (
                    Style::default().fg(Color::Green),
                    Style::default().fg(status_color),
                )
            };
            let line = Line::from(vec![
                Span::styled(pre, style),
                Span::styled(status, status_style),
                Span::styled(format!("{mid}{model:<model_width$}{suffix}"), style),
            ]);
            ListItem::new(line)
        })
        .collect();

    let title = format!("Active Agents ({})", app.active_agents.len());
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title(title));
    f.render_widget(list, area);
}

pub(crate) fn render_agent_overlay(f: &mut Frame, state: &AgentOverlayState, area: Rect) {
    f.render_widget(Clear, area);

    let status = if state.unavailable {
        String::new()
    } else if state.ended {
        " · ENDED".to_string()
    } else if let Some(tool) = state.running_tool.as_deref() {
        let glyph = zdx_transcript::tool_state_glyph(
            &zdx_transcript::ToolState::Running,
            spinner_frame_now(),
        );
        format!(" · {glyph} {}…", tool.to_ascii_uppercase())
    } else if let Some(phase) = state.run_phase.as_deref() {
        format!(" · {}…", phase.to_ascii_uppercase())
    } else if state.scroll.is_none() {
        " · FOLLOW".to_string()
    } else {
        String::new()
    };
    let mut hints = String::from(" · ");
    if !state.tools.is_empty() {
        hints.push_str("click/Tab tool · Enter detail · y copy · ");
    }
    if !state.thinking.is_empty() {
        hints.push_str("click/t thinking · ");
    }
    hints.push_str("Esc close ");
    let title = format!(" {}{status}{hints}", state.title);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);

    let total = state.lines.len();
    if total == 0 {
        let p = Paragraph::new(" No transcript yet for this run.")
            .style(Style::default().fg(Color::DarkGray))
            .block(block);
        f.render_widget(p, area);
        return;
    }

    let visible_rows = area.height.saturating_sub(2) as usize;
    let offset = state.top_line(visible_rows);
    let end = (offset + visible_rows).min(total);

    let selected_line = state.selected_tool_index().map(|idx| state.tools[idx].line);

    let items: Vec<ListItem> = state.lines[offset..end]
        .iter()
        .enumerate()
        .map(|(row, line)| {
            let item = ListItem::new(line.clone());
            if selected_line == Some(offset + row) {
                item.style(Style::default().bg(SELECTED_BG))
            } else {
                item
            }
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);

    if let Some(pane) = &state.tool_pane {
        render_tool_pane(f, state, pane, area);
    }
}

/// Tool detail pane: the same body the chat TUI's tool popup shows, rendered
/// read-only over the transcript overlay.
fn render_tool_pane(f: &mut Frame, state: &AgentOverlayState, pane: &ToolPaneState, area: Rect) {
    let popup = centered_rect(88, 88, area);
    f.render_widget(Clear, popup);

    let Some(cell) = state.tool_cell(&pane.tool_use_id) else {
        // Panes for vanished tools are dropped on refresh, so a miss here only
        // means the transcript window moved mid-frame; nothing useful to draw.
        return;
    };
    let (name, glyph, color) = match cell {
        zdx_transcript::HistoryCell::Tool { name, state, .. } => (
            name.as_str(),
            zdx_transcript::tool_state_glyph(state, spinner_frame_now()),
            zdx_transcript::tool_state_color(state),
        ),
        _ => return,
    };
    let body = zdx_transcript::tool_detail_body(cell).lines;

    let visible_rows = popup.height.saturating_sub(2) as usize;
    let inner_width = popup.width.saturating_sub(2) as usize;
    // Pre-wrap so the scroll offset counts rendered rows, not logical lines.
    let wrapped: Vec<Line<'static>> = body
        .iter()
        .flat_map(|line| zdx_transcript::wrap_line_to_width(line, inner_width.max(1)))
        .collect();
    let max_offset = wrapped.len().saturating_sub(visible_rows);
    let offset = pane.scroll.min(max_offset);
    let end = (offset + visible_rows).min(wrapped.len());

    let position = if wrapped.len() > visible_rows {
        format!(" [{}/{}]", offset + 1, wrapped.len())
    } else {
        String::new()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(format!(" {glyph} {name} "))
        .title_bottom(format!(
            " j/k scroll · gg/G top/bottom · y cmd · Y all · Esc back{position} "
        ));

    let items: Vec<ListItem> = wrapped[offset..end]
        .iter()
        .map(|line| ListItem::new(line.clone()))
        .collect();
    f.render_widget(List::new(items).block(block), popup);
}

#[cfg(test)]
mod transcript_tests {
    use zdx_engine::core::thread_persistence::ThreadEvent;

    use super::*;
    use crate::tabs::threads::{TimingOverlayState, timing_overlay_from_events};

    fn parse(lines: &[&str]) -> Vec<ThreadEvent> {
        lines
            .iter()
            .filter_map(|l| serde_json::from_str::<ThreadEvent>(l).ok())
            .collect()
    }

    fn line_text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn lines_for(cells: &[zdx_transcript::HistoryCell], width: usize) -> Vec<Line<'static>> {
        zdx_transcript::cells_to_lines_with_offsets(cells, width).0
    }

    fn render(events: &[ThreadEvent], width: usize) -> Vec<String> {
        let cells = zdx_transcript::build_transcript_from_events(events);
        lines_for(&cells, width).iter().map(line_text).collect()
    }

    #[test]
    fn renders_formatted_transcript_with_paired_tools_and_notice() {
        let events = parse(&[
            r#"{"type":"meta","schema_version":1,"ts":"t"}"#,
            r#"{"type":"message","role":"user","text":"hello there","ts":"t"}"#,
            r#"{"type":"message","role":"assistant","text":"**bold** answer","ts":"t"}"#,
            r#"{"type":"tool_use","id":"t1","name":"grep","input":{"pattern":"foo"},"ts":"t"}"#,
            r#"{"type":"tool_result","tool_use_id":"t1","output":"match","ok":true,"ts":"t"}"#,
            r#"{"type":"notice","kind":"refusal","message":"heads up","ts":"t"}"#,
            r#"{"type":"usage","input_tokens":1,"output_tokens":2,"cache_read_tokens":0,"cache_write_tokens":0,"ts":"t"}"#,
            r"{ malformed line",
        ]);
        let lines = render(&events, 80);
        let joined = lines.join("\n");

        assert!(
            joined.contains("hello there"),
            "user text present: {joined}"
        );
        assert!(
            joined.contains("answer"),
            "assistant text present: {joined}"
        );
        assert!(joined.contains("grep"), "tool name present: {joined}");
        assert!(joined.contains("heads up"), "notice present: {joined}");
        // Markdown bold markers are consumed, not rendered literally.
        assert!(!joined.contains("**bold**"), "markdown parsed: {joined}");
    }

    #[test]
    fn inserts_blank_line_between_cells() {
        let events = parse(&[
            r#"{"type":"message","role":"user","text":"one","ts":"t"}"#,
            r#"{"type":"message","role":"assistant","text":"two","ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let lines = lines_for(&cells, 80);
        let blanks = lines.iter().filter(|l| line_text(l).is_empty()).count();
        assert!(
            blanks >= cells.len(),
            "one blank separator per cell: blanks={blanks} cells={}",
            cells.len()
        );
    }

    /// Tool rows must map to the exact line their header renders on, since the
    /// overlay highlights that line and drills in from it.
    #[test]
    fn tool_rows_map_to_their_header_lines_and_body_shows_args_and_output() {
        let events = parse(&[
            r#"{"type":"message","role":"user","text":"hi","ts":"t"}"#,
            r#"{"type":"tool_use","id":"t1","name":"grep","input":{"pattern":"needle"},"ts":"t"}"#,
            r#"{"type":"tool_result","tool_use_id":"t1","output":"a match","ok":true,"ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let (lines, offsets) = zdx_transcript::cells_to_lines_with_offsets(&cells, 80);

        let (tool_idx, tool_cell) = cells
            .iter()
            .enumerate()
            .find(|(_, c)| matches!(c, zdx_transcript::HistoryCell::Tool { .. }))
            .expect("tool cell");
        let header = line_text(&lines[offsets[tool_idx]]);
        assert!(
            header.contains("grep"),
            "header line is the tool row: {header}"
        );

        let body = zdx_transcript::tool_detail_body(tool_cell);
        let text: String = body
            .lines
            .iter()
            .map(line_text)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("needle"), "args shown: {text}");
        assert!(text.contains("a match"), "output shown: {text}");
        assert!(
            body.output_start > 0 && body.output_start <= body.lines.len(),
            "output_start points into the body: {}",
            body.output_start
        );
    }

    /// Clicking a tool's rows opens its pane; the overlay's top border must not
    /// be counted as content, and non-tool rows must be ignored.
    #[test]
    fn click_row_opens_the_tool_under_the_cursor() {
        let events = parse(&[
            r#"{"type":"message","role":"user","text":"hi","ts":"t"}"#,
            r#"{"type":"tool_use","id":"t1","name":"grep","input":{"pattern":"needle"},"ts":"t"}"#,
            r#"{"type":"tool_result","tool_use_id":"t1","output":"a match","ok":true,"ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let (lines, offsets) = zdx_transcript::cells_to_lines_with_offsets(&cells, 80);
        let tool_idx = cells
            .iter()
            .position(|c| matches!(c, zdx_transcript::HistoryCell::Tool { .. }))
            .expect("tool cell");
        let tool_line = offsets[tool_idx];

        let mut state = AgentOverlayState {
            thread_id: "t".into(),
            title: String::new(),
            lines,
            cells,
            tools: vec![ToolRef {
                tool_use_id: "t1".into(),
                line: tool_line,
                end: tool_line + 1,
            }],
            thinking: Vec::new(),
            expanded_thinking: HashSet::new(),
            tool_selected: None,
            tool_pane: None,
            scroll: Some(0),
            ended: false,
            unavailable: false,
            file_len: 0,
            file_mtime: None,
            running_sig: Vec::new(),
            run_phase: None,
            running_tool: None,
            width: 80,
        };
        let page = 40;

        // Screen row 0 is the border, so the tool's row is `tool_line + 1`.
        open_tool_pane_at_row(&mut state, u16::try_from(tool_line + 1).unwrap(), page);
        assert_eq!(
            state.tool_pane.as_ref().map(|p| p.tool_use_id.as_str()),
            Some("t1"),
            "click on the tool row opens its pane"
        );
        assert_eq!(state.tool_selected.as_deref(), Some("t1"));

        // A row that is not part of any tool must not open anything.
        state.tool_pane = None;
        open_tool_pane_at_row(&mut state, 0, page);
        assert!(
            state.tool_pane.is_none(),
            "clicking the border opens nothing"
        );
    }

    /// Consecutive tool cells render with no blank separator between them, so a
    /// tool's clickable rows must come from the shared gap rule rather than
    /// assuming one trailing blank line.
    #[test]
    fn adjacent_tool_rows_stay_clickable() {
        let events = parse(&[
            r#"{"type":"tool_use","id":"t1","name":"grep","input":{"pattern":"one"},"ts":"t"}"#,
            r#"{"type":"tool_result","tool_use_id":"t1","output":"a","ok":true,"ts":"t"}"#,
            r#"{"type":"tool_use","id":"t2","name":"read","input":{"file_path":"b.rs"},"ts":"t"}"#,
            r#"{"type":"tool_result","tool_use_id":"t2","output":"b","ok":true,"ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let (_, tools, _) = render_cells(&cells, 80);

        assert_eq!(tools.len(), 2);
        for tool in &tools {
            assert!(
                tool.end > tool.line,
                "tool {} has no clickable rows ({}..{})",
                tool.tool_use_id,
                tool.line,
                tool.end
            );
        }
    }

    /// Clicking a thinking header expands it in place; clicking again collapses
    /// it, and the block keeps the screen row it was clicked on.
    #[test]
    fn click_toggles_a_thinking_block() {
        let events = parse(&[
            r#"{"type":"message","role":"user","text":"hi","ts":"t"}"#,
            r#"{"type":"reasoning","text":"Weighing the options\n\nThe gap rule is the cheaper fix.","ts":"t"}"#,
            r#"{"type":"message","role":"assistant","text":"done","ts":"t"}"#,
        ]);
        let mut cells = zdx_transcript::build_transcript_from_events(&events);
        let expanded = HashSet::new();
        apply_thinking_expansion(&mut cells, &expanded);
        let (lines, tools, thinking) = render_cells(&cells, 80);

        assert_eq!(thinking.len(), 1, "one thinking block");
        let header = thinking[0].line;
        let collapsed_total = lines.len();

        let mut state = AgentOverlayState {
            thread_id: "t".into(),
            title: String::new(),
            lines,
            cells,
            tools,
            thinking,
            expanded_thinking: expanded,
            tool_selected: None,
            tool_pane: None,
            scroll: Some(0),
            ended: false,
            unavailable: false,
            file_len: 0,
            file_mtime: None,
            running_sig: Vec::new(),
            run_phase: None,
            running_tool: None,
            width: 80,
        };
        let page = 40;

        // Screen row 0 is the border, so the header sits at `header + 1`.
        assert!(toggle_thinking_at_row(
            &mut state,
            u16::try_from(header + 1).unwrap(),
            page
        ));
        assert_eq!(state.expanded_thinking.len(), 1);
        assert!(
            state.lines.len() > collapsed_total,
            "expanding adds rows: {} -> {}",
            collapsed_total,
            state.lines.len()
        );

        assert!(toggle_thinking_at_row(
            &mut state,
            u16::try_from(header + 1).unwrap(),
            page
        ));
        assert!(state.expanded_thinking.is_empty());
        assert_eq!(
            state.lines.len(),
            collapsed_total,
            "collapsing restores the original height"
        );

        // A row outside any thinking block is left for tool hit-testing.
        assert!(!toggle_thinking_at_row(&mut state, 0, page));
    }

    /// `t` expands every block at once, then collapses them all.
    #[test]
    fn t_key_toggles_all_thinking_blocks() {
        let events = parse(&[
            r#"{"type":"reasoning","text":"First pass\n\nCheck the renderer.","ts":"t"}"#,
            r#"{"type":"message","role":"assistant","text":"mid","ts":"t"}"#,
            r#"{"type":"reasoning","text":"Second pass\n\nCheck the monitor.","ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let (lines, tools, thinking) = render_cells(&cells, 80);
        let collapsed_total = lines.len();

        let mut state = AgentOverlayState {
            thread_id: "t".into(),
            title: String::new(),
            lines,
            cells,
            tools,
            thinking,
            expanded_thinking: HashSet::new(),
            tool_selected: None,
            tool_pane: None,
            scroll: Some(0),
            ended: false,
            unavailable: false,
            file_len: 0,
            file_mtime: None,
            running_sig: Vec::new(),
            run_phase: None,
            running_tool: None,
            width: 80,
        };

        toggle_all_thinking(&mut state);
        assert_eq!(state.expanded_thinking.len(), 2, "both blocks expanded");
        assert!(state.lines.len() > collapsed_total);

        toggle_all_thinking(&mut state);
        assert!(state.expanded_thinking.is_empty());
        assert_eq!(state.lines.len(), collapsed_total);
    }

    #[test]
    fn narrower_width_wraps_into_more_lines() {
        let events = parse(&[
            r#"{"type":"message","role":"assistant","text":"the quick brown fox jumps over the lazy dog again and again","ts":"t"}"#,
        ]);
        let cells = zdx_transcript::build_transcript_from_events(&events);
        let wide = lines_for(&cells, 100).len();
        let narrow = lines_for(&cells, 20).len();
        assert!(narrow > wide, "narrow={narrow} wide={wide}");
    }

    #[test]
    fn arrange_agent_tree_nests_children_with_connectors() {
        fn agent(thread: &str, parent: Option<&str>) -> ActiveAgentInfo {
            ActiveAgentInfo {
                pid: 0,
                surface: "exec".to_string(),
                thread_id: thread.to_string(),
                full_thread_id: Some(thread.to_string()),
                parent_thread_id: parent.map(str::to_string),
                tree_prefix: String::new(),
                model: "-".to_string(),
                provider: "-".to_string(),
                account: None,
                thinking: "-".to_string(),
                uptime: "0s".to_string(),
                kind: None,
                subagent_name: None,
                current_tool: None,
                phase: None,
            }
        }

        let flat = vec![
            agent("aaa", None),
            agent("bbb", Some("aaa")),
            agent("ccc", Some("aaa")),
            agent("ddd", Some("bbb")),
            agent("eee", None),
        ];

        let out = arrange_agent_tree(flat);
        let got: Vec<(&str, &str)> = out
            .iter()
            .map(|a| (a.thread_id.as_str(), a.tree_prefix.as_str()))
            .collect();

        assert_eq!(
            got,
            vec![
                ("aaa", ""),
                ("bbb", "├─ "),
                ("ddd", "│  └─ "),
                ("ccc", "└─ "),
                ("eee", ""),
            ]
        );
    }

    #[test]
    fn arrange_agent_tree_orphan_parent_becomes_root() {
        fn agent(thread: &str, parent: Option<&str>) -> ActiveAgentInfo {
            ActiveAgentInfo {
                pid: 0,
                surface: "exec".to_string(),
                thread_id: thread.to_string(),
                full_thread_id: Some(thread.to_string()),
                parent_thread_id: parent.map(str::to_string),
                tree_prefix: String::new(),
                model: "-".to_string(),
                provider: "-".to_string(),
                account: None,
                thinking: "-".to_string(),
                uptime: "0s".to_string(),
                kind: None,
                subagent_name: None,
                current_tool: None,
                phase: None,
            }
        }

        // Parent "zzz" is not present among the runs → child is a root.
        let out = arrange_agent_tree(vec![agent("bbb", Some("zzz"))]);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].thread_id, "bbb");
        assert_eq!(out[0].tree_prefix, "");
    }

    #[test]
    fn timing_overlay_uses_shared_report_and_legacy_state() {
        let events = vec![
            thread_persistence::ThreadEvent::user_message("hello"),
            thread_persistence::ThreadEvent::tool_use("t1", "read", serde_json::json!({})),
            thread_persistence::ThreadEvent::tool_result("t1", serde_json::json!({}), true),
        ];
        let state = timing_overlay_from_events("thread-1", Some("Demo"), &events);

        assert_eq!(state.title, "thread-1 · Demo");
        assert!(state.lines.iter().any(|line| line.contains("read · ok")));
        assert!(
            state
                .lines
                .iter()
                .any(|line| line.contains("unavailable (0/1 measured)"))
        );
    }

    #[test]
    fn timing_overlay_navigation_clamps_and_closes() {
        let mut state = TimingOverlayState {
            title: "thread".to_string(),
            lines: (0..20).map(|n| n.to_string()).collect(),
            scroll: 0,
        };
        assert!(!state.handle_key(KeyCode::Char('G'), 5));
        assert_eq!(state.scroll, 15);
        assert!(!state.handle_key(KeyCode::PageUp, 5));
        assert_eq!(state.scroll, 10);
        assert!(!state.handle_key(KeyCode::Home, 5));
        assert_eq!(state.scroll, 0);
        assert!(state.handle_key(KeyCode::Esc, 5));
    }
}
