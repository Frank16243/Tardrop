//! The TarDrop desktop interface.
//!
//! This module intentionally owns presentation only. Archive handling remains in the installer,
//! allowing the window to stay polished without weakening the security boundary.

use std::{
    collections::VecDeque,
    path::PathBuf,
    process::Command,
    sync::mpsc::{self, Receiver},
};

use eframe::egui::{self, Align, Color32, CornerRadius, Frame, Layout, Margin, RichText, Sense, Stroke, Vec2};

use crate::{
    installer::{self, ExistingChoice, InstallResult, InstalledApp, LauncherCandidate},
    updates::{self, InstalledDatabase, InstalledRecord, ReleaseInfo, UpdateInterval, UpdateSettings},
    utils,
};

const ACCENT: Color32 = Color32::from_rgb(61, 174, 233);
const SUCCESS: Color32 = Color32::from_rgb(39, 174, 96);

/// Top-level pages keep installation, management, updates, and preferences discoverable.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Page { Install, Installed, Updates, Settings }

/// UI-owned state. Archive work is performed in a worker so the window remains interactive.
pub struct TarDropApp {
    queue: VecDeque<PathBuf>,
    receiver: Option<Receiver<WorkResult>>,
    current: Option<PathBuf>,
    current_choice: Option<ExistingChoice>,
    log: Vec<String>,
    installed: Vec<InstalledApp>,
    message: Option<(bool, String)>,
    replace_prompt: Option<PathBuf>,
    launcher_prompt: Option<(PathBuf, ExistingChoice, Vec<LauncherCandidate>)>,
    page: Page,
    records: Vec<InstalledRecord>,
    settings: UpdateSettings,
    update_receiver: Option<Receiver<UpdateWorkResult>>,
    update_busy: Option<String>,
}

/// Result sent from the install worker back to the single UI thread.
struct WorkResult {
    result: anyhow::Result<InstallResult>,
    log: Vec<String>,
}

/// Completion signal for network checks and update transactions performed off the UI thread.
enum UpdateWorkResult { Checked(Result<(InstalledRecord, Option<ReleaseInfo>), String>), Updated(Result<InstalledRecord, String>) }

impl TarDropApp {
    /// Builds an empty window ready for normal file selection and system drag-and-drop.
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_visuals(&cc.egui_ctx);
        let records = InstalledDatabase::load().unwrap_or_default();
        let settings = InstalledDatabase::load_settings().unwrap_or_default();
        let mut app = Self {
            queue: VecDeque::new(),
            receiver: None,
            current: None,
            current_choice: None,
            log: vec!["Ready. Drop a portable archive to install it safely.".into()],
            installed: Vec::new(),
            message: None,
            replace_prompt: None,
            launcher_prompt: None,
            page: Page::Install,
            records,
            settings,
            update_receiver: None,
            update_busy: None,
        };
        // Startup checks are opt-in and skip manual records, so opening TarDrop never contacts a
        // network service unless the user enabled automatic checking and configured a provider.
        if app.settings.check_automatically && app.settings.check_on_startup {
            if let Some(record) = app.records.iter().find(|record| record.update_provider != updates::ProviderKind::Manual && startup_check_due(record, app.settings.interval)).cloned() { app.check_record(record); }
        }
        app
    }

    /// Adds supported files to the sequential queue; unsuitable files receive a friendly error.
    fn enqueue(&mut self, files: impl IntoIterator<Item = PathBuf>) {
        for file in files {
            match crate::archive::detect(&file) {
                Ok(_) => self.queue.push_back(file),
                Err(error) => self.message = Some((false, format!("{}: {error}", file.display()))),
            }
        }
        self.start_next();
    }

    /// Opens the portal/native file picker with the archive extensions TarDrop understands.
    fn choose_archives(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .set_title("Choose portable application archives")
            .add_filter("Portable archives", &["tar", "gz", "tgz", "xz", "bz2", "zip"])
            .pick_files()
        {
            self.enqueue(files);
        }
    }

    /// Starts one queued archive only after the prior task or decision dialog has resolved.
    fn start_next(&mut self) {
        if self.receiver.is_some() || self.replace_prompt.is_some() || self.launcher_prompt.is_some() {
            return;
        }
        let Some(path) = self.queue.pop_front() else { return; };
        let likely_target = utils::applications_dir().ok().map(|root| root.join(utils::archive_stem(&path)));
        if likely_target.as_ref().is_some_and(|target| target.exists()) {
            self.replace_prompt = Some(path);
        } else {
            self.start_worker(path, ExistingChoice::KeepBoth, None);
        }
    }

    /// Runs an install on a worker so animation, input, and dialogs remain responsive.
    fn start_worker(&mut self, path: PathBuf, choice: ExistingChoice, selected_launcher: Option<PathBuf>) {
        self.current = Some(path.clone());
        self.current_choice = Some(choice);
        self.log.push(format!("Installing {}…", path.display()));
        let (sender, receiver) = mpsc::channel();
        self.receiver = Some(receiver);
        std::thread::spawn(move || {
            let mut log = Vec::new();
            let result = installer::install(&path, choice, selected_launcher.as_deref(), &mut log);
            let _ = sender.send(WorkResult { result, log });
        });
    }

    /// Integrates a completed worker result and continues queued archives when appropriate.
    fn poll_worker(&mut self) {
        let finished = self.receiver.as_ref().and_then(|receiver| receiver.try_recv().ok());
        if let Some(work) = finished {
            let path = self.current.take();
            let choice = self.current_choice.take().unwrap_or(ExistingChoice::KeepBoth);
            self.receiver = None;
            self.log.extend(work.log);
            match work.result {
                Ok(InstallResult::Installed(app)) => {
                    self.message = Some((true, format!("{} is ready to use.", app.name)));
                    self.installed.push(app);
                    self.records = InstalledDatabase::load().unwrap_or_default();
                }
                Ok(InstallResult::NeedsLauncherChoice(candidates)) => {
                    if let Some(path) = path {
                        self.launcher_prompt = Some((path, choice, candidates));
                    }
                }
                Err(error) => self.message = Some((false, format!("Installation failed: {error:#}"))),
            }
            self.start_next();
        }
    }

    /// Receives background release checks and update transactions without blocking the window.
    fn poll_update_worker(&mut self) {
        let finished = self.update_receiver.as_ref().and_then(|receiver| receiver.try_recv().ok());
        if let Some(result) = finished {
            self.update_receiver = None; self.update_busy = None;
            match result {
                UpdateWorkResult::Checked(Ok((record, release))) => {
                    replace_record(&mut self.records, record.clone());
                    self.message = Some((true, match release { Some(info) => format!("{} {} is available for {}.{}{}", record.name, info.version, record.name, info.notes.as_deref().map(|_| " Release notes were found.").unwrap_or(""), info.download_url.as_deref().map(|_| " Download is ready when you choose Update.").unwrap_or("")), None => format!("{} is up to date.", record.name) }));
                }
                UpdateWorkResult::Updated(Ok(record)) => { replace_record(&mut self.records, record.clone()); self.message = Some((true, format!("{} was updated successfully.", record.name))); }
                UpdateWorkResult::Checked(Err(error)) | UpdateWorkResult::Updated(Err(error)) => self.message = Some((false, error)),
            }
        }
    }

    /// Starts an explicit release check. Manual providers report their configuration requirement.
    fn check_record(&mut self, record: InstalledRecord) {
        if self.update_receiver.is_some() { return; }
        self.update_busy = Some(format!("Checking {}…", record.name));
        let (sender, receiver) = mpsc::channel(); self.update_receiver = Some(receiver);
        std::thread::spawn(move || { let mut record = record; let result = updates::check_for_update(&mut record).map(|release| (record, release)).map_err(|error| format!("Update check failed: {error}")); let _ = sender.send(UpdateWorkResult::Checked(result)); });
    }

    /// Starts a reversible update transaction through the update subsystem.
    fn update_record(&mut self, record: InstalledRecord) {
        if self.update_receiver.is_some() { return; }
        self.update_busy = Some(format!("Updating {}…", record.name));
        let (sender, receiver) = mpsc::channel(); self.update_receiver = Some(receiver);
        std::thread::spawn(move || { let mut log = Vec::new(); let result = updates::update(&record, &mut log).map_err(|error| format!("Update failed: {error:#}")); let _ = sender.send(UpdateWorkResult::Updated(result)); });
    }

    /// Renders the large, explicit target for pointer drops and provides visual drag feedback.
    fn drop_zone(&mut self, ui: &mut egui::Ui, hovered_files: usize) {
        let available = ui.available_width().min(700.0);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(available, 218.0), Sense::click());
        let hovering = hovered_files > 0 && ui.ctx().pointer_hover_pos().is_some_and(|position| rect.contains(position));
        let fill = if hovering { ACCENT.linear_multiply(0.20) } else { ui.visuals().faint_bg_color };
        let stroke = if hovering { Stroke::new(2.0_f32, ACCENT) } else { Stroke::new(1.0_f32, ui.visuals().widgets.inactive.bg_stroke.color) };
        ui.painter().rect(rect, CornerRadius::same(14), fill, stroke, egui::StrokeKind::Outside);
        ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
            ui.with_layout(Layout::top_down_justified(Align::Center), |ui| {
                ui.add_space(30.0);
                ui.label(RichText::new(if hovering { "⇩" } else { "⇪" }).size(42.0).color(if hovering { ACCENT } else { ui.visuals().text_color() }));
                ui.add_space(4.0);
                ui.label(RichText::new(if hovering { "Release to add archive" } else { "Drop an archive here" }).size(24.0).strong());
                ui.label(if hovering { "TarDrop will validate it before making any changes." } else { "Tar, gzip, xz, bzip2, and ZIP archives are supported." });
                ui.add_space(12.0);
                if ui.button(RichText::new("Open archive…").strong()).clicked() || response.clicked() {
                    self.choose_archives();
                }
            });
        });
    }

    /// Shows the current queue and worker state in one compact status card.
    fn activity_card(&self, ui: &mut egui::Ui) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Installation activity").strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if self.receiver.is_some() { ui.label(RichText::new("Working").color(ACCENT)); }
                    else { ui.label(RichText::new("Idle").color(SUCCESS)); }
                });
            });
            ui.add_space(8.0);
            if let Some(current) = &self.current {
                let name = current.file_name().and_then(|name| name.to_str()).unwrap_or("archive");
                ui.add(egui::ProgressBar::new(0.55).animate(true).text(format!("Installing {name}")));
            } else {
                ui.label("No installation is currently running.");
            }
            if !self.queue.is_empty() { ui.small(format!("{} archive(s) waiting in the queue", self.queue.len())); }
        });
    }

    /// Displays launch and removal actions for installations created in this session.
    fn installed_apps(&mut self, ui: &mut egui::Ui) {
        let mut uninstall = None;
        if self.installed.is_empty() {
            empty_state(ui, "No applications installed in this session", "Installed applications will appear here with quick actions.");
            return;
        }
        for (index, app) in self.installed.iter().enumerate() {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("◉").size(24.0).color(ACCENT));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&app.name).strong());
                        ui.small(app.directory.display().to_string());
                        ui.small(format!("Archive SHA-256: {}", app.sha256));
                    });
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.button("Launch").clicked() { let _ = Command::new(&app.executable).spawn(); }
                    if ui.button("Open folder").clicked() { let _ = Command::new("xdg-open").arg(&app.directory).spawn(); }
                    if ui.button(RichText::new("Uninstall").color(ui.visuals().error_fg_color)).clicked() { uninstall = Some(index); }
                });
            });
            ui.add_space(6.0);
        }
        if let Some(index) = uninstall {
            let app = self.installed.remove(index);
            match installer::uninstall(&app) {
                Ok(()) => self.message = Some((true, format!("{} was uninstalled.", app.name))),
                Err(error) => self.message = Some((false, format!("Uninstall failed: {error}"))),
            }
        }
    }
}

impl eframe::App for TarDropApp {
    /// Draws the window and receives native drag events from Wayland/X11 through eframe.
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_worker();
        self.poll_update_worker();

        // `hovered_files` is populated while the pointer is over the native window, whereas
        // `dropped_files` is emitted once on release. Reading both fixes visible feedback and
        // makes the whole drop target accept drops instead of relying on a button click.
        let (hovered_files, dropped_files): (usize, Vec<PathBuf>) = context.input(|input| {
            (input.raw.hovered_files.len(), input.raw.dropped_files.iter().filter_map(|file| file.path.clone()).collect())
        });
        if !dropped_files.is_empty() { self.enqueue(dropped_files); }
        if hovered_files > 0 { context.request_repaint(); }

        egui::TopBottomPanel::top("title_bar").frame(Frame::new().fill(context.style().visuals.panel_fill).inner_margin(Margin::symmetric(20, 14))).show(context, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("◉").size(25.0).color(ACCENT));
                ui.vertical(|ui| { ui.label(RichText::new("TarDrop").size(20.0).strong()); ui.small("Portable application installer"); });
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| { ui.label(RichText::new("User-only • No sudo").color(ui.visuals().weak_text_color())); });
            });
        });

        egui::CentralPanel::default().frame(Frame::new().inner_margin(Margin::symmetric(22, 18))).show(context, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                self.page_navigation(ui);
                ui.add_space(14.0);
                match self.page {
                    Page::Install => self.install_page(ui, hovered_files),
                    Page::Installed => self.records_page(ui, false),
                    Page::Updates => self.records_page(ui, true),
                    Page::Settings => self.settings_page(ui),
                }
            });
        });

        self.dialogs(context);
    }
}

impl TarDropApp {
    /// Presents compact page controls without hiding ongoing installation or update work.
    fn page_navigation(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            for (page, label) in [(Page::Install, "Install"), (Page::Installed, "Installed Applications"), (Page::Updates, "Updates"), (Page::Settings, "Settings")] {
                let selected = self.page == page;
                if ui.selectable_label(selected, label).clicked() { self.page = page; }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if let Some(work) = &self.update_busy { ui.label(RichText::new(work).color(ACCENT)); }
            });
        });
        ui.separator();
    }

    /// Combines the drop surface, active queue, session actions, and technical log on Install.
    fn install_page(&mut self, ui: &mut egui::Ui, hovered_files: usize) {
        ui.vertical_centered(|ui| { self.drop_zone(ui, hovered_files); });
        ui.add_space(16.0); self.activity_card(ui); ui.add_space(16.0);
        ui.label(RichText::new("Installed this session").size(18.0).strong()); ui.add_space(6.0);
        self.installed_apps(ui); ui.add_space(14.0);
        egui::CollapsingHeader::new(RichText::new("Technical installation log").strong()).default_open(false).show(ui, |ui| {
            Frame::new().fill(ui.visuals().faint_bg_color).corner_radius(CornerRadius::same(8)).inner_margin(Margin::same(10)).show(ui, |ui| {
                egui::ScrollArea::vertical().max_height(175.0).show(ui, |ui| { for line in &self.log { ui.monospace(line); } });
            });
        });
    }

    /// Renders durable database-backed records either as management cards or update cards.
    fn records_page(&mut self, ui: &mut egui::Ui, updates_only: bool) {
        let heading = if updates_only { "Updates" } else { "Installed Applications" };
        ui.label(RichText::new(heading).size(22.0).strong());
        ui.small(if updates_only { "Check sources manually; TarDrop never downloads updates without your approval." } else { "Applications installed by TarDrop are stored in your user-only application directory." });
        ui.add_space(12.0);
        if self.records.is_empty() { empty_state(ui, "No managed applications yet", "Install a portable archive to create an application record."); return; }
        let mut uninstall = None;
        let records = self.records.clone();
        for record in records {
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("◉").size(26.0).color(ACCENT));
                    ui.vertical(|ui| {
                        ui.label(RichText::new(&record.name).size(17.0).strong());
                        ui.small(format!("Installed: {}", record.version.as_deref().unwrap_or("Unknown version")));
                        if updates_only { ui.small(format!("Latest: {}", record.latest_version.as_deref().unwrap_or("Not checked"))); }
                        ui.small(record.install_path.display().to_string());
                    });
                });
                ui.add_space(7.0);
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Launch").clicked() { launch_record(&record); }
                    if ui.button("Open folder").clicked() { let _ = Command::new("xdg-open").arg(&record.install_path).spawn(); }
                    if ui.button("Check updates").clicked() { self.check_record(record.clone()); }
                    let can_update = record.latest_version.as_deref().is_some_and(|latest| record.version.as_deref() != Some(latest));
                    if ui.add_enabled(can_update && self.update_receiver.is_none(), egui::Button::new("Update")).clicked() { self.update_record(record.clone()); }
                    if ui.button(RichText::new("Uninstall").color(ui.visuals().error_fg_color)).clicked() { uninstall = Some(record.clone()); }
                });
                if self.update_busy.as_deref().is_some_and(|status| status.contains(&record.name)) { ui.add(egui::ProgressBar::new(0.5).animate(true).text("Working…")); }
            });
            ui.add_space(7.0);
        }
        if let Some(record) = uninstall { self.uninstall_record(record); }
    }

    /// Saves update preferences immediately so the next launch sees the selected policy.
    fn settings_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Update settings").size(22.0).strong()); ui.add_space(8.0);
        card(ui, |ui| {
            let mut changed = false;
            changed |= ui.checkbox(&mut self.settings.check_automatically, "Check for updates automatically").changed();
            changed |= ui.checkbox(&mut self.settings.notify_beta_releases, "Notify about beta releases").changed();
            changed |= ui.checkbox(&mut self.settings.check_on_startup, "Check on startup").changed();
            ui.add_space(8.0); ui.label("Update interval");
            for (interval, label) in [(UpdateInterval::Daily, "Daily"), (UpdateInterval::Weekly, "Weekly"), (UpdateInterval::Monthly, "Monthly"), (UpdateInterval::Never, "Never")] { changed |= ui.radio_value(&mut self.settings.interval, interval, label).changed(); }
            if changed { if let Err(error) = InstalledDatabase::save_settings(&self.settings) { self.message = Some((false, format!("Could not save settings: {error}"))); } }
        });
    }

    /// Converts a durable record to the installer’s narrowly scoped removal operation.
    fn uninstall_record(&mut self, record: InstalledRecord) {
        let app = InstalledApp { name: record.name.clone(), directory: record.install_path.clone(), executable: PathBuf::new(), desktop_file: record.desktop_file_path.clone(), icon: record.icon_path.clone(), sha256: String::new() };
        match installer::uninstall(&app) {
            Ok(()) => { self.records.retain(|existing| existing.install_path != record.install_path); self.message = Some((true, format!("{} was uninstalled.", record.name))); }
            Err(error) => self.message = Some((false, format!("Uninstall failed: {error}"))),
        }
    }

    /// Shows the modal decisions that intentionally pause the installation queue.
    fn dialogs(&mut self, context: &egui::Context) {
        if let Some(path) = self.replace_prompt.clone() {
            modal(context, "Existing installation", |ui| {
                ui.label(format!("An application named “{}” is already installed.", utils::archive_stem(&path)));
                ui.small("Choose whether to replace it or retain both installations.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("Replace existing").strong()).clicked() { self.replace_prompt = None; self.start_worker(path.clone(), ExistingChoice::Replace, None); }
                    if ui.button("Keep both").clicked() { self.replace_prompt = None; self.start_worker(path.clone(), ExistingChoice::KeepBoth, None); }
                    if ui.button("Cancel").clicked() { self.replace_prompt = None; self.log.push("Installation cancelled.".into()); self.start_next(); }
                });
            });
        }
        if let Some((path, choice, candidates)) = self.launcher_prompt.clone() {
            modal(context, "Choose application launcher", |ui| {
                ui.label("Several launchers look equally suitable. Select the application’s main entry point:");
                ui.add_space(8.0);
                egui::ScrollArea::vertical().max_height(280.0).show(ui, |ui| {
                    for candidate in &candidates {
                        let label = format!("{}\nScore {} · {}", candidate.relative_path.display(), candidate.score, candidate.reason);
                        if ui.add_sized([ui.available_width(), 42.0], egui::Button::new(label)).clicked() { self.launcher_prompt = None; self.start_worker(path.clone(), choice, Some(candidate.relative_path.clone())); }
                        ui.add_space(3.0);
                    }
                });
                if ui.button("Cancel installation").clicked() { self.launcher_prompt = None; self.log.push("Installation cancelled while choosing a launcher.".into()); self.start_next(); }
            });
        }
        if let Some((success, text)) = self.message.clone() {
            modal(context, if success { "Installation complete" } else { "TarDrop error" }, |ui| {
                ui.horizontal(|ui| { ui.label(RichText::new(if success { "✓" } else { "!" }).size(26.0).color(if success { SUCCESS } else { ui.visuals().error_fg_color })); ui.label(text); });
                ui.add_space(12.0);
                if ui.button(RichText::new("OK").strong()).clicked() { self.message = None; }
            });
        }
    }
}

/// Applies restrained rounded controls and spacing while retaining the current system theme.
fn configure_visuals(context: &egui::Context) {
    context.style_mut(|style| {
        style.spacing.item_spacing = Vec2::new(8.0, 8.0);
        style.spacing.button_padding = Vec2::new(12.0, 7.0);
        style.visuals.widgets.inactive.corner_radius = CornerRadius::same(7);
        style.visuals.widgets.hovered.corner_radius = CornerRadius::same(7);
        style.visuals.widgets.active.corner_radius = CornerRadius::same(7);
    });
}

/// Draws a soft rounded container used for related content and action groups.
fn card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    Frame::new().fill(ui.visuals().faint_bg_color).stroke(Stroke::new(1.0_f32, ui.visuals().widgets.inactive.bg_stroke.color)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(13)).show(ui, content);
}

/// Gives an empty section a calm explanatory placeholder instead of a bare blank area.
fn empty_state(ui: &mut egui::Ui, title: &str, detail: &str) {
    card(ui, |ui| { ui.label(RichText::new(title).strong()); ui.small(detail); });
}

/// Reuses the installed desktop launcher to launch a database-backed application safely.
fn launch_record(record: &InstalledRecord) {
    let _ = Command::new("xdg-open").arg(&record.desktop_file_path).spawn();
}

/// Replaces a changed record in the in-memory list after a background operation persists it.
fn replace_record(records: &mut Vec<InstalledRecord>, replacement: InstalledRecord) {
    if let Some(record) = records.iter_mut().find(|record| record.install_path == replacement.install_path) { *record = replacement; }
    else { records.push(replacement); }
}

/// Applies the chosen update interval to one record's persisted last-check timestamp.
fn startup_check_due(record: &InstalledRecord, interval: UpdateInterval) -> bool {
    let seconds = match interval { UpdateInterval::Daily => 86_400, UpdateInterval::Weekly => 604_800, UpdateInterval::Monthly => 2_592_000, UpdateInterval::Never => return false };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs();
    record.last_update_check.map(|last| now.saturating_sub(last) >= seconds).unwrap_or(true)
}

/// Opens a consistently styled modal dialog centered on the application viewport.
fn modal(context: &egui::Context, title: &str, content: impl FnOnce(&mut egui::Ui)) {
    egui::Window::new(title).collapsible(false).resizable(false).default_width(470.0).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(context, |ui| { ui.add_space(4.0); content(ui); ui.add_space(4.0); });
}
