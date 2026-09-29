//! The window (egui / eframe). All file work goes through the library modules; slow work (hashing nie.exe,
//! downloads, unpacking, ModLoader install) runs on a thread and reports back through a channel.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;

use eframe::egui::{self, Align, Align2, Color32, FontId, Id, Layout, Margin, RichText, Sense, Stroke, Ui, Vec2};
use evt_installer::game;
use evt_modfmt as fmt;
use vr_modloader_app::i18n::{self, tr, trf, Lang};
use vr_modloader_app::install::{self, Existing, Prepared};
use vr_modloader_app::model::{self, ConflictRow, Level, ModList, Status};
use vr_modloader_app::modloader::{self, Payload};
use vr_modloader_app::settings::{self, Settings};
use vr_modloader_app::urlscheme::{self, Link};
use vr_modloader_app::{audio, catalog, mods_root, APP_NAME};

use crate::theme;

#[derive(Debug, Default, Clone)]
pub struct Args {
    pub game_dir: Option<PathBuf>,
    pub lang: Option<String>,
    pub tab: Option<String>,
    pub select: Option<String>,
    /// `--section`: which part of «More» to show (conflicts, problems, log, profiles, game, links, trash, language, about).
    pub section: Option<String>,
    /// A `vrmodloader:` link or an archive path.
    pub open: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Mods,
    Studio,
    More,
}

/// The parts of the «More» tab (everything that is not the everyday list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Conflicts,
    Problems,
    Log,
    Profiles,
    Game,
    Links,
    Trash,
    Language,
    About,
}

impl Section {
    fn from_arg(s: &str) -> Option<Section> {
        Some(match s.to_ascii_lowercase().as_str() {
            "conflicts" => Section::Conflicts,
            "problems" => Section::Problems,
            "log" => Section::Log,
            "profiles" => Section::Profiles,
            "game" | "modloader" => Section::Game,
            "links" => Section::Links,
            "trash" => Section::Trash,
            "language" => Section::Language,
            "about" => Section::About,
            _ => return None,
        })
    }

    fn title(self) -> &'static str {
        match self {
            Section::Conflicts => tr("Conflicts"),
            Section::Problems => tr("Problems"),
            Section::Log => tr("Log"),
            Section::Profiles => tr("Profiles"),
            Section::Game => tr("Game & ModLoader"),
            Section::Links => tr("1-click install"),
            Section::Trash => tr("Trash"),
            Section::Language => tr("Language"),
            Section::About => tr("About"),
        }
    }
}

/// Results of background work (rare, so the size of the biggest variant does not matter).
#[allow(clippy::large_enum_variant)]
enum Msg {
    Version(PathBuf, Result<bool, String>),
    Payload(Option<Result<Payload, String>>),
    Progress(String, Option<f32>),
    Downloaded(Result<PathBuf, String>),
    Staged(Result<Prepared, String>),
    LoaderInstalled(Result<bool, String>),
    /// Ok(true) = the manager deletes itself: close the window.
    LoaderRemoved(Result<bool, String>),
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
    SetAll(bool),
    GoTo(Section),
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
    /// true = also delete VR-ModLoader.exe (after the window closes).
    LoaderRemove(bool),
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
    section: Section,
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

/// Status marks (list squares, kickers).
fn level_color(l: Level) -> Color32 {
    match l {
        Level::Ok | Level::Info => theme::GREEN,
        Level::Off => theme::INK_4,
        Level::Warn => theme::GOLD,
        Level::Error => theme::RED,
    }
}

/// Running text: fine is plain ink, only warnings and errors take colour.
fn note_color(l: Level) -> Color32 {
    match l {
        Level::Ok | Level::Info => theme::INK_2,
        Level::Off => theme::INK_3,
        Level::Warn => theme::GOLD,
        Level::Error => theme::RED,
    }
}

/// The masthead dateline: quiet unless something needs attention.
fn dateline_color(l: Level) -> Color32 {
    match l {
        Level::Ok | Level::Info => theme::INK_2,
        Level::Off => theme::INK_3,
        Level::Warn => theme::GOLD,
        Level::Error => theme::RED,
    }
}

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
        theme::apply(&cc.egui_ctx);
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
            tab: match (args.tab.as_deref(), &args.section) {
                (Some("studio"), _) => Tab::Studio,
                (Some("settings" | "more"), _) | (_, Some(_)) => Tab::More,
                _ => Tab::Mods,
            },
            section: match (args.section.as_deref().and_then(Section::from_arg), args.tab.as_deref()) {
                (Some(s), _) => s,
                (None, Some("settings")) => Section::Game,
                _ => Section::Conflicts,
            },
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
        // the release layout: VR-ModLoader.exe in the game folder IS the game; elsewhere, the saved folder / Steam
        let beside = modloader::exe_game_dir(&app.exe);
        let game = args.game_dir.clone().or(beside.clone()).or_else(|| app.settings.game_dir.clone().filter(|g| game::is_game_dir(g))).or_else(|| game::find_games().into_iter().next());
        if let Some(g) = game {
            app.set_game(g, &cc.egui_ctx);
        }
        if beside.is_none() {
            app.log(tr("VR-ModLoader.exe should be in the game folder (next to nie.exe)."));
        }
        app
    }

    fn log(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.status_line = s.clone();
        self.log.push(format!("[{}] {s}", &evt_installer::stamp(evt_installer::now_secs())[9..]));
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
                        Ok(true) => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
                        Ok(false) => self.log(tr("Done")),
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
            Action::SetAll(on) => {
                if let Some(l) = &mut self.list {
                    l.set_all(on);
                    self.stale = true;
                }
            }
            Action::GoTo(s) => {
                self.tab = Tab::More;
                self.section = s;
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
            Action::LoaderRemove(delete_exe) => {
                self.dialog = None;
                let Some(g) = self.game.clone() else { return };
                let exe = self.exe.clone();
                self.spawn(tr("Working…").into(), ctx, move |_| {
                    let r = modloader::remove(&g).and_then(|()| if delete_exe { modloader::delete_after_exit(&exe).map(|()| true) } else { Ok(false) });
                    Msg::LoaderRemoved(r)
                });
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

    // ------------------------------------------------------------ status summaries

    /// The `loader_min` the enabled mods ask for (empty = none).
    fn loader_need(&self) -> String {
        self.list.as_ref().map(|l| evt_installer::modpack::max_version(l.rows.iter().filter(|r| r.enabled).map(|r| r.info.manifest.loader_min.as_str()))).unwrap_or_default()
    }

    fn payload_version(&self) -> Option<String> {
        self.payload.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.version.clone())
    }

    /// The ModLoader in one short phrase (the most important thing first) and its level.
    fn loader_summary(&self) -> Option<(String, Level)> {
        let st = self.loader.as_ref()?;
        let need = self.loader_need();
        let pv = self.payload_version();
        Some(match st.version() {
            _ if matches!(st.state, evt_installer::modpack::LoaderState::Missing) => (tr("ModLoader not installed").to_string(), Level::Error),
            Some(v) if !need.is_empty() && fmt::compare_versions(v, &need).is_lt() => (trf("ModLoader {}: outdated", &[&v]), Level::Error),
            _ if st.mods_module == Some(false) => (tr("ModLoader: mods module off").to_string(), Level::Error),
            None => (tr("ModLoader (unknown version)").to_string(), Level::Warn),
            Some(v) if pv.as_deref().is_some_and(|p| st.update_available(p)) => (trf("ModLoader {}: update available", &[&v]), Level::Warn),
            Some(v) => (trf("ModLoader {}", &[&v]), Level::Ok),
        })
    }

    /// The v7.1.2 check: short text, level, longer explanation.
    fn version_summary(&self) -> (String, Level, String) {
        match &self.game_version {
            None => (tr("checking version…").to_string(), Level::Off, String::new()),
            Some(Ok(true)) => (tr("v7.1.2 (OK)").to_string(), Level::Ok, String::new()),
            Some(Ok(false)) => (tr("Not v7.1.2").to_string(), Level::Error, tr("Not v7.1.2: the ModLoader stays inactive on this build").to_string()),
            Some(Err(e)) => (tr("Version unknown").to_string(), Level::Error, e.clone()),
        }
    }

    // ------------------------------------------------------------ masthead

    fn masthead(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 72.0), Sense::hover());
        ui.painter().text(rect.center() - Vec2::new(0.0, 2.0), Align2::CENTER_CENTER, APP_NAME, theme::display_font(48.0), theme::INK);
        let corner = egui::Rect::from_min_size(rect.min, Vec2::new(230.0, rect.height()));
        place(ui, corner, Layout::top_down(Align::Min), |ui| {
            ui.add_space(20.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(theme::kicker("Inazuma Eleven", theme::INK_3));
            ui.label(theme::kicker(tr("Victory Road · PC v7.1.2"), theme::INK_3));
        });
        let corner = egui::Rect::from_min_size(egui::pos2(rect.right() - 230.0, rect.top()), Vec2::new(230.0, rect.height()));
        place(ui, corner, Layout::top_down(Align::Max), |ui| {
            ui.add_space(20.0);
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(theme::kicker(tr("The mod manager"), theme::INK_3));
            ui.label(RichText::new(format!("{} {}", tr("Edition"), vr_modloader_app::APP_VERSION)).font(theme::mono(11.0)).color(theme::INK_3));
        });
        ui.add_space(2.0);
        theme::double_rule(ui, theme::RED);
        ui.horizontal(|ui| {
            ui.set_height(32.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                for (t, label) in [(Tab::More, tr("More")), (Tab::Studio, tr("Studio")), (Tab::Mods, tr("Mods"))] {
                    let sel = self.tab == t;
                    let text = RichText::new(label.to_uppercase()).font(theme::sans_bold(12.5)).extra_letter_spacing(1.6).color(if sel { theme::PAPER } else { theme::INK_2 });
                    let b = egui::Button::new(text).fill(if sel { theme::INK } else { theme::PAPER }).stroke(Stroke::NONE).frame_when_inactive(sel).min_size(Vec2::new(0.0, 26.0));
                    if ui.add(b).clicked() {
                        self.tab = t;
                    }
                }
                ui.add_space(12.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    self.dateline(ui, acts);
                });
            });
        });
        theme::rule(ui, theme::RULE_STRONG, 1.0);
    }

    /// Game path · v7.1.2 check · ModLoader · game running: each opens its place in «More».
    fn dateline(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let dot = |ui: &mut Ui| {
            ui.label(RichText::new("·").color(theme::INK_4));
        };
        let item = |ui: &mut Ui, text: RichText, hover: &str| -> bool {
            let r = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_cursor(egui::CursorIcon::PointingHand);
            let r = if hover.is_empty() { r } else { r.on_hover_text(hover) };
            r.clicked()
        };
        let Some(g) = self.game.clone() else {
            if item(ui, RichText::new(tr("No game folder selected")).font(theme::sans(13.0)).color(theme::RED), "") {
                acts.push(Action::GoTo(Section::Game));
            }
            return;
        };
        if item(ui, RichText::new(short_path(&g, 44)).font(theme::mono(11.5)).color(theme::INK_3), &g.display().to_string()) {
            acts.push(Action::GoTo(Section::Game));
        }
        dot(ui);
        let (vt, vl, vh) = self.version_summary();
        if self.game_version.is_none() {
            ui.spinner();
        }
        if item(ui, RichText::new(vt).font(theme::sans(13.0)).color(dateline_color(vl)), &vh) {
            acts.push(Action::GoTo(Section::Game));
        }
        if let Some((lt, ll)) = self.loader_summary() {
            dot(ui);
            if item(ui, RichText::new(lt).font(theme::sans(13.0)).color(dateline_color(ll)), "") {
                acts.push(Action::GoTo(Section::Game));
            }
        }
        if self.game_running {
            dot(ui);
            let text = RichText::new(tr("Game running")).font(theme::sans_bold(13.0)).color(theme::RED);
            ui.label(text).on_hover_text(tr("The game is running: changes apply at the next start."));
        }
    }

    // ------------------------------------------------------------ footer: status + primary actions

    fn footer(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let busy = self.busy.is_some();
        let dirty = self.list.as_ref().is_some_and(|l| l.dirty);
        let has_game = self.game.is_some();
        ui.horizontal(|ui| {
            ui.set_height(38.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.add_enabled(!busy && has_game, theme::primary_button(&format!("▶  {}", tr("Play")))).on_hover_text(tr("Saves the list if needed, then starts the game through Steam.")).clicked() {
                    acts.push(Action::Launch);
                }
                let save = if dirty {
                    theme::button(tr("Save changes")).stroke(Stroke::new(1.0, theme::GOLD))
                } else {
                    theme::button(tr("Saved"))
                };
                if ui.add_enabled(dirty && !busy, save).clicked() {
                    acts.push(Action::Save);
                }
                if ui.add_enabled(!busy && has_game, theme::button(tr("Install mod (.zip)…"))).on_hover_text(tr("You can also drop a .zip on the window.")).clicked() {
                    acts.push(Action::PickArchive);
                }
                ui.add_space(10.0);
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    if let Some((label, f)) = &self.busy {
                        ui.spinner();
                        ui.add(egui::Label::new(RichText::new(label.as_str()).font(theme::sans(13.0)).color(theme::INK_2)).truncate());
                        if let Some(f) = f {
                            ui.add(egui::ProgressBar::new(*f).desired_width(200.0).show_percentage());
                        }
                    } else {
                        ui.add(egui::Label::new(RichText::new(&self.status_line).font(theme::sans(13.0)).color(theme::INK_3)).truncate());
                    }
                });
            });
        });
    }

    // ------------------------------------------------------------ the list column

    fn mod_list(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let total = self.list.as_ref().map_or(0, |l| l.rows.len());
        // head: kicker + headline, profile + options on the right
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                ui.label(theme::kicker(tr("Load order"), theme::RED));
                ui.label(theme::headline(tr("Mods"), 32.0));
            });
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.menu_button(theme::caps(tr("Options")), |ui| {
                    ui.set_min_width(210.0);
                    if ui.button(tr("Enable all")).clicked() {
                        acts.push(Action::SetAll(true));
                    }
                    if ui.button(tr("Disable all")).clicked() {
                        acts.push(Action::SetAll(false));
                    }
                    ui.separator();
                    if ui.add_enabled(self.busy.is_none(), egui::Button::new(tr("Reload from disk"))).clicked() {
                        acts.push(Action::Refresh);
                    }
                    if let Some(root) = self.mods_root() {
                        if ui.button(tr("Open mods folder")).clicked() {
                            let _ = std::fs::create_dir_all(&root);
                            acts.push(Action::Open(root.display().to_string()));
                        }
                    }
                    ui.separator();
                    if ui.button(tr("Manage profiles…")).clicked() {
                        acts.push(Action::GoTo(Section::Profiles));
                    }
                });
                let (names, cur) = self.profiles.clone();
                let cur_name = names.get(cur).cloned().unwrap_or_else(|| fmt::DEFAULT_PROFILE.to_string());
                egui::ComboBox::from_id_salt("profile").width(150.0).selected_text(RichText::new(profile_label(&cur_name)).font(theme::sans_bold(13.0))).show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        if ui.selectable_label(i == cur, profile_label(n)).clicked() && i != cur {
                            acts.push(Action::SwitchProfile(n.clone()));
                        }
                    }
                    ui.separator();
                    if ui.selectable_label(false, tr("Manage profiles…")).clicked() {
                        acts.push(Action::GoTo(Section::Profiles));
                    }
                });
                ui.label(theme::kicker(tr("Profile"), theme::INK_3));
            });
        });
        ui.horizontal(|ui| {
            let caption = trf("{} of {} active. The top of the list wins conflicts; drag a row or press Alt+↑/↓ to reorder.", &[&self.derived.active, &total]);
            let nc = self.derived.conflicts.len();
            let np = self.derived.problems.len();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if np > 0 && ui.add(theme::text_button(RichText::new(trf("{} problem(s)", &[&np])).font(theme::sans_bold(13.0)).color(theme::RED))).clicked() {
                    acts.push(Action::GoTo(Section::Problems));
                }
                if nc > 0 && ui.add(theme::text_button(RichText::new(trf("{} conflict(s)", &[&nc])).font(theme::sans_bold(13.0)).color(theme::GOLD))).clicked() {
                    acts.push(Action::GoTo(Section::Conflicts));
                }
                ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                    ui.add(egui::Label::new(theme::italic(&caption)).truncate());
                });
            });
        });
        ui.add_space(2.0);
        theme::rule(ui, theme::RULE_STRONG, 1.0);
        let Some(list) = self.list.as_mut() else { return };
        if list.rows.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.label(theme::headline(tr("No mods yet"), 22.0));
                ui.label(theme::italic(tr("Use «Install mod (.zip)» or drop a .zip on the window.")));
            });
            return;
        }
        let row_h = 38.0;
        let w = ui.available_width();
        let cols = [24.0, 30.0, (w - 24.0 - 30.0 - 96.0 - 128.0).max(140.0), 96.0, 128.0];
        // column heads
        let (hr, _) = ui.allocate_exact_size(Vec2::new(w, 24.0), Sense::hover());
        let mut x = hr.left();
        for (i, name) in ["", "", tr("Name"), tr("Version"), tr("Status")].iter().enumerate() {
            if !name.is_empty() {
                cell(ui, egui::Rect::from_min_size(egui::pos2(x + 4.0, hr.top()), Vec2::new(cols[i] - 8.0, hr.height())), theme::kicker(name, theme::INK_4));
            }
            x += cols[i];
        }
        theme::rule(ui, theme::RULE, 1.0);
        let mut row_rects: Vec<egui::Rect> = Vec::with_capacity(list.rows.len());
        let mut toggles: Vec<(usize, bool)> = Vec::new();
        let mut clicked: Option<String> = None;
        let mut drag_started: Option<usize> = None;
        egui::ScrollArea::vertical().id_salt("mods").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for (i, row) in list.rows.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, row_h), Sense::click_and_drag());
                row_rects.push(rect);
                let selected = self.selected.as_deref() == Some(row.id.as_str());
                let painter = ui.painter();
                if selected {
                    painter.rect_filled(rect, 0.0, theme::PAPER_3);
                    painter.rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), 0.0, theme::RED);
                } else if resp.hovered() {
                    painter.rect_filled(rect, 0.0, theme::PAPER_2);
                }
                painter.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, theme::RULE));
                // grip
                let gx = rect.left() + 8.0;
                for k in 0..3 {
                    let y = rect.center().y - 4.0 + k as f32 * 4.0;
                    painter.hline(gx..=gx + 9.0, y, Stroke::new(1.0, if resp.hovered() || selected { theme::INK_3 } else { theme::INK_4 }));
                }
                let st = self.derived.statuses.get(i);
                let lvl = st.map_or(Level::Ok, |s| s.level);
                let mut x = rect.left() + cols[0];
                let mut on = row.enabled;
                let cb = ui.put(egui::Rect::from_min_size(egui::pos2(x + 4.0, rect.center().y - 10.0), Vec2::new(20.0, 20.0)), egui::Checkbox::without_text(&mut on));
                if cb.changed() {
                    toggles.push((i, on));
                }
                x += cols[1];
                let name = if row.info.manifest.name.trim().is_empty() { row.id.clone() } else { row.info.manifest.name.clone() };
                let name_color = if row.enabled { theme::INK } else { theme::INK_3 };
                let name_font = if selected { theme::serif_bold(16.0) } else { FontId::proportional(16.0) };
                cell(ui, egui::Rect::from_min_size(egui::pos2(x + 4.0, rect.top()), Vec2::new(cols[2] - 8.0, row_h)), RichText::new(name).font(name_font).color(name_color));
                x += cols[2];
                cell(ui, egui::Rect::from_min_size(egui::pos2(x + 4.0, rect.top()), Vec2::new(cols[3] - 8.0, row_h)), RichText::new(&row.info.manifest.version).font(theme::mono(12.0)).color(theme::INK_3));
                x += cols[3];
                let color = level_color(lvl);
                ui.painter().rect_filled(egui::Rect::from_center_size(egui::pos2(x + 8.0, rect.center().y), Vec2::splat(6.0)), 0.0, color);
                cell(ui, egui::Rect::from_min_size(egui::pos2(x + 18.0, rect.top()), Vec2::new(cols[4] - 22.0, row_h)), theme::kicker(model::level_label(lvl), color));
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
            let target = ptr.map(|p| row_rects.iter().position(|r| p.y < r.center().y).unwrap_or(row_rects.len()));
            if let (Some(t), Some(last)) = (target, row_rects.last()) {
                let y = if t < row_rects.len() { row_rects[t].top() } else { last.bottom() };
                ui.painter().hline(last.x_range(), y, Stroke::new(2.0, theme::RED));
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
        // keyboard: Alt + up / down moves the selected mod
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

    // ------------------------------------------------------------ the article column (selected mod)

    fn details(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        let Some((row, st)) = self.selected.as_ref().and_then(|s| {
            let l = self.list.as_ref()?;
            let i = l.index_of(s)?;
            Some((l.rows[i].clone(), self.derived.statuses.get(i).cloned()))
        }) else {
            ui.add_space(60.0);
            ui.vertical_centered(|ui| ui.label(theme::italic(tr("Select a mod to see its details."))));
            return;
        };
        let m = &row.info.manifest;
        let in_conflict = self.derived.conflicts.iter().any(|c| c.mods.iter().any(|x| x == &m.id));
        egui::ScrollArea::vertical().id_salt("details").auto_shrink([false, false]).show(ui, |ui| {
            let lvl = st.as_ref().map_or(Level::Ok, |s| s.level);
            ui.horizontal(|ui| {
                ui.label(theme::kicker(model::level_label(lvl), level_color(lvl)));
                ui.label(RichText::new("·").color(theme::INK_4));
                ui.label(RichText::new(&m.id).font(theme::mono(11.5)).color(theme::INK_3));
            });
            let title = if m.name.trim().is_empty() { m.id.clone() } else { m.name.clone() };
            ui.add(egui::Label::new(theme::headline(&title, 30.0)).wrap());
            let mut byline = Vec::new();
            if !m.author.is_empty() {
                byline.push(trf("By {}", &[&m.author]));
            }
            byline.push(trf("version {}", &[&m.version]));
            let upd = row.info.updated();
            if !upd.is_empty() {
                byline.push(trf("updated {}", &[&upd]));
            }
            ui.label(RichText::new(byline.join("  ·  ")).font(theme::serif_italic(14.5)).color(theme::INK_2));
            ui.add_space(4.0);
            theme::rule(ui, theme::RULE, 1.0);
            ui.add_space(6.0);
            if let Some(p) = row.info.preview_png.clone() {
                if let Some(t) = self.preview(ui.ctx(), &p) {
                    let w = ui.available_width().min(560.0);
                    let s = t.size_vec2();
                    ui.add(egui::Image::new(&t).fit_to_exact_size(Vec2::new(w, w * s.y / s.x.max(1.0))));
                    ui.add_space(6.0);
                }
            }
            if !m.description.trim().is_empty() {
                ui.add(egui::Label::new(RichText::new(m.description.trim()).font(FontId::proportional(15.5)).color(theme::INK).line_height(Some(22.0))).wrap());
                ui.add_space(6.0);
            }
            if let Some(st) = &st {
                if !st.lines.is_empty() {
                    ui.add_space(4.0);
                    ui.label(theme::kicker(tr("Notes"), theme::RED));
                    for (l, text) in &st.lines {
                        ui.horizontal_top(|ui| {
                            ui.label(RichText::new("—").color(level_color(*l)));
                            ui.add(egui::Label::new(RichText::new(text).font(FontId::proportional(14.5)).color(note_color(*l))).wrap());
                        });
                    }
                    ui.horizontal(|ui| {
                        if !st.missing.is_empty() {
                            let miss: Vec<(String, String, Vec<String>)> = st.missing.iter().map(|(id, c)| (id.clone(), c.clone(), vec![m.id.clone()])).collect();
                            if ui.add(theme::small_button(tr("Install missing…"))).clicked() {
                                acts.push(Action::ShowMissing(miss));
                            }
                        }
                        if in_conflict && ui.add(theme::text_button(RichText::new(tr("See all conflicts")).font(theme::sans_bold(12.5)).color(theme::GOLD))).clicked() {
                            acts.push(Action::GoTo(Section::Conflicts));
                        }
                    });
                    ui.add_space(6.0);
                }
            }
            theme::rule(ui, theme::RULE, 1.0);
            ui.add_space(6.0);
            let monos = |ui: &mut Ui, v: &[String]| {
                for x in v {
                    ui.label(RichText::new(x).font(theme::mono(12.5)).color(theme::INK));
                }
            };
            if !m.requires.is_empty() {
                fact(ui, tr("Requires"), |ui| monos(ui, &m.requires));
            }
            if !m.conflicts.is_empty() {
                fact(ui, tr("Incompatible with"), |ui| monos(ui, &m.conflicts));
            }
            if !m.provides.is_empty() {
                fact(ui, tr("Provides"), |ui| monos(ui, &m.provides));
            }
            if !m.loader_min.trim().is_empty() {
                fact(ui, tr("Needs ModLoader"), |ui| monos(ui, std::slice::from_ref(&m.loader_min)));
            }
            if !m.plugin.trim().is_empty() {
                fact(ui, tr("Plugin"), |ui| monos(ui, std::slice::from_ref(&m.plugin)));
            }
            if !m.tags.is_empty() {
                fact(ui, tr("Tags"), |ui| {
                    ui.add(egui::Label::new(RichText::new(m.tags.join(", ")).color(theme::INK_2)).wrap());
                });
            }
            let deltas: usize = row.info.deltas.iter().map(|d| d.delta.set.len() + d.delta.add.len()).sum();
            fact(ui, tr("Contents"), |ui| {
                ui.add(egui::Label::new(RichText::new(trf("{} files, {} Lua scripts, {} data deltas", &[&row.info.files.len(), &row.info.lua_scripts.len(), &deltas])).color(theme::INK_2)).wrap());
            });
            fact(ui, tr("Folder"), |ui| {
                ui.add(egui::Label::new(RichText::new(row.info.dir.display().to_string()).font(theme::mono(11.5)).color(theme::INK_3)).wrap());
            });
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.add(theme::small_button(tr("Open folder"))).clicked() {
                    acts.push(Action::Open(row.info.dir.display().to_string()));
                }
                let un = egui::Button::new(RichText::new(tr("Uninstall…").to_uppercase()).font(theme::sans_bold(11.5)).extra_letter_spacing(0.8).color(theme::RED));
                if ui.add_enabled(self.busy.is_none(), un).clicked() {
                    acts.push(Action::AskUninstall(m.id.clone()));
                }
            });
            ui.add_space(8.0);
        });
    }

    // ------------------------------------------------------------ «More»: one index column + one section

    fn more_index(&mut self, ui: &mut Ui) {
        ui.add_space(18.0);
        let nc = self.derived.conflicts.len();
        let np = self.derived.problems.len();
        for (group, items) in [
            (tr("Reports"), &[Section::Conflicts, Section::Problems, Section::Log][..]),
            (tr("Setup"), &[Section::Profiles, Section::Game, Section::Links, Section::Trash][..]),
            (tr("Program"), &[Section::Language, Section::About][..]),
        ] {
            ui.label(theme::kicker(group, theme::INK_4));
            ui.add_space(2.0);
            for &s in items {
                let w = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
                let sel = self.section == s;
                let p = ui.painter();
                if sel {
                    p.rect_filled(rect, 0.0, theme::PAPER_3);
                    p.rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(3.0, rect.height())), 0.0, theme::RED);
                } else if resp.hovered() {
                    p.rect_filled(rect, 0.0, theme::PAPER_2);
                }
                let font = if sel { theme::serif_bold(15.5) } else { FontId::proportional(15.5) };
                p.text(egui::pos2(rect.left() + 14.0, rect.center().y), Align2::LEFT_CENTER, s.title(), font, if sel { theme::INK } else { theme::INK_2 });
                let count = match s {
                    Section::Conflicts => nc,
                    Section::Problems => np,
                    _ => 0,
                };
                if count > 0 {
                    let c = if s == Section::Problems { theme::RED } else { theme::GOLD };
                    p.text(egui::pos2(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, count.to_string(), theme::mono(12.0), c);
                }
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    self.section = s;
                }
            }
            ui.add_space(14.0);
        }
    }

    fn more_section(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        egui::ScrollArea::vertical().id_salt("more").auto_shrink([false, false]).show(ui, |ui| {
            ui.set_max_width(900.0);
            match self.section {
                Section::Conflicts => self.sec_conflicts(ui),
                Section::Problems => self.sec_problems(ui, acts),
                Section::Log => self.sec_log(ui),
                Section::Profiles => self.sec_profiles(ui, acts),
                Section::Game => self.sec_game(ui, acts),
                Section::Links => self.sec_links(ui, acts),
                Section::Trash => self.sec_trash(ui, acts),
                Section::Language => self.sec_language(ui),
                Section::About => self.sec_about(ui),
            }
        });
    }

    fn sec_conflicts(&mut self, ui: &mut Ui) {
        section_head(ui, tr("Reports"), tr("Conflicts"), tr("Places where two enabled mods change the same thing. The mod higher in the list loads later and wins."));
        if self.derived.conflicts.is_empty() {
            ui.label(theme::italic(tr("No conflicts between the enabled mods.")));
            return;
        }
        ui.horizontal(|ui| {
            ui.label(theme::kicker(tr("Filter"), theme::INK_3));
            ui.add(egui::TextEdit::singleline(&mut self.conflict_filter).desired_width(240.0).hint_text(tr("file, cell or mod id")));
        });
        ui.add_space(6.0);
        let f = self.conflict_filter.to_lowercase();
        egui::Grid::new("conf").num_columns(4).spacing([18.0, 8.0]).show(ui, |ui| {
            ui.label(theme::kicker(tr("Kind"), theme::INK_4));
            ui.label(theme::kicker(tr("What"), theme::INK_4));
            ui.label(theme::kicker(tr("Mods (load order)"), theme::INK_4));
            ui.label(theme::kicker(tr("Winner"), theme::INK_4));
            ui.end_row();
            for c in self.derived.conflicts.iter().filter(|c| f.is_empty() || c.what.to_lowercase().contains(&f) || c.mods.iter().any(|m| m.contains(&f))) {
                ui.label(RichText::new(&c.kind).font(theme::sans(13.0)).color(theme::INK_3));
                ui.label(RichText::new(&c.what).font(theme::mono(12.5)).color(theme::INK));
                ui.label(RichText::new(c.mods.join("  >  ")).font(theme::mono(12.5)).color(theme::INK_2));
                ui.label(RichText::new(c.winner()).font(theme::mono(12.5)).color(theme::GOLD));
                ui.end_row();
            }
        });
        if self.derived.conflicts.iter().any(|c| c.kind.starts_with("audio")) {
            ui.add_space(8.0);
            ui.label(theme::italic(tr("Audio declarations (audio.toml): the mod loaded last is expected to win.")));
        }
    }

    fn sec_problems(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        section_head(ui, tr("Reports"), tr("Problems"), tr("What stops a mod from loading, and files the loader cannot read."));
        if !self.derived.missing.is_empty() {
            let n = self.derived.missing.len();
            if ui.add(theme::button(&format!("{} ({n})", tr("Install missing…")))).clicked() {
                acts.push(Action::ShowMissing(self.derived.missing.clone()));
            }
            ui.add_space(6.0);
        }
        if self.derived.problems.is_empty() {
            ui.label(theme::italic(tr("No problems.")));
        }
        for (l, t) in &self.derived.problems {
            ui.horizontal_top(|ui| {
                ui.label(RichText::new("—").color(level_color(*l)));
                ui.add(egui::Label::new(RichText::new(t).color(note_color(*l))).wrap());
            });
        }
    }

    fn sec_log(&mut self, ui: &mut Ui) {
        section_head(ui, tr("Reports"), tr("Log"), tr("What this window did since it opened."));
        if self.log.is_empty() {
            ui.label(theme::italic(tr("Nothing yet.")));
        }
        for l in &self.log {
            ui.label(RichText::new(l).font(theme::mono(12.0)).color(theme::INK_2));
        }
    }

    fn sec_profiles(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        section_head(ui, tr("Setup"), tr("Profiles"), tr("Each profile keeps its own enabled mods and order. The game's Mods menu uses the same profiles."));
        let (names, cur) = self.profiles.clone();
        for (i, n) in names.iter().enumerate() {
            ui.horizontal(|ui| {
                let sel = i == cur;
                if ui.radio(sel, RichText::new(profile_label(n)).font(if sel { theme::serif_bold(15.5) } else { FontId::proportional(15.5) })).clicked() && !sel {
                    acts.push(Action::SwitchProfile(n.clone()));
                }
                if sel {
                    ui.label(theme::kicker(tr("Active"), theme::GREEN));
                }
            });
        }
        ui.add_space(10.0);
        let cur_name = names.get(cur).cloned().unwrap_or_else(|| fmt::DEFAULT_PROFILE.to_string());
        let custom = cur_name != fmt::DEFAULT_PROFILE;
        ui.horizontal(|ui| {
            if ui.add(theme::small_button(tr("New…"))).clicked() {
                self.dialog = Some(Dialog::ProfileName { rename: None, text: String::new(), error: String::new() });
            }
            if ui.add_enabled(custom, theme::small_button(tr("Rename…"))).clicked() {
                self.dialog = Some(Dialog::ProfileName { rename: Some(cur_name.clone()), text: cur_name.clone(), error: String::new() });
            }
            if ui.add_enabled(custom, theme::small_button(tr("Delete"))).clicked() {
                self.dialog = Some(Dialog::ConfirmDeleteProfile(cur_name.clone()));
            }
        });
        if !custom {
            ui.label(theme::italic(tr("The default profile cannot be renamed or deleted.")));
        }
    }

    fn sec_game(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        section_head(ui, tr("Setup"), tr("Game & ModLoader"), tr("Where the game is, whether it is the right version, and the ModLoader that loads the mods."));
        ui.label(theme::headline(tr("Game folder"), 20.0));
        match &self.game {
            Some(g) => {
                ui.add(egui::Label::new(RichText::new(g.display().to_string()).font(theme::mono(12.5)).color(theme::INK)).wrap());
                if self.game_override {
                    ui.label(theme::italic(tr("(command-line override, not saved)")));
                }
                if modloader::exe_game_dir(&self.exe).is_none() {
                    ui.label(RichText::new(tr("VR-ModLoader.exe should be in the game folder (next to nie.exe).")).color(theme::GOLD));
                }
                let (vt, vl, vh) = self.version_summary();
                ui.label(RichText::new(if vh.is_empty() { vt } else { vh }).color(note_color(vl)));
            }
            None => {
                ui.label(RichText::new(tr("No game folder selected")).color(theme::RED));
            }
        }
        ui.horizontal(|ui| {
            if ui.add(theme::small_button(tr("Change…"))).clicked() {
                acts.push(Action::PickGame);
            }
            if ui.add(theme::small_button(tr("Detect"))).clicked() {
                acts.push(Action::DetectGame);
            }
            if let Some(root) = self.mods_root() {
                if ui.add(theme::small_button(tr("Open mods folder"))).clicked() {
                    let _ = std::fs::create_dir_all(&root);
                    acts.push(Action::Open(root.display().to_string()));
                }
            }
        });
        if self.game_running {
            ui.label(RichText::new(tr("The game is running: changes apply at the next start.")).color(theme::RED));
        }
        let Some(st) = self.loader.clone() else { return };
        ui.add_space(14.0);
        theme::rule(ui, theme::RULE, 1.0);
        ui.add_space(8.0);
        ui.label(theme::headline(tr("ModLoader"), 20.0));
        let busy = self.busy.is_some();
        let need = self.loader_need();
        let (text, lvl) = match (&st.state, st.version()) {
            (evt_installer::modpack::LoaderState::Missing, _) => (tr("Not installed").to_string(), Level::Error),
            (_, Some(v)) => (trf("Installed {}", &[&v]), Level::Ok),
            (_, None) => (tr("Installed (unknown version)").to_string(), Level::Warn),
        };
        ui.label(RichText::new(text).font(theme::serif_bold(15.5)).color(note_color(lvl)));
        if let Some(v) = st.version() {
            if !need.is_empty() && fmt::compare_versions(v, &need).is_lt() {
                ui.label(RichText::new(trf("Outdated: a mod needs {}", &[&need])).color(theme::RED));
            }
        }
        let pv = self.payload_version();
        if let Some(pv) = &pv {
            if st.update_available(pv) {
                ui.label(RichText::new(trf("Update available: {}", &[pv])).color(theme::GOLD));
            }
        }
        if st.mods_module == Some(false) {
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr("The mods module is off in evt_loader\\config.toml: mods are ignored.")).color(theme::GOLD));
                if ui.add(theme::small_button(tr("Turn on"))).clicked() {
                    acts.push(Action::EnableModsModule);
                }
            });
        }
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if let Some(pv) = &pv {
                let label = if !st.installed() {
                    tr("Install")
                } else if st.update_available(pv) {
                    tr("Update")
                } else {
                    tr("Repair")
                };
                let enabled = !busy;
                let src = self.payload.as_ref().and_then(|p| p.as_ref().ok()).map(|p| p.source.clone()).unwrap_or_default();
                let r = ui.add_enabled(enabled, theme::small_button(&format!("{label} {pv}")));
                if r.on_hover_text(trf("Package: {}", &[&src])).clicked() {
                    acts.push(Action::LoaderInstall);
                }
            }
            if ui.add_enabled(!busy, theme::small_button(tr("Install ModLoader from file…"))).clicked() {
                acts.push(Action::LoaderPick);
            }
            if st.installed() {
                let b = egui::Button::new(RichText::new(tr("Remove").to_uppercase()).font(theme::sans_bold(11.5)).extra_letter_spacing(0.8).color(theme::RED));
                let r = ui.add_enabled(!busy && st.ours, b);
                let r = if st.ours { r } else { r.on_disabled_hover_text(tr("installed by another tool: remove it with that tool")) };
                if r.clicked() {
                    self.dialog = Some(Dialog::ConfirmRemoveLoader);
                }
            }
        });
        if pv.is_none() {
            ui.add(egui::Label::new(theme::italic(tr("No ModLoader package found (embedded, modloader\\ or modloader.zip next to the exe)."))).wrap());
        }
    }

    fn sec_links(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        section_head(ui, tr("Setup"), tr("1-click install"), tr("Registers the vrmodloader: link type for your Windows user (HKCU\\Software\\Classes\\vrmodloader), so «1-click install» buttons on mod sites open this program. Nothing is registered without this button."));
        let (status, ok) = match &self.scheme_cmd {
            Some(c) if urlscheme::command_is(c, &self.exe) => (tr("Status: registered to this program").to_string(), Level::Ok),
            Some(c) => (trf("Status: registered to another program: {}", &[c]), Level::Warn),
            None => (tr("Status: not registered").to_string(), Level::Off),
        };
        ui.add(egui::Label::new(RichText::new(status).font(theme::serif_bold(15.0)).color(note_color(ok))).wrap());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if ui.add(theme::small_button(tr("Register 1-click links"))).clicked() {
                acts.push(Action::Register);
            }
            if ui.add_enabled(self.scheme_cmd.is_some(), theme::small_button(tr("Unregister"))).clicked() {
                acts.push(Action::Unregister);
            }
        });
    }

    fn sec_trash(&mut self, ui: &mut Ui, acts: &mut Vec<Action>) {
        section_head(ui, tr("Setup"), tr("Trash"), tr("Uninstalled mods and replaced versions wait in mods\\_trash\\ until you empty it."));
        let Some(root) = self.mods_root() else {
            ui.label(RichText::new(tr("No game folder selected")).color(theme::RED));
            return;
        };
        let n = install::trash_items(&root);
        ui.label(trf("{} item(s) in mods\\_trash\\", &[&n]));
        ui.horizontal(|ui| {
            if ui.add_enabled(n > 0, theme::small_button(tr("Open trash"))).clicked() {
                acts.push(Action::Open(install::trash_root(&root).display().to_string()));
            }
            let b = egui::Button::new(RichText::new(tr("Empty trash…").to_uppercase()).font(theme::sans_bold(11.5)).extra_letter_spacing(0.8).color(theme::RED));
            if ui.add_enabled(n > 0, b).clicked() {
                self.dialog = Some(Dialog::ConfirmEmptyTrash);
            }
        });
    }

    fn sec_language(&mut self, ui: &mut Ui) {
        section_head(ui, tr("Program"), tr("Language"), tr("The language of this window."));
        let cur = i18n::lang();
        ui.horizontal(|ui| {
            for l in [Lang::En, Lang::Es] {
                if ui.radio(cur == l, RichText::new(l.label()).font(FontId::proportional(15.5))).clicked() && cur != l {
                    i18n::set_lang(l);
                    self.settings.language = l.code().into();
                    self.save_settings();
                    self.stale = true;
                }
            }
        });
    }

    fn sec_about(&mut self, ui: &mut Ui) {
        section_head(ui, tr("Program"), tr("About"), "");
        ui.label(RichText::new(format!("{APP_NAME} {}", vr_modloader_app::APP_VERSION)).font(theme::serif_bold(16.0)).color(theme::INK));
        ui.add(egui::Label::new(tr("Mod manager for the VR-ModLoader (Inazuma Eleven Victory Road PC v7.1.2). Free software under the GPL-3.0.")).wrap());
        ui.add(egui::Label::new(theme::italic(tr("Fonts: Playfair Display, Source Serif 4, IBM Plex Sans Condensed and IBM Plex Mono, under the SIL Open Font License 1.1."))).wrap());
        ui.add_space(6.0);
        fact(ui, tr("Settings"), |ui| {
            ui.add(egui::Label::new(RichText::new(settings::settings_path().display().to_string()).font(theme::mono(11.5)).color(theme::INK_3)).wrap());
        });
    }

    // ------------------------------------------------------------ Studio (secondary section)

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
        section_head(ui, tr("Coming soon"), tr("Studio"), tr("Create and edit mods with forms: characters, techniques, teams, texts… It reads your own game data (v7.1.2) to build a local index, so you pick game things from lists instead of typing ids. Nothing from the game ships with this program."));
        ui.label(theme::headline(tr("Game index"), 20.0));
        let Some(idx) = self.studio.index.clone() else {
            ui.add(egui::Label::new(tr("The index is built once from your game (a few minutes; read only) and stored in %LOCALAPPDATA%\\VR-ModLoader\\index.")).wrap());
            ui.add_space(4.0);
            if ui.add_enabled(self.game.is_some() && self.busy.is_none(), theme::small_button(tr("Open / build the index"))).clicked() {
                self.open_index(&ctx);
            }
            return;
        };
        let lang = if i18n::lang() == Lang::Es { 2 } else { 1 };
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.studio.query).desired_width(300.0).hint_text(tr("Search names or ids (any language)")));
            let cat_label = self.studio.category.map(|c| c.as_str().to_string()).unwrap_or_else(|| tr("All").to_string());
            egui::ComboBox::from_id_salt("cat").selected_text(cat_label).show_ui(ui, |ui| {
                ui.selectable_value(&mut self.studio.category, None, tr("All"));
                for c in vr_index::Category::ALL {
                    ui.selectable_value(&mut self.studio.category, Some(c), c.as_str());
                }
            });
            let m = idx.meta();
            ui.label(RichText::new(format!("{} · {}", trf("{} entries", &[&idx.entities().len()]), m.game_version.clone().unwrap_or_default())).font(theme::mono(11.5)).color(theme::INK_3));
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
        ui.add_space(4.0);
        theme::rule(ui, theme::RULE, 1.0);
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
            egui::Grid::new("hits_grid").num_columns(4).spacing([14.0, 6.0]).show(ui, |ui| {
                for (k, (tex, name, id, detail)) in rows.iter().enumerate() {
                    match tex {
                        Some(t) => {
                            ui.add(egui::Image::new(t).fit_to_exact_size(Vec2::splat(28.0)));
                        }
                        None => {
                            ui.label("");
                        }
                    }
                    if ui.selectable_label(self.studio.selected == Some(k), RichText::new(name).font(theme::serif_bold(15.0))).clicked() {
                        self.studio.selected = Some(k);
                    }
                    ui.label(RichText::new(id).font(theme::mono(12.0)).color(theme::INK_2));
                    ui.label(RichText::new(detail).color(theme::INK_3));
                    ui.end_row();
                }
            });
        });
    }
    fn dialogs(&mut self, ctx: &egui::Context, acts: &mut Vec<Action>) {
        let Some(mut d) = self.dialog.take() else { return };
        let mut keep = true;
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(Margin::same(24)).stroke(Stroke::new(1.0, theme::RULE_STRONG));
        egui::Modal::new(Id::new("dialog")).frame(frame).show(ctx, |ui| {
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
                                    ui.label(RichText::new(t).color(level_color(l)));
                                }
                                ui.add(egui::Label::new(RichText::new(name).strong()).truncate());
                            });
                            if !ok {
                                ui.label(RichText::new(tr("Errors: this mod cannot be installed")).color(theme::RED));
                            }
                            for e in &c.staged.errors {
                                ui.label(RichText::new(format!("  •  {e}")).color(theme::RED).small());
                            }
                            for w in c.staged.warnings.iter().take(8) {
                                ui.label(RichText::new(format!("  •  {w}")).color(theme::GOLD).small());
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
                        if ui.add_enabled(any, theme::primary_button(tr("Install selected"))).clicked() {
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
                        ui.label(RichText::new(trf("These enabled mods need it: {}", &[&dependents.join(", ")])).color(theme::GOLD));
                    }
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Uninstall")).color(theme::RED)).clicked() {
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
                        ui.label(RichText::new(error.as_str()).color(theme::RED));
                    }
                    ui.horizontal(|ui| {
                        let label = if rename.is_some() { tr("Rename") } else { tr("Create") };
                        let enter = ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui.add(theme::primary_button(label)).clicked() || enter {
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
                        if ui.add(theme::primary_button(tr("Save and switch"))).clicked() {
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
                    ui.label(RichText::new(link.host()).font(theme::serif_bold(18.0)).color(theme::INK));
                    ui.add(egui::Label::new(RichText::new(&link.url).monospace().small()).wrap());
                    if let Some((t, id)) = &link.item {
                        ui.label(RichText::new(format!("GameBanana {t} #{id}")).weak());
                    }
                    ui.label(RichText::new(tr("Only continue if you trust this site. The archive is checked before anything is installed.")).color(theme::GOLD));
                    ui.horizontal(|ui| {
                        if ui.add(theme::primary_button(tr("Download"))).clicked() {
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
                    ui.label(tr("Remove the ModLoader? The game goes back to how it was (what it replaced is put back, its evt_loader folder is deleted). Your mods folder is kept."));
                    ui.horizontal(|ui| {
                        if ui.button(RichText::new(tr("Remove")).color(theme::RED)).clicked() {
                            acts.push(Action::LoaderRemove(false));
                        }
                        if modloader::exe_game_dir(&self.exe).is_some() && ui.button(RichText::new(tr("Remove and delete VR-ModLoader.exe")).color(theme::RED)).clicked() {
                            acts.push(Action::LoaderRemove(true));
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
                        if ui.button(RichText::new(tr("Delete permanently")).color(theme::RED)).clicked() {
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
    place(ui, r, Layout::left_to_right(Align::Center), |ui| {
        ui.add(egui::Label::new(text).truncate().selectable(false));
    });
}

/// Lay out `add` inside `r`.
fn place(ui: &mut Ui, r: egui::Rect, layout: Layout, add: impl FnOnce(&mut Ui)) {
    ui.scope_builder(egui::UiBuilder::new().max_rect(r).layout(layout), add);
}

/// Kicker + headline + italic standfirst + rule: the top of every secondary section.
fn section_head(ui: &mut Ui, kicker: &str, title: &str, standfirst: &str) {
    ui.add_space(2.0);
    ui.label(theme::kicker(kicker, theme::RED));
    ui.label(theme::headline(title, 30.0));
    if !standfirst.is_empty() {
        ui.add(egui::Label::new(theme::italic(standfirst)).wrap());
    }
    ui.add_space(4.0);
    theme::rule(ui, theme::RULE_STRONG, 1.0);
    ui.add_space(8.0);
}

/// One line of a fact box: small caps label on the left, the value(s) on the right.
fn fact(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(Vec2::new(128.0, 18.0), Layout::top_down(Align::Min), |ui| {
            ui.set_min_width(128.0);
            ui.add_space(3.0);
            ui.add(egui::Label::new(theme::kicker(label, theme::INK_3)).wrap());
        });
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            add(ui);
        });
    });
    ui.add_space(4.0);
}

/// A path shortened from the left («…\steamapps\common\Game»), for the dateline.
fn short_path(p: &Path, max: usize) -> String {
    let s = p.display().to_string();
    let n = s.chars().count();
    if n <= max {
        return s;
    }
    let tail: String = s.chars().skip(n - max.saturating_sub(1)).collect();
    let tail = match tail.find(['\\', '/']) {
        Some(i) => tail[i..].to_string(),
        None => tail,
    };
    format!("…{tail}")
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
        let paper = |l: i8, r: i8, t: i8, b: i8| egui::Frame::new().fill(theme::PAPER).inner_margin(Margin { left: l, right: r, top: t, bottom: b });
        egui::Panel::top("masthead").frame(paper(28, 28, 10, 0)).show_separator_line(false).show(ui, |ui| self.masthead(ui, &mut acts));
        egui::Panel::bottom("footer").frame(paper(28, 28, 8, 8)).show(ui, |ui| self.footer(ui, &mut acts));
        match self.tab {
            Tab::Mods if self.game.is_some() => {
                egui::Panel::right("article").frame(paper(26, 28, 20, 8)).resizable(true).default_size(430.0).min_size(300.0).max_size(700.0).show(ui, |ui| self.details(ui, &mut acts));
                egui::CentralPanel::default().frame(paper(28, 24, 16, 8)).show(ui, |ui| self.mod_list(ui, &mut acts));
            }
            Tab::Mods => {
                egui::CentralPanel::default().frame(paper(28, 28, 16, 8)).show(ui, |ui| {
                    ui.add_space(60.0);
                    ui.vertical_centered(|ui| {
                        ui.label(theme::headline(tr("Game not found"), 30.0));
                        ui.label(theme::italic(tr("Game not found. Choose the folder that contains nie.exe.")));
                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            ui.add_space((ui.available_width() - 230.0).max(0.0) / 2.0);
                            if ui.add(theme::button(tr("Change…"))).clicked() {
                                acts.push(Action::PickGame);
                            }
                            if ui.add(theme::button(tr("Detect"))).clicked() {
                                acts.push(Action::DetectGame);
                            }
                        });
                    });
                });
            }
            Tab::Studio => {
                egui::CentralPanel::default().frame(paper(28, 28, 20, 8)).show(ui, |ui| {
                    ui.set_max_width(980.0);
                    self.studio(ui);
                });
            }
            Tab::More => {
                egui::Panel::left("index").frame(paper(20, 16, 4, 8)).resizable(false).exact_size(230.0).show(ui, |ui| self.more_index(ui));
                egui::CentralPanel::default().frame(paper(32, 28, 20, 8)).show(ui, |ui| self.more_section(ui, &mut acts));
            }
        }
        // hovering files: drop hint
        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.content_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("drop")));
            painter.rect_filled(screen, 0.0, theme::PAPER.gamma_multiply(0.93));
            let inner = screen.shrink(28.0);
            painter.rect_stroke(inner, 0.0, Stroke::new(1.0, theme::RED), egui::StrokeKind::Inside);
            painter.rect_stroke(inner.shrink(5.0), 0.0, Stroke::new(1.0, theme::RED), egui::StrokeKind::Inside);
            painter.text(screen.center(), Align2::CENTER_CENTER, tr("Drop a .zip here to install it"), theme::display_font(34.0), theme::INK);
        }
        self.dialogs(&ctx, &mut acts);
        for a in acts {
            self.run(a, &ctx);
        }
    }
}
