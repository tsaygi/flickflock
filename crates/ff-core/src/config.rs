//! Settings, persisted as human-readable TOML in the OS config directory.
//! Loading is lenient: anything invalid falls back to its default with a
//! warning instead of failing.

use crate::folder::SortSpec;
use crate::input::KeyBindings;
use crate::nav::{EndOfFolder, FolderSort};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn project_dirs() -> Option<directories::ProjectDirs> {
    directories::ProjectDirs::from("", "", "flickflock")
}

/// `~/Library/Application Support/flickflock`, `%APPDATA%\flickflock`, `~/.config/flickflock`.
pub fn config_dir() -> Option<PathBuf> {
    let dirs = project_dirs()?;
    // On Windows `config_dir` is `...\flickflock\config`; keep it flat.
    Some(if cfg!(windows) {
        dirs.config_dir()
            .parent()
            .unwrap_or(dirs.config_dir())
            .to_path_buf()
    } else {
        dirs.config_dir().to_path_buf()
    })
}

/// `~/Library/Caches/flickflock`, `%LOCALAPPDATA%\flickflock\cache`, `~/.cache/flickflock`.
pub fn cache_dir() -> Option<PathBuf> {
    Some(project_dirs()?.cache_dir().to_path_buf())
}

/// The config file. `FLICKFLOCK_CONFIG` overrides the location (portable
/// installs, tests).
pub fn config_file() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("FLICKFLOCK_CONFIG").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    Some(config_dir()?.join("config.toml"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Background {
    Black,
    #[default]
    DarkGray,
    Checkerboard,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScrollMode {
    #[default]
    Zoom,
    Navigate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StripPosition {
    Top,
    #[default]
    Bottom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FolderEntry {
    #[default]
    First,
    Last,
}

/// Names of the built-in transition shaders.
pub const TRANSITIONS: [&str; 11] = [
    "none",
    "crossfade",
    "slide",
    "push",
    "zoom",
    "wipe",
    "iris",
    "page_curl",
    "ripple",
    "pixelate",
    "dissolve",
];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    pub end_of_folder: EndOfFolder,
    pub background: Background,
    pub zoom_small_to_fit: bool,
    pub scroll: ScrollMode,
    /// Opening another file reuses the running window.
    pub single_window: bool,
    /// Nearest-neighbour sampling above 200% (pixel art).
    pub pixelated_zoom: bool,
    /// Windows/Linux: hide the title bar.
    pub borderless: bool,
    pub show_thumbnails: bool,
    pub strip_position: StripPosition,
    pub thumbnail_size: f32,
    pub show_info: bool,
    pub confirm_overwrite: bool,
}

impl Default for General {
    fn default() -> Self {
        Self {
            end_of_folder: EndOfFolder::Continue,
            background: Background::DarkGray,
            zoom_small_to_fit: false,
            scroll: ScrollMode::Zoom,
            single_window: true,
            pixelated_zoom: true,
            borderless: false,
            show_thumbnails: false,
            strip_position: StripPosition::Bottom,
            thumbnail_size: 72.0,
            show_info: false,
            confirm_overwrite: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keys {
    /// New bindings recorded in Settings bind by character instead of key position.
    pub bind_by_character: bool,
    /// Holding a navigation key flips continuously.
    pub key_repeat: bool,
    /// Images per second while a key is held.
    pub repeat_rate: f32,
}

impl Default for Keys {
    fn default() -> Self {
        Self {
            bind_by_character: false,
            key_repeat: true,
            repeat_rate: 12.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Folders {
    /// Walk the whole tree depth-first instead of sibling folders only.
    pub traverse_tree: bool,
    /// Root for tree traversal (empty = parent of the first opened folder).
    pub tree_root: Option<PathBuf>,
    pub sort: FolderSort,
    /// First press at a folder boundary only announces the next folder.
    pub boundary_pause: bool,
    /// Where Previous-folder lands.
    pub prev_folder_entry: FolderEntry,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Performance {
    /// Budget for decoded images kept on the GPU, in MiB.
    pub cache_mb: u32,
    /// Images to prefetch ahead / behind.
    pub prefetch_ahead: u32,
    pub prefetch_behind: u32,
}

impl Default for Performance {
    fn default() -> Self {
        Self {
            cache_mb: 512,
            prefetch_ahead: 2,
            prefetch_behind: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transitions {
    /// Transition while browsing (default: none = instant).
    pub browse: String,
    pub slideshow: String,
    pub duration_ms: u32,
}

impl Default for Transitions {
    fn default() -> Self {
        Self {
            browse: "none".into(),
            slideshow: "crossfade".into(),
            duration_ms: 350,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Slideshow {
    pub interval_secs: f32,
    pub shuffle: bool,
    pub looping: bool,
}

impl Default for Slideshow {
    fn default() -> Self {
        Self {
            interval_secs: 4.0,
            shuffle: false,
            looping: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Metadata {
    /// Property keys ("EXIF/ExposureTime") drawn over the image.
    pub pinned: Vec<String>,
}

/// The whole configuration file.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub general: General,
    pub keys: Keys,
    pub folders: Folders,
    pub performance: Performance,
    pub transitions: Transitions,
    pub slideshow: Slideshow,
    pub sorting: SortSpec,
    pub metadata: Metadata,
    pub bindings: KeyBindings,
}

impl Settings {
    /// Parse a config document. Returns the settings plus human-readable
    /// warnings for everything that was ignored.
    pub fn from_toml(text: &str, is_mac: bool) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut settings = Settings {
            bindings: KeyBindings::defaults(is_mac),
            ..Default::default()
        };
        let doc: toml::Table = match text.parse() {
            Ok(t) => t,
            Err(e) => {
                warnings.push(format!(
                    "config file is not valid TOML, using defaults: {e}"
                ));
                return (settings, warnings);
            }
        };
        settings.general = section(&doc, "general", &mut warnings);
        settings.keys = section(&doc, "keys", &mut warnings);
        settings.folders = section(&doc, "folders", &mut warnings);
        settings.performance = section(&doc, "performance", &mut warnings);
        settings.transitions = section(&doc, "transitions", &mut warnings);
        settings.slideshow = section(&doc, "slideshow", &mut warnings);
        settings.sorting = section(&doc, "sorting", &mut warnings);
        settings.metadata = section(&doc, "metadata", &mut warnings);
        settings.sanitize(&mut warnings);
        if let Some(value) = doc.get("bindings") {
            match value.clone().try_into::<BTreeMap<String, toml::Value>>() {
                Ok(raw) => {
                    let mut table = BTreeMap::new();
                    for (action, v) in raw {
                        match v.try_into::<Vec<String>>() {
                            Ok(list) => {
                                table.insert(action, list);
                            }
                            Err(_) => warnings
                                .push(format!("bindings.{action}: expected a list of strings")),
                        }
                    }
                    let (bindings, w) = KeyBindings::from_table(&table, is_mac);
                    settings.bindings = bindings;
                    warnings.extend(w.into_iter().map(|w| format!("bindings: {w}")));
                }
                Err(_) => warnings.push("[bindings] must be a table".into()),
            }
        }
        (settings, warnings)
    }

    fn sanitize(&mut self, warnings: &mut Vec<String>) {
        let mut clamp = |name: &str, v: &mut f32, lo: f32, hi: f32, default: f32| {
            if !v.is_finite() {
                warnings.push(format!("{name} is not a number, using {default}"));
                *v = default;
            } else if *v < lo || *v > hi {
                warnings.push(format!(
                    "{name} = {v} is out of range, clamped to {lo}..{hi}"
                ));
                *v = v.clamp(lo, hi);
            }
        };
        clamp(
            "general.thumbnail_size",
            &mut self.general.thumbnail_size,
            32.0,
            256.0,
            72.0,
        );
        clamp(
            "keys.repeat_rate",
            &mut self.keys.repeat_rate,
            1.0,
            60.0,
            12.0,
        );
        clamp(
            "slideshow.interval_secs",
            &mut self.slideshow.interval_secs,
            0.2,
            3600.0,
            4.0,
        );
        self.performance.cache_mb = self.performance.cache_mb.clamp(64, 65_536);
        self.performance.prefetch_ahead = self.performance.prefetch_ahead.min(8);
        self.performance.prefetch_behind = self.performance.prefetch_behind.min(8);
        self.transitions.duration_ms = self.transitions.duration_ms.clamp(50, 5000);
        for (name, t, default) in [
            ("transitions.browse", &mut self.transitions.browse, "none"),
            (
                "transitions.slideshow",
                &mut self.transitions.slideshow,
                "crossfade",
            ),
        ] {
            if !TRANSITIONS.contains(&t.as_str()) {
                warnings.push(format!(
                    "{name}: unknown transition {t:?}, using {default:?}"
                ));
                *t = default.to_owned();
            }
        }
    }

    pub fn to_toml(&self) -> String {
        #[derive(Serialize)]
        struct Doc<'a> {
            general: &'a General,
            keys: &'a Keys,
            folders: &'a Folders,
            performance: &'a Performance,
            transitions: &'a Transitions,
            slideshow: &'a Slideshow,
            sorting: &'a SortSpec,
            metadata: &'a Metadata,
            bindings: BTreeMap<String, Vec<String>>,
        }
        let doc = Doc {
            general: &self.general,
            keys: &self.keys,
            folders: &self.folders,
            performance: &self.performance,
            transitions: &self.transitions,
            slideshow: &self.slideshow,
            sorting: &self.sorting,
            metadata: &self.metadata,
            bindings: self.bindings.to_table(),
        };
        match toml::to_string_pretty(&doc) {
            Ok(s) => format!("# FlickFlock settings. Edited by the Settings window; manual edits are read at startup.\n\n{s}"),
            Err(e) => {
                tracing::error!("could not serialize settings: {e}");
                String::new()
            }
        }
    }

    /// Load from `path`. A missing file yields defaults without warnings.
    pub fn load(path: &Path, is_mac: bool) -> (Self, Vec<String>) {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text, is_mac),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (
                Settings {
                    bindings: KeyBindings::defaults(is_mac),
                    ..Default::default()
                },
                Vec::new(),
            ),
            Err(e) => (
                Settings {
                    bindings: KeyBindings::defaults(is_mac),
                    ..Default::default()
                },
                vec![format!("could not read {}: {e}", path.display())],
            ),
        }
    }

    /// Save atomically (write to a temp file, then rename).
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml())?;
        std::fs::rename(&tmp, path)
    }
}

/// Deserialize one `[section]`. If the section as a whole is invalid, each
/// key is tried on its own so one bad value does not reset its neighbours.
fn section<T: serde::de::DeserializeOwned + Default>(
    doc: &toml::Table,
    name: &str,
    warnings: &mut Vec<String>,
) -> T {
    let Some(value) = doc.get(name) else {
        return T::default();
    };
    let Some(table) = value.as_table() else {
        warnings.push(format!("[{name}] must be a table, using defaults"));
        return T::default();
    };
    if let Ok(parsed) = value.clone().try_into::<T>() {
        return parsed;
    }
    let mut good = toml::Table::new();
    for (key, v) in table {
        let mut single = toml::Table::new();
        single.insert(key.clone(), v.clone());
        if toml::Value::Table(single).try_into::<T>().is_ok() {
            good.insert(key.clone(), v.clone());
        } else {
            warnings.push(format!(
                "{name}.{key} has an invalid value ({v}), using the default"
            ));
        }
    }
    toml::Value::Table(good).try_into::<T>().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::folder::{GroupKey, SortKey};
    use crate::input::{Action, Binding};

    #[test]
    fn round_trip() {
        let mut s = Settings {
            bindings: KeyBindings::defaults(true),
            ..Default::default()
        };
        s.general.end_of_folder = EndOfFolder::Wrap;
        s.general.background = Background::Checkerboard;
        s.folders.traverse_tree = true;
        s.folders.tree_root = Some(PathBuf::from("/photos/Trip"));
        s.sorting = SortSpec {
            key: SortKey::Taken,
            descending: true,
            group_by: vec![GroupKey::Year, GroupKey::Type],
            recursive: false,
        };
        s.metadata.pinned = vec!["EXIF/ExposureTime".into()];
        s.transitions.slideshow = "ripple".into();
        s.bindings
            .add(Action::NextImage, Binding::plain("L"), true)
            .unwrap();
        let text = s.to_toml();
        let (loaded, warnings) = Settings::from_toml(&text, true);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded, s);
    }

    #[test]
    fn empty_and_missing_files_give_defaults() {
        let (s, w) = Settings::from_toml("", false);
        assert!(w.is_empty());
        assert_eq!(s.general, General::default());
        assert_eq!(s.bindings, KeyBindings::defaults(false));
        let (s, w) = Settings::load(Path::new("/definitely/not/here.toml"), true);
        assert!(w.is_empty());
        assert_eq!(s.performance.cache_mb, 512);
    }

    #[test]
    fn invalid_entries_fall_back_individually() {
        let text = r#"
            [general]
            end_of_folder = "teleport"
            zoom_small_to_fit = true
            thumbnail_size = 9000.0

            [performance]
            cache_mb = "lots"
            prefetch_ahead = 3

            [transitions]
            slideshow = "explode"

            [slideshow]
            interval_secs = "soon"

            folders = 3

            [bindings]
            next_image = ["L", "Mod+Q"]
            crop = "C"
        "#;
        let (s, w) = Settings::from_toml(text, true);
        assert_eq!(s.general.end_of_folder, EndOfFolder::Continue);
        assert!(s.general.zoom_small_to_fit, "valid neighbours are kept");
        assert_eq!(s.general.thumbnail_size, 256.0);
        assert_eq!(s.performance.cache_mb, 512);
        assert_eq!(s.performance.prefetch_ahead, 3);
        assert_eq!(s.transitions.slideshow, "crossfade");
        assert_eq!(s.slideshow.interval_secs, 4.0);
        assert_eq!(
            s.bindings.bindings(Action::NextImage),
            [Binding::plain("L")]
        );
        assert!(w.len() >= 6, "{w:#?}");
    }

    #[test]
    fn garbage_never_panics() {
        for text in [
            "[[[",
            "general = 1",
            "[general]\nbackground = 7",
            "\u{0}\u{1}",
            "[bindings]\nx = 1",
        ] {
            let (s, w) = Settings::from_toml(text, false);
            assert!(!w.is_empty(), "{text:?} should warn");
            assert_eq!(s.general, General::default());
        }
    }

    #[test]
    fn save_and_load_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/config.toml");
        let mut s = Settings {
            bindings: KeyBindings::defaults(false),
            ..Default::default()
        };
        s.keys.repeat_rate = 20.0;
        s.save(&path).unwrap();
        let (loaded, w) = Settings::load(&path, false);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(loaded, s);
    }

    #[test]
    fn default_transitions_exist() {
        let t = Transitions::default();
        assert!(
            TRANSITIONS.contains(&t.browse.as_str()) && TRANSITIONS.contains(&t.slideshow.as_str())
        );
        assert!(config_file().is_none_or(|p| p.extension().is_some()));
    }
}
