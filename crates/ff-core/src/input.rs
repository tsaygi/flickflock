//! Actions and key bindings.
//!
//! The UI never looks at raw keys: every key press, mouse button or scroll
//! gesture is turned into a [`Chord`] and looked up in [`KeyBindings`], which
//! yields an [`Action`]. Key names are plain strings (the UI toolkit's key
//! names, e.g. `"Right"`, `"L"`, `"F5"`) so this module stays GUI-free.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

macro_rules! actions {
    ($( $variant:ident => $name:literal, $label:literal; )*) => {
        /// Everything the user can trigger from the keyboard or mouse.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Action { $( $variant, )* }

        impl Action {
            pub const ALL: &'static [Action] = &[ $( Action::$variant, )* ];

            /// Stable identifier used in the config file.
            pub fn name(self) -> &'static str {
                match self { $( Action::$variant => $name, )* }
            }

            /// Human-readable label for the settings UI.
            pub fn label(self) -> &'static str {
                match self { $( Action::$variant => $label, )* }
            }

            pub fn from_name(name: &str) -> Option<Action> {
                match name { $( $name => Some(Action::$variant), )* _ => None }
            }
        }
    };
}

actions! {
    PrevImage => "prev_image", "Previous image";
    NextImage => "next_image", "Next image";
    PrevFolder => "prev_folder", "Previous folder";
    NextFolder => "next_folder", "Next folder";
    FirstImage => "first_image", "First image";
    LastImage => "last_image", "Last image";
    ZoomIn => "zoom_in", "Zoom in";
    ZoomOut => "zoom_out", "Zoom out";
    ZoomFit => "zoom_fit", "Fit to window";
    ZoomActual => "zoom_actual", "Actual size (100%)";
    ToggleFullscreen => "toggle_fullscreen", "Toggle full screen";
    Escape => "escape", "Cancel / exit full screen";
    ToggleThumbnails => "toggle_thumbnails", "Toggle thumbnails";
    ToggleInfo => "toggle_info", "Toggle info panel";
    HistoryBack => "history_back", "Back";
    HistoryForward => "history_forward", "Forward";
    Skim => "skim", "Skim (hold)";
    PlayPause => "play_pause", "Animation: play / pause";
    FramePrev => "frame_prev", "Animation: previous frame";
    FrameNext => "frame_next", "Animation: next frame";
    ReversePlay => "reverse_play", "Animation: reverse";
    RotateLeft => "rotate_left", "Rotate left";
    RotateRight => "rotate_right", "Rotate right";
    FlipHorizontal => "flip_horizontal", "Flip horizontal";
    FlipVertical => "flip_vertical", "Flip vertical";
    Crop => "crop", "Crop";
    Undo => "undo", "Undo edit";
    Redo => "redo", "Redo edit";
    Save => "save", "Save";
    SaveAs => "save_as", "Save as…";
    Trash => "trash", "Move to Trash";
    Reveal => "reveal", "Reveal in file manager";
    Rename => "rename", "Rename";
    CopyImage => "copy_image", "Copy image";
    CopyPath => "copy_path", "Copy path";
    OpenFile => "open_file", "Open…";
    OpenFolder => "open_folder", "Open folder…";
    OpenWith => "open_with", "Open with default app";
    Slideshow => "slideshow", "Slideshow";
    Settings => "settings", "Settings";
}

impl Action {
    /// The navigation actions shown (and editable) in Settings > Keys.
    pub const NAVIGATION: [Action; 6] = [
        Action::PrevImage,
        Action::NextImage,
        Action::PrevFolder,
        Action::NextFolder,
        Action::FirstImage,
        Action::LastImage,
    ];

    pub fn is_navigation(self) -> bool {
        Self::NAVIGATION.contains(&self)
    }

    /// Actions that may fire repeatedly while the key is held down.
    pub fn repeats(self) -> bool {
        matches!(
            self,
            Action::PrevImage
                | Action::NextImage
                | Action::ZoomIn
                | Action::ZoomOut
                | Action::FramePrev
                | Action::FrameNext
        )
    }
}

/// Modifier keys. `command` is ⌘ on macOS and Ctrl elsewhere ("Mod").
/// `ctrl` is the physical Control key and only distinct from `command` on macOS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub struct Mods {
    pub command: bool,
    pub alt: bool,
    pub shift: bool,
    pub ctrl: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        command: false,
        alt: false,
        shift: false,
        ctrl: false,
    };
    pub const COMMAND: Mods = Mods {
        command: true,
        ..Self::NONE
    };
    pub const ALT: Mods = Mods {
        alt: true,
        ..Self::NONE
    };
    pub const SHIFT: Mods = Mods {
        shift: true,
        ..Self::NONE
    };
    pub const COMMAND_SHIFT: Mods = Mods {
        command: true,
        shift: true,
        ..Self::NONE
    };

    /// On Windows/Linux `Mod` *is* Ctrl, so fold `ctrl` into `command`.
    pub fn normalized(mut self, is_mac: bool) -> Mods {
        if !is_mac && self.ctrl {
            self.ctrl = false;
            self.command = true;
        }
        self
    }

    pub fn is_none(self) -> bool {
        self == Self::NONE
    }
}

/// What was pressed, without modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Trigger {
    /// A key identified by its physical position (layout-independent).
    Key(String),
    /// A key identified by the character it produces in the active layout.
    Char(String),
    /// Mouse "back" button (button 4).
    MouseBack,
    /// Mouse "forward" button (button 5).
    MouseForward,
    /// Horizontal scroll / trackpad swipe towards the left.
    ScrollLeft,
    /// Horizontal scroll / trackpad swipe towards the right.
    ScrollRight,
}

/// One binding: modifiers plus a trigger. Serialized as e.g. `"Alt+Right"`,
/// `"Mod+Shift+R"`, `"char:L"`, `"Mouse4"`, `"ScrollRight"`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Binding {
    pub mods: Mods,
    pub trigger: Trigger,
}

impl Binding {
    pub fn key(mods: Mods, key: &str) -> Self {
        Self {
            mods,
            trigger: Trigger::Key(key.to_owned()),
        }
    }

    pub fn plain(key: &str) -> Self {
        Self::key(Mods::NONE, key)
    }

    pub fn is_keyboard(&self) -> bool {
        matches!(self.trigger, Trigger::Key(_) | Trigger::Char(_))
    }

    /// Friendly text for the UI ("⌘⇧R" style on macOS, "Ctrl+Shift+R" elsewhere).
    pub fn display(&self, is_mac: bool) -> String {
        let mut s = String::new();
        let m = self.mods;
        if is_mac {
            for (on, sym) in [
                (m.ctrl, "⌃"),
                (m.alt, "⌥"),
                (m.shift, "⇧"),
                (m.command, "⌘"),
            ] {
                if on {
                    s.push_str(sym);
                }
            }
        } else {
            for (on, sym) in [
                (m.command || m.ctrl, "Ctrl+"),
                (m.alt, "Alt+"),
                (m.shift, "Shift+"),
            ] {
                if on {
                    s.push_str(sym);
                }
            }
        }
        s.push_str(&match &self.trigger {
            Trigger::Key(k) => match k.as_str() {
                "Left" => "←".to_owned(),
                "Right" => "→".to_owned(),
                "Up" => "↑".to_owned(),
                "Down" => "↓".to_owned(),
                other => other.to_owned(),
            },
            Trigger::Char(c) => format!("“{c}”"),
            Trigger::MouseBack => "Mouse 4".to_owned(),
            Trigger::MouseForward => "Mouse 5".to_owned(),
            Trigger::ScrollLeft => "Scroll ←".to_owned(),
            Trigger::ScrollRight => "Scroll →".to_owned(),
        });
        s
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let m = self.mods;
        for (on, name) in [
            (m.command, "Mod+"),
            (m.ctrl, "Ctrl+"),
            (m.alt, "Alt+"),
            (m.shift, "Shift+"),
        ] {
            if on {
                f.write_str(name)?;
            }
        }
        match &self.trigger {
            Trigger::Key(k) => f.write_str(k),
            Trigger::Char(c) => write!(f, "char:{c}"),
            Trigger::MouseBack => f.write_str("Mouse4"),
            Trigger::MouseForward => f.write_str("Mouse5"),
            Trigger::ScrollLeft => f.write_str("ScrollLeft"),
            Trigger::ScrollRight => f.write_str("ScrollRight"),
        }
    }
}

impl FromStr for Binding {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut mods = Mods::NONE;
        let mut rest = s.trim();
        loop {
            let lower = rest.to_ascii_lowercase();
            let (flag, len): (&mut bool, usize) =
                if lower.starts_with("mod+") || lower.starts_with("cmd+") {
                    (&mut mods.command, 4)
                } else if lower.starts_with("ctrl+") {
                    (&mut mods.ctrl, 5)
                } else if lower.starts_with("alt+") {
                    (&mut mods.alt, 4)
                } else if lower.starts_with("shift+") {
                    (&mut mods.shift, 6)
                } else {
                    break;
                };
            *flag = true;
            rest = &rest[len..];
        }
        if rest.is_empty() {
            return Err(format!("binding {s:?} has no key"));
        }
        let trigger = match rest {
            "Mouse4" | "MouseBack" => Trigger::MouseBack,
            "Mouse5" | "MouseForward" => Trigger::MouseForward,
            "ScrollLeft" => Trigger::ScrollLeft,
            "ScrollRight" => Trigger::ScrollRight,
            _ => match rest.strip_prefix("char:") {
                Some("") => return Err(format!("binding {s:?} has no character")),
                Some(c) => Trigger::Char(c.to_owned()),
                None => Trigger::Key(rest.to_owned()),
            },
        };
        Ok(Binding { mods, trigger })
    }
}

/// An input event as seen by the router: modifiers plus what was pressed.
/// For key presses both the physical key and the layout-dependent key are
/// provided so either binding style can match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Chord {
    Key {
        mods: Mods,
        physical: Option<String>,
        logical: String,
    },
    Pointer {
        mods: Mods,
        trigger: Trigger,
    },
}

/// Why a binding was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindError {
    /// Reserved by the OS or the app (quit, close window, …).
    Reserved,
    /// Already bound to another action.
    Conflict(Action),
}

/// The complete action → bindings table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBindings {
    map: BTreeMap<Action, Vec<Binding>>,
}

impl Default for KeyBindings {
    fn default() -> Self {
        Self::defaults(crate::IS_MAC)
    }
}

impl KeyBindings {
    /// Factory bindings. `is_mac` selects the platform-specific ones.
    pub fn defaults(is_mac: bool) -> Self {
        use Action::*;
        let k = Binding::plain;
        let m = |key: &str| Binding::key(Mods::COMMAND, key);
        let ms = |key: &str| Binding::key(Mods::COMMAND_SHIFT, key);
        let mut map: BTreeMap<Action, Vec<Binding>> = BTreeMap::new();
        let mut set = |a: Action, b: Vec<Binding>| {
            map.insert(a, b);
        };
        set(
            PrevImage,
            vec![k("Left"), Binding::key(Mods::SHIFT, "Space")],
        );
        set(NextImage, vec![k("Right"), k("Space")]);
        set(PrevFolder, vec![Binding::key(Mods::ALT, "Left")]);
        set(NextFolder, vec![Binding::key(Mods::ALT, "Right")]);
        set(FirstImage, vec![k("Home")]);
        set(LastImage, vec![k("End")]);
        set(ZoomIn, vec![m("Equals"), m("Plus")]);
        set(ZoomOut, vec![m("Minus")]);
        set(ZoomFit, vec![m("0")]);
        set(ZoomActual, vec![m("1")]);
        set(
            ToggleFullscreen,
            if is_mac {
                vec![k("F")]
            } else {
                vec![k("F"), k("F11")]
            },
        );
        set(Escape, vec![k("Escape")]);
        set(ToggleThumbnails, vec![k("T")]);
        set(ToggleInfo, vec![k("I")]);
        set(
            HistoryBack,
            vec![
                m("OpenBracket"),
                Binding {
                    mods: Mods::NONE,
                    trigger: Trigger::MouseBack,
                },
            ],
        );
        set(
            HistoryForward,
            vec![
                m("CloseBracket"),
                Binding {
                    mods: Mods::NONE,
                    trigger: Trigger::MouseForward,
                },
            ],
        );
        set(Skim, vec![k("S")]);
        set(PlayPause, vec![k("P")]);
        set(FramePrev, vec![k("Comma")]);
        set(FrameNext, vec![k("Period")]);
        set(ReversePlay, vec![k("R")]);
        set(RotateLeft, vec![m("L")]);
        set(RotateRight, vec![m("R")]);
        set(FlipHorizontal, vec![k("H")]);
        set(FlipVertical, vec![k("V")]);
        set(Crop, vec![k("C")]);
        set(Undo, vec![m("Z")]);
        set(Redo, vec![ms("Z")]);
        set(Save, vec![m("S")]);
        set(
            SaveAs,
            vec![Binding::key(
                Mods {
                    command: true,
                    alt: true,
                    ..Mods::NONE
                },
                "S",
            )],
        );
        set(
            Trash,
            if is_mac {
                vec![m("Backspace")]
            } else {
                vec![k("Delete")]
            },
        );
        set(Reveal, vec![ms("R")]);
        set(Rename, vec![k("F2")]);
        set(CopyImage, vec![m("C")]);
        set(CopyPath, vec![ms("C")]);
        set(OpenFile, vec![m("O")]);
        set(OpenFolder, vec![ms("O")]);
        set(OpenWith, vec![m("Enter")]);
        set(Slideshow, vec![ms("S")]);
        set(Settings, vec![m("Comma")]);
        Self { map }
    }

    pub fn bindings(&self, action: Action) -> &[Binding] {
        self.map.get(&action).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Which action (if any) a binding currently triggers.
    pub fn action_for(&self, binding: &Binding) -> Option<Action> {
        self.map
            .iter()
            .find(|(_, bs)| bs.contains(binding))
            .map(|(a, _)| *a)
    }

    /// Resolve an input event to an action. Modifiers must match exactly, so
    /// `Space` and `Shift+Space` are different bindings.
    pub fn resolve(&self, chord: &Chord) -> Option<Action> {
        self.map
            .iter()
            .find(|(_, bs)| bs.iter().any(|b| binding_matches(b, chord)))
            .map(|(a, _)| *a)
    }

    /// Shortcuts owned by the OS or the app that can never be rebound.
    pub fn is_reserved(binding: &Binding, is_mac: bool) -> bool {
        let m = binding.mods.normalized(is_mac);
        let key = match &binding.trigger {
            Trigger::Key(k) | Trigger::Char(k) => k.to_ascii_uppercase(),
            _ => return false,
        };
        let cmd_only = m == Mods::COMMAND;
        let alt_only = m == Mods::ALT;
        let cmd_alt = m.command && m.alt && !m.shift;
        let cmd_ctrl = m.command && m.ctrl;
        match key.as_str() {
            "Q" | "W" if cmd_only => true,                   // quit / close window
            "F4" if alt_only && !is_mac => true,             // close window
            "H" | "M" | "TAB" if cmd_only && is_mac => true, // hide / minimise / app switcher
            "TAB" if alt_only && !is_mac => true,            // window switcher
            "ESCAPE" if cmd_alt && is_mac => true,           // force quit
            "Q" | "F" if cmd_ctrl && is_mac => true,         // lock screen / system full screen
            "SPACE" if cmd_only && is_mac => true,           // Spotlight
            "DELETE" if m.command && m.alt && !is_mac => true, // Ctrl+Alt+Del
            _ => false,
        }
    }

    /// Check whether `binding` could be added to `action`.
    pub fn check(&self, action: Action, binding: &Binding, is_mac: bool) -> Result<(), BindError> {
        if Self::is_reserved(binding, is_mac) {
            return Err(BindError::Reserved);
        }
        match self.action_for(binding) {
            Some(other) if other != action => Err(BindError::Conflict(other)),
            _ => Ok(()),
        }
    }

    /// Add a binding. Fails on reserved shortcuts and on conflicts (use
    /// [`KeyBindings::reassign`] to take a binding away from another action).
    pub fn add(&mut self, action: Action, binding: Binding, is_mac: bool) -> Result<(), BindError> {
        self.check(action, &binding, is_mac)?;
        let list = self.map.entry(action).or_default();
        if !list.contains(&binding) {
            list.push(binding);
        }
        Ok(())
    }

    /// Add a binding, removing it from whichever action had it before.
    pub fn reassign(
        &mut self,
        action: Action,
        binding: Binding,
        is_mac: bool,
    ) -> Result<(), BindError> {
        if Self::is_reserved(&binding, is_mac) {
            return Err(BindError::Reserved);
        }
        for list in self.map.values_mut() {
            list.retain(|b| *b != binding);
        }
        self.map.entry(action).or_default().push(binding);
        Ok(())
    }

    pub fn remove(&mut self, action: Action, binding: &Binding) {
        if let Some(list) = self.map.get_mut(&action) {
            list.retain(|b| b != binding);
        }
    }

    /// Restore the factory bindings of the navigation actions only, dropping
    /// any conflicting bindings from other actions.
    pub fn restore_navigation_defaults(&mut self, is_mac: bool) {
        let defaults = Self::defaults(is_mac);
        for a in Action::NAVIGATION {
            self.map.insert(a, Vec::new());
        }
        for a in Action::NAVIGATION {
            for b in defaults.bindings(a) {
                let _ = self.reassign(a, b.clone(), is_mac);
            }
        }
    }

    /// Serializable form: action name → list of binding strings.
    pub fn to_table(&self) -> BTreeMap<String, Vec<String>> {
        self.map
            .iter()
            .map(|(a, bs)| {
                (
                    a.name().to_owned(),
                    bs.iter().map(Binding::to_string).collect(),
                )
            })
            .collect()
    }

    /// Build bindings from a (possibly hand-edited) table. Unknown actions,
    /// unparsable, reserved and duplicate bindings are skipped and reported
    /// in the returned warnings; actions missing from the table keep their
    /// defaults (minus anything the table assigned elsewhere).
    pub fn from_table(table: &BTreeMap<String, Vec<String>>, is_mac: bool) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut result = Self {
            map: BTreeMap::new(),
        };
        for (name, list) in table {
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("unknown action {name:?} ignored"));
                continue;
            };
            result.map.entry(action).or_default();
            for text in list {
                let binding = match text.parse::<Binding>() {
                    Ok(b) => Binding {
                        mods: b.mods.normalized(is_mac),
                        trigger: b.trigger,
                    },
                    Err(e) => {
                        warnings.push(format!("{name}: {e}"));
                        continue;
                    }
                };
                match result.add(action, binding, is_mac) {
                    Ok(()) => {}
                    Err(BindError::Reserved) => {
                        warnings.push(format!("{name}: {text:?} is a reserved shortcut"))
                    }
                    Err(BindError::Conflict(other)) => warnings.push(format!(
                        "{name}: {text:?} is already bound to {}",
                        other.name()
                    )),
                }
            }
        }
        let defaults = Self::defaults(is_mac);
        for action in Action::ALL {
            if !result.map.contains_key(action) {
                let free: Vec<Binding> = defaults
                    .bindings(*action)
                    .iter()
                    .filter(|b| result.action_for(b).is_none())
                    .cloned()
                    .collect();
                result.map.insert(*action, free);
            }
        }
        (result, warnings)
    }

    /// Export as a standalone TOML document.
    pub fn export_toml(&self) -> String {
        #[derive(Serialize)]
        struct Doc {
            keys: BTreeMap<String, Vec<String>>,
        }
        toml::to_string_pretty(&Doc {
            keys: self.to_table(),
        })
        .unwrap_or_default()
    }

    /// Import from a TOML document produced by [`KeyBindings::export_toml`].
    pub fn import_toml(text: &str, is_mac: bool) -> Result<(Self, Vec<String>), String> {
        #[derive(Deserialize)]
        struct Doc {
            keys: BTreeMap<String, Vec<String>>,
        }
        let doc: Doc = toml::from_str(text).map_err(|e| e.to_string())?;
        Ok(Self::from_table(&doc.keys, is_mac))
    }
}

fn binding_matches(b: &Binding, chord: &Chord) -> bool {
    match chord {
        Chord::Key {
            mods,
            physical,
            logical,
        } => {
            if b.mods != *mods {
                return false;
            }
            match &b.trigger {
                // Fall back to the logical key when the platform reports no physical key.
                Trigger::Key(k) => physical.as_deref().unwrap_or(logical) == k,
                Trigger::Char(c) => logical.eq_ignore_ascii_case(c),
                _ => false,
            }
        }
        Chord::Pointer { mods, trigger } => b.mods == *mods && b.trigger == *trigger,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(mods: Mods, physical: &str) -> Chord {
        Chord::Key {
            mods,
            physical: Some(physical.into()),
            logical: physical.into(),
        }
    }

    #[test]
    fn default_navigation_keys() {
        for is_mac in [true, false] {
            let kb = KeyBindings::defaults(is_mac);
            assert_eq!(
                kb.resolve(&key(Mods::NONE, "Right")),
                Some(Action::NextImage)
            );
            assert_eq!(
                kb.resolve(&key(Mods::NONE, "Left")),
                Some(Action::PrevImage)
            );
            assert_eq!(
                kb.resolve(&key(Mods::NONE, "Space")),
                Some(Action::NextImage)
            );
            assert_eq!(
                kb.resolve(&key(Mods::SHIFT, "Space")),
                Some(Action::PrevImage)
            );
            assert_eq!(
                kb.resolve(&key(Mods::ALT, "Right")),
                Some(Action::NextFolder)
            );
            assert_eq!(
                kb.resolve(&key(Mods::ALT, "Left")),
                Some(Action::PrevFolder)
            );
            assert_eq!(
                kb.resolve(&key(Mods::NONE, "Home")),
                Some(Action::FirstImage)
            );
            assert_eq!(kb.resolve(&key(Mods::NONE, "End")), Some(Action::LastImage));
            assert_eq!(
                kb.resolve(&key(Mods::COMMAND, "R")),
                Some(Action::RotateRight)
            );
            assert_eq!(kb.resolve(&key(Mods::NONE, "R")), Some(Action::ReversePlay));
            assert_eq!(
                kb.resolve(&key(Mods::COMMAND_SHIFT, "R")),
                Some(Action::Reveal)
            );
            assert_eq!(kb.resolve(&key(Mods::NONE, "X")), None);
        }
        assert_eq!(
            KeyBindings::defaults(true).resolve(&key(Mods::COMMAND, "Backspace")),
            Some(Action::Trash)
        );
        assert_eq!(
            KeyBindings::defaults(false).resolve(&key(Mods::NONE, "Delete")),
            Some(Action::Trash)
        );
        assert_eq!(
            KeyBindings::defaults(false).resolve(&key(Mods::NONE, "F11")),
            Some(Action::ToggleFullscreen)
        );
    }

    #[test]
    fn defaults_have_no_conflicts_and_nothing_reserved() {
        for is_mac in [true, false] {
            let kb = KeyBindings::defaults(is_mac);
            let mut seen = std::collections::BTreeMap::new();
            for a in Action::ALL {
                for b in kb.bindings(*a) {
                    assert!(!KeyBindings::is_reserved(b, is_mac), "{b} is reserved");
                    if let Some(prev) = seen.insert(b.clone(), *a) {
                        panic!("{b} bound to both {prev:?} and {a:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn multiple_bindings_and_rebinding_j_l() {
        let mut kb = KeyBindings::defaults(false);
        kb.add(Action::NextImage, Binding::plain("L"), false)
            .unwrap();
        kb.add(Action::PrevImage, Binding::plain("J"), false)
            .unwrap();
        kb.add(Action::NextImage, Binding::plain("D"), false)
            .unwrap();
        assert_eq!(kb.resolve(&key(Mods::NONE, "L")), Some(Action::NextImage));
        assert_eq!(kb.resolve(&key(Mods::NONE, "J")), Some(Action::PrevImage));
        assert_eq!(kb.resolve(&key(Mods::NONE, "D")), Some(Action::NextImage));
        // Arrow keys are still bound.
        assert_eq!(
            kb.resolve(&key(Mods::NONE, "Right")),
            Some(Action::NextImage)
        );
        assert_eq!(kb.bindings(Action::NextImage).len(), 4);
        // Adding the same binding twice does not duplicate it.
        kb.add(Action::NextImage, Binding::plain("L"), false)
            .unwrap();
        assert_eq!(kb.bindings(Action::NextImage).len(), 4);
        kb.remove(Action::NextImage, &Binding::plain("L"));
        assert_eq!(kb.resolve(&key(Mods::NONE, "L")), None);
    }

    #[test]
    fn conflicts_are_detected_and_can_be_reassigned() {
        let mut kb = KeyBindings::defaults(true);
        let t = Binding::plain("T");
        assert_eq!(
            kb.add(Action::NextImage, t.clone(), true),
            Err(BindError::Conflict(Action::ToggleThumbnails))
        );
        assert_eq!(
            kb.resolve(&key(Mods::NONE, "T")),
            Some(Action::ToggleThumbnails)
        );
        kb.reassign(Action::NextImage, t.clone(), true).unwrap();
        assert_eq!(kb.resolve(&key(Mods::NONE, "T")), Some(Action::NextImage));
        assert!(kb.bindings(Action::ToggleThumbnails).is_empty());
        assert_eq!(kb.action_for(&t), Some(Action::NextImage));
    }

    #[test]
    fn reserved_shortcuts_cannot_be_bound() {
        let mut kb = KeyBindings::defaults(true);
        let quit = Binding::key(Mods::COMMAND, "Q");
        assert_eq!(
            kb.add(Action::NextImage, quit.clone(), true),
            Err(BindError::Reserved)
        );
        assert_eq!(
            kb.reassign(Action::NextImage, quit, true),
            Err(BindError::Reserved)
        );
        assert!(KeyBindings::is_reserved(
            &Binding::key(Mods::COMMAND, "W"),
            false
        ));
        assert!(KeyBindings::is_reserved(
            &Binding::key(Mods::ALT, "F4"),
            false
        ));
        assert!(!KeyBindings::is_reserved(
            &Binding::key(Mods::ALT, "F4"),
            true
        ));
        // Ctrl on Windows/Linux is the same key as Mod.
        assert!(KeyBindings::is_reserved(
            &Binding::key(
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                "Q"
            ),
            false
        ));
        assert!(!KeyBindings::is_reserved(&Binding::plain("Q"), true));
    }

    #[test]
    fn binding_string_round_trip() {
        for s in [
            "Right",
            "Alt+Right",
            "Mod+Shift+R",
            "Mod+Ctrl+Alt+Shift+F5",
            "char:L",
            "Shift+char:é",
            "Mouse4",
            "Mouse5",
            "ScrollLeft",
            "Alt+ScrollRight",
        ] {
            let b: Binding = s.parse().unwrap();
            assert_eq!(b.to_string(), s, "round trip of {s}");
        }
        assert_eq!(
            "cmd+shift+O".parse::<Binding>().unwrap(),
            Binding::key(Mods::COMMAND_SHIFT, "O")
        );
        assert!("".parse::<Binding>().is_err());
        assert!("Mod+".parse::<Binding>().is_err());
        assert!("char:".parse::<Binding>().is_err());
    }

    #[test]
    fn physical_vs_character_bindings() {
        let mut kb = KeyBindings::defaults(false);
        kb.add(
            Action::NextImage,
            Binding {
                mods: Mods::NONE,
                trigger: Trigger::Char("ö".into()),
            },
            false,
        )
        .unwrap();
        // AZERTY: the physical "Q" position produces "A".
        let azerty_a = Chord::Key {
            mods: Mods::NONE,
            physical: Some("Q".into()),
            logical: "A".into(),
        };
        kb.add(Action::PrevImage, Binding::plain("Q"), false)
            .unwrap();
        assert_eq!(kb.resolve(&azerty_a), Some(Action::PrevImage));
        let o_umlaut = Chord::Key {
            mods: Mods::NONE,
            physical: Some("Semicolon".into()),
            logical: "Ö".into(),
        };
        assert_eq!(kb.resolve(&o_umlaut), None); // non-ASCII case is not folded…
        let o_umlaut = Chord::Key {
            mods: Mods::NONE,
            physical: Some("Semicolon".into()),
            logical: "ö".into(),
        };
        assert_eq!(kb.resolve(&o_umlaut), Some(Action::NextImage));
        // Without a physical key the logical one is used for position bindings.
        let no_physical = Chord::Key {
            mods: Mods::NONE,
            physical: None,
            logical: "Q".into(),
        };
        assert_eq!(kb.resolve(&no_physical), Some(Action::PrevImage));
    }

    #[test]
    fn pointer_triggers() {
        let mut kb = KeyBindings::defaults(false);
        let back = Chord::Pointer {
            mods: Mods::NONE,
            trigger: Trigger::MouseBack,
        };
        assert_eq!(kb.resolve(&back), Some(Action::HistoryBack));
        kb.reassign(
            Action::PrevImage,
            Binding {
                mods: Mods::NONE,
                trigger: Trigger::MouseBack,
            },
            false,
        )
        .unwrap();
        assert_eq!(kb.resolve(&back), Some(Action::PrevImage));
        kb.add(
            Action::NextImage,
            Binding {
                mods: Mods::NONE,
                trigger: Trigger::ScrollRight,
            },
            false,
        )
        .unwrap();
        let swipe = Chord::Pointer {
            mods: Mods::NONE,
            trigger: Trigger::ScrollRight,
        };
        assert_eq!(kb.resolve(&swipe), Some(Action::NextImage));
    }

    #[test]
    fn persistence_round_trip() {
        let mut kb = KeyBindings::defaults(true);
        kb.add(Action::NextImage, Binding::plain("L"), true)
            .unwrap();
        kb.add(Action::PrevImage, Binding::plain("J"), true)
            .unwrap();
        kb.reassign(Action::NextFolder, Binding::plain("T"), true)
            .unwrap();
        let text = kb.export_toml();
        let (loaded, warnings) = KeyBindings::import_toml(&text, true).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(loaded, kb);
        // An action explicitly left without bindings stays unbound after reload.
        assert!(loaded.bindings(Action::ToggleThumbnails).is_empty());
    }

    #[test]
    fn loading_is_lenient() {
        let mut table = BTreeMap::new();
        table.insert(
            "next_image".to_owned(),
            vec!["L".to_owned(), "Mod+Q".to_owned(), "".to_owned()],
        );
        table.insert(
            "prev_image".to_owned(),
            vec!["L".to_owned(), "J".to_owned()],
        );
        table.insert("warp_drive".to_owned(), vec!["W".to_owned()]);
        table.insert("crop".to_owned(), vec!["T".to_owned()]);
        let (kb, warnings) = KeyBindings::from_table(&table, true);
        assert_eq!(warnings.len(), 4, "{warnings:?}");
        assert_eq!(kb.bindings(Action::NextImage), [Binding::plain("L")]);
        assert_eq!(kb.bindings(Action::PrevImage), [Binding::plain("J")]);
        // Missing actions fall back to defaults…
        assert_eq!(kb.bindings(Action::LastImage), [Binding::plain("End")]);
        // …except bindings the file gave to something else.
        assert_eq!(kb.bindings(Action::Crop), [Binding::plain("T")]);
        assert!(kb.bindings(Action::ToggleThumbnails).is_empty());
        assert!(KeyBindings::import_toml("not toml [", true).is_err());
    }

    #[test]
    fn ctrl_is_normalised_off_mac() {
        let mut table = BTreeMap::new();
        table.insert("next_image".to_owned(), vec!["Ctrl+L".to_owned()]);
        let (kb, _) = KeyBindings::from_table(&table, false);
        assert_eq!(
            kb.resolve(&key(Mods::COMMAND, "L")),
            Some(Action::NextImage)
        );
        let (kb, _) = KeyBindings::from_table(&table, true);
        assert_eq!(
            kb.resolve(&key(
                Mods {
                    ctrl: true,
                    ..Mods::NONE
                },
                "L"
            )),
            Some(Action::NextImage)
        );
    }

    #[test]
    fn restore_navigation_defaults_only_touches_navigation() {
        let mut kb = KeyBindings::defaults(true);
        kb.remove(Action::NextImage, &Binding::plain("Right"));
        kb.reassign(Action::Crop, Binding::plain("Right"), true)
            .unwrap();
        kb.add(Action::Crop, Binding::plain("X"), true).unwrap();
        kb.restore_navigation_defaults(true);
        assert_eq!(
            kb.bindings(Action::NextImage),
            KeyBindings::defaults(true).bindings(Action::NextImage)
        );
        assert_eq!(
            kb.bindings(Action::Crop),
            [Binding::plain("C"), Binding::plain("X")]
        );
    }

    #[test]
    fn action_names_round_trip() {
        for a in Action::ALL {
            assert_eq!(Action::from_name(a.name()), Some(*a));
        }
        assert!(Action::NextImage.is_navigation() && !Action::Crop.is_navigation());
    }
}
