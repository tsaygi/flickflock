//! The central image area: pan/zoom/skim interaction, placeholders, the crop
//! overlay, pinned metadata and toasts.

use crate::app::{CropHandle, FlickApp};
use crate::canvas::{CanvasCallback, View, Zoom};
use crate::router::RouterOutput;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use eframe::egui_wgpu;
use ff_core::config::ScrollMode;
use ff_core::edit::ASPECT_PRESETS;
use ff_core::input::Action;
use std::path::Path;
use std::time::{Duration, Instant};

/// Delay before a loading spinner appears (prefetched images never show one).
const SPINNER_DELAY: Duration = Duration::from_millis(250);
/// Trackpad scroll distance (points) per image when scrolling navigates.
const SCROLL_NAV_DISTANCE: f32 = 50.0;

impl FlickApp {
    pub(crate) fn ui_canvas(&mut self, ui: &mut egui::Ui, out: &RouterOutput) {
        let bg = self.background_color();
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(bg))
            .show(ui, |ui| {
                let rect = ui.max_rect();
                self.canvas_rect = rect;
                let ppp = ui.ctx().pixels_per_point();
                self.viewport_px = [
                    (rect.width() * ppp).max(1.0),
                    (rect.height() * ppp).max(1.0),
                ];
                let response = ui.allocate_rect(rect, Sense::click_and_drag());
                let pointer = response
                    .hover_pos()
                    .or_else(|| ui.ctx().pointer_latest_pos().filter(|p| rect.contains(*p)));
                let anchor = pointer.map(|p| [(p.x - rect.left()) * ppp, (p.y - rect.top()) * ppp]);

                if self.crop.is_some() {
                    self.crop_interaction(&response, rect, ppp);
                } else {
                    self.view_interaction(ui, &response, out, anchor, ppp);
                }

                // Skim: horizontal pointer position scrubs through the folder.
                if out.skim_held && self.modal.is_none() {
                    if let Some(p) = pointer {
                        self.skim_to((p.x - rect.left()) / rect.width().max(1.0));
                    }
                } else if self.skim.is_some() {
                    self.end_skim();
                }

                let scene = self.build_scene();
                let has_image = scene.layer.is_some();
                ui.painter().add(egui_wgpu::Callback::new_paint_callback(
                    rect,
                    CanvasCallback {
                        scene,
                        size_px: [self.viewport_px[0] as u32, self.viewport_px[1] as u32],
                    },
                ));

                if !has_image {
                    self.draw_placeholder(ui, rect);
                }
                if self.crop.is_some() {
                    self.draw_crop(ui, rect, ppp);
                }
                if self.edit.as_ref().map(|e| e.busy).unwrap_or(false) {
                    ui.put(
                        Rect::from_center_size(rect.center(), Vec2::splat(28.0)),
                        egui::Spinner::new(),
                    );
                }
                self.draw_pinned(ui, rect);
                response.context_menu(|ui| self.menu_contents(ui));
            });
    }

    fn view_interaction(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        out: &RouterOutput,
        anchor: Option<[f32; 2]>,
        ppp: f32,
    ) {
        if response.dragged_by(egui::PointerButton::Primary) {
            let d = response.drag_delta();
            self.view.pan[0] += d.x * ppp;
            self.view.pan[1] += d.y * ppp;
        }
        if response.double_clicked() {
            // Double-click resets a zoomed/panned view; on an untouched view it toggles full screen.
            if self.view.is_default() {
                self.set_fullscreen(!self.fullscreen);
            } else {
                self.view = View::default();
            }
        }
        if (out.pinch - 1.0).abs() > f32::EPSILON && anchor.is_some() {
            self.zoom_by(out.pinch, anchor);
        }
        if anchor.is_none()
            || self.over_strip
            || self.modal.is_some()
            || self.show_settings && ui.ctx().is_pointer_over_egui()
        {
            return;
        }
        for wheel in &out.wheel {
            let precise = wheel.unit == egui::MouseWheelUnit::Point;
            let zoom_mode = self.settings.general.scroll == ScrollMode::Zoom;
            if wheel.modifiers.command {
                // Mod+scroll always zooms.
                let k = if precise { 0.01 } else { 0.2 };
                self.zoom_by((wheel.delta.y * k).exp(), anchor);
            } else if precise && self.image_overflows() {
                // Trackpad scrolling pans an image that is larger than the window.
                self.view.pan[0] += wheel.delta.x * ppp;
                self.view.pan[1] += wheel.delta.y * ppp;
            } else if zoom_mode {
                let k = if precise { 0.01 } else { 0.2 };
                self.zoom_by((wheel.delta.y * k).exp(), anchor);
            } else {
                // Scroll navigates: one image per wheel notch, or per stretch of trackpad travel.
                let id = egui::Id::unique("ff-scroll-nav");
                let mut acc: f32 = ui.ctx().data(|d| d.get_temp(id).unwrap_or(0.0));
                acc += if precise {
                    wheel.delta.y
                } else {
                    wheel.delta.y * SCROLL_NAV_DISTANCE
                };
                while acc.abs() >= SCROLL_NAV_DISTANCE {
                    let action = if acc > 0.0 {
                        Action::PrevImage
                    } else {
                        Action::NextImage
                    };
                    acc -= SCROLL_NAV_DISTANCE * acc.signum();
                    self.perform(action);
                }
                ui.ctx().data_mut(|d| d.insert_temp(id, acc));
            }
        }
    }

    /// Is the image currently larger than the viewport in either dimension?
    fn image_overflows(&self) -> bool {
        let Some(size) = self.current.as_deref().and_then(|p| self.image_size(p)) else {
            return false;
        };
        let r = self.view.image_rect(
            size,
            self.viewport_px,
            self.settings.general.zoom_small_to_fit,
        );
        r[2] > self.viewport_px[0] + 1.0 || r[3] > self.viewport_px[1] + 1.0
    }

    /// Current zoom in percent, for the HUD.
    pub(crate) fn zoom_percent(&self) -> Option<f32> {
        let size = self.current.as_deref().and_then(|p| self.image_size(p))?;
        Some(
            self.view.scale(
                size,
                self.viewport_px,
                self.settings.general.zoom_small_to_fit,
            ) * 100.0,
        )
    }

    fn draw_placeholder(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let Some(path) = self.display_path().map(Path::to_path_buf) else {
            self.draw_welcome(ui, rect);
            return;
        };
        let text_color = if self.background_color().r() > 128 {
            Color32::from_gray(40)
        } else {
            Color32::from_gray(200)
        };
        if let Some(error) = self.errors.get(&path).cloned() {
            // An unreadable file never blocks browsing: show why and let the user flick on.
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let painter = ui.painter();
            painter.text(
                rect.center() - Vec2::new(0.0, 22.0),
                Align2::CENTER_CENTER,
                "⚠",
                FontId::proportional(34.0),
                text_color,
            );
            painter.text(
                rect.center() + Vec2::new(0.0, 14.0),
                Align2::CENTER_CENTER,
                name,
                FontId::proportional(15.0),
                text_color,
            );
            let galley = painter.layout(
                error,
                FontId::proportional(13.0),
                text_color.gamma_multiply(0.75),
                rect.width() * 0.8,
            );
            painter.galley(
                Pos2::new(
                    rect.center().x - galley.size().x / 2.0,
                    rect.center().y + 32.0,
                ),
                galley,
                text_color,
            );
            return;
        }
        // Low-resolution stand-in while the real image decodes.
        if let Some(tex) = self.thumb(&path) {
            let size = self.image_size(&path).or_else(|| {
                self.index
                    .entries
                    .get(self.current_idx)
                    .filter(|e| e.path == path)
                    .and_then(|e| e.dimensions)
                    .map(|(w, h)| [w as f32, h as f32])
            });
            let ppp = ui.ctx().pixels_per_point();
            let target = match size {
                Some(size) => {
                    let r = View::default().image_rect(
                        size,
                        self.viewport_px,
                        self.settings.general.zoom_small_to_fit,
                    );
                    Rect::from_min_size(
                        rect.min + Vec2::new(r[0], r[1]) / ppp,
                        Vec2::new(r[2], r[3]) / ppp,
                    )
                }
                None => {
                    let t = tex.size_vec2();
                    let s = (rect.width() / t.x).min(rect.height() / t.y);
                    Rect::from_center_size(rect.center(), t * s)
                }
            };
            ui.painter().image(
                tex.id(),
                target,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let waited = self.current_since.elapsed();
        if waited >= SPINNER_DELAY {
            ui.put(
                Rect::from_center_size(rect.center(), Vec2::splat(28.0)),
                egui::Spinner::new(),
            );
        } else {
            ui.ctx().request_repaint_after(SPINNER_DELAY - waited);
        }
    }

    fn draw_welcome(&mut self, ui: &mut egui::Ui, rect: Rect) {
        let scanning = self.pending_scan.is_some() || self.pending_jump.is_some();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_center_size(
            rect.center(),
            Vec2::new(360.0, 150.0),
        )));
        child.vertical_centered(|ui| {
            if scanning {
                ui.add(egui::Spinner::new());
                return;
            }
            ui.label(egui::RichText::new("FlickFlock").size(26.0).strong());
            ui.add_space(4.0);
            let hint = if self.index.dir.as_os_str().is_empty() {
                "Drop an image or a folder here"
            } else {
                "No images in this folder"
            };
            ui.label(egui::RichText::new(hint).size(14.0).weak());
            ui.add_space(12.0);
            if ui.button("Open…").clicked() {
                self.perform(Action::OpenFile);
            }
            if ui.button("Open folder…").clicked() {
                self.perform(Action::OpenFolder);
            }
        });
    }

    /// Pinned metadata properties, drawn over the image.
    fn draw_pinned(&mut self, ui: &egui::Ui, rect: Rect) {
        if self.settings.metadata.pinned.is_empty() {
            return;
        }
        let Some((path, props)) = &self.meta else {
            return;
        };
        if self.display_path() != Some(path.as_path()) {
            return;
        }
        let lines: Vec<String> = self
            .settings
            .metadata
            .pinned
            .iter()
            .filter_map(|key| props.iter().find(|p| p.key() == *key))
            .map(|p| format!("{}  {}", p.name, p.value))
            .collect();
        if lines.is_empty() {
            return;
        }
        let painter = ui.painter();
        let font = FontId::proportional(12.5);
        let galley = painter.layout_no_wrap(lines.join("\n"), font, Color32::from_gray(235));
        let pad = Vec2::new(8.0, 6.0);
        let size = galley.size() + pad * 2.0;
        let pos = Pos2::new(rect.left() + 10.0, rect.bottom() - size.y - 10.0);
        painter.rect_filled(
            Rect::from_min_size(pos, size),
            6.0,
            Color32::from_black_alpha(150),
        );
        painter.galley(pos + pad, galley, Color32::WHITE);
    }

    pub(crate) fn ui_toasts(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        self.toasts.retain(|t| t.until > now);
        let Some(toast) = self.toasts.last() else {
            return;
        };
        ctx.request_repaint_after(toast.until - now);
        let pos = Pos2::new(
            self.canvas_rect.center().x,
            self.canvas_rect.top() + self.canvas_rect.height() * 0.12,
        );
        egui::Area::new(egui::Id::unique("ff-toast"))
            .order(egui::Order::Tooltip)
            .fixed_pos(pos)
            .pivot(Align2::CENTER_CENTER)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(Color32::from_black_alpha(190))
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::symmetric(14, 8))
                    .show(ui, |ui| {
                        ui.label(
                            egui::RichText::new(&toast.text)
                                .size(15.0)
                                .color(Color32::WHITE),
                        );
                    });
            });
    }

    // ----- Crop -----------------------------------------------------------

    /// Screen rectangle (points) of the image being cropped.
    fn crop_image_rect(&self, canvas: Rect, ppp: f32) -> Option<(Rect, [f32; 2])> {
        let size = self.current.as_deref().and_then(|p| self.image_size(p))?;
        let r = self.view.image_rect(
            size,
            self.viewport_px,
            self.settings.general.zoom_small_to_fit,
        );
        Some((
            Rect::from_min_size(
                canvas.min + Vec2::new(r[0], r[1]) / ppp,
                Vec2::new(r[2], r[3]) / ppp,
            ),
            size,
        ))
    }

    fn crop_interaction(&mut self, response: &egui::Response, canvas: Rect, ppp: f32) {
        let Some((screen, size)) = self.crop_image_rect(canvas, ppp) else {
            return;
        };
        let to_image = |p: Pos2| -> [f32; 2] {
            [
                ((p.x - screen.left()) / screen.width().max(1.0) * size[0]).clamp(0.0, size[0]),
                ((p.y - screen.top()) / screen.height().max(1.0) * size[1]).clamp(0.0, size[1]),
            ]
        };
        let to_screen = |x: f32, y: f32| {
            Pos2::new(
                screen.left() + x / size[0] * screen.width(),
                screen.top() + y / size[1] * screen.height(),
            )
        };
        let Some(crop) = self.crop.as_mut() else {
            return;
        };
        if response.drag_started() {
            if let Some(p) = response.interact_pointer_pos() {
                let [x, y, w, h] = crop.rect;
                let grab = 14.0;
                let corners = [
                    (true, true, x, y),
                    (false, true, x + w, y),
                    (true, false, x, y + h),
                    (false, false, x + w, y + h),
                ];
                let handle = corners
                    .iter()
                    .find(|(_, _, cx, cy)| to_screen(*cx, *cy).distance(p) <= grab)
                    .map(|(l, t, _, _)| CropHandle::Corner(*l, *t))
                    .unwrap_or_else(|| {
                        if Rect::from_min_max(to_screen(x, y), to_screen(x + w, y + h)).contains(p)
                        {
                            CropHandle::Move
                        } else {
                            CropHandle::New
                        }
                    });
                crop.drag = Some((handle, to_image(p), crop.rect));
            }
        }
        if response.dragged() {
            if let (Some((handle, start, orig)), Some(p)) =
                (crop.drag, response.interact_pointer_pos())
            {
                let now = to_image(p);
                let [ox, oy, ow, oh] = orig;
                crop.rect = match handle {
                    CropHandle::Move => {
                        let x = (ox + now[0] - start[0]).clamp(0.0, size[0] - ow);
                        let y = (oy + now[1] - start[1]).clamp(0.0, size[1] - oh);
                        [x, y, ow, oh]
                    }
                    CropHandle::Corner(left, top) => {
                        // The opposite corner stays fixed.
                        let fixed = [
                            if left { ox + ow } else { ox },
                            if top { oy + oh } else { oy },
                        ];
                        rect_from_drag(fixed, now, crop.aspect, size)
                    }
                    CropHandle::New => rect_from_drag(start, now, crop.aspect, size),
                };
            }
        }
        if response.drag_stopped() {
            crop.drag = None;
            if crop.rect[2] < 2.0 || crop.rect[3] < 2.0 {
                crop.rect = [0.0, 0.0, size[0], size[1]];
            }
        }
    }

    fn draw_crop(&mut self, ui: &mut egui::Ui, canvas: Rect, ppp: f32) {
        let Some((screen, size)) = self.crop_image_rect(canvas, ppp) else {
            return;
        };
        let Some(crop) = self.crop.as_ref() else {
            return;
        };
        let [x, y, w, h] = crop.rect;
        let map = |ix: f32, iy: f32| {
            Pos2::new(
                screen.left() + ix / size[0] * screen.width(),
                screen.top() + iy / size[1] * screen.height(),
            )
        };
        let sel = Rect::from_min_max(map(x, y), map(x + w, y + h));
        let painter = ui.painter().with_clip_rect(canvas);
        let shade = Color32::from_black_alpha(150);
        // Darken everything outside the selection.
        painter.rect_filled(
            Rect::from_min_max(canvas.min, Pos2::new(canvas.right(), sel.top())),
            0.0,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(Pos2::new(canvas.left(), sel.bottom()), canvas.max),
            0.0,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(canvas.left(), sel.top()),
                Pos2::new(sel.left(), sel.bottom()),
            ),
            0.0,
            shade,
        );
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(sel.right(), sel.top()),
                Pos2::new(canvas.right(), sel.bottom()),
            ),
            0.0,
            shade,
        );
        painter.rect_stroke(
            sel,
            0.0,
            Stroke::new(1.5, Color32::WHITE),
            StrokeKind::Outside,
        );
        let thirds = Stroke::new(0.5, Color32::from_white_alpha(110));
        for i in 1..3 {
            let fx = sel.left() + sel.width() * i as f32 / 3.0;
            let fy = sel.top() + sel.height() * i as f32 / 3.0;
            painter.line_segment(
                [Pos2::new(fx, sel.top()), Pos2::new(fx, sel.bottom())],
                thirds,
            );
            painter.line_segment(
                [Pos2::new(sel.left(), fy), Pos2::new(sel.right(), fy)],
                thirds,
            );
        }
        for corner in [
            sel.left_top(),
            sel.right_top(),
            sel.left_bottom(),
            sel.right_bottom(),
        ] {
            painter.rect_filled(
                Rect::from_center_size(corner, Vec2::splat(9.0)),
                2.0,
                Color32::WHITE,
            );
        }

        // Toolbar: aspect presets, size, apply / cancel.
        let mut apply = false;
        let mut cancel = false;
        let mut new_aspect = None;
        let current_aspect = crop.aspect;
        let label = format!("{} × {}", w.round() as u32, h.round() as u32);
        let pos = Pos2::new(canvas.center().x, canvas.top() + 54.0);
        egui::Area::new(egui::Id::unique("ff-crop-bar"))
            .fixed_pos(pos)
            .pivot(Align2::CENTER_TOP)
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.style_mut().visuals = egui::Visuals::dark();
                hud_frame().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Crop");
                        let selected = ASPECT_PRESETS
                            .iter()
                            .find(|(_, a)| *a == current_aspect)
                            .map(|(n, _)| *n)
                            .unwrap_or("Free");
                        egui::ComboBox::from_id_salt("ff-crop-aspect")
                            .selected_text(selected)
                            .width(70.0)
                            .show_ui(ui, |ui| {
                                for (name, aspect) in ASPECT_PRESETS {
                                    if ui
                                        .selectable_label(aspect == current_aspect, name)
                                        .clicked()
                                    {
                                        new_aspect = Some(aspect);
                                    }
                                }
                            });
                        ui.label(egui::RichText::new(label).monospace());
                        apply = ui.button("Apply").clicked();
                        cancel = ui.button("Cancel").clicked();
                    });
                });
            });
        // Enter confirms the selection, like a default dialog button.
        if self.modal.is_none()
            && ui
                .ctx()
                .input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none())
        {
            apply = true;
        }
        if let Some(aspect) = new_aspect {
            if let Some(crop) = self.crop.as_mut() {
                crop.aspect = aspect;
                if let Some((aw, ah)) = aspect {
                    // Largest centred rectangle of that ratio inside the current selection's centre.
                    let ratio = aw as f32 / ah as f32;
                    let (cx, cy) = (
                        crop.rect[0] + crop.rect[2] / 2.0,
                        crop.rect[1] + crop.rect[3] / 2.0,
                    );
                    let mut nw = crop.rect[2].max(crop.rect[3] * ratio).min(size[0]);
                    let mut nh = nw / ratio;
                    if nh > size[1] {
                        nh = size[1];
                        nw = nh * ratio;
                    }
                    let nx = (cx - nw / 2.0).clamp(0.0, size[0] - nw);
                    let ny = (cy - nh / 2.0).clamp(0.0, size[1] - nh);
                    crop.rect = [nx, ny, nw, nh];
                }
            }
        }
        if apply {
            self.apply_crop();
        } else if cancel {
            self.crop = None;
        }
    }
}

/// Rectangle spanned by a fixed corner and the dragged point, optionally
/// constrained to an aspect ratio and always inside the image.
fn rect_from_drag(
    fixed: [f32; 2],
    moving: [f32; 2],
    aspect: Option<(u32, u32)>,
    size: [f32; 2],
) -> [f32; 4] {
    let dir = [
        if moving[0] >= fixed[0] { 1.0 } else { -1.0 },
        if moving[1] >= fixed[1] { 1.0 } else { -1.0 },
    ];
    let mut w = (moving[0] - fixed[0]).abs();
    let mut h = (moving[1] - fixed[1]).abs();
    if let Some((aw, ah)) = aspect {
        let ratio = aw as f32 / ah as f32;
        // Space available from the fixed corner in the drag direction.
        let max_w = if dir[0] > 0.0 {
            size[0] - fixed[0]
        } else {
            fixed[0]
        };
        let max_h = if dir[1] > 0.0 {
            size[1] - fixed[1]
        } else {
            fixed[1]
        };
        w = w.max(h * ratio).min(max_w).min(max_h * ratio);
        h = w / ratio;
    }
    let x = if dir[0] > 0.0 { fixed[0] } else { fixed[0] - w };
    let y = if dir[1] > 0.0 { fixed[1] } else { fixed[1] - h };
    [x, y, w, h]
}

/// The translucent rounded panel used by every HUD element.
pub(crate) fn hud_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(Color32::from_rgba_unmultiplied(18, 18, 20, 205))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .stroke(Stroke::new(0.5, Color32::from_white_alpha(28)))
}

/// Human-readable zoom state for tooltips.
pub(crate) fn zoom_label(view: &View, percent: Option<f32>) -> String {
    match (view.zoom, percent) {
        (Zoom::Fit, Some(p)) => format!("Fit · {p:.0}%"),
        (_, Some(p)) => format!("{p:.0}%"),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: [f32; 2] = [400.0, 300.0];

    #[test]
    fn free_drag_spans_both_points_in_any_direction() {
        assert_eq!(
            rect_from_drag([100.0, 100.0], [150.0, 180.0], None, SIZE),
            [100.0, 100.0, 50.0, 80.0]
        );
        assert_eq!(
            rect_from_drag([100.0, 100.0], [40.0, 20.0], None, SIZE),
            [40.0, 20.0, 60.0, 80.0]
        );
    }

    #[test]
    fn aspect_drag_keeps_ratio_and_stays_inside() {
        let r = rect_from_drag([100.0, 100.0], [200.0, 110.0], Some((1, 1)), SIZE);
        assert_eq!(r, [100.0, 100.0, 100.0, 100.0]);
        // Dragging far outside is limited by the image bounds: 200 px of height are left.
        let r = rect_from_drag([100.0, 100.0], [400.0, 300.0], Some((16, 9)), SIZE);
        assert!((r[2] / r[3] - 16.0 / 9.0).abs() < 1e-3);
        assert!(r[0] + r[2] <= SIZE[0] + 0.01 && r[1] + r[3] <= SIZE[1] + 0.01);
        // Up-left drag.
        let r = rect_from_drag([100.0, 100.0], [0.0, 0.0], Some((1, 1)), SIZE);
        assert_eq!(r, [0.0, 0.0, 100.0, 100.0]);
    }
}
