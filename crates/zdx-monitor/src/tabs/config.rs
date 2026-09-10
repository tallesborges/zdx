use std::path::Path;

use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use serde_json::Value;
use zdx_engine::config;
use zdx_engine::models::{
    available_models, bare_model_id, custom_provider_models, model_id_matches_patterns,
};

use crate::app::MonitorApp;
use crate::ui::{SELECTED_BG, centered_rect, truncate_chars};

/// A single displayable line in the Config tab.
#[derive(Clone)]
pub enum ConfigLine {
    /// Section header (derived from a top-level object key, e.g. "providers").
    Section(String),
    /// Subtle separator between sub-groups within a section (e.g. between providers).
    Separator,
    /// Key-value row inside a section.
    Row(String, String),
}

pub(crate) struct ConfigView {
    pub model: String,
    pub lines: Vec<ConfigLine>,
    pub sources: Vec<Option<String>>,
}

pub(crate) fn load_config_view(root: &Path) -> anyhow::Result<ConfigView> {
    let (config, sources) =
        config::Config::load_layered_with_sources(&config::paths::config_layer_paths_for(root))?;
    let lines = build_config_lines(&config, root);
    let sources = config_source_labels(&config, &lines, &sources, &config::paths::config_path());
    Ok(ConfigView {
        model: config.model,
        lines,
        sources,
    })
}

fn source_name(source: Option<&Path>, global: &Path) -> String {
    match source {
        None => "default".to_string(),
        Some(path) if path == global => "global".to_string(),
        Some(path) => path
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .map_or_else(
                || "workspace".to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
    }
}

fn config_source_labels(
    _config: &config::Config,
    lines: &[ConfigLine],
    sources: &config::ConfigSources,
    global: &Path,
) -> Vec<Option<String>> {
    let mut section = "";
    lines
        .iter()
        .map(|line| {
            let ConfigLine::Row(key, value) = line else {
                if let ConfigLine::Section(name) = line {
                    section = name;
                }
                return None;
            };
            if section == "core" && key == "model" {
                return sources
                    .source("model")
                    .filter(|path| *path != global)
                    .map(|source| source_name(Some(source), global));
            }
            if section == "subagents" && key != "enabled" {
                if value == SUBAGENT_DEFAULT_LABEL {
                    return None;
                }
                let prefix = format!("subagents.overrides.{key}");
                return sources
                    .source(&format!("{prefix}.model"))
                    .filter(|path| *path != global)
                    .map(|source| source_name(Some(source), global));
            }
            let path = match section {
                "core" | "helper models" => key.clone(),
                "model modes" if key == ADD_MODE_LABEL => return None,
                "model modes" => "model_modes".to_string(),
                _ => format!("{section}.{key}"),
            };
            sources
                .source(&path)
                .filter(|path| *path != global)
                .map(|source| source_name(Some(source), global))
        })
        .collect()
}

const SENSITIVE_PATTERNS: &[&str] = &["api_key", "token", "secret", "password", "webhook"];

fn is_sensitive(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_PATTERNS.iter().any(|p| lower.contains(p))
}

fn format_json_scalar(val: &Value) -> String {
    match val {
        Value::Null => "(unset)".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if s.is_empty() => "(empty)".to_string(),
        Value::String(s) if s.len() > 60 => format!("{}…", &s[..57]),
        Value::String(s) => s.clone(),
        Value::Array(arr) if arr.is_empty() => "(empty)".to_string(),
        Value::Array(arr) => arr
            .iter()
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Object(_) => "(object)".to_string(),
    }
}

fn flatten_object(
    obj: &serde_json::Map<String, Value>,
    prefix: &str,
    out: &mut Vec<(String, String)>,
) {
    for (key, val) in obj {
        let full_key = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        match val {
            Value::Object(nested) => flatten_object(nested, &full_key, out),
            Value::Array(arr) if arr.iter().any(Value::is_object) => {
                for (i, item) in arr.iter().enumerate() {
                    let indexed = format!("{full_key}[{i}]");
                    if let Value::Object(nested) = item {
                        flatten_object(nested, &indexed, out);
                    } else {
                        let display = if is_sensitive(&full_key) && !matches!(item, Value::Null) {
                            "***".to_string()
                        } else {
                            format_json_scalar(item)
                        };
                        out.push((indexed, display));
                    }
                }
            }
            _ => {
                let display = if is_sensitive(&full_key) && !matches!(val, Value::Null) {
                    "***".to_string()
                } else {
                    format_json_scalar(val)
                };
                out.push((full_key, display));
            }
        }
    }
}

/// Build the display lines for a single section's object.
/// When the object's direct values are themselves objects (e.g. providers),
/// a `Separator` is emitted between each sub-group.
fn build_section_lines(section_obj: &serde_json::Map<String, Value>) -> Vec<ConfigLine> {
    let mut lines = Vec::new();
    let mut first_group = true;

    for (key, val) in section_obj {
        if let Value::Object(nested) = val {
            if !first_group {
                lines.push(ConfigLine::Separator);
            }
            first_group = false;
            let mut rows = Vec::new();
            flatten_object(nested, key, &mut rows);
            for (k, v) in rows {
                lines.push(ConfigLine::Row(k, v));
            }
        } else {
            let display = if is_sensitive(key) && !matches!(val, Value::Null) {
                "***".to_string()
            } else {
                format_json_scalar(val)
            };
            lines.push(ConfigLine::Row(key.clone(), display));
        }
    }

    lines
}

/// Serialize `config` to JSON and flatten it into displayable lines.
/// Top-level scalars are grouped under a synthetic "core" section.
/// Top-level objects each become their own named section.
/// Config model fields handled by helper subagents, grouped together on the
/// Config tab (in display order) instead of being scattered through `core`.
pub(crate) const HELPER_MODEL_KEYS: [&str; 5] = [
    "title_model",
    "tldr_model",
    "handoff_model",
    "prompt_builder_model",
    "read_thread_model",
];

pub fn build_config_lines(config: &config::Config, root: &Path) -> Vec<ConfigLine> {
    let value = match serde_json::to_value(config) {
        Ok(v) => v,
        Err(e) => return vec![ConfigLine::Row("error".to_string(), e.to_string())],
    };
    let Some(obj) = value.as_object() else {
        return Vec::new();
    };

    let mut core_rows: Vec<(String, String)> = Vec::new();
    let mut sections: Vec<(String, Vec<ConfigLine>)> = Vec::new();

    for (key, val) in obj {
        if key == "model_modes" || key == "subagents" {
            // Rendered as dedicated groups below (modes from
            // `config.model_modes`, subagents from `discover` + overrides).
            continue;
        }
        if let Value::Object(nested) = val {
            let section_lines = build_section_lines(nested);
            sections.push((key.clone(), section_lines));
        } else {
            let display = if is_sensitive(key) && !matches!(val, Value::Null) {
                "***".to_string()
            } else {
                format_json_scalar(val)
            };
            core_rows.push((key.clone(), display));
        }
    }

    let mut lines = Vec::new();

    // Pull helper-subagent model fields out of `core` into their own group so
    // every model handled by a helper is visible together.
    let mut helper_rows: Vec<(String, String)> = Vec::new();
    for key in HELPER_MODEL_KEYS {
        if let Some(pos) = core_rows.iter().position(|(k, _)| k == key) {
            helper_rows.push(core_rows.remove(pos));
        }
    }

    if !core_rows.is_empty() {
        lines.push(ConfigLine::Section("core".to_string()));
        for (k, v) in core_rows {
            lines.push(ConfigLine::Row(k, v));
        }
    }

    if !helper_rows.is_empty() {
        lines.push(ConfigLine::Section("helper models".to_string()));
        for (k, v) in helper_rows {
            lines.push(ConfigLine::Row(k, v));
        }
    }

    // Model modes group: one row per mode (`name → primary@thinking (+N alt)`)
    // plus a trailing action row to add a new one.
    let mut mode_lines: Vec<ConfigLine> = config
        .model_modes
        .iter()
        .map(|mode| ConfigLine::Row(mode.name.clone(), format_mode_value(mode)))
        .collect();
    mode_lines.push(ConfigLine::Row(ADD_MODE_LABEL.to_string(), String::new()));
    sections.push(("model modes".to_string(), mode_lines));

    // Subagents group: every discovered (non-reserved) subagent with its
    // effective model. Config overrides (`[subagents.overrides.<name>]`) win
    // over the live definition; unset rows read `(default)`.
    let sub_lines = build_subagent_lines(config, root);
    if !sub_lines.is_empty() {
        sections.push(("subagents".to_string(), sub_lines));
    }

    // Order model-bearing sections first so every editable model is grouped at
    // the top of the tab.
    reorder_model_sections(&mut sections);

    for (name, section_lines) in sections {
        lines.push(ConfigLine::Section(name));
        lines.extend(section_lines);
    }

    lines
}

/// Section names that carry models, in the order they should appear at the top
/// of the Config tab (after `core`/`helper models`).
const MODEL_SECTION_ORDER: [&str; 4] = ["transcription", "speech", "model modes", "subagents"];

/// Action row appended to the `model modes` group to create a new mode.
pub(crate) const ADD_MODE_LABEL: &str = "[+ add mode]";

/// True when a model field value references a mode instead of naming a model.
fn is_mode_ref(value: &str) -> bool {
    value.trim().starts_with(config::MODE_REF_PREFIX)
}

/// Row value for a mode: its primary spec plus an alternatives count.
fn format_mode_value(mode: &config::ModelMode) -> String {
    let primary = zdx_engine::models::format_model_thinking(&mode.primary, mode.thinking);
    match mode.alternatives.len() {
        0 => primary,
        n => format!("{primary} (+{n} alt)"),
    }
}

/// Value shown for a subagent with no model override and no definition model
/// (it inherits the parent/default model at runtime).
pub(crate) const SUBAGENT_DEFAULT_LABEL: &str = "(default)";

/// Field path for the subagents-enabled toggle row (not a model field; toggled
/// on `Enter` rather than opening the model picker). Kept dot-free so it never
/// matches the `subagents.<name>` override prefix.
pub(crate) const SUBAGENTS_ENABLED_PATH: &str = "subagents_enabled";

/// Builds the `subagents` group rows: a leading on/off toggle, then one row per
/// discovered (non-reserved) subagent showing its effective model (config
/// override wins over the live definition), or `(default)` when neither sets a
/// model.
fn build_subagent_lines(config: &config::Config, root: &Path) -> Vec<ConfigLine> {
    let mut out = vec![ConfigLine::Row(
        "enabled".to_string(),
        if config.subagents.enabled {
            "on".to_string()
        } else {
            "off".to_string()
        },
    )];
    let Ok(defs) = zdx_engine::subagents::discover(root) else {
        return out;
    };
    for def in defs {
        let over = config.subagents.overrides.get(&def.name);
        let model = over
            .and_then(|o| o.model.clone())
            .or_else(|| def.model.clone());
        let display = match model {
            // A `mode:<name>` override shows what it resolves to, or that it
            // resolves to nothing, so a stale name is visible where it is fixed.
            Some(m) => match config.resolve_mode_ref(&m) {
                Some(Ok(primary)) => format!("{m} → {primary}"),
                Some(Err(name)) => format!("mode:{name} (missing)"),
                None => {
                    let level = over
                        .and_then(|o| o.thinking_level)
                        .or(def.thinking_level)
                        .unwrap_or(config::ThinkingLevel::Low);
                    zdx_engine::models::format_model_thinking(&m, level)
                }
            },
            None => SUBAGENT_DEFAULT_LABEL.to_string(),
        };
        out.push(ConfigLine::Row(def.name, display));
    }
    out
}

/// Reorders sections so model-bearing sections come first (per
/// `MODEL_SECTION_ORDER`), preserving the original order of the rest.
fn reorder_model_sections(sections: &mut [(String, Vec<ConfigLine>)]) {
    sections.sort_by_key(|(name, _)| {
        MODEL_SECTION_ORDER
            .iter()
            .position(|s| *s == name.as_str())
            .unwrap_or(MODEL_SECTION_ORDER.len())
    });
}

/// Number of lines `render_config` will produce for these lines
/// (sections get a blank spacer before them, except the first).
pub(crate) fn rendered_line_count(config_lines: &[ConfigLine]) -> usize {
    let sections = config_lines
        .iter()
        .filter(|l| matches!(l, ConfigLine::Section(_)))
        .count();
    // Every ConfigLine is 1 rendered line; sections also get a blank line before (except first)
    config_lines.len() + sections.saturating_sub(1)
}

/// Visible content rows in the Config panel (terminal height minus chrome).
pub(crate) fn config_page_size(app: &MonitorApp) -> usize {
    // layout: 3 (tabs) + content + 3 (footer); panel borders take 2 more rows
    (app.terminal_height.saturating_sub(8) as usize).max(1)
}

/// Maximum valid scroll offset so the last line stays visible.
pub(crate) fn config_max_scroll(app: &MonitorApp) -> usize {
    app.config_line_count.saturating_sub(config_page_size(app))
}

// ============================================================================
// Config model editing (Config tab → model picker overlay)
// ============================================================================

/// Kind of an editable model field, which determines the picker's model source,
/// whether it has a thinking step, and how it is persisted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ModelFieldKind {
    /// Chat/agent model (`available_models`, has a thinking step).
    Chat,
    /// Speech-to-text model (curated STT list, no thinking).
    Transcription,
    /// Text-to-speech model (curated TTS list, no thinking).
    Speech,
}

/// An editable model row on the Config tab.
pub struct EditableModelField {
    /// Index into `config_lines` of the row.
    pub line_index: usize,
    /// Config path to persist (`model`, `title_model`, `transcription.model`…).
    pub path: String,
    /// Field kind.
    pub kind: ModelFieldKind,
}

/// Editable model rows on the Config tab, resolved with section context so the
/// (section, key) pair maps to the right config path and kind.
pub(crate) fn editable_model_fields(lines: &[ConfigLine]) -> Vec<EditableModelField> {
    let mut out = Vec::new();
    let mut section = String::new();
    let mut mode_index = 0usize;
    for (i, cl) in lines.iter().enumerate() {
        match cl {
            ConfigLine::Section(name) => {
                section.clone_from(name);
                mode_index = 0;
            }
            ConfigLine::Row(key, _) => {
                let mapped = match (section.as_str(), key.as_str()) {
                    ("core", "model")
                    | (
                        "helper models",
                        "title_model"
                        | "tldr_model"
                        | "handoff_model"
                        | "prompt_builder_model"
                        | "read_thread_model",
                    ) => Some((key.clone(), ModelFieldKind::Chat)),
                    ("transcription", "model") => Some((
                        "transcription.model".to_string(),
                        ModelFieldKind::Transcription,
                    )),
                    ("speech", "model") => {
                        Some(("speech.model".to_string(), ModelFieldKind::Speech))
                    }
                    ("model modes", k) if k == ADD_MODE_LABEL => {
                        Some(("model_modes.add".to_string(), ModelFieldKind::Chat))
                    }
                    ("model modes", _) => {
                        let path = format!("model_modes.{mode_index}");
                        mode_index += 1;
                        Some((path, ModelFieldKind::Chat))
                    }
                    ("subagents", "enabled") => {
                        Some((SUBAGENTS_ENABLED_PATH.to_string(), ModelFieldKind::Chat))
                    }
                    ("subagents", name) => {
                        Some((format!("subagents.{name}"), ModelFieldKind::Chat))
                    }
                    _ => None,
                };
                if let Some((path, kind)) = mapped {
                    out.push(EditableModelField {
                        line_index: i,
                        path,
                        kind,
                    });
                }
            }
            ConfigLine::Separator => {}
        }
    }
    out
}

/// Rendered row offset of `config_lines[target]`, mirroring `render_config`
/// (a blank spacer precedes every section except the first).
fn config_line_render_row(lines: &[ConfigLine], target: usize) -> usize {
    let mut row = 0usize;
    let mut is_first = true;
    for (i, cl) in lines.iter().enumerate() {
        if i == target {
            return row;
        }
        match cl {
            ConfigLine::Section(_) => {
                if !is_first {
                    row += 1;
                }
                row += 1;
                is_first = false;
            }
            ConfigLine::Separator | ConfigLine::Row(..) => row += 1,
        }
    }
    row
}

/// Scrolls the Config panel so the selected editable row stays visible.
fn ensure_config_selection_visible(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    let Some(field) = fields.get(app.config_selected) else {
        return;
    };
    let render_row = config_line_render_row(&app.config_lines, field.line_index);
    let page = config_page_size(app);
    if render_row < app.config_scroll {
        app.config_scroll = render_row;
    } else if render_row >= app.config_scroll + page {
        app.config_scroll = render_row + 1 - page;
    }
}

/// Moves the Config model-row selection and keeps it visible.
pub(crate) fn move_config_selection(app: &mut MonitorApp, forward: bool) {
    let count = editable_model_fields(&app.config_lines).len();
    if count == 0 {
        return;
    }
    app.config_selected = if forward {
        (app.config_selected + 1).min(count - 1)
    } else {
        app.config_selected.saturating_sub(1)
    };
    ensure_config_selection_visible(app);
}

/// Opens the model picker for the currently selected Config model row, or
/// toggles the subagents-enabled flag when that row is selected.
pub(crate) fn open_model_picker(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    let Some(field) = fields.get(app.config_selected) else {
        return;
    };
    if field.path == SUBAGENTS_ENABLED_PATH {
        toggle_subagents_enabled(app);
        return;
    }
    let (path, kind, line_index) = (field.path.clone(), field.kind, field.line_index);
    let current = match &app.config_lines[line_index] {
        // Ignore the `(unset)`/`(empty)`/`(default)` display placeholders.
        ConfigLine::Row(_, v)
            if matches!(v.as_str(), "(unset)" | "(empty)" | SUBAGENT_DEFAULT_LABEL) =>
        {
            String::new()
        }
        ConfigLine::Row(_, v) => v.clone(),
        _ => String::new(),
    };
    let Ok(cfg) = config::Config::load() else {
        app.set_status("Failed to load config");
        return;
    };
    // A `mode:<name> → primary` row shows the ref, not a model id: seed the
    // picker with the ref so it preselects the mode entry.
    let current = current
        .split_once(" → ")
        .map_or(current.as_str(), |(head, _)| head)
        .to_string();
    app.model_picker = Some(ModelPickerState::new(
        path,
        kind,
        &current,
        &cfg.providers,
        &cfg.model_modes,
    ));
}

/// Flips `subagents.enabled` in the config and reloads the Config tab.
fn toggle_subagents_enabled(app: &mut MonitorApp) {
    let Ok(cfg) = config::Config::load() else {
        app.set_status("Failed to load config");
        return;
    };
    let next = !cfg.subagents.enabled;
    match config::Config::save_subagents_enabled(next) {
        Ok(()) => {
            reload_config_lines(app);
            app.set_status(format!(
                "Subagents {}",
                if next { "enabled" } else { "disabled" }
            ));
        }
        Err(e) => app.set_status(format!("Failed to toggle subagents: {e}")),
    }
}

/// Reloads config lines from disk after an edit, clamping selection.
fn reload_config_lines(app: &mut MonitorApp) {
    let Ok(view) = load_config_view(&app.root) else {
        app.set_status("Failed to reload config");
        return;
    };
    app.default_model = view.model;
    app.config_lines = view.lines;
    app.config_sources = view.sources;
    app.config_line_count = rendered_line_count(&app.config_lines);
    let count = editable_model_fields(&app.config_lines).len();
    if app.config_selected >= count {
        app.config_selected = count.saturating_sub(1);
    }
}

/// Which step of the model+thinking picker is active.
#[derive(PartialEq, Eq)]
pub enum PickerPhase {
    Model,
    Thinking,
}

/// Two-step picker for editing a Config model field. Chat models pick a model
/// then a thinking level; audio (STT/TTS) models pick a model only and commit.
pub struct ModelPickerState {
    /// Config path being edited (e.g. `title_model`, `transcription.model`).
    pub field: String,
    /// Field kind (drives model source, thinking step, and persistence).
    pub kind: ModelFieldKind,
    /// Active step.
    pub phase: PickerPhase,
    /// Typed filter text (model step).
    pub filter: String,
    /// All selectable model ids (`provider:id`), sorted.
    pub items: Vec<String>,
    /// Indices into `items` matching the current filter.
    pub matches: Vec<usize>,
    /// Index into `matches` of the highlighted row.
    pub selected: usize,
    /// Model chosen in step 1 (used by the thinking step).
    pub chosen_model: String,
    /// Thinking level the field had when the picker opened.
    pub thinking_current: config::ThinkingLevel,
    /// Index into `config::ThinkingLevel::all()` of the highlighted level.
    pub thinking_selected: usize,
}

impl ModelPickerState {
    pub(crate) fn new(
        field: String,
        kind: ModelFieldKind,
        current: &str,
        providers: &config::ProvidersConfig,
        modes: &[config::ModelMode],
    ) -> Self {
        let spec = zdx_engine::models::ModelSpec::parse(current);
        let model_part = spec.without_thinking();
        let thinking_current = spec.thinking.unwrap_or(config::ThinkingLevel::Low);
        let thinking_selected = config::ThinkingLevel::all()
            .iter()
            .position(|l| *l == thinking_current)
            .unwrap_or(0);

        let mut items: Vec<String> = match kind {
            ModelFieldKind::Chat => available_models()
                .iter()
                .chain(custom_provider_models(providers))
                .filter(|model| {
                    let patterns = if let Some(custom) = providers.custom.get(model.provider) {
                        custom.models.as_slice()
                    } else {
                        let Some(kind) =
                            zdx_engine::providers::provider_kind_from_id(model.provider)
                        else {
                            return false;
                        };
                        if !providers.is_enabled(model.provider) {
                            return false;
                        }
                        providers.get(kind).models.as_slice()
                    };
                    model_id_matches_patterns(bare_model_id(model.provider, model.id), patterns)
                })
                .flat_map(|m| {
                    let id = m.qualified_id();
                    let fast = zdx_engine::models::fast_variant(&id);
                    std::iter::once(id).chain(fast)
                })
                .collect(),
            ModelFieldKind::Transcription => {
                zdx_engine::audio::transcribe::transcription_model_options()
            }
            ModelFieldKind::Speech => zdx_engine::audio::speak::speech_model_options(),
        };
        items.sort();
        items.dedup();
        // Subagent overrides may point at a mode instead of a concrete model;
        // list those refs first so the tier is the easy choice.
        if field.starts_with("subagents.") {
            let refs: Vec<String> = modes.iter().map(config::ModelMode::as_ref_spec).collect();
            items.splice(0..0, refs);
        }

        let mut state = Self {
            field,
            kind,
            phase: PickerPhase::Model,
            filter: String::new(),
            items,
            matches: Vec::new(),
            selected: 0,
            chosen_model: model_part.clone(),
            thinking_current,
            thinking_selected,
        };
        state.recompute();
        // Preselect the current value (exact `provider:id` or bare `id`).
        if let Some(pos) = state.matches.iter().position(|&i| {
            let item = &state.items[i];
            item.as_str() == model_part || item.rsplit(':').next() == Some(model_part.as_str())
        }) {
            state.selected = pos;
        }
        state
    }

    /// Whether this field has a thinking step (chat models only).
    pub(crate) fn has_thinking(&self) -> bool {
        // A mode ref carries its own level in the mode's primary spec.
        self.kind == ModelFieldKind::Chat && !is_mode_ref(&self.chosen_model)
    }

    pub(crate) fn recompute(&mut self) {
        let needle = self.filter.to_lowercase();
        self.matches = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, it)| needle.is_empty() || it.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        if self.selected >= self.matches.len() {
            self.selected = self.matches.len().saturating_sub(1);
        }
    }

    /// The `provider:id` of the highlighted model row, if any.
    pub fn selected_model(&self) -> Option<&str> {
        self.matches
            .get(self.selected)
            .map(|&i| self.items[i].as_str())
    }

    /// The highlighted thinking level.
    pub fn selected_thinking(&self) -> config::ThinkingLevel {
        config::ThinkingLevel::all()
            .get(self.thinking_selected)
            .copied()
            .unwrap_or(config::ThinkingLevel::Low)
    }
}

/// Applies a mode edit for a `model_modes.<i>`/`model_modes.add` field and
/// persists the whole list. Appends a new mode for `add`, else updates index.
/// Only the primary spec is edited here; `description`/`alternatives` stay
/// TOML-only and are preserved on edit.
fn save_mode(field: &str, model: &str, level: config::ThinkingLevel) -> anyhow::Result<()> {
    let mut cfg = config::Config::load()?;
    let suffix = field.strip_prefix("model_modes.").unwrap_or_default();
    if suffix == "add" {
        let name = format!("mode{}", cfg.model_modes.len() + 1);
        cfg.model_modes.push(config::ModelMode {
            name,
            description: String::new(),
            primary: model.to_string(),
            alternatives: Vec::new(),
            thinking: level,
        });
    } else if let Ok(idx) = suffix.parse::<usize>() {
        let mode = cfg
            .model_modes
            .get_mut(idx)
            .ok_or_else(|| anyhow::anyhow!("mode index out of range: {idx}"))?;
        mode.primary = model.to_string();
        mode.thinking = level;
    } else {
        anyhow::bail!("invalid mode field: {field}");
    }
    config::Config::save_model_modes(&cfg.model_modes)
}

/// Handles `d`/`Del` on the Config tab: removes the selected mode, or
/// resets the selected subagent to its default (clears the config override).
pub(crate) fn delete_or_reset_selected(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    let Some(path) = fields.get(app.config_selected).map(|f| f.path.clone()) else {
        return;
    };

    if let Some(suffix) = path.strip_prefix("model_modes.") {
        let Ok(idx) = suffix.parse::<usize>() else {
            return; // add-row or invalid: nothing to delete
        };
        let Ok(mut cfg) = config::Config::load() else {
            app.set_status("Failed to load config");
            return;
        };
        if idx >= cfg.model_modes.len() {
            return;
        }
        let removed = cfg.model_modes.remove(idx);
        match config::Config::save_model_modes(&cfg.model_modes) {
            Ok(()) => {
                reload_config_lines(app);
                app.set_status(format!("Removed mode {}", removed.name));
            }
            Err(e) => app.set_status(format!("Failed to remove mode: {e}")),
        }
    } else if let Some(name) = path.strip_prefix("subagents.") {
        match config::Config::clear_subagent_override(name) {
            Ok(()) => {
                reload_config_lines(app);
                app.set_status(format!("Reset {name} to default"));
            }
            Err(e) => app.set_status(format!("Failed to reset {name}: {e}")),
        }
    }
}

/// Persists the picker's chosen model (+ thinking for chat models).
fn commit_model_picker(app: &mut MonitorApp) {
    let Some(picker) = app.model_picker.as_ref() else {
        return;
    };
    let field = picker.field.clone();
    let model = picker.chosen_model.clone();
    let level = picker.selected_thinking();

    let (result, shown) = match picker.kind {
        // Model mode: update the entry at `model_modes.<i>` or append via
        // `model_modes.add`; thinking rides in the saved primary spec.
        ModelFieldKind::Chat if field.starts_with("model_modes.") => (
            save_mode(&field, &model, level),
            zdx_engine::models::format_model_thinking(&model, level),
        ),
        // Subagent override pointed at a mode: store the ref verbatim, since
        // the mode's primary spec carries the level.
        ModelFieldKind::Chat if field.starts_with("subagents.") && is_mode_ref(&model) => {
            let name = field.strip_prefix("subagents.").unwrap_or_default();
            (
                config::Config::save_subagent_override_spec(name, &model),
                model.clone(),
            )
        }
        // Subagent override: thinking carried inline in the saved model.
        ModelFieldKind::Chat if field.starts_with("subagents.") => {
            let name = field.strip_prefix("subagents.").unwrap_or_default();
            (
                config::Config::save_subagent_override(name, &model, level),
                zdx_engine::models::format_model_thinking(&model, level),
            )
        }
        // Main and role chat models: thinking carried inline as `model@thinking`.
        ModelFieldKind::Chat => {
            let combined = zdx_engine::models::format_model_thinking(&model, level);
            (
                config::Config::save_model_field(&field, &combined),
                combined,
            )
        }
        // Audio (STT/TTS): model only, no thinking.
        ModelFieldKind::Transcription | ModelFieldKind::Speech => (
            config::Config::save_model_field(&field, &model),
            model.clone(),
        ),
    };

    match result {
        Ok(()) => {
            app.model_picker = None;
            reload_config_lines(app);
            app.set_status(format!("Set {field} = {shown}"));
        }
        Err(e) => app.set_status(format!("Failed to set {field}: {e}")),
    }
}

/// Handles a key while the model picker overlay is open.
pub(crate) fn handle_model_picker_key(app: &mut MonitorApp, key: KeyCode) {
    let Some(picker) = app.model_picker.as_mut() else {
        return;
    };
    match picker.phase {
        PickerPhase::Model => match key {
            KeyCode::Esc => app.model_picker = None,
            KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
            KeyCode::Down => {
                let last = picker.matches.len().saturating_sub(1);
                picker.selected = (picker.selected + 1).min(last);
            }
            KeyCode::Backspace => {
                picker.filter.pop();
                picker.recompute();
            }
            KeyCode::Char(c) => {
                picker.filter.push(c);
                picker.recompute();
            }
            KeyCode::Enter => {
                if let Some(model) = picker.selected_model().map(str::to_string) {
                    picker.chosen_model = model;
                    if picker.has_thinking() {
                        picker.phase = PickerPhase::Thinking;
                    } else {
                        commit_model_picker(app);
                    }
                }
            }
            _ => {}
        },
        PickerPhase::Thinking => match key {
            KeyCode::Esc => picker.phase = PickerPhase::Model,
            KeyCode::Up => picker.thinking_selected = picker.thinking_selected.saturating_sub(1),
            KeyCode::Down => {
                let last = config::ThinkingLevel::all().len().saturating_sub(1);
                picker.thinking_selected = (picker.thinking_selected + 1).min(last);
            }
            KeyCode::Enter => commit_model_picker(app),
            _ => {}
        },
    }
}

pub(crate) fn render_config(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let inner_width = area.width.saturating_sub(2) as usize;
    let key_col = 30usize;

    let selected_line = editable_model_fields(&app.config_lines)
        .get(app.config_selected)
        .map(|f| f.line_index);

    let mut lines: Vec<Line> = Vec::new();
    let mut is_first = true;

    for (idx, cl) in app.config_lines.iter().enumerate() {
        match cl {
            ConfigLine::Section(name) => {
                if !is_first {
                    lines.push(Line::from(""));
                }
                is_first = false;

                lines.push(Line::from(vec![Span::styled(
                    format!(" ── {} ", name.to_uppercase()),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                )]));
            }
            ConfigLine::Separator => {
                let dashes = "─".repeat(inner_width.saturating_sub(4));
                lines.push(Line::from(Span::styled(
                    format!("    {dashes}"),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            ConfigLine::Row(key, value) => {
                let is_selected = selected_line == Some(idx);
                let val_style = if value == "***" || value.starts_with("***") {
                    Style::default().fg(Color::DarkGray)
                } else {
                    Style::default()
                };
                let marker = if is_selected { "  ▸ " } else { "    " };
                let key_style = if is_selected {
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let key_span = Span::styled(format!("{marker}{key:<key_col$} "), key_style);
                let source = app.config_sources.get(idx).and_then(Option::as_deref);
                let source_span = Span::styled(
                    source.map_or_else(String::new, |source| format!(" [{source}]")),
                    Style::default().fg(Color::DarkGray),
                );
                let value = if source.is_some() {
                    truncate_chars(
                        value,
                        inner_width.saturating_sub(key_span.width() + source_span.width()),
                    )
                } else {
                    value.clone()
                };
                lines.push(Line::from(vec![
                    key_span,
                    Span::styled(value, val_style),
                    source_span,
                ]));
            }
        }
    }

    let total_fields = app
        .config_lines
        .iter()
        .filter(|l| matches!(l, ConfigLine::Row(..)))
        .count();

    let visible_lines = area.height.saturating_sub(2) as usize;
    let total_lines = lines.len();

    let scroll_info = if total_lines > visible_lines {
        let max_scroll = total_lines - visible_lines;
        let current_scroll = app.config_scroll.min(max_scroll);
        let percent = (current_scroll * 100)
            .checked_div(max_scroll)
            .unwrap_or(100);
        format!(" [{percent}%]")
    } else {
        String::new()
    };

    let root_label = app.root.file_name().unwrap_or_default().to_string_lossy();
    let title = format!(" Config · {root_label} ({total_fields} fields){scroll_info} ");

    let p = Paragraph::new(Text::from(lines))
        .block(Block::default().borders(Borders::ALL).title(title))
        .scroll((app.config_scroll as u16, 0));

    f.render_widget(p, area);
}

/// Build a centered Rect using `percent_x` × `percent_y` of `area`.
pub(crate) fn render_model_picker(f: &mut Frame, picker: &ModelPickerState, area: Rect) {
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);

    match picker.phase {
        PickerPhase::Model => render_picker_models(f, picker, popup),
        PickerPhase::Thinking => render_picker_thinking(f, picker, popup),
    }
}

fn render_picker_models(f: &mut Frame, picker: &ModelPickerState, popup: Rect) {
    let confirm = if picker.kind == ModelFieldKind::Chat {
        "Enter next"
    } else {
        "Enter save"
    };
    let title = format!(
        " {} · pick model · {} match · {confirm} · Esc cancel ",
        picker.field,
        picker.matches.len(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(inner);

    let filter_line = Line::from(vec![
        Span::styled("filter: ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            if picker.filter.is_empty() {
                "(type to filter)".to_string()
            } else {
                picker.filter.clone()
            },
            Style::default().fg(Color::Yellow),
        ),
    ]);
    f.render_widget(Paragraph::new(filter_line), rows[0]);

    let visible = rows[1].height as usize;
    let offset = picker.selected.saturating_sub(visible.saturating_sub(1));
    let end = (offset + visible).min(picker.matches.len());

    let items: Vec<ListItem> = picker.matches[offset..end]
        .iter()
        .enumerate()
        .map(|(i, &item_idx)| {
            let global = offset + i;
            let model = &picker.items[item_idx];
            let is_current = *model == picker.chosen_model
                || model.rsplit(':').next() == Some(picker.chosen_model.as_str());
            let marker = if is_current { "● " } else { "  " };
            let mut style = Style::default();
            if global == picker.selected {
                style = style.fg(Color::Green).bg(SELECTED_BG);
            } else if is_current {
                style = style.fg(Color::Green);
            }
            ListItem::new(Line::from(format!("{marker}{model}"))).style(style)
        })
        .collect();

    f.render_widget(List::new(items), rows[1]);
}

fn render_picker_thinking(f: &mut Frame, picker: &ModelPickerState, popup: Rect) {
    use zdx_engine::config::ThinkingLevel;

    let title = format!(
        " {} · thinking for {} · Enter save · Esc back ",
        picker.field, picker.chosen_model,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let items: Vec<ListItem> = ThinkingLevel::all()
        .iter()
        .enumerate()
        .map(|(i, level)| {
            let is_current = *level == picker.thinking_current;
            let marker = if is_current { "● " } else { "  " };
            let mut style = Style::default();
            if i == picker.thinking_selected {
                style = style.fg(Color::Green).bg(SELECTED_BG);
            } else if is_current {
                style = style.fg(Color::Green);
            }
            let text = format!(
                "{marker}{:<7} {}",
                level.display_name(),
                level.description()
            );
            ListItem::new(Line::from(text)).style(style)
        })
        .collect();

    f.render_widget(List::new(items), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_index(lines: &[ConfigLine], wanted_section: &str, wanted_key: &str) -> usize {
        let mut section = "";
        lines
            .iter()
            .position(|line| match line {
                ConfigLine::Section(name) => {
                    section = name;
                    false
                }
                ConfigLine::Row(key, _) => section == wanted_section && key == wanted_key,
                ConfigLine::Separator => false,
            })
            .unwrap()
    }

    #[test]
    fn config_view_loads_supplied_root_instead_of_process_cwd() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let root = home.path().join("selected-root");
        std::fs::create_dir_all(root.join(".zdx")).unwrap();
        std::fs::write(home.path().join("config.toml"), "model = \"global@low\"\n").unwrap();
        let overlay = root.join(".zdx/config.toml");
        std::fs::write(&overlay, "model = \"selected@high\"\n").unwrap();
        assert_ne!(std::env::current_dir().unwrap(), root);

        let view = load_config_view(&root).unwrap();
        let row = row_index(&view.lines, "core", "model");
        assert_eq!(view.model, "selected@high");
        assert_eq!(view.sources[row].as_deref(), Some("selected-root"));
        assert!(matches!(&view.lines[row], ConfigLine::Row(_, value) if value == "selected@high"));
        std::fs::write(&overlay, "model = \"changed@medium\"\n").unwrap();
        assert_eq!(load_config_view(&root).unwrap().model, "changed@medium");
    }

    #[test]
    fn workspace_labels_name_the_actual_parent_or_child_source() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let global = home.path().join("config.toml");
        let parent = home.path().join("parity/.zdx/config.toml");
        let child = home.path().join("parity/nova/.zdx/config.toml");
        std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
        std::fs::create_dir_all(child.parent().unwrap()).unwrap();
        std::fs::write(&global, "model = \"global@low\"\nmax_tokens = 42\n").unwrap();
        std::fs::write(&parent, "model = \"parent@high\"\n[[model_modes]]\nname = \"work\"\nprimary = \"tier@high\"\n[providers.openai]\napi_key = \"private-value\"\n").unwrap();
        for (contents, expected) in [
            ("[skills]\nenabled = true\n", "parity"),
            ("model = \"parent@high\"\n", "nova"),
        ] {
            std::fs::write(&child, contents).unwrap();
            let (cfg, sources) = config::Config::load_layered_with_sources(&[
                global.clone(),
                parent.clone(),
                child.clone(),
            ])
            .unwrap();
            let lines = build_config_lines(&cfg, child.parent().unwrap().parent().unwrap());
            let labels = config_source_labels(&cfg, &lines, &sources, &global);
            assert_eq!(
                labels[row_index(&lines, "core", "model")].as_deref(),
                Some(expected)
            );
            assert_eq!(labels[row_index(&lines, "core", "max_tokens")], None);
            assert_eq!(
                labels[row_index(&lines, "model modes", "work")].as_deref(),
                Some("parity")
            );
            assert_eq!(
                labels[row_index(&lines, "model modes", ADD_MODE_LABEL)],
                None
            );
            let secret_row = row_index(&lines, "providers", "openai.api_key");
            assert_eq!(labels[secret_row].as_deref(), Some("parity"));
            assert!(matches!(&lines[secret_row], ConfigLine::Row(_, value) if value == "***"));
            let field = editable_model_fields(&lines)
                .into_iter()
                .find(|field| field.path == "model")
                .unwrap();
            assert!(
                matches!(&lines[field.line_index], ConfigLine::Row(_, value) if value == "parent@high")
            );
        }
    }

    #[test]
    fn unified_model_spec_origins_follow_the_winning_layer() {
        let home = zdx_engine::test_support::temp_zdx_home();
        let global = home.path().join("config.toml");
        let parent = home.path().join("parity/.zdx/config.toml");
        std::fs::create_dir_all(parent.parent().unwrap()).unwrap();
        std::fs::write(
            &global,
            "model = \"global@off\"\n[subagents.overrides.explorer]\nmodel = \"global-model@low\"\n",
        )
        .unwrap();
        std::fs::write(
            &parent,
            "model = \"workspace@high\"\n[subagents.overrides.explorer]\nmodel = \"workspace-model@high\"\n",
        )
        .unwrap();
        let (cfg, sources) =
            config::Config::load_layered_with_sources(&[global.clone(), parent]).unwrap();
        let lines = build_config_lines(&cfg, home.path());
        let labels = config_source_labels(&cfg, &lines, &sources, &global);
        assert_eq!(
            labels[row_index(&lines, "core", "model")].as_deref(),
            Some("parity")
        );
        assert_eq!(
            labels[row_index(&lines, "subagents", "explorer")].as_deref(),
            Some("parity")
        );
    }

    #[test]
    fn editable_fields_resolve_path_and_kind_by_section() {
        let lines = vec![
            ConfigLine::Section("core".into()),
            ConfigLine::Row("model".into(), "x".into()),
            ConfigLine::Row("verbose".into(), "true".into()),
            ConfigLine::Section("helper models".into()),
            ConfigLine::Row("title_model".into(), "y".into()),
            ConfigLine::Row("prompt_builder_model".into(), "p".into()),
            ConfigLine::Section("transcription".into()),
            ConfigLine::Row("model".into(), "z".into()),
            ConfigLine::Row("language".into(), "en".into()),
            ConfigLine::Section("speech".into()),
            ConfigLine::Row("model".into(), "w".into()),
            ConfigLine::Section("telegram".into()),
            ConfigLine::Row("bot_token".into(), "***".into()),
        ];
        let fields = editable_model_fields(&lines);
        let got: Vec<(&str, ModelFieldKind)> =
            fields.iter().map(|f| (f.path.as_str(), f.kind)).collect();
        assert_eq!(
            got,
            vec![
                ("model", ModelFieldKind::Chat),
                ("title_model", ModelFieldKind::Chat),
                ("prompt_builder_model", ModelFieldKind::Chat),
                ("transcription.model", ModelFieldKind::Transcription),
                ("speech.model", ModelFieldKind::Speech),
            ]
        );
    }

    #[test]
    fn build_config_lines_groups_helper_models() {
        let config = config::Config::default();
        let lines = build_config_lines(&config, std::path::Path::new("/nonexistent-zdx-test-root"));

        let mut section = String::new();
        let mut helper_keys: Vec<String> = Vec::new();
        let mut core_has_helper = false;
        for cl in &lines {
            match cl {
                ConfigLine::Section(name) => section.clone_from(name),
                ConfigLine::Row(key, _) => {
                    let is_helper = HELPER_MODEL_KEYS.contains(&key.as_str());
                    if section == "helper models" && is_helper {
                        helper_keys.push(key.clone());
                    }
                    if section == "core" && is_helper {
                        core_has_helper = true;
                    }
                }
                ConfigLine::Separator => {}
            }
        }

        assert!(!core_has_helper, "helper models must not remain in `core`");
        assert_eq!(
            helper_keys,
            vec![
                "title_model",
                "tldr_model",
                "handoff_model",
                "prompt_builder_model",
                "read_thread_model",
            ],
            "helpers section should list every helper model in display order"
        );
    }

    #[test]
    fn model_modes_group_resolves_indices_and_add_row() {
        let lines = vec![
            ConfigLine::Section("model modes".into()),
            ConfigLine::Row("fast".into(), "gemini:x@low".into()),
            ConfigLine::Row("smart".into(), "claude:y@high (+1 alt)".into()),
            ConfigLine::Row(ADD_MODE_LABEL.into(), String::new()),
        ];
        let fields = editable_model_fields(&lines);
        let paths: Vec<&str> = fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["model_modes.0", "model_modes.1", "model_modes.add"]
        );
    }

    #[test]
    fn subagents_group_resolves_names_to_override_paths() {
        let lines = vec![
            ConfigLine::Section("subagents".into()),
            ConfigLine::Row("enabled".into(), "on".into()),
            ConfigLine::Row("explorer".into(), "gemini:x@low".into()),
            ConfigLine::Row("oracle".into(), SUBAGENT_DEFAULT_LABEL.into()),
        ];
        let fields = editable_model_fields(&lines);
        let got: Vec<&str> = fields.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            got,
            vec![
                SUBAGENTS_ENABLED_PATH,
                "subagents.explorer",
                "subagents.oracle",
            ]
        );
    }

    fn test_mode(name: &str, primary: &str, alternatives: &[&str]) -> config::ModelMode {
        config::ModelMode {
            name: name.to_string(),
            description: "tier".to_string(),
            primary: primary.to_string(),
            alternatives: alternatives.iter().map(|a| (*a).to_string()).collect(),
            thinking: zdx_engine::models::ModelSpec::parse(primary)
                .thinking
                .unwrap_or_default(),
        }
    }

    /// Mode rows show the primary and an alternatives count; a subagent row
    /// pointing at a mode shows what it resolves to, and a stale one says so.
    #[test]
    fn mode_rows_and_subagent_refs_render_resolved_values() {
        let modes = vec![
            test_mode("fast", "gemini:flash", &["claude-cli:sonnet@low"]),
            test_mode("smart", "claude-cli:opus@high", &[]),
        ];
        let rows: Vec<String> = modes.iter().map(format_mode_value).collect();
        assert_eq!(
            rows,
            vec!["gemini:flash@off (+1 alt)", "claude-cli:opus@high"]
        );

        let config = config::Config {
            model_modes: modes,
            ..Default::default()
        };
        assert_eq!(
            config.resolve_mode_ref("mode:smart"),
            Some(Ok("claude-cli:opus@high"))
        );
        assert!(is_mode_ref("mode:fast"));
        assert!(!is_mode_ref("claude-cli:opus@high"));
    }

    /// The picker offers mode refs for subagent fields only, and a chosen ref
    /// skips the thinking step because the mode's primary carries the level.
    #[test]
    fn picker_offers_mode_refs_for_subagent_fields_only() {
        let providers = config::ProvidersConfig::default();
        let modes = vec![test_mode("fast", "gemini:flash@low", &[])];

        let subagent = ModelPickerState::new(
            "subagents.explorer".to_string(),
            ModelFieldKind::Chat,
            "mode:fast",
            &providers,
            &modes,
        );
        assert_eq!(
            subagent.items.first().map(String::as_str),
            Some("mode:fast")
        );
        assert_eq!(subagent.selected_model(), Some("mode:fast"));
        assert!(
            !ModelPickerState {
                chosen_model: "mode:fast".to_string(),
                ..subagent
            }
            .has_thinking(),
            "a mode ref must not open the thinking step"
        );

        let helper = ModelPickerState::new(
            "title_model".to_string(),
            ModelFieldKind::Chat,
            "",
            &providers,
            &modes,
        );
        assert!(!helper.items.iter().any(|item| is_mode_ref(item)));
        assert!(helper.has_thinking());
    }

    #[test]
    fn build_config_lines_orders_model_sections_first() {
        let config = config::Config::default();
        let lines = build_config_lines(&config, std::path::Path::new("/nonexistent-zdx-test-root"));

        // Collect section names in display order.
        let sections: Vec<&str> = lines
            .iter()
            .filter_map(|cl| match cl {
                ConfigLine::Section(name) => Some(name.as_str()),
                _ => None,
            })
            .collect();
        let pos = |name: &str| sections.iter().position(|s| *s == name);
        // Model sections lead: core, helper models, transcription, speech.
        for pair in [
            ("core", "helper models"),
            ("helper models", "transcription"),
            ("transcription", "speech"),
            ("speech", "telegram"),
        ] {
            assert!(
                pos(pair.0) < pos(pair.1),
                "{} should come before {}",
                pair.0,
                pair.1
            );
        }

        // The telegram section carries no model rows anymore.
        let mut in_telegram = false;
        for cl in &lines {
            match cl {
                ConfigLine::Section(name) => in_telegram = name == "telegram",
                ConfigLine::Row(k, _) if in_telegram => assert!(
                    k != "model" && k != "thinking_level",
                    "telegram must not carry a {k} row"
                ),
                _ => {}
            }
        }
    }

    #[test]
    fn model_picker_filters_and_reports_selection() {
        let providers = config::ProvidersConfig::default();
        let mut p = ModelPickerState::new(
            "title_model".to_string(),
            ModelFieldKind::Chat,
            "no-such-model",
            &providers,
            &[],
        );
        assert!(!p.items.is_empty(), "registry should list models");
        let before = p.matches.len();
        p.filter.push_str("claude");
        p.recompute();
        assert!(p.matches.len() <= before);
        assert!(
            p.matches
                .iter()
                .all(|&i| p.items[i].to_lowercase().contains("claude"))
        );
        if let Some(sel) = p.selected_model() {
            assert!(sel.to_lowercase().contains("claude"));
        }
    }

    #[test]
    fn model_picker_excludes_disabled_providers() {
        let model = available_models()
            .first()
            .expect("registry should list models");
        let kind = zdx_engine::providers::provider_kind_from_id(model.provider)
            .expect("registry provider should be built in");
        let mut providers = config::ProvidersConfig::default();
        let provider = providers.get_mut(kind);
        provider.enabled = Some(true);
        provider.models.clear();

        let enabled = ModelPickerState::new(
            "model".to_string(),
            ModelFieldKind::Chat,
            "",
            &providers,
            &[],
        );
        assert!(enabled.items.contains(&model.qualified_id()));

        providers.get_mut(kind).enabled = Some(false);
        let disabled = ModelPickerState::new(
            "model".to_string(),
            ModelFieldKind::Chat,
            "",
            &providers,
            &[],
        );
        assert!(!disabled.items.contains(&model.qualified_id()));
    }

    #[test]
    fn speech_picker_lists_curated_options_without_thinking() {
        let p = ModelPickerState::new(
            "speech.model".to_string(),
            ModelFieldKind::Speech,
            "",
            &config::ProvidersConfig::default(),
            &[],
        );
        assert!(!p.has_thinking());
        assert!(p.items.iter().all(|o| o.contains(':')));
        assert!(p.items.iter().any(|o| o.starts_with("mistral:")));
    }

    #[test]
    fn model_picker_parses_inline_thinking_suffix() {
        let providers = config::ProvidersConfig::default();
        let p = ModelPickerState::new(
            "title_model".to_string(),
            ModelFieldKind::Chat,
            "gemini:some-model@high",
            &providers,
            &[],
        );
        assert_eq!(p.thinking_current, config::ThinkingLevel::High);
        assert_eq!(p.chosen_model, "gemini:some-model");

        // No suffix defaults to Low.
        let p2 = ModelPickerState::new(
            "tldr_model".to_string(),
            ModelFieldKind::Chat,
            "gemini:some-model",
            &providers,
            &[],
        );
        assert_eq!(p2.thinking_current, config::ThinkingLevel::Low);
    }
}
