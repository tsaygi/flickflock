//! Image state: the GPU cache, what to load next, animation playback and
//! assembling the scene the canvas draws.

use crate::app::{ActiveTransition, AnimState, FlickApp, ImageInfo};
use crate::canvas::{transition_index, Layer, Scene, View};
use eframe::egui;
use ff_core::anim::Player;
use ff_core::config::Background;
use ff_core::loader::{Level, LoadEvent, LoadJob};
use ff_core::nav::Direction;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// While skimming, full previews are only requested once the mouse rests this long.
const SKIM_SETTLE: Duration = Duration::from_millis(120);
/// Prefetch across the folder boundary when this close to it.
const BOUNDARY_PREFETCH: usize = 3;

impl FlickApp {
    pub(crate) fn background_color(&self) -> egui::Color32 {
        match self.settings.general.background {
            Background::Black => egui::Color32::BLACK,
            Background::DarkGray | Background::Checkerboard => egui::Color32::from_gray(30),
            Background::System => {
                if self.ctx.global_style().visuals.dark_mode {
                    egui::Color32::from_gray(30)
                } else {
                    egui::Color32::from_gray(232)
                }
            }
        }
    }

    /// Longest side of preview images: the largest monitor dimension in
    /// physical pixels, so a preview is sharp at fit-to-window on any window size.
    fn preview_dim(&self) -> u32 {
        let (monitor, ppp) = self
            .ctx
            .input(|i| (i.viewport().monitor_size, i.pixels_per_point()));
        let from_monitor = monitor.map(|m| (m.x.max(m.y) * ppp) as u32).unwrap_or(0);
        let from_window = self.viewport_px[0].max(self.viewport_px[1]) as u32;
        from_monitor
            .max(from_window)
            .clamp(1024, self.gpu.max_texture.min(8192))
    }

    fn tile_size(&self) -> u32 {
        self.gpu.max_texture.min(8192)
    }

    /// Layout size (full-resolution pixels) of an image, when known.
    pub(crate) fn image_size(&self, path: &Path) -> Option<[f32; 2]> {
        if let Some(edit) = self
            .edit
            .as_ref()
            .filter(|e| e.path == path && e.gpu.is_some())
        {
            return Some([edit.size.0 as f32, edit.size.1 as f32]);
        }
        self.infos
            .get(path)
            .map(|i| [i.full_size.0 as f32, i.full_size.1 as f32])
    }

    fn view_for(&self, path: &Path) -> View {
        if self.current.as_deref() == Some(path) {
            self.view
        } else {
            View::default()
        }
    }

    /// The best available GPU image for `path`, placed in the viewport.
    pub(crate) fn layer_for(&mut self, path: &Path) -> Option<Layer> {
        let size = self.image_size(path)?;
        let enlarge = self.settings.general.zoom_small_to_fit;
        let rect = self
            .view_for(path)
            .image_rect(size, self.viewport_px, enlarge);
        let scale = rect[2] / size[0].max(1.0);
        let key_full = (path.to_path_buf(), Level::Full);
        let key_preview = (path.to_path_buf(), Level::Preview);
        let image = if let Some(gpu) = self
            .edit
            .as_ref()
            .filter(|e| e.path == path)
            .and_then(|e| e.gpu.clone())
        {
            gpu
        } else if let Some(anim) = self.anim.as_ref().filter(|a| a.path == path) {
            anim.gpu.clone()
        } else if let Some(gpu) = self.cache.get(&key_full) {
            gpu.clone()
        } else {
            self.cache.get(&key_preview)?.clone()
        };
        Some(Layer {
            image,
            rect,
            nearest: self.settings.general.pixelated_zoom && scale >= 2.0,
            checker: self.settings.general.background == Background::Checkerboard,
        })
    }

    /// Snapshot the image on screen as the outgoing side of a transition.
    pub(crate) fn begin_transition(&mut self) {
        let name = if self.slideshow.is_some() {
            &self.settings.transitions.slideshow
        } else {
            &self.settings.transitions.browse
        };
        self.transition = match (transition_index(name), self.skim.is_some()) {
            (Some(index), false) if self.displayed.is_some() => Some(ActiveTransition {
                from: self.displayed.clone(),
                index,
                start: None,
                duration: Duration::from_millis(u64::from(self.settings.transitions.duration_ms)),
            }),
            _ => None,
        };
    }

    /// Decide what the canvas draws this frame.
    pub(crate) fn build_scene(&mut self) -> Scene {
        let Some(path) = self.display_path().map(Path::to_path_buf) else {
            self.displayed = None;
            self.transition = None;
            return Scene::default();
        };
        let layer = self.layer_for(&path);
        if self.errors.contains_key(&path) {
            self.transition = None;
        }
        let now = Instant::now();
        let mut scene = Scene::default();
        match (&mut self.transition, &layer) {
            // The incoming image is not ready: keep showing the outgoing one.
            (Some(t), None) => scene.layer = t.from.clone(),
            (Some(t), Some(_)) => {
                let start = *t.start.get_or_insert(now);
                let progress =
                    now.duration_since(start).as_secs_f32() / t.duration.as_secs_f32().max(0.001);
                scene.layer = layer.clone();
                if progress >= 1.0 {
                    self.transition = None;
                } else {
                    scene.transition = Some((t.from.clone(), t.index, progress));
                    self.ctx.request_repaint();
                }
            }
            (None, _) => scene.layer = layer.clone(),
        }
        if layer.is_some() || self.transition.is_none() {
            self.displayed = layer;
        }
        scene
    }

    /// Drop everything cached for a file (it changed on disk or was removed).
    pub(crate) fn invalidate(&mut self, path: &Path) {
        self.cache.remove(&(path.to_path_buf(), Level::Preview));
        self.cache.remove(&(path.to_path_buf(), Level::Full));
        self.infos.remove(path);
        self.errors.remove(path);
        self.full_failed.remove(path);
        self.thumb_cache.remove(&path.to_path_buf());
        self.thumb_failed.remove(path);
        if self.anim.as_ref().map(|a| a.path == path).unwrap_or(false) {
            self.anim = None;
        }
        if self.meta.as_ref().map(|(p, _)| p == path).unwrap_or(false) {
            self.meta = None;
        }
        self.last_wanted.clear();
        self.last_thumb_wanted.clear();
    }

    pub(crate) fn poll_loader(&mut self) {
        for event in self.loader.poll() {
            match event {
                LoadEvent::Loaded(loaded) => {
                    let loaded = *loaded;
                    let is_current = self.current.as_deref() == Some(loaded.path.as_path());
                    let info = self.infos.entry(loaded.path.clone()).or_insert(ImageInfo {
                        full_size: loaded.full_size,
                        format: loaded.format,
                        decoder: loaded.decoder,
                        preview_is_full: false,
                        maybe_animated: loaded.maybe_animated,
                    });
                    info.full_size = loaded.full_size;
                    if loaded.level == Level::Preview {
                        info.preview_is_full = loaded.is_full_resolution;
                        info.maybe_animated = loaded.maybe_animated;
                    }
                    if loaded.level == Level::Full {
                        // A full load is also the animation probe.
                        info.maybe_animated = false;
                        if !is_current {
                            continue;
                        }
                        if let Some(anim) = loaded.animation {
                            let gpu = Arc::new(self.gpu.create_dynamic(anim.width, anim.height));
                            if let Some(frame) = anim.frame(0) {
                                self.gpu.write_frame(&gpu, &frame);
                            }
                            self.anim = Some(AnimState {
                                path: loaded.path.clone(),
                                anim,
                                player: Player::default(),
                                gpu,
                                shown: 0,
                                last_tick: Instant::now(),
                            });
                            continue;
                        }
                    }
                    let started = Instant::now();
                    let gpu = Arc::new(self.gpu.upload(&loaded.prepared));
                    tracing::debug!(
                        "{}: {:?} decoded in {} ms, uploaded {} tile(s) in {:.1} ms",
                        loaded.path.display(),
                        loaded.level,
                        loaded.decode_ms,
                        gpu.tile_count(),
                        started.elapsed().as_secs_f64() * 1000.0
                    );
                    let pinned_path = self.display_path().map(Path::to_path_buf);
                    let bytes = gpu.bytes;
                    let evicted = self
                        .cache
                        .insert((loaded.path, loaded.level), gpu, bytes, |k| {
                            Some(&k.0) == pinned_path.as_ref()
                        });
                    for ((path, _), _) in evicted {
                        let still_cached = self.cache.contains(&(path.clone(), Level::Preview))
                            || self.cache.contains(&(path.clone(), Level::Full));
                        if !still_cached {
                            self.infos.remove(&path);
                        }
                    }
                }
                LoadEvent::Failed {
                    path,
                    level,
                    error,
                    denied,
                } => {
                    tracing::debug!("{}: {level:?} failed: {error}", path.display());
                    if denied {
                        self.offer_folder_access(&path);
                    }
                    match level {
                        Level::Preview => {
                            self.errors.insert(path, error);
                        }
                        Level::Full => {
                            if let Some(info) = self.infos.get_mut(&path) {
                                info.maybe_animated = false;
                            }
                            self.full_failed.insert(path);
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn poll_thumbs(&mut self) {
        for event in self.thumbs.poll() {
            match event.image {
                Some(img) => {
                    let size = [img.width() as usize, img.height() as usize];
                    let bytes = img.as_raw().len();
                    let color = egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw());
                    let name = event.path.to_string_lossy().into_owned();
                    let texture = self
                        .ctx
                        .load_texture(name, color, egui::TextureOptions::LINEAR);
                    self.thumb_cache
                        .insert(event.path, texture, bytes, |_| false);
                }
                None => {
                    self.thumb_failed.insert(event.path);
                }
            }
        }
    }

    /// Thumbnail texture for `path`; requests it when missing.
    pub(crate) fn thumb(&mut self, path: &Path) -> Option<egui::TextureHandle> {
        let key = path.to_path_buf();
        if let Some(t) = self.thumb_cache.get(&key) {
            return Some(t.clone());
        }
        if !self.thumb_failed.contains(path) && !self.thumb_requests.contains(&key) {
            self.thumb_requests.push(key);
        }
        None
    }

    /// Tell the loader and the thumbnail service what is needed right now.
    pub(crate) fn update_wanted(&mut self) {
        let job = LoadJob {
            preview_dim: self.preview_dim(),
            tile_size: self.tile_size(),
            animate: false,
        };
        let mut wanted: Vec<((PathBuf, Level), LoadJob)> = Vec::new();
        let want_preview =
            |app: &Self, path: &Path, wanted: &mut Vec<((PathBuf, Level), LoadJob)>| {
                let key = (path.to_path_buf(), Level::Preview);
                if !app.cache.contains(&key)
                    && !app.errors.contains_key(path)
                    && !wanted.iter().any(|(k, _)| *k == key)
                {
                    wanted.push((key, job));
                }
            };

        let skimming_fast = self
            .skim
            .as_ref()
            .map(|s| s.last_change.elapsed() < SKIM_SETTLE)
            .unwrap_or(false);
        if skimming_fast {
            self.ctx.request_repaint_after(SKIM_SETTLE);
        }
        let editing = self.edit.as_ref().map(|e| e.path.clone());

        if let (Some(path), false) = (self.display_path().map(Path::to_path_buf), skimming_fast) {
            want_preview(self, &path, &mut wanted);
            if let Some(info) = self.infos.get(&path).cloned() {
                let is_current = self.current.as_deref() == Some(path.as_path());
                let full_key = (path.clone(), Level::Full);
                let can_load_full =
                    !self.full_failed.contains(&path) && editing.as_ref() != Some(&path);
                let animated_here = self.anim.as_ref().map(|a| a.path == path).unwrap_or(false);
                if is_current && can_load_full && info.maybe_animated && !animated_here {
                    wanted.push((
                        full_key,
                        LoadJob {
                            animate: true,
                            ..job
                        },
                    ));
                } else if is_current
                    && can_load_full
                    && !info.preview_is_full
                    && !animated_here
                    && !self.cache.contains(&full_key)
                {
                    // Zoomed in past what the preview resolves?
                    let size = [info.full_size.0 as f32, info.full_size.1 as f32];
                    let rect = self.view.image_rect(
                        size,
                        self.viewport_px,
                        self.settings.general.zoom_small_to_fit,
                    );
                    let preview_w = self
                        .cache
                        .peek(&(path.clone(), Level::Preview))
                        .map(|g| g.width as f32);
                    if preview_w.map(|w| rect[2] > w * 1.02).unwrap_or(false) {
                        wanted.push((full_key, job));
                    }
                }
            }
        }

        // Prefetch around the current image, favouring the direction of travel.
        if let (Some(_), false, None) = (&self.current, skimming_fast, &self.peek) {
            let len = self.index.len();
            let i = self.current_idx;
            let (ahead, behind) = (
                self.settings.performance.prefetch_ahead as usize,
                self.settings.performance.prefetch_behind as usize,
            );
            let forward = self.direction == Direction::Forward;
            let mut offsets: Vec<isize> = Vec::new();
            for n in 1..=ahead.max(behind) as isize {
                if n as usize <= ahead {
                    offsets.push(if forward { n } else { -n });
                }
                if n as usize <= behind {
                    offsets.push(if forward { -n } else { n });
                }
            }
            for off in offsets {
                let j = i as isize + off;
                if j >= 0 && (j as usize) < len {
                    if let Some(p) = self.index.path(j as usize).map(Path::to_path_buf) {
                        want_preview(self, &p, &mut wanted);
                    }
                }
            }
            // …and across the folder boundary when close to it.
            if i + BOUNDARY_PREFETCH >= len {
                if let Some(p) = self
                    .neighbor_index(Direction::Forward)
                    .and_then(|ix| ix.path(0))
                    .map(Path::to_path_buf)
                {
                    want_preview(self, &p, &mut wanted);
                }
            }
            if i < BOUNDARY_PREFETCH {
                let last = self
                    .neighbor_index(Direction::Backward)
                    .and_then(|ix| ix.entries.last().map(|e| e.path.clone()));
                if let Some(p) = last {
                    want_preview(self, &p, &mut wanted);
                }
            }
        }

        let keys: Vec<(PathBuf, Level)> = wanted.iter().map(|(k, _)| k.clone()).collect();
        if keys != self.last_wanted {
            self.last_wanted = keys;
            self.loader.set_wanted(wanted);
        }

        // Thumbnails: whatever the strip (and placeholders) asked for this frame.
        if self.thumb_requests != self.last_thumb_wanted {
            self.last_thumb_wanted = self.thumb_requests.clone();
            self.thumbs.set_wanted(self.thumb_requests.clone());
        }
    }

    /// Advance the animation clock and upload the frame that is due.
    pub(crate) fn tick_animation(&mut self) {
        let Some(a) = &mut self.anim else { return };
        let now = Instant::now();
        let dt = now.duration_since(a.last_tick);
        a.last_tick = now;
        a.player.advance(dt, &a.anim);
        if a.player.frame != a.shown {
            if let Some(frame) = a.anim.frame(a.player.frame) {
                self.gpu.write_frame(&a.gpu, &frame);
            }
            a.shown = a.player.frame;
        }
        if let Some(wait) = a.player.time_until_next(&a.anim) {
            self.ctx.request_repaint_after(wait);
        }
    }

    /// Ask for metadata of the displayed image when the panel or pins need it.
    pub(crate) fn request_metadata_if_needed(&mut self) {
        let needed = self.settings.general.show_info || !self.settings.metadata.pinned.is_empty();
        let Some(path) = self.display_path().map(Path::to_path_buf) else {
            self.meta = None;
            return;
        };
        if !needed {
            return;
        }
        let have = self.meta.as_ref().map(|(p, _)| *p == path).unwrap_or(false);
        // Wait for the decode so dimensions/format are known and files are not read twice at once.
        let ready = self.infos.contains_key(&path) || self.errors.contains_key(&path);
        if have || !ready || self.meta_pending.as_ref() == Some(&path) {
            return;
        }
        if self.meta.as_ref().map(|(p, _)| *p != path).unwrap_or(false) {
            self.meta = None;
        }
        self.meta_pending = Some(path.clone());
        let info = self.infos.get(&path);
        let facts = ff_core::meta::ImageFacts {
            dimensions: info.map(|i| i.full_size),
            format: info.map(|i| i.format.label()),
            frames: self
                .anim
                .as_ref()
                .filter(|a| a.path == path)
                .map(|a| a.anim.frame_count()),
        };
        let decoder = info.map(|i| i.decoder);
        self.spawn_bg(move || {
            let mut props = ff_core::meta::extract(&path, &facts);
            if let Some(decoder) = decoder {
                let at = props
                    .iter()
                    .rposition(|p| p.group == "Image")
                    .map(|i| i + 1)
                    .unwrap_or(props.len());
                props.insert(
                    at,
                    ff_core::meta::Property {
                        group: "Image",
                        name: "Decoder".into(),
                        value: decoder.into(),
                    },
                );
            }
            crate::app::BgEvent::Meta { path, props }
        });
    }
}
