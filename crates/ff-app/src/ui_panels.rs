//! Side panel (metadata), the Settings window and modal dialogs.

use crate::app::{FlickApp, Modal, SettingsTab};
use crate::ui_strip::{MAX_THUMB, MIN_THUMB};
use eframe::egui::{self, Color32, RichText};
use ff_core::config::{Background, FolderEntry, ScrollMode, StripPosition, TRANSITIONS};
use ff_core::edit::{EditOp, SaveFormat};
use ff_core::folder::{GroupKey, SortKey};
use ff_core::input::{Action, BindError, Binding, KeyBindings};
use ff_core::nav::{EndOfFolder, FolderSort};
use ff_core::IS_MAC;

fn transition_label(name: &str) -> String {
    let mut label = name.replace('_', " ");
    if let Some(first) = label.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    label
}

impl FlickApp {
    // ----- Info panel -----------------------------------------------------

    pub(crate) fn ui_info(&mut self, ui: &mut egui::Ui) {
        egui::Panel::right("ff-info")
            .default_size(300.0)
            .size_range(220.0..=520.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading("Info");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("×").clicked() {
                            self.perform(Action::ToggleInfo);
                        }
                    });
                });
                ui.label(
                    RichText::new("📌 pins a property onto the image")
                        .weak()
                        .small(),
                );
                ui.separator();
                let Some((path, props)) = self.meta.clone() else {
                    if self.display_path().is_some() {
                        ui.add(egui::Spinner::new());
                    } else {
                        ui.weak("No image");
                    }
                    return;
                };
                if self.display_path() != Some(path.as_path()) {
                    ui.add(egui::Spinner::new());
                    return;
                }
                let mut toggled: Option<String> = None;
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        for group in ["File", "Image", "EXIF", "GPS", "XMP"] {
                            let rows: Vec<_> = props.iter().filter(|p| p.group == group).collect();
                            if rows.is_empty() {
                                continue;
                            }
                            egui::CollapsingHeader::new(RichText::new(group).strong())
                                .default_open(true)
                                .show(ui, |ui| {
                                    egui::Grid::new(("ff-info-grid", group))
                                        .num_columns(3)
                                        .spacing([6.0, 4.0])
                                        .show(ui, |ui| {
                                            for p in rows {
                                                let key = p.key();
                                                let pinned =
                                                    self.settings.metadata.pinned.contains(&key);
                                                let pin = egui::Button::new(
                                                    RichText::new("📌").size(11.0),
                                                )
                                                .frame(false)
                                                .selected(pinned);
                                                if ui
                                                    .add(pin)
                                                    .on_hover_text(if pinned {
                                                        "Unpin"
                                                    } else {
                                                        "Pin to image"
                                                    })
                                                    .clicked()
                                                {
                                                    toggled = Some(key);
                                                }
                                                ui.label(RichText::new(&p.name).weak());
                                                let value = egui::Label::new(&p.value)
                                                    .wrap()
                                                    .sense(egui::Sense::click());
                                                if ui
                                                    .add(value)
                                                    .on_hover_text("Click to copy")
                                                    .clicked()
                                                {
                                                    ui.ctx().copy_text(p.value.clone());
                                                }
                                                ui.end_row();
                                            }
                                        });
                                });
                        }
                    });
                if let Some(key) = toggled {
                    let pins = &mut self.settings.metadata.pinned;
                    match pins.iter().position(|k| *k == key) {
                        Some(i) => {
                            pins.remove(i);
                        }
                        None => pins.push(key),
                    }
                    self.mark_settings_dirty();
                }
            });
    }

    // ----- Key recorder ---------------------------------------------------

    pub(crate) fn on_binding_recorded(&mut self, binding: Binding) {
        let Some(action) = self.recording.take() else {
            return;
        };
        self.record_message = None;
        match self.settings.bindings.add(action, binding.clone(), IS_MAC) {
            Ok(()) => self.mark_settings_dirty(),
            Err(BindError::Reserved) => {
                self.record_message = Some(format!(
                    "{} is reserved by the system and cannot be used.",
                    binding.display(IS_MAC)
                ));
            }
            Err(BindError::Conflict(other)) => {
                self.record_conflict = Some((action, binding, other))
            }
        }
    }

    // ----- Settings -------------------------------------------------------

    pub(crate) fn ui_settings(&mut self, ctx: &egui::Context) {
        if !self.show_settings {
            self.recording = None;
            return;
        }
        let mut open = true;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .default_size([560.0, 480.0])
            .min_width(480.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SettingsTab::General, "General"),
                        (SettingsTab::Keys, "Keys"),
                        (SettingsTab::Folders, "Folders"),
                        (SettingsTab::Performance, "Performance"),
                        (SettingsTab::Transitions, "Transitions"),
                        (SettingsTab::Sorting, "Sorting"),
                    ] {
                        if ui
                            .selectable_label(self.settings_tab == tab, label)
                            .clicked()
                        {
                            self.settings_tab = tab;
                            self.recording = None;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, true])
                    .show(ui, |ui| match self.settings_tab {
                        SettingsTab::General => self.settings_general(ui),
                        SettingsTab::Keys => self.settings_keys(ui),
                        SettingsTab::Folders => self.settings_folders(ui),
                        SettingsTab::Performance => self.settings_performance(ui),
                        SettingsTab::Transitions => self.settings_transitions(ui),
                        SettingsTab::Sorting => self.settings_sorting(ui),
                    });
                if let Some(path) = &self.settings_path {
                    ui.separator();
                    ui.label(
                        RichText::new(format!("Stored in {}", path.display()))
                            .weak()
                            .small(),
                    );
                }
            });
        if !open {
            self.show_settings = false;
            self.recording = None;
        }
    }

    fn settings_general(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let g = &mut self.settings.general;
        egui::Grid::new("ff-set-general")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("At the end of a folder");
                ui.vertical(|ui| {
                    changed |= ui
                        .radio_value(
                            &mut g.end_of_folder,
                            EndOfFolder::Continue,
                            "Continue into the next folder",
                        )
                        .changed();
                    changed |= ui
                        .radio_value(&mut g.end_of_folder, EndOfFolder::Wrap, "Wrap around")
                        .changed();
                    changed |= ui
                        .radio_value(&mut g.end_of_folder, EndOfFolder::Stop, "Stop")
                        .changed();
                });
                ui.end_row();

                ui.label("Background");
                egui::ComboBox::from_id_salt("ff-bg")
                    .selected_text(background_label(g.background))
                    .show_ui(ui, |ui| {
                        for b in [
                            Background::Black,
                            Background::DarkGray,
                            Background::Checkerboard,
                            Background::System,
                        ] {
                            changed |= ui
                                .selectable_value(&mut g.background, b, background_label(b))
                                .changed();
                        }
                    });
                ui.end_row();

                ui.label("Scroll wheel");
                ui.horizontal(|ui| {
                    changed |= ui
                        .radio_value(&mut g.scroll, ScrollMode::Zoom, "Zooms")
                        .changed();
                    changed |= ui
                        .radio_value(&mut g.scroll, ScrollMode::Navigate, "Changes image")
                        .changed();
                });
                ui.end_row();

                ui.label("Zoom");
                ui.vertical(|ui| {
                    changed |= ui
                        .checkbox(
                            &mut g.zoom_small_to_fit,
                            "Zoom small images to fit the window",
                        )
                        .changed();
                    changed |= ui
                        .checkbox(
                            &mut g.pixelated_zoom,
                            "Show sharp pixels above 200% (pixel art)",
                        )
                        .changed();
                });
                ui.end_row();

                ui.label("Windows");
                ui.vertical(|ui| {
                    changed |= ui
                        .checkbox(&mut g.single_window, "Open files in the existing window")
                        .changed();
                    if !IS_MAC
                        && ui
                            .checkbox(&mut g.borderless, "Borderless window (no title bar)")
                            .changed()
                    {
                        changed = true;
                        ui.ctx()
                            .send_viewport_cmd(egui::ViewportCommand::Decorations(!g.borderless));
                    }
                });
                ui.end_row();

                ui.label("Thumbnails");
                ui.vertical(|ui| {
                    ui.horizontal(|ui| {
                        changed |= ui
                            .radio_value(&mut g.strip_position, StripPosition::Bottom, "Bottom")
                            .changed();
                        changed |= ui
                            .radio_value(&mut g.strip_position, StripPosition::Top, "Top")
                            .changed();
                    });
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut g.thumbnail_size, MIN_THUMB..=MAX_THUMB)
                                .text("size"),
                        )
                        .changed();
                });
                ui.end_row();

                ui.label("Saving");
                changed |= ui
                    .checkbox(
                        &mut g.confirm_overwrite,
                        "Ask before saving over the original",
                    )
                    .changed();
                ui.end_row();
            });
        if changed {
            self.mark_settings_dirty();
        }
    }

    fn settings_keys(&mut self, ui: &mut egui::Ui) {
        ui.label("Navigation keys. Each action can have several bindings.");
        ui.add_space(6.0);
        let mut remove: Option<(Action, Binding)> = None;
        let mut start: Option<Action> = None;
        egui::Grid::new("ff-set-keys")
            .num_columns(2)
            .spacing([16.0, 8.0])
            .striped(true)
            .show(ui, |ui| {
                for action in Action::NAVIGATION {
                    ui.label(action.label());
                    ui.horizontal_wrapped(|ui| {
                        for binding in self.settings.bindings.bindings(action).to_vec() {
                            let chip = egui::Button::new(format!("{}  ×", binding.display(IS_MAC)))
                                .corner_radius(10.0);
                            if ui.add(chip).on_hover_text("Remove this binding").clicked() {
                                remove = Some((action, binding));
                            }
                        }
                        if self.recording == Some(action) {
                            ui.label(
                                RichText::new(
                                    "Press a key, mouse button 4/5 or swipe… (Esc cancels)",
                                )
                                .color(Color32::from_rgb(120, 200, 255)),
                            );
                            ui.ctx().request_repaint();
                        } else if ui.button("+").on_hover_text("Add a binding").clicked() {
                            start = Some(action);
                        }
                    });
                    ui.end_row();
                }
            });
        if let Some((action, binding)) = remove {
            self.settings.bindings.remove(action, &binding);
            self.mark_settings_dirty();
        }
        if let Some(action) = start {
            self.recording = Some(action);
            self.record_conflict = None;
            self.record_message = None;
        }
        if let Some(message) = &self.record_message {
            ui.colored_label(Color32::from_rgb(255, 170, 90), message);
        }
        if let Some((action, binding, other)) = self.record_conflict.clone() {
            ui.add_space(4.0);
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.label(format!(
                    "{} is already used for “{}”.",
                    binding.display(IS_MAC),
                    other.label()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(format!("Use it for “{}” instead", action.label()))
                        .clicked()
                    {
                        if self
                            .settings
                            .bindings
                            .reassign(action, binding.clone(), IS_MAC)
                            .is_ok()
                        {
                            self.mark_settings_dirty();
                        }
                        self.record_conflict = None;
                    }
                    if ui.button("Keep as is").clicked() {
                        self.record_conflict = None;
                    }
                });
            });
        }

        ui.add_space(10.0);
        ui.separator();
        let mut changed = false;
        let k = &mut self.settings.keys;
        changed |= ui
            .checkbox(&mut k.bind_by_character, "Bind new keys by the character they type (instead of key position)")
            .on_hover_text("Key position keeps the same physical keys working on QWERTY, AZERTY, JIS, … layouts.")
            .changed();
        changed |= ui
            .checkbox(&mut k.key_repeat, "Holding a key flips continuously")
            .changed();
        ui.add_enabled_ui(k.key_repeat, |ui| {
            changed |= ui
                .add(egui::Slider::new(&mut k.repeat_rate, 1.0..=60.0).text("images per second"))
                .changed();
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Restore defaults").clicked() {
                self.settings.bindings.restore_navigation_defaults(IS_MAC);
                self.record_conflict = None;
                changed = true;
            }
            if ui.button("Export…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .set_file_name("flickflock-keys.toml")
                    .add_filter("TOML", &["toml"])
                    .save_file()
                {
                    if let Err(e) = std::fs::write(&path, self.settings.bindings.export_toml()) {
                        self.record_message = Some(format!("Could not export: {e}"));
                    }
                }
            }
            if ui.button("Import…").clicked() {
                if let Some(path) = rfd::FileDialog::new()
                    .add_filter("TOML", &["toml"])
                    .pick_file()
                {
                    let imported = std::fs::read_to_string(&path)
                        .map_err(|e| e.to_string())
                        .and_then(|text| KeyBindings::import_toml(&text, IS_MAC));
                    match imported {
                        Ok((bindings, warnings)) => {
                            self.settings.bindings = bindings;
                            self.record_message = (!warnings.is_empty()).then(|| {
                                format!("Imported with {} ignored entr(y/ies).", warnings.len())
                            });
                            changed = true;
                        }
                        Err(e) => self.record_message = Some(format!("Could not import: {e}")),
                    }
                }
            }
        });
        if changed {
            self.mark_settings_dirty();
        }
    }

    fn settings_folders(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let mut nav_changed = false;
        let f = &mut self.settings.folders;
        ui.label("Next / previous folder moves through:");
        nav_changed |= ui
            .radio_value(&mut f.traverse_tree, false, "Sibling folders (same parent)")
            .changed();
        nav_changed |= ui
            .radio_value(
                &mut f.traverse_tree,
                true,
                "The whole folder tree, depth-first",
            )
            .changed();
        ui.add_enabled_ui(f.traverse_tree, |ui| {
            ui.horizontal(|ui| {
                ui.label("Tree root:");
                match &f.tree_root {
                    Some(root) => ui.label(root.to_string_lossy()),
                    None => ui.weak("parent of the first opened folder"),
                };
                if ui.button("Choose…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        f.tree_root = Some(dir);
                        nav_changed = true;
                    }
                }
                if f.tree_root.is_some() && ui.button("Reset").clicked() {
                    f.tree_root = None;
                    nav_changed = true;
                }
            });
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Folder order:");
            nav_changed |= ui
                .radio_value(&mut f.sort, FolderSort::Name, "Name")
                .changed();
            nav_changed |= ui
                .radio_value(&mut f.sort, FolderSort::Date, "Date modified")
                .changed();
        });
        ui.horizontal(|ui| {
            ui.label("“Previous folder” opens its:");
            changed |= ui
                .radio_value(&mut f.prev_folder_entry, FolderEntry::First, "First image")
                .changed();
            changed |= ui
                .radio_value(&mut f.prev_folder_entry, FolderEntry::Last, "Last image")
                .changed();
        });
        changed |= ui
            .checkbox(
                &mut f.boundary_pause,
                "Pause at folder boundaries (press again to continue)",
            )
            .changed();
        if nav_changed {
            if let Some(root) = self.settings.folders.tree_root.clone() {
                self.nav_root = Some(root);
            }
            self.reset_neighbors();
        }
        if changed || nav_changed {
            self.mark_settings_dirty();
        }
    }

    fn settings_performance(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let p = &mut self.settings.performance;
        changed |= ui
            .add(
                egui::Slider::new(&mut p.cache_mb, 64..=8192)
                    .logarithmic(true)
                    .text("MB image cache"),
            )
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut p.prefetch_ahead, 0..=8).text("images preloaded ahead"))
            .changed();
        changed |= ui
            .add(egui::Slider::new(&mut p.prefetch_behind, 0..=8).text("images preloaded behind"))
            .changed();
        ui.label(
            RichText::new(format!(
                "In use: {} MB in {} images",
                self.cache.used() >> 20,
                self.cache.len()
            ))
            .weak(),
        );
        ui.add_space(8.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label("Thumbnail cache on disk");
            if ui.button("Clear").clicked() {
                if let Some(cache) = self.disk_cache.clone() {
                    self.spawn_bg(move || {
                        let freed = cache.clear();
                        crate::app::BgEvent::Toast(format!(
                            "Freed {}",
                            ff_core::meta::human_size(freed)
                        ))
                    });
                }
            }
        });
        if let Some(cache) = &self.disk_cache {
            ui.label(RichText::new(cache.dir().to_string_lossy()).weak().small());
        }
        if changed {
            let budget = self.settings.performance.cache_mb as usize * (1 << 20);
            let pinned = self.display_path().map(std::path::Path::to_path_buf);
            self.cache
                .set_budget(budget, |k| Some(&k.0) == pinned.as_ref());
            self.last_wanted.clear();
            self.mark_settings_dirty();
        }
    }

    fn settings_transitions(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let t = &mut self.settings.transitions;
        egui::Grid::new("ff-set-transitions")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                for (label, value, id) in [
                    ("While browsing", &mut t.browse, "ff-tr-browse"),
                    ("In slideshows", &mut t.slideshow, "ff-tr-show"),
                ] {
                    ui.label(label);
                    egui::ComboBox::from_id_salt(id)
                        .selected_text(transition_label(value))
                        .show_ui(ui, |ui| {
                            for name in TRANSITIONS {
                                changed |= ui
                                    .selectable_value(
                                        value,
                                        name.to_owned(),
                                        transition_label(name),
                                    )
                                    .changed();
                            }
                        });
                    ui.end_row();
                }
                ui.label("Duration");
                changed |= ui
                    .add(egui::Slider::new(&mut t.duration_ms, 50..=3000).suffix(" ms"))
                    .changed();
                ui.end_row();
            });
        ui.add_space(8.0);
        ui.separator();
        ui.strong("Slideshow");
        let s = &mut self.settings.slideshow;
        changed |= ui
            .add(
                egui::Slider::new(&mut s.interval_secs, 0.5..=60.0)
                    .logarithmic(true)
                    .suffix(" s")
                    .text("per image"),
            )
            .changed();
        changed |= ui.checkbox(&mut s.shuffle, "Shuffle").changed();
        changed |= ui.checkbox(&mut s.looping, "Loop").changed();
        if changed {
            self.mark_settings_dirty();
        }
    }

    fn settings_sorting(&mut self, ui: &mut egui::Ui) {
        let mut changed = false;
        let s = &mut self.settings.sorting;
        egui::Grid::new("ff-set-sorting")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                ui.label("Sort by");
                egui::ComboBox::from_id_salt("ff-sort-key")
                    .selected_text(s.key.label())
                    .show_ui(ui, |ui| {
                        for key in SortKey::ALL {
                            changed |= ui.selectable_value(&mut s.key, key, key.label()).changed();
                        }
                    });
                ui.end_row();
                ui.label("Order");
                ui.horizontal(|ui| {
                    changed |= ui
                        .radio_value(&mut s.descending, false, "Ascending")
                        .changed();
                    changed |= ui
                        .radio_value(&mut s.descending, true, "Descending")
                        .changed();
                });
                ui.end_row();
                for level in 0..2 {
                    ui.label(if level == 0 { "Group by" } else { "Then by" });
                    let current = s.group_by.get(level).copied();
                    let text = current.map(GroupKey::label).unwrap_or("None");
                    let mut pick: Option<Option<GroupKey>> = None;
                    egui::ComboBox::from_id_salt(("ff-group", level))
                        .selected_text(text)
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(current.is_none(), "None").clicked() {
                                pick = Some(None);
                            }
                            for key in GroupKey::ALL {
                                if ui
                                    .selectable_label(current == Some(key), key.label())
                                    .clicked()
                                {
                                    pick = Some(Some(key));
                                }
                            }
                        });
                    if let Some(choice) = pick {
                        match choice {
                            None => s.group_by.truncate(level),
                            Some(key) => {
                                s.group_by.retain(|g| *g != key);
                                if level < s.group_by.len() {
                                    s.group_by[level] = key;
                                } else {
                                    s.group_by.push(key);
                                }
                            }
                        }
                        changed = true;
                    }
                    ui.end_row();
                    if s.group_by.len() <= level {
                        break;
                    }
                }
                ui.label("Subfolders");
                changed |= ui
                    .checkbox(&mut s.recursive, "Include images in subfolders")
                    .changed();
                ui.end_row();
            });
        if changed {
            self.resort();
        }
    }

    // ----- Modals ---------------------------------------------------------

    pub(crate) fn ui_modal(&mut self, ctx: &egui::Context) {
        let Some(mut modal) = self.modal.take() else {
            return;
        };
        let mut keep = true;
        // What to do once the dialog closure has finished borrowing.
        enum Outcome {
            None,
            Rename(String),
            Overwrite,
            SaveThenClose,
            Discard,
            SaveAs(SaveFormat, u8),
            Extend([u32; 4], [u8; 3]),
        }
        let mut outcome = Outcome::None;
        let response = egui::Modal::new(egui::Id::unique("ff-modal")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            match &mut modal {
                Modal::Rename { text, focus } => {
                    ui.heading("Rename");
                    let edit = ui.add(egui::TextEdit::singleline(text).desired_width(300.0));
                    if std::mem::take(focus) {
                        edit.request_focus();
                    }
                    let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Rename").clicked() || enter {
                            outcome = Outcome::Rename(text.clone());
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::ConfirmOverwrite => {
                    ui.heading("Save over the original?");
                    ui.label("The original file will be replaced. This cannot be undone.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            outcome = Outcome::Overwrite;
                            keep = false;
                        }
                        if ui.button("Save As…").clicked() {
                            outcome = Outcome::SaveAs(SaveFormat::Jpeg, 0);
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Unsaved => {
                    ui.heading("Unsaved changes");
                    ui.label("This image has edits that are not saved yet.");
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            outcome = Outcome::SaveThenClose;
                            keep = false;
                        }
                        if ui.button("Discard").clicked() {
                            outcome = Outcome::Discard;
                            keep = false;
                        }
                        if ui.button("Keep editing").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::SaveAs { format, quality } => {
                    ui.heading("Save As");
                    egui::Grid::new("ff-saveas")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Format");
                            egui::ComboBox::from_id_salt("ff-saveas-format")
                                .selected_text(format.label())
                                .show_ui(ui, |ui| {
                                    for f in SaveFormat::ALL {
                                        ui.selectable_value(format, f, f.label());
                                    }
                                });
                            ui.end_row();
                            if *format == SaveFormat::Jpeg {
                                ui.label("JPEG quality");
                                ui.add(egui::Slider::new(quality, 1..=100));
                                ui.end_row();
                            }
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Choose location…").clicked() {
                            outcome = Outcome::SaveAs(*format, *quality);
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Extend { margins, color } => {
                    ui.heading("Extend canvas");
                    ui.label("Add borders around the image (pixels).");
                    egui::Grid::new("ff-extend")
                        .num_columns(4)
                        .spacing([10.0, 8.0])
                        .show(ui, |ui| {
                            for (i, label) in
                                ["Left", "Top", "Right", "Bottom"].into_iter().enumerate()
                            {
                                ui.label(label);
                                ui.add(egui::DragValue::new(&mut margins[i]).range(0..=10_000));
                                if i == 1 {
                                    ui.end_row();
                                }
                            }
                            ui.end_row();
                            ui.label("Colour");
                            ui.color_edit_button_srgb(color);
                            ui.end_row();
                        });
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Apply").clicked() {
                            outcome = Outcome::Extend(*margins, *color);
                            keep = false;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                }
                Modal::Message { title, body } => {
                    ui.heading(title.as_str());
                    ui.label(body.as_str());
                    ui.add_space(8.0);
                    if ui.button("OK").clicked() {
                        keep = false;
                    }
                }
            }
        });
        if response.should_close() {
            keep = false;
        }
        if keep {
            self.modal = Some(modal);
        }
        match outcome {
            Outcome::None => {}
            Outcome::Rename(name) => self.rename_current(&name),
            Outcome::Overwrite | Outcome::SaveThenClose => self.save_over_original_or_as(),
            Outcome::Discard => self.discard_edits(),
            // Quality 0 = "ask for format first".
            Outcome::SaveAs(_, 0) => self.request_save_as(),
            Outcome::SaveAs(format, quality) => self.save_as_dialog(format, quality),
            Outcome::Extend(m, c) => self.edit_op(EditOp::Extend {
                left: m[0],
                top: m[1],
                right: m[2],
                bottom: m[3],
                color: [c[0], c[1], c[2], 255],
            }),
        }
    }

    fn save_over_original_or_as(&mut self) {
        let writable = self
            .edit
            .as_ref()
            .map(|e| SaveFormat::from_path(&e.path).is_some())
            .unwrap_or(false);
        if writable {
            self.save_over_original();
        } else {
            self.request_save_as();
        }
    }
}

fn background_label(b: Background) -> &'static str {
    match b {
        Background::Black => "Black",
        Background::DarkGray => "Dark gray",
        Background::Checkerboard => "Checkerboard (for transparency)",
        Background::System => "Follow system theme",
    }
}
