//! The thumbnail strip: virtualised, grouped, lazily filled.

use crate::app::{FlickApp, Record};
use crate::router::RouterOutput;
use eframe::egui::{self, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use ff_core::config::StripPosition;
use std::path::PathBuf;

const GAP: f32 = 4.0;
const GROUP_GAP: f32 = 14.0;
const HEADER_HEIGHT: f32 = 15.0;
pub(crate) const MIN_THUMB: f32 = 32.0;
pub(crate) const MAX_THUMB: f32 = 256.0;

impl FlickApp {
    pub(crate) fn ui_strip(&mut self, ui: &mut egui::Ui, out: &RouterOutput) {
        let size = self
            .settings
            .general
            .thumbnail_size
            .clamp(MIN_THUMB, MAX_THUMB);
        let grouped = !self.index.groups.is_empty();
        let header = if grouped { HEADER_HEIGHT } else { 0.0 };
        let height = size + header + 2.0 * GAP + 10.0;
        let panel = match self.settings.general.strip_position {
            StripPosition::Top => egui::Panel::top("ff-strip"),
            StripPosition::Bottom => egui::Panel::bottom("ff-strip"),
        };
        let frame = egui::Frame::NONE.fill(Color32::from_gray(22));
        panel
            .exact_size(height)
            .resizable(false)
            .show_separator_line(false)
            .frame(frame)
            .show(ui, |ui| {
                let panel_rect = ui.max_rect();
                let pointer = ui.ctx().pointer_latest_pos();
                self.over_strip = pointer.map(|p| panel_rect.contains(p)).unwrap_or(false);

                // Mod+scroll over the strip resizes thumbnails.
                if self.over_strip {
                    let resize: f32 = out
                        .wheel
                        .iter()
                        .filter(|w| w.modifiers.command)
                        .map(|w| w.delta.y + w.delta.x)
                        .sum();
                    if resize != 0.0 {
                        let step = if resize.abs() < 3.0 {
                            resize * 8.0
                        } else {
                            resize * 0.5
                        };
                        self.settings.general.thumbnail_size =
                            (size + step).clamp(MIN_THUMB, MAX_THUMB);
                        self.mark_settings_dirty();
                    }
                }

                let cell = size + GAP;
                let count = self.index.len();
                // Section boundaries: (first index, end index, title).
                let sections: Vec<(usize, usize, String)> = if grouped {
                    self.index
                        .groups
                        .iter()
                        .map(|g| (g.start, g.end, g.title()))
                        .collect()
                } else {
                    vec![(0, count, String::new())]
                };
                let total =
                    count as f32 * cell + sections.len().saturating_sub(1) as f32 * GROUP_GAP + GAP;
                let space_down = ui.ctx().input(|i| i.key_down(egui::Key::Space));
                let mut clicked: Option<PathBuf> = None;
                let mut peek: Option<PathBuf> = None;
                let scroll_to = std::mem::take(&mut self.strip_scroll_to);

                egui::ScrollArea::horizontal()
                    .auto_shrink(false)
                    .show_viewport(ui, |ui, viewport| {
                        let (content, _) = ui.allocate_exact_size(
                            Vec2::new(total.max(viewport.width()), size + header + 2.0 * GAP),
                            Sense::hover(),
                        );
                        let origin = content.min;
                        let painter = ui.painter().clone();
                        for (k, (start, end, title)) in sections.iter().enumerate() {
                            let x0 = GAP + *start as f32 * cell + k as f32 * GROUP_GAP;
                            let x1 = x0 + (*end - *start) as f32 * cell;
                            if x1 < viewport.left() - cell || x0 > viewport.right() + cell {
                                // Still honour scroll-to-current for sections that are off screen.
                                if scroll_to && (*start..*end).contains(&self.current_idx) {
                                    let x = x0 + (self.current_idx - *start) as f32 * cell;
                                    ui.scroll_to_rect(
                                        Rect::from_min_size(
                                            origin + Vec2::new(x, 0.0),
                                            Vec2::splat(size),
                                        ),
                                        Some(egui::Align::Center),
                                    );
                                }
                                continue;
                            }
                            if grouped {
                                // The title sticks to the left edge while its section is visible.
                                let tx = x0.max(viewport.left() + GAP).min(x1 - 20.0);
                                painter.text(
                                    origin + Vec2::new(tx, 1.0),
                                    egui::Align2::LEFT_TOP,
                                    format!("{title}  ({})", end - start),
                                    FontId::proportional(11.0),
                                    Color32::from_gray(170),
                                );
                            }
                            let first =
                                start + ((viewport.left() - x0) / cell).floor().max(0.0) as usize;
                            let last = (start
                                + ((viewport.right() - x0) / cell).ceil().max(0.0) as usize
                                + 1)
                            .min(*end);
                            if scroll_to && (*start..*end).contains(&self.current_idx) {
                                let x = x0 + (self.current_idx - *start) as f32 * cell;
                                ui.scroll_to_rect(
                                    Rect::from_min_size(
                                        origin + Vec2::new(x, 0.0),
                                        Vec2::splat(size),
                                    ),
                                    Some(egui::Align::Center),
                                );
                            }
                            for i in first..last {
                                let Some(path) = self.index.path(i).map(PathBuf::from) else {
                                    continue;
                                };
                                let x = x0 + (i - *start) as f32 * cell;
                                let rect = Rect::from_min_size(
                                    origin + Vec2::new(x, header + GAP),
                                    Vec2::splat(size),
                                );
                                let response = ui.interact(
                                    rect,
                                    egui::Id::unique(("ff-thumb", i)),
                                    Sense::click(),
                                );
                                painter.rect_filled(rect, 3.0, Color32::from_gray(38));
                                match self.thumb(&path) {
                                    Some(tex) => {
                                        // Centre-crop to a square.
                                        let t = tex.size_vec2();
                                        let (u, v) = if t.x > t.y {
                                            ((1.0 - t.y / t.x) / 2.0, 0.0)
                                        } else {
                                            (0.0, (1.0 - t.x / t.y) / 2.0)
                                        };
                                        let uv = Rect::from_min_max(
                                            Pos2::new(u, v),
                                            Pos2::new(1.0 - u, 1.0 - v),
                                        );
                                        painter.image(tex.id(), rect, uv, Color32::WHITE);
                                    }
                                    None if self.thumb_failed.contains(&path) => {
                                        painter.text(
                                            rect.center(),
                                            egui::Align2::CENTER_CENTER,
                                            "⚠",
                                            FontId::proportional(size * 0.3),
                                            Color32::from_gray(120),
                                        );
                                    }
                                    None => {}
                                }
                                let is_current = i == self.current_idx && self.current.is_some();
                                if is_current {
                                    painter.rect_stroke(
                                        rect,
                                        3.0,
                                        Stroke::new(2.5, Color32::from_rgb(90, 170, 255)),
                                        StrokeKind::Outside,
                                    );
                                } else if response.hovered() {
                                    painter.rect_stroke(
                                        rect,
                                        3.0,
                                        Stroke::new(1.0, Color32::from_gray(200)),
                                        StrokeKind::Outside,
                                    );
                                }
                                if response.hovered() && space_down {
                                    peek = Some(path.clone());
                                }
                                if response.clicked() {
                                    clicked = Some(path.clone());
                                }
                                response.on_hover_text(
                                    path.file_name().unwrap_or_default().to_string_lossy(),
                                );
                            }
                        }
                    });

                // Peek: hold Space over a thumbnail to look at it; releasing returns.
                if peek.as_ref() == self.current.as_ref() {
                    peek = None;
                }
                if peek != self.peek {
                    self.transition = None;
                    self.peek = peek;
                }
                if let Some(path) = clicked {
                    if !self.guard_unsaved() {
                        self.peek = None;
                        self.set_current(path, Record::Jump);
                    }
                }
            });
    }
}
