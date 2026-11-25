//! A scripted run for automated smoke tests and debugging, enabled with the
//! `FLICKFLOCK_SELFTEST` environment variable. Steps are separated by `;`:
//!
//! * an action name (`next_image`, `toggle_thumbnails`, …)
//! * `ready` – wait until the current image is on screen
//! * `wait:<ms>`, `hud`, `zoom:<factor>`, `pan:<dx>,<dy>`, `skim:<0..1>`, `open:<path>`, `tab:<settings tab>`
//! * `lang:<code>` – switch the interface language (`auto`, `tr`, `ja`, …)
//! * `claim:<n>` – make FlickFlock the default app for file type group `n`; `assoc` prints the status
//! * `toast:<text>` – show a toast (handy for checking font coverage)
//! * `state` – print the browsing state to stdout
//! * `shot:<file.png>` – save a screenshot of the window
//! * `quit`

use crate::app::{FlickApp, SettingsTab};
use eframe::egui;
use ff_core::input::Action;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) struct SelfTest {
    steps: VecDeque<String>,
    resume_at: Instant,
    ready_deadline: Option<Instant>,
    pending_shot: Option<Arc<AtomicBool>>,
    /// Keeps the HUD visible (the pointer never moves in a scripted run).
    pub keep_hud: bool,
}

impl SelfTest {
    pub(crate) fn from_env() -> Option<Self> {
        let script = std::env::var("FLICKFLOCK_SELFTEST").ok()?;
        Some(Self {
            steps: script
                .split(';')
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())
                .collect(),
            resume_at: Instant::now(),
            ready_deadline: None,
            pending_shot: None,
            keep_hud: false,
        })
    }
}

impl FlickApp {
    pub(crate) fn run_selftest(&mut self, ctx: &egui::Context) {
        let Some(mut test) = self.selftest.take() else {
            return;
        };
        ctx.request_repaint_after(Duration::from_millis(30));

        // A requested screenshot is delivered through a callback a frame or two later.
        if let Some(done) = &test.pending_shot {
            if !done.load(Ordering::SeqCst) {
                self.selftest = Some(test);
                return;
            }
            test.pending_shot = None;
        }

        while Instant::now() >= test.resume_at {
            let Some(step) = test.steps.front().cloned() else {
                break;
            };
            let (name, arg) = step.split_once(':').unwrap_or((step.as_str(), ""));
            match name {
                "ready" => {
                    let deadline = *test.ready_deadline.get_or_insert(Instant::now() + Duration::from_secs(20));
                    let shown = self.displayed.is_some() && self.transition.is_none();
                    let failed = self.display_path().map(|p| self.errors.contains_key(p)).unwrap_or(false);
                    let busy = self.pending_scan.is_some() || self.pending_jump.is_some() || self.edit.as_ref().map(|e| e.busy).unwrap_or(false);
                    if (!(shown || failed) || busy) && Instant::now() < deadline {
                        break;
                    }
                    test.ready_deadline = None;
                }
                "wait" => test.resume_at = Instant::now() + Duration::from_millis(arg.parse().unwrap_or(100)),
                "hud" => test.keep_hud = arg != "off",
                "zoom" => self.zoom_by(arg.parse().unwrap_or(1.0), None),
                "pan" => {
                    if let Some((x, y)) = arg.split_once(',') {
                        self.view.pan[0] += x.parse().unwrap_or(0.0);
                        self.view.pan[1] += y.parse().unwrap_or(0.0);
                    }
                }
                "lang" => {
                    self.settings.general.language = arg.to_owned();
                    crate::apply_language(arg);
                }
                "claim" => self.refresh_associations(arg.parse().ok().map(crate::app::AssocOp::Claim).into_iter().collect()),
                "restore" => self.refresh_associations(arg.parse().ok().map(crate::app::AssocOp::Restore).into_iter().collect()),
                "assoc" => println!("selftest: assoc {:?}", self.assoc_status),
                "toast" => self.toast(arg.to_owned()),
                "skim" => self.skim_to(arg.parse().unwrap_or(0.0)),
                "endskim" => self.end_skim(),
                "open" => self.open_path(Path::new(arg)),
                "tab" => {
                    self.show_settings = true;
                    self.settings_tab = match arg {
                        "keys" => SettingsTab::Keys,
                        "folders" => SettingsTab::Folders,
                        "performance" => SettingsTab::Performance,
                        "transitions" => SettingsTab::Transitions,
                        "sorting" => SettingsTab::Sorting,
                        "filetypes" => SettingsTab::FileTypes,
                        _ => SettingsTab::General,
                    };
                }
                "state" => println!(
                    "selftest: state current={:?} index={}/{} dir={:?} zoom={:?} anim_frames={:?} error={:?} toast={:?}",
                    self.current.as_ref().and_then(|p| p.file_name()),
                    self.current_idx + 1,
                    self.index.len(),
                    self.index.dir.file_name(),
                    self.zoom_percent().map(|z| z.round()),
                    self.anim.as_ref().map(|a| a.anim.frame_count()),
                    self.display_path().and_then(|p| self.errors.get(p)),
                    self.toasts.last().map(|t| t.text.as_str()),
                ),
                "modal" => match &self.modal {
                    Some(crate::app::Modal::Message { title, body }) => println!("selftest: modal {title}: {body}"),
                    Some(_) => println!("selftest: modal (dialog open)"),
                    None => println!("selftest: modal none"),
                },
                "shot" => {
                    let done = Arc::new(AtomicBool::new(false));
                    test.pending_shot = Some(done.clone());
                    let path = PathBuf::from(arg);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::ScreenshotCallback::new(move |image| {
                        let [w, h] = image.size;
                        let bytes: Vec<u8> = image.pixels.iter().flat_map(|c| c.to_array()).collect();
                        match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|img| img.save(&path)) {
                            Some(Ok(())) => println!("selftest: saved {}", path.display()),
                            other => println!("selftest: could not save {}: {other:?}", path.display()),
                        }
                        done.store(true, Ordering::SeqCst);
                    })));
                    test.steps.pop_front();
                    break;
                }
                "quit" => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                other => match Action::from_name(other) {
                    Some(action) => self.perform(action),
                    None => println!("selftest: unknown step {other:?}"),
                },
            }
            test.steps.pop_front();
        }
        if test.keep_hud {
            self.last_activity = Instant::now();
        }
        // Script finished: stop driving repaints so the app goes idle again.
        if test.steps.is_empty() && test.pending_shot.is_none() {
            return;
        }
        self.selftest = Some(test);
    }
}
