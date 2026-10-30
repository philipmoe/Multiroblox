#![windows_subsystem = "windows"]

mod jobs;
mod multi;
mod roblox;
mod store;

use eframe::egui::{self, Color32, RichText};
use jobs::{Acct, Bg, Msg};
use roblox::{Presence, Target};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(90, 200, 120);
const AMBER: Color32 = Color32::from_rgb(235, 180, 70);
const RED: Color32 = Color32::from_rgb(235, 90, 90);
const BLUE: Color32 = Color32::from_rgb(110, 170, 240);

struct App {
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
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let (tx, rx) = channel();
        let bg = Bg { tx, ctx: cc.egui_ctx.clone(), jobs: Arc::new(AtomicUsize::new(0)) };

        // Free the singleton in clients that were started earlier, then claim it for ourselves.
        let freed = multi::release_in_running_clients();
        let held = multi::acquire();

        let cfg = store::load();
        let mut app = App {
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
        };
        if freed > 0 {
            app.say(format!("Released the singleton lock in {freed} running client handle(s)."));
        }
        app.show_add = app.cfg.accounts.is_empty();
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
        let Some(place) = roblox::parse_place(&self.cfg.place_id) else {
            self.say("Enter a place ID or a roblox.com/games/... link first.");
            return None;
        };
        let job = self.cfg.job_id.trim();
        let job = (!job.is_empty()).then(|| job.to_string());
        Some(Target::Place { place, job })
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
            }
        }
    }

    fn header(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Multi Roblox");
            ui.separator();
            let failed = self.held.iter().any(|s| matches!(s.state, multi::State::Failed(_)));
            let taken = self.held.iter().any(|s| s.state == multi::State::AlreadyExisted);
            if failed {
                ui.colored_label(RED, "Multi-instance lock failed");
            } else if taken {
                ui.colored_label(AMBER, "A client holds the lock - press Release");
            } else {
                ui.colored_label(GREEN, "Multi-instance active");
            }
            ui.label(format!("{} client(s) running", self.clients));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Kill all").on_hover_text("Close every running Roblox client").clicked() {
                    let n = multi::kill_all();
                    self.say(format!("Closed {n} client(s)."));
                }
                if ui
                    .button("Release")
                    .on_hover_text("Unlock the singleton inside clients that were already running")
                    .clicked()
                {
                    multi::drop_singletons(&mut self.held);
                    let n = multi::release_in_running_clients();
                    self.held = multi::acquire();
                    self.say(format!("Released {n} handle(s)."));
                }
                if self.bg.jobs.load(Ordering::Relaxed) > 0 {
                    ui.colored_label(BLUE, "Working...");
                }
            });
        });
    }

    fn game_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(4.0);
        ui.heading("Game");
        ui.add_space(6.0);
        ui.label("Place ID or game link");
        let a = ui.add(
            egui::TextEdit::singleline(&mut self.cfg.place_id)
                .hint_text("4924922222")
                .desired_width(f32::INFINITY),
        );
        ui.label("Job ID (optional, to pick a server)");
        let b = ui.add(egui::TextEdit::singleline(&mut self.cfg.job_id).desired_width(f32::INFINITY));
        self.dirty |= a.changed() || b.changed();
        ui.horizontal(|ui| {
            ui.label("Delay between launches");
            let d = ui.add(egui::DragValue::new(&mut self.cfg.delay_secs).range(0..=120).suffix(" s"));
            self.dirty |= d.changed();
        });
        ui.add_space(6.0);
        ui.separator();

        let main = self.cfg.main.and_then(|id| self.cfg.accounts.iter().find(|a| a.user_id == id));
        let main_name = main.map(|a| a.username.clone());
        match &main_name {
            Some(n) => ui.label(RichText::new(format!("Main: {n}")).strong()),
            None => ui.colored_label(AMBER, "No main account - mark one in the list"),
        };
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

    fn accounts_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Accounts");
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
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if ui
                            .selectable_label(is_main, "Main")
                            .on_hover_text("Make this the main account")
                            .clicked()
                        {
                            self.cfg.main = if is_main { None } else { Some(id) };
                            self.dirty = true;
                        }
                        let mut sel = self.cfg.selected.contains(&id);
                        if ui.add_enabled(!is_main, egui::Checkbox::new(&mut sel, "Alt")).changed() {
                            if sel {
                                self.cfg.selected.push(id);
                            } else {
                                self.cfg.selected.retain(|x| *x != id);
                            }
                            self.dirty = true;
                        }
                        ui.vertical(|ui| {
                            ui.label(RichText::new(a.title()).strong());
                            let (text, color) = if self.expired.contains(&id) {
                                ("Session expired - re-add".to_string(), RED)
                            } else {
                                match self.presence.get(&id) {
                                    Some(p) if p.kind == 2 => (p.label(), GREEN),
                                    Some(p) if p.kind == 1 => (p.label(), BLUE),
                                    Some(p) => (p.label(), Color32::GRAY),
                                    None => ("Status unknown".to_string(), Color32::GRAY),
                                }
                            };
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
                        });
                    });
                });
            }
        });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Log").strong());
            if ui.small_button("Clear").clicked() {
                self.log.clear();
            }
        });
        egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
            for line in &self.log {
                ui.label(RichText::new(line).monospace().small());
            }
        });
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump();
        if self.last_poll.elapsed() > Duration::from_secs(2) {
            self.clients = multi::roblox_pids().len();
            self.last_poll = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(2));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(4.0);
            self.header(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("log").resizable(true).default_size(120.0).show(ui, |ui| {
            self.log_panel(ui);
        });
        egui::Panel::right("game").resizable(false).exact_size(290.0).show(ui, |ui| {
            self.game_panel(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.accounts_panel(ui);
        });
        if self.dirty {
            self.save();
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        store::save(&self.cfg);
    }
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
            .with_title("Multi Roblox"),
        ..Default::default()
    };
    eframe::run_native("Multi Roblox", options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}
