//! The window (egui / eframe). All file work goes through the library modules; slow work (hashing nie.exe,
//! downloads, unpacking, ModLoader install) runs on a thread and reports back through a channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Align, Align2, Color32, FontId, Id, Layout, RichText, Sense, Stroke, Ui, Vec2};
use evt_installer::game;
use evt_modfmt as fmt;
use vr_modloader_app::i18n::{self, tr, trf, Lang};
use vr_modloader_app::install::{self, Existing, Prepared};
use vr_modloader_app::model::{self, ConflictRow, Level, ModList, Status};
use vr_modloader_app::modloader::{self, Payload};
use vr_modloader_app::settings::{self, Settings};
use vr_modloader_app::urlscheme::{self, Link};
use vr_modloader_app::{audio, catalog, mods_root, APP_NAME};

#[derive(Debug, Default, Clone)]
pub struct Args {
    pub game_dir: Option<PathBuf>,
    pub lang: Option<String>,
    pub tab: Option<String>,
    pub select: Option<String>,
    /// A `vrmodloader:` link or an archive path.
    pub open: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Manager,
    Studio,
    Settings,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bottom {
    Conflicts,
    Problems,
    Log,
}

/// Results of background work.
enum Msg {
    Version(PathBuf, Result<bool, String>),
    Payload(Option<Result<Payload, String>>),
    Progress(String, Option<f32>),
    Downloaded(Result<PathBuf, String>),
    Staged(Result<Prepared, String>),
    LoaderInstalled(Result<bool, String>),
    LoaderRemoved(Result<(), String>),
    Index(Result<(Arc<vr_index::Index>, bool), String>),
}

enum Dialog {
    Message { title: String, body: String },
    InstallReview(Prepared),
    MissingDeps(Vec<(String, String, Vec<String>)>),
    ConfirmUninstall { id: String, dependents: Vec<String> },
    ProfileName { rename: Option<String>, text: String, error: String },
    ConfirmDeleteProfile(String),
    UnsavedSwitch(String),
    ConfirmLink(Link),
    ConfirmRemoveLoader,
    ConfirmEmptyTrash,
}

enum Action {
    Save,
    Refresh,
    Launch,
    PickArchive,
    InstallArchive(PathBuf),
    Download(String),
    CommitInstall,
    DiscardInstall,
    AskUninstall(String),
    Uninstall(String),
    ShowMissing(Vec<(String, String, Vec<String>)>),
    SwitchProfile(String),
    SwitchProfileNow(String, bool),
    NewProfile(String),
    RenameProfile(String, String),
    DeleteProfile(String),
    LoaderInstall,
    LoaderPick,
    LoaderRemove,
    EnableModsModule,
    Register,
    Unregister,
    EmptyTrash,
    Open(String),
    PickGame,
    DetectGame,
    SetGame(PathBuf),
}

/// Everything computed from the in-memory list (recomputed when it changes).
#[derive(Default)]
struct Derived {
    statuses: Vec<Status>,
    conflicts: Vec<ConflictRow>,
    problems: Vec<(Level, String)>,
    missing: Vec<(String, String, Vec<String>)>,
    active: usize,
}

pub struct ManagerApp {
    settings: Settings,
    /// `--game-dir`: used, never saved.
    game_override: bool,
    game: Option<PathBuf>,
    game_version: Option<Result<bool, String>>,
    game_running: bool,
    loader: Option<modloader::Status>,
    payload: Option<Result<Payload, String>>,
    list: Option<ModList>,
    derived: Derived,
    stale: bool,
    selected: Option<String>,
    profiles: (Vec<String>, usize),
    tab: Tab,
    bottom: Bottom,
    conflict_filter: String,
    log: Vec<String>,
    status_line: String,
    dialog: Option<Dialog>,
    busy: Option<(String, Option<f32>)>,
    tx: Sender<Msg>,
    rx: Receiver<Msg>,
    drag_from: Option<usize>,
    previews: HashMap<PathBuf, Option<egui::TextureHandle>>,
    catalog: Vec<catalog::Entry>,
    scheme_cmd: Option<String>,
    exe: PathBuf,
    pending_open: Option<String>,
    studio: Studio,
}

/// Studio tab state: the game index (built on request) and a search over it.
#[derive(Default)]
struct Studio {
    index: Option<Arc<vr_index::Index>>,
    query: String,
    category: Option<vr_index::Category>,
    hits: Vec<vr_index::Hit>,
    searched: Option<(String, Option<vr_index::Category>, usize)>,
    selected: Option<usize>,
}

// ---------------------------------------------------------------- colours

fn level_color(l: Level, dark: bool) -> Color32 {
    match l {
        Level::Ok | Level::Info => {
            if dark {
                Color32::from_rgb(110, 200, 120)
            } else {
                Color32::from_rgb(30, 130, 50)
            }
        }
        Level::Off => Color32::GRAY,
        Level::Warn => Color32::from_rgb(230, 170, 40),
        Level::Error => Color32::from_rgb(230, 80, 70),
    }
}

const ACCENT: Color32 = Color32::from_rgb(64, 140, 230);

/// Window icon: a lightning bolt on blue (drawn, no file).
pub fn icon() -> egui::IconData {
    let n = 64usize;
    let mut rgba = vec![0u8; n * n * 4];
    let bolt = [(36.0, 4.0), (14.0, 36.0), (30.0, 36.0), (24.0, 60.0), (50.0, 24.0), (34.0, 24.0), (42.0, 4.0)];
    let inside = |x: f32, y: f32| {
        let mut c = false;
        let mut j = bolt.len() - 1;
        for i in 0..bolt.len() {
            let (xi, yi) = bolt[i];
            let (xj, yj) = bolt[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    };
    for y in 0..n {
        for x in 0..n {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = ((fx - 32.0).powi(2) + (fy - 32.0).powi(2)).sqrt();
            let p = &mut rgba[(y * n + x) * 4..][..4];
            if d < 31.0 {
                p.copy_from_slice(&[40, 110, 210, 255]);
            }
            if inside(fx, fy) {
                p.copy_from_slice(&[255, 214, 40, 255]);
            }
        }
    }
    egui::IconData { rgba, width: n as u32, height: n as u32 }
}

impl ManagerApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args) -> ManagerApp {
        let settings = Settings::load_from(&settings::settings_path());
        let lang = args.lang.clone().filter(|l| !l.is_empty()).unwrap_or_else(|| {
            if settings.language.is_empty() {
                settings::system_language()
            } else {
                settings.language.clone()
            }
        });
        i18n::set_lang(Lang::from_code(&lang));
        cc.egui_ctx.global_style_mut(|s| {
            s.interaction.selectable_labels = false;
            s.spacing.item_spacing = Vec2::new(8.0, 6.0);
            s.spacing.button_padding = Vec2::new(10.0, 4.0);
        });
        let (tx, rx) = channel();
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("VR-ModLoader.exe"));
        let mut app = ManagerApp {
            game_override: args.game_dir.is_some(),
            game: None,
            game_version: None,
            game_running: false,
            loader: None,
            payload: None,
            list: None,
            derived: Derived::default(),
            stale: true,
            selected: args.select.clone(),
            profiles: (Vec::new(), 0),
            tab: match args.tab.as_deref() {
                Some("studio") => Tab::Studio,
                Some("settings") => Tab::Settings,
                _ => Tab::Manager,
            },
            bottom: Bottom::Conflicts,
            conflict_filter: String::new(),
            log: Vec::new(),
            status_line: String::new(),
            dialog: None,
            busy: None,
            tx,
            rx,
            drag_from: None,
            previews: HashMap::new(),
            catalog: catalog::load(&settings::app_data_dir().join("catalog.toml")),
            scheme_cmd: urlscheme::registered_command_at(urlscheme::CLASSES),
            exe,
            pending_open: args.open.clone(),
            studio: Studio::default(),
            settings,
        };
        // payload of the ModLoader (the built-in one is unpacked to %TEMP%)
        let tx = app.tx.clone();
        let ctx = cc.egui_ctx.clone();
        let exe_dir = app.exe.parent().map(Path::to_path_buf).unwrap_or_default();
        std::thread::spawn(move || {
            vr_modloader_app::cleanup_temp();
            let _ = tx.send(Msg::Payload(modloader::find_payload(&exe_dir)));
            ctx.request_repaint();
        });
        let game = args.game_dir.clone().or_else(|| app.settings.game_dir.clone().filter(|g| game::is_game_dir(g))).or_else(|| game::find_games().into_iter().next());
        if let Some(g) = game {
            app.set_game(g, &cc.egui_ctx);
        }
        app
    }

    fn log(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.status_line = s.clone();
        self.log.push(format!("[{}] {s}", evt_installer::stamp(evt_installer::now_secs())[9..].to_string()));
    }

    fn error(&mut self, e: impl Into<String>) {
        let e = e.into();
        self.log(format!("{}: {e}", tr("Error")));
        self.dialog = Some(Dialog::Message { title: tr("Error").into(), body: e });
    }

    fn save_settings(&mut self) {
        let mut s = self.settings.clone();
        if self.game_override {
            s.game_dir = Settings::load_from(&settings::settings_path()).game_dir;
        }
        if let Err(e) = s.save_to(&settings::settings_path()) {
            self.log(e);
        }
    }

    fn set_game(&mut self, dir: PathBuf, ctx: &egui::Context) {
        if !game::is_game_dir(&dir) {
            self.error(tr("That folder is not the game (no nie.exe / data\\cpk_list.cfg.bin)."));
            return;
        }
        self.game = Some(dir.clone());
        if !self.game_override {
            self.settings.game_dir = Some(dir.clone());
            self.save_settings();
        }
        self.game_version = None;
        let tx = self.tx.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let r = game::version_ok(&dir);
            let _ = tx.send(Msg::Version(dir, r));
            c.request_repaint();
        });
        install::clean_staging(&mods_root(self.game.as_ref().unwrap()));
        self.reload();
    }

    /// Re-read everything from disk (discards unsaved changes).
    fn reload(&mut self) {
        let Some(g) = self.game.clone() else { return };
        self.loader = Some(modloader::status(&g));
        let root = mods_root(&g);
        self.list = Some(ModList::load(&root));
        self.profiles = fmt::profiles(&root);
        self.game_running = game::game_running();
        self.stale = true;
        if self.selected.as_ref().is_some_and(|s| self.list.as_ref().is_some_and(|l| l.index_of(s).is_none())) {
            self.selected = None;
        }
    }

    /// Re-read only the ModLoader status (the list and its unsaved edits stay).
    fn reload_loader(&mut self) {
        if let Some(g) = self.game.clone() {
            self.loader = Some(modloader::status(&g));
            self.stale = true;
        }
    }

    fn loader_version(&self) -> Option<String> {
        self.loader.as_ref().and_then(|l| l.version().map(str::to_string))
    }

    fn recompute(&mut self) {
        if !self.stale {
            return;
        }
        self.stale = false;
        let lv = self.loader_version();
        let Some(list) = &self.list else {
            self.derived = Derived::default();
            return;
        };
        let plan = list.plan(lv.as_deref());
        let mut conflicts = model::conflict_rows(&plan);
        conflicts.extend(audio::conflicts(&plan));
        self.derived = Derived {
            statuses: model::statuses(list, &plan, lv.as_deref()),
            conflicts,
            problems: model::problem_lines(&plan),
            missing: list.missing_dependencies(),
            active: plan.mods.len(),
        };
    }

    fn spawn(&mut self, label: String, ctx: &egui::Context, f: impl FnOnce(&Sender<Msg>) -> Msg + Send + 'static) {
        self.busy = Some((label, None));
        let tx = self.tx.clone();
        let c = ctx.clone();
        std::thread::spawn(move || {
            let m = f(&tx);
            let _ = tx.send(m);
            c.request_repaint();
        });
    }

    fn handle_msgs(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Version(dir, r) => {
                    if self.game.as_deref() == Some(dir.as_path()) {
                        self.game_version = Some(r);
                    }
                }
                Msg::Payload(p) => self.payload = p,
                Msg::Progress(label, f) => self.busy = Some((label, f)),
                Msg::Downloaded(r) => {
                    self.busy = None;
                    match r {
                        Ok(p) => self.run(Action::InstallArchive(p), ctx),
                        Err(e) => self.error(e),
                    }
                }
                Msg::Staged(r) => {
                    self.busy = None;
                    match r {
                        Ok(p) => self.dialog = Some(Dialog::InstallReview(p)),
                        Err(e) => self.error(e),
                    }
                }
                Msg::LoaderInstalled(r) => {
                    self.busy = None;
                    self.reload_loader();
                    match r {
                        Ok(true) => {
                            let v = self.loader_version().unwrap_or_default();
                            self.log(trf("Installed {}", &[&format!("ModLoader {v}")]));
                        }
                        Ok(false) => self.log(tr("Done")),
                        Err(e) => self.error(e),
                    }
                }
                Msg::Index(r) => {
                    self.busy = None;
                    match r {
                        Ok((i, rebuilt)) => {
                            let n = i.entities().len();
                            self.log(trf("Game index ready: {} entries{}", &[&n, &if rebuilt { tr(" (built now)") } else { "" }]));
                            self.studio = Studio { index: Some(i), ..Studio::default() };
                        }
                        Err(e) => self.error(e),
                    }
                }
                Msg::LoaderRemoved(r) => {
                    self.busy = None;
                    self.reload_loader();
                    match r {
                        Ok(()) => self.log(tr("Done")),
                        Err(e) => self.error(e),
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------ actions

    fn mods_root(&self) -> Option<PathBuf> {
        self.game.as_deref().map(mods_root)
    }

    fn run(&mut self, a: Action, ctx: &egui::Context) {
        match a {
            Action::Save => {
                if let Some(l) = &mut self.list {
                    match l.save() {
                        Ok(()) => self.log(tr("Saved.")),
                        Err(e) => self.error(e),
                    }
                }
            }
            Action::Refresh => {
                self.reload();
                self.scheme_cmd = urlscheme::registered_command_at(urlscheme::CLASSES);
            }
            Action::Launch => {
                if self.list.as_ref().is_some_and(|l| l.dirty) {
                    self.run(Action::Save, ctx);
                    if self.list.as_ref().is_some_and(|l| l.dirty) {
                        return; // save failed
                    }
                }
                match vr_modloader_app::launch_game() {
                    Ok(()) => self.log(tr("Launch game")),
                    Err(e) => self.error(e),
                }
            }
            Action::PickArchive => {
                if let Some(p) = rfd::FileDialog::new().add_filter("Mod archive", &["zip", "7z", "rar"]).pick_file() {
                    self.run(Action::InstallArchive(p), ctx);
                }
            }
            Action::InstallArchive(p) => {
                let Some(root) = self.mods_root() else {
                    self.error(tr("No game folder selected"));
                    return;
                };
                let label = trf("Reading {}…", &[&p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()]);
                self.log(label.clone());
                self.spawn(label, ctx, move |_| {
                    let r = std::fs::create_dir_all(&root).map_err(|e| e.to_string()).and_then(|()| install::prepare(&root, &p));
                    Msg::Staged(r)
                });
            }
            Action::Download(url) => {
                let label = trf("Downloading {}…", &[&url]);
                self.log(label.clone());
                let dir = std::env::temp_dir().join(APP_NAME).join("downloads");
                let c = ctx.clone();
                self.spawn(label.clone(), ctx, move |tx| {
                    let mut last = 0u64;
                    let r = urlscheme::download(&url, &dir, &mut |done, total| {
                        if done - last > (1 << 20) || Some(done) == total {
                            last = done;
                            let f = total.map(|t| done as f32 / t.max(1) as f32);
                            let _ = tx.send(Msg::Progress(format!("{label} {}", evt_installer::human_size(done)), f));
                            c.request_repaint();
                        }
                    });
                    Msg::Downloaded(r)
                });
            }
            Action::CommitInstall => {
                let (Some(Dialog::InstallReview(prep)), Some(root)) = (self.dialog.take(), self.mods_root()) else { return };
                let missing = install::missing_after(&root, &prep);
                match install::commit(&root, &prep) {
                    Ok(done) => {
                        let installed: Vec<String> = done.iter().filter_map(|d| d.split(' ').next().map(str::to_string)).collect();
                        let was_dirty = self.list.as_ref().is_some_and(|l| l.dirty);
                        let keep: Option<(Vec<String>, Vec<String>)> = self.list.as_ref().map(|l| (l.enabled_list(), l.order_list()));
                        let before: Vec<String> = keep.as_ref().map(|k| k.1.clone()).unwrap_or_default();
                        // new ids are enabled and go on top; an updated mod keeps its place and switch
                        let fresh: Vec<String> = installed.iter().filter(|i| !before.contains(i)).cloned().collect();
                        self.reload();
                        if let (Some(l), Some((en, order))) = (self.list.as_mut(), keep) {
                            if was_dirty {
                                // put the unsaved edits back on top of what was just read from disk
                                for r in l.rows.iter_mut() {
                                    if !fresh.contains(&r.id) {
                                        r.enabled = en.contains(&r.id);
                                    }
                                }
                                let pos = |id: &str| order.iter().position(|o| o == id).map_or(usize::MAX, |p| p + 1);
                                l.rows.sort_by_key(|r| pos(&r.id));
                                l.dirty = true;
                            }
                            for id in fresh.iter().rev() {
                                if let Some(i) = l.index_of(id) {
                                    let mut r = l.rows.remove(i);
                                    r.enabled = true;
                                    l.rows.insert(0, r);
                                    l.dirty = true;
                                }
                            }
                        }
                        if let Some(first) = installed.first() {
                            self.selected = Some(first.clone());
                        }
                        self.stale = true;
                        for d in &done {
                            self.log(trf("Installed: {}", &[d]));
                        }
                        if !missing.is_empty() {
                            self.dialog = Some(Dialog::MissingDeps(missing));
                        }
                    }
                    Err(e) => self.error(e),
                }
            }
            Action::DiscardInstall => {
                if let Some(Dialog::InstallReview(p)) = self.dialog.take() {
                    p.discard();
                }
            }
            Action::AskUninstall(id) => {
                let lv = self.loader_version();
                let dependents = self.list.as_ref().map(|l| l.dependents(&id, lv.as_deref())).unwrap_or_default();
                self.dialog = Some(Dialog::ConfirmUninstall { id, dependents });
            }
            Action::Uninstall(id) => {
                self.dialog = None;
                let Some(root) = self.mods_root() else { return };
                match install::move_to_trash(&root, &id) {
                    Ok(p) => {
                        if let Some(l) = &mut self.list {
                            l.remove(&id);
                        }
                        // the lists on disk forget it too (other unsaved edits stay in memory)
                        if let Ok(Some(e)) = fmt::read_enabled(&root) {
                            let _ = fmt::write_atomic(&root.join(fmt::ENABLED_FILE), &fmt::enabled_text(&fmt::toggle_enabled(Some(e), &[], &id, false)));
                        }
                        if let Ok(Some(mut o)) = fmt::read_order(&root) {
                            o.retain(|x| x != &id);
                            let _ = fmt::write_atomic(&root.join(fmt::ORDER_FILE), &fmt::order_text(&o));
                        }
                        self.selected = None;
                        self.stale = true;
                        self.log(trf("Moved to {}", &[&p.display()]));
                    }
                    Err(e) => self.error(e),
                }
            }
            Action::ShowMissing(m) => self.dialog = Some(Dialog::MissingDeps(m)),
            Action::SwitchProfile(name) => {
                if self.list.as_ref().is_some_and(|l| l.dirty) {
                    self.dialog = Some(Dialog::UnsavedSwitch(name));
                } else {
                    self.run(Action::SwitchProfileNow(name, false), ctx);
                }
            }
            Action::SwitchProfileNow(name, save) => {
                self.dialog = None;
                if save {
                    self.run(Action::Save, ctx);
                }
                let Some(root) = self.mods_root() else { return };
                match fmt::switch_profile(&root, &name) {
                    Ok(()) => {
                        self.reload();
                        self.log(format!("{}: {}", tr("Profile"), profile_label(&name)));
                    }
                    Err(e) => self.error(e),
                }
            }
            Action::NewProfile(name) | Action::RenameProfile(_, name) if name.trim().is_empty() => {}
            Action::NewProfile(name) => {
                let Some(root) = self.mods_root() else { return };
                if self.list.as_ref().is_some_and(|l| l.dirty) {
                    self.run(Action::Save, ctx);
                }
                match fmt::create_profile(&root, &name) {
                    Ok(()) => {
                        self.dialog = None;
                        self.reload();
                    }
                    Err(e) => self.set_profile_error(e),
                }
            }
            Action::RenameProfile(old, new) => {
                let Some(root) = self.mods_root() else { return };
                match fmt::rename_profile(&root, &old, &new) {
                    Ok(()) => {
                        self.dialog = None;
                        self.profiles = fmt::profiles(&root);
                    }
                    Err(e) => self.set_profile_error(e),
                }
            }
            Action::DeleteProfile(name) => {
                self.dialog = None;
                let Some(root) = self.mods_root() else { return };
                match fmt::delete_profile(&root, &name) {
                    Ok(()) => self.reload(),
                    Err(e) => self.error(e),
                }
            }
            Action::LoaderInstall => {
                let (Some(g), Some(Ok(p))) = (self.game.clone(), self.payload.as_ref()) else { return };
                if game::game_running() {
                    self.error(tr("The game is running: changes apply at the next start."));
                    return;
                }
                let p = p.duplicate();
                self.spawn(tr("Installing…").into(), ctx, move |_| Msg::LoaderInstalled(modloader::install(&g, &p)));
            }
            Action::LoaderPick => {
                if let Some(p) = rfd::FileDialog::new().add_filter("ModLoader (.zip)", &["zip"]).pick_file() {
                    match modloader::from_path(&p) {
                        Ok(pl) => {
                            self.payload = Some(Ok(pl));
                            self.run(Action::LoaderInstall, ctx);
                        }
                        Err(e) => self.error(e),
                    }
                }
            }
            Action::LoaderRemove => {
                self.dialog = None;
                let Some(g) = self.game.clone() else { return };
                self.spawn(tr("Working…").into(), ctx, move |_| Msg::LoaderRemoved(modloader::remove(&g)));
            }
            Action::EnableModsModule => {
                if let Some(g) = self.game.clone() {
                    match modloader::enable_mods_module(&g) {
                        Ok(()) => self.reload_loader(),
                        Err(e) => self.error(e),
                    }
                }
            }
            Action::Register => match urlscheme::register_at(urlscheme::CLASSES, &self.exe) {
                Ok(()) => {
                    self.scheme_cmd = urlscheme::registered_command_at(urlscheme::CLASSES);
                    self.log(tr("Registered."));
                }
                Err(e) => self.error(e),
            },
            Action::Unregister => match urlscheme::unregister_at(urlscheme::CLASSES) {
                Ok(()) => {
                    self.scheme_cmd = None;
                    self.log(tr("Unregistered."));
                }
                Err(e) => self.error(e),
            },
            Action::EmptyTrash => {
                self.dialog = None;
                if let Some(root) = self.mods_root() {
                    if let Err(e) = install::empty_trash(&root) {
                        self.error(e);
                    }
                }
            }
            Action::Open(t) => {
                if let Err(e) = vr_modloader_app::shell_open(&t) {
                    self.error(e);
                }
            }
            Action::PickGame => {
                if let Some(d) = rfd::FileDialog::new().set_title(tr("Game folder")).pick_folder() {
                    self.run(Action::SetGame(d), ctx);
                }
            }
            Action::DetectGame => match game::find_games().into_iter().next() {
                Some(g) => self.run(Action::SetGame(g), ctx),
                None => self.error(tr("Game not found. Choose the folder that contains nie.exe.")),
            },
            Action::SetGame(d) => {
                self.game_override = false;
                self.set_game(d, ctx);
            }
        }
    }

    fn set_profile_error(&mut self, e: String) {
        if let Some(Dialog::ProfileName { error, .. }) = &mut self.dialog {
            *error = e;
        } else {
            self.error(e);
        }
    }

    // ------------------------------------------------------------ drawing

    fn header(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        ui.horizontal(|ui| {
            ui.label(RichText::new(APP_NAME).size(20.0).strong().color(ACCENT));
            ui.add_space(12.0);
            for (t, label) in [(Tab::Manager, tr("Manager")), (Tab::Studio, tr("Studio")), (Tab::Settings, tr("Settings"))] {
                if ui.selectable_label(self.tab == t, RichText::new(label).size(15.0)).clicked() {
                    self.tab = t;
                }
            }
        });
        ui.add_space(2.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("Game folder")).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button(tr("Change…")).clicked() {
                        acts.push(Action::PickGame);
                    }
                    if ui.button(tr("Detect")).clicked() {
                        acts.push(Action::DetectGame);
                    }
                    match &self.game {
                        Some(g) => {
                            match &self.game_version {
                                None => {
                                    ui.label(RichText::new(tr("checking version…")).weak());
                                    ui.spinner();
                                }
                                Some(Ok(true)) => {
                                    ui.label(RichText::new(tr("v7.1.2 (OK)")).strong().color(level_color(Level::Ok, ui.visuals().dark_mode)));
                                }
                                Some(Ok(false)) => {
                                    ui.label(RichText::new(tr("Not v7.1.2: the ModLoader stays inactive on this build")).color(level_color(Level::Error, true)));
                                }
                                Some(Err(e)) => {
                                    ui.label(RichText::new(e).color(level_color(Level::Error, true)));
                                }
                            }
                            if self.game_override {
                                ui.label(RichText::new(tr("(command-line override, not saved)")).small().weak());
                            }
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.add(egui::Label::new(RichText::new(g.display().to_string()).monospace()).truncate());
                            });
                        }
                        None => {
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.label(RichText::new(tr("No game folder selected")).color(level_color(Level::Warn, true)));
                            });
                        }
                    }
                });
            });
            if self.game.is_some() {
                ui.separator();
                self.loader_card(ui, acts);
            }
        });
        if self.game_running {
            ui.label(RichText::new(tr("The game is running: changes apply at the next start.")).color(level_color(Level::Warn, true)));
        }
    }

    fn loader_card(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let dark = ui.visuals().dark_mode;
        let busy = self.busy.is_some();
        let Some(st) = self.loader.clone() else { return };
        let need = self.list.as_ref().map(|l| evt_installer::modpack::max_version(l.rows.iter().filter(|r| r.enabled).map(|r| r.info.manifest.loader_min.as_str()))).unwrap_or_default();
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("ModLoader")).strong());
            let (text, lvl) = match (&st.state, st.version()) {
                (evt_installer::modpack::LoaderState::Missing, _) => (tr("Not installed").to_string(), Level::Error),
                (_, Some(v)) => (trf("Installed {}", &[&v]), Level::Ok),
                (_, None) => (tr("Installed (unknown version)").to_string(), Level::Warn),
            };
            ui.label(RichText::new(text).color(level_color(lvl, dark)));
            if let Some(v) = st.version() {
                if !need.is_empty() && fmt::compare_versions(v, &need).is_lt() {
                    ui.label(RichText::new(trf("Outdated: a mod needs {}", &[&need])).color(level_color(Level::Error, dark)));
                }
            }
            let pv = self.payload.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.version.clone());
            if let Some(pv) = &pv {
                if st.update_available(pv) {
                    ui.label(RichText::new(trf("Update available: {}", &[pv])).color(level_color(Level::Warn, dark)));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if st.installed() {
                    let r = ui.add_enabled(!busy && st.ours, egui::Button::new(tr("Remove")));
                    let r = if st.ours { r } else { r.on_disabled_hover_text(tr("installed by another tool: remove it with that tool")) };
                    if r.clicked() {
                        self.dialog = Some(Dialog::ConfirmRemoveLoader);
                    }
                }
                if ui.add_enabled(!busy, egui::Button::new(tr("Install ModLoader from file…"))).clicked() {
                    acts.push(Action::LoaderPick);
                }
                if let Some(pv) = &pv {
                    let label = if !st.installed() {
                        tr("Install")
                    } else if st.update_available(pv) {
                        tr("Update")
                    } else {
                        tr("Reinstall")
                    };
                    let enabled = !busy && (label != tr("Reinstall"));
                    let src = self.payload.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.source.clone()).unwrap_or_default();
                    let r = ui.add_enabled(enabled, egui::Button::new(RichText::new(format!("{label} {pv}")).strong()));
                    if r.on_hover_text(trf("Package: {}", &[&src])).clicked() {
                        acts.push(Action::LoaderInstall);
                    }
                } else {
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(tr("No ModLoader package found (embedded, modloader\\ or modloader.zip next to the exe).")).small().weak()).truncate());
                    });
                }
            });
        });
        if st.mods_module == Some(false) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("The mods module is off in evt_loader\\config.toml: mods are ignored.")).color(level_color(Level::Warn, dark)));
                if ui.button(tr("Turn on")).clicked() {
                    acts.push(Action::EnableModsModule);
                }
            });
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let busy = self.busy.is_some();
        let dirty = self.list.as_ref().is_some_and(|l| l.dirty);
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr("Profile")).strong());
            let (names, cur) = self.profiles.clone();
            let cur_name = names.get(cur).cloned().unwrap_or_else(|| fmt::DEFAULT_PROFILE.to_string());
            egui::ComboBox::from_id_salt("profile").width(170.0).selected_text(profile_label(&cur_name)).show_ui(ui, |ui| {
                for (i, n) in names.iter().enumerate() {
                    if ui.selectable_label(i == cur, profile_label(n)).clicked() && i != cur {
                        acts.push(Action::SwitchProfile(n.clone()));
                    }
                }
            });
            if ui.button(tr("New…")).clicked() {
                self.dialog = Some(Dialog::ProfileName { rename: None, text: String::new(), error: String::new() });
            }
            let custom = cur_name != fmt::DEFAULT_PROFILE;
            if ui.add_enabled(custom, egui::Button::new(tr("Rename…"))).clicked() {
                self.dialog = Some(Dialog::ProfileName { rename: Some(cur_name.clone()), text: cur_name.clone(), error: String::new() });
            }
            if ui.add_enabled(custom, egui::Button::new(tr("Delete"))).clicked() {
                self.dialog = Some(Dialog::ConfirmDeleteProfile(cur_name.clone()));
            }
            ui.separator();
            if ui.add_enabled(!busy, egui::Button::new(tr("Install mod…"))).clicked() {
                acts.push(Action::PickArchive);
            }
            if ui.add_enabled(!busy, egui::Button::new(tr("Refresh"))).clicked() {
                acts.push(Action::Refresh);
            }
            if let Some(root) = self.mods_root() {
                if ui.button(tr("Open mods folder")).clicked() {
                    let _ = std::fs::create_dir_all(&root);
                    acts.push(Action::Open(root.display().to_string()));
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let launch = egui::Button::new(RichText::new(format!("▶ {}", tr("Launch game"))).strong().color(Color32::WHITE)).fill(Color32::from_rgb(40, 150, 80));
                if ui.add_enabled(!busy, launch).clicked() {
                    acts.push(Action::Launch);
                }
                let save = egui::Button::new(RichText::new(tr("Save")).strong()).fill(if dirty { ACCENT } else { ui.visuals().widgets.inactive.weak_bg_fill });
                if ui.add_enabled(dirty && !busy, save).clicked() {
                    acts.push(Action::Save);
                }
                if dirty {
                    ui.label(RichText::new(tr("Unsaved changes")).color(level_color(Level::Warn, true)));
                }
            });
        });
    }

    fn mod_list(&mut self, ui: &mut Ui) {
        let dark = ui.visuals().dark_mode;
        let Some(list) = self.list.as_mut() else { return };
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} ({}/{})", tr("Mods"), self.derived.active, list.rows.len())).strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button(tr("Disable all")).clicked() {
                    list.set_all(false);
                    self.stale = true;
                }
                if ui.small_button(tr("Enable all")).clicked() {
                    list.set_all(true);
                    self.stale = true;
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(RichText::new(tr("Top = highest priority (loads last, wins conflicts). Drag a row to reorder.")).small().weak()).truncate());
                });
            });
        });
        if list.rows.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new(tr("No mods installed. Use «Install mod…» or drop a .zip on the window.")).weak()));
            return;
        }
        let row_h = 30.0;
        let w = ui.available_width();
        let cols = [26.0, 28.0, (w - 26.0 - 28.0 - 90.0 - 150.0 - 130.0).max(160.0), 90.0, 150.0, 130.0];
        // header
        let (hr, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
        let hdr_color = ui.visuals().weak_text_color();
        let mut x = hr.left();
        for (i, name) in ["", tr("On"), tr("Name"), tr("Version"), tr("Author"), tr("Status")].iter().enumerate() {
            ui.painter().text(egui::pos2(x + 4.0, hr.center().y), Align2::LEFT_CENTER, *name, FontId::proportional(12.5), hdr_color);
            x += cols[i];
        }
        let mut row_rects: Vec<egui::Rect> = Vec::with_capacity(list.rows.len());
        let mut toggles: Vec<(usize, bool)> = Vec::new();
        let mut clicked: Option<String> = None;
        let mut drag_started: Option<usize> = None;
        egui::ScrollArea::vertical().id_salt("mods").auto_shrink([false, false]).show(ui, |ui| {
            for (i, row) in list.rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::click_and_drag());
                row_rects.push(rect);
                let selected = self.selected.as_deref() == Some(row.id.as_str());
                let painter = ui.painter();
                let bg = if selected {
                    ACCENT.gamma_multiply(0.35)
                } else if resp.hovered() {
                    ui.visuals().widgets.hovered.weak_bg_fill
                } else if i % 2 == 1 {
                    ui.visuals().faint_bg_color
                } else {
                    Color32::TRANSPARENT
                };
                painter.rect_filled(rect, 3.0, bg);
                // grip
                let gx = rect.left() + 9.0;
                for k in 0..3 {
                    let y = rect.center().y - 5.0 + k as f32 * 5.0;
                    painter.line_segment([egui::pos2(gx, y), egui::pos2(gx + 10.0, y)], Stroke::new(1.5, ui.visuals().weak_text_color()));
                }
                let st = self.derived.statuses.get(i);
                let lvl = st.map_or(Level::Ok, |s| s.level);
                let text_color = if row.enabled { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
                let mut x = rect.left() + cols[0];
                let mut on = row.enabled;
                let cb = ui.put(egui::Rect::from_min_size(egui::pos2(x + 4.0, rect.top() + 5.0), Vec2::new(20.0, 20.0)), egui::Checkbox::without_text(&mut on));
                if cb.changed() {
                    toggles.push((i, on));
                }
                x += cols[1];
                let name = if row.info.manifest.name.trim().is_empty() { row.id.clone() } else { row.info.manifest.name.clone() };
                let cells = [
                    (name, cols[2], text_color, true),
                    (row.info.manifest.version.clone(), cols[3], text_color, false),
                    (row.info.manifest.author.clone(), cols[4], text_color, false),
                    (model::level_label(lvl).to_string(), cols[5], level_color(lvl, dark), false),
                ];
                for (k, (text, cw, color, strong)) in cells.into_iter().enumerate() {
                    let mut r = egui::Rect::from_min_size(egui::pos2(x + 4.0, rect.top()), Vec2::new(cw - 8.0, row_h));
                    if k == 3 {
                        ui.painter().circle_filled(egui::pos2(r.left() + 5.0, rect.center().y), 4.5, color);
                        r.min.x += 16.0;
                    }
                    let rt = if strong { RichText::new(text).color(color).strong() } else { RichText::new(text).color(color) };
                    cell(ui, r, rt);
                    x += cw;
                }
                if resp.clicked() {
                    clicked = Some(row.id.clone());
                }
                if resp.drag_started() {
                    drag_started = Some(i);
                }
                if let Some(s) = st.filter(|s| !s.lines.is_empty()) {
                    let tip: Vec<String> = s.lines.iter().map(|(_, l)| l.clone()).collect();
                    resp.on_hover_text(tip.join("\n"));
                }
            }
        });
        for (i, on) in toggles {
            list.set_enabled(i, on);
            self.stale = true;
        }
        if let Some(c) = clicked {
            self.selected = Some(c);
        }
        if let Some(i) = drag_started {
            self.drag_from = Some(i);
            self.selected = Some(list.rows[i].id.clone());
        }
        // drop target while dragging
        if let Some(from) = self.drag_from {
            let ptr = ui.ctx().pointer_interact_pos();
            let target = ptr.map(|p| {
                row_rects.iter().position(|r| p.y < r.center().y).unwrap_or(row_rects.len())
            });
            if let (Some(t), Some(last)) = (target, row_rects.last()) {
                let y = if t < row_rects.len() { row_rects[t].top() } else { last.bottom() };
                let (l, r) = (last.left(), last.right());
                ui.painter().line_segment([egui::pos2(l, y), egui::pos2(r, y)], Stroke::new(2.5, ACCENT));
                ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
            }
            if ui.input(|i| !i.pointer.any_down()) {
                if let Some(t) = target {
                    let before = list.order_list();
                    list.move_row(from, t);
                    if list.order_list() != before {
                        self.stale = true;
                    }
                }
                self.drag_from = None;
            }
        }
        // keyboard: up / down moves the selected mod
        if let Some(sel) = self.selected.clone() {
            if let Some(i) = list.index_of(&sel) {
                let (up, down) = ui.input(|inp| (inp.modifiers.alt && inp.key_pressed(egui::Key::ArrowUp), inp.modifiers.alt && inp.key_pressed(egui::Key::ArrowDown)));
                if up && i > 0 {
                    list.move_row(i, i - 1);
                    self.stale = true;
                } else if down && i + 1 < list.rows.len() {
                    list.move_row(i, i + 2);
                    self.stale = true;
                }
            }
        }
    }

    fn preview(&mut self, ctx: &egui::Context, p: &Path) -> Option<egui::TextureHandle> {
        if let Some(t) = self.previews.get(p) {
            return t.clone();
        }
        let tex = image::open(p).ok().map(|img| {
            let img = if img.width() > 640 { img.thumbnail(640, 640) } else { img };
            let rgba = img.to_rgba8();
            let ci = egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw());
            ctx.load_texture(p.display().to_string(), ci, egui::TextureOptions::LINEAR)
        });
        self.previews.insert(p.to_path_buf(), tex.clone());
        tex
    }

    fn details(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let dark = ui.visuals().dark_mode;
        let Some((row, st)) = self.selected.as_ref().and_then(|s| {
            let l = self.list.as_ref()?;
            let i = l.index_of(s)?;
            Some((l.rows[i].clone(), self.derived.statuses.get(i).cloned()))
        }) else {
            ui.add_space(20.0);
            ui.label(RichText::new(tr("Select a mod to see its details.")).weak());
            return;
        };
        let m = &row.info.manifest;
        egui::ScrollArea::vertical().id_salt("details").auto_shrink([false, false]).show(ui, |ui| {
            let title = if m.name.trim().is_empty() { m.id.clone() } else { m.name.clone() };
            ui.label(RichText::new(title).size(19.0).strong());
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("v{}", m.version)).strong());
                if !m.author.is_empty() {
                    ui.label(trf("by {}", &[&m.author]));
                }
            });
            if let Some(p) = row.info.preview_png.clone() {
                if let Some(t) = self.preview(ui.ctx(), &p) {
                    let w = ui.available_width().min(460.0);
                    let s = t.size_vec2();
                    ui.add(egui::Image::new(&t).fit_to_exact_size(Vec2::new(w, w * s.y / s.x.max(1.0))).corner_radius(4.0));
                }
            }
            if !m.description.trim().is_empty() {
                ui.add_space(4.0);
                ui.label(m.description.trim());
            }
            if let Some(st) = &st {
                if !st.lines.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(tr("Problems")).strong());
                    for (l, text) in &st.lines {
                        ui.label(RichText::new(format!("•  {text}")).color(level_color(*l, dark)));
                    }
                    if !st.missing.is_empty() {
                        let miss: Vec<(String, String, Vec<String>)> = st.missing.iter().map(|(id, c)| (id.clone(), c.clone(), vec![m.id.clone()])).collect();
                        if ui.button(tr("Install missing…")).clicked() {
                            acts.push(Action::ShowMissing(miss));
                        }
                    }
                }
            }
            ui.add_space(6.0);
            egui::Grid::new("info").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                let vw = (ui.available_width() - 110.0).max(120.0);
                let mut kv = |k: &str, v: String| {
                    if !v.trim().is_empty() {
                        ui.label(RichText::new(k).weak());
                        ui.add_sized([vw, 18.0], egui::Label::new(v.clone()).truncate()).on_hover_text(v);
                        ui.end_row();
                    }
                };
                kv("id", m.id.clone());
                kv(tr("Requires"), m.requires.join(", "));
                kv(tr("Provides"), m.provides.join(", "));
                kv(tr("Incompatible with"), m.conflicts.join(", "));
                kv(tr("Needs ModLoader"), m.loader_min.clone());
                kv(tr("Plugin"), m.plugin.clone());
                kv(tr("Tags"), m.tags.join(", "));
                kv(tr("Updated"), row.info.updated());
                let deltas: usize = row.info.deltas.iter().map(|d| d.delta.set.len() + d.delta.add.len()).sum();
                kv(tr("Contents"), trf("{} files, {} Lua scripts, {} data deltas", &[&row.info.files.len(), &row.info.lua_scripts.len(), &deltas]));
                kv(tr("Folder"), row.info.dir.display().to_string());
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button(tr("Open folder")).clicked() {
                    acts.push(Action::Open(row.info.dir.display().to_string()));
                }
                if ui.add_enabled(self.busy.is_none(), egui::Button::new(RichText::new(tr("Uninstall…")).color(level_color(Level::Error, dark)))).clicked() {
                    acts.push(Action::AskUninstall(m.id.clone()));
                }
            });
        });
    }

    fn bottom_panel(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let dark = ui.visuals().dark_mode;
        ui.horizontal(|ui| {
            let nc = self.derived.conflicts.len();
            let np = self.derived.problems.len();
            ui.selectable_value(&mut self.bottom, Bottom::Conflicts, format!("{} ({nc})", tr("Conflicts")));
            ui.selectable_value(&mut self.bottom, Bottom::Problems, format!("{} ({np})", tr("Problems")));
            ui.selectable_value(&mut self.bottom, Bottom::Log, tr("Log"));
            if !self.derived.missing.is_empty() {
                ui.separator();
                let n = self.derived.missing.len();
                if ui.button(RichText::new(format!("{} ({n})", tr("Install missing…"))).color(level_color(Level::Error, dark))).clicked() {
                    acts.push(Action::ShowMissing(self.derived.missing.clone()));
                }
            }
            if self.bottom == Bottom::Conflicts {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.conflict_filter).desired_width(180.0).hint_text(tr("Filter")));
                });
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().id_salt("bottom").auto_shrink([false, false]).show(ui, |ui| match self.bottom {
            Bottom::Conflicts => {
                if self.derived.conflicts.is_empty() {
                    ui.label(RichText::new(tr("No conflicts between the enabled mods.")).weak());
                    return;
                }
                let f = self.conflict_filter.to_lowercase();
                egui::Grid::new("conf").striped(true).num_columns(4).spacing([16.0, 3.0]).show(ui, |ui| {
                    ui.label(RichText::new(tr("What")).strong());
                    ui.label("");
                    ui.label(RichText::new(tr("Mods (load order)")).strong());
                    ui.label(RichText::new(tr("Winner")).strong());
                    ui.end_row();
                    for c in self.derived.conflicts.iter().filter(|c| f.is_empty() || c.what.to_lowercase().contains(&f) || c.mods.iter().any(|m| m.contains(&f))) {
                        ui.label(RichText::new(&c.kind).weak());
                        ui.label(RichText::new(&c.what).monospace());
                        ui.label(c.mods.join("  >  "));
                        ui.label(RichText::new(c.winner()).strong());
                        ui.end_row();
                    }
                });
                if self.derived.conflicts.iter().any(|c| c.kind.starts_with("audio")) {
                    ui.label(RichText::new(tr("Audio declarations (audio.toml): the mod loaded last is expected to win.")).small().weak());
                }
            }
            Bottom::Problems => {
                if self.derived.problems.is_empty() {
                    ui.label(RichText::new(tr("No problems.")).weak());
                }
                for (l, t) in &self.derived.problems {
                    ui.label(RichText::new(format!("•  {t}")).color(level_color(*l, dark)));
                }
            }
            Bottom::Log => {
                for l in &self.log {
                    ui.label(RichText::new(l).monospace().small());
                }
            }
        });
    }

    fn open_index(&mut self, ctx: &egui::Context) {
        let Some(g) = self.game.clone() else { return };
        let c = ctx.clone();
        let label = tr("Reading your game data…").to_string();
        self.spawn(label.clone(), ctx, move |tx| {
            let tx2 = std::sync::Mutex::new(tx.clone());
            let progress = move |step: &str, detail: &str| {
                if let Ok(t) = tx2.lock() {
                    let _ = t.send(Msg::Progress(format!("{label} {step} {detail}"), None));
                }
                c.request_repaint();
            };
            let r = vr_index::Index::open_or_build(&g, vr_index::default_out_dir(), &vr_index::BuildOptions::default(), &progress)
                .map(|(i, rebuilt)| (Arc::new(i), rebuilt))
                .map_err(|e| e.to_string());
            Msg::Index(r)
        });
    }

    fn studio(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(8.0);
        ui.label(RichText::new(tr("Studio (coming soon)")).size(20.0).strong());
        ui.label(tr("Create and edit mods with forms: characters, techniques, teams, texts… It reads your own game data (v7.1.2) to build a local index, so you pick game things from lists instead of typing ids. Nothing from the game ships with this program."));
        ui.add_space(8.0);
        ui.separator();
        ui.heading(tr("Game index"));
        let Some(idx) = self.studio.index.clone() else {
            ui.label(tr("The index is built once from your game (a few minutes; read only) and stored in %LOCALAPPDATA%\\VR-ModLoader\\index."));
            if ui.add_enabled(self.game.is_some() && self.busy.is_none(), egui::Button::new(RichText::new(tr("Open / build the index")).strong())).clicked() {
                self.open_index(&ctx);
            }
            return;
        };
        let lang = if i18n::lang() == Lang::Es { 2 } else { 1 };
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.studio.query).desired_width(280.0).hint_text(tr("Search names or ids (any language)")));
            let cat_label = self.studio.category.map(|c| c.as_str().to_string()).unwrap_or_else(|| tr("All").to_string());
            egui::ComboBox::from_id_salt("cat").selected_text(cat_label).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.studio.category, None, tr("All"));
                for c in vr_index::Category::ALL {
                    ui.selectable_value(&mut self.studio.category, Some(c), c.as_str());
                }
            });
            let m = idx.meta();
            ui.label(RichText::new(format!("{} · {}", trf("{} entries", &[&idx.entities().len()]), m.game_version.clone().unwrap_or_default())).weak());
        });
        let key = (self.studio.query.trim().to_string(), self.studio.category, lang);
        if self.studio.searched.as_ref() != Some(&key) {
            self.studio.hits = if key.0.is_empty() {
                Vec::new()
            } else {
                let mut q = vr_index::Query::new(key.0.clone()).lang(lang).limit(200);
                if let Some(c) = key.1 {
                    q = q.category(c);
                }
                idx.search(&q)
            };
            self.studio.searched = Some(key);
            self.studio.selected = None;
        }
        ui.separator();
        let rows: Vec<(Option<std::path::PathBuf>, String, String, String)> = self
            .studio
            .hits
            .iter()
            .map(|h| {
                let e = &idx.entities()[h.index];
                (idx.thumb_path(e), h.name.clone(), h.id.clone(), format!("{}  {}", h.category.as_str(), idx.detail(e)))
            })
            .collect();
        let rows: Vec<_> = rows.into_iter().map(|(p, n, id, d)| (p.and_then(|p| self.preview(&ctx, &p)), n, id, d)).collect();
        egui::ScrollArea::vertical().id_salt("hits").auto_shrink([false, false]).show(ui, |ui| {
            egui::Grid::new("hits_grid").striped(true).num_columns(4).spacing([14.0, 4.0]).show(ui, |ui| {
                for (k, (tex, name, id, detail)) in rows.iter().enumerate() {
                    match tex {
                        Some(t) => {
                            ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::splat(28.0)));
                        }
                        None => {
                            ui.label("");
                        }
                    }
                    if ui.selectable_label(self.studio.selected == Some(k), RichText::new(name).strong()).clicked() {
                        self.studio.selected = Some(k);
                    }
                    ui.label(RichText::new(id).monospace());
                    ui.label(RichText::new(detail).weak());
                    ui.end_row();
                }
            });
        });
    }

    fn settings_tab(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(760.0);
            ui.heading(tr("Language"));
            let cur = i18n::lang();
            ui.horizontal(|ui| {
                for l in [Lang::En, Lang::Es] {
                    if ui.selectable_label(cur == l, l.label()).clicked() && cur != l {
                        i18n::set_lang(l);
                        self.settings.language = l.code().into();
                        self.save_settings();
                        self.stale = true;
                    }
                }
            });
            ui.add_space(14.0);
            ui.heading(tr("1-click install (GameBanana / Nexus style links)"));
            ui.label(tr("Registers the vrmodloader: link type for your Windows user (HKCU\\Software\\Classes\\vrmodloader), so «1-click install» buttons on mod sites open this program. Nothing is registered without this button."));
            let status = match &self.scheme_cmd {
                Some(c) if urlscheme::command_is(c, &self.exe) => tr("Status: registered to this program").to_string(),
                Some(c) => trf("Status: registered to another program: {}", &[c]),
                None => tr("Status: not registered").to_string(),
            };
            ui.label(RichText::new(status).strong());
            ui.horizontal(|ui| {
                if ui.button(tr("Register 1-click links")).clicked() {
                    acts.push(Action::Register);
                }
                if ui.add_enabled(self.scheme_cmd.is_some(), egui::Button::new(tr("Unregister"))).clicked() {
                    acts.push(Action::Unregister);
                }
            });
            if let Some(root) = self.mods_root() {
                ui.add_space(14.0);
                ui.heading(tr("Trash"));
                let n = install::trash_items(&root);
                ui.label(trf("{} item(s) in mods\\_trash\\", &[&n]));
                ui.horizontal(|ui| {
                    if ui.add_enabled(n > 0, egui::Button::new(tr("Open trash"))).clicked() {
                        acts.push(Action::Open(install::trash_root(&root).display().to_string()));
                    }
                    if ui.add_enabled(n > 0, egui::Button::new(tr("Empty trash…"))).clicked() {
                        self.dialog = Some(Dialog::ConfirmEmptyTrash);
                    }
                });
            }
            ui.add_space(14.0);
            ui.heading(tr("About"));
            ui.label(format!("{APP_NAME} {}", vr_modloader_app::APP_VERSION));
            ui.label(tr("Mod manager for the VR-ModLoader (Inazuma Eleven Victory Road PC v7.1.2). Free software under the GPL-3.0."));
            ui.label(RichText::new(settings::settings_path().display().to_string()).small().weak());
        });
    }

    fn dialogs(&mut self, ctx: &egui::Context, acts: &mut Vec<Action>) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut keep = true;
        let dark = ctx.global_style().visuals.dark_mode;
        egui::Modal::new(Id::new("dialog")).show(ctx, |ui| {
            ui.set_max_width(640.0);
            match &mut d {
                Dialog::Message { title, body } => {
                    ui.set_width(520.0);
                    ui.heading(title.as_str());
                    ui.label(body.as_str());
                    if ui.button(tr("Close")).clicked() {
                        keep = false;
                    }
                }
                Dialog::InstallReview(p) => {
                    ui.set_width(600.0);
                    ui.heading(tr("Install mods"));
                    let arch = p.archive.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.add(egui::Label::new(RichText::new(arch).weak()).truncate()).on_hover_text(p.archive.display().to_string());
                    ui.separator();
                    egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                        for c in p.candidates.iter_mut() {
                            let ok = c.staged.ok();
                            ui.horizontal(|ui| {
                                ui.add_enabled(ok, egui::Checkbox::without_text(&mut c.selected));
                                let folder = c.staged.dir.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                                let name = c.staged.manifest.as_ref().map(|m| format!("{} ({}) {}", m.name, m.id, m.version)).unwrap_or(folder);
                                let (t, l) = match &c.existing {
                                    Existing::New => (tr("new").to_string(), Level::Ok),
                                    Existing::Upgrade(v) => (trf("upgrade from {}", &[v]), Level::Ok),
                                    Existing::Same(v) => (trf("same version {} (reinstall)", &[v]), Level::Warn),
                                    Existing::Downgrade(v) => (trf("downgrade from {}", &[v]), Level::Warn),
                                };
                                if ok {
                                    ui.label(RichText::new(t).color(level_color(l, dark)));
                                }
                                ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                            });
                            if !ok {
                                ui.label(RichText::new(tr("Errors: this mod cannot be installed")).color(level_color(Level::Error, dark)));
                            }
                            for e in &c.staged.errors {
                                ui.label(RichText::new(format!("  •  {e}")).color(level_color(Level::Error, dark)).small());
                            }
                            for w in c.staged.warnings.iter().take(8) {
                                ui.label(RichText::new(format!("  •  {w}")).color(level_color(Level::Warn, dark)).small());
                            }
                            if let Some(m) = &c.staged.manifest {
                                if !m.description.is_empty() {
                                    ui.label(RichText::new(m.description.lines().next().unwrap_or("")).small());
                                }
                            }
                            ui.add_space(4.0);
                        }
                    });
                    if p.candidates.iter().any(|c| c.selected && !matches!(c.existing, Existing::New)) {
                        ui.label(RichText::new(tr("The installed copy is moved to mods\\_trash\\ first.")).small().weak());
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        let any = p.candidates.iter().any(|c| c.selected && c.staged.ok());
                        if ui.add_enabled(any, egui::Button::new(RichText::new(tr("Install selected")).strong())).clicked() {
                            acts.push(Action::CommitInstall);
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            acts.push(Action::DiscardInstall);
                        }
                    });
                }
                Dialog::MissingDeps(miss) => {
                    ui.set_width(520.0);
                    ui.heading(tr("Missing dependencies"));
                    ui.label(tr("These mods need other mods that are not installed:"));
                    ui.separator();
                    for (id, c, by) in miss.iter() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{id} {c}")).strong());
                            ui.label(RichText::new(trf("needed by {}", &[&by.join(", ")])).weak());
                        });
                        if let Some(e) = catalog::find(&self.catalog, id) {
                            ui.horizontal(|ui| {
                                if !e.download.is_empty() && ui.button(tr("Download and install")).clicked() {
                                    acts.push(Action::Download(e.download.clone()));
                                    keep = false;
                                }
                                if !e.page.is_empty() && ui.button(tr("Open page")).clicked() {
                                    acts.push(Action::Open(e.page.clone()));
                                }
                            });
                        }
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button(tr("Install from file…")).clicked() {
                            acts.push(Action::PickArchive);
                            keep = false;
                        }
                        if ui.button(tr("Later")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmUninstall { id, dependents } => {
                    ui.heading(trf("Uninstall «{}»?", &[id]));
                    ui.label(tr("The folder is moved to mods\\_trash\\ (you can restore it by moving it back)."));
                    if !dependents.is_empty() {
                        ui.label(RichText::new(trf("These enabled mods need it: {}", &[&dependents.join(", ")])).color(level_color(Level::Warn, dark)));
                    }
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Uninstall")).color(level_color(Level::Error, dark))).clicked() {
                            acts.push(Action::Uninstall(id.clone()));
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ProfileName { rename, text, error } => {
                    ui.heading(if rename.is_some() { tr("Rename profile") } else { tr("New profile") });
                    ui.horizontal(|ui| {
                        ui.label(tr("Name"));
                        let r = ui.add(egui::TextEdit::singleline(text).desired_width(260.0).char_limit(32));
                        r.request_focus();
                    });
                    if !error.is_empty() {
                        ui.label(RichText::new(error.as_str()).color(level_color(Level::Error, dark)));
                    }
                    ui.horizontal(|ui| {
                        let label = if rename.is_some() { tr("Rename") } else { tr("Create") };
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.button(RichText::new(label).strong()).clicked() || enter {
                            match rename {
                                Some(old) => acts.push(Action::RenameProfile(old.clone(), text.trim().to_string())),
                                None => acts.push(Action::NewProfile(text.trim().to_string())),
                            }
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmDeleteProfile(name) => {
                    ui.heading(trf("Delete profile «{}»?", &[name]));
                    ui.horizontal(|ui| {
                        if ui.button(tr("Delete")).clicked() {
                            acts.push(Action::DeleteProfile(name.clone()));
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::UnsavedSwitch(name) => {
                    ui.heading(tr("Unsaved changes"));
                    ui.label(tr("Unsaved changes: save before switching profile?"));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Save and switch")).strong()).clicked() {
                            acts.push(Action::SwitchProfileNow(name.clone(), true));
                        }
                        if ui.button(tr("Discard and switch")).clicked() {
                            acts.push(Action::SwitchProfileNow(name.clone(), false));
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmLink(link) => {
                    ui.set_width(520.0);
                    ui.heading(tr("Install from link"));
                    ui.label(tr("A website asks to download and install a mod from:"));
                    ui.label(RichText::new(link.host()).size(17.0).strong());
                    ui.add(egui::Label::new(RichText::new(&link.url).monospace().small()).wrap());
                    if let Some((t, id)) = &link.item {
                        ui.label(RichText::new(format!("GameBanana {t} #{id}")).weak());
                    }
                    ui.label(RichText::new(tr("Only continue if you trust this site. The archive is checked before anything is installed.")).color(level_color(Level::Warn, dark)));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Download")).strong()).clicked() {
                            acts.push(Action::Download(link.url.clone()));
                            keep = false;
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmRemoveLoader => {
                    ui.heading(tr("ModLoader"));
                    ui.label(tr("Remove the ModLoader? Its backup puts back the files it replaced. Your mods folder is kept."));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Remove")).color(level_color(Level::Error, dark))).clicked() {
                            acts.push(Action::LoaderRemove);
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
                Dialog::ConfirmEmptyTrash => {
                    ui.heading(tr("Trash"));
                    ui.label(tr("Permanently delete everything in mods\\_trash\\? This cannot be undone."));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Delete permanently")).color(level_color(Level::Error, dark))).clicked() {
                            acts.push(Action::EmptyTrash);
                        }
                        if ui.button(tr("Cancel")).clicked() {
                            keep = false;
                        }
                    });
                }
            }
        });
        if keep && self.dialog.is_none() {
            self.dialog = Some(d);
        }
    }
}

/// A left-aligned, truncated label in `r`.
fn cell(ui: &mut Ui, r: egui::Rect, text: RichText) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(Layout::left_to_right(Align::Center)), |ui| {
        ui.add(egui::Label::new(text).truncate().selectable(false));
    });
}

fn profile_label(name: &str) -> String {
    if name == fmt::DEFAULT_PROFILE {
        tr("Default").to_string()
    } else {
        name.to_string()
    }
}

impl eframe::App for ManagerApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_msgs(ctx);
        if let Some(open) = self.pending_open.take() {
            if open.to_ascii_lowercase().starts_with(&format!("{}:", urlscheme::SCHEME)) {
                match urlscheme::parse_link(&open) {
                    Ok(l) => self.dialog = Some(Dialog::ConfirmLink(l)),
                    Err(e) => self.error(trf("Invalid link: {}", &[&e])),
                }
            } else {
                self.run(Action::InstallArchive(PathBuf::from(open)), ctx);
            }
        }
        // files dropped on the window
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if let Some(p) = dropped.into_iter().next() {
            if self.busy.is_none() && self.dialog.is_none() {
                self.run(Action::InstallArchive(p), ctx);
            }
        }
        self.recompute();
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let mut acts: Vec<Action> = Vec::new();
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(4.0);
            self.header(ui, &mut acts);
            if self.tab == Tab::Manager && self.game.is_some() {
                ui.add_space(2.0);
                self.toolbar(ui, &mut acts);
            }
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                if let Some((label, f)) = &self.busy {
                    ui.spinner();
                    ui.label(label.as_str());
                    if let Some(f) = f {
                        ui.add(egui::ProgressBar::new(*f).desired_width(220.0).show_percentage());
                    }
                } else {
                    ui.label(RichText::new(&self.status_line).weak());
                }
            });
        });
        match self.tab {
            Tab::Manager if self.game.is_some() => {
                egui::Panel::bottom("conflicts").resizable(true).default_size(190.0).min_size(90.0).show(ui, |ui| self.bottom_panel(ui, &mut acts));
                egui::Panel::right("details").resizable(true).default_size(380.0).min_size(260.0).max_size(640.0).show(ui, |ui| self.details(ui, &mut acts));
                egui::CentralPanel::default().show(ui, |ui| self.mod_list(ui));
            }
            Tab::Manager => {
                egui::CentralPanel::default().show(ui, |ui| {
                    ui.add_space(40.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(tr("Game not found. Choose the folder that contains nie.exe.")).size(16.0));
                        if ui.button(tr("Change…")).clicked() {
                            acts.push(Action::PickGame);
                        }
                    });
                });
            }
            Tab::Studio => {
                egui::CentralPanel::default().show(ui, |ui| self.studio(ui));
            }
            Tab::Settings => {
                egui::CentralPanel::default().show(ui, |ui| self.settings_tab(ui, &mut acts));
            }
        }
        // hovering files: drop hint
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("drop")));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(170));
            painter.text(screen.center(), Align2::CENTER_CENTER, tr("Drop a .zip here to install it"), FontId::proportional(26.0), Color32::WHITE);
        }
        self.dialogs(&ctx, &mut acts);
        for a in acts {
            self.run(a, &ctx);
        }
    }
}
