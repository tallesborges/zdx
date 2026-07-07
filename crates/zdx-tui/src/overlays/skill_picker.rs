#![allow(
    clippy::match_wildcard_for_single_variants,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::unnecessary_wraps
)]

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Wrap};

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::mutations::StateMutation;
use crate::state::TuiState;

#[derive(Debug, Clone)]
pub struct SkillItem {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
    /// Source label for locally-loaded skills (e.g. "zdx-user"). `None` for
    /// remote/installable skills fetched from a repository.
    pub source: Option<String>,
}

/// Which view the skill picker is showing.
#[derive(Debug)]
enum SkillView {
    /// Currently-loaded skills in the active thread (default).
    Loaded,
    /// Install picker: remote skills listed per repo.
    List,
    /// Detail view for a selected skill.
    Detail {
        skill: SkillItem,
        instructions: DetailContent,
        scroll: u16,
        /// True if reached from the install `List` view; false from `Loaded`.
        from_install: bool,
    },
}

/// Content state for the detail view instructions.
#[derive(Debug)]
enum DetailContent {
    Loading,
    Loaded(String),
    Failed(String),
}

#[derive(Debug)]
pub struct SkillPickerState {
    repos: Vec<String>,
    selected_repo: usize,
    filter: String,
    selected: usize,
    skills_by_repo: HashMap<String, Vec<SkillItem>>,
    loaded_skills: Vec<SkillItem>,
    loading_repo: Option<String>,
    installing_skill: Option<String>,
    error: Option<String>,
    installed: HashSet<String>,
    view: SkillView,
}

impl SkillPickerState {
    pub fn open(
        loaded_skills: Vec<zdx_engine::skills::Skill>,
        repos: Vec<String>,
        last_repo: Option<&str>,
    ) -> (Self, Vec<UiEffect>) {
        let installed = load_installed_skills();
        let repos: Vec<String> = repos
            .into_iter()
            .filter(|repo| !repo.trim().is_empty())
            .collect();
        let selected_repo = last_repo
            .and_then(|repo| repos.iter().position(|r| r == repo))
            .unwrap_or(0);

        let loaded_items: Vec<SkillItem> = loaded_skills
            .into_iter()
            .map(|s| SkillItem {
                name: s.name,
                path: s.file_path.display().to_string(),
                description: Some(s.description).filter(|d| !d.is_empty()),
                source: Some(s.source.as_str().to_string()),
            })
            .collect();

        let state = Self {
            repos,
            selected_repo,
            filter: String::new(),
            selected: 0,
            skills_by_repo: HashMap::new(),
            loaded_skills: loaded_items,
            loading_repo: None,
            installing_skill: None,
            error: None,
            installed,
            view: SkillView::Loaded,
        };

        // Defer fetching remote skills until the user enters the install view.
        (state, vec![])
    }

    pub fn current_repo(&self) -> Option<&str> {
        self.repos.get(self.selected_repo).map(String::as_str)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        match &self.view {
            SkillView::Loaded => render_skill_loaded(frame, self, area, input_y),
            SkillView::List => render_skill_list(frame, self, area, input_y),
            SkillView::Detail {
                skill,
                instructions,
                scroll,
                from_install,
            } => render_skill_detail(
                frame,
                self,
                skill,
                instructions,
                *scroll,
                *from_install,
                area,
                input_y,
            ),
        }
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
        match &self.view {
            SkillView::Loaded => self.handle_loaded_key(key),
            SkillView::List => self.handle_list_key(key),
            SkillView::Detail { .. } => self.handle_detail_key(key),
        }
    }

    fn handle_loaded_key(&mut self, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close()
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                let count = self.filtered_loaded().len();
                if count > 0 && self.selected < count.saturating_sub(1) {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter => self.open_loaded_detail(),
            KeyCode::Tab => self.cycle_view(true),
            KeyCode::BackTab => self.cycle_view(false),
            KeyCode::Char('u') if ctrl => {
                self.filter.clear();
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Backspace => {
                if alt {
                    super::render_utils::clear_word_left(&mut self.filter);
                } else {
                    self.filter.pop();
                }
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    /// Cycle through views: Loaded → Install(repo 0) → Install(repo 1) → … → Loaded.
    fn cycle_view(&mut self, forward: bool) -> OverlayUpdate {
        // Total positions: 1 (Loaded) + repos.len(). If no repos, Tab is a no-op.
        let repo_count = self.repos.len();
        if repo_count == 0 {
            return OverlayUpdate::stay();
        }

        let positions = 1 + repo_count;
        let current = match self.view {
            SkillView::List => 1 + self.selected_repo,
            // Detail shouldn't reach here; treat as Loaded.
            SkillView::Loaded | SkillView::Detail { .. } => 0,
        };

        let next = if forward {
            (current + 1) % positions
        } else {
            (current + positions - 1) % positions
        };

        self.filter.clear();
        self.selected = 0;
        self.error = None;
        self.loading_repo = None;
        self.clamp_selection();

        if next == 0 {
            self.view = SkillView::Loaded;
            return OverlayUpdate::stay();
        }

        self.selected_repo = next - 1;
        self.view = SkillView::List;

        let Some(repo) = self.current_repo().map(ToString::to_string) else {
            return OverlayUpdate::stay();
        };

        let mut effects = Vec::new();
        if !self.skills_by_repo.contains_key(&repo) {
            self.loading_repo = Some(repo.clone());
            effects.push(UiEffect::FetchSkillsList { repo: repo.clone() });
        }

        OverlayUpdate::stay()
            .with_ui_effects(effects)
            .with_mutations(vec![StateMutation::SetLastSkillRepo(repo)])
    }

    fn open_loaded_detail(&mut self) -> OverlayUpdate {
        let filtered = self.filtered_loaded();
        let Some(skill) = filtered.get(self.selected).map(|s| (*s).clone()) else {
            return OverlayUpdate::stay();
        };

        let instructions = match std::fs::read_to_string(&skill.path) {
            Ok(content) => DetailContent::Loaded(content),
            Err(err) => DetailContent::Failed(format!("Failed to read SKILL.md: {err}")),
        };

        self.view = SkillView::Detail {
            skill,
            instructions,
            scroll: 0,
            from_install: false,
        };
        OverlayUpdate::stay()
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        if self.installing_skill.is_some() {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    OverlayUpdate::close()
                }
                _ => OverlayUpdate::stay(),
            };
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                OverlayUpdate::close()
            }
            KeyCode::Up => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down => {
                let count = self.filtered_skills().len();
                if count > 0 && self.selected < count.saturating_sub(1) {
                    self.selected += 1;
                }
                OverlayUpdate::stay()
            }
            KeyCode::Tab => self.cycle_view(true),
            KeyCode::BackTab => self.cycle_view(false),
            KeyCode::Enter => self.open_detail(),
            KeyCode::Char('u') if ctrl && !shift && !alt => {
                self.filter.clear();
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Backspace => {
                if alt {
                    super::render_utils::clear_word_left(&mut self.filter);
                } else {
                    self.filter.pop();
                }
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Char(c) if !ctrl => {
                self.filter.push(c);
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            _ => OverlayUpdate::stay(),
        }
    }

    fn handle_detail_key(&mut self, key: KeyEvent) -> OverlayUpdate {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        if self.installing_skill.is_some() {
            return match key.code {
                KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                    OverlayUpdate::close()
                }
                _ => OverlayUpdate::stay(),
            };
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('c') if key.code == KeyCode::Esc || ctrl => {
                let from_install = matches!(
                    &self.view,
                    SkillView::Detail {
                        from_install: true,
                        ..
                    }
                );
                self.view = if from_install {
                    SkillView::List
                } else {
                    SkillView::Loaded
                };
                self.error = None;
                OverlayUpdate::stay()
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let SkillView::Detail { scroll, .. } = &mut self.view {
                    *scroll = scroll.saturating_sub(1);
                }
                OverlayUpdate::stay()
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let SkillView::Detail { scroll, .. } = &mut self.view {
                    *scroll = scroll.saturating_add(1);
                }
                OverlayUpdate::stay()
            }
            KeyCode::Enter => self.install_from_detail(),
            _ => OverlayUpdate::stay(),
        }
    }

    pub fn set_skills(&mut self, repo: &str, skills: Vec<SkillItem>) {
        self.skills_by_repo.insert(repo.to_string(), skills);
        if self.current_repo() == Some(repo) {
            self.loading_repo = None;
            self.error = None;
            self.selected = 0;
            self.clamp_selection();
        }
    }

    pub fn set_error(&mut self, repo: &str, error: String) {
        if self.current_repo() == Some(repo) {
            self.loading_repo = None;
            self.error = Some(error);
            self.clamp_selection();
        }
    }

    pub fn set_installing(&mut self, skill_name: Option<String>) {
        self.installing_skill = skill_name;
    }

    pub fn mark_installed(&mut self, skill_name: &str) {
        self.installed.insert(normalize_skill_name(skill_name));
    }

    pub fn set_instructions(&mut self, skill_path: &str, content: String) {
        if let SkillView::Detail {
            skill,
            instructions,
            ..
        } = &mut self.view
            && skill.path == skill_path
        {
            *instructions = DetailContent::Loaded(content);
        }
    }

    pub fn set_instructions_error(&mut self, skill_path: &str, error: String) {
        if let SkillView::Detail {
            skill,
            instructions,
            ..
        } = &mut self.view
            && skill.path == skill_path
        {
            *instructions = DetailContent::Failed(error);
        }
    }

    fn open_detail(&mut self) -> OverlayUpdate {
        let Some(repo) = self.current_repo().map(ToString::to_string) else {
            return OverlayUpdate::close();
        };

        if self.loading_repo.is_some() {
            return OverlayUpdate::stay();
        }

        let filtered = self.filtered_skills();
        let Some(skill) = filtered.get(self.selected) else {
            return OverlayUpdate::close();
        };
        let skill = (*skill).clone();

        let skill_path = skill.path.clone();
        self.view = SkillView::Detail {
            skill,
            instructions: DetailContent::Loading,
            scroll: 0,
            from_install: true,
        };

        OverlayUpdate::stay()
            .with_ui_effects(vec![UiEffect::FetchSkillInstructions { repo, skill_path }])
    }

    fn install_from_detail(&mut self) -> OverlayUpdate {
        let Some(repo) = self.current_repo().map(ToString::to_string) else {
            return OverlayUpdate::close();
        };

        let (skill, from_install) = match &self.view {
            SkillView::Detail {
                skill,
                from_install,
                ..
            } => (skill.clone(), *from_install),
            _ => return OverlayUpdate::stay(),
        };

        if !from_install {
            return OverlayUpdate::stay();
        }

        if self.is_installed(&skill) {
            self.error = Some("Skill already installed.".to_string());
            return OverlayUpdate::stay();
        }

        let skill_name = skill.name.clone();
        self.installing_skill = Some(skill_name);

        OverlayUpdate::stay().with_ui_effects(vec![UiEffect::InstallSkill {
            repo,
            skill_path: skill.path.clone(),
        }])
    }

    fn filtered_skills(&self) -> Vec<&SkillItem> {
        let Some(repo) = self.current_repo() else {
            return Vec::new();
        };

        let skills = self.skills_by_repo.get(repo).map_or(&[][..], Vec::as_slice);

        if self.filter.is_empty() {
            return skills.iter().collect();
        }

        let filter = self.filter.to_lowercase();
        skills
            .iter()
            .filter(|skill| {
                skill.name.to_lowercase().contains(&filter)
                    || skill
                        .description
                        .as_ref()
                        .is_some_and(|desc| desc.to_lowercase().contains(&filter))
            })
            .collect()
    }

    fn clamp_selection(&mut self) {
        let count = match self.view {
            SkillView::Loaded => self.filtered_loaded().len(),
            _ => self.filtered_skills().len(),
        };
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    fn filtered_loaded(&self) -> Vec<&SkillItem> {
        if self.filter.is_empty() {
            return self.loaded_skills.iter().collect();
        }
        let filter = self.filter.to_lowercase();
        self.loaded_skills
            .iter()
            .filter(|skill| {
                skill.name.to_lowercase().contains(&filter)
                    || skill
                        .description
                        .as_ref()
                        .is_some_and(|desc| desc.to_lowercase().contains(&filter))
                    || skill
                        .source
                        .as_ref()
                        .is_some_and(|src| src.to_lowercase().contains(&filter))
            })
            .collect()
    }

    fn is_installed(&self, skill: &SkillItem) -> bool {
        self.installed.contains(&normalize_skill_name(&skill.name))
    }
}

// =============================================================================
// List view rendering
// =============================================================================

fn render_skill_list(frame: &mut Frame, picker: &SkillPickerState, area: Rect, input_top_y: u16) {
    use super::render_utils::{
        InputHint, InputLine, OverlayConfig, render_input_line, render_overlay, render_separator,
    };

    let filtered = picker.filtered_skills();

    let max_width = area.width.saturating_sub(4);
    let picker_width = max_width.clamp(40, 90);
    let picker_height = (filtered.len() as u16 + 9).max(9);

    let mut hints = vec![
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Enter", "details"),
        InputHint::new("Esc", "cancel"),
    ];
    let tab_label = if picker.repos.len() > 1 {
        "next repo / loaded"
    } else {
        "loaded"
    };
    hints.insert(2, InputHint::new("Tab", tab_label));

    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "Install Skill",
            border_color: Color::Magenta,
            width: picker_width,
            height: picker_height,
            hints: &hints,
        },
    );

    let filter_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, 1);
    render_input_line(
        frame,
        filter_area,
        &InputLine {
            value: &picker.filter,
            placeholder: Some("Filter skills"),
            prompt: "> ",
            prompt_color: Color::DarkGray,
            text_color: Color::Magenta,
            placeholder_color: Color::DarkGray,
            cursor_color: Color::Magenta,
        },
    );

    render_separator(frame, layout.body, 1);

    let repo_label = format_repo_label(picker, layout.body.width as usize);
    let repo_area = Rect::new(layout.body.x, layout.body.y + 2, layout.body.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            repo_label,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center),
        repo_area,
    );

    render_separator(frame, layout.body, 3);

    let list_height = layout.body.height.saturating_sub(6);
    let list_area = Rect::new(
        layout.body.x,
        layout.body.y + 4,
        layout.body.width,
        list_height,
    );

    let items: Vec<ListItem> = if picker.loading_repo.is_some() {
        vec![ListItem::new(Line::from(Span::styled(
            "  Loading skills...",
            Style::default().fg(Color::DarkGray),
        )))]
    } else if filtered.is_empty() {
        let label = if picker.filter.is_empty() {
            "  No skills found"
        } else {
            "  No matches"
        };
        vec![ListItem::new(Line::from(Span::styled(
            label,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        let line_width = list_area.width.saturating_sub(2);
        filtered
            .iter()
            .map(|skill| ListItem::new(skill_line(picker, skill, line_width)))
            .collect()
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !filtered.is_empty() && picker.loading_repo.is_none() {
        list_state.select(Some(picker.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, 4 + list_height);

    let status_area = Rect::new(
        layout.body.x,
        layout.body.y + 5 + list_height,
        layout.body.width,
        1,
    );
    let status_line = list_status_text(picker);
    if let Some(line) = status_line {
        frame.render_widget(
            Paragraph::new(line).alignment(Alignment::Center),
            status_area,
        );
    }
}

// =============================================================================
// Loaded view rendering
// =============================================================================

fn render_skill_loaded(frame: &mut Frame, picker: &SkillPickerState, area: Rect, input_top_y: u16) {
    use super::render_utils::{
        InputHint, InputLine, OverlayConfig, render_input_line, render_overlay, render_separator,
    };

    let filtered = picker.filtered_loaded();

    let max_width = area.width.saturating_sub(4);
    let picker_width = max_width.clamp(40, 90);
    let picker_height = (filtered.len() as u16 + 9).max(9);

    let mut hints = vec![
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Enter", "details"),
    ];
    if !picker.repos.is_empty() {
        hints.push(InputHint::new("Tab", "install"));
    }
    hints.push(InputHint::new("Esc", "close"));

    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "Loaded Skills",
            border_color: Color::Magenta,
            width: picker_width,
            height: picker_height,
            hints: &hints,
        },
    );

    let filter_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, 1);
    render_input_line(
        frame,
        filter_area,
        &InputLine {
            value: &picker.filter,
            placeholder: Some("Filter loaded skills"),
            prompt: "> ",
            prompt_color: Color::DarkGray,
            text_color: Color::Magenta,
            placeholder_color: Color::DarkGray,
            cursor_color: Color::Magenta,
        },
    );

    render_separator(frame, layout.body, 1);

    let summary = format!(
        "{} skill{} loaded into this thread",
        picker.loaded_skills.len(),
        if picker.loaded_skills.len() == 1 {
            ""
        } else {
            "s"
        }
    );
    let summary_area = Rect::new(layout.body.x, layout.body.y + 2, layout.body.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            summary,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Center),
        summary_area,
    );

    render_separator(frame, layout.body, 3);

    let list_height = layout.body.height.saturating_sub(6);
    let list_area = Rect::new(
        layout.body.x,
        layout.body.y + 4,
        layout.body.width,
        list_height,
    );

    let items: Vec<ListItem> = if filtered.is_empty() {
        let label = if picker.loaded_skills.is_empty() {
            "  No skills loaded"
        } else {
            "  No matches"
        };
        vec![ListItem::new(Line::from(Span::styled(
            label,
            Style::default().fg(Color::DarkGray),
        )))]
    } else {
        let line_width = list_area.width.saturating_sub(2);
        filtered
            .iter()
            .map(|skill| ListItem::new(loaded_skill_line(skill, line_width)))
            .collect()
    };

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !filtered.is_empty() {
        list_state.select(Some(picker.selected));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);

    render_separator(frame, layout.body, 4 + list_height);

    let status_area = Rect::new(
        layout.body.x,
        layout.body.y + 5 + list_height,
        layout.body.width,
        1,
    );
    if let Some(error) = &picker.error {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                error.clone(),
                Style::default().fg(Color::Red),
            )))
            .alignment(Alignment::Center),
            status_area,
        );
    } else if let Some(skill) = filtered.get(picker.selected)
        && let Some(desc) = &skill.description
    {
        let text = crate::common::truncate_start_with_ellipsis(desc, layout.body.width as usize);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(Color::DarkGray),
            )))
            .alignment(Alignment::Center),
            status_area,
        );
    }
}

fn loaded_skill_line(skill: &SkillItem, width: u16) -> Line<'static> {
    let source = skill.source.clone().unwrap_or_default();
    let base = skill.name.clone();
    let left_width = base.len() as u16;
    let right_width = source.len() as u16;
    let spacing = if right_width == 0 || width <= left_width + right_width {
        1
    } else {
        width - left_width - right_width
    } as usize;

    let mut spans = vec![Span::styled(base, Style::default().fg(Color::Cyan))];
    spans.push(Span::raw(" ".repeat(spacing)));
    if !source.is_empty() {
        spans.push(Span::styled(source, Style::default().fg(Color::DarkGray)));
    }
    Line::from(spans)
}

// =============================================================================
// Detail view rendering
// =============================================================================

#[allow(clippy::too_many_arguments)]
fn render_skill_detail(
    frame: &mut Frame,
    picker: &SkillPickerState,
    skill: &SkillItem,
    instructions: &DetailContent,
    scroll: u16,
    from_install: bool,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{InputHint, OverlayConfig, render_overlay, render_separator};

    let max_width = area.width.saturating_sub(4);
    let detail_width = max_width.clamp(40, 90);
    let detail_height = (input_top_y.saturating_sub(4)).max(12);

    let installed = picker.is_installed(skill);

    let mut hints = vec![
        InputHint::new("↑↓", "scroll"),
        InputHint::new("Esc", "back"),
    ];
    if from_install && !installed && picker.installing_skill.is_none() {
        hints.insert(1, InputHint::new("Enter", "install"));
    }

    let layout = render_overlay(
        frame,
        area,
        input_top_y,
        &OverlayConfig {
            title: "Skill Details",
            border_color: Color::Magenta,
            width: detail_width,
            height: detail_height,
            hints: &hints,
        },
    );

    // -- Header: skill name + install status --
    let header_area = Rect::new(layout.body.x, layout.body.y, layout.body.width, 1);
    let mut header_spans = vec![Span::styled(
        skill.name.clone(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )];
    if installed {
        header_spans.push(Span::styled(
            " (installed)",
            Style::default().fg(Color::Green),
        ));
    }
    if !from_install && let Some(source) = &skill.source {
        header_spans.push(Span::styled(
            format!("  [{source}]"),
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(header_spans)), header_area);

    render_separator(frame, layout.body, 1);

    // -- Content area --
    let content_height = layout.body.height.saturating_sub(4);
    let content_area = Rect::new(
        layout.body.x,
        layout.body.y + 2,
        layout.body.width,
        content_height,
    );

    match instructions {
        DetailContent::Loading => {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "Loading...",
                    Style::default().fg(Color::DarkGray),
                ))),
                content_area,
            );
        }
        DetailContent::Failed(error) => {
            let lines = vec![
                Line::from(Span::styled(error.clone(), Style::default().fg(Color::Red))),
                Line::from(""),
                Line::from(Span::styled(
                    if !from_install {
                        "Press Esc to go back."
                    } else if installed {
                        "Skill is already installed."
                    } else {
                        "Press Enter to install anyway."
                    },
                    Style::default().fg(Color::DarkGray),
                )),
            ];
            frame.render_widget(Paragraph::new(lines), content_area);
        }
        DetailContent::Loaded(content) => {
            let lines: Vec<Line> = content.lines().map(|l| Line::from(l.to_string())).collect();
            let para = Paragraph::new(lines)
                .wrap(Wrap { trim: false })
                .scroll((scroll, 0));
            frame.render_widget(para, content_area);
        }
    }

    render_separator(frame, layout.body, 2 + content_height);

    // -- Status line --
    let status_area = Rect::new(
        layout.body.x,
        layout.body.y + 3 + content_height,
        layout.body.width,
        1,
    );
    let status_line = detail_status_text(picker, skill, from_install);
    if let Some(line) = status_line {
        frame.render_widget(
            Paragraph::new(line).alignment(Alignment::Center),
            status_area,
        );
    }
}

// =============================================================================
// Helpers
// =============================================================================

fn format_repo_label(picker: &SkillPickerState, max_width: usize) -> String {
    if picker.repos.is_empty() {
        return "No repositories configured".to_string();
    }

    let repo = picker.current_repo().unwrap_or("?");
    let label = if picker.repos.len() > 1 {
        format!(
            "Repo: {} ({}/{})",
            repo,
            picker.selected_repo + 1,
            picker.repos.len()
        )
    } else {
        format!("Repo: {repo}")
    };

    crate::common::truncate_start_with_ellipsis(&label, max_width)
}

fn list_status_text(picker: &SkillPickerState) -> Option<Line<'static>> {
    if let Some(error) = &picker.error {
        return Some(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    if let Some(skill) = &picker.installing_skill {
        return Some(Line::from(Span::styled(
            format!("Installing {skill}..."),
            Style::default().fg(Color::Yellow),
        )));
    }

    if picker.loading_repo.is_some() {
        return Some(Line::from(Span::styled(
            "Fetching skills...",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let count = picker.filtered_skills().len();
    Some(Line::from(Span::styled(
        format!("{} skill{}", count, if count == 1 { "" } else { "s" }),
        Style::default().fg(Color::DarkGray),
    )))
}

fn detail_status_text(
    picker: &SkillPickerState,
    skill: &SkillItem,
    from_install: bool,
) -> Option<Line<'static>> {
    if let Some(error) = &picker.error {
        return Some(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    if let Some(installing) = &picker.installing_skill {
        return Some(Line::from(Span::styled(
            format!("Installing {installing}..."),
            Style::default().fg(Color::Yellow),
        )));
    }

    if !from_install {
        return Some(Line::from(Span::styled(
            crate::common::truncate_start_with_ellipsis(&skill.path, 80),
            Style::default().fg(Color::DarkGray),
        )));
    }

    if picker.is_installed(skill) {
        return Some(Line::from(Span::styled(
            "✓ Installed",
            Style::default().fg(Color::Green),
        )));
    }

    Some(Line::from(Span::styled(
        "Press Enter to install",
        Style::default().fg(Color::DarkGray),
    )))
}

fn skill_line(picker: &SkillPickerState, skill: &SkillItem, width: u16) -> Line<'static> {
    let installed = picker.is_installed(skill);
    let suffix = if installed { "(installed)" } else { "" };

    let base = skill.name.clone();
    let left_width = base.len() as u16;
    let right_width = suffix.len() as u16;
    let spacing = if right_width == 0 || width <= left_width + right_width {
        1
    } else {
        width - left_width - right_width
    } as usize;

    let mut spans = Vec::new();
    spans.push(Span::styled(
        base,
        Style::default().fg(if installed {
            Color::DarkGray
        } else {
            Color::Cyan
        }),
    ));
    spans.push(Span::raw(" ".repeat(spacing)));
    if installed {
        spans.push(Span::styled(suffix, Style::default().fg(Color::DarkGray)));
    }

    Line::from(spans)
}

fn load_installed_skills() -> HashSet<String> {
    let mut installed = HashSet::new();

    // Check project's .zdx/skills/ (where new skills are installed)
    if let Ok(cwd) = std::env::current_dir() {
        add_installed_from_dir(&cwd.join(".zdx").join("skills"), &mut installed);
    }

    // Check ZDX_HOME/skills (respects ZDX_HOME env var)
    let zdx_skills = zdx_engine::config::paths::zdx_home().join("skills");
    add_installed_from_dir(&zdx_skills, &mut installed);

    // Also check ~/.codex/skills for Codex-installed skills
    if let Some(home) = zdx_engine::config::paths::home_dir() {
        add_installed_from_dir(&home.join(".codex").join("skills"), &mut installed);
    }

    installed
}

fn add_installed_from_dir(root: &Path, installed: &mut HashSet<String>) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let skill_file = path.join("SKILL.md");
        if skill_file.exists()
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            installed.insert(normalize_skill_name(name));
        }
    }
}

fn normalize_skill_name(name: &str) -> String {
    name.trim().to_lowercase().replace([' ', '_'], "-")
}
