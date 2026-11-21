//! The auto-hiding HUD: file info at the top, controls at the bottom, and
//! the menu shared with the context menu.

use crate::app::{FlickApp, Modal, HUD_TIMEOUT};
use crate::ui_canvas::{hud_frame, zoom_label};
use eframe::egui::{self, Align2, Color32, Pos2, RichText, Vec2};
use ff_core::anim::{MAX_SPEED, MIN_SPEED};
use ff_core::folder::{GroupKey, SortKey};
use ff_core::input::Action;
use ff_core::IS_MAC;

const FADE_SECONDS: f32 = 0.18;

impl FlickApp {
    fn shortcut(&self, action: Action) -> String {
        self.settings
            .bindings
            .bindings(action)
            .first()
            .map(|b| b.display(IS_MAC))
            .unwrap_or_default()
    }

    /// A menu entry that performs `action`, showing its shortcut.
    fn action_item(&mut self, ui: &mut egui::Ui, action: Action, enabled: bool) {
        let button = egui::Button::new(action.label()).shortcut_text(self.shortcut(action));
        if ui.add_enabled(enabled, button).clicked() {
            ui.close();
            self.perform(action);
        }
    }

    /// Contents of the ☰ menu and of the canvas context menu.
    pub(crate) fn menu_contents(&mut self, ui: &mut egui::Ui) {
        let has_image = self.current.is_some();
        let (dirty, can_undo, can_redo) = self
            .edit
            .as_ref()
            .map(|e| (e.dirty, e.can_undo, e.can_redo))
            .unwrap_or((false, false, false));
        self.action_item(ui, Action::OpenFile, true);
        self.action_item(ui, Action::OpenFolder, true);
        ui.separator();
        ui.menu_button("Sort by", |ui| {
            let mut changed = false;
            for key in SortKey::ALL {
                changed |= ui
                    .radio_value(&mut self.settings.sorting.key, key, key.label())
                    .clicked();
            }
            ui.separator();
            changed |= ui
                .checkbox(&mut self.settings.sorting.descending, "Descending")
                .clicked();
            changed |= ui
                .checkbox(&mut self.settings.sorting.recursive, "Include subfolders")
                .clicked();
            if changed {
                self.resort();
            }
        });
        ui.menu_button("Group by", |ui| {
            let mut changed = false;
            if ui
                .radio(self.settings.sorting.group_by.is_empty(), "None")
                .clicked()
            {
                self.settings.sorting.group_by.clear();
                changed = true;
            }
            // Clicking toggles a level; the order of clicks is the nesting order.
            for key in GroupKey::ALL {
                let level = self
                    .settings
                    .sorting
                    .group_by
                    .iter()
                    .position(|g| *g == key);
                let label = match level {
                    Some(n) => format!("{}  (level {})", key.label(), n + 1),
                    None => key.label().to_owned(),
                };
                if ui.selectable_label(level.is_some(), label).clicked() {
                    match level {
                        Some(n) => {
                            self.settings.sorting.group_by.remove(n);
                        }
                        None => self.settings.sorting.group_by.push(key),
                    }
                    changed = true;
                }
            }
            if changed {
                self.resort();
            }
        });
        ui.separator();
        ui.menu_button("Edit", |ui| {
            self.action_item(ui, Action::RotateLeft, has_image);
            self.action_item(ui, Action::RotateRight, has_image);
            self.action_item(ui, Action::FlipHorizontal, has_image);
            self.action_item(ui, Action::FlipVertical, has_image);
            self.action_item(ui, Action::Crop, has_image);
            if ui
                .add_enabled(has_image, egui::Button::new("Extend canvas…"))
                .clicked()
            {
                ui.close();
                self.modal = Some(Modal::Extend {
                    margins: [50; 4],
                    color: [255, 255, 255],
                });
            }
            ui.separator();
            self.action_item(ui, Action::Undo, can_undo);
            self.action_item(ui, Action::Redo, can_redo);
            ui.separator();
            self.action_item(ui, Action::Save, dirty);
            self.action_item(ui, Action::SaveAs, has_image);
        });
        ui.menu_button("File", |ui| {
            self.action_item(ui, Action::Rename, has_image);
            self.action_item(ui, Action::Trash, has_image);
            self.action_item(ui, Action::Reveal, has_image);
            self.action_item(ui, Action::OpenWith, has_image);
            ui.separator();
            self.action_item(ui, Action::CopyImage, has_image);
            self.action_item(ui, Action::CopyPath, has_image);
        });
        ui.menu_button("View", |ui| {
            self.action_item(ui, Action::ZoomFit, has_image);
            self.action_item(ui, Action::ZoomActual, has_image);
            self.action_item(ui, Action::ZoomIn, has_image);
            self.action_item(ui, Action::ZoomOut, has_image);
            ui.separator();
            self.action_item(ui, Action::ToggleThumbnails, true);
            self.action_item(ui, Action::ToggleInfo, true);
            self.action_item(ui, Action::ToggleFullscreen, true);
        });
        ui.menu_button("Go", |ui| {
            self.action_item(ui, Action::PrevImage, has_image);
            self.action_item(ui, Action::NextImage, has_image);
            self.action_item(ui, Action::FirstImage, has_image);
            self.action_item(ui, Action::LastImage, has_image);
            ui.separator();
            self.action_item(ui, Action::PrevFolder, true);
            self.action_item(ui, Action::NextFolder, true);
            ui.separator();
            let (back, forward) = (self.history.can_go_back(), self.history.can_go_forward());
            self.action_item(ui, Action::HistoryBack, back);
            self.action_item(ui, Action::HistoryForward, forward);
        });
        ui.separator();
        self.action_item(ui, Action::Slideshow, has_image);
        self.action_item(ui, Action::Settings, true);
    }

    pub(crate) fn ui_hud(&mut self, ctx: &egui::Context, visible: bool) {
        // The HUD stays out of the way while cropping or running a slideshow.
        let visible = visible && self.crop.is_none();
        let opacity =
            ctx.animate_bool_with_time(egui::Id::unique("ff-hud-fade"), visible, FADE_SECONDS);
        if opacity <= 0.0 {
            self.hud_hovered = false;
            return;
        }
        let canvas = self.canvas_rect;
        let pointer = ctx.pointer_latest_pos();
        let mut hovered = false;

        // ---- Top: menu, file name, position, dimensions, zoom ----
        // On macOS the window buttons float over the top-left corner of the content.
        let inset = if IS_MAC && !self.fullscreen && canvas.top() < 1.0 {
            78.0
        } else {
            10.0
        };
        let top = egui::Area::new(egui::Id::unique("ff-hud-top"))
            .fixed_pos(Pos2::new(canvas.left() + inset, canvas.top() + 8.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.style_mut().visuals = egui::Visuals::dark();
                ui.set_opacity(opacity);
                ui.set_max_width((canvas.width() - inset - 10.0).max(120.0));
                hud_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.menu_button(RichText::new("☰").size(16.0), |ui| {
                            self.menu_contents(ui)
                        });
                        let back = egui::Button::new("⏴").frame(false);
                        if ui
                            .add_enabled(self.history.can_go_back(), back)
                            .on_hover_text("Back")
                            .clicked()
                        {
                            self.perform(Action::HistoryBack);
                        }
                        let Some(path) = self.current.clone() else {
                            ui.label(RichText::new("FlickFlock").color(Color32::WHITE));
                            return;
                        };
                        let name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let dirty = self.edit.as_ref().map(|e| e.dirty).unwrap_or(false);
                        ui.label(
                            RichText::new(if dirty { format!("{name} •") } else { name })
                                .color(Color32::WHITE)
                                .strong(),
                        )
                        .on_hover_text(path.to_string_lossy());
                        let dim = Color32::from_gray(175);
                        ui.label(
                            RichText::new(format!(
                                "{} / {}",
                                self.current_idx + 1,
                                self.index.len()
                            ))
                            .color(dim)
                            .monospace(),
                        );
                        if let Some(group) = self.index.group_of(self.current_idx) {
                            ui.label(RichText::new(group.title()).color(dim));
                        }
                        if let Some(size) = self.image_size(&path) {
                            ui.label(
                                RichText::new(format!("{} × {}", size[0] as u32, size[1] as u32))
                                    .color(dim),
                            );
                        }
                        let zoom = zoom_label(&self.view, self.zoom_percent());
                        if !zoom.is_empty() {
                            ui.label(RichText::new(zoom).color(dim));
                        }
                        if self.slideshow.is_some() {
                            ui.label(
                                RichText::new("▶ Slideshow")
                                    .color(Color32::from_rgb(120, 200, 255)),
                            );
                        }
                    });
                });
            });
        hovered |= pointer
            .map(|p| top.response.rect.contains(p))
            .unwrap_or(false);

        // ---- Bottom: navigation and toggles, animation controls ----
        let bottom = egui::Area::new(egui::Id::unique("ff-hud-bottom"))
            .fixed_pos(Pos2::new(canvas.center().x, canvas.bottom() - 10.0))
            .pivot(Align2::CENTER_BOTTOM)
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.style_mut().visuals = egui::Visuals::dark();
                ui.set_opacity(opacity);
                ui.vertical_centered(|ui| {
                    if self.anim.is_some() {
                        hud_frame().show(ui, |ui| self.animation_controls(ui));
                        ui.add_space(6.0);
                    }
                    hud_frame().show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
                            let has_image = self.current.is_some();
                            self.hud_button(ui, "⏴", Action::PrevImage, has_image, false);
                            self.hud_button(ui, "⏵", Action::NextImage, has_image, false);
                            ui.separator();
                            self.hud_button(
                                ui,
                                "▦",
                                Action::ToggleThumbnails,
                                true,
                                self.settings.general.show_thumbnails,
                            );
                            self.hud_button(
                                ui,
                                "ℹ",
                                Action::ToggleInfo,
                                true,
                                self.settings.general.show_info,
                            );
                            self.hud_button(
                                ui,
                                "▶",
                                Action::Slideshow,
                                has_image,
                                self.slideshow.is_some(),
                            );
                            self.hud_button(
                                ui,
                                "⛶",
                                Action::ToggleFullscreen,
                                true,
                                self.fullscreen,
                            );
                            self.hud_button(ui, "⚙", Action::Settings, true, self.show_settings);
                        });
                    });
                });
            });
        hovered |= pointer
            .map(|p| bottom.response.rect.contains(p))
            .unwrap_or(false);

        self.hud_hovered = hovered;
        if opacity < 1.0 || (visible && !hovered) {
            ctx.request_repaint_after(HUD_TIMEOUT / 6);
        }
    }

    fn hud_button(
        &mut self,
        ui: &mut egui::Ui,
        icon: &str,
        action: Action,
        enabled: bool,
        active: bool,
    ) {
        let color = if active {
            Color32::from_rgb(120, 200, 255)
        } else {
            Color32::from_gray(230)
        };
        let button = egui::Button::new(RichText::new(icon).size(17.0).color(color))
            .frame(false)
            .min_size(Vec2::splat(26.0));
        let shortcut = self.shortcut(action);
        let tip = if shortcut.is_empty() {
            action.label().to_owned()
        } else {
            format!("{}  ({shortcut})", action.label())
        };
        if ui.add_enabled(enabled, button).on_hover_text(tip).clicked() {
            self.perform(action);
        }
    }

    /// Play/pause, frame stepping, timeline scrubber, reverse, speed and loop.
    fn animation_controls(&mut self, ui: &mut egui::Ui) {
        let Some(a) = self.anim.as_mut() else { return };
        let count = a.anim.frame_count();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
            let white = Color32::from_gray(235);
            let on = Color32::from_rgb(120, 200, 255);
            let icon = |text: &str, color: Color32| {
                egui::Button::new(RichText::new(text).size(15.0).color(color)).frame(false)
            };
            if ui
                .add(icon(if a.player.playing { "⏸" } else { "▶" }, white))
                .on_hover_text("Play / pause")
                .clicked()
            {
                a.player.playing = !a.player.playing;
                a.last_tick = std::time::Instant::now();
            }
            if ui
                .add(icon("⏮", white))
                .on_hover_text("Previous frame")
                .clicked()
            {
                a.player.step(-1, count);
            }
            if ui
                .add(icon("⏭", white))
                .on_hover_text("Next frame")
                .clicked()
            {
                a.player.step(1, count);
            }
            // Timeline scrubber: frame-accurate seeking.
            let mut frame = a.player.frame;
            ui.spacing_mut().slider_width = 220.0;
            let slider =
                egui::Slider::new(&mut frame, 0..=count.saturating_sub(1)).show_value(false);
            if ui.add(slider).changed() {
                a.player.seek(frame, count);
            }
            ui.label(
                RichText::new(format!("{} / {}", a.player.frame + 1, count))
                    .monospace()
                    .color(white),
            );
            if ui
                .add(icon("⏪", if a.player.reverse { on } else { white }))
                .on_hover_text("Reverse")
                .clicked()
            {
                a.player.reverse = !a.player.reverse;
            }
            let mut speed = a.player.speed;
            egui::ComboBox::from_id_salt("ff-anim-speed")
                .selected_text(format!("{speed}×"))
                .width(58.0)
                .show_ui(ui, |ui| {
                    for s in [0.25, 0.5, 1.0, 1.5, 2.0, 3.0, 4.0] {
                        if (MIN_SPEED..=MAX_SPEED).contains(&s) {
                            ui.selectable_value(&mut speed, s, format!("{s}×"));
                        }
                    }
                });
            a.player.set_speed(speed);
            if ui
                .add(icon("🔁", if a.player.looping { on } else { white }))
                .on_hover_text("Loop")
                .clicked()
            {
                a.player.looping = !a.player.looping;
                if a.player.looping {
                    a.player.playing = true;
                }
            }
        });
    }
}
