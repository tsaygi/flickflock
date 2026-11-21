//! The input router: turns raw egui input into [`Action`]s through the
//! user's key bindings. No other UI code looks at keys directly.

use eframe::egui;
use ff_core::config::Keys;
use ff_core::input::{Action, Binding, Chord, KeyBindings, Mods, Trigger};
use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Delay before a held key starts repeating.
const REPEAT_DELAY: Duration = Duration::from_millis(350);
/// Horizontal scroll distance (points) that counts as one swipe.
const SWIPE_DISTANCE: f32 = 60.0;
const SWIPE_COOLDOWN: Duration = Duration::from_millis(350);

pub fn mods_from(m: egui::Modifiers) -> Mods {
    Mods {
        command: m.command,
        alt: m.alt,
        shift: m.shift,
        ctrl: ff_core::IS_MAC && m.ctrl,
    }
}

fn is_modifier_key(key: egui::Key) -> bool {
    use egui::Key::*;
    matches!(
        key,
        ShiftLeft
            | ShiftRight
            | ControlLeft
            | ControlRight
            | AltLeft
            | AltRight
            | SuperLeft
            | SuperRight
            | CapsLock
    )
}

struct Held {
    action: Action,
    since: Instant,
    last_fire: Instant,
}

/// One wheel / trackpad scroll event that was not consumed by a binding.
#[derive(Debug, Clone, Copy)]
pub struct Wheel {
    pub unit: egui::MouseWheelUnit,
    pub delta: egui::Vec2,
    pub modifiers: egui::Modifiers,
}

#[derive(Default)]
pub struct RouterOutput {
    /// Actions triggered this frame, in order.
    pub actions: Vec<Action>,
    /// A binding captured by the key recorder.
    pub recorded: Option<Binding>,
    /// The recorder was cancelled with Escape.
    pub record_cancelled: bool,
    /// A key bound to Skim is being held.
    pub skim_held: bool,
    /// Scroll events left for the canvas (zoom / pan / navigate).
    pub wheel: Vec<Wheel>,
    /// Pinch-zoom factor (1.0 = none).
    pub pinch: f32,
}

#[derive(Default)]
pub struct RouterOptions {
    /// A text field has focus: letter keys must not trigger actions.
    pub typing: bool,
    /// Capture the next key press as a binding instead of acting on it.
    pub recording: bool,
    /// Record by character instead of key position.
    pub record_by_character: bool,
    /// The pointer is over the thumbnail strip, where Space means Peek.
    pub space_is_peek: bool,
}

#[derive(Default)]
pub struct InputRouter {
    held: HashMap<egui::Key, Held>,
    skim_key: Option<egui::Key>,
    swipe: f32,
    swipe_block_until: Option<Instant>,
}

impl InputRouter {
    pub fn process(
        &mut self,
        ctx: &egui::Context,
        bindings: &KeyBindings,
        keys: &Keys,
        opts: &RouterOptions,
    ) -> RouterOutput {
        let mut out = RouterOutput {
            pinch: 1.0,
            ..Default::default()
        };
        let now = Instant::now();
        let events = ctx.input(|i| i.events.clone());
        for event in &events {
            match event {
                egui::Event::Key {
                    key,
                    physical_key,
                    pressed: true,
                    repeat,
                    modifiers,
                } => {
                    if is_modifier_key(*key) {
                        continue;
                    }
                    let mods = mods_from(*modifiers);
                    if opts.recording {
                        if *key == egui::Key::Escape && mods.is_none() {
                            out.record_cancelled = true;
                        } else if !*repeat {
                            let trigger = if opts.record_by_character {
                                Trigger::Char(key.name().to_owned())
                            } else {
                                Trigger::Key(physical_key.unwrap_or(*key).name().to_owned())
                            };
                            out.recorded = Some(Binding { mods, trigger });
                        }
                        continue;
                    }
                    // OS key repeat is ignored; repeating is done below at the configured rate.
                    if *repeat {
                        continue;
                    }
                    if opts.space_is_peek && *key == egui::Key::Space {
                        continue;
                    }
                    let chord = Chord::Key {
                        mods,
                        physical: physical_key.map(|k| k.name().to_owned()),
                        logical: key.name().to_owned(),
                    };
                    let Some(action) = bindings.resolve(&chord) else {
                        continue;
                    };
                    // While typing only shortcuts with Mod/Ctrl (and Escape) get through.
                    if opts.typing && !(mods.command || mods.ctrl || action == Action::Escape) {
                        continue;
                    }
                    if action == Action::Skim {
                        self.skim_key = Some(*key);
                        continue;
                    }
                    out.actions.push(action);
                    if action.repeats() && keys.key_repeat {
                        self.held.insert(
                            *key,
                            Held {
                                action,
                                since: now,
                                last_fire: now,
                            },
                        );
                    }
                }
                egui::Event::PointerButton {
                    button,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    let trigger = match button {
                        egui::PointerButton::Extra1 => Trigger::MouseBack,
                        egui::PointerButton::Extra2 => Trigger::MouseForward,
                        _ => continue,
                    };
                    let mods = mods_from(*modifiers);
                    if opts.recording {
                        out.recorded = Some(Binding { mods, trigger });
                    } else if let Some(action) = bindings.resolve(&Chord::Pointer { mods, trigger })
                    {
                        out.actions.push(action);
                    }
                }
                egui::Event::MouseWheel {
                    unit,
                    delta,
                    modifiers,
                    ..
                } => {
                    let mods = mods_from(*modifiers);
                    let horizontal = delta.x.abs() > delta.y.abs() * 1.5;
                    let swipe_bound =
                        |t: Trigger| bindings.resolve(&Chord::Pointer { mods, trigger: t });
                    let any_swipe_binding = swipe_bound(Trigger::ScrollLeft).is_some()
                        || swipe_bound(Trigger::ScrollRight).is_some();
                    if horizontal && (opts.recording || any_swipe_binding) {
                        let step = match unit {
                            egui::MouseWheelUnit::Point => delta.x,
                            _ => delta.x * 40.0,
                        };
                        if self.swipe_block_until.map(|t| now < t).unwrap_or(false) {
                            continue;
                        }
                        self.swipe += step;
                        if self.swipe.abs() >= SWIPE_DISTANCE {
                            // Content moving right (positive delta) = swipe towards the left edge's content, i.e. "back".
                            let trigger = if self.swipe > 0.0 {
                                Trigger::ScrollLeft
                            } else {
                                Trigger::ScrollRight
                            };
                            self.swipe = 0.0;
                            self.swipe_block_until = Some(now + SWIPE_COOLDOWN);
                            if opts.recording {
                                out.recorded = Some(Binding { mods, trigger });
                            } else if let Some(action) = swipe_bound(trigger) {
                                out.actions.push(action);
                            }
                        }
                        continue;
                    }
                    out.wheel.push(Wheel {
                        unit: *unit,
                        delta: *delta,
                        modifiers: *modifiers,
                    });
                }
                egui::Event::Zoom(factor) => out.pinch *= *factor,
                _ => {}
            }
        }

        // Key repeat at the configured rate while navigation keys are held.
        let interval = Duration::from_secs_f32(1.0 / keys.repeat_rate.clamp(1.0, 60.0));
        let down = ctx.input(|i| i.keys_down.clone());
        self.held.retain(|key, _| down.contains(key));
        if opts.typing || opts.recording || !keys.key_repeat {
            self.held.clear();
        }
        for held in self.held.values_mut() {
            if now.duration_since(held.since) >= REPEAT_DELAY
                && now.duration_since(held.last_fire) >= interval
            {
                held.last_fire = now;
                out.actions.push(held.action);
            }
        }
        if !self.held.is_empty() {
            ctx.request_repaint_after(interval.min(Duration::from_millis(16)));
        }

        if let Some(key) = self.skim_key {
            if down.contains(&key) && !opts.typing {
                out.skim_held = true;
            } else {
                self.skim_key = None;
            }
        }
        out
    }
}

/// Convert a recorded/loaded key name into an egui key (used to validate
/// that binding names are understood by the toolkit).
#[cfg(test)]
pub fn key_from_name(name: &str) -> Option<egui::Key> {
    egui::Key::from_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_binding_uses_a_key_name_egui_understands() {
        for is_mac in [true, false] {
            let kb = KeyBindings::defaults(is_mac);
            for action in Action::ALL {
                for b in kb.bindings(*action) {
                    if let Trigger::Key(name) | Trigger::Char(name) = &b.trigger {
                        let key = key_from_name(name)
                            .unwrap_or_else(|| panic!("{name:?} ({action:?}) is not an egui key"));
                        // …and the name round-trips, so events match bindings.
                        assert_eq!(key.name(), name, "{action:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn modifiers_map_per_platform() {
        let m = egui::Modifiers {
            alt: true,
            ctrl: true,
            shift: false,
            mac_cmd: false,
            command: !ff_core::IS_MAC,
        };
        let mods = mods_from(m);
        assert!(mods.alt && !mods.shift);
        assert_eq!(mods.ctrl, ff_core::IS_MAC);
        assert_eq!(mods.command, !ff_core::IS_MAC);
    }
}
