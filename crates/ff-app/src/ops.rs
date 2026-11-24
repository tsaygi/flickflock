//! Editing and file operations. Pixel work and disk I/O run on worker
//! threads; results come back as [`EditEvent`]s.

use crate::app::{BgEvent, CropState, EditEvent, EditState, FlickApp, Modal, Record};
use ff_core::decode::{DecodeOptions, Format};
use ff_core::edit::{self, EditOp, EditSession, SaveFormat};
use ff_core::i18n::{t, tf};
use ff_core::loader::{Level, Prepared};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The clipboard must outlive the copy on some platforms (X11/Wayland serve
/// the data from the owning process), so one instance is kept for the app's lifetime.
static CLIPBOARD: Mutex<Option<arboard::Clipboard>> = Mutex::new(None);

fn with_clipboard<T>(
    f: impl FnOnce(&mut arboard::Clipboard) -> Result<T, arboard::Error>,
) -> Result<T, String> {
    let mut guard = CLIPBOARD.lock();
    if guard.is_none() {
        *guard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
    }
    f(guard.as_mut().expect("initialised above")).map_err(|e| e.to_string())
}

enum SessionChange {
    Apply(EditOp),
    Undo,
    Redo,
}

impl FlickApp {
    /// Returns `true` (and asks the user) when unsaved edits block leaving the image.
    pub(crate) fn guard_unsaved(&mut self) -> bool {
        if self
            .edit
            .as_ref()
            .map(|e| e.dirty || e.busy)
            .unwrap_or(false)
        {
            if self.modal.is_none() {
                self.modal = Some(Modal::Unsaved);
            }
            return true;
        }
        false
    }

    pub(crate) fn discard_edits(&mut self) {
        self.edit = None;
        self.crop = None;
    }

    /// Run a change on the edit session in the background. Starts a session
    /// (decoding the full-resolution image) when there is none yet.
    fn change_session(&mut self, change: SessionChange) {
        let Some(path) = self.current.clone() else {
            return;
        };
        if self.errors.contains_key(&path) {
            return;
        }
        if self.anim.as_ref().map(|a| a.path == path).unwrap_or(false) {
            self.toast(t("Animated images cannot be edited"));
            return;
        }
        let needs_new = self.edit.as_ref().map(|e| e.path != path).unwrap_or(true);
        if needs_new {
            if !matches!(change, SessionChange::Apply(_)) {
                return;
            }
            self.edit = Some(EditState {
                path: path.clone(),
                session: Arc::new(Mutex::new(None)),
                gpu: None,
                size: (0, 0),
                busy: false,
                dirty: false,
                can_undo: false,
                can_redo: false,
            });
        }
        let Some(state) = self.edit.as_mut() else {
            return;
        };
        if state.busy {
            return;
        }
        state.busy = true;
        let session = state.session.clone();
        let decoders = self.decoders.clone();
        let tile_size = self.gpu.max_texture.min(8192);
        self.spawn_bg(move || {
            let mut guard = session.lock();
            if guard.is_none() {
                match decoders.decode(&path, &DecodeOptions::default()) {
                    Ok(decoded) => *guard = Some(EditSession::new(decoded.image)),
                    Err(e) => {
                        return BgEvent::Edit(EditEvent::Failed {
                            path,
                            error: e.to_string(),
                        })
                    }
                }
            }
            let s = guard.as_mut().expect("session created above");
            match change {
                SessionChange::Apply(op) => s.apply(op),
                SessionChange::Undo => {
                    s.undo();
                }
                SessionChange::Redo => {
                    s.redo();
                }
            }
            let image = s.current().as_ref().clone();
            let size = image.dimensions();
            let prepared = Prepared::from_rgba(image, tile_size, true);
            BgEvent::Edit(EditEvent::Updated {
                path,
                prepared,
                size,
                dirty: s.is_dirty(),
                can_undo: s.can_undo(),
                can_redo: s.can_redo(),
            })
        });
    }

    pub(crate) fn edit_op(&mut self, op: EditOp) {
        self.crop = None;
        self.change_session(SessionChange::Apply(op));
    }

    pub(crate) fn edit_undo_redo(&mut self, undo: bool) {
        self.crop = None;
        self.change_session(if undo {
            SessionChange::Undo
        } else {
            SessionChange::Redo
        });
    }

    pub(crate) fn toggle_crop(&mut self) {
        if self.crop.take().is_some() {
            return;
        }
        let Some(size) = self.current.clone().and_then(|p| self.image_size(&p)) else {
            return;
        };
        // Start with a centred selection covering 80% of the image.
        let (w, h) = (size[0] * 0.8, size[1] * 0.8);
        self.crop = Some(CropState {
            rect: [(size[0] - w) / 2.0, (size[1] - h) / 2.0, w, h],
            aspect: None,
            drag: None,
        });
        self.view = Default::default();
    }

    pub(crate) fn apply_crop(&mut self) {
        let Some(crop) = self.crop.take() else { return };
        let [x, y, w, h] = crop.rect;
        if w < 1.0 || h < 1.0 {
            return;
        }
        self.edit_op(EditOp::Crop {
            x: x.round().max(0.0) as u32,
            y: y.round().max(0.0) as u32,
            width: w.round().max(1.0) as u32,
            height: h.round().max(1.0) as u32,
        });
    }

    pub(crate) fn on_edit_event(&mut self, event: EditEvent) {
        match event {
            EditEvent::Updated {
                path,
                prepared,
                size,
                dirty,
                can_undo,
                can_redo,
            } => {
                let Some(state) = self.edit.as_mut().filter(|e| e.path == path) else {
                    return;
                };
                state.gpu = Some(Arc::new(self.gpu.upload(&prepared)));
                state.size = size;
                state.busy = false;
                state.dirty = dirty;
                state.can_undo = can_undo;
                state.can_redo = can_redo;
                self.view = Default::default();
            }
            EditEvent::Failed { path, error } => {
                if self.edit.as_ref().map(|e| e.path == path).unwrap_or(false) {
                    self.edit = None;
                }
                self.toast(tf("Cannot edit this image: {0}", &[&error]));
            }
            EditEvent::Saved {
                source,
                dest,
                result,
            } => match result {
                Ok(()) => {
                    if self
                        .edit
                        .as_ref()
                        .map(|e| e.path == source)
                        .unwrap_or(false)
                    {
                        self.edit = None;
                    }
                    self.invalidate(&dest);
                    self.cache.remove(&(dest.clone(), Level::Full));
                    let name = dest
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    self.toast(tf("Saved {0}", &[&name]));
                    if dest != source && dest.parent() == Some(self.index.dir.as_path()) {
                        // A new file in this folder: show it once the watcher re-indexes.
                        self.watch_dirty_since = Some(std::time::Instant::now());
                    }
                }
                Err(error) => {
                    if let Some(state) = self.edit.as_mut() {
                        state.busy = false;
                    }
                    self.modal = Some(Modal::Message {
                        title: t("Could not save").into(),
                        body: error,
                    });
                }
            },
        }
    }

    /// Save over the original (Mod+S). Asks first unless disabled.
    pub(crate) fn request_save(&mut self) {
        let Some(state) = self.edit.as_ref().filter(|e| e.dirty && !e.busy) else {
            return;
        };
        if SaveFormat::from_path(&state.path).is_none() {
            // The original format cannot be written: choose another one.
            self.request_save_as();
        } else if self.settings.general.confirm_overwrite {
            self.modal = Some(Modal::ConfirmOverwrite);
        } else {
            self.save_over_original();
        }
    }

    pub(crate) fn request_save_as(&mut self) {
        if self.current.is_none() {
            return;
        }
        let format = self
            .current
            .as_deref()
            .and_then(SaveFormat::from_path)
            .unwrap_or(SaveFormat::Jpeg);
        self.modal = Some(Modal::SaveAs {
            format,
            quality: 92,
        });
    }

    pub(crate) fn save_over_original(&mut self) {
        let Some(path) = self.edit.as_ref().map(|e| e.path.clone()) else {
            return;
        };
        let Some(format) = SaveFormat::from_path(&path) else {
            return;
        };
        self.save_to(path, format, 92);
    }

    /// Write the edited (or, without edits, the original) pixels to `dest`.
    pub(crate) fn save_to(&mut self, dest: PathBuf, format: SaveFormat, quality: u8) {
        let Some(source) = self.current.clone() else {
            return;
        };
        let session = self
            .edit
            .as_ref()
            .filter(|e| e.path == source)
            .map(|e| e.session.clone());
        if let Some(state) = self.edit.as_mut() {
            state.busy = true;
        }
        let decoders = self.decoders.clone();
        let source_is_jpeg = self
            .infos
            .get(&source)
            .map(|i| i.format == Format::Jpeg)
            .unwrap_or(false);
        self.spawn_bg(move || {
            let run = || -> Result<(), String> {
                let guard = session.as_ref().map(|s| s.lock());
                let edited = guard.as_ref().and_then(|g| g.as_ref());
                // Rotations/flips of a JPEG saved as JPEG only touch the EXIF orientation tag.
                if let Some(s) = edited {
                    if source_is_jpeg && format == SaveFormat::Jpeg && s.is_orientation_only() {
                        match edit::save_jpeg_orientation(&source, &dest, s.ops()) {
                            Ok(true) => return Ok(()),
                            Ok(false) => {}
                            Err(e) => return Err(e.to_string()),
                        }
                    }
                }
                let image = match edited {
                    Some(s) => s.current().clone(),
                    None => Arc::new(
                        decoders
                            .decode(&source, &DecodeOptions::default())
                            .map_err(|e| e.to_string())?
                            .image,
                    ),
                };
                edit::save_image(&image, &dest, format, quality, Some(&source))
                    .map_err(|e| e.to_string())
            };
            let result = run();
            BgEvent::Edit(EditEvent::Saved {
                source,
                dest,
                result,
            })
        });
    }

    pub(crate) fn trash_current(&mut self) {
        let Some(path) = self.current.clone() else {
            return;
        };
        self.discard_edits();
        if let Err(e) = move_to_trash(&path) {
            self.modal = Some(Modal::Message {
                title: t("Could not move to Trash").into(),
                body: e.to_string(),
            });
            return;
        }
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let idx = self.current_idx;
        self.invalidate(&path);
        self.history.remove(&path);
        self.index.entries.retain(|e| e.path != path);
        let spec = self.index.spec.clone();
        self.index.resort(&spec);
        self.current = None;
        self.displayed = None;
        self.transition = None;
        // Advance to the image that took its place (or the new last one).
        if self.index.is_empty() {
            self.anim = None;
            self.toast(tf("Moved {0} to the Trash — folder is now empty", &[&name]));
        } else {
            self.go_index(idx.min(self.index.len() - 1), Record::Step);
            self.toast(tf("Moved {0} to the Trash", &[&name]));
        }
    }

    pub(crate) fn rename_current(&mut self, new_name: &str) {
        let Some(old) = self.current.clone() else {
            return;
        };
        let new_name = new_name.trim();
        if new_name.is_empty()
            || new_name.contains(['/', '\\'])
            || new_name == "."
            || new_name == ".."
        {
            self.modal = Some(Modal::Message {
                title: t("Invalid name").into(),
                body: t("File names cannot be empty or contain slashes.").into(),
            });
            return;
        }
        let new = old.with_file_name(new_name);
        if new == old {
            return;
        }
        if new.exists() {
            self.modal = Some(Modal::Message {
                title: t("Cannot rename").into(),
                body: tf("“{0}” already exists.", &[&new_name]),
            });
            return;
        }
        if let Err(e) = std::fs::rename(&old, &new) {
            self.modal = Some(Modal::Message {
                title: t("Cannot rename").into(),
                body: e.to_string(),
            });
            return;
        }
        self.invalidate(&old);
        self.history.rename(&old, &new);
        if let Some(entry) = self.index.entries.iter_mut().find(|e| e.path == old) {
            if let Ok(updated) = ff_core::folder::ImageEntry::from_path(&new) {
                *entry = updated;
            }
        }
        let spec = self.index.spec.clone();
        self.index.resort(&spec);
        self.current = Some(new.clone());
        self.current_idx = self.index.position(&new).unwrap_or(0);
        self.edit = None;
        self.anim = None;
        self.strip_scroll_to = true;
        self.toast(tf("Renamed to {0}", &[&new_name]));
    }

    pub(crate) fn reveal_current(&mut self) {
        let Some(path) = self.current.clone() else {
            return;
        };
        #[cfg(target_os = "macos")]
        let result = ff_platform::reveal(&path);
        #[cfg(not(target_os = "macos"))]
        let result = opener::reveal(&path);
        if let Err(e) = result {
            self.toast(tf("Cannot reveal file: {0}", &[&e]));
        }
    }

    pub(crate) fn open_with_default(&mut self) {
        let Some(path) = self.current.clone() else {
            return;
        };
        #[cfg(target_os = "macos")]
        let result = ff_platform::open(&path);
        #[cfg(not(target_os = "macos"))]
        let result = opener::open(&path);
        if let Err(e) = result {
            self.toast(tf("Cannot open file: {0}", &[&e]));
        }
    }

    pub(crate) fn copy_path(&mut self) {
        let Some(path) = self.current.clone() else {
            return;
        };
        let text = path.to_string_lossy().into_owned();
        match with_clipboard(|c| c.set_text(text)) {
            Ok(()) => self.toast(t("Path copied")),
            Err(e) => self.toast(tf("Clipboard error: {0}", &[&e])),
        }
    }

    /// Copy the (edited) image to the clipboard at full resolution.
    pub(crate) fn copy_image(&mut self) {
        let Some(path) = self.current.clone() else {
            return;
        };
        let session = self
            .edit
            .as_ref()
            .filter(|e| e.path == path)
            .map(|e| e.session.clone());
        let decoders = self.decoders.clone();
        self.spawn_bg(move || {
            let image = match session.and_then(|s| s.lock().as_ref().map(|s| s.current().clone())) {
                Some(img) => Ok(img),
                None => decoders
                    .decode(&path, &DecodeOptions::default())
                    .map(|d| Arc::new(d.image))
                    .map_err(|e| e.to_string()),
            };
            let result = image.and_then(|img| {
                with_clipboard(|c| {
                    c.set_image(arboard::ImageData {
                        width: img.width() as usize,
                        height: img.height() as usize,
                        bytes: std::borrow::Cow::Borrowed(img.as_raw()),
                    })
                })
            });
            BgEvent::Toast(match result {
                Ok(()) => t("Image copied").to_owned(),
                Err(e) => tf("Could not copy image: {0}", &[&e]),
            })
        });
    }

    pub(crate) fn open_file_dialog(&mut self) {
        let mut dialog =
            rfd::FileDialog::new().add_filter(t("Images"), ff_core::decode::SUPPORTED_EXTENSIONS);
        if !self.index.dir.as_os_str().is_empty() {
            dialog = dialog.set_directory(&self.index.dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open_path(&path);
        }
    }

    pub(crate) fn open_folder_dialog(&mut self) {
        let mut dialog = rfd::FileDialog::new();
        if !self.index.dir.as_os_str().is_empty() {
            dialog = dialog.set_directory(&self.index.dir);
        }
        if let Some(path) = dialog.pick_folder() {
            ff_platform::remember_folder(&path);
            self.open_path(&path);
        }
    }

    /// Ask where to save, then save in the chosen format.
    pub(crate) fn save_as_dialog(&mut self, format: SaveFormat, quality: u8) {
        let Some(current) = self.current.clone() else {
            return;
        };
        let stem = current
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let mut dialog = rfd::FileDialog::new()
            .add_filter(t(format.label()), &[format.extension()])
            .set_file_name(format!("{stem}.{}", format.extension()));
        if let Some(dir) = current.parent() {
            dialog = dialog.set_directory(dir);
        }
        if let Some(mut dest) = dialog.save_file() {
            if SaveFormat::from_path(&dest) != Some(format) {
                dest = with_appended_extension(&dest, format.extension());
            }
            self.save_to(dest, format, quality);
        }
    }
}

fn with_appended_extension(path: &Path, ext: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".");
    name.push(ext);
    path.with_file_name(name)
}

/// Move a file to the Trash / Recycle Bin (never deletes permanently).
fn move_to_trash(path: &Path) -> Result<(), trash::Error> {
    #[allow(unused_mut)]
    let mut context = trash::TrashContext::default();
    // The default on macOS scripts Finder, which needs an Automation
    // permission prompt; NSFileManager needs none.
    #[cfg(target_os = "macos")]
    {
        use trash::macos::{DeleteMethod, TrashContextExtMacos};
        context.set_delete_method(DeleteMethod::NsFileManager);
    }
    context.delete(path)
}
