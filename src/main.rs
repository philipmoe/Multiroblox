#![windows_subsystem = "windows"]

mod audio;
mod discord;
mod gamesettings;
mod history;
mod home;
mod icon;
mod imgcache;
mod jobs;
mod live;
mod logwatch;
mod multi;
mod roblox;
mod servers;
mod store;
mod tray;

use eframe::egui::{self, Color32, RichText};
use history::History;
use home::HomeState;
use jobs::{Acct, Bg, HomePart, Msg, Watch};
use live::LiveState;
use roblox::{Presence, Server, Target};
use servers::ServersTab;
use store::ServerPick;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(90, 200, 120);
const AMBER: Color32 = Color32::from_rgb(235, 180, 70);
const RED: Color32 = Color32::from_rgb(235, 90, 90);
const BLUE: Color32 = Color32::from_rgb(110, 170, 240);

/// Frame-rate caps offered for AFK clients. 30 and up match Roblox's own menu; the lower ones
/// aren't offered by Roblox and may be ignored by the client.
const FPS_CAPS: [u32; 9] = [1, 5, 10, 15, 30, 60, 120, 144, 240];

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Home,
    Accounts,
    Settings,
}

struct App {
    tab: Tab,
    history: History,
    home: HomeState,
    /// Avatar headshot URLs of the saved accounts.
    avatars: HashMap<u64, String>,
    /// Mirrors the Windows startup entry, which is the real setting.
    start_with_windows: bool,
    cfg: store::Config,
    held: Vec<multi::Singleton>,
    bg: Bg,
    rx: Receiver<Msg>,
    log: Vec<String>,
    presence: HashMap<u64, Presence>,
    expired: HashSet<u64>,
    cookie_input: String,
    show_add: bool,
    adding: bool,
    confirm_remove: Option<u64>,
    clients: usize,
    last_poll: Instant,
    dirty: bool,
    show_servers: bool,
    servers: Vec<Server>,
    servers_loading: bool,
    servers_failed: bool,
    fewest_first: bool,
    servers_tab: ServersTab,
    /// Name typed for "save the current game".
    save_name: String,
    awake_applied: bool,
    live: LiveState,
    log_open: bool,
    icon: egui::TextureHandle,
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        apply_style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let bg = Bg {
            tx,
            ctx: cc.egui_ctx.clone(),
            jobs: Arc::new(AtomicUsize::new(0)),
            watch: Arc::new(Mutex::new(Watch::default())),
        };
        jobs::start_watcher(&bg);
        jobs::start_anti_afk(&bg);
        jobs::start_status_refresh(&bg);
        jobs::start_friend_alerts(&bg);
        logwatch::start(&bg);
        // Keep the startup entry pointing at this exe, in case it was moved.
        let start_with_windows = multi::starts_with_windows();
        if start_with_windows {
            multi::set_start_with_windows(true);
        }
        egui_extras::install_image_loaders(&cc.egui_ctx);
        add_windows_fonts(&cc.egui_ctx);
        if std::env::args().any(|a| a == "--startup") {
            // Started by Windows at sign-in: stay out of the way until opened from the taskbar.
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
        }

        // Free the singleton in clients that were started earlier, then claim it for ourselves.
        let freed = multi::release_in_running_clients();
        let held = multi::acquire();

        let cfg = store::load();
        let tab = if cfg.accounts.is_empty() { Tab::Accounts } else { Tab::Home };
        let mut app = App {
            tab,
            history: History::load(),
            home: HomeState::new(cc.egui_ctx.clone()),
            avatars: HashMap::new(),
            start_with_windows,
            cfg,
            held,
            bg,
            rx,
            log: Vec::new(),
            presence: HashMap::new(),
            expired: HashSet::new(),
            cookie_input: String::new(),
            show_add: false,
            adding: false,
            confirm_remove: None,
            clients: multi::roblox_pids().len(),
            last_poll: Instant::now(),
            dirty: false,
            show_servers: false,
            servers: Vec::new(),
            servers_loading: false,
            servers_failed: false,
            fewest_first: false,
            servers_tab: ServersTab::default(),
            save_name: String::new(),
            awake_applied: false,
            live: LiveState::new(),
            log_open: false,
            icon: cc.egui_ctx.load_texture(
                "app-icon",
                egui::ColorImage::from_rgba_unmultiplied([64, 64], &icon::rgba(64)),
                egui::TextureOptions::LINEAR,
            ),
        };
        if freed > 0 {
            app.say(format!("Released the singleton lock in {freed} running client handle(s)."));
        }
        app.show_add = app.cfg.accounts.is_empty();
        app.sync_accounts();
        app.load_avatars();
        let ids: Vec<u64> = app.cfg.accounts.iter().map(|a| a.user_id).collect();
        app.refresh(&ids);
        app
    }

    fn say(&mut self, s: impl Into<String>) {
        self.log.push(format!("[{}] {}", multi::clock(), s.into()));
        if self.log.len() > 400 {
            self.log.drain(..100);
        }
    }

    fn save(&mut self) {
        store::save(&self.cfg);
        self.dirty = false;
    }

    fn acct(&mut self, id: u64) -> Option<Acct> {
        let a = self.cfg.accounts.iter().find(|a| a.user_id == id)?;
        let name = a.username.clone();
        match a.cookie_plain() {
            Some(cookie) => Some(Acct { id, name, cookie }),
            None => {
                self.say(format!("{name}: could not decrypt the saved cookie - re-add this account."));
                None
            }
        }
    }

    /// Hands every saved account to the background threads (for playtime tracking).
    fn sync_accounts(&mut self) {
        let list = self
            .cfg
            .accounts
            .iter()
            .filter_map(|a| Some(Acct { id: a.user_id, name: a.username.clone(), cookie: a.cookie_plain()? }))
            .collect();
        self.bg.watch().accounts = list;
    }

    fn accts(&mut self, ids: &[u64]) -> Vec<Acct> {
        ids.iter().filter_map(|&id| self.acct(id)).collect()
    }

    fn alt_ids(&self) -> Vec<u64> {
        self.cfg
            .accounts
            .iter()
            .map(|a| a.user_id)
            .filter(|id| Some(*id) != self.cfg.main && self.cfg.selected.contains(id))
            .collect()
    }

    fn refresh(&mut self, ids: &[u64]) {
        let list = self.accts(ids);
        if !list.is_empty() {
            jobs::refresh(&self.bg, list);
        }
    }

    fn target(&mut self) -> Option<Target> {
        let t = roblox::parse_target(&self.cfg.place_id, &self.cfg.job_id);
        if t.is_none() {
            self.say("Enter a place ID, a game link or a private server link first.");
        }
        t
    }

    fn load_servers(&mut self) {
        let place = match self.target() {
            Some(Target::Place { place, .. }) => place,
            Some(_) => {
                self.say("The server list is for public servers - replace the private server link with the game first.");
                return;
            }
            None => return,
        };
        let Some(id) = self.cfg.main.or_else(|| self.cfg.accounts.first().map(|a| a.user_id)) else {
            self.say("Add an account first - the server list is fetched as a signed-in user.");
            return;
        };
        let Some(a) = self.acct(id) else {
            return;
        };
        self.show_servers = true;
        self.servers_loading = true;
        self.servers_failed = false;
        self.servers.clear();
        jobs::servers(&self.bg, a, place, self.fewest_first);
    }

    /// Ids of tracked clients that belong to alts, i.e. every account except main.
    fn alt_client_pids(&self) -> Vec<u32> {
        let main = self.cfg.main;
        self.bg.watch().clients.iter().filter(|(id, _)| Some(**id) != main).map(|(_, c)| c.pid).collect()
    }

    fn count_clients(&mut self) {
        self.clients = multi::roblox_pids().len();
        self.last_poll = Instant::now();
    }

    /// Makes an account main. Main is never also an alt.
    fn set_main(&mut self, id: u64) {
        self.cfg.main = Some(id);
        self.cfg.selected.retain(|x| *x != id);
        self.dirty = true;
        if let Some(a) = self.cfg.accounts.iter().find(|a| a.user_id == id) {
            let name = a.username.clone();
            self.say(format!("{name} is now the main account."));
        }
    }

    fn load_avatars(&mut self) {
        let ids: Vec<u64> = self.cfg.accounts.iter().map(|a| a.user_id).collect();
        let Some(&first) = ids.first() else { return };
        if let Some(a) = self.acct(first) {
            jobs::load_avatars(&self.bg, a, ids);
        }
    }

    /// Closes one account's client. It is untracked first so auto-rejoin doesn't bring it back.
    fn stop_client(&mut self, id: u64, name: &str) {
        let pid = self.bg.watch().clients.remove(&id).map(|c| c.pid);
        let stopped = pid.is_some_and(multi::kill);
        self.count_clients();
        match pid {
            Some(_) if stopped => self.say(format!("Stopped {name}'s client.")),
            Some(_) => self.say(format!("{name}: could not close its client.")),
            None => self.say(format!("{name} has no client running from here.")),
        }
    }

    /// Closes every alt client started from here and leaves main running.
    fn stop_alts(&mut self) {
        let main = self.cfg.main;
        let pids: Vec<u32> = {
            let mut w = self.bg.watch();
            let ids: Vec<u64> = w.clients.keys().copied().filter(|id| Some(*id) != main).collect();
            ids.iter().filter_map(|id| w.clients.remove(id)).map(|c| c.pid).collect()
        };
        let n = pids.into_iter().filter(|&pid| multi::kill(pid)).count();
        self.count_clients();
        self.say(format!("Stopped {n} alt client(s)."));
    }

    fn settings(&self) -> jobs::Settings {
        let c = &self.cfg;
        jobs::Settings {
            main: c.main,
            server_pick: c.server_pick,
            include_main: c.afk_include_main,
            low_graphics: c.low_graphics,
            fps_cap: c.fps_cap_on.then_some(c.fps_cap),
            cpu_limit: c.cpu_limit_on.then_some(c.cpu_limit),
            mute: c.mute_alts,
            low_priority: c.low_priority_alts,
            minimize: c.minimize_alts,
            auto_rejoin: c.auto_rejoin,
            anti_afk: c.anti_afk,
            afk_minutes: c.afk_minutes,
            afk_jump: c.afk_jump,
            friend_alerts: c.friend_alerts,
        }
    }

    fn pump(&mut self) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Log(s) => self.say(s),
                Msg::Added(a) => {
                    let id = a.user_id;
                    self.say(format!("Added {} ({}).", a.username, a.title()));
                    match self.cfg.accounts.iter_mut().find(|x| x.user_id == id) {
                        Some(slot) => *slot = a,
                        None => self.cfg.accounts.push(a),
                    }
                    if self.cfg.main.is_none() {
                        self.cfg.main = Some(id);
                    }
                    self.expired.remove(&id);
                    self.adding = false;
                    self.cookie_input.clear();
                    self.save();
                    self.sync_accounts();
                    self.load_avatars();
                    self.refresh(&[id]);
                }
                Msg::AddFailed => self.adding = false,
                Msg::Presence(id, p) => {
                    self.expired.remove(&id);
                    self.presence.insert(id, p);
                }
                Msg::Expired(id) => {
                    self.expired.insert(id);
                }
                Msg::Servers(list) => {
                    self.servers_loading = false;
                    self.servers_failed = list.is_none();
                    self.servers = list.unwrap_or_default();
                }
                Msg::Launched(place) => {
                    self.history.launched(place);
                    self.history.save();
                }
                Msg::Played { user, place, universe, name, secs } => {
                    self.history.add_time(place, universe, &name, user, secs);
                    self.history.save();
                }
                Msg::Home { account, part } => {
                    if let HomePart::Continue(Ok(games)) = &part {
                        for g in games {
                            self.history.set_meta(g.place, g.universe, &g.name);
                        }
                        self.history.save();
                    }
                    if let Some(e) = self.home.receive(account, part) {
                        self.say(e);
                    }
                }
                Msg::Avatars(found) => self.avatars.extend(found),
                Msg::Live(list) => self.live.sessions = list,
                Msg::Joined { live, fresh } => self.on_joined(live, fresh),
                Msg::JoinInfo { live, fresh, game, location } => self.on_join_info(live, fresh, game, location),
                Msg::FriendsPlaying(list) => self.on_friends_playing(list),
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        let failed = self.held.iter().any(|s| matches!(s.state, multi::State::Failed(_)));
        let taken = self.held.iter().any(|s| s.state == multi::State::AlreadyExisted);
        let (lock, lock_color, lock_tip) = if failed {
            ("Multi-instance off", RED, "Couldn't take Roblox's single-instance lock.")
        } else if taken {
            ("Lock held by a client", AMBER, "A running client holds the lock. Use More > Release lock.")
        } else {
            ("Multi-instance on", GREEN, "New Roblox clients can start next to running ones.")
        };
        // Who is in which game right now, e.g. "Philip · Brookhaven · London, GB".
        let playing: Vec<String> = self
            .live
            .sessions
            .iter()
            .take(3)
            .map(|s| {
                let mut parts = vec![self.account_name(s.user)];
                parts.extend(self.live.game_name(s.place).map(str::to_string));
                parts.extend(self.live.location(s).map(str::to_string));
                parts.join(" · ")
            })
            .collect();

        ui.horizontal(|ui| {
            let icon = egui::load::SizedTexture::new(self.icon.id(), egui::vec2(22.0, 22.0));
            ui.add(egui::Image::from_texture(icon));
            ui.label(RichText::new("Multi Roblox").strong().size(17.0));
            ui.add_space(6.0);
            pill(ui, &format!("● {lock}"), lock_color).on_hover_text(lock_tip);
            pill(ui, &format!("{} running", self.clients), BLUE);
            for p in &playing {
                pill(ui, p, GREEN);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("More", |ui| {
                    if ui.button("Minimize alts").clicked() {
                        let pids = self.alt_client_pids();
                        for &pid in &pids {
                            multi::minimize_client(pid);
                        }
                        self.say(format!("Minimized {} alt client(s).", pids.len()));
                        ui.close();
                    }
                    if ui.button("Release lock").on_hover_text("Unlock the single-instance lock inside clients that were already running").clicked() {
                        multi::drop_singletons(&mut self.held);
                        let n = multi::release_in_running_clients();
                        self.held = multi::acquire();
                        self.say(format!("Released {n} handle(s)."));
                        ui.close();
                    }
                    if ui.button("Servers...").clicked() {
                        self.open_servers(ServersTab::Recent);
                        ui.close();
                    }
                });
                if ui.button("Kill all").on_hover_text("Close every running Roblox client").clicked() {
                    self.bg.watch().clients.clear();
                    let n = multi::kill_all();
                    self.count_clients();
                    self.say(format!("Closed {n} client(s)."));
                }
                if ui.button("Stop alts").on_hover_text("Close the alt clients started from here; main keeps running").clicked() {
                    self.stop_alts();
                }
                if self.bg.jobs.load(Ordering::Relaxed) > 0 {
                    ui.spinner();
                }
            });
        });
    }

    fn game_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Game");
        ui.add_space(6.0);
        ui.label("Place ID, game link or private server link");
        let a = ui.add(
            egui::TextEdit::singleline(&mut self.cfg.place_id)
                .hint_text("4924922222")
                .desired_width(f32::INFINITY),
        );
        let mut browse = false;
        ui.horizontal(|ui| {
            ui.label("Job ID (optional)");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                browse = ui
                    .small_button("Servers...")
                    .on_hover_text("Browse open servers, rejoin a recent one, or use a saved game")
                    .clicked();
            });
        });
        let b = ui.add(egui::TextEdit::singleline(&mut self.cfg.job_id).desired_width(f32::INFINITY));
        self.dirty |= a.changed() || b.changed();
        ui.horizontal(|ui| {
            ui.label("Server").on_hover_text(
                "Which public server to join when the Job ID is empty. Ping is the average of the \
                 players already in the server, so treat \"Lowest ping\" as a good guess.",
            );
            egui::ComboBox::from_id_salt("server_pick")
                .selected_text(self.cfg.server_pick.label())
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for p in ServerPick::ALL {
                        self.dirty |= ui.selectable_value(&mut self.cfg.server_pick, p, p.label()).changed();
                    }
                });
        });
        if browse {
            let tab = self.servers_tab;
            self.open_servers(tab);
        }
        ui.add_space(6.0);
        ui.separator();

        let main = self.cfg.main.and_then(|id| self.cfg.accounts.iter().find(|a| a.user_id == id));
        let main_name = main.map(|a| a.username.clone());
        let choices: Vec<(u64, String)> =
            self.cfg.accounts.iter().map(|a| (a.user_id, format!("{} (@{})", a.title(), a.username))).collect();
        let mut new_main = None;
        ui.horizontal(|ui| {
            ui.label(RichText::new("Main").strong().color(AMBER));
            let shown = main.map(|a| a.title().to_string()).unwrap_or_else(|| "Pick an account".into());
            egui::ComboBox::from_id_salt("main_pick")
                .selected_text(shown)
                .width(ui.available_width())
                .show_ui(ui, |ui| {
                    for (id, label) in &choices {
                        if ui.selectable_label(self.cfg.main == Some(*id), label).clicked() {
                            new_main = Some(*id);
                        }
                    }
                });
        });
        if let Some(id) = new_main {
            self.set_main(id);
        }
        let alts = self.alt_ids();
        ui.label(format!("{} alt(s) selected", alts.len()));
        ui.add_space(6.0);

        let has_main = main_name.is_some();
        let has_alts = !alts.is_empty();
        let w = ui.available_width();
        let full = |ui: &mut egui::Ui, enabled: bool, text: &str| {
            ui.add_enabled(enabled, egui::Button::new(text).min_size(egui::vec2(w, 28.0)))
        };
        let delay = self.cfg.delay_secs;

        if full(ui, has_main, "Launch main").clicked() {
            if let (Some(t), Some(m)) = (self.target(), self.cfg.main.and_then(|id| self.acct(id))) {
                jobs::launch_many(&self.bg, vec![m], t, delay);
            }
        }
        if full(ui, has_alts, "Launch selected alts").clicked() {
            if let Some(t) = self.target() {
                let list = self.accts(&alts);
                jobs::launch_many(&self.bg, list, t, delay);
            }
        }
        ui.add_space(4.0);
        ui.label(RichText::new("Join the main account").weak());
        if full(ui, has_main && has_alts, "Main joins, then alts follow").clicked() {
            if let (Some(t), Some(m)) = (self.target(), self.cfg.main.and_then(|id| self.acct(id))) {
                let list = self.accts(&alts);
                jobs::join_main(&self.bg, m, list, Some(t), true, false, delay);
            }
        }
        if full(ui, has_main && has_alts, "Alts join main's server now").clicked() {
            if let Some(m) = self.cfg.main.and_then(|id| self.acct(id)) {
                let list = self.accts(&alts);
                jobs::join_main(&self.bg, m, list, None, false, false, delay);
            }
        }
        if full(ui, has_main && has_alts, "Alts follow main (friends)").clicked() {
            if let Some(m) = self.cfg.main.and_then(|id| self.acct(id)) {
                let list = self.accts(&alts);
                jobs::join_main(&self.bg, m, list, None, false, true, delay);
            }
        }
        ui.add_space(6.0);
        ui.label(
            RichText::new("\"Alts join main's server\" copies the exact server the main account is in right now.")
                .weak()
                .small(),
        );
    }

    fn add_panel(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label("Paste the .ROBLOSECURITY cookie of the account to add:");
            ui.add(
                egui::TextEdit::singleline(&mut self.cookie_input)
                    .password(true)
                    .hint_text("_|WARNING:-DO-NOT-SHARE-THIS...")
                    .desired_width(f32::INFINITY),
            );
            ui.label(
                RichText::new(
                    "Get it in a browser logged into that account: F12 > Application > Cookies > roblox.com. \
                     It is stored encrypted with Windows DPAPI on this PC only.",
                )
                .weak()
                .small(),
            );
            ui.horizontal(|ui| {
                let ready = !self.cookie_input.trim().is_empty() && !self.adding;
                if ui.add_enabled(ready, egui::Button::new("Add account")).clicked() {
                    self.adding = true;
                    jobs::add_account(&self.bg, self.cookie_input.clone());
                }
                if self.adding {
                    ui.colored_label(BLUE, "Checking...");
                }
            });
        });
        ui.add_space(6.0);
    }

    fn tabs(&mut self, ui: &mut egui::Ui) {
        ui.selectable_value(&mut self.tab, Tab::Home, RichText::new("Home").heading());
        ui.selectable_value(&mut self.tab, Tab::Accounts, RichText::new("Accounts").heading());
        ui.selectable_value(&mut self.tab, Tab::Settings, RichText::new("Settings").heading());
    }

    fn settings_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| self.tabs(ui));
        ui.add_space(4.0);
        let mut changed = false;
        let mut startup_failed = false;
        let discord_status = self.live.discord_status.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let c = &mut self.cfg;
            card(ui, "AFK mode", "Applies to alts. Graphics and FPS take effect on the next launch, the rest right away.", |ui| {
            changed |= ui
                .checkbox(&mut c.afk_include_main, "Include main")
                .on_hover_text("Also apply AFK mode to the main account.")
                .changed();
            changed |= ui
                .checkbox(&mut c.low_graphics, "Lowest graphics")
                .on_hover_text("Launch with graphics quality 1. Your own setting comes back for clients that aren't AFK.")
                .changed();
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut c.fps_cap_on, "FPS cap")
                    .on_hover_text(
                        "Launch with a frame-rate limit to save CPU and GPU. Roblox's own menu stops at 30, \
                         so 1-15 may be ignored - check with Shift+F5 in the client.",
                    )
                    .changed();
                ui.add_enabled_ui(c.fps_cap_on, |ui| {
                    egui::ComboBox::from_id_salt("fps_cap").selected_text(format!("{} FPS", c.fps_cap)).show_ui(ui, |ui| {
                        for v in FPS_CAPS {
                            changed |= ui.selectable_value(&mut c.fps_cap, v, format!("{v} FPS")).changed();
                        }
                    });
                });
            });
            ui.horizontal(|ui| {
                changed |= ui
                    .checkbox(&mut c.cpu_limit_on, "CPU limit")
                    .on_hover_text(
                        "Windows caps how much of the PC's total CPU each client may use. Works right away. \
                         Too low can make Roblox lag so badly it disconnects - start around 10% and lower carefully.",
                    )
                    .changed();
                ui.add_enabled_ui(c.cpu_limit_on, |ui| {
                    changed |= ui
                        .add(egui::DragValue::new(&mut c.cpu_limit).range(1..=50).suffix("% per client"))
                        .changed();
                });
            });
            changed |= ui
                .checkbox(&mut c.mute_alts, "Mute")
                .on_hover_text("Mutes the client in the Windows volume mixer.")
                .changed();
            changed |= ui
                .checkbox(&mut c.low_priority_alts, "Low CPU priority")
                .on_hover_text("Runs the client at below-normal priority so anything else you do stays smooth.")
                .changed();
            changed |= ui
                .checkbox(&mut c.minimize_alts, "Minimize after launch")
                .on_hover_text("Minimizes the window about 15 seconds after it opens.")
                .changed();
            });

            card(ui, "Staying in game", "", |ui| {
            changed |= ui
                .checkbox(&mut c.anti_afk, "Anti-AFK")
                .on_hover_text(
                    "Every few minutes, briefly brings each Roblox window to the front and presses Space or \
                     nudges the mouse, so the 20-minute idle kick never happens. Waits for a pause in your \
                     typing and puts your window back afterwards. Doesn't work while the PC is locked.",
                )
                .changed();
            ui.add_enabled_ui(c.anti_afk, |ui| {
                ui.indent("anti_afk", |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Every");
                        changed |= ui.add(egui::DragValue::new(&mut c.afk_minutes).range(1..=16).suffix(" min")).changed();
                    });
                    ui.horizontal(|ui| {
                        changed |= ui.radio_value(&mut c.afk_jump, true, "Jump (Space)").changed();
                        changed |= ui.radio_value(&mut c.afk_jump, false, "Mouse nudge").changed();
                    });
                });
            });
            changed |= ui
                .checkbox(&mut c.auto_rejoin, "Auto-rejoin dropped accounts")
                .on_hover_text(
                    "Every 30 s, checks each client launched from here. If its account is no longer in game \
                     (disconnected, kicked, or the 20-minute idle kick), the client is closed and launched again.",
                )
                .changed();
            changed |= ui
                .checkbox(&mut c.keep_awake, "Keep PC awake")
                .on_hover_text("Stops Windows from going to sleep while Multi Roblox is open. The screen can still turn off.")
                .changed();
            });

            card(ui, "Notifications", "Shown as Windows notifications from the tray icon.", |ui| {
                changed |= ui
                    .checkbox(&mut c.notify_joins, "When a client joins a game, with the server's location")
                    .changed();
                changed |= ui
                    .checkbox(&mut c.friend_alerts, "When a friend of any of your accounts starts playing")
                    .on_hover_text("Checked once a minute.")
                    .changed();
            });

            card(ui, "Discord", "Shows the game you're in on your Discord profile, with a Join button for friends.", |ui| {
                changed |= ui.checkbox(&mut c.discord_on, "Show what I'm playing on Discord").changed();
                ui.add_enabled_ui(c.discord_on, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Application ID");
                        changed |= ui
                            .add(
                                egui::TextEdit::singleline(&mut c.discord_app_id)
                                    .hint_text("e.g. 1234567890123456789")
                                    .desired_width(190.0),
                            )
                            .changed();
                    });
                    ui.label(
                        RichText::new(
                            "Discord needs one: at discord.com/developers click New Application, name it what \
                             your profile should say (e.g. Roblox), then copy its Application ID here.",
                        )
                        .weak()
                        .small(),
                    );
                    if c.discord_on && !discord_status.is_empty() {
                        let color = if discord_status == "Connected" { GREEN } else { AMBER };
                        ui.colored_label(color, &discord_status);
                    }
                });
            });

            card(ui, "Launching", "", |ui| {
                let sw = ui
                    .checkbox(&mut self.start_with_windows, "Start with Windows")
                    .on_hover_text("Opens Multi Roblox minimized when you sign in to Windows.");
                if sw.changed() && !multi::set_start_with_windows(self.start_with_windows) {
                    self.start_with_windows = multi::starts_with_windows();
                    startup_failed = true;
                }
                ui.horizontal(|ui| {
                    ui.label("Delay between launches");
                    changed |= ui.add(egui::DragValue::new(&mut c.delay_secs).range(0..=120).suffix(" s")).changed();
                });
            });
        });
        self.dirty |= changed;
        if startup_failed {
            self.say("Could not change the Windows startup entry.");
        }
    }

    fn accounts_panel(&mut self, ui: &mut egui::Ui) {
        let running: HashSet<u64> = self.bg.watch().clients.keys().copied().collect();
        ui.horizontal(|ui| {
            self.tabs(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let label = if self.show_add { "Hide add" } else { "Add account" };
                if ui.button(label).clicked() {
                    self.show_add = !self.show_add;
                }
                if ui.button("Refresh all").clicked() {
                    let ids: Vec<u64> = self.cfg.accounts.iter().map(|a| a.user_id).collect();
                    self.refresh(&ids);
                }
                if ui.button("Select none").clicked() {
                    self.cfg.selected.clear();
                    self.dirty = true;
                }
                if ui.button("Select all").clicked() {
                    self.cfg.selected = self.cfg.accounts.iter().map(|a| a.user_id).collect();
                    self.dirty = true;
                }
            });
        });
        ui.add_space(4.0);
        if self.show_add {
            self.add_panel(ui);
        }
        if self.cfg.accounts.is_empty() {
            ui.label(RichText::new("No accounts yet. Add one with its cookie.").weak());
            return;
        }

        let accounts = self.cfg.accounts.clone();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for a in &accounts {
                let id = a.user_id;
                let is_main = self.cfg.main == Some(id);
                let mut frame = egui::Frame::group(ui.style());
                if is_main {
                    frame = frame.stroke(egui::Stroke::new(1.5, AMBER));
                }
                frame.show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::hover());
                        ui.painter().rect_filled(rect, 18.0, ui.visuals().extreme_bg_color);
                        if let Some(url) = self.avatars.get(&id) {
                            egui::Image::new(self.home.images.uri(url)).corner_radius(18.0).paint_at(ui, rect);
                        }
                        ui.vertical(|ui| {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(a.title()).strong());
                                if is_main {
                                    ui.label(RichText::new("MAIN").small().strong().color(AMBER));
                                }
                            });
                            let (mut text, color) = if self.expired.contains(&id) {
                                ("Session expired - re-add".to_string(), RED)
                            } else {
                                match self.presence.get(&id) {
                                    Some(p) if p.kind == 2 => (p.label(), GREEN),
                                    Some(p) if p.kind == 1 => (p.label(), BLUE),
                                    Some(p) => (p.label(), Color32::GRAY),
                                    None => ("Status unknown".to_string(), Color32::GRAY),
                                }
                            };
                            let session = self.live.sessions.iter().find(|s| s.user == Some(id));
                            if let Some(loc) = session.and_then(|s| self.live.location(s)) {
                                text = format!("{text} · {loc}");
                            }
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("@{}", a.username)).weak().small());
                                ui.label(RichText::new(text).color(color).small());
                            });
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let confirming = self.confirm_remove == Some(id);
                            if ui.button(if confirming { "Sure?" } else { "Remove" }).clicked() {
                                if confirming {
                                    self.cfg.accounts.retain(|x| x.user_id != id);
                                    self.cfg.selected.retain(|x| *x != id);
                                    if self.cfg.main == Some(id) {
                                        self.cfg.main = None;
                                    }
                                    self.presence.remove(&id);
                                    self.confirm_remove = None;
                                    self.dirty = true;
                                    self.sync_accounts();
                                } else {
                                    self.confirm_remove = Some(id);
                                }
                            }
                            if ui.button("Refresh").clicked() {
                                self.refresh(&[id]);
                            }
                            if ui.button("Launch").on_hover_text("Launch just this account into the game").clicked() {
                                if let (Some(t), Some(acct)) = (self.target(), self.acct(id)) {
                                    jobs::launch_many(&self.bg, vec![acct], t, 0);
                                }
                            }
                            if running.contains(&id)
                                && ui.button("Stop").on_hover_text("Close this account's Roblox client").clicked()
                            {
                                self.stop_client(id, &a.username);
                            }
                            if !is_main {
                                let mut sel = self.cfg.selected.contains(&id);
                                if ui.checkbox(&mut sel, "Alt").on_hover_text("Include in alt launches").changed() {
                                    if sel {
                                        self.cfg.selected.push(id);
                                    } else {
                                        self.cfg.selected.retain(|x| *x != id);
                                    }
                                    self.dirty = true;
                                }
                                if ui.button("Make main").clicked() {
                                    self.set_main(id);
                                }
                            }
                        });
                    });
                });
            }
        });
    }

    /// The log: one line with the latest message, or the whole log when opened.
    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let label = if self.log_open { "Hide log" } else { "Log" };
            if ui.small_button(label).clicked() {
                self.log_open = !self.log_open;
            }
            if self.log_open {
                if ui.small_button("Clear").clicked() {
                    self.log.clear();
                }
            } else if let Some(last) = self.log.last() {
                ui.add(egui::Label::new(RichText::new(last).weak().small()).truncate());
            }
        });
        if self.log_open {
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for line in &self.log {
                    ui.label(RichText::new(line).monospace().small());
                }
            });
        }
    }
}

/// A small rounded label in a tint of `color`.
fn pill(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.16))
        .corner_radius(10.0)
        .inner_margin(egui::Margin::symmetric(8, 2))
        .show(ui, |ui| ui.label(RichText::new(text).color(color).small()))
        .response
}

/// A settings group: a title, an optional note, then its controls.
fn card(ui: &mut egui::Ui, title: &str, note: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style()).inner_margin(egui::Margin::same(12)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).strong().size(15.0));
        if !note.is_empty() {
            ui.label(RichText::new(note).weak().small());
        }
        ui.add_space(4.0);
        add(ui);
    });
    ui.add_space(8.0);
}

/// A calmer dark theme with rounder widgets and a blue accent.
fn apply_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.style_mut_of(egui::Theme::Dark, |s| {
        s.spacing.item_spacing = egui::vec2(8.0, 6.0);
        s.spacing.button_padding = egui::vec2(10.0, 4.0);
        let v = &mut s.visuals;
        v.panel_fill = Color32::from_rgb(23, 25, 30);
        v.window_fill = Color32::from_rgb(28, 31, 37);
        v.extreme_bg_color = Color32::from_rgb(15, 16, 20);
        v.faint_bg_color = Color32::from_rgb(30, 33, 40);
        v.window_corner_radius = egui::CornerRadius::same(10);
        v.selection.bg_fill = Color32::from_rgb(45, 105, 200);
        let w = &mut v.widgets;
        for state in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
            state.corner_radius = egui::CornerRadius::same(6);
        }
        w.inactive.weak_bg_fill = Color32::from_rgb(40, 44, 53);
        w.hovered.weak_bg_fill = Color32::from_rgb(52, 58, 70);
        w.active.weak_bg_fill = Color32::from_rgb(60, 68, 84);
    });
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        self.bg.watch().settings = self.settings();
        if self.cfg.keep_awake != self.awake_applied {
            multi::keep_awake(self.cfg.keep_awake);
            self.awake_applied = self.cfg.keep_awake;
        }
        if self.last_poll.elapsed() >= Duration::from_secs(1) {
            self.count_clients();
        }
        self.sync_discord();
        ctx.request_repaint_after(Duration::from_secs(1));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(4.0);
            self.header(ui);
            ui.add_space(4.0);
        });
        // Separate ids so the open log keeps its own size.
        let log = if self.log_open {
            egui::Panel::bottom("log").resizable(true).default_size(160.0)
        } else {
            egui::Panel::bottom("log_line").resizable(false)
        };
        log.show(ui, |ui| self.log_panel(ui));
        egui::Panel::right("game").resizable(false).exact_size(290.0).show(ui, |ui| {
            self.game_panel(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Home => self.home_panel(ui),
            Tab::Accounts => self.accounts_panel(ui),
            Tab::Settings => self.settings_panel(ui),
        });
        let ctx = ui.ctx().clone();
        self.servers_window(&ctx);
        if self.dirty {
            self.save();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        store::save(&self.cfg);
        self.live.tray.close();
    }
}

/// Adds Windows' symbol and emoji fonts as fallbacks, so game names with emoji or symbols don't
/// show empty boxes.
fn add_windows_fonts(ctx: &egui::Context) {
    let Some(windir) = std::env::var_os("WINDIR") else { return };
    let dir = std::path::PathBuf::from(windir).join("Fonts");
    let mut fonts = egui::FontDefinitions::default();
    for (name, file) in [("segoe_symbol", "seguisym.ttf"), ("segoe_emoji", "seguiemj.ttf")] {
        let Ok(bytes) = std::fs::read(dir.join(file)) else { continue };
        fonts.font_data.insert(name.into(), Arc::new(egui::FontData::from_owned(bytes)));
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
            fonts.families.entry(family).or_default().push(name.into());
        }
    }
    ctx.set_fonts(fonts);
}

fn main() -> eframe::Result {
    if !multi::claim_app_guard() {
        multi::message_box("Multi Roblox", "Multi Roblox is already running.");
        return Ok(());
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([940.0, 620.0])
            .with_min_inner_size([760.0, 480.0])
            .with_title("Multi Roblox")
            .with_icon(Arc::new(egui::IconData { rgba: icon::rgba(64), width: 64, height: 64 })),
        ..Default::default()
    };
    eframe::run_native("Multi Roblox", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
