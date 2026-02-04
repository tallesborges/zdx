use std::collections::{HashMap, HashSet};
use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph};

use super::OverlayUpdate;
use crate::effects::UiEffect;
use crate::mutations::StateMutation;
use crate::state::TuiState;

#[derive(Debug, Clone)]
pub struct SkillItem {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
}

#[derive(Debug)]
pub struct SkillPickerState {
    repos: Vec<String>,
    selected_repo: usize,
    filter: String,
    selected: usize,
    skills_by_repo: HashMap<String, Vec<SkillItem>>,
    loading_repo: Option<String>,
    installing_skill: Option<String>,
    error: Option<String>,
    installed: HashSet<String>,
}

impl SkillPickerState {
    pub fn open(repos: Vec<String>, last_repo: Option<&str>) -> (Self, Vec<UiEffect>) {
        let installed = load_installed_skills();
        let repos: Vec<String> = repos
            .into_iter()
            .filter(|repo| !repo.trim().is_empty())
            .collect();
        let selected_repo = last_repo
            .and_then(|repo| repos.iter().position(|r| r == repo))
            .unwrap_or(0);

        let mut state = Self {
            repos,
            selected_repo,
            filter: String::new(),
            selected: 0,
            skills_by_repo: HashMap::new(),
            loading_repo: None,
            installing_skill: None,
            error: None,
            installed,
        };

        let effects = if let Some(repo) = state.current_repo().map(ToString::to_string) {
            state.loading_repo = Some(repo.clone());
            vec![UiEffect::FetchSkillsList { repo }]
        } else {
            vec![]
        };

        (state, effects)
    }

    pub fn current_repo(&self) -> Option<&str> {
        self.repos.get(self.selected_repo).map(String::as_str)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, input_y: u16) {
        render_skill_picker(frame, self, area, input_y)
    }

    pub fn handle_key(&mut self, _tui: &TuiState, key: KeyEvent) -> OverlayUpdate {
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
            KeyCode::Tab => self.switch_repo(true),
            KeyCode::BackTab => self.switch_repo(false),
            KeyCode::Enter => self.install_selected_skill(),
            KeyCode::Char('u') if ctrl && !shift && !alt => {
                self.filter.clear();
                self.error = None;
                self.clamp_selection();
                OverlayUpdate::stay()
            }
            KeyCode::Backspace => {
                if alt {
                    clear_word_left(&mut self.filter);
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

    fn install_selected_skill(&mut self) -> OverlayUpdate {
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

        if self.is_installed(&skill) {
            self.error = Some("Skill already installed.".to_string());
            return OverlayUpdate::stay();
        }

        let skill_name = skill.name.clone();
        self.installing_skill = Some(skill_name.clone());

        OverlayUpdate::stay().with_ui_effects(vec![UiEffect::InstallSkill {
            repo,
            skill_path: skill.path.clone(),
        }])
    }

    fn switch_repo(&mut self, forward: bool) -> OverlayUpdate {
        if self.repos.len() <= 1 {
            return OverlayUpdate::stay();
        }

        let repo_count = self.repos.len();
        if forward {
            self.selected_repo = (self.selected_repo + 1) % repo_count;
        } else {
            self.selected_repo = (self.selected_repo + repo_count - 1) % repo_count;
        }

        self.filter.clear();
        self.selected = 0;
        self.error = None;
        self.loading_repo = None;
        self.clamp_selection();

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

    fn filtered_skills(&self) -> Vec<&SkillItem> {
        let Some(repo) = self.current_repo() else {
            return Vec::new();
        };

        let skills = self
            .skills_by_repo
            .get(repo)
            .map(Vec::as_slice)
            .unwrap_or(&[]);

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
        let count = self.filtered_skills().len();
        if count == 0 {
            self.selected = 0;
        } else if self.selected >= count {
            self.selected = count - 1;
        }
    }

    fn is_installed(&self, skill: &SkillItem) -> bool {
        self.installed.contains(&normalize_skill_name(&skill.name))
    }
}

pub fn render_skill_picker(
    frame: &mut Frame,
    picker: &SkillPickerState,
    area: Rect,
    input_top_y: u16,
) {
    use super::render_utils::{
        InputHint, InputLine, OverlayConfig, render_input_line, render_overlay, render_separator,
    };

    let filtered = picker.filtered_skills();

    let max_width = area.width.saturating_sub(4);
    let picker_width = max_width.clamp(40, 90);
    let picker_height = (filtered.len() as u16 + 9).max(9);

    let mut hints = vec![
        InputHint::new("↑↓", "navigate"),
        InputHint::new("Enter", "install"),
        InputHint::new("Esc", "cancel"),
    ];
    if picker.repos.len() > 1 {
        hints.insert(2, InputHint::new("Tab", "repo"));
    }

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
    let status_line = status_text(picker);
    if let Some(line) = status_line {
        frame.render_widget(
            Paragraph::new(line).alignment(Alignment::Center),
            status_area,
        );
    }
}

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
        format!("Repo: {}", repo)
    };

    crate::common::truncate_start_with_ellipsis(&label, max_width)
}

fn status_text(picker: &SkillPickerState) -> Option<Line<'static>> {
    if let Some(error) = &picker.error {
        return Some(Line::from(Span::styled(
            error.clone(),
            Style::default().fg(Color::Red),
        )));
    }

    if let Some(skill) = &picker.installing_skill {
        return Some(Line::from(Span::styled(
            format!("Installing {}...", skill),
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
    let zdx_skills = zdx_core::config::paths::zdx_home().join("skills");
    add_installed_from_dir(&zdx_skills, &mut installed);

    // Also check ~/.codex/skills for Codex-installed skills
    if let Some(home) = dirs::home_dir() {
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

fn clear_word_left(input: &mut String) {
    let trimmed_len = input.trim_end().len();
    if trimmed_len == 0 {
        input.clear();
        return;
    }

    input.truncate(trimmed_len);
    let mut chars: Vec<char> = input.chars().collect();
    while let Some(&ch) = chars.last() {
        if ch.is_whitespace() {
            break;
        }
        chars.pop();
    }
    input.clear();
    input.extend(chars);
}
