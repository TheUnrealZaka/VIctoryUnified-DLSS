//! Where the user's files live.
//!
//! Resolution order for each value: environment variable, then the config
//! file (`VICTORY_CONFIG`, else `./victory.toml`), then the default.
//! See `victory.example.toml` at the repository root.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::policy::ensure_outside_repo;

/// Requested DLSS Frame Generation mode.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DlssFgMode {
    #[default]
    Off,
    On,
    Auto,
    Dynamic,
}

impl DlssFgMode {
    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "0" | "false" | "disabled" => Some(Self::Off),
            "on" | "1" | "true" | "enabled" => Some(Self::On),
            "auto" => Some(Self::Auto),
            "dynamic" => Some(Self::Dynamic),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::On => "on",
            Self::Auto => "auto",
            Self::Dynamic => "dynamic",
        }
    }
}

/// Streamline runtime DLL variant.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlRuntimeMode {
    #[default]
    Production,
    Development,
}

impl SlRuntimeMode {
    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "prod" | "production" | "release" => Some(Self::Production),
            "dev" | "development" | "debug" => Some(Self::Development),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Production => "production",
            Self::Development => "development",
        }
    }
}

/// Units stored inside the title's motion vector buffer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MvecUnits {
    /// Normalize by render resolution unless FidelityFX / explicit scale specifies otherwise.
    #[default]
    Auto,
    /// Motion vectors are in pixel space; scaled by `1.0 / width, 1.0 / height`.
    Pixels,
    /// Motion vectors are already normalized to `[-1, 1]` UV/NDC space (`mvecScale = [1, 1]`).
    Normalized,
}

impl MvecUnits {
    pub fn parse_str(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "pixels" | "pixel" | "px" => Some(Self::Pixels),
            "normalized" | "uv" | "ndc" => Some(Self::Normalized),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookRole {
    Depth,
    MotionVectors,
    Hudless,
    Ui,
    Camera,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookBinding {
    #[default]
    Any,
    DepthTarget,
    RenderTarget,
    Srv,
    Uav,
    Cbv,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookOccurrence {
    First,
    #[default]
    Last,
    #[serde(untagged)]
    Index(u32),
}

/// Explicit TOML rule for capturing Depth, Motion Vectors, HUDless, UI, or Camera CBVs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookRule {
    pub role: HookRole,
    #[serde(default)]
    pub shader_hash: Option<String>,
    #[serde(default)]
    pub stage: Option<String>,
    #[serde(default)]
    pub binding: HookBinding,
    #[serde(default)]
    pub slot: Option<u32>,
    /// Table dword offset (optional, for `Srv`/`Uav`/`Cbv` inside descriptor tables).
    #[serde(default)]
    pub table_dword: Option<u32>,
    /// DXGI format numeric code or name (e.g. `40`, `"D32_FLOAT"`, `"R16G16_FLOAT"`).
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub occurrence: HookOccurrence,
    /// Copy resource via GPU `CopyResource` at capture time so later passes cannot overwrite it.
    #[serde(default)]
    pub snapshot: Option<bool>,
    /// Optional CBV extraction parameters when `role = "camera"`.
    #[serde(default)]
    pub camera: Option<CameraCbvRule>,
}

/// Byte offsets inside a matched camera constant buffer.
#[derive(Debug, Default, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CameraCbvRule {
    pub view_proj_offset: Option<u32>,
    pub prev_view_proj_offset: Option<u32>,
    pub view_offset: Option<u32>,
    pub proj_offset: Option<u32>,
    pub jitter_offset: Option<u32>,
    pub camera_pos_offset: Option<u32>,
    #[serde(default = "default_true")]
    pub row_major: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DlssFgHooksConfig {
    #[serde(default = "default_true")]
    pub heuristic: bool,
    #[serde(default = "default_true")]
    pub depth_inverted: bool,
    #[serde(default = "default_true")]
    pub camera_motion_included: bool,
    #[serde(default)]
    pub mvec_jittered: bool,
    #[serde(default)]
    pub mvec_dilated: bool,
    #[serde(default)]
    pub mvec_units: MvecUnits,
    #[serde(default)]
    pub mvec_scale: Option<[f32; 2]>,
    #[serde(default)]
    pub render_width: Option<u32>,
    #[serde(default)]
    pub render_height: Option<u32>,
    #[serde(default)]
    pub probe: bool,
    #[serde(default)]
    pub rules: Vec<HookRule>,
}

impl Default for DlssFgHooksConfig {
    fn default() -> Self {
        Self {
            heuristic: true,
            depth_inverted: true,
            camera_motion_included: true,
            mvec_jittered: false,
            mvec_dilated: false,
            mvec_units: MvecUnits::Auto,
            mvec_scale: None,
            render_width: None,
            render_height: None,
            probe: false,
            rules: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DlssFgConfig {
    pub mode: DlssFgMode,
    pub frames: u32,
    pub dynamic_target_fps: f32,
    pub runtime_mode: SlRuntimeMode,
    pub runtime_dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
    pub project_id: String,
    pub application_id: u32,
    pub ui_recomposition: bool,
    pub retain_when_off: bool,
    pub fullscreen_menu_detection: bool,
    pub hooks: DlssFgHooksConfig,
}

impl Default for DlssFgConfig {
    fn default() -> Self {
        Self {
            mode: DlssFgMode::Off,
            frames: 1,
            dynamic_target_fps: 0.0,
            runtime_mode: SlRuntimeMode::Production,
            runtime_dir: None,
            log_dir: None,
            project_id: "a3f7e812-4c9b-4e6a-9d12-71b4e509a8c3".to_owned(),
            application_id: 0,
            ui_recomposition: true,
            retain_when_off: true,
            fullscreen_menu_detection: false,
            hooks: DlssFgHooksConfig::default(),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct Config {
    /// The user's dump of the title.
    pub game_dir: Option<PathBuf>,
    /// Root for all derived data. Always outside the repository.
    pub cache_dir: PathBuf,
    /// What titles write and must keep (persistent local storage). Never
    /// inside the cache, which may be deleted at any time.
    pub data_dir: PathBuf,
    /// Other titles used to exercise platform-level code.
    pub test_titles: Vec<PathBuf>,
    /// DLSS Frame Generation and hook configuration.
    pub dlss_fg: DlssFgConfig,
    /// The config file that was read, if any.
    pub source: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    paths: FilePaths,
    #[serde(default)]
    dlss_fg: FileDlssFg,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FilePaths {
    game_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    #[serde(default)]
    test_titles: Vec<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDlssFg {
    mode: Option<DlssFgMode>,
    frames: Option<u32>,
    dynamic_target_fps: Option<f32>,
    runtime_mode: Option<SlRuntimeMode>,
    runtime_dir: Option<PathBuf>,
    project_id: Option<String>,
    application_id: Option<u32>,
    ui_recomposition: Option<bool>,
    retain_when_off: Option<bool>,
    fullscreen_menu_detection: Option<bool>,
    #[serde(default)]
    hooks: Option<DlssFgHooksConfig>,
}

impl Config {
    /// Loads configuration from the environment and the config file.
    pub fn load() -> Result<Self> {
        let explicit = explicit_config_path(|k| std::env::var_os(k));
        let path = explicit
            .clone()
            .unwrap_or_else(|| PathBuf::from("victory.toml"));
        let file = if path.is_file() {
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            Some((path, text))
        } else if let Some(p) = explicit {
            anyhow::bail!(
                "VICTORY_CONFIG points at {}, which does not exist",
                p.display()
            );
        } else {
            None
        };
        let (source, parsed) = match file {
            Some((path, text)) => {
                let parsed: FileConfig =
                    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
                (Some(path), parsed)
            }
            None => (None, FileConfig::default()),
        };
        Self::resolve(parsed.paths, parsed.dlss_fg, source, |k| std::env::var_os(k))
    }

    fn resolve(
        file: FilePaths,
        file_fg: FileDlssFg,
        source: Option<PathBuf>,
        env: impl Fn(&str) -> Option<std::ffi::OsString>,
    ) -> Result<Self> {
        let env_path = |k: &str| env(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let env_str = |k: &str| {
            env(k)
                .filter(|v| !v.is_empty())
                .and_then(|v| v.into_string().ok())
        };
        let env_bool = |k: &str| {
            env_str(k).and_then(|v| match v.trim().to_ascii_lowercase().as_str() {
                "1" | "true" | "on" | "yes" => Some(true),
                "0" | "false" | "off" | "no" => Some(false),
                _ => None,
            })
        };
        let test_titles = match env("VICTORY_TEST_TITLES") {
            Some(v) if !v.is_empty() => std::env::split_paths(&v)
                .filter(|p| !p.as_os_str().is_empty())
                .collect(),
            _ => file.test_titles,
        };
        let cache_dir = env_path("VICTORY_CACHE_DIR")
            .or(file.cache_dir)
            .map_or_else(|| default_dir("cache"), Ok)?;
        let data_dir = env_path("VICTORY_DATA_DIR")
            .or(file.data_dir)
            .map_or_else(|| default_dir("data"), Ok)?;

        let mut fg = DlssFgConfig::default();
        if let Some(m) = file_fg.mode {
            fg.mode = m;
        }
        if let Some(f) = file_fg.frames {
            fg.frames = f.clamp(1, 5);
        }
        if let Some(fps) = file_fg.dynamic_target_fps {
            fg.dynamic_target_fps = fps.max(0.0);
        }
        if let Some(rt) = file_fg.runtime_mode {
            fg.runtime_mode = rt;
        }
        fg.runtime_dir = file_fg.runtime_dir;
        if let Some(pid) = file_fg.project_id {
            fg.project_id = pid;
        }
        if let Some(aid) = file_fg.application_id {
            fg.application_id = aid;
        }
        if let Some(v) = file_fg.ui_recomposition {
            fg.ui_recomposition = v;
        }
        if let Some(v) = file_fg.retain_when_off {
            fg.retain_when_off = v;
        }
        if let Some(v) = file_fg.fullscreen_menu_detection {
            fg.fullscreen_menu_detection = v;
        }
        if let Some(hooks) = file_fg.hooks {
            fg.hooks = hooks;
        }

        if let Some(m) = env_str("VICTORY_DLSS_FG").and_then(|s| DlssFgMode::parse_str(&s)) {
            fg.mode = m;
        }
        if let Some(f) = env_str("VICTORY_DLSS_FG_FRAMES").and_then(|s| s.trim().parse::<u32>().ok()) {
            fg.frames = f.clamp(1, 5);
        }
        if let Some(fps) = env_str("VICTORY_DLSS_FG_TARGET_FPS").and_then(|s| s.trim().parse::<f32>().ok()) {
            fg.dynamic_target_fps = fps.max(0.0);
        }
        if let Some(rt) = env_str("VICTORY_DLSS_FG_RUNTIME").and_then(|s| SlRuntimeMode::parse_str(&s)) {
            fg.runtime_mode = rt;
        }
        if let Some(dir) = env_path("VICTORY_SL_DIR") {
            fg.runtime_dir = Some(dir);
        }
        if let Some(pid) = env_str("VICTORY_SL_PROJECT_ID") {
            fg.project_id = pid;
        }
        if let Some(aid) = env_str("VICTORY_SL_APP_ID").and_then(|s| s.trim().parse::<u32>().ok()) {
            fg.application_id = aid;
        }
        if let Some(v) = env_bool("VICTORY_DLSS_FG_UI_RECOMP") {
            fg.ui_recomposition = v;
        }
        if let Some(v) = env_bool("VICTORY_DLSS_FG_PROBE") {
            fg.hooks.probe = v;
        }
        if let Some(v) = env_bool("VICTORY_DLSS_FG_HEURISTIC") {
            fg.hooks.heuristic = v;
        }
        if let Some(v) = env_bool("VICTORY_DLSS_FG_DEPTH_INVERTED") {
            fg.hooks.depth_inverted = v;
        }
        if let Some(v) = env_bool("VICTORY_DLSS_FG_MVEC_JITTERED") {
            fg.hooks.mvec_jittered = v;
        }
        if let Some(u) = env_str("VICTORY_DLSS_FG_MVEC_UNITS").and_then(|s| MvecUnits::parse_str(&s)) {
            fg.hooks.mvec_units = u;
        }
        if let Some((sx, sy)) = env_str("VICTORY_DLSS_FG_MVEC_SCALE").and_then(|s| {
            let (a, b) = s.split_once(',')?;
            Some((a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?))
        }) {
            fg.hooks.mvec_scale = Some([sx, sy]);
        }
        if let Some((w, h)) = env_str("VICTORY_DLSS_FG_RENDER_SIZE").and_then(|s| {
            let (a, b) = s.split_once(['x', 'X'])?;
            Some((a.trim().parse::<u32>().ok()?, b.trim().parse::<u32>().ok()?))
        }) {
            fg.hooks.render_width = Some(w);
            fg.hooks.render_height = Some(h);
        }

        Ok(Config {
            game_dir: env_path("VICTORY_GAME_DIR").or(file.game_dir),
            cache_dir,
            data_dir,
            test_titles,
            dlss_fg: fg,
            source,
        })
    }

    /// The game directory, or a helpful error if it is not configured.
    pub fn require_game_dir(&self) -> Result<&Path> {
        self.game_dir.as_deref().context(
            "no game directory configured: set VICTORY_GAME_DIR or [paths].game_dir in \
             victory.toml (see victory.example.toml)",
        )
    }

    /// Returns `<cache>/<sub>`, creating it, after checking the asset policy.
    pub fn cache_subdir(&self, sub: &str) -> Result<PathBuf> {
        subdir(&self.cache_dir, sub)
    }

    /// Returns `<data>/<sub>`, creating it, after checking the asset policy.
    pub fn data_subdir(&self, sub: &str) -> Result<PathBuf> {
        subdir(&self.data_dir, sub)
    }
}

/// The config file explicitly named by the environment, if any. An empty
/// `VICTORY_CONFIG` means unset, like the other path variables.
fn explicit_config_path(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> Option<PathBuf> {
    env("VICTORY_CONFIG")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn subdir(root: &Path, sub: &str) -> Result<PathBuf> {
    let dir = root.join(sub);
    ensure_outside_repo(&dir)?;
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir)
}

/// `%LOCALAPPDATA%/victory/<name>` (elsewhere the XDG or home cache).
fn default_dir(name: &str) -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("XDG_CACHE_HOME"))
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("cannot determine a user directory; set VICTORY_CACHE_DIR and VICTORY_DATA_DIR")?;
    Ok(base.join("victory").join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn environment_overrides_file() {
        let file = FilePaths {
            game_dir: Some("from-file".into()),
            cache_dir: Some("cache-from-file".into()),
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_GAME_DIR").then(|| OsString::from("from-env"));
        let cfg = Config::resolve(file, FileDlssFg::default(), None, env).unwrap();
        assert_eq!(cfg.game_dir.unwrap(), PathBuf::from("from-env"));
        assert_eq!(cfg.cache_dir, PathBuf::from("cache-from-file"));
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let file = FilePaths {
            game_dir: Some("from-file".into()),
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_GAME_DIR").then(OsString::new);
        let cfg = Config::resolve(file, FileDlssFg::default(), None, env).unwrap();
        assert_eq!(cfg.game_dir.unwrap(), PathBuf::from("from-file"));
    }

    #[test]
    fn empty_test_titles_env_is_ignored() {
        let file = FilePaths {
            test_titles: vec!["from-file".into()],
            ..Default::default()
        };
        let env = |k: &str| (k == "VICTORY_TEST_TITLES").then(OsString::new);
        let cfg = Config::resolve(file, FileDlssFg::default(), None, env).unwrap();
        assert_eq!(cfg.test_titles, vec![PathBuf::from("from-file")]);
    }

    #[test]
    fn test_titles_env_splits_paths() {
        let value = std::env::join_paths(["first", "second"]).unwrap();
        let file = FilePaths::default();
        let env = |k: &str| (k == "VICTORY_TEST_TITLES").then(|| value.clone());
        let cfg = Config::resolve(file, FileDlssFg::default(), None, env).unwrap();
        assert_eq!(
            cfg.test_titles,
            vec![PathBuf::from("first"), PathBuf::from("second")]
        );
    }

    #[test]
    fn dlss_fg_toml_and_env_overrides() {
        let toml_text = r#"
[paths]
game_dir = 'D:\game'

[dlss_fg]
mode = "auto"
frames = 3
runtime_mode = "development"

[dlss_fg.hooks]
heuristic = false
depth_inverted = true
mvec_units = "pixels"
mvec_scale = [1.0, -1.0]
render_width = 2560
render_height = 1440

[[dlss_fg.hooks.rules]]
role = "motion_vectors"
binding = "uav"
format = "R16G16_FLOAT"
width = 2560
height = 1440
occurrence = "last"
snapshot = false
"#;
        let parsed: FileConfig = toml::from_str(toml_text).unwrap();
        let env = |k: &str| match k {
            "VICTORY_DLSS_FG" => Some(OsString::from("on")),
            "VICTORY_DLSS_FG_FRAMES" => Some(OsString::from("9")),
            "VICTORY_DLSS_FG_PROBE" => Some(OsString::from("1")),
            _ => None,
        };
        let cfg = Config::resolve(parsed.paths, parsed.dlss_fg, None, env).unwrap();
        assert_eq!(cfg.dlss_fg.mode, DlssFgMode::On);
        assert_eq!(cfg.dlss_fg.frames, 5);
        assert_eq!(cfg.dlss_fg.runtime_mode, SlRuntimeMode::Development);
        assert!(!cfg.dlss_fg.hooks.heuristic);
        assert!(cfg.dlss_fg.hooks.probe);
        assert_eq!(cfg.dlss_fg.hooks.mvec_units, MvecUnits::Pixels);
        assert_eq!(cfg.dlss_fg.hooks.mvec_scale, Some([1.0, -1.0]));
        assert_eq!(cfg.dlss_fg.hooks.rules.len(), 1);
        assert_eq!(cfg.dlss_fg.hooks.rules[0].role, HookRole::MotionVectors);
    }

    #[test]
    fn empty_config_env_is_ignored() {
        let empty = |k: &str| (k == "VICTORY_CONFIG").then(OsString::new);
        assert_eq!(explicit_config_path(empty), None);
        let set = |k: &str| (k == "VICTORY_CONFIG").then(|| OsString::from("custom.toml"));
        assert_eq!(
            explicit_config_path(set),
            Some(PathBuf::from("custom.toml"))
        );
    }

    #[test]
    fn example_config_parses() {
        let text = include_str!("../../../victory.example.toml");
        let parsed: FileConfig = toml::from_str(text).unwrap();
        assert!(parsed.paths.game_dir.is_some());
    }

    #[test]
    fn cache_inside_repo_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("cache");
        let cfg = Config {
            cache_dir: inside,
            ..Default::default()
        };
        assert!(cfg.cache_subdir("x").is_err());
    }

    #[test]
    fn data_inside_repo_is_refused() {
        let inside = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        let cfg = Config {
            data_dir: inside,
            ..Default::default()
        };
        assert!(cfg.data_subdir("x").is_err());
    }
}
