//! Folder browsing: opening files and folders, stepping through images,
//! crossing into neighbouring folders, history and live folder updates.

use crate::app::{
    BgEvent, FlickApp, Landing, Neighbor, NeighborState, PendingJump, PendingScan, Record,
    ScanPurpose, SkimState,
};
use ff_core::folder::{FolderIndex, FolderWatcher, ImageEntry};
use ff_core::i18n::{t, tf};
use ff_core::loader::Level;
use ff_core::nav::{self, Direction, DirectoryNavigator, FsSource, Step};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Quiet period after a file-system event before the folder is re-read.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(350);
/// How long a "press again" boundary prompt stays armed.
const BOUNDARY_ARM: Duration = Duration::from_secs(6);

fn slot(direction: Direction) -> usize {
    match direction {
        Direction::Forward => 0,
        Direction::Backward => 1,
    }
}

fn folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned())
}

impl FlickApp {
    /// Open a file or folder given by the user (argv, drop, dialog, IPC).
    pub(crate) fn open_path(&mut self, path: &Path) {
        if self.guard_unsaved() {
            return;
        }
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let Ok(md) = std::fs::metadata(&path) else {
            self.toast(tf("Cannot open {0}", &[&path.display()]));
            return;
        };
        self.slideshow = None;
        if md.is_dir() {
            self.nav_root = self
                .settings
                .folders
                .tree_root
                .clone()
                .or_else(|| path.parent().map(Path::to_path_buf));
            self.open_folder(&path, Landing::First);
        } else {
            self.show_file(&path, Record::Jump);
        }
    }

    /// Show a file, switching folders when necessary.
    pub(crate) fn show_file(&mut self, path: &Path, record: Record) {
        let dir = path.parent().unwrap_or(Path::new("")).to_path_buf();
        if dir == self.index.dir && self.index.position(path).is_some() {
            self.set_current(path.to_path_buf(), record);
            return;
        }
        if dir != self.index.dir {
            // Show the file at once from a one-entry index; the real folder
            // index replaces it as soon as the background scan finishes.
            let entries = ImageEntry::from_path(path).ok().into_iter().collect();
            self.index = FolderIndex::from_entries(&dir, entries, &self.settings.sorting);
            if self
                .nav_root
                .as_ref()
                .map(|r| !dir.starts_with(r))
                .unwrap_or(true)
            {
                self.nav_root = self
                    .settings
                    .folders
                    .tree_root
                    .clone()
                    .or_else(|| dir.parent().map(Path::to_path_buf));
            }
            self.on_folder_changed();
        } else {
            self.index.ensure_contains(path);
        }
        self.set_current(path.to_path_buf(), record);
        self.request_scan(&dir, ScanPurpose::Refresh);
    }

    /// Open a folder and land on its first or last image.
    pub(crate) fn open_folder(&mut self, dir: &Path, landing: Landing) {
        self.index = FolderIndex {
            dir: dir.to_path_buf(),
            spec: self.settings.sorting.clone(),
            ..Default::default()
        };
        self.leave_current();
        self.on_folder_changed();
        self.request_scan(dir, ScanPurpose::Open(landing));
    }

    pub(crate) fn request_scan(&mut self, dir: &Path, purpose: ScanPurpose) {
        self.scan_seq += 1;
        let id = self.scan_seq;
        self.pending_scan = Some(PendingScan { id, purpose });
        let dir = dir.to_path_buf();
        let spec = self.settings.sorting.clone();
        self.spawn_bg(move || BgEvent::Scanned {
            id,
            result: FolderIndex::scan(&dir, &spec),
        });
    }

    pub(crate) fn on_scanned(&mut self, id: u64, result: std::io::Result<FolderIndex>) {
        let Some(pending) = self.pending_scan.take_if(|p| p.id == id) else {
            return;
        };
        let mut index = match result {
            Ok(index) => index,
            Err(e) => {
                self.toast(tf("Cannot read folder: {0}", &[&e]));
                return;
            }
        };
        if index.dir != self.index.dir {
            return;
        }
        // Files that changed on disk since the last index must be reloaded.
        let changed: Vec<PathBuf> = index
            .entries
            .iter()
            .filter(|new| {
                self.index
                    .entries
                    .iter()
                    .find(|old| old.path == new.path)
                    .map(|old| old.size != new.size || old.modified != new.modified)
                    .unwrap_or(false)
            })
            .map(|e| e.path.clone())
            .collect();
        for path in &changed {
            self.invalidate(path);
        }
        let old_position = self.current_idx;
        let target = match (&self.current, &pending.purpose) {
            (Some(cur), _) if cur.exists() => {
                index.ensure_contains(cur);
                Some(cur.clone())
            }
            // The current file vanished: stay at the same position in the list.
            (Some(_), _) => index
                .path(old_position.min(index.len().saturating_sub(1)))
                .map(Path::to_path_buf),
            (None, ScanPurpose::Open(Landing::Last)) => {
                index.entries.last().map(|e| e.path.clone())
            }
            (None, _) => index.entries.first().map(|e| e.path.clone()),
        };
        let was_open = matches!(pending.purpose, ScanPurpose::Open(_));
        self.index = index;
        match target {
            Some(path) if self.current.as_ref() == Some(&path) => {
                self.current_idx = self.index.position(&path).unwrap_or(0);
                self.strip_scroll_to = true;
            }
            Some(path) => {
                if let Some(gone) = self.current.clone() {
                    self.history.remove(&gone);
                }
                self.set_current(path, Record::Jump);
                if was_open {
                    self.folder_toast();
                }
            }
            None => {
                self.leave_current();
                if was_open {
                    self.toast(tf("No images in {0}", &[&folder_name(&self.index.dir)]));
                }
            }
        }
        self.request_neighbors();
    }

    /// Reset per-folder state after `self.index.dir` changed.
    fn on_folder_changed(&mut self) {
        self.pending_jump = None;
        self.boundary_armed = None;
        self.thumb_failed.clear();
        self.errors.clear();
        self.full_failed.clear();
        self.watch_dirty_since = None;
        self.watch_flag.store(false, Ordering::Relaxed);
        let flag = self.watch_flag.clone();
        let ctx = self.ctx.clone();
        let recursive = self.settings.sorting.recursive;
        self.watcher = FolderWatcher::new(&self.index.dir, recursive, move || {
            flag.store(true, Ordering::Relaxed);
            ctx.request_repaint();
        })
        .map_err(|e| tracing::debug!("cannot watch {}: {e}", self.index.dir.display()))
        .ok();
        self.request_neighbors();
    }

    /// Re-read the folder shortly after it changed on disk.
    pub(crate) fn poll_watcher(&mut self) {
        if self.watch_flag.swap(false, Ordering::Relaxed) {
            self.watch_dirty_since = Some(Instant::now());
        }
        let Some(since) = self.watch_dirty_since else {
            return;
        };
        if since.elapsed() < WATCH_DEBOUNCE {
            self.ctx.request_repaint_after(WATCH_DEBOUNCE);
            return;
        }
        if self.pending_scan.is_some() {
            self.ctx.request_repaint_after(WATCH_DEBOUNCE);
            return;
        }
        self.watch_dirty_since = None;
        let dir = self.index.dir.clone();
        if !dir.as_os_str().is_empty() {
            self.request_scan(&dir, ScanPurpose::Refresh);
        }
    }

    fn navigator(&self) -> DirectoryNavigator {
        DirectoryNavigator::new(self.settings.folders.traverse_tree, self.nav_root.clone())
    }

    /// Pre-index the next and previous folders in the background.
    pub(crate) fn request_neighbors(&mut self) {
        let dir = self.index.dir.clone();
        if dir.as_os_str().is_empty() {
            return;
        }
        for direction in [Direction::Forward, Direction::Backward] {
            let n = &self.neighbors[slot(direction)];
            if n.from == dir && !matches!(n.state, NeighborState::Unknown) {
                continue;
            }
            self.neighbors[slot(direction)] = Neighbor {
                from: dir.clone(),
                state: NeighborState::Pending,
            };
            let nav = self.navigator();
            let source = FsSource {
                sort: self.settings.folders.sort,
            };
            let spec = self.settings.sorting.clone();
            let from = dir.clone();
            self.spawn_bg(move || {
                // Keep looking past folders whose images vanished between the check and the scan.
                let mut cursor = from.clone();
                let mut found = None;
                for _ in 0..64 {
                    let Some(next) = nav.neighbor(&cursor, direction, &source) else {
                        break;
                    };
                    match FolderIndex::scan(&next, &spec) {
                        Ok(index) if !index.is_empty() => {
                            found = Some(index);
                            break;
                        }
                        _ => cursor = next,
                    }
                }
                BgEvent::Neighbor {
                    from,
                    direction,
                    found,
                }
            });
        }
    }

    /// Forget pre-indexed neighbours (folder settings changed).
    pub(crate) fn reset_neighbors(&mut self) {
        for n in &mut self.neighbors {
            n.state = NeighborState::Unknown;
        }
        self.request_neighbors();
    }

    pub(crate) fn on_neighbor(
        &mut self,
        from: PathBuf,
        direction: Direction,
        found: Option<FolderIndex>,
    ) {
        if from != self.index.dir {
            return;
        }
        self.neighbors[slot(direction)] = Neighbor {
            from,
            state: NeighborState::Ready(found.map(Arc::new)),
        };
        if let Some(jump) = self.pending_jump.take_if(|j| j.direction == direction) {
            self.leave_folder(jump.direction, jump.landing, jump.boundary);
        }
    }

    /// The pre-indexed neighbour folder in `direction`, if known.
    pub(crate) fn neighbor_index(&self, direction: Direction) -> Option<&Arc<FolderIndex>> {
        let n = &self.neighbors[slot(direction)];
        match &n.state {
            NeighborState::Ready(Some(ix)) if n.from == self.index.dir => Some(ix),
            _ => None,
        }
    }

    /// Step to the next / previous image, applying the end-of-folder policy.
    pub(crate) fn navigate(&mut self, direction: Direction) {
        if self.guard_unsaved() {
            return;
        }
        self.direction = direction;
        if self.current.is_none() && self.index.is_empty() {
            if self.settings.general.end_of_folder == nav::EndOfFolder::Continue {
                self.leave_folder(direction, landing_for(direction), true);
            }
            return;
        }
        match nav::step(
            self.current_idx,
            self.index.len(),
            direction,
            self.settings.general.end_of_folder,
        ) {
            Step::Index(i) => {
                self.boundary_armed = None;
                self.go_index(i, Record::Step);
            }
            Step::Stay => self.toast(if direction == Direction::Forward {
                t("Last image")
            } else {
                t("First image")
            }),
            Step::Leave => self.leave_folder(direction, landing_for(direction), true),
        }
    }

    /// Move to the neighbouring folder in `direction`.
    pub(crate) fn leave_folder(&mut self, direction: Direction, landing: Landing, boundary: bool) {
        if self.guard_unsaved() {
            return;
        }
        self.direction = direction;
        let n = &self.neighbors[slot(direction)];
        let current_dir = self.index.dir.clone();
        if n.from != current_dir || matches!(n.state, NeighborState::Unknown) {
            self.request_neighbors();
        }
        let next = match &self.neighbors[slot(direction)].state {
            NeighborState::Ready(Some(ix)) => ix.clone(),
            NeighborState::Ready(None) => {
                self.toast(if direction == Direction::Forward {
                    t("No more folders")
                } else {
                    t("No previous folder")
                });
                return;
            }
            _ => {
                // Still searching (slow disk or network share): jump as soon as it is known.
                self.pending_jump = Some(PendingJump {
                    direction,
                    landing,
                    boundary,
                });
                return;
            }
        };
        if boundary && self.settings.folders.boundary_pause {
            let armed = self
                .boundary_armed
                .map(|(d, at)| d == direction && at.elapsed() < BOUNDARY_ARM)
                .unwrap_or(false);
            if !armed {
                self.boundary_armed = Some((direction, Instant::now()));
                let (here, there) = (folder_name(&current_dir), folder_name(&next.dir));
                let message = if direction == Direction::Forward {
                    tf("End of {0} — press again for {1}", &[&here, &there])
                } else {
                    tf("Start of {0} — press again for {1}", &[&here, &there])
                };
                self.toast(message);
                return;
            }
        }
        self.enter_folder(next, landing);
    }

    fn enter_folder(&mut self, index: Arc<FolderIndex>, landing: Landing) {
        let target = match landing {
            Landing::First => index.entries.first(),
            Landing::Last => index.entries.last(),
        }
        .map(|e| e.path.clone());
        let Some(target) = target else { return };
        self.index = (*index).clone();
        self.on_folder_changed();
        self.set_current(target, Record::Jump);
        self.folder_toast();
    }

    /// "📁 Day2 · 1 / 84"
    pub(crate) fn folder_toast(&mut self) {
        let text = format!(
            "📁 {} · {} / {}",
            folder_name(&self.index.dir),
            self.current_idx + 1,
            self.index.len()
        );
        self.toast(text);
    }

    pub(crate) fn go_index(&mut self, i: usize, record: Record) {
        if self.guard_unsaved() {
            return;
        }
        if let Some(path) = self.index.path(i).map(Path::to_path_buf) {
            self.set_current(path, record);
        }
    }

    /// Clear the current image (empty folder).
    fn leave_current(&mut self) {
        if let Some(old) = self.current.take() {
            self.cache.remove(&(old, Level::Full));
        }
        self.current_idx = 0;
        self.anim = None;
        self.edit = None;
        self.crop = None;
        self.peek = None;
        self.transition = None;
        self.displayed = None;
        self.view = Default::default();
    }

    /// Make `path` (which must be in the index) the current image.
    pub(crate) fn set_current(&mut self, path: PathBuf, record: Record) {
        if self.current.as_ref() == Some(&path) {
            self.current_idx = self.index.position(&path).unwrap_or(self.current_idx);
            return;
        }
        if let Some(old) = self.current.take() {
            // Full-resolution pixels are only kept for the image on screen.
            self.cache.remove(&(old, Level::Full));
        }
        self.begin_transition();
        self.view = Default::default();
        self.anim = None;
        self.edit = None;
        self.crop = None;
        self.peek = None;
        self.current_idx = self.index.position(&path).unwrap_or(0);
        match record {
            Record::Jump => self.history.jump(&path),
            Record::Step => self.history.step(&path),
            Record::None => {}
        }
        self.current = Some(path);
        self.current_since = Instant::now();
        self.strip_scroll_to = true;
        self.ctx.request_repaint();
    }

    pub(crate) fn history_move(&mut self, forward: bool) {
        if self.guard_unsaved() {
            return;
        }
        let target = if forward {
            self.history.forward()
        } else {
            self.history.back()
        }
        .map(Path::to_path_buf);
        match target {
            Some(path) if path.exists() => self.show_file(&path, Record::None),
            Some(path) => {
                self.history.remove(&path);
                self.toast(t("That image no longer exists"));
            }
            None => self.toast(if forward {
                t("Nothing to go forward to")
            } else {
                t("Nothing to go back to")
            }),
        }
    }

    /// Skim: map a horizontal position (0..1) to an image of the folder.
    pub(crate) fn skim_to(&mut self, fraction: f32) {
        let len = self.index.len();
        if len == 0 || self.guard_unsaved() {
            return;
        }
        if self.skim.is_none() {
            self.skim = Some(SkimState {
                origin: self.current.clone(),
                last_change: Instant::now(),
            });
        }
        let i = ((fraction.clamp(0.0, 1.0) * len as f32) as usize).min(len - 1);
        if i != self.current_idx || self.current.is_none() {
            self.transition = None;
            if let Some(path) = self.index.path(i).map(Path::to_path_buf) {
                self.set_current(path, Record::None);
                self.transition = None;
            }
            if let Some(s) = &mut self.skim {
                s.last_change = Instant::now();
            }
        }
    }

    /// The Skim key was released: stay on the image shown.
    pub(crate) fn end_skim(&mut self) {
        let Some(skim) = self.skim.take() else { return };
        if let (Some(cur), true) = (self.current.clone(), skim.origin != self.current) {
            if let Some(origin) = &skim.origin {
                // Make sure Back returns to where skimming started.
                self.history.step(origin);
            }
            self.history.jump(&cur);
        }
    }

    /// Apply a changed sort/group specification, keeping the current image.
    pub(crate) fn resort(&mut self) {
        self.mark_settings_dirty();
        let dir = self.index.dir.clone();
        if dir.as_os_str().is_empty() {
            return;
        }
        if self.index.spec.recursive != self.settings.sorting.recursive {
            // The watcher's recursion mode follows the setting.
            self.on_folder_changed();
        }
        self.request_scan(&dir, ScanPurpose::Refresh);
        self.reset_neighbors();
    }
}

fn landing_for(direction: Direction) -> Landing {
    match direction {
        Direction::Forward => Landing::First,
        Direction::Backward => Landing::Last,
    }
}
