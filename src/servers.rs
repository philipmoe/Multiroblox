//! The Servers window: the game's public servers, servers you were in recently, and games and
//! private servers saved under a name.

use crate::history;
use crate::jobs;
use crate::roblox::Target;
use crate::store::SavedLink;
use crate::{App, BLUE, RED};
use eframe::egui::{self, RichText};

#[derive(Clone, Copy, PartialEq, Default)]
pub enum ServersTab {
    #[default]
    Browse,
    Recent,
    Saved,
}

enum Pick {
    Job(String),
    Rejoin(usize),
    UseSaved(usize),
    JoinSaved(usize),
    Delete(usize),
    Save,
}

/// One list row: a title and a detail line on the left, buttons on the right.
fn row(ui: &mut egui::Ui, title: &str, detail: &str, buttons: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.set_max_width(ui.available_width() - 140.0);
            ui.add(egui::Label::new(RichText::new(title).strong()).truncate());
            ui.add(egui::Label::new(RichText::new(detail).weak().small()).truncate());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), buttons);
    });
    ui.separator();
}

impl App {
    pub(crate) fn open_servers(&mut self, tab: ServersTab) {
        self.show_servers = true;
        self.servers_tab = tab;
        if tab == ServersTab::Browse && self.servers.is_empty() && !self.servers_loading {
            self.load_servers();
        }
    }

    fn browse_tab(&mut self, ui: &mut egui::Ui, pick: &mut Option<Pick>, reload: &mut bool) {
        ui.horizontal(|ui| {
            *reload |= ui.checkbox(&mut self.fewest_first, "Fewest players first").changed();
            *reload |= ui.button("Reload").clicked();
        });
        ui.add_space(4.0);
        if self.servers_loading {
            ui.colored_label(BLUE, "Loading...");
        } else if self.servers_failed {
            ui.colored_label(RED, "Could not load servers - see the log.");
        } else if self.servers.is_empty() {
            ui.label(RichText::new("Pick a game, then press Reload.").weak());
        } else {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                egui::Grid::new("servers").num_columns(3).striped(true).spacing([16.0, 6.0]).show(ui, |ui| {
                    for s in &self.servers {
                        ui.label(format!("{}/{} players", s.playing, s.max));
                        let ping = s.ping.map_or("-".to_string(), |p| format!("{p} ms"));
                        ui.label(RichText::new(ping).weak());
                        if ui.button("Use").on_hover_text(s.id.as_str()).clicked() {
                            *pick = Some(Pick::Job(s.id.clone()));
                        }
                        ui.end_row();
                    }
                });
            });
        }
    }

    fn recent_tab(&mut self, ui: &mut egui::Ui, pick: &mut Option<Pick>) {
        if self.history.servers.is_empty() {
            ui.label(RichText::new("Servers you join show up here, from any Roblox client on this PC.").weak());
            return;
        }
        let rows: Vec<(String, String)> = self
            .history
            .servers
            .iter()
            .map(|v| {
                let game = if v.game.is_empty() { format!("Place {}", v.place) } else { v.game.clone() };
                let mut detail = vec![self.account_name(v.user)];
                if !v.location.is_empty() {
                    detail.push(v.location.clone());
                }
                detail.push(history::ago(v.at));
                (game, detail.join(" · "))
            })
            .collect();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            for (i, (game, detail)) in rows.iter().enumerate() {
                row(ui, game, detail, |ui| {
                    if ui.button("Rejoin").on_hover_text("Join this exact server again").clicked() {
                        *pick = Some(Pick::Rejoin(i));
                    }
                });
            }
        });
    }

    fn saved_tab(&mut self, ui: &mut egui::Ui, pick: &mut Option<Pick>) {
        let list = self.cfg.saved.clone();
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(300.0).show(ui, |ui| {
            if list.is_empty() {
                ui.label(RichText::new("Nothing saved yet. Save a game or private server link below.").weak());
            }
            for (i, s) in list.iter().enumerate() {
                let detail = if s.job.is_empty() { s.link.clone() } else { format!("{} · server {}", s.link, s.job) };
                row(ui, &s.name, &detail, |ui| {
                    if ui.small_button("Remove").clicked() {
                        *pick = Some(Pick::Delete(i));
                    }
                    if ui.small_button("Use").on_hover_text("Put it in the Game panel").clicked() {
                        *pick = Some(Pick::UseSaved(i));
                    }
                    if ui.button("Join").clicked() {
                        *pick = Some(Pick::JoinSaved(i));
                    }
                });
            }
        });
        ui.add_space(6.0);
        ui.label(RichText::new("Save the game in the Game panel").strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.save_name).hint_text("Name, e.g. Clan server").desired_width(200.0));
            let can = !self.cfg.place_id.trim().is_empty();
            if ui.add_enabled(can, egui::Button::new("Save")).clicked() {
                *pick = Some(Pick::Save);
            }
        });
        ui.label(RichText::new("Saves the place ID or link, and the Job ID if one is set.").weak().small());
    }

    pub(crate) fn servers_window(&mut self, ctx: &egui::Context) {
        if !self.show_servers {
            return;
        }
        let mut open = true;
        let mut pick = None;
        let mut reload = false;
        egui::Window::new("Servers")
            .open(&mut open)
            .collapsible(false)
            .default_size([400.0, 440.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for (t, label) in [
                        (ServersTab::Browse, "Browse"),
                        (ServersTab::Recent, "Recent"),
                        (ServersTab::Saved, "Saved"),
                    ] {
                        ui.selectable_value(&mut self.servers_tab, t, label);
                    }
                });
                ui.separator();
                match self.servers_tab {
                    ServersTab::Browse => self.browse_tab(ui, &mut pick, &mut reload),
                    ServersTab::Recent => self.recent_tab(ui, &mut pick),
                    ServersTab::Saved => self.saved_tab(ui, &mut pick),
                }
            });
        self.show_servers = open;
        if reload {
            self.load_servers();
        }
        if let Some(p) = pick {
            self.servers_action(p);
        }
    }

    fn use_saved(&mut self, i: usize) -> Option<SavedLink> {
        let s = self.cfg.saved.get(i)?.clone();
        self.cfg.place_id = s.link.clone();
        self.cfg.job_id = s.job.clone();
        self.dirty = true;
        Some(s)
    }

    fn servers_action(&mut self, pick: Pick) {
        match pick {
            Pick::Job(id) => {
                self.cfg.job_id = id;
                self.dirty = true;
                self.show_servers = false;
                self.say("Server picked - launches now go to that server.");
            }
            Pick::Rejoin(i) => {
                let Some(v) = self.history.servers.get(i).cloned() else { return };
                // The account that was in it, when it's saved here; otherwise the one Home plays as.
                let acct = v.user.and_then(|id| self.cfg.accounts.iter().any(|a| a.user_id == id).then_some(id));
                let acct = match acct {
                    Some(id) => self.acct(id),
                    None => self.home_account(),
                };
                let Some(a) = acct else {
                    self.say("Add an account first.");
                    return;
                };
                self.cfg.place_id = v.place.to_string();
                self.cfg.job_id = v.job.clone();
                self.dirty = true;
                let game = if v.game.is_empty() { "the server".to_string() } else { v.game };
                self.say(format!("Rejoining {game} as {}...", a.name));
                jobs::launch_many(&self.bg, vec![a], Target::Place { place: v.place, job: Some(v.job) }, 0);
            }
            Pick::UseSaved(i) => {
                if let Some(s) = self.use_saved(i) {
                    self.say(format!("Selected {}.", s.name));
                }
            }
            Pick::JoinSaved(i) => {
                let Some(s) = self.use_saved(i) else { return };
                if let (Some(t), Some(a)) = (self.target(), self.home_account()) {
                    self.say(format!("Joining {} as {}...", s.name, a.name));
                    jobs::launch_many(&self.bg, vec![a], t, 0);
                }
            }
            Pick::Delete(i) => {
                if i < self.cfg.saved.len() {
                    self.cfg.saved.remove(i);
                    self.dirty = true;
                }
            }
            Pick::Save => {
                let link = self.cfg.place_id.trim().to_string();
                let name = match self.save_name.trim() {
                    "" => link.clone(),
                    n => n.to_string(),
                };
                self.cfg.saved.push(SavedLink { name: name.clone(), link, job: self.cfg.job_id.trim().to_string() });
                self.save_name.clear();
                self.dirty = true;
                self.say(format!("Saved {name}."));
            }
        }
    }
}
