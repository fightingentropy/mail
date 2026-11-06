//! The desktop window: `mail` with no command opens it.
//!
//! Everything shown comes from the local database, so the window opens and
//! responds instantly; a background thread syncs while it is open.

mod background;
mod compose;
mod demo;
mod downloads;
mod drafts;
mod files;
mod icloud_form;
mod keys;
mod links;
mod original;
mod preferences;
mod script;
mod shortcuts;
mod state;
mod theme;
mod toast;
mod view;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::Instant,
};

use anyhow::Context;
use mail_core::{auth::OAuthClient, compose::Draft, paths::Paths, store::Store, sync::Progress};

use crate::accounts;

use background::{Event, Syncer};
use downloads::Downloader;
use drafts::Finished;
use original::{Original, Page};
use preferences::Preferences;
use state::{Action, Effect, Model};
use view::{Screen, SyncStatus};

pub struct Options {
    /// Open a made-up mailbox in a temporary folder, with no network.
    pub demo: bool,
    /// Save a PNG of the window to this path after it renders, then quit.
    pub screenshot: Option<PathBuf>,
    /// Keys to press in the demo before the screenshot, such as `j e cmd+k text:seat`.
    pub demo_keys: Option<String>,
    /// Light or dark instead of following macOS, for screenshots of both.
    pub appearance: Option<egui::Theme>,
}

pub fn run(paths: &Paths, options: Options) -> anyhow::Result<()> {
    let demo_folder = options
        .demo
        .then(|| std::env::temp_dir().join(format!("mail-demo-{}", std::process::id())));
    let paths = match &demo_folder {
        Some(folder) => {
            let _ = fs::remove_dir_all(folder);
            demo::seed(&folder.join("mail.sqlite3"))?;
            demo::seed_work(&folder.join("mail.sqlite3"))?;
            Paths::under(folder)
        }
        None => paths.clone(),
    };

    let store = Store::open(&paths.database())?;
    store
        .account()?
        .context("Not signed in. Run `mail auth --client <file>` in Terminal first.")?;
    let mut model = Model::new(store)?;
    // A screenshot captures what egui draws, and WebKit draws apart from it.
    let originals = options.screenshot.is_none();
    let preferences = Preferences::load(&Preferences::path(&paths));
    model.show_original = originals && preferences.show_original;
    model.remote_images = preferences.remote_images;

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mail")
            .with_app_id("org.erlin.mail")
            // An empty icon keeps eframe from swapping the Dock icon for its
            // own logo, so the Dock shows Mail.app's icon like everywhere else.
            .with_icon(egui::IconData::default())
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([980.0, 600.0]),
        persist_window: !options.demo,
        persistence_path: Some(paths.root.join("window")),
        ..Default::default()
    };
    let demo = options.demo;
    let screenshot = options.screenshot;
    let script = script::parse(options.demo_keys.as_deref().unwrap_or_default());
    let appearance = options.appearance;
    let result = eframe::run_native(
        "Mail",
        native,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            if let Some(theme) = appearance {
                creation.egui_ctx.set_theme(theme);
            }
            let downloader = (!demo).then(|| {
                let ctx = creation.egui_ctx.clone();
                Downloader::start(paths.clone(), move || ctx.request_repaint())
            });
            let syncer = (!demo).then(|| {
                let ctx = creation.egui_ctx.clone();
                Syncer::start(paths.clone(), move || ctx.request_repaint())
            });
            let (drafts_done, drafts) = mpsc::channel();
            let (sign_ins_done, sign_ins) = mpsc::channel();
            let (icloud_sign_ins_done, icloud_sign_ins) = mpsc::channel();
            let (photos_done, photos) = mpsc::channel();
            let status = if demo {
                SyncStatus::Demo
            } else {
                SyncStatus::Working("Syncing…".into())
            };
            let mut screen = Screen::new(status, paths.clone(), preferences);
            screen.original_available = originals;
            Ok(Box::new(Shell {
                model,
                original: originals.then(Original::new),
                screen,
                syncer,
                paths,
                demo,
                drafts,
                drafts_done,
                sign_ins,
                sign_ins_done,
                icloud_sign_ins,
                icloud_sign_ins_done,
                photos,
                photos_done,
                downloader,
                screenshot: screenshot.map(|path| Screenshot { path, frames: 0 }),
                script,
                frames: 0,
            }))
        }),
    )
    .map_err(|error| anyhow::anyhow!("The window could not open: {error}"));

    if let Some(folder) = demo_folder {
        let _ = fs::remove_dir_all(folder);
    }
    result
}

struct Shell {
    model: Model,
    /// The WebKit view for originals; `None` for screenshots.
    original: Option<Original>,
    screen: Screen,
    syncer: Option<Syncer>,
    paths: Paths,
    demo: bool,
    /// Results of draft saves and discards running on their own threads.
    drafts: Receiver<Finished>,
    drafts_done: Sender<Finished>,
    /// Results of "Add account…": the new account's address, or the error.
    sign_ins: Receiver<Result<String, String>>,
    sign_ins_done: Sender<Result<String, String>>,
    /// Results of "Add iCloud account…", which its form shows.
    icloud_sign_ins: Receiver<Result<String, String>>,
    icloud_sign_ins_done: Sender<Result<String, String>>,
    /// Results of "Choose photo…": the account's address once its photo is
    /// saved, `None` if you cancelled, or the error.
    photos: Receiver<Result<Option<String>, String>>,
    photos_done: Sender<Result<Option<String>, String>>,
    /// `None` in the demo, which has no Gmail to download from.
    downloader: Option<Downloader>,
    screenshot: Option<Screenshot>,
    /// Scripted key presses for demo screenshots, one step every few frames.
    script: std::collections::VecDeque<Vec<egui::Event>>,
    frames: u64,
}

struct Screenshot {
    path: PathBuf,
    frames: u32,
}

impl eframe::App for Shell {
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.frames += 1;
        // Keys pressed while the original had focus.
        if let Some(original) = &self.original {
            input.events.extend(original.take_keys());
        }
        if !self.script.is_empty() {
            ctx.request_repaint();
            if self.frames > 10
                && self.frames.is_multiple_of(4)
                && let Some(step) = self.script.pop_front()
            {
                input.events.extend(step);
            }
        }
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive_sync_events();
        self.receive_draft_results();
        self.receive_downloads();
        self.receive_sign_ins();
        self.receive_photos();
        if let Some(syncer) = &self.syncer
            && (std::mem::take(&mut self.model.wants_sync)
                || std::mem::take(&mut self.screen.sync_requested))
        {
            self.screen.status = SyncStatus::Working("Syncing…".into());
            syncer.sync_now();
        }
        match self.model.tick(Instant::now()) {
            Ok(Some(wait)) => ctx.request_repaint_after(wait),
            Ok(None) => {}
            Err(error) => self.model.notify(format!("{error:#}")),
        }
        if let Some(screenshot) = &mut self.screenshot
            && self.script.is_empty()
        {
            take_screenshot(ctx, screenshot);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        view::draw(ui, &mut self.model, &mut self.screen);
        let ctx = ui.ctx().clone();
        self.place_original(&ctx, frame);
        self.carry_out_effects(&ctx);
    }

    /// Window size and place persist; egui's own memory does not.
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl Shell {
    /// Puts the WebKit view where the reader left room for it, or hides it.
    /// It draws over the window, so menus and overlays hide it too.
    fn place_original(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let Some(original) = &mut self.original else {
            return;
        };
        let covered =
            ctx.any_popup_open() || ctx.memory(|memory| memory.top_modal_layer().is_some());
        let scroll = self.screen.scroll_original.take();
        let (Some(rect), Some(html), false) = (
            self.screen.original_rect,
            self.model.showing_original(),
            covered,
        ) else {
            original.hide();
            return;
        };
        let page = Page {
            message_id: html.message_id.clone(),
            images: html.images,
            inline: html.inline,
        };
        let shown = original.show(frame, ctx, rect, &page, || html.html.clone());
        if let Err(error) = shown {
            // Fall back to text for good rather than try every frame.
            self.original = None;
            self.model.show_original = false;
            self.screen.original_available = false;
            self.model
                .notify(format!("Could not show the original: {error:#}"));
            return;
        }
        if let Some(pages) = scroll {
            original.scroll(pages);
        }
        // Typing in search goes to the window, not to the page.
        if ctx.egui_wants_keyboard_input() {
            original.release_focus();
        }
    }

    fn receive_sync_events(&mut self) {
        let events: Vec<Event> = match &self.syncer {
            Some(syncer) => syncer.events.try_iter().collect(),
            None => return,
        };
        for event in events {
            match event {
                Event::Progress(Progress::Listing { found }) => {
                    self.screen.status = SyncStatus::Working(format!("Listing messages… {found}"));
                }
                Event::Progress(Progress::Filling { done, total }) => {
                    self.screen.status = SyncStatus::Working(format!(
                        "Downloading message bodies {done} of {total}"
                    ));
                    if done.is_multiple_of(50) {
                        self.reload();
                    }
                }
                Event::Progress(Progress::Checking { done, total }) => {
                    self.screen.status =
                        SyncStatus::Working(format!("Checking labels {done} of {total}"));
                }
                Event::Progress(Progress::Downloading { done, total }) => {
                    self.screen.status =
                        SyncStatus::Working(format!("Downloading {done} of {total}"));
                    // Show mail as it arrives during a first sync.
                    if done.is_multiple_of(50) {
                        self.reload();
                    }
                }
                Event::Synced(report) => {
                    self.screen.status = if report.bodies_pending > 0 {
                        SyncStatus::Working(format!(
                            "{} message bodies still downloading",
                            report.bodies_pending
                        ))
                    } else {
                        SyncStatus::Idle
                    };
                    if report.rejected > 0 {
                        self.model.notify(format!(
                            "{} change{} couldn't be made; showing your mailbox as it is.",
                            report.rejected,
                            if report.rejected == 1 { "" } else { "s" }
                        ));
                    }
                    self.reload();
                }
                Event::Failed(error) => self.screen.status = SyncStatus::Failed(error),
                Event::Photo(email) => self.screen.forget_photo(&email),
            }
        }
    }

    fn receive_photos(&mut self) {
        let results: Vec<Result<Option<String>, String>> = self.photos.try_iter().collect();
        for result in results {
            match result {
                Ok(Some(email)) => {
                    self.screen.forget_photo(&email);
                    self.model.notify("Photo updated.".into());
                }
                Ok(None) => {}
                Err(error) => self.model.notify(error),
            }
        }
    }

    /// Picks up accounts signed in from the window, and syncs them. A failed
    /// iCloud sign-in shows in its form, if that is still open.
    fn receive_sign_ins(&mut self) {
        let mut results: Vec<Result<String, String>> = self.sign_ins.try_iter().collect();
        for result in self.icloud_sign_ins.try_iter().collect::<Vec<_>>() {
            match (result, &mut self.screen.icloud_form) {
                (Err(error), Some(form)) => {
                    form.waiting = false;
                    form.error = Some(error);
                }
                (result, form) => {
                    if result.is_ok() && form.is_some() {
                        self.screen.icloud_form = None;
                    }
                    results.push(result);
                }
            }
        }
        for result in results {
            match result {
                Ok(email) => {
                    self.screen.forget_photo(&email);
                    self.reload();
                    self.model.notify(format!("Signed in as {email}."));
                    if let Some(syncer) = &self.syncer {
                        self.screen.status = SyncStatus::Working("Syncing…".into());
                        syncer.sync_now();
                    }
                }
                Err(error) => self.model.notify(error),
            }
        }
    }

    /// Reports finished draft saves and discards back to the model.
    fn receive_draft_results(&mut self) {
        let finished: Vec<Finished> = self.drafts.try_iter().collect();
        for result in finished {
            match result {
                Finished::Saved(id) => self.apply(Action::DraftSaved(id)),
                Finished::Failed(error) => self.apply(Action::DraftFailed(error)),
                Finished::Discarded => self.model.wants_sync = true,
                Finished::Sent => self.apply(Action::MessageSent),
                Finished::SendFailed(error) => self.apply(Action::SendFailed(error)),
            }
        }
    }

    fn receive_downloads(&mut self) {
        let events: Vec<downloads::Event> = match &self.downloader {
            Some(downloader) => downloader.events.try_iter().collect(),
            None => return,
        };
        for event in events {
            let notice = match event {
                downloads::Event::Saved { path } => {
                    let name = path
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    match files::open_download(&path) {
                        files::Opened::InApp => format!("Saved {name} to Downloads."),
                        files::Opened::InFinder => format!(
                            "Saved {name} to Downloads. Shown in Finder, not opened: it could run code."
                        ),
                    }
                }
                downloads::Event::Failed { filename, error } => {
                    format!("Could not download {filename}: {error}")
                }
                downloads::Event::InlineImages {
                    account_id,
                    message_id,
                    images,
                } => {
                    self.apply(Action::InlineImagesArrived {
                        account_id,
                        message_id,
                        images,
                    });
                    continue;
                }
            };
            self.model.notify(notice);
        }
    }

    /// Opens links, starts downloads, shows HTML and saves or discards
    /// drafts, as clicks and keys asked the model to.
    fn carry_out_effects(&mut self, ctx: &egui::Context) {
        for effect in std::mem::take(&mut self.model.effects) {
            let notice = match effect {
                Effect::OpenLink(url) => match files::open_link(&url) {
                    Ok(()) => None,
                    Err(error) => Some(format!("{error:#}")),
                },
                Effect::Download {
                    account,
                    message_id,
                    attachment,
                } => match &self.downloader {
                    Some(downloader) => {
                        let notice = format!("Downloading {}…", attachment.filename);
                        downloader.download(account, message_id, attachment);
                        Some(notice)
                    }
                    None => Some("Downloads aren't available in the demo.".to_owned()),
                },
                Effect::SaveDraft {
                    from,
                    draft,
                    draft_id,
                } => {
                    self.save_draft(ctx, &from, draft, draft_id);
                    None
                }
                Effect::DiscardDraft { from, draft_id } => {
                    self.discard_draft(ctx, &from, draft_id);
                    None
                }
                Effect::SendDraft {
                    from,
                    draft,
                    draft_id,
                } => {
                    self.send_draft(ctx, &from, draft, draft_id);
                    None
                }
                Effect::AddAccount => self.add_account(ctx),
                Effect::AddIcloudAccount { address, password } => {
                    self.add_icloud_account(ctx, address, password);
                    None
                }
                Effect::ChoosePhoto(email) => {
                    self.choose_photo(ctx, email);
                    None
                }
                Effect::UseGooglePhoto(email) => {
                    let _ = fs::remove_file(self.paths.chosen_photo(&email));
                    self.screen.forget_photo(&email);
                    None
                }
                Effect::FetchInlineImages {
                    account,
                    account_id,
                    message_id,
                    wanted,
                } => {
                    if let Some(downloader) = &self.downloader {
                        downloader.inline_images(account, account_id, message_id, wanted);
                    }
                    None
                }
                Effect::OpenHtml { message_id, html } => {
                    Some(match files::open_html(&message_id, &html) {
                        Ok(_) => "Opened the HTML version in your browser.".to_owned(),
                        Err(error) => format!("Could not open the HTML version: {error:#}"),
                    })
                }
            };
            if let Some(notice) = notice {
                self.model.notify(notice);
            }
        }
    }

    fn save_draft(
        &mut self,
        ctx: &egui::Context,
        from: &str,
        draft: Draft,
        draft_id: Option<String>,
    ) {
        if self.demo {
            self.apply(Action::DraftFailed(
                "The demo mailbox doesn't save drafts.".into(),
            ));
            return;
        }
        let wake = ctx.clone();
        drafts::save(
            &self.paths,
            from,
            draft,
            draft_id,
            self.drafts_done.clone(),
            move || wake.request_repaint(),
        );
    }

    fn send_draft(
        &mut self,
        ctx: &egui::Context,
        from: &str,
        draft: Draft,
        draft_id: Option<String>,
    ) {
        if self.demo {
            self.apply(Action::SendFailed("The demo mailbox doesn't send.".into()));
            return;
        }
        let wake = ctx.clone();
        drafts::send(
            &self.paths,
            from,
            draft,
            draft_id,
            self.drafts_done.clone(),
            move || wake.request_repaint(),
        );
    }

    fn discard_draft(&mut self, ctx: &egui::Context, from: &str, draft_id: String) {
        if self.demo {
            return;
        }
        let wake = ctx.clone();
        drafts::discard(
            &self.paths,
            from,
            draft_id,
            self.drafts_done.clone(),
            move || wake.request_repaint(),
        );
    }

    /// Asks for an image with the macOS file picker, on a thread of its own
    /// so the window keeps drawing.
    fn choose_photo(&mut self, ctx: &egui::Context, email: String) {
        let paths = self.paths.clone();
        let done = self.photos_done.clone();
        let wake = ctx.clone();
        thread::spawn(move || {
            let result = match files::choose_image("Choose a photo for this account") {
                Ok(Some(path)) => accounts::choose_photo(&paths, &email, &path)
                    .map(|()| Some(email))
                    .map_err(|error| format!("Could not use that photo: {error:#}")),
                Ok(None) => Ok(None),
                Err(error) => Err(format!("{error:#}")),
            };
            let _ = done.send(result);
            wake.request_repaint();
        });
    }

    /// Google sign-in runs in the browser; the window waits on a thread of
    /// its own. Returns a notice for the status bar.
    fn add_account(&mut self, ctx: &egui::Context) -> Option<String> {
        if self.demo {
            return Some("Adding accounts isn't available in the demo.".into());
        }
        let client = match OAuthClient::load(&self.paths.oauth_client()) {
            Ok(client) => client,
            Err(error) => return Some(format!("{error:#}")),
        };
        let paths = self.paths.clone();
        let done = self.sign_ins_done.clone();
        let wake = ctx.clone();
        thread::spawn(move || {
            let result = accounts::sign_in(&paths, client, files::open_link)
                .map_err(|error| format!("Could not add the account: {error:#}"));
            let _ = done.send(result);
            wake.request_repaint();
        });
        Some("Finish signing in in your browser.".into())
    }

    /// Checks the app-specific password with iCloud on a thread of its own;
    /// the form waits for the answer.
    fn add_icloud_account(&mut self, ctx: &egui::Context, address: String, password: String) {
        let done = self.icloud_sign_ins_done.clone();
        if self.demo {
            let _ = done.send(Err("Adding accounts isn't available in the demo.".into()));
            return;
        }
        let paths = self.paths.clone();
        let wake = ctx.clone();
        thread::spawn(move || {
            let result = accounts::sign_in_icloud(&paths, &address, &password)
                .map_err(|error| format!("{error:#}"));
            let _ = done.send(result);
            wake.request_repaint();
        });
    }

    fn apply(&mut self, action: Action) {
        if let Err(error) = self.model.apply(action) {
            self.model.notify(format!("{error:#}"));
        }
    }

    fn reload(&mut self) {
        if let Err(error) = self.model.reload() {
            self.model.notify(format!("{error:#}"));
        }
    }
}

/// Waits for fonts and layout to settle, captures the window, saves it and
/// closes.
fn take_screenshot(ctx: &egui::Context, screenshot: &mut Screenshot) {
    screenshot.frames += 1;
    ctx.request_repaint();
    if screenshot.frames == 20 {
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
    }
    let image = ctx.input(|input| {
        input.events.iter().find_map(|event| match event {
            egui::Event::Screenshot { image, .. } => Some(image.clone()),
            _ => None,
        })
    });
    if let Some(image) = image {
        if let Err(error) = save_png(&screenshot.path, &image) {
            eprintln!("mail: could not save the screenshot: {error:#}");
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

fn save_png(path: &Path, image: &egui::ColorImage) -> anyhow::Result<()> {
    let [width, height] = [image.size[0] as u32, image.size[1] as u32];
    let pixels: Vec<u8> = image
        .pixels
        .iter()
        .flat_map(|pixel| pixel.to_srgba_unmultiplied())
        .collect();
    let buffer = image::RgbaImage::from_raw(width, height, pixels)
        .context("The frame size did not match")?;
    buffer.save(path)?;
    Ok(())
}
