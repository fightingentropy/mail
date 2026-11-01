//! The desktop window: `mail` with no command opens it.
//!
//! Everything shown comes from the local database, so the window opens and
//! responds instantly; a background thread syncs while it is open.

mod background;
mod compose;
mod demo;
mod drafts;
mod keys;
mod script;
mod state;
mod theme;
mod view;

use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc::{self, Receiver, Sender},
    time::Instant,
};

use anyhow::Context;
use mail_core::{paths::Paths, store::Store, sync::Progress};

use background::{Event, Syncer};
use drafts::Finished;
use state::{Action, Model};
use view::{Screen, SyncStatus};

pub struct Options {
    /// Open a made-up mailbox in a temporary folder, with no network.
    pub demo: bool,
    /// Save a PNG of the window to this path after it renders, then quit.
    pub screenshot: Option<PathBuf>,
    /// Keys to press in the demo before the screenshot, such as `j e / text:seat`.
    pub demo_keys: Option<String>,
}

pub fn run(paths: &Paths, options: Options) -> anyhow::Result<()> {
    let demo_folder = options
        .demo
        .then(|| std::env::temp_dir().join(format!("mail-demo-{}", std::process::id())));
    let paths = match &demo_folder {
        Some(folder) => {
            let _ = fs::remove_dir_all(folder);
            demo::seed(&folder.join("mail.sqlite3"))?;
            Paths::under(folder)
        }
        None => paths.clone(),
    };

    let store = Store::open(&paths.database())?;
    let account = store
        .account()?
        .context("Not signed in. Run `mail auth --client <file>` in Terminal first.")?;
    let model = Model::new(store, account)?;

    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mail")
            .with_app_id("org.erlin.mail")
            .with_inner_size([1360.0, 860.0])
            .with_min_inner_size([980.0, 600.0]),
        persist_window: !options.demo,
        persistence_path: Some(paths.root.join("window")),
        ..Default::default()
    };
    let demo = options.demo;
    let screenshot = options.screenshot;
    let script = script::parse(options.demo_keys.as_deref().unwrap_or_default());
    let result = eframe::run_native(
        "Mail",
        native,
        Box::new(move |creation| {
            theme::install(&creation.egui_ctx);
            let syncer = (!demo).then(|| {
                let ctx = creation.egui_ctx.clone();
                Syncer::start(paths.clone(), move || ctx.request_repaint())
            });
            let (drafts_done, drafts) = mpsc::channel();
            let status = if demo {
                SyncStatus::Demo
            } else {
                SyncStatus::Working("Syncing…".into())
            };
            Ok(Box::new(Shell {
                model,
                screen: Screen::new(status),
                syncer,
                paths,
                demo,
                drafts,
                drafts_done,
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
    screen: Screen,
    syncer: Option<Syncer>,
    paths: Paths,
    demo: bool,
    /// Results of draft saves and discards running on their own threads.
    drafts: Receiver<Finished>,
    drafts_done: Sender<Finished>,
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
        self.handle_drafts(ctx);
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
            Err(error) => self.model.notice = Some(format!("{error:#}")),
        }
        if let Some(screenshot) = &mut self.screenshot
            && self.script.is_empty()
        {
            take_screenshot(ctx, screenshot);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        view::draw(ui, &mut self.model, &mut self.screen);
    }

    /// Window size and place persist; egui's own memory does not.
    fn persist_egui_memory(&self) -> bool {
        false
    }
}

impl Shell {
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
                Event::Progress(Progress::Downloading { done, total }) => {
                    self.screen.status =
                        SyncStatus::Working(format!("Downloading {done} of {total}"));
                    // Show mail as it arrives during a first sync.
                    if done.is_multiple_of(50) {
                        self.reload();
                    }
                }
                Event::Synced(report) => {
                    self.screen.status = SyncStatus::Idle;
                    if report.rejected > 0 {
                        self.model.notice = Some(format!(
                            "Gmail refused {} change{}; showing Gmail's version.",
                            report.rejected,
                            if report.rejected == 1 { "" } else { "s" }
                        ));
                    }
                    self.reload();
                }
                Event::Failed(error) => self.screen.status = SyncStatus::Failed(error),
            }
        }
    }

    /// Starts draft saves and discards the model asked for, and reports
    /// finished ones back to it.
    fn handle_drafts(&mut self, ctx: &egui::Context) {
        let from = self.model.account.email.clone();
        if let Some(request) = self.model.save_request.take() {
            if self.demo {
                self.apply(Action::DraftFailed(
                    "The demo mailbox doesn't save drafts.".into(),
                ));
            } else {
                let wake = ctx.clone();
                drafts::save(
                    &self.paths,
                    &from,
                    request.draft,
                    request.draft_id,
                    self.drafts_done.clone(),
                    move || wake.request_repaint(),
                );
            }
        }
        if let Some(draft_id) = self.model.discard_request.take()
            && !self.demo
        {
            let wake = ctx.clone();
            drafts::discard(
                &self.paths,
                &from,
                draft_id,
                self.drafts_done.clone(),
                move || wake.request_repaint(),
            );
        }
        let finished: Vec<Finished> = self.drafts.try_iter().collect();
        for result in finished {
            match result {
                Finished::Saved(id) => self.apply(Action::DraftSaved(id)),
                Finished::Failed(error) => self.apply(Action::DraftFailed(error)),
                Finished::Discarded => self.model.wants_sync = true,
            }
        }
    }

    fn apply(&mut self, action: Action) {
        if let Err(error) = self.model.apply(action) {
            self.model.notice = Some(format!("{error:#}"));
        }
    }

    fn reload(&mut self) {
        if let Err(error) = self.model.reload() {
            self.model.notice = Some(format!("{error:#}"));
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
