//! Application state and the per-frame update loop.

use crate::canvas::{Gpu, GpuImage, Layer, View};
use crate::router::{InputRouter, RouterOptions};
use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use ff_core::anim::{AnimatedImage, Player};
use ff_core::cache::{ByteLru, ThumbDiskCache};
use ff_core::config::Settings;
use ff_core::decode::{Decoders, Format};
use ff_core::edit::{EditSession, SaveFormat};
use ff_core::folder::{FolderIndex, FolderWatcher};
use ff_core::i18n::{t, tf};
use ff_core::input::{Action, Binding};
use ff_core::loader::{Level, Loader, Prepared};
use ff_core::meta::Property;
use ff_core::nav::{Direction, HistoryStack};
use ff_core::thumbs::ThumbService;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long the HUD stays visible after the last pointer movement.
pub(crate) const HUD_TIMEOUT: Duration = Duration::from_millis(1500);
/// GPU memory budget for thumbnails shown in the strip.
const THUMB_BUDGET: usize = 256 << 20;

/// What we know about an image once it has been decoded.
#[derive(Debug, Clone)]
pub(crate) struct ImageInfo {
    pub full_size: (u32, u32),
    pub format: Format,
    pub decoder: &'static str,
    /// The cached preview already contains every pixel.
    pub preview_is_full: bool,
    pub maybe_animated: bool,
}

pub(crate) struct AnimState {
    pub path: PathBuf,
    pub anim: Arc<AnimatedImage>,
    pub player: Player,
    pub gpu: Arc<GpuImage>,
    /// Frame currently in the texture.
    pub shown: usize,
    pub last_tick: Instant,
}

pub(crate) struct ActiveTransition {
    pub from: Option<Layer>,
    pub index: usize,
    /// Set once the incoming image is ready.
    pub start: Option<Instant>,
    pub duration: Duration,
}

pub(crate) struct SkimState {
    /// Image shown when skimming started.
    pub origin: Option<PathBuf>,
    pub last_change: Instant,
}

pub(crate) struct SlideshowState {
    pub next_at: Instant,
    /// The countdown is suspended (the image stays until resumed).
    pub paused: bool,
    /// Shuffled order of indexes still to show (shuffle mode).
    pub queue: Vec<usize>,
}

/// An editing session for the current image. Pixel work happens on worker
/// threads; the UI only holds the latest uploaded result.
pub(crate) struct EditState {
    pub path: PathBuf,
    pub session: Arc<Mutex<Option<EditSession>>>,
    pub gpu: Option<Arc<GpuImage>>,
    pub size: (u32, u32),
    pub busy: bool,
    pub dirty: bool,
    pub can_undo: bool,
    pub can_redo: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum CropHandle {
    Move,
    /// Corner being dragged: (left?, top?).
    Corner(bool, bool),
    New,
}

pub(crate) struct CropState {
    /// Selection in image pixels: x, y, width, height.
    pub rect: [f32; 4],
    pub aspect: Option<(u32, u32)>,
    pub drag: Option<(CropHandle, [f32; 2], [f32; 4])>,
}

pub(crate) enum Modal {
    Rename { text: String, focus: bool },
    ConfirmOverwrite,
    Unsaved,
    SaveAs { format: SaveFormat, quality: u8 },
    Extend { margins: [u32; 4], color: [u8; 3] },
    Message { title: String, body: String },
}

pub(crate) enum EditEvent {
    Updated {
        path: PathBuf,
        prepared: Prepared,
        size: (u32, u32),
        dirty: bool,
        can_undo: bool,
        can_redo: bool,
    },
    Failed {
        path: PathBuf,
        error: String,
    },
    Saved {
        source: PathBuf,
        dest: PathBuf,
        result: Result<(), String>,
    },
}

/// Results from background work, delivered to the UI thread.
pub(crate) enum BgEvent {
    Scanned {
        id: u64,
        result: std::io::Result<FolderIndex>,
    },
    Neighbor {
        from: PathBuf,
        direction: Direction,
        found: Option<FolderIndex>,
    },
    Meta {
        path: PathBuf,
        props: Vec<Property>,
    },
    Edit(EditEvent),
    Toast(String),
    /// Default-app status per entry of `FILE_TYPE_GROUPS`, plus an error to show.
    Associations {
        status: Vec<Option<bool>>,
        claimed: Vec<usize>,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Landing {
    First,
    Last,
}

pub(crate) enum ScanPurpose {
    /// Opening a folder: land on the first/last image unless one is selected already.
    Open(Landing),
    /// Re-reading the current folder (file opened, watcher fired, sort changed).
    Refresh,
}

pub(crate) struct PendingScan {
    pub id: u64,
    pub purpose: ScanPurpose,
}

pub(crate) enum NeighborState {
    Unknown,
    Pending,
    Ready(Option<Arc<FolderIndex>>),
}

pub(crate) struct Neighbor {
    /// Folder this neighbour was computed for.
    pub from: PathBuf,
    pub state: NeighborState,
}

pub(crate) struct PendingJump {
    pub direction: Direction,
    pub landing: Landing,
    pub boundary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Record {
    Jump,
    Step,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SettingsTab {
    General,
    Keys,
    Folders,
    Performance,
    Transitions,
    Sorting,
    FileTypes,
}

pub(crate) struct Toast {
    pub text: String,
    pub until: Instant,
}

pub struct FlickApp {
    pub(crate) ctx: egui::Context,
    pub(crate) gpu: Gpu,
    pub(crate) settings: Settings,
    pub(crate) settings_path: Option<PathBuf>,
    pub(crate) settings_dirty: Option<Instant>,
    pub(crate) settings_warnings: Vec<String>,
    pub(crate) decoders: Decoders,
    pub(crate) loader: Loader,
    pub(crate) thumbs: ThumbService,
    pub(crate) disk_cache: Option<ThumbDiskCache>,
    pub(crate) router: InputRouter,
    inbox: Receiver<Vec<PathBuf>>,
    pub(crate) bg_tx: Sender<BgEvent>,
    bg_rx: Receiver<BgEvent>,

    // Browsing.
    pub(crate) index: FolderIndex,
    pub(crate) current: Option<PathBuf>,
    pub(crate) current_idx: usize,
    pub(crate) history: HistoryStack,
    pub(crate) direction: Direction,
    pub(crate) nav_root: Option<PathBuf>,
    pub(crate) scan_seq: u64,
    pub(crate) pending_scan: Option<PendingScan>,
    pub(crate) neighbors: [Neighbor; 2],
    pub(crate) pending_jump: Option<PendingJump>,
    pub(crate) boundary_armed: Option<(Direction, Instant)>,
    pub(crate) watcher: Option<FolderWatcher>,
    pub(crate) watch_flag: Arc<AtomicBool>,
    pub(crate) watch_dirty_since: Option<Instant>,

    // Images.
    pub(crate) cache: ByteLru<(PathBuf, Level), Arc<GpuImage>>,
    pub(crate) infos: HashMap<PathBuf, ImageInfo>,
    pub(crate) errors: HashMap<PathBuf, String>,
    pub(crate) full_failed: HashSet<PathBuf>,
    pub(crate) last_wanted: Vec<(PathBuf, Level)>,
    pub(crate) thumb_cache: ByteLru<PathBuf, egui::TextureHandle>,
    pub(crate) thumb_failed: HashSet<PathBuf>,
    pub(crate) thumb_requests: Vec<PathBuf>,
    pub(crate) last_thumb_wanted: Vec<PathBuf>,
    pub(crate) view: View,
    pub(crate) viewport_px: [f32; 2],
    pub(crate) canvas_rect: egui::Rect,
    pub(crate) anim: Option<AnimState>,
    pub(crate) transition: Option<ActiveTransition>,
    pub(crate) displayed: Option<Layer>,
    pub(crate) current_since: Instant,

    // UI state.
    pub(crate) last_activity: Instant,
    pub(crate) hud_hovered: bool,
    pub(crate) show_settings: bool,
    pub(crate) settings_tab: SettingsTab,
    pub(crate) recording: Option<Action>,
    pub(crate) record_conflict: Option<(Action, Binding, Action)>,
    pub(crate) record_message: Option<String>,
    pub(crate) toasts: Vec<Toast>,
    pub(crate) skim: Option<SkimState>,
    pub(crate) peek: Option<PathBuf>,
    pub(crate) over_strip: bool,
    pub(crate) strip_scroll_to: bool,
    pub(crate) slideshow: Option<SlideshowState>,
    /// A slideshow was requested on the command line and starts with the first image.
    pub(crate) slideshow_requested: bool,
    pub(crate) edit: Option<EditState>,
    pub(crate) crop: Option<CropState>,
    pub(crate) modal: Option<Modal>,
    pub(crate) meta: Option<(PathBuf, Arc<Vec<Property>>)>,
    pub(crate) meta_pending: Option<PathBuf>,
    /// Default-app status per file type (`None` until queried).
    pub(crate) assoc_status: Option<Vec<Option<bool>>>,
    pub(crate) assoc_busy: bool,
    /// Types just claimed, shown as ours until the OS has caught up.
    pub(crate) assoc_claimed: Vec<usize>,
    pub(crate) assoc_recheck_at: Option<Instant>,
    pub(crate) fullscreen: bool,
    pub(crate) title: String,
    pub(crate) rng: u64,
    pub(crate) selftest: Option<crate::selftest::SelfTest>,
}

/// Everything `main` prepares before the window exists.
pub struct Startup {
    pub settings: Settings,
    pub settings_path: Option<PathBuf>,
    pub settings_warnings: Vec<String>,
    pub decoders: Decoders,
    pub initial: Vec<PathBuf>,
    /// Start a slideshow once the first image is known (`Some(Some(secs))` sets the interval).
    pub slideshow: Option<Option<f32>>,
    pub inbox: Receiver<Vec<PathBuf>>,
    /// Filled with the egui context so background threads can wake the UI.
    pub waker: Arc<std::sync::OnceLock<egui::Context>>,
}

impl FlickApp {
    pub fn new(cc: &eframe::CreationContext<'_>, startup: Startup) -> Result<Self, String> {
        let render_state = cc
            .wgpu_render_state
            .as_ref()
            .ok_or("FlickFlock needs the wgpu renderer")?;
        let gpu = crate::canvas::install(render_state);
        let ctx = cc.egui_ctx.clone();
        let _ = startup.waker.set(ctx.clone());

        let threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        let wake = ctx.clone();
        let loader = Loader::new(startup.decoders.clone(), threads.clamp(2, 6), move || {
            wake.request_repaint()
        });
        let wake = ctx.clone();
        let disk_cache = ThumbDiskCache::default_location();
        let thumbs = ThumbService::new(
            startup.decoders.clone(),
            disk_cache.clone(),
            threads.clamp(2, 8),
            move || wake.request_repaint(),
        );
        let (bg_tx, bg_rx) = crossbeam_channel::unbounded();
        let cache_bytes = startup.settings.performance.cache_mb as usize * (1 << 20);
        let now = Instant::now();

        let mut app = Self {
            ctx,
            gpu,
            fullscreen: false,
            settings: startup.settings,
            settings_path: startup.settings_path,
            settings_dirty: None,
            settings_warnings: startup.settings_warnings,
            decoders: startup.decoders,
            loader,
            thumbs,
            disk_cache,
            router: InputRouter::default(),
            inbox: startup.inbox,
            bg_tx,
            bg_rx,
            index: FolderIndex::default(),
            current: None,
            current_idx: 0,
            history: HistoryStack::new(),
            direction: Direction::Forward,
            nav_root: None,
            scan_seq: 0,
            pending_scan: None,
            neighbors: [
                Neighbor {
                    from: PathBuf::new(),
                    state: NeighborState::Unknown,
                },
                Neighbor {
                    from: PathBuf::new(),
                    state: NeighborState::Unknown,
                },
            ],
            pending_jump: None,
            boundary_armed: None,
            watcher: None,
            watch_flag: Arc::new(AtomicBool::new(false)),
            watch_dirty_since: None,
            cache: ByteLru::new(cache_bytes),
            infos: HashMap::new(),
            errors: HashMap::new(),
            full_failed: HashSet::new(),
            last_wanted: Vec::new(),
            thumb_cache: ByteLru::new(THUMB_BUDGET),
            thumb_failed: HashSet::new(),
            thumb_requests: Vec::new(),
            last_thumb_wanted: Vec::new(),
            view: View::default(),
            viewport_px: [1280.0, 800.0],
            canvas_rect: egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0)),
            anim: None,
            transition: None,
            displayed: None,
            current_since: now,
            last_activity: now,
            hud_hovered: false,
            show_settings: false,
            settings_tab: SettingsTab::General,
            recording: None,
            record_conflict: None,
            record_message: None,
            toasts: Vec::new(),
            skim: None,
            peek: None,
            over_strip: false,
            strip_scroll_to: false,
            slideshow: None,
            slideshow_requested: startup.slideshow.is_some(),
            edit: None,
            crop: None,
            modal: None,
            meta: None,
            meta_pending: None,
            assoc_status: None,
            assoc_busy: false,
            assoc_claimed: Vec::new(),
            assoc_recheck_at: None,
            title: String::new(),
            selftest: crate::selftest::SelfTest::from_env(),
            rng: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0x9E37_79B9_7F4A_7C15)
                | 1,
        };
        for w in app.settings_warnings.clone() {
            tracing::warn!("config: {w}");
        }
        if !app.settings_warnings.is_empty() {
            app.toast(tf(
                "Config file: {0} setting(s) were invalid and reset to defaults",
                &[&app.settings_warnings.len()],
            ));
        }
        if let Some(Some(seconds)) = startup.slideshow {
            app.settings.slideshow.interval_secs = seconds.clamp(0.2, 3600.0);
        }
        if let Some(first) = startup.initial.first() {
            app.open_path(first);
        }
        Ok(app)
    }

    /// The image the main view shows: the peeked thumbnail or the current image.
    pub(crate) fn display_path(&self) -> Option<&Path> {
        self.peek.as_deref().or(self.current.as_deref())
    }

    pub(crate) fn toast(&mut self, text: impl Into<String>) {
        self.toasts.clear();
        self.toasts.push(Toast {
            text: text.into(),
            until: Instant::now() + Duration::from_millis(2200),
        });
        self.ctx.request_repaint();
    }

    /// Run `work` on the thread pool and deliver its result to the UI thread.
    pub(crate) fn spawn_bg(&self, work: impl FnOnce() -> BgEvent + Send + 'static) {
        let tx = self.bg_tx.clone();
        let ctx = self.ctx.clone();
        rayon::spawn(move || {
            if tx.send(work()).is_ok() {
                ctx.request_repaint();
            }
        });
    }

    pub(crate) fn mark_settings_dirty(&mut self) {
        self.settings_dirty = Some(Instant::now());
    }

    fn save_settings_if_due(&mut self, force: bool) {
        let Some(since) = self.settings_dirty else {
            return;
        };
        if !force && since.elapsed() < Duration::from_millis(600) {
            self.ctx.request_repaint_after(Duration::from_millis(650));
            return;
        }
        self.settings_dirty = None;
        if let Some(path) = &self.settings_path {
            if let Err(e) = self.settings.save(path) {
                tracing::error!("could not save settings: {e}");
            }
        }
    }

    pub(crate) fn next_random(&mut self) -> u64 {
        // xorshift64*: plenty for shuffling a slideshow.
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn poll_background(&mut self) {
        while let Ok(paths) = self.inbox.try_recv() {
            if let Some(first) = paths.first() {
                self.open_path(first);
            }
            self.ctx
                .send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            self.ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        while let Ok(event) = self.bg_rx.try_recv() {
            match event {
                BgEvent::Scanned { id, result } => self.on_scanned(id, result),
                BgEvent::Neighbor {
                    from,
                    direction,
                    found,
                } => self.on_neighbor(from, direction, found),
                BgEvent::Meta { path, props } => {
                    if self.meta_pending.as_ref() == Some(&path) {
                        self.meta_pending = None;
                    }
                    if self.display_path() == Some(path.as_path()) {
                        self.meta = Some((path, Arc::new(props)));
                    }
                }
                BgEvent::Edit(ev) => self.on_edit_event(ev),
                BgEvent::Toast(text) => self.toast(text),
                BgEvent::Associations {
                    status,
                    claimed,
                    error,
                } => {
                    self.assoc_status = Some(status);
                    self.assoc_busy = false;
                    if !claimed.is_empty() {
                        // The OS applies a new default a few seconds later: show it
                        // right away and verify afterwards.
                        self.assoc_claimed = claimed;
                        self.assoc_recheck_at = Some(Instant::now() + Duration::from_secs(8));
                    } else if self.assoc_recheck_at.is_none() {
                        self.assoc_claimed.clear();
                    }
                    if let Some(error) = error {
                        self.toast(tf("Could not change the default app: {0}", &[&error]));
                    }
                }
            }
        }
        self.poll_loader();
        self.poll_thumbs();
        self.poll_watcher();
    }

    fn is_typing(&self) -> bool {
        self.ctx.egui_wants_keyboard_input()
    }

    pub(crate) fn stop_slideshow(&mut self) {
        if self.slideshow.take().is_some() {
            self.toast(t("Slideshow stopped"));
        }
    }

    fn toggle_slideshow(&mut self) {
        if self.slideshow.is_some() {
            self.stop_slideshow();
        } else if !self.index.is_empty() {
            let interval = Duration::from_secs_f32(self.settings.slideshow.interval_secs);
            self.slideshow = Some(SlideshowState {
                next_at: Instant::now() + interval,
                paused: false,
                queue: Vec::new(),
            });
            self.toast(t("Slideshow started — Esc to stop"));
        }
    }

    /// Restart the countdown: called whenever the image changes, so stepping
    /// with the arrow keys during a slideshow gives each image its full time.
    pub(crate) fn restart_slideshow_timer(&mut self) {
        let interval = Duration::from_secs_f32(self.settings.slideshow.interval_secs);
        if let Some(show) = &mut self.slideshow {
            show.next_at = Instant::now() + interval;
        }
    }

    pub(crate) fn toggle_slideshow_pause(&mut self) {
        if let Some(show) = &mut self.slideshow {
            show.paused = !show.paused;
        }
        self.restart_slideshow_timer();
    }

    fn tick_slideshow(&mut self) {
        let Some(show) = &self.slideshow else { return };
        if show.paused {
            return;
        }
        let now = Instant::now();
        if now < show.next_at {
            self.ctx.request_repaint_after(show.next_at - now);
            return;
        }
        // Wait for the current image before counting down to the next one.
        if self.transition.is_some() || self.modal.is_some() {
            self.ctx.request_repaint_after(Duration::from_millis(100));
            return;
        }
        let interval = Duration::from_secs_f32(self.settings.slideshow.interval_secs);
        let len = self.index.len();
        if len == 0 {
            self.slideshow = None;
            return;
        }
        if self.settings.slideshow.shuffle {
            let mut queue = self
                .slideshow
                .as_mut()
                .map(|s| std::mem::take(&mut s.queue))
                .unwrap_or_default();
            if queue.is_empty() {
                queue = (0..len).filter(|i| *i != self.current_idx).collect();
                for i in (1..queue.len()).rev() {
                    let j = (self.next_random() % (i as u64 + 1)) as usize;
                    queue.swap(i, j);
                }
            }
            let next = queue.pop();
            let exhausted = queue.is_empty();
            if let Some(s) = &mut self.slideshow {
                s.queue = queue;
                s.next_at = now + interval;
            }
            match next {
                Some(i) if i < len => self.go_index(i, Record::Step),
                _ => self.stop_slideshow(),
            }
            if exhausted && !self.settings.slideshow.looping {
                // Show the last picked image for one interval, then stop.
                if let Some(s) = &mut self.slideshow {
                    s.queue = vec![usize::MAX];
                }
            }
        } else {
            let at_end = self.current_idx + 1 >= len;
            if at_end && !self.settings.slideshow.looping {
                self.stop_slideshow();
                return;
            }
            let next = if at_end { 0 } else { self.current_idx + 1 };
            if let Some(s) = &mut self.slideshow {
                s.next_at = now + interval;
            }
            self.direction = Direction::Forward;
            self.go_index(next, Record::Step);
        }
        self.ctx.request_repaint_after(interval);
    }

    pub(crate) fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    /// Change zoom by a factor around the centre of the view.
    pub(crate) fn zoom_by(&mut self, factor: f32, anchor: Option<[f32; 2]>) {
        let Some(path) = self.current.clone() else {
            return;
        };
        let Some(size) = self.image_size(&path) else {
            return;
        };
        let vp = self.viewport_px;
        let enlarge = self.settings.general.zoom_small_to_fit;
        let old = self.view.scale(size, vp, enlarge);
        let anchor = anchor.unwrap_or([vp[0] / 2.0, vp[1] / 2.0]);
        self.view.zoom_to(old * factor, anchor, size, vp, enlarge);
    }

    fn zoom_actual(&mut self) {
        let Some(path) = self.current.clone() else {
            return;
        };
        let Some(size) = self.image_size(&path) else {
            return;
        };
        let vp = self.viewport_px;
        let enlarge = self.settings.general.zoom_small_to_fit;
        self.view
            .zoom_to(1.0, [vp[0] / 2.0, vp[1] / 2.0], size, vp, enlarge);
    }

    /// Execute one action. This is the only place actions turn into behaviour.
    pub(crate) fn perform(&mut self, action: Action) {
        use Action::*;
        // Any explicit interaction other than Escape/slideshow keeps a running slideshow going only for navigation-free actions.
        match action {
            PrevImage => self.navigate(Direction::Backward),
            NextImage => self.navigate(Direction::Forward),
            PrevFolder => {
                let landing = match self.settings.folders.prev_folder_entry {
                    ff_core::config::FolderEntry::First => Landing::First,
                    ff_core::config::FolderEntry::Last => Landing::Last,
                };
                self.leave_folder(Direction::Backward, landing, false);
            }
            NextFolder => self.leave_folder(Direction::Forward, Landing::First, false),
            FirstImage => {
                self.direction = Direction::Forward;
                self.go_index(0, Record::Jump);
            }
            LastImage => {
                self.direction = Direction::Backward;
                self.go_index(self.index.len().saturating_sub(1), Record::Jump);
            }
            ZoomIn => self.zoom_by(1.25, None),
            ZoomOut => self.zoom_by(0.8, None),
            ZoomFit => self.view = View::default(),
            ZoomActual => self.zoom_actual(),
            ToggleFullscreen => self.set_fullscreen(!self.fullscreen),
            Escape => {
                // Dismiss the innermost thing first.
                let dismissed = self.recording.take().is_some()
                    || self.modal.take().is_some()
                    || self.crop.take().is_some();
                if dismissed {
                } else if self.slideshow.is_some() {
                    self.stop_slideshow();
                } else if self.show_settings {
                    self.show_settings = false;
                } else if self.fullscreen {
                    self.set_fullscreen(false);
                }
            }
            ToggleThumbnails => {
                self.settings.general.show_thumbnails = !self.settings.general.show_thumbnails;
                self.strip_scroll_to = true;
                self.mark_settings_dirty();
            }
            ToggleInfo => {
                self.settings.general.show_info = !self.settings.general.show_info;
                self.mark_settings_dirty();
            }
            HistoryBack => self.history_move(false),
            HistoryForward => self.history_move(true),
            Skim => {}
            PlayPause => {
                // Without an animation on screen, P pauses/resumes a running slideshow.
                if self.anim.is_none() {
                    self.toggle_slideshow_pause();
                }
                if let Some(a) = &mut self.anim {
                    a.player.playing = !a.player.playing;
                    a.last_tick = Instant::now();
                }
            }
            FramePrev | FrameNext => {
                if let Some(a) = &mut self.anim {
                    a.player.step(
                        if action == FrameNext { 1 } else { -1 },
                        a.anim.frame_count(),
                    );
                }
            }
            ReversePlay => {
                if let Some(a) = &mut self.anim {
                    a.player.reverse = !a.player.reverse;
                    a.player.playing = true;
                    a.last_tick = Instant::now();
                }
            }
            RotateLeft => self.edit_op(ff_core::edit::EditOp::RotateLeft),
            RotateRight => self.edit_op(ff_core::edit::EditOp::RotateRight),
            FlipHorizontal => self.edit_op(ff_core::edit::EditOp::FlipHorizontal),
            FlipVertical => self.edit_op(ff_core::edit::EditOp::FlipVertical),
            Crop => self.toggle_crop(),
            Undo => self.edit_undo_redo(true),
            Redo => self.edit_undo_redo(false),
            Save => self.request_save(),
            SaveAs => self.request_save_as(),
            Trash => self.trash_current(),
            Reveal => self.reveal_current(),
            Rename => {
                if let Some(name) = self.current.as_ref().and_then(|p| p.file_name()) {
                    self.modal = Some(Modal::Rename {
                        text: name.to_string_lossy().into_owned(),
                        focus: true,
                    });
                }
            }
            CopyImage => self.copy_image(),
            CopyPath => self.copy_path(),
            OpenFile => self.open_file_dialog(),
            OpenFolder => self.open_folder_dialog(),
            OpenWith => self.open_with_default(),
            Slideshow => self.toggle_slideshow(),
            Settings => self.show_settings = !self.show_settings,
        }
        self.ctx.request_repaint();
    }

    fn update_title(&mut self) {
        let title = match &self.current {
            Some(p) => format!(
                "{} — FlickFlock",
                p.file_name().unwrap_or_default().to_string_lossy()
            ),
            None => "FlickFlock".to_owned(),
        };
        if title != self.title {
            self.title = title.clone();
            self.ctx
                .send_viewport_cmd(egui::ViewportCommand::Title(title));
        }
    }
}

impl eframe::App for FlickApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c = self.background_color();
        [
            f32::from(c.r()) / 255.0,
            f32::from(c.g()) / 255.0,
            f32::from(c.b()) / 255.0,
            1.0,
        ]
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(self.fullscreen));
        self.poll_background();
        self.run_selftest(&ctx);

        // Dropped files / folders.
        let dropped = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .find_map(|f| Some(f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()))
        });
        if let Some(path) = dropped {
            self.open_path(&path);
        }

        // Input → actions.
        let opts = RouterOptions {
            typing: self.is_typing() || self.modal.is_some(),
            recording: self.recording.is_some(),
            record_by_character: self.settings.keys.bind_by_character,
            space_is_peek: self.over_strip,
        };
        let out = self
            .router
            .process(&ctx, &self.settings.bindings, &self.settings.keys, &opts);
        if out.record_cancelled {
            self.recording = None;
        }
        if let Some(binding) = out.recorded.clone() {
            self.on_binding_recorded(binding);
        }
        for action in &out.actions {
            // With a modal open only Escape acts (it closes the modal).
            if self.modal.is_some() && *action != Action::Escape {
                continue;
            }
            self.perform(*action);
        }

        // Pointer activity keeps the HUD visible.
        let (moved, in_window) = ctx.input(|i| {
            (
                i.pointer.delta() != egui::Vec2::ZERO || i.pointer.any_pressed(),
                i.pointer.has_pointer(),
            )
        });
        if moved {
            self.last_activity = Instant::now();
        }
        let scripted = self.selftest.as_ref().map(|t| t.keep_hud).unwrap_or(false);
        let hud_visible = (in_window || scripted)
            && (self.last_activity.elapsed() < HUD_TIMEOUT || self.hud_hovered);

        if self.slideshow_requested && self.current.is_some() {
            self.slideshow_requested = false;
            if self.slideshow.is_none() {
                self.toggle_slideshow();
            }
        }
        self.tick_slideshow();
        self.tick_animation();

        self.thumb_requests.clear();
        self.over_strip = false;
        if self.settings.general.show_thumbnails && !self.index.is_empty() {
            self.ui_strip(ui, &out);
        }
        if self.settings.general.show_info {
            self.ui_info(ui);
        }
        self.ui_canvas(ui, &out);
        self.ui_hud(&ctx, hud_visible);
        self.ui_settings(&ctx);
        self.ui_modal(&ctx);
        self.ui_toasts(&ctx);

        self.update_wanted();
        self.request_metadata_if_needed();
        self.update_title();
        self.save_settings_if_due(false);

        if hud_visible && !self.hud_hovered {
            // Wake up once to fade the HUD out.
            ctx.request_repaint_after(
                HUD_TIMEOUT.saturating_sub(self.last_activity.elapsed())
                    + Duration::from_millis(20),
            );
        }
    }

    fn on_exit(&mut self) {
        self.save_settings_if_due(true);
    }
}
