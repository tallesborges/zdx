//! Workspace model settings: one view over every workspace's effective model
//! configuration, with the layer that supplied each value.
//!
//! A workspace is a directory whose `.zdx/config.toml` overlay layers over the
//! global config (see [`crate::config::paths::config_layer_paths_for`]). The
//! list comes from Telegram profile bindings (each bound `cwd` is its own
//! workspace; several profiles may share one) plus the caller's current root.
//!
//! Writes go to one named file, the [`Scope`]'s own: the global config, or a
//! workspace's own overlay. A workspace whose overlay is the global config
//! file (the home directory, when `$ZDX_HOME` is `~/.zdx`) writes the global
//! file. Resetting removes a key from that file, so the value falls back to
//! what the layers below supply. For a workspace that can be an ancestor
//! overlay rather than the global config, which is why every setting carries
//! its inherited value and origin next to the effective one. A write also
//! reaches every other workspace that layers the same file and does not set
//! the key above it ([`reach`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{self, Config, ConfigSources, ModelMode, TelegramProfileConfig, paths};

/// Helper-model keys, in display order.
pub const HELPER_MODEL_KEYS: [&str; 5] = [
    "title_model",
    "tldr_model",
    "handoff_model",
    "prompt_builder_model",
    "read_thread_model",
];

/// Setting key for the subagents on/off flag.
pub const SUBAGENTS_ENABLED_KEY: &str = "subagents.enabled";
/// Setting key for the whole `[[model_modes]]` list (arrays replace wholesale).
pub const MODEL_MODES_KEY: &str = "model_modes";

/// A directory whose config layers can be viewed and edited as a unit.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Workspace {
    /// Canonical workspace directory.
    pub root: PathBuf,
    /// Display name (directory basename).
    pub name: String,
    /// Telegram profile names bound to this directory.
    pub profiles: Vec<String>,
    /// Whether this is the caller's current root (the Monitor or bot root).
    pub current: bool,
    /// This workspace's own overlay, `<root>/.zdx/config.toml`.
    pub overlay_path: PathBuf,
    /// Whether the overlay file exists.
    pub has_overlay: bool,
    /// Whether the overlay is the global config file itself, so a write
    /// "to this workspace" changes every workspace.
    pub overlay_is_global: bool,
}

impl Workspace {
    fn new(root: PathBuf, current: bool) -> Self {
        let name = root.file_name().map_or_else(
            || root.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let overlay_path = root.join(".zdx").join("config.toml");
        Self {
            has_overlay: overlay_path.is_file(),
            overlay_is_global: same_file(&overlay_path, &paths::config_path()),
            root,
            name,
            profiles: Vec::new(),
            current,
            overlay_path,
        }
    }
}

/// Whether two config paths name the same file, also through symlinks and
/// when the file does not exist yet.
fn same_file(a: &Path, b: &Path) -> bool {
    a == b || canonical_file(a) == canonical_file(b)
}

/// Canonical form of a file path. A missing file resolves through its parent
/// directory, so `<dir>/.zdx/config.toml` with `.zdx` linked to `$ZDX_HOME`
/// matches the global config before either file exists.
fn canonical_file(path: &Path) -> PathBuf {
    if let Ok(canonical) = path.canonicalize() {
        return canonical;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => config::canonical_or_self(parent).join(name),
        _ => path.to_path_buf(),
    }
}

/// Builds the workspace list from Telegram profile bindings plus
/// `current_root`. The current root resolves to its nearest project root
/// (the directory holding `.zdx`), falling back to itself. Bound `cwd`s are
/// used as-is: a binding is an explicit choice of directory.
///
/// The current workspace comes first, the rest are ordered by path.
#[must_use]
pub fn workspaces_from(
    profiles: &BTreeMap<String, TelegramProfileConfig>,
    current_root: Option<&Path>,
) -> Vec<Workspace> {
    let current = current_root.map(|root| {
        let root = config::canonical_or_self(root);
        paths::project_root_for(&root, paths::home_dir().as_deref()).unwrap_or(root)
    });

    let mut by_root: BTreeMap<PathBuf, Workspace> = BTreeMap::new();
    if let Some(root) = &current {
        by_root.insert(root.clone(), Workspace::new(root.clone(), true));
    }
    for (name, profile) in profiles {
        let root = config::canonical_or_self(&profile.cwd_path());
        by_root
            .entry(root.clone())
            .or_insert_with(|| Workspace::new(root, false))
            .profiles
            .push(name.clone());
    }

    let mut list: Vec<Workspace> = by_root.into_values().collect();
    list.sort_by_key(|workspace| !workspace.current);
    list
}

/// Loads the Telegram profiles visible from `current_root` (or from the
/// global config alone) and builds the workspace list.
///
/// # Errors
/// Returns an error if the config layers cannot be read or parsed.
pub fn discover(current_root: Option<&Path>) -> Result<Vec<Workspace>> {
    let layers = match current_root {
        Some(root) => paths::config_layer_paths_for(root),
        None => vec![paths::config_path()],
    };
    let config = Config::load_layered(&layers)?;
    Ok(workspaces_from(&config.telegram.profiles, current_root))
}

/// Which layer supplied a value.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// No layer sets it; the built-in default applies.
    Default,
    /// `$ZDX_HOME/config.toml`.
    Global,
    /// The workspace's own overlay: an override set here.
    Workspace,
    /// An ancestor directory's overlay, inherited by this workspace.
    Ancestor,
}

/// Origin of a value: its layer, the file, and a short display label
/// (`default`, `global`, or the overlay directory's name).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Source {
    pub layer: Layer,
    pub path: Option<PathBuf>,
    pub label: String,
}

impl Source {
    /// Classifies the layer file that supplied a value.
    #[must_use]
    pub fn classify(path: Option<&Path>, global: &Path, overlay: &Path) -> Self {
        let (layer, label) = match path {
            None => (Layer::Default, "default".to_string()),
            Some(path) if path == global => (Layer::Global, "global".to_string()),
            Some(path) => (
                if path == overlay {
                    Layer::Workspace
                } else {
                    Layer::Ancestor
                },
                overlay_label(path),
            ),
        };
        Self {
            layer,
            path: path.map(Path::to_path_buf),
            label,
        }
    }
}

/// Display name of an overlay layer: the directory that holds its `.zdx`.
#[must_use]
pub fn overlay_label(path: &Path) -> String {
    path.parent()
        .and_then(Path::parent)
        .and_then(Path::file_name)
        .map_or_else(
            || "workspace".to_string(),
            |name| name.to_string_lossy().into_owned(),
        )
}

/// How a setting is edited.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SettingKind {
    /// Chat model spec (`provider:model@thinking`) or a `mode:<name>` ref.
    Chat,
    /// Speech-to-text model.
    Transcription,
    /// Text-to-speech model.
    Speech,
    /// On/off flag.
    Toggle,
    /// The `[[model_modes]]` list.
    Modes,
}

/// One model setting of a workspace.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Setting {
    /// Dotted config key (`model`, `subagents.overrides.orchestrator.model`, …).
    pub key: String,
    pub group: &'static str,
    pub label: String,
    pub kind: SettingKind,
    /// Effective value; `None` when unset (the runtime default applies).
    pub value: Option<String>,
    pub source: Source,
    /// Value without this workspace's own overlay: what resetting the
    /// workspace layer resolves to.
    pub inherited_value: Option<String>,
    pub inherited_source: Source,
}

impl Setting {
    /// Whether this workspace's own overlay sets the value.
    #[must_use]
    pub fn overridden_here(&self) -> bool {
        self.source.layer == Layer::Workspace
    }

    /// Whether `scope`'s own file sets the value: what a reset there removes.
    #[must_use]
    pub fn set_in_own_file(&self, scope: Scope<'_>) -> bool {
        if scope.writes_global_file() {
            self.source.layer == Layer::Global
        } else {
            self.overridden_here()
        }
    }
}

/// A workspace with its settings, or the error that prevented loading them.
#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceReport {
    #[serde(flatten)]
    pub workspace: Workspace,
    pub settings: Vec<Setting>,
    pub error: Option<String>,
}

/// Discovers the workspaces and loads each one's settings. A workspace whose
/// layers fail to load is reported with its error instead of failing all.
///
/// # Errors
/// Returns an error if the workspace list itself cannot be built.
pub fn report(current_root: Option<&Path>) -> Result<Vec<WorkspaceReport>> {
    Ok(discover(current_root)?
        .into_iter()
        .map(
            |workspace| match load_settings(Scope::Workspace(&workspace)) {
                Ok(settings) => WorkspaceReport {
                    workspace,
                    settings,
                    error: None,
                },
                Err(error) => WorkspaceReport {
                    workspace,
                    settings: Vec::new(),
                    error: Some(format!("{error:#}")),
                },
            },
        )
        .collect())
}

/// Loads the model settings of `scope` from its real config layers.
///
/// # Errors
/// Returns an error if a layer cannot be read or parsed.
pub fn load_settings(scope: Scope<'_>) -> Result<Vec<Setting>> {
    settings_from_layers(scope, &scope.layers(), &paths::config_path())
}

/// Number of model settings that `scope`'s own file sets. Values it only
/// inherits from other layers are not counted.
///
/// # Errors
/// Returns an error if a layer cannot be read or parsed.
pub fn own_override_count(scope: Scope<'_>) -> Result<usize> {
    if let Scope::Workspace(workspace) = scope
        && !workspace.has_overlay
    {
        return Ok(0);
    }
    Ok(load_settings(scope)?
        .iter()
        .filter(|setting| setting.set_in_own_file(scope))
        .count())
}

/// Resolves every model setting of `scope` from an explicit layer list
/// (lowest precedence first). The inherited view is the same list without
/// the scope's own file.
///
/// # Errors
/// Returns an error if a layer cannot be read or parsed.
pub fn settings_from_layers(
    scope: Scope<'_>,
    layers: &[PathBuf],
    global: &Path,
) -> Result<Vec<Setting>> {
    let own = match scope {
        Scope::Workspace(workspace) if !workspace.overlay_is_global => {
            workspace.overlay_path.as_path()
        }
        _ => global,
    };
    let (config, sources) = Config::load_layered_with_sources(layers)?;
    let inherited_layers: Vec<PathBuf> = layers
        .iter()
        .filter(|path| path.as_path() != own)
        .cloned()
        .collect();
    let (inherited, inherited_sources) = Config::load_layered_with_sources(&inherited_layers)?;

    let effective = Resolved::new(&config, &sources)?;
    let base = Resolved::new(&inherited, &inherited_sources)?;

    let mut settings = Vec::new();
    for (key, group, label, kind) in setting_defs(scope, &config, &inherited) {
        settings.push(Setting {
            value: effective.value(&key),
            source: Source::classify(effective.sources.source(&key), global, own),
            inherited_value: base.value(&key),
            inherited_source: Source::classify(base.sources.source(&key), global, own),
            key,
            group,
            label,
            kind,
        });
    }
    Ok(settings)
}

struct Resolved<'a> {
    config: &'a Config,
    json: serde_json::Value,
    sources: &'a ConfigSources,
}

impl<'a> Resolved<'a> {
    fn new(config: &'a Config, sources: &'a ConfigSources) -> Result<Self> {
        Ok(Self {
            config,
            json: serde_json::to_value(config).context("serialize config")?,
            sources,
        })
    }

    fn value(&self, key: &str) -> Option<String> {
        if key == MODEL_MODES_KEY {
            return (!self.config.model_modes.is_empty()).then(|| {
                self.config
                    .model_modes
                    .iter()
                    .map(|mode| {
                        format!(
                            "{} → {}",
                            mode.name,
                            crate::models::format_model_thinking(&mode.primary, mode.thinking)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            });
        }
        let pointer = format!("/{}", key.replace('.', "/"));
        match self.json.pointer(&pointer)? {
            serde_json::Value::String(value) if !value.is_empty() => Some(value.clone()),
            serde_json::Value::Bool(value) => Some(if *value { "on" } else { "off" }.to_string()),
            _ => None,
        }
    }
}

fn setting_defs(
    scope: Scope<'_>,
    config: &Config,
    inherited: &Config,
) -> Vec<(String, &'static str, String, SettingKind)> {
    let mut defs = vec![(
        "model".to_string(),
        "core",
        "model".to_string(),
        SettingKind::Chat,
    )];
    for key in HELPER_MODEL_KEYS {
        defs.push((
            key.to_string(),
            "helper models",
            key.to_string(),
            SettingKind::Chat,
        ));
    }
    defs.push((
        "transcription.model".to_string(),
        "audio",
        "transcription".to_string(),
        SettingKind::Transcription,
    ));
    defs.push((
        "speech.model".to_string(),
        "audio",
        "speech".to_string(),
        SettingKind::Speech,
    ));
    defs.push((
        MODEL_MODES_KEY.to_string(),
        "model modes",
        "modes".to_string(),
        SettingKind::Modes,
    ));
    defs.push((
        SUBAGENTS_ENABLED_KEY.to_string(),
        "subagents",
        "enabled".to_string(),
        SettingKind::Toggle,
    ));

    // Discovered subagents plus any override that names one no longer
    // discovered, so a stale override stays visible where it can be reset.
    let mut names: Vec<String> = scope
        .subagents()
        .map(|defs| defs.into_iter().map(|def| def.name).collect())
        .unwrap_or_default();
    for name in config
        .subagents
        .overrides
        .keys()
        .chain(inherited.subagents.overrides.keys())
    {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names.sort_by_key(|name| {
        (
            name != crate::subagents::ORCHESTRATOR_SUBAGENT_NAME,
            name.clone(),
        )
    });
    for name in names {
        defs.push((
            format!("subagents.overrides.{name}.model"),
            "subagents",
            name,
            SettingKind::Chat,
        ));
    }
    defs
}

/// What a settings view shows and edits: the global config on its own, or
/// one workspace through all its layers. Writes and resets go to the scope's
/// own file ([`Scope::file`]), whose chain is also the one shown, so an edit
/// never starts from values another chain supplied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope<'a> {
    /// `$ZDX_HOME/config.toml` alone, as every workspace inherits it.
    Global,
    /// A workspace's merged layers; edits go to its own overlay.
    Workspace(&'a Workspace),
}

impl<'a> Scope<'a> {
    /// Display name: `global` or the workspace name.
    #[must_use]
    pub fn name(self) -> &'a str {
        match self {
            Self::Global => "global",
            Self::Workspace(workspace) => &workspace.name,
        }
    }

    /// Whether writes land in the global config file: the global scope, or
    /// a workspace whose overlay is that file.
    #[must_use]
    pub fn writes_global_file(self) -> bool {
        match self {
            Self::Global => true,
            Self::Workspace(workspace) => workspace.overlay_is_global,
        }
    }

    /// The file this scope writes and resets.
    #[must_use]
    pub fn file(self) -> PathBuf {
        match self {
            Self::Workspace(workspace) if !workspace.overlay_is_global => {
                workspace.overlay_path.clone()
            }
            _ => paths::config_path(),
        }
    }

    /// Config layers the scope reads, lowest precedence first.
    #[must_use]
    pub fn layers(self) -> Vec<PathBuf> {
        match self {
            Self::Global => vec![paths::config_path()],
            Self::Workspace(workspace) => layer_chain(&workspace.root),
        }
    }

    /// Subagents visible in the scope: built-in and global ones, plus the
    /// workspace's project subagents.
    ///
    /// # Errors
    /// Returns an error if a definition cannot be read or parsed.
    pub fn subagents(self) -> Result<Vec<crate::subagents::SubagentDefinition>> {
        match self {
            Self::Global => crate::subagents::discover_global(),
            Self::Workspace(workspace) => crate::subagents::discover(&workspace.root),
        }
    }
}

/// Which workspaces read one setting from a config file.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Reach {
    /// Workspaces whose effective value comes from the file once it sets
    /// the key: no layer above the file sets it.
    pub reads: Vec<String>,
    /// Workspaces where a layer above the file sets the key, so the file's
    /// value does not apply there.
    pub skips: Vec<Skip>,
    /// Workspaces that layer the file but whose layers failed to load.
    pub unknown: Vec<String>,
}

/// A workspace that does not read a file's value for a key.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Skip {
    pub workspace: String,
    /// Label of the layer that sets the key above the file.
    pub by: String,
}

/// For each of `keys`, which of `workspaces` layer `file` and would read its
/// value. Only listed workspaces are checked: other directories that layer
/// the file are not discovered.
#[must_use]
pub fn reach(file: &Path, keys: &[String], workspaces: &[Workspace]) -> BTreeMap<String, Reach> {
    let chains: Vec<(&str, Vec<PathBuf>)> = workspaces
        .iter()
        .map(|workspace| (workspace.name.as_str(), layer_chain(&workspace.root)))
        .collect();
    reach_in_chains(file, keys, &chains, &paths::config_path())
}

/// Layers of the workspace at `root`, keeping one entry per file: an
/// overlay reached through a symlink to `$ZDX_HOME` is the global file.
fn layer_chain(root: &Path) -> Vec<PathBuf> {
    let mut chain: Vec<PathBuf> = Vec::new();
    for layer in paths::config_layer_paths_for(root) {
        if !chain.iter().any(|seen| same_file(seen, &layer)) {
            chain.push(layer);
        }
    }
    chain
}

/// [`reach`] over explicit layer chains (lowest precedence first).
#[must_use]
pub fn reach_in_chains(
    file: &Path,
    keys: &[String],
    chains: &[(&str, Vec<PathBuf>)],
    global: &Path,
) -> BTreeMap<String, Reach> {
    let mut out: BTreeMap<String, Reach> = keys
        .iter()
        .map(|key| (key.clone(), Reach::default()))
        .collect();
    for (name, layers) in chains {
        let Some(at) = layers.iter().position(|layer| same_file(layer, file)) else {
            continue;
        };
        let Ok((_, sources)) = Config::load_layered_with_sources(layers) else {
            for reach in out.values_mut() {
                reach.unknown.push((*name).to_string());
            }
            continue;
        };
        for (key, reach) in &mut out {
            let above = sources.source(key).and_then(|source| {
                layers
                    .iter()
                    .position(|layer| layer.as_path() == source)
                    .filter(|index| *index > at)
                    .map(|_| source)
            });
            match above {
                Some(source) => reach.skips.push(Skip {
                    workspace: (*name).to_string(),
                    by: if source == global {
                        "global".to_string()
                    } else {
                        overlay_label(source)
                    },
                }),
                None => reach.reads.push((*name).to_string()),
            }
        }
    }
    out
}

/// A new value for a setting.
#[derive(Debug, Clone, Copy)]
pub enum SettingValue<'a> {
    /// Model spec, including `@thinking` and `mode:<name>` refs.
    Spec(&'a str),
    Enabled(bool),
    Modes(&'a [ModelMode]),
}

/// Subagent name of a `subagents.overrides.<name>.model` key.
fn subagent_override_name(key: &str) -> Option<&str> {
    let name = key
        .strip_prefix("subagents.overrides.")?
        .strip_suffix(".model")?;
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
    .then_some(name)
}

fn is_setting_key(key: &str) -> bool {
    Config::EDITABLE_MODEL_FIELDS.contains(&key)
        || key == SUBAGENTS_ENABLED_KEY
        || key == MODEL_MODES_KEY
        || subagent_override_name(key).is_some()
}

/// Writes one setting to the config layer at `path`, preserving the rest of
/// the file. Workspace overlays stay minimal. An empty mode list removes the
/// key, so the layer inherits the modes below it.
///
/// # Errors
/// Returns an error for an unknown key, a value of the wrong kind, or a
/// failed write.
pub fn set_setting(path: &Path, key: &str, value: SettingValue<'_>) -> Result<()> {
    match value {
        SettingValue::Spec(spec) if Config::EDITABLE_MODEL_FIELDS.contains(&key) => {
            Config::save_model_field_to(path, key, spec)
        }
        SettingValue::Spec(spec) => {
            let name = subagent_override_name(key)
                .with_context(|| format!("not a model setting: {key}"))?;
            Config::save_subagent_override_spec_to(path, name, spec)
        }
        SettingValue::Enabled(enabled) if key == SUBAGENTS_ENABLED_KEY => {
            Config::save_subagents_enabled_to(path, enabled)
        }
        SettingValue::Modes(modes) if key == MODEL_MODES_KEY => {
            Config::save_model_modes_to(path, modes)
        }
        _ => anyhow::bail!("value does not fit setting {key}"),
    }
}

/// Removes one setting from the config layer at `path`, so it falls back to
/// the layers below. Returns whether the layer set it.
///
/// # Errors
/// Returns an error for an unknown key or a failed read/write.
pub fn reset_setting(path: &Path, key: &str) -> Result<bool> {
    anyhow::ensure!(is_setting_key(key), "not a model setting: {key}");
    Config::unset_layer_key(path, key)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn setting<'a>(settings: &'a [Setting], key: &str) -> &'a Setting {
        settings.iter().find(|s| s.key == key).unwrap()
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("home/config.toml");
        let parent = dir.path().join("example/.zdx/config.toml");
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        fs::create_dir_all(parent.parent().unwrap()).unwrap();
        fs::create_dir_all(dir.path().join("example/app")).unwrap();
        fs::write(
            &global,
            "model = \"global:model@low\"\ntitle_model = \"global:title\"\n\
             [subagents.overrides.orchestrator]\nmodel = \"global:orch@high\"\n",
        )
        .unwrap();
        fs::write(&parent, "model = \"parent:model@high\"\n").unwrap();
        let workspace = Workspace::new(dir.path().join("example/app"), false);
        (dir, global, parent, workspace)
    }

    #[test]
    fn settings_report_source_and_inherited_value_per_layer() {
        let (_dir, global, parent, workspace) = fixture();
        fs::create_dir_all(workspace.overlay_path.parent().unwrap()).unwrap();
        fs::write(&workspace.overlay_path, "model = \"app:model@medium\"\n").unwrap();
        let layers = vec![
            global.clone(),
            parent.clone(),
            workspace.overlay_path.clone(),
        ];

        let settings =
            settings_from_layers(Scope::Workspace(&workspace), &layers, &global).unwrap();

        let model = setting(&settings, "model");
        assert_eq!(model.value.as_deref(), Some("app:model@medium"));
        assert!(model.overridden_here());
        assert_eq!(model.source.label, "app");
        assert_eq!(model.inherited_value.as_deref(), Some("parent:model@high"));
        assert_eq!(model.inherited_source.layer, Layer::Ancestor);
        assert_eq!(model.inherited_source.label, "example");

        let orchestrator = setting(&settings, "subagents.overrides.orchestrator.model");
        assert_eq!(orchestrator.value.as_deref(), Some("global:orch@high"));
        assert_eq!(orchestrator.source.layer, Layer::Global);
        assert_eq!(orchestrator.label, "orchestrator");

        let tldr = setting(&settings, "tldr_model");
        assert_eq!(tldr.source.layer, Layer::Default);
    }

    #[test]
    fn override_count_ignores_inherited_values() {
        let _home = crate::test_support::temp_zdx_home();
        let (_dir, _global, _parent, workspace) = fixture();
        let workspace = Workspace::new(config::canonical_or_self(&workspace.root), false);
        let global = paths::config_path();
        fs::create_dir_all(global.parent().unwrap()).unwrap();
        fs::write(&global, "title_model = \"global:title\"\n").unwrap();
        assert_eq!(own_override_count(Scope::Workspace(&workspace)).unwrap(), 0);

        fs::create_dir_all(workspace.overlay_path.parent().unwrap()).unwrap();
        fs::write(
            &workspace.overlay_path,
            "model = \"app:model@medium\"\ntldr_model = \"app:tldr\"\n",
        )
        .unwrap();
        let workspace = Workspace::new(workspace.root.clone(), false);
        assert_eq!(own_override_count(Scope::Workspace(&workspace)).unwrap(), 2);

        fs::write(&workspace.overlay_path, "model = [").unwrap();
        assert!(own_override_count(Scope::Workspace(&workspace)).is_err());
    }

    #[test]
    fn workspace_writes_stay_minimal_and_reset_falls_back_to_ancestor() {
        let _home = crate::test_support::temp_zdx_home();
        let (_dir, global, parent, workspace) = fixture();
        let layers = vec![global.clone(), parent, workspace.overlay_path.clone()];
        let target = Scope::Workspace(&workspace).file();

        set_setting(&target, "model", SettingValue::Spec("app:model@high")).unwrap();
        set_setting(
            &target,
            "subagents.overrides.orchestrator.model",
            SettingValue::Spec("mode:smart"),
        )
        .unwrap();
        let written = fs::read_to_string(&target).unwrap();
        assert!(!written.contains("# ZDX Configuration"), "{written}");
        assert!(!written.contains("title_model"), "{written}");

        let settings =
            settings_from_layers(Scope::Workspace(&workspace), &layers, &global).unwrap();
        assert!(setting(&settings, "model").overridden_here());
        assert!(setting(&settings, "subagents.overrides.orchestrator.model").overridden_here());

        assert!(reset_setting(&target, "model").unwrap());
        assert!(reset_setting(&target, "subagents.overrides.orchestrator.model").unwrap());
        assert!(!reset_setting(&target, "model").unwrap());
        assert_eq!(fs::read_to_string(&target).unwrap().trim(), "");

        let settings =
            settings_from_layers(Scope::Workspace(&workspace), &layers, &global).unwrap();
        let model = setting(&settings, "model");
        assert_eq!(model.value.as_deref(), Some("parent:model@high"));
        assert_eq!(model.source.layer, Layer::Ancestor);
        let orchestrator = setting(&settings, "subagents.overrides.orchestrator.model");
        assert_eq!(orchestrator.value.as_deref(), Some("global:orch@high"));
        assert_eq!(fs::read_to_string(&global).unwrap().lines().count(), 4);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".zdx/config.toml");
        assert!(set_setting(&path, "max_tokens", SettingValue::Spec("1")).is_err());
        assert!(reset_setting(&path, "telegram.bot_token").is_err());
        assert!(
            set_setting(
                &path,
                "subagents.overrides.a.b.model",
                SettingValue::Spec("x")
            )
            .is_err()
        );
        assert!(!path.exists());
    }

    #[test]
    fn profiles_sharing_a_root_become_one_workspace_after_the_current_root() {
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        let current = dir.path().join("current");
        fs::create_dir_all(&shared).unwrap();
        fs::create_dir_all(&current).unwrap();
        let profile = |cwd: &Path| TelegramProfileConfig {
            chat_id: -1,
            cwd: cwd.display().to_string(),
            orchestrator: false,
            worker_root: None,
        };
        let profiles = BTreeMap::from([
            ("alpha".to_string(), profile(&shared)),
            ("beta".to_string(), profile(&shared)),
        ]);

        let list = workspaces_from(&profiles, Some(&current));

        assert_eq!(list.len(), 2);
        assert!(list[0].current);
        assert_eq!(list[0].name, "current");
        assert_eq!(list[1].profiles, vec!["alpha", "beta"]);
        assert!(!list[1].has_overlay);
    }

    /// A parent overlay reaches nested workspaces unless a layer above it
    /// sets the key; the global file reaches every workspace the same way.
    #[test]
    fn reach_lists_nested_readers_and_the_layer_that_skips_them() {
        let (_dir, global, parent, workspace) = fixture();
        let nested_overlay = workspace.overlay_path.clone();
        fs::create_dir_all(nested_overlay.parent().unwrap()).unwrap();
        fs::write(&nested_overlay, "title_model = \"app:title\"\n").unwrap();
        let chains = vec![
            ("example", vec![global.clone(), parent.clone()]),
            ("app", vec![global.clone(), parent.clone(), nested_overlay]),
            ("other", vec![global.clone()]),
        ];
        let keys = vec!["model".to_string(), "title_model".to_string()];

        let from_parent = reach_in_chains(&parent, &keys, &chains, &global);
        assert_eq!(from_parent["model"].reads, vec!["example", "app"]);
        assert!(from_parent["model"].skips.is_empty());
        assert_eq!(from_parent["title_model"].reads, vec!["example"]);
        assert_eq!(
            from_parent["title_model"].skips,
            vec![Skip {
                workspace: "app".into(),
                by: "app".into()
            }]
        );

        let from_global = reach_in_chains(&global, &keys, &chains, &global);
        assert_eq!(from_global["model"].reads, vec!["other"]);
        let skipped: Vec<(&str, &str)> = from_global["model"]
            .skips
            .iter()
            .map(|skip| (skip.workspace.as_str(), skip.by.as_str()))
            .collect();
        assert_eq!(skipped, vec![("example", "example"), ("app", "example")]);

        fs::write(&parent, "model = [").unwrap();
        let broken = reach_in_chains(&global, &keys, &chains, &global);
        assert_eq!(broken["model"].unknown, vec!["example", "app"]);
    }

    /// The global scope reads only the global file, and a reset there falls
    /// back to the built-in default, never to a workspace value.
    #[test]
    fn global_scope_reads_only_the_global_file() {
        let _home = crate::test_support::temp_zdx_home();
        let (_dir, global, _parent, _workspace) = fixture();
        let settings =
            settings_from_layers(Scope::Global, std::slice::from_ref(&global), &global).unwrap();
        let model = setting(&settings, "model");
        assert_eq!(model.value.as_deref(), Some("global:model@low"));
        assert!(model.set_in_own_file(Scope::Global));
        assert_eq!(model.inherited_value, Some(Config::default().model));
        assert_eq!(model.inherited_source.layer, Layer::Default);
        assert!(!setting(&settings, "tldr_model").set_in_own_file(Scope::Global));
    }

    /// A workspace whose `.zdx` is `$ZDX_HOME` (the home directory in a
    /// default install) has the global config as its overlay: its scope
    /// writes the global file and treats global values as its own.
    #[test]
    fn workspace_whose_overlay_is_the_global_file_writes_global() {
        use std::os::unix::fs::symlink;

        let home = crate::test_support::temp_zdx_home();
        let global = paths::config_path();
        fs::write(&global, "model = \"global:model@low\"\n").unwrap();
        let root = tempfile::tempdir().unwrap();
        symlink(home.path(), root.path().join(".zdx")).unwrap();

        let workspace = workspaces_from(&BTreeMap::new(), Some(root.path())).remove(0);
        assert!(workspace.overlay_is_global);
        let scope = Scope::Workspace(&workspace);
        assert!(scope.writes_global_file());
        assert_eq!(scope.file(), global);

        let settings = load_settings(scope).unwrap();
        let model = setting(&settings, "model");
        assert!(model.set_in_own_file(scope));
        assert_eq!(model.inherited_source.layer, Layer::Default);
        assert_eq!(own_override_count(scope).unwrap(), 1);

        fs::remove_file(&global).unwrap();
        let before_first_write = workspaces_from(&BTreeMap::new(), Some(root.path())).remove(0);
        assert!(
            before_first_write.overlay_is_global,
            "a link to $ZDX_HOME must match before the global config exists"
        );

        let elsewhere = workspaces_from(&BTreeMap::new(), Some(home.path())).remove(0);
        assert!(!elsewhere.overlay_is_global);
        assert!(!Scope::Workspace(&elsewhere).writes_global_file());
    }
}
