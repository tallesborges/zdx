use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crossterm::event::KeyCode;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use serde_json::Value;
use zdx_engine::config;
use zdx_engine::models::{
    available_models, bare_model_id, custom_provider_models, model_id_matches_patterns,
};
use zdx_engine::subagents::SubagentDefinition;
use zdx_engine::workspace_config::{
    self, HELPER_MODEL_KEYS, Layer, MODEL_MODES_KEY, Reach, SUBAGENTS_ENABLED_KEY, Scope, Setting,
    SettingValue, Source, Workspace,
};

use crate::app::{MonitorApp, TargetPickerState};
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

/// Overlay tag shown next to a row whose value comes from a workspace layer.
/// Global and default values stay untagged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowSource {
    pub label: String,
    /// Set in the viewed workspace's own overlay (not inherited).
    pub here: bool,
}

pub(crate) struct ConfigView {
    pub model: String,
    pub lines: Vec<ConfigLine>,
    pub sources: Vec<Option<RowSource>>,
    pub settings: Vec<Setting>,
}

/// Loads what `scope` shows: its own layer chain only, so the global view
/// never mixes in a workspace's values.
pub(crate) fn load_config_view(scope: Scope<'_>) -> anyhow::Result<ConfigView> {
    let (config, sources) = config::Config::load_layered_with_sources(&scope.layers())?;
    let lines = build_config_lines(&config, &scope.subagents().unwrap_or_default());
    let sources = config_source_labels(
        &lines,
        &sources,
        &config::paths::config_path(),
        &scope.file(),
    );
    Ok(ConfigView {
        model: config.model,
        lines,
        sources,
        settings: workspace_config::load_settings(scope)?,
    })
}

/// Which workspaces read `scope`'s file, for every setting it shows.
pub(crate) fn config_reach_for(
    scope: Scope<'_>,
    settings: &[Setting],
    workspaces: &[Workspace],
) -> BTreeMap<String, Reach> {
    let keys: Vec<String> = settings.iter().map(|setting| setting.key.clone()).collect();
    workspace_config::reach(&scope.file(), &keys, workspaces)
}

fn row_source(source: Option<&Path>, global: &Path, overlay: &Path) -> Option<RowSource> {
    let source = Source::classify(source, global, overlay);
    match source.layer {
        Layer::Default | Layer::Global => None,
        Layer::Workspace | Layer::Ancestor => Some(RowSource {
            label: source.label,
            here: source.layer == Layer::Workspace,
        }),
    }
}

fn config_source_labels(
    lines: &[ConfigLine],
    sources: &config::ConfigSources,
    global: &Path,
    overlay: &Path,
) -> Vec<Option<RowSource>> {
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
            let path = match section {
                "subagents" if key == "enabled" => SUBAGENTS_ENABLED_KEY.to_string(),
                "subagents" if value == SUBAGENT_DEFAULT_LABEL => return None,
                "subagents" => format!("subagents.overrides.{key}.model"),
                "core" | "helper models" => key.clone(),
                "model modes" if key == ADD_MODE_LABEL => return None,
                "model modes" => MODEL_MODES_KEY.to_string(),
                _ => format!("{section}.{key}"),
            };
            row_source(sources.source(&path), global, overlay)
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
pub fn build_config_lines(
    config: &config::Config,
    subagents: &[SubagentDefinition],
) -> Vec<ConfigLine> {
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
    let sub_lines = build_subagent_lines(config, subagents);
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
    config::mode_ref_name(value).is_some()
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
fn build_subagent_lines(config: &config::Config, defs: &[SubagentDefinition]) -> Vec<ConfigLine> {
    let mut out = vec![ConfigLine::Row(
        "enabled".to_string(),
        if config.subagents.enabled {
            "on".to_string()
        } else {
            "off".to_string()
        },
    )];
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
        out.push(ConfigLine::Row(def.name.clone(), display));
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
    // layout: 3 (tabs) + content + 3 (footer); panel borders take 2 more
    // rows and the destination strip takes its wrapped height
    let header = header_lines(app, app.terminal_width.saturating_sub(2)).len();
    (app.terminal_height.saturating_sub(8) as usize)
        .saturating_sub(header)
        .max(1)
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

/// Setting key (see `zdx_engine::workspace_config`) edited by a Config row path
/// from [`editable_model_fields`].
pub(crate) fn setting_key(field_path: &str) -> String {
    if field_path.starts_with("model_modes.") {
        return MODEL_MODES_KEY.to_string();
    }
    if field_path == SUBAGENTS_ENABLED_PATH {
        return SUBAGENTS_ENABLED_KEY.to_string();
    }
    if let Some(name) = field_path.strip_prefix("subagents.") {
        return format!("subagents.overrides.{name}.model");
    }
    field_path.to_string()
}

/// The shared setting behind the selected Config row, if it is editable.
pub(crate) fn selected_setting(app: &MonitorApp) -> Option<&Setting> {
    let fields = editable_model_fields(&app.config_lines);
    let key = setting_key(&fields.get(app.config_selected)?.path);
    app.config_settings
        .iter()
        .find(|setting| setting.key == key)
}

/// Merged config of the viewed scope's own chain: also the chain its file
/// sits in, so edits start from values the destination actually inherits.
fn effective_config(app: &MonitorApp) -> anyhow::Result<config::Config> {
    config::Config::load_layered(&app.config_scope().layers())
}

/// Destination label: `GLOBAL` for the global config file, else the
/// workspace name.
fn destination_label(scope: Scope<'_>) -> String {
    if scope.writes_global_file() {
        "GLOBAL".to_string()
    } else {
        scope.name().to_string()
    }
}

/// A change to one setting in the viewed scope's own file.
#[derive(Clone, Debug)]
pub enum Change {
    Spec(String),
    Enabled(bool),
    Modes(Vec<config::ModelMode>),
    /// Remove the key, so the value falls back to the layers below.
    Reset,
}

/// One write, bound to its file when it is requested. Writes to the global
/// config file, and writes with a `note`, wait in `config_confirm` for `y`;
/// the captured `file` is what `y` writes, whatever the view is by then.
#[derive(Clone, Debug)]
pub struct PendingWrite {
    pub file: PathBuf,
    /// `GLOBAL` or the workspace name.
    pub dest: String,
    pub global_file: bool,
    pub key: String,
    pub change: Change,
    /// Verb for the status line (`Set`, `Reset`, `Deleted mode fast from`).
    pub action: String,
    /// Value in the destination's chain now.
    pub old: String,
    /// Value after the write.
    pub new: String,
    /// Which workspaces read the file for this key, when known.
    pub effect: Option<String>,
    /// Side effect the user must accept (e.g. copying an inherited list).
    pub note: Option<String>,
}

impl PendingWrite {
    fn needs_confirm(&self) -> bool {
        self.global_file || self.note.is_some()
    }
}

fn value_label(value: Option<&str>) -> String {
    value.unwrap_or(SUBAGENT_DEFAULT_LABEL).to_string()
}

/// What a reset in the scope's own file falls back to.
fn fallback_label(setting: &Setting) -> String {
    format!(
        "{} [{}]",
        value_label(setting.inherited_value.as_deref()),
        setting.inherited_source.label
    )
}

fn modes_label(modes: &[config::ModelMode]) -> String {
    modes
        .iter()
        .map(|mode| {
            format!(
                "{} → {}",
                mode.name,
                zdx_engine::models::format_model_thinking(&mode.primary, mode.thinking)
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Display value of `change` once written.
fn change_label(change: &Change, setting: Option<&Setting>) -> String {
    let fallback = || setting.map_or_else(|| SUBAGENT_DEFAULT_LABEL.to_string(), fallback_label);
    match change {
        Change::Spec(spec) => spec.clone(),
        Change::Enabled(enabled) => if *enabled { "on" } else { "off" }.to_string(),
        Change::Modes(modes) if modes.is_empty() => format!("(no own modes) → {}", fallback()),
        Change::Modes(modes) => modes_label(modes),
        Change::Reset => format!("(removed) → {}", fallback()),
    }
}

/// Names that read the file, and the ones a higher layer keeps from it.
fn reach_text(reach: &Reach) -> String {
    let mut text = if reach.reads.is_empty() {
        "reaches no listed workspace".to_string()
    } else {
        format!("reaches {}", reach.reads.join(", "))
    };
    if !reach.skips.is_empty() {
        let skips: Vec<String> = reach
            .skips
            .iter()
            .map(|skip| {
                if skip.by == skip.workspace {
                    format!("{} (own value)", skip.workspace)
                } else {
                    format!("{} (via {})", skip.workspace, skip.by)
                }
            })
            .collect();
        let _ = write!(text, " · skips {}", skips.join(", "));
    }
    if !reach.unknown.is_empty() {
        let _ = write!(text, " · unknown {}", reach.unknown.join(", "));
    }
    text
}

/// Writes or resets `key` in the viewed scope's own file. A write to the
/// global config file, or one with a `note`, is held in `config_confirm`
/// until `y`; a reset of a key the file does not set writes nothing.
fn request_change(
    app: &mut MonitorApp,
    key: &str,
    change: Change,
    action: &str,
    note: Option<String>,
) {
    app.model_picker = None;
    let scope = app.config_scope();
    let setting = app.config_settings.iter().find(|s| s.key == key);
    if matches!(change, Change::Reset) && !setting.is_some_and(|s| s.set_in_own_file(scope)) {
        let message = format!(
            "Nothing to reset: {key} is not set in {}",
            destination_label(scope)
        );
        return app.set_status(message);
    }
    let write = PendingWrite {
        file: scope.file(),
        dest: destination_label(scope),
        global_file: scope.writes_global_file(),
        key: key.to_string(),
        old: value_label(setting.and_then(|s| s.value.as_deref())),
        new: change_label(&change, setting),
        effect: app.config_reach.get(key).map(reach_text),
        change,
        action: action.to_string(),
        note,
    };
    if write.needs_confirm() {
        app.config_confirm = Some(write);
    } else {
        apply_write(app, &write);
    }
}

/// Writes `write` to its captured file and reloads the tab.
fn apply_write(app: &mut MonitorApp, write: &PendingWrite) {
    let (path, key) = (&write.file, write.key.as_str());
    let result = match &write.change {
        Change::Spec(spec) => {
            workspace_config::set_setting(path, key, SettingValue::Spec(spec)).map(|()| true)
        }
        Change::Enabled(enabled) => {
            workspace_config::set_setting(path, key, SettingValue::Enabled(*enabled)).map(|()| true)
        }
        Change::Modes(modes) => {
            workspace_config::set_setting(path, key, SettingValue::Modes(modes)).map(|()| true)
        }
        Change::Reset => workspace_config::reset_setting(path, key),
    };
    let (dest, action) = (&write.dest, &write.action);
    match result {
        Ok(true) => {
            reload_config_lines(app);
            let message = write_outcome(app, write);
            app.set_status(message);
        }
        Ok(false) => app.set_status(format!("Nothing to reset: {key} is not set in {dest}")),
        Err(e) => app.set_status(format!("Failed: {action} {key} in {dest}: {e:#}")),
    }
}

/// Status line after a write: where it went, the new value, and who reads it.
fn write_outcome(app: &MonitorApp, write: &PendingWrite) -> String {
    let mut message = format!("{} {} in {}", write.action, write.key, write.dest);
    if app.config_scope().file() == write.file {
        if let Some(setting) = app.config_settings.iter().find(|s| s.key == write.key) {
            let _ = write!(message, " · now {}", value_label(setting.value.as_deref()));
        }
        if let Some(reach) = app.config_reach.get(&write.key) {
            let _ = write!(message, " · {}", reach_text(reach));
        }
    }
    message
}

/// Handles a key while a write waits for confirmation. Only `y` writes;
/// `Enter` does nothing, so a habitual Enter cannot write global.
fn handle_confirm_key(app: &mut MonitorApp, key: KeyCode) {
    match key {
        KeyCode::Char('y') => {
            if let Some(pending) = app.config_confirm.take() {
                apply_write(app, &pending);
            }
        }
        KeyCode::Esc | KeyCode::Char('n') => {
            app.config_confirm = None;
            app.set_status("Cancelled: nothing written");
        }
        _ => {}
    }
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
    let Ok(cfg) = effective_config(app) else {
        app.set_status("Failed to load config");
        return;
    };
    // A `mode:<name> → primary` row shows the ref, not a model id: seed the
    // picker with the ref so it preselects the mode entry.
    let current = current
        .split_once(" → ")
        .map_or(current.as_str(), |(head, _)| head)
        .to_string();
    let target = PickerTarget::Config(config_destination(app, &path));
    app.model_picker = Some(ModelPickerState::new(
        path,
        kind,
        &current,
        &cfg.providers,
        &cfg.model_modes,
        target,
    ));
}

/// Destination shown by the picker for the Config row at `field_path`.
fn config_destination(app: &MonitorApp, field_path: &str) -> ConfigDestination {
    let scope = app.config_scope();
    let key = setting_key(field_path);
    let inherited_modes = key == MODEL_MODES_KEY
        && app
            .config_settings
            .iter()
            .find(|s| s.key == key)
            .is_some_and(|s| s.value.is_some() && !s.set_in_own_file(scope));
    ConfigDestination {
        label: destination_label(scope),
        global_file: scope.writes_global_file(),
        note: inherited_modes.then(|| {
            format!(
                "{} has no own mode list: saving copies the inherited list into its file",
                scope.name()
            )
        }),
    }
}

/// Flips `subagents.enabled` in the viewed scope's file, starting from the
/// value its own chain resolves to.
fn toggle_subagents_enabled(app: &mut MonitorApp) {
    let Ok(cfg) = effective_config(app) else {
        app.set_status("Failed to load config");
        return;
    };
    request_change(
        app,
        SUBAGENTS_ENABLED_KEY,
        Change::Enabled(!cfg.subagents.enabled),
        "Set",
        None,
    );
}

/// Reloads config lines from disk after an edit or view switch, clamping
/// selection.
pub(crate) fn reload_config_lines(app: &mut MonitorApp) {
    let index = app.config_workspace_index;
    if let Some(workspace) = app.config_workspaces.get_mut(index) {
        workspace.has_overlay = workspace.overlay_path.is_file();
    }
    let scope = app.config_scope();
    let Ok(view) = load_config_view(scope) else {
        app.set_status("Failed to reload config");
        return;
    };
    let reach = config_reach_for(scope, &view.settings, &app.config_workspaces);
    let current = matches!(scope, Scope::Workspace(workspace) if workspace.current);
    if current {
        app.default_model = view.model;
    }
    app.config_lines = view.lines;
    app.config_sources = view.sources;
    app.config_settings = view.settings;
    app.config_reach = reach;
    app.config_line_count = rendered_line_count(&app.config_lines);
    let count = editable_model_fields(&app.config_lines).len();
    if app.config_selected >= count {
        app.config_selected = count.saturating_sub(1);
    }
}

/// Status after switching what the Config tab views.
fn viewing_status(app: &MonitorApp) -> String {
    let scope = app.config_scope();
    let path = scope.file();
    match scope {
        Scope::Global => format!(
            "Viewing global · edits write {} after y · a back to {}",
            path.display(),
            app.config_workspace().name
        ),
        Scope::Workspace(workspace) if workspace.overlay_is_global => format!(
            "Viewing {} · its file is the global config {} · edits ask y",
            workspace.name,
            path.display()
        ),
        Scope::Workspace(workspace) => {
            format!(
                "Viewing {} · edits write {}",
                workspace.name,
                path.display()
            )
        }
    }
}

/// Reloads after a view switch and keeps the same row selected when the new
/// view has it.
fn switch_view(app: &mut MonitorApp) {
    let selected = editable_model_fields(&app.config_lines)
        .get(app.config_selected)
        .map(|field| field.path.clone());
    app.config_scroll = 0;
    reload_config_lines(app);
    app.config_selected = selected
        .and_then(|path| {
            editable_model_fields(&app.config_lines)
                .iter()
                .position(|field| field.path == path)
        })
        .unwrap_or(0);
    ensure_config_selection_visible(app);
    let message = viewing_status(app);
    app.set_status(message);
}

/// `a`: switches between the viewed workspace and the global view, on the
/// same row.
fn toggle_global_view(app: &mut MonitorApp) {
    app.config_global_view = !app.config_global_view;
    switch_view(app);
}

/// Picker label: name and tags first, path last, so a narrow popup trims the
/// path instead of the name.
fn workspace_picker_item(workspace: &Workspace, home: Option<&Path>) -> String {
    let mut item = workspace.name.clone();
    if workspace.current {
        item.push_str("  · monitor root");
    }
    if !workspace.profiles.is_empty() {
        let _ = write!(item, "  · {}", workspace.profiles.join(", "));
    }
    if workspace.overlay_is_global {
        item.push_str("  · ⚠ file = global config");
    }
    let _ = write!(item, "  {}", home_relative(&workspace.root, home));
    item
}

fn home_relative(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(relative) if relative.as_os_str().is_empty() => "~".to_string(),
        Some(relative) => format!("~/{}", relative.display()),
        None => path.display().to_string(),
    }
}

/// Re-discovers workspaces and opens the picker. The first entry is the
/// global view; workspaces follow.
fn open_workspace_picker(app: &mut MonitorApp) {
    match workspace_config::discover(Some(&app.root)) {
        Ok(list) if !list.is_empty() => {
            let viewed = app.config_workspace().clone();
            let found = list.iter().position(|w| w.root == viewed.root);
            app.config_workspace_index = found.unwrap_or(0);
            app.config_workspaces = list;
            // Rows, header, and the next write must describe the same
            // workspace, even if the picker is then cancelled.
            if found.is_none() {
                if !app.config_global_view {
                    app.config_scroll = 0;
                    reload_config_lines(app);
                }
                let message = format!(
                    "{} is no longer listed · {} {}",
                    viewed.name,
                    if app.config_global_view {
                        "a now returns to"
                    } else {
                        "now viewing"
                    },
                    app.config_workspace().name
                );
                app.set_status(message);
            }
        }
        Ok(_) => {}
        Err(e) => app.set_status(format!("Failed to list workspaces: {e:#}")),
    }
    let home = config::paths::home_dir();
    let global = format!(
        "global · all workspaces  {}",
        home_relative(&config::paths::config_path(), home.as_deref())
    );
    let mut items = vec![(
        global,
        workspace_config::own_override_count(Scope::Global).ok(),
    )];
    items.extend(app.config_workspaces.iter().map(|workspace| {
        (
            workspace_picker_item(workspace, home.as_deref()),
            workspace_config::own_override_count(Scope::Workspace(workspace)).ok(),
        )
    }));
    let mut picker = TargetPickerState::from_maybe_counted(items);
    let viewed = if app.config_global_view {
        0
    } else {
        app.config_workspace_index + 1
    };
    picker.selected = viewed.min(picker.matches.len().saturating_sub(1));
    app.config_workspace_picker = Some(picker);
}

/// Handles a Config-tab key, including every key while the workspace picker
/// or a global-write confirmation is open. Returns `true` if the key was
/// consumed.
pub(crate) fn handle_config_key(app: &mut MonitorApp, key: KeyCode) -> bool {
    if app.config_confirm.is_some() {
        handle_confirm_key(app, key);
        return true;
    }
    if app.config_workspace_picker.is_some() {
        handle_workspace_picker_key(app, key);
        return true;
    }
    match key {
        KeyCode::Char('d') | KeyCode::Delete => delete_or_reset_selected(app),
        KeyCode::Char('D') => reset_selected_modes(app),
        KeyCode::Char('p') => open_workspace_picker(app),
        KeyCode::Char('a') => toggle_global_view(app),
        _ => return false,
    }
    true
}

/// Handles a key while the workspace picker is open. Consumes every key.
fn handle_workspace_picker_key(app: &mut MonitorApp, key: KeyCode) {
    let Some(picker) = app.config_workspace_picker.as_mut() else {
        return;
    };
    match key {
        KeyCode::Esc => app.config_workspace_picker = None,
        KeyCode::Down => {
            let last = picker.matches.len().saturating_sub(1);
            picker.selected = (picker.selected + 1).min(last);
        }
        KeyCode::Up => picker.selected = picker.selected.saturating_sub(1),
        KeyCode::Backspace => {
            picker.filter.pop();
            picker.recompute();
        }
        KeyCode::Char(c) => {
            picker.filter.push(c);
            picker.recompute();
        }
        KeyCode::Enter => {
            let chosen = picker.matches.get(picker.selected).copied();
            app.config_workspace_picker = None;
            match chosen {
                Some(0) => app.config_global_view = true,
                Some(index) => {
                    app.config_global_view = false;
                    app.config_workspace_index = index - 1;
                }
                None => return,
            }
            switch_view(app);
        }
        _ => {}
    }
}

/// Where a Config edit goes, as the picker shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfigDestination {
    /// `GLOBAL` or the workspace name.
    pub label: String,
    /// The global config file: the picker is colored for it and the write
    /// asks `y`.
    pub global_file: bool,
    /// Warning shown above the list (e.g. a first mode edit copies the
    /// inherited list).
    pub note: Option<String>,
}

/// Where a committed model choice is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickerTarget {
    /// The Config tab field in `ModelPickerState::field`, in the viewed
    /// scope's own file.
    Config(ConfigDestination),
    /// The `model:` of one job file. `declared` is the value on disk
    /// when the picker opened; the save refuses if it changed since.
    Job {
        name: String,
        path: PathBuf,
        declared: Option<String>,
    },
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
    /// Where the choice is saved.
    pub target: PickerTarget,
}

impl ModelPickerState {
    pub(crate) fn new(
        field: String,
        kind: ModelFieldKind,
        current: &str,
        providers: &config::ProvidersConfig,
        modes: &[config::ModelMode],
        target: PickerTarget,
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
            target,
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

/// Applies a mode edit for a `model_modes.<i>`/`model_modes.add` field to the
/// mode list of the viewed scope's chain and writes the whole list to its
/// file (arrays replace wholesale across layers). The global view's chain is
/// the global file alone, so workspace modes never reach global. Only the
/// primary spec is edited here; `description`/`alternatives` stay TOML-only
/// and are preserved on edit.
fn save_mode(app: &mut MonitorApp, field: &str, model: &str, level: config::ThinkingLevel) {
    let mut cfg = match effective_config(app) {
        Ok(cfg) => cfg,
        Err(e) => return app.set_status(format!("Failed to load config: {e:#}")),
    };
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
    } else if let Some(mode) = suffix
        .parse::<usize>()
        .ok()
        .and_then(|idx| cfg.model_modes.get_mut(idx))
    {
        mode.primary = model.to_string();
        mode.thinking = level;
    } else {
        return app.set_status(format!("Invalid mode field: {field}"));
    }
    request_change(
        app,
        MODEL_MODES_KEY,
        Change::Modes(cfg.model_modes),
        "Set",
        None,
    );
}

/// Warning for a mode delete when the viewed scope has no mode list of its
/// own: the remaining `kept` inherited modes become its own list, which stops
/// following the layer it inherits from.
fn inherited_modes_note(app: &MonitorApp, kept: usize) -> Option<String> {
    let scope = app.config_scope();
    let setting = app
        .config_settings
        .iter()
        .find(|s| s.key == MODEL_MODES_KEY)?;
    if setting.set_in_own_file(scope) {
        return None;
    }
    Some(format!(
        "{} has no own mode list: this writes the other {kept} inherited modes into its file, and later mode changes in [{}] stop reaching it",
        scope.name(),
        setting.source.label
    ))
}

/// Handles `d`/`Del` on the Config tab: removes the selected mode (writing
/// the remaining list to the viewed scope's file), or resets any other
/// selected setting in that file so it falls back to the layers below.
fn delete_or_reset_selected(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    let Some(path) = fields.get(app.config_selected).map(|f| f.path.clone()) else {
        return;
    };

    let Some(suffix) = path.strip_prefix("model_modes.") else {
        request_change(app, &setting_key(&path), Change::Reset, "Reset", None);
        return;
    };
    let Ok(idx) = suffix.parse::<usize>() else {
        return; // add-row: nothing to delete
    };
    let mut cfg = match effective_config(app) {
        Ok(cfg) => cfg,
        Err(e) => return app.set_status(format!("Failed to load config: {e:#}")),
    };
    if idx >= cfg.model_modes.len() {
        return;
    }
    let removed = cfg.model_modes.remove(idx);
    let note = inherited_modes_note(app, cfg.model_modes.len());
    request_change(
        app,
        MODEL_MODES_KEY,
        Change::Modes(cfg.model_modes),
        &format!("Deleted mode {} from", removed.name),
        note,
    );
}

/// Handles `D` on a mode row: resets the whole mode list in the viewed
/// scope's file.
fn reset_selected_modes(app: &mut MonitorApp) {
    let fields = editable_model_fields(&app.config_lines);
    if fields
        .get(app.config_selected)
        .is_some_and(|f| f.path.starts_with("model_modes."))
    {
        request_change(app, MODEL_MODES_KEY, Change::Reset, "Reset", None);
    }
}

/// Persists the picker's chosen model (+ thinking for chat models) to the
/// viewed scope's file.
fn commit_model_picker(app: &mut MonitorApp) {
    let Some(picker) = app.model_picker.as_ref() else {
        return;
    };
    let field = picker.field.clone();
    let model = picker.chosen_model.clone();
    let level = picker.selected_thinking();
    if let PickerTarget::Job { .. } = picker.target {
        let spec = zdx_engine::models::format_model_thinking(&model, level);
        crate::tabs::jobs::save_picked_model(app, &spec);
        return;
    }

    let spec = match picker.kind {
        // Model mode: update the entry at `model_modes.<i>` or append via
        // `model_modes.add`; thinking rides in the saved primary spec.
        ModelFieldKind::Chat if field.starts_with("model_modes.") => {
            return save_mode(app, &field, &model, level);
        }
        // A mode ref is stored verbatim: the mode's primary carries the level.
        ModelFieldKind::Chat if is_mode_ref(&model) => model,
        // Chat models carry thinking inline as `model@thinking`.
        ModelFieldKind::Chat => zdx_engine::models::format_model_thinking(&model, level),
        // Audio (STT/TTS): model only, no thinking.
        ModelFieldKind::Transcription | ModelFieldKind::Speech => model,
    };
    request_change(app, &setting_key(&field), Change::Spec(spec), "Set", None);
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
                let source = app.config_sources.get(idx).and_then(Option::as_ref);
                let source_span = source_span(source);
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

    let header = header_lines(app, area.width.saturating_sub(2));
    let header_rows = u16::try_from(header.len()).unwrap_or(MAX_HEADER_ROWS);
    let visible_lines = area.height.saturating_sub(2 + header_rows) as usize;
    let total_lines = lines.len();

    let scroll_info = scroll_percent(app.config_scroll, total_lines, visible_lines);

    let scope = app.config_scope();
    let title = format!(
        " Config · {} ({total_fields} fields){scroll_info} ",
        scope.name()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(destination_color(scope)))
        .title(title);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_rows), Constraint::Min(0)])
        .split(inner);
    f.render_widget(Paragraph::new(Text::from(header)), rows[0]);
    f.render_widget(
        Paragraph::new(Text::from(lines)).scroll((app.config_scroll as u16, 0)),
        rows[1],
    );
}

/// ` [N%]` scroll position when the list overflows its area.
fn scroll_percent(scroll: usize, total: usize, visible: usize) -> String {
    if total <= visible {
        return String::new();
    }
    let max_scroll = total - visible;
    let percent = (scroll.min(max_scroll) * 100)
        .checked_div(max_scroll)
        .unwrap_or(100);
    format!(" [{percent}%]")
}

/// Most rows the destination strip takes above the config list; a longer
/// strip ends in `…`.
const MAX_HEADER_ROWS: u16 = 8;

/// Yellow for a workspace file, magenta for the global config file.
fn destination_color(scope: Scope<'_>) -> Color {
    if scope.writes_global_file() {
        Color::Magenta
    } else {
        Color::Yellow
    }
}

/// Destination strip, wrapped to `width`: where edits go (and, for the global
/// file, that writes ask `y`), then the selected row's source and what a reset
/// falls back to, then which workspaces an edit reaches.
fn header_lines(app: &MonitorApp, width: u16) -> Vec<Line<'static>> {
    let scope = app.config_scope();
    let home = config::paths::home_dir();
    let path = home_relative(&scope.file(), home.as_deref());
    let dim = Style::default().fg(Color::DarkGray);
    let accent = Style::default()
        .fg(destination_color(scope))
        .add_modifier(Modifier::BOLD);
    let mut first = match scope {
        Scope::Global => vec![Span::styled(" edits → GLOBAL · all workspaces ", accent)],
        Scope::Workspace(workspace) if workspace.overlay_is_global => vec![Span::styled(
            format!(" ⚠ {}'s file is the global config ", workspace.name),
            accent,
        )],
        Scope::Workspace(workspace) => {
            vec![Span::styled(
                format!(" edits → {} ", workspace.name),
                accent,
            )]
        }
    };
    if scope.writes_global_file() {
        first.push(Span::styled(
            "· every write asks y ",
            Style::default().fg(Color::Magenta),
        ));
    }
    first.push(Span::styled(path, dim));
    if let Scope::Workspace(workspace) = scope
        && !workspace.overlay_is_global
        && !workspace.has_overlay
    {
        first.push(Span::styled(" (created on first write)", dim));
    }
    let mut lines = vec![Line::from(first)];
    let width = usize::from(width.max(2));
    for text in selected_row_hint(app) {
        lines.extend(
            wrap_words(&text, width - 1)
                .into_iter()
                .map(|row| Line::from(Span::styled(format!(" {row}"), dim))),
        );
    }
    let max = usize::from(MAX_HEADER_ROWS);
    if lines.len() > max {
        lines.truncate(max);
        let last = lines[max - 1].to_string();
        lines[max - 1] = Line::from(Span::styled(
            truncate_chars(&format!("{last}…"), width),
            dim,
        ));
    }
    lines
}

/// Splits `text` into rows of at most `width` chars, breaking at spaces and
/// splitting longer words. Indents continuation rows by one space.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    let mut row = String::new();
    for word in text.split(' ').filter(|word| !word.is_empty()) {
        let mut word: Vec<char> = word.chars().collect();
        loop {
            let used = row.chars().count();
            let gap = usize::from(!row.trim().is_empty());
            if used + gap + word.len() <= width {
                if gap == 1 {
                    row.push(' ');
                }
                row.extend(word.iter());
                break;
            }
            if gap == 1 {
                rows.push(std::mem::replace(&mut row, " ".to_string()));
                continue;
            }
            let room = width.saturating_sub(used).max(1);
            let rest = word.split_off(room.min(word.len()));
            row.extend(word.iter());
            rows.push(std::mem::replace(&mut row, " ".to_string()));
            if rest.is_empty() {
                break;
            }
            word = rest;
        }
    }
    if row.trim().is_empty() {
        if rows.is_empty() {
            rows.push(String::new());
        }
    } else {
        rows.push(row);
    }
    rows
}

/// The selected row: where its value comes from in this view and what a
/// reset of the view's file falls back to, then who an edit reaches.
fn selected_row_hint(app: &MonitorApp) -> Vec<String> {
    let Some(setting) = selected_setting(app) else {
        return Vec::new();
    };
    let scope = app.config_scope();
    let dest = destination_label(scope);
    let mut origin = format!("{}: ", setting.label);
    if setting.set_in_own_file(scope) {
        let reset_key = if setting.key == MODEL_MODES_KEY {
            "D"
        } else {
            "d"
        };
        let _ = write!(
            origin,
            "set in {dest} · {reset_key} → {}",
            fallback_label(setting)
        );
    } else {
        let _ = write!(origin, "from {} · not set in {dest}", setting.source.label);
    }
    let mut out = vec![origin];
    if let Some(reach) = app.config_reach.get(&setting.key) {
        out.push(format!("edit {}", reach_text(reach)));
    }
    out
}

/// Row tag: overrides set in the viewed workspace stand out; inherited
/// ancestor values stay dim.
fn source_span(source: Option<&RowSource>) -> Span<'static> {
    Span::styled(
        source.map_or_else(String::new, |source| format!(" [{}]", source.label)),
        if source.is_some_and(|source| source.here) {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        },
    )
}

/// Confirmation popup for a pending write: the captured file, the old and
/// new value, who reads it, and any side effect. Only `y` writes.
pub(crate) fn render_pending_write(f: &mut Frame, app: &MonitorApp, area: Rect) {
    let Some(pending) = &app.config_confirm else {
        return;
    };
    let popup = centered_rect(70, 60, area);
    f.render_widget(Clear, popup);
    let home = config::paths::home_dir();
    let file = home_relative(&pending.file, home.as_deref());
    let label = Style::default().fg(Color::DarkGray);
    let row = |name: &str, value: String| {
        Line::from(vec![
            Span::styled(format!(" {name:<8}"), label),
            Span::raw(value),
        ])
    };
    let mut lines = vec![
        row("file", file),
        row("key", pending.key.clone()),
        row("now", pending.old.clone()),
        row("new", pending.new.clone()),
    ];
    if let Some(effect) = &pending.effect {
        lines.push(row("effect", effect.clone()));
    }
    if let Some(note) = &pending.note {
        lines.push(Line::from(Span::styled(
            format!(" ⚠ {note}"),
            Style::default().fg(Color::Yellow),
        )));
    }
    let color = if pending.global_file {
        Color::Magenta
    } else {
        Color::Yellow
    };
    let (title_dest, button) = if pending.global_file {
        ("GLOBAL config".to_string(), "write global".to_string())
    } else {
        (pending.dest.clone(), format!("write {}", pending.dest))
    };
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(
            " y ",
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!("{button}   ")),
        Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" cancel   (Enter does nothing here)"),
    ]));
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(format!(
            " {} {} in {title_dest}? ",
            pending.action, pending.key
        ));
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .wrap(ratatui::widgets::Wrap { trim: false })
            .block(block),
        popup,
    );
}

/// Footer hint for the Config tab: the keys that apply right now.
pub(crate) fn config_footer_hint(app: &MonitorApp) -> String {
    if let Some(pending) = &app.config_confirm {
        let dest = if pending.global_file {
            "global"
        } else {
            &pending.dest
        };
        return format!("y write {dest} • Esc cancel • Enter does nothing here");
    }
    let view = if app.config_global_view {
        format!("a back to {}", app.config_workspace().name)
    } else {
        "a global view".to_string()
    };
    format!(
        "↑↓ select • Enter edit • d reset / delete mode • D reset modes • p view • {view} • PgUp/PgDn scroll • q quit"
    )
}

/// Picker heading and border color: a Config edit names its destination.
fn picker_heading(picker: &ModelPickerState) -> (String, Color, &'static str) {
    match &picker.target {
        PickerTarget::Config(dest) if dest.global_file => (
            format!("Set {} in {}", picker.field, dest.label),
            Color::Magenta,
            "Enter review",
        ),
        PickerTarget::Config(dest) => (
            format!("Set {} in {}", picker.field, dest.label),
            Color::Yellow,
            "Enter save",
        ),
        PickerTarget::Job { .. } => (picker.field.clone(), Color::Cyan, "Enter save"),
    }
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
    let (heading, color, save) = picker_heading(picker);
    let confirm = if picker.kind == ModelFieldKind::Chat {
        "Enter next"
    } else {
        save
    };
    let title = format!(
        " {heading} · pick model · {} match · {confirm} · Esc cancel ",
        picker.matches.len(),
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
        .title(title);
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let note = match &picker.target {
        PickerTarget::Config(dest) => dest.note.clone(),
        PickerTarget::Job { .. } => None,
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(u16::from(note.is_some())),
            Constraint::Min(0),
        ])
        .split(inner);
    if let Some(note) = note {
        f.render_widget(
            Paragraph::new(Span::styled(
                format!("⚠ {note}"),
                Style::default().fg(Color::Yellow),
            )),
            rows[1],
        );
    }
    let list_area = rows[2];

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

    let visible = list_area.height as usize;
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

    f.render_widget(List::new(items), list_area);
}

fn render_picker_thinking(f: &mut Frame, picker: &ModelPickerState, popup: Rect) {
    use zdx_engine::config::ThinkingLevel;

    let (heading, color, save) = picker_heading(picker);
    let title = format!(
        " {heading} · thinking for {} · {save} · Esc back ",
        picker.chosen_model,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(color))
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

    #[test]
    fn workspace_picker_item_puts_tags_before_home_relative_path() {
        let workspace = Workspace {
            root: "/home/user/projects/example".into(),
            name: "example".to_string(),
            profiles: vec!["alpha".to_string(), "beta".to_string()],
            current: true,
            overlay_path: "/home/user/projects/example/.zdx/config.toml".into(),
            has_overlay: true,
            overlay_is_global: false,
        };
        assert_eq!(
            workspace_picker_item(&workspace, Some(Path::new("/home/user"))),
            "example  · monitor root  · alpha, beta  ~/projects/example"
        );
        assert_eq!(
            workspace_picker_item(&workspace, Some(Path::new("/other"))),
            "example  · monitor root  · alpha, beta  /home/user/projects/example"
        );
    }

    fn subagent_defs(root: &Path) -> Vec<SubagentDefinition> {
        zdx_engine::subagents::discover(root).unwrap()
    }

    fn label(source: Option<&RowSource>) -> Option<&str> {
        source.map(|source| source.label.as_str())
    }

    #[test]
    fn rows_map_to_shared_setting_keys() {
        assert_eq!(setting_key("model"), "model");
        assert_eq!(setting_key("transcription.model"), "transcription.model");
        assert_eq!(setting_key("model_modes.2"), MODEL_MODES_KEY);
        assert_eq!(setting_key("model_modes.add"), MODEL_MODES_KEY);
        assert_eq!(setting_key(SUBAGENTS_ENABLED_PATH), SUBAGENTS_ENABLED_KEY);
        assert_eq!(
            setting_key("subagents.orchestrator"),
            "subagents.overrides.orchestrator.model"
        );
    }

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

        let workspace =
            workspace_config::workspaces_from(&std::collections::BTreeMap::default(), Some(&root))
                .remove(0);
        let view = load_config_view(Scope::Workspace(&workspace)).unwrap();
        let row = row_index(&view.lines, "core", "model");
        assert_eq!(view.model, "selected@high");
        assert_eq!(
            view.sources[row],
            Some(RowSource {
                label: "selected-root".into(),
                here: true
            })
        );
        let model = view.settings.iter().find(|s| s.key == "model").unwrap();
        assert!(model.overridden_here());
        assert_eq!(model.inherited_value.as_deref(), Some("global@low"));
        assert!(matches!(&view.lines[row], ConfigLine::Row(_, value) if value == "selected@high"));
        std::fs::write(&overlay, "model = \"changed@medium\"\n").unwrap();
        assert_eq!(
            load_config_view(Scope::Workspace(&workspace))
                .unwrap()
                .model,
            "changed@medium"
        );
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
            let lines = build_config_lines(
                &cfg,
                &subagent_defs(child.parent().unwrap().parent().unwrap()),
            );
            let labels = config_source_labels(&lines, &sources, &global, &child);
            assert_eq!(
                label(labels[row_index(&lines, "core", "model")].as_ref()),
                Some(expected)
            );
            assert_eq!(labels[row_index(&lines, "core", "max_tokens")], None);
            assert_eq!(
                label(labels[row_index(&lines, "model modes", "work")].as_ref()),
                Some("parity")
            );
            assert_eq!(
                labels[row_index(&lines, "model modes", ADD_MODE_LABEL)],
                None
            );
            let secret_row = row_index(&lines, "providers", "openai.api_key");
            assert_eq!(label(labels[secret_row].as_ref()), Some("parity"));
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
        let lines = build_config_lines(&cfg, &subagent_defs(home.path()));
        let labels = config_source_labels(
            &lines,
            &sources,
            &global,
            &home.path().join(".zdx/config.toml"),
        );
        assert_eq!(
            label(labels[row_index(&lines, "core", "model")].as_ref()),
            Some("parity")
        );
        assert_eq!(
            label(labels[row_index(&lines, "subagents", "explorer")].as_ref()),
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
        let lines = build_config_lines(&config, &[]);

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
            PickerTarget::Config(ConfigDestination::default()),
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
            PickerTarget::Config(ConfigDestination::default()),
        );
        assert!(!helper.items.iter().any(|item| is_mode_ref(item)));
        assert!(helper.has_thinking());
    }

    #[test]
    fn build_config_lines_orders_model_sections_first() {
        let config = config::Config::default();
        let lines = build_config_lines(&config, &[]);

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
            PickerTarget::Config(ConfigDestination::default()),
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
            PickerTarget::Config(ConfigDestination::default()),
        );
        assert!(enabled.items.contains(&model.qualified_id()));

        providers.get_mut(kind).enabled = Some(false);
        let disabled = ModelPickerState::new(
            "model".to_string(),
            ModelFieldKind::Chat,
            "",
            &providers,
            &[],
            PickerTarget::Config(ConfigDestination::default()),
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
            PickerTarget::Config(ConfigDestination::default()),
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
            PickerTarget::Config(ConfigDestination::default()),
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
            PickerTarget::Config(ConfigDestination::default()),
        );
        assert_eq!(p2.thinking_current, config::ThinkingLevel::Low);
    }

    mod scope_edits {
        use std::fs;
        use std::os::unix::fs::symlink;

        use ratatui::backend::TestBackend;
        use zdx_engine::test_support::TestZdxHomeGuard;

        use super::*;
        use crate::app::Section;

        const GLOBAL: &str = "model = \"global:model@low\"\n\
            [subagents]\nenabled = true\n\
            [[model_modes]]\nname = \"base\"\nprimary = \"global:base@low\"\n";
        const OVERLAY: &str = "model = \"ws:model@high\"\n\
            [subagents]\nenabled = false\n\
            [[model_modes]]\nname = \"local\"\nprimary = \"ws:local@high\"\n";

        struct Fixture {
            home: TestZdxHomeGuard,
            app: MonitorApp,
            global: PathBuf,
            overlay: PathBuf,
        }

        impl Fixture {
            fn new() -> Self {
                let home = zdx_engine::test_support::temp_zdx_home();
                let global = config::paths::config_path();
                fs::write(&global, GLOBAL).unwrap();
                let root = home.path().join("ws");
                fs::create_dir_all(root.join(".zdx")).unwrap();
                let overlay = root.join(".zdx/config.toml");
                fs::write(&overlay, OVERLAY).unwrap();
                let app = app_at(&root);
                Self {
                    home,
                    app,
                    global,
                    overlay,
                }
            }

            fn global_config(&self) -> config::Config {
                config::Config::load_layered(std::slice::from_ref(&self.global)).unwrap()
            }
        }

        fn app_at(root: &Path) -> MonitorApp {
            let mut app = crate::app::build_app(root).unwrap();
            app.active_section = Section::Config;
            app.status_section = Section::Config;
            app
        }

        fn select(app: &mut MonitorApp, path: &str) {
            app.config_selected = editable_model_fields(&app.config_lines)
                .iter()
                .position(|field| field.path == path)
                .unwrap();
        }

        fn selected_path(app: &MonitorApp) -> String {
            editable_model_fields(&app.config_lines)[app.config_selected]
                .path
                .clone()
        }

        fn pick(app: &mut MonitorApp, path: &str, model: &str) {
            select(app, path);
            open_model_picker(app);
            app.model_picker.as_mut().unwrap().chosen_model = model.to_string();
            commit_model_picker(app);
        }

        fn screen(app: &MonitorApp) -> String {
            let mut terminal = Terminal::new(TestBackend::new(220, 30)).unwrap();
            terminal.draw(|f| crate::ui::render(f, app)).unwrap();
            let buffer = terminal.backend().buffer();
            (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }

        /// The tab opens on the workspace and writes its overlay without a
        /// confirmation; the global file is untouched.
        #[test]
        fn workspace_view_is_the_default_and_writes_the_overlay() {
            let mut fx = Fixture::new();
            assert!(matches!(fx.app.config_scope(), Scope::Workspace(w) if w.name == "ws"));

            pick(&mut fx.app, "tldr_model", "ws:tldr");

            assert!(fx.app.config_confirm.is_none());
            assert!(
                fs::read_to_string(&fx.overlay)
                    .unwrap()
                    .contains("ws:tldr@low")
            );
            assert_eq!(fs::read_to_string(&fx.global).unwrap(), GLOBAL);
            assert!(fx.app.status_message.starts_with("Set tldr_model in ws"));
        }

        /// In the global view, mode edits and the subagents toggle start from
        /// global's own values, and nothing is written before `y`.
        #[test]
        fn global_view_edits_start_from_global_values_and_wait_for_y() {
            let mut fx = Fixture::new();
            select(&mut fx.app, "model_modes.0");
            assert!(handle_config_key(&mut fx.app, KeyCode::Char('a')));
            assert_eq!(fx.app.config_scope(), Scope::Global);
            assert_eq!(selected_path(&fx.app), "model_modes.0");

            pick(&mut fx.app, "model_modes.0", "global:edited");
            let text = screen(&fx.app);
            assert!(text.contains("Set model_modes in GLOBAL config?"), "{text}");
            assert!(text.contains("base → global:base@low"), "{text}");
            assert!(handle_config_key(&mut fx.app, KeyCode::Enter));
            assert!(fx.app.config_confirm.is_some(), "Enter must not confirm");
            assert_eq!(fs::read_to_string(&fx.global).unwrap(), GLOBAL);
            assert!(handle_config_key(&mut fx.app, KeyCode::Char('y')));
            let modes = fx.global_config().model_modes;
            assert_eq!(modes.len(), 1);
            assert_eq!(modes[0].name, "base");
            assert_eq!(modes[0].primary, "global:edited@low");

            select(&mut fx.app, SUBAGENTS_ENABLED_PATH);
            open_model_picker(&mut fx.app);
            assert!(fx.global_config().subagents.enabled);
            handle_config_key(&mut fx.app, KeyCode::Char('y'));
            assert!(!fx.global_config().subagents.enabled);

            select(&mut fx.app, "model");
            handle_config_key(&mut fx.app, KeyCode::Char('d'));
            handle_config_key(&mut fx.app, KeyCode::Esc);
            assert!(fx.app.config_confirm.is_none());
            assert_eq!(fx.global_config().model, "global:model@low");
            assert_eq!(fs::read_to_string(&fx.overlay).unwrap(), OVERLAY);

            handle_config_key(&mut fx.app, KeyCode::Char('a'));
            assert!(matches!(fx.app.config_scope(), Scope::Workspace(_)));
        }

        /// The strip names the destination, and the reset hint uses the
        /// viewed file's own fallback chain.
        #[test]
        fn header_names_destination_and_target_correct_reset() {
            let mut fx = Fixture::new();
            select(&mut fx.app, "model");
            let text = screen(&fx.app);
            assert!(text.contains("edits → ws"), "{text}");
            assert!(
                text.contains(" model: set in ws · d → global:model@low [global] "),
                "{text}"
            );
            assert!(text.contains(" edit reaches ws "), "{text}");

            handle_config_key(&mut fx.app, KeyCode::Char('a'));
            let text = screen(&fx.app);
            assert!(text.contains("edits → GLOBAL · all workspaces"), "{text}");
            assert!(text.contains("every write asks y"), "{text}");
            let default_model = config::Config::default().model;
            assert!(
                text.contains(&format!(
                    " model: set in GLOBAL · d → {default_model} [default] "
                )),
                "{text}"
            );
            assert!(
                text.contains(" edit reaches no listed workspace · skips ws (own value) "),
                "{text}"
            );

            select(&mut fx.app, "tldr_model");
            open_model_picker(&mut fx.app);
            assert!(screen(&fx.app).contains("Set tldr_model in GLOBAL"));
        }

        /// A workspace whose overlay is the global file (home with
        /// `$ZDX_HOME` at `~/.zdx`) is labelled as the global config and its
        /// writes wait for `y`.
        #[test]
        fn workspace_aliasing_the_global_file_asks_before_writing() {
            let home = zdx_engine::test_support::temp_zdx_home();
            let global = config::paths::config_path();
            fs::write(&global, GLOBAL).unwrap();
            let root = home.path().join("home-dir");
            fs::create_dir_all(&root).unwrap();
            symlink(home.path(), root.join(".zdx")).unwrap();
            let mut app = app_at(&root);
            assert!(app.config_workspace().overlay_is_global);

            let text = screen(&app);
            assert!(text.contains("file is the global config"), "{text}");
            assert!(text.contains("every write asks y"), "{text}");

            pick(&mut app, "tldr_model", "home:tldr");
            assert!(app.config_confirm.is_some());
            assert_eq!(fs::read_to_string(&global).unwrap(), GLOBAL);
            handle_config_key(&mut app, KeyCode::Char('y'));
            assert!(
                fs::read_to_string(&global)
                    .unwrap()
                    .contains("home:tldr@low")
            );
        }

        fn alias_app(home: &TestZdxHomeGuard) -> MonitorApp {
            let root = home.path().join("home-dir");
            fs::create_dir_all(&root).unwrap();
            symlink(home.path(), root.join(".zdx")).unwrap();
            let app = app_at(&root);
            assert!(app.config_workspace().overlay_is_global);
            app
        }

        fn add_profile(global: &Path, name: &str, cwd: &Path) {
            let mut text = fs::read_to_string(global).unwrap();
            let _ = write!(
                text,
                "\n[telegram.profiles.{name}]\nchat_id = -100\ncwd = \"{}\"\n",
                cwd.display()
            );
            fs::write(global, text).unwrap();
        }

        fn key(app: &mut MonitorApp, code: KeyCode) {
            crate::app::handle_key_event(
                app,
                crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::NONE),
            );
        }

        fn type_text(app: &mut MonitorApp, text: &str) {
            for c in text.chars() {
                key(app, KeyCode::Char(c));
            }
        }

        /// A link to `$ZDX_HOME` is the global file even before that file
        /// exists: the first write still waits for `y`.
        #[test]
        fn alias_to_missing_global_file_still_asks() {
            let home = zdx_engine::test_support::temp_zdx_home();
            let global = config::paths::config_path();
            let mut app = alias_app(&home);
            assert!(!global.exists());
            pick(&mut app, "tldr_model", "home:tldr");
            assert!(app.config_confirm.as_ref().is_some_and(|w| w.global_file));
            assert!(!global.exists(), "nothing is written before y");
        }

        /// `d` and `D` in a workspace whose file is the global config wait
        /// for `y` like any other global write.
        #[test]
        fn alias_workspace_resets_and_mode_deletes_wait_for_y() {
            let home = zdx_engine::test_support::temp_zdx_home();
            let global = config::paths::config_path();
            fs::write(&global, GLOBAL).unwrap();
            let mut app = alias_app(&home);
            let global_config = || config::Config::load_layered(std::slice::from_ref(&global));

            select(&mut app, "model");
            handle_config_key(&mut app, KeyCode::Char('d'));
            assert!(app.config_confirm.is_some());
            assert_eq!(fs::read_to_string(&global).unwrap(), GLOBAL);
            handle_config_key(&mut app, KeyCode::Char('y'));
            assert_eq!(
                global_config().unwrap().model,
                config::Config::default().model
            );

            select(&mut app, "model_modes.0");
            handle_config_key(&mut app, KeyCode::Char('d'));
            let pending = app.config_confirm.as_ref().expect("mode delete asks");
            assert!(pending.note.is_none(), "the list is the file's own");
            handle_config_key(&mut app, KeyCode::Esc);
            assert_eq!(global_config().unwrap().model_modes.len(), 1);

            handle_config_key(&mut app, KeyCode::Char('D'));
            assert!(app.config_confirm.is_some());
            assert_eq!(global_config().unwrap().model_modes.len(), 1);
            handle_config_key(&mut app, KeyCode::Char('y'));
            assert!(global_config().unwrap().model_modes.is_empty());
        }

        /// Resetting a key the destination does not set writes nothing and
        /// asks nothing, in a workspace and in the global view.
        #[test]
        fn reset_of_a_key_the_destination_does_not_set_writes_nothing() {
            let mut fx = Fixture::new();
            select(&mut fx.app, "tldr_model");
            handle_config_key(&mut fx.app, KeyCode::Char('d'));
            assert!(fx.app.config_confirm.is_none());
            assert_eq!(
                fx.app.status_message,
                "Nothing to reset: tldr_model is not set in ws"
            );

            handle_config_key(&mut fx.app, KeyCode::Char('a'));
            handle_config_key(&mut fx.app, KeyCode::Char('d'));
            assert!(fx.app.config_confirm.is_none());
            assert_eq!(
                fx.app.status_message,
                "Nothing to reset: tldr_model is not set in GLOBAL"
            );
            assert_eq!(fs::read_to_string(&fx.overlay).unwrap(), OVERLAY);
            assert_eq!(fs::read_to_string(&fx.global).unwrap(), GLOBAL);
        }

        /// Deleting a mode a workspace only inherits copies the rest of the
        /// list into its file: that waits for `y` and says so.
        #[test]
        fn deleting_an_inherited_mode_warns_and_waits_for_y() {
            let fx = Fixture::new();
            fs::write(
                &fx.global,
                format!(
                    "{GLOBAL}[[model_modes]]\nname = \"second\"\nprimary = \"global:second@low\"\n"
                ),
            )
            .unwrap();
            fs::write(&fx.overlay, "model = \"ws:model@high\"\n").unwrap();
            let mut app = app_at(fx.overlay.parent().unwrap().parent().unwrap());

            select(&mut app, "model_modes.0");
            handle_config_key(&mut app, KeyCode::Char('d'));
            let pending = app.config_confirm.as_ref().expect("asks before copying");
            assert!(!pending.global_file);
            assert!(
                pending
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("ws has no own mode list")),
                "{:?}",
                pending.note
            );
            assert!(screen(&app).contains("later mode changes in [global] stop reaching it"));
            assert_eq!(
                fs::read_to_string(&fx.overlay).unwrap(),
                "model = \"ws:model@high\"\n"
            );

            handle_config_key(&mut app, KeyCode::Char('y'));
            let overlay = fs::read_to_string(&fx.overlay).unwrap();
            assert!(
                overlay.contains("second") && !overlay.contains("base"),
                "{overlay}"
            );
            assert_eq!(fx.global_config().model_modes.len(), 2);
        }

        /// `y` writes the file captured when the write was requested, even
        /// if the view changed meanwhile.
        #[test]
        fn pending_write_keeps_its_destination() {
            let mut fx = Fixture::new();
            handle_config_key(&mut fx.app, KeyCode::Char('a'));
            pick(&mut fx.app, "model", "global:new");
            fx.app.config_global_view = false;
            handle_config_key(&mut fx.app, KeyCode::Char('y'));
            assert_eq!(fx.global_config().model, "global:new@low");
            assert_eq!(fs::read_to_string(&fx.overlay).unwrap(), OVERLAY);
        }

        /// While a write waits for `y`, keys and the mouse cannot switch tabs,
        /// views, rows, or quit.
        #[test]
        fn confirmation_blocks_navigation_and_mouse() {
            use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

            let mut fx = Fixture::new();
            key(&mut fx.app, KeyCode::Char('a'));
            select(&mut fx.app, "model");
            open_model_picker(&mut fx.app);
            let picker = fx.app.model_picker.as_mut().unwrap();
            picker.chosen_model = "global:new".into();
            picker.phase = PickerPhase::Thinking;
            key(&mut fx.app, KeyCode::Enter);
            assert!(fx.app.config_confirm.is_some());

            let selected = fx.app.config_selected;
            for code in [
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Char('q'),
                KeyCode::Down,
                KeyCode::Char('j'),
                KeyCode::Char('a'),
                KeyCode::Char('p'),
                KeyCode::Char('d'),
                KeyCode::Enter,
            ] {
                key(&mut fx.app, code);
            }
            for kind in [
                MouseEventKind::ScrollDown,
                MouseEventKind::Down(MouseButton::Left),
            ] {
                crate::app::handle_mouse_event(
                    &mut fx.app,
                    MouseEvent {
                        kind,
                        column: 2,
                        row: 1,
                        modifiers: KeyModifiers::NONE,
                    },
                );
            }
            assert!(fx.app.active_section == Section::Config);
            assert!(!fx.app.should_quit);
            assert!(fx.app.config_global_view);
            assert_eq!(fx.app.config_selected, selected);
            assert_eq!(fx.app.config_scroll, 0);
            assert!(fx.app.config_workspace_picker.is_none());
            assert!(fx.app.config_confirm.is_some());
            assert_eq!(fs::read_to_string(&fx.global).unwrap(), GLOBAL);

            key(&mut fx.app, KeyCode::Char('y'));
            assert_eq!(fx.global_config().model, "global:new@low");
        }

        /// The picker lists global first, then workspaces; a filtered pick
        /// opens the entry that was chosen.
        #[test]
        fn picker_maps_global_first_and_filtered_workspaces() {
            let fx = Fixture::new();
            let other = fx.home.path().join("zz-other");
            fs::create_dir_all(other.join(".zdx")).unwrap();
            fs::write(other.join(".zdx/config.toml"), "model = \"other:m@low\"\n").unwrap();
            add_profile(&fx.global, "zz-other", &other);
            let mut app = app_at(fx.overlay.parent().unwrap().parent().unwrap());

            key(&mut app, KeyCode::Char('p'));
            let picker = app.config_workspace_picker.as_ref().unwrap();
            assert!(picker.items[0].0.starts_with("global · all workspaces"));
            assert!(picker.items[1].0.starts_with("ws"));
            assert!(picker.items[2].0.starts_with("zz-other"));
            assert_eq!(picker.selected, 1, "the viewed workspace is preselected");

            type_text(&mut app, "zz-oth");
            key(&mut app, KeyCode::Enter);
            assert!(matches!(app.config_scope(), Scope::Workspace(w) if w.name == "zz-other"));
            let model = app
                .config_settings
                .iter()
                .find(|s| s.key == "model")
                .unwrap();
            assert_eq!(model.value.as_deref(), Some("other:m@low"));

            key(&mut app, KeyCode::Char('p'));
            assert_eq!(app.config_workspace_picker.as_ref().unwrap().selected, 2);
            type_text(&mut app, "all work");
            key(&mut app, KeyCode::Enter);
            assert_eq!(app.config_scope(), Scope::Global);
            assert!(config_footer_hint(&app).contains("a back to zz-other"));

            key(&mut app, KeyCode::Char('p'));
            assert_eq!(app.config_workspace_picker.as_ref().unwrap().selected, 0);
            type_text(&mut app, "no-such-entry");
            key(&mut app, KeyCode::Enter);
            assert_eq!(app.config_scope(), Scope::Global, "no match keeps the view");
        }

        /// If the viewed workspace is gone when the picker re-discovers,
        /// rows, header, and destination switch together, even on Esc.
        #[test]
        fn rediscovery_without_the_viewed_workspace_reloads_the_view() {
            let fx = Fixture::new();
            let other = fx.home.path().join("zz-other");
            fs::create_dir_all(other.join(".zdx")).unwrap();
            fs::write(other.join(".zdx/config.toml"), "model = \"other:m@low\"\n").unwrap();
            add_profile(&fx.global, "zz-other", &other);
            let mut app = app_at(fx.overlay.parent().unwrap().parent().unwrap());
            key(&mut app, KeyCode::Char('p'));
            type_text(&mut app, "zz-oth");
            key(&mut app, KeyCode::Enter);
            assert!(matches!(app.config_scope(), Scope::Workspace(w) if w.name == "zz-other"));

            fs::write(&fx.global, GLOBAL).unwrap();
            key(&mut app, KeyCode::Char('p'));
            key(&mut app, KeyCode::Esc);

            assert!(matches!(app.config_scope(), Scope::Workspace(w) if w.name == "ws"));
            let model = app
                .config_settings
                .iter()
                .find(|s| s.key == "model")
                .unwrap();
            assert_eq!(model.value.as_deref(), Some("ws:model@high"));
            assert!(app.status_message.contains("zz-other is no longer listed"));
            select(&mut app, "tldr_model");
            pick(&mut app, "tldr_model", "ws:tldr");
            assert!(
                fs::read_to_string(&fx.overlay)
                    .unwrap()
                    .contains("ws:tldr@low")
            );
            assert_eq!(
                fs::read_to_string(other.join(".zdx/config.toml")).unwrap(),
                "model = \"other:m@low\"\n"
            );
        }

        /// The footer names what `a` does from the current view.
        #[test]
        fn footer_names_the_way_back_from_global() {
            let mut fx = Fixture::new();
            assert!(config_footer_hint(&fx.app).contains("a global view"));
            handle_config_key(&mut fx.app, KeyCode::Char('a'));
            let hint = config_footer_hint(&fx.app);
            assert!(
                hint.contains("a back to ws") && !hint.contains("a global view"),
                "{hint}"
            );
            pick(&mut fx.app, "model", "global:new");
            assert!(config_footer_hint(&fx.app).starts_with("y write global • Esc cancel"));
        }

        /// A narrow terminal wraps the origin and reach lines instead of
        /// cutting them off.
        #[test]
        fn narrow_header_wraps_reach_instead_of_cutting_it() {
            let mut fx = Fixture::new();
            handle_config_key(&mut fx.app, KeyCode::Char('a'));
            select(&mut fx.app, "model");
            let header: Vec<String> = header_lines(&fx.app, 40)
                .iter()
                .map(ToString::to_string)
                .collect();
            assert!(header.len() > 3, "{header:?}");
            assert!(
                header.iter().skip(1).all(|row| row.chars().count() <= 40),
                "{header:?}"
            );
            let joined = header[1..].join(" ");
            let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                words(&joined).ends_with("edit reaches no listed workspace · skips ws (own value)"),
                "{joined}"
            );
            fx.app.terminal_width = 42;
            let mut terminal = Terminal::new(TestBackend::new(42, 30)).unwrap();
            terminal.draw(|f| crate::ui::render(f, &fx.app)).unwrap();
            let buffer = terminal.backend().buffer();
            let text: String = (0..buffer.area.height)
                .map(|y| {
                    (0..buffer.area.width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(text.contains("(own value)"), "{text}");
        }

        #[test]
        fn wrap_words_breaks_at_spaces_and_splits_long_words() {
            assert_eq!(wrap_words("ab cd ef", 5), vec!["ab cd", " ef"]);
            assert_eq!(wrap_words("abcdefgh", 3), vec!["abc", " de", " fg", " h"]);
            assert_eq!(wrap_words("", 3), vec![""]);
        }
    }
}
