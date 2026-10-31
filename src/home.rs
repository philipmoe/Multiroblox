//! The Home tab: recently played games, favorites, friends in game, Roblox's charts and search.
//! Rows are saved to disk, so Home shows the last results instantly and refreshes them behind.

use crate::history::{self, History};
use crate::imgcache::ImageCache;
use crate::jobs::{self, Acct, HomePart};
use crate::roblox::{FriendGame, Game, Target};
use crate::{App, AMBER, BLUE, GREEN, RED};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

/// Width and height of a card's picture.
const CARD: f32 = 128.0;

/// `None` while loading, then the rows or why they failed.
type Row<T> = Option<Result<T, String>>;

#[derive(Serialize, Deserialize, Default)]
struct AccountRows {
    recent: Vec<Game>,
    favorites: Vec<Game>,
    friends: Vec<FriendGame>,
}

/// What Home showed last time, per account (Charts are the same for everyone).
#[derive(Serialize, Deserialize, Default)]
struct Cache {
    charts: Vec<(String, Vec<Game>)>,
    accounts: HashMap<u64, AccountRows>,
}

fn cache_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("MultiRoblox").join("home_cache.json")
}

pub struct HomeState {
    /// The account the rows belong to; `None` until Home first loads.
    account: Option<u64>,
    recent: Row<Vec<Game>>,
    favorites: Row<Vec<Game>>,
    friends: Row<Vec<FriendGame>>,
    charts: Row<Vec<(String, Vec<Game>)>>,
    /// Rows still being fetched.
    pending: usize,
    query: String,
    /// `None` with `searching` false means no search is shown.
    search: Row<Vec<Game>>,
    searching: bool,
    cache: Cache,
    pub images: ImageCache,
}

/// Puts fresh results in place and remembers them. When the fetch fails but saved results are
/// showing, they stay and the error is returned for the log instead.
fn apply<T: Clone>(row: &mut Row<T>, result: Result<T, String>, saved: &mut T, title: &str) -> Option<String> {
    match result {
        Ok(v) => {
            *saved = v.clone();
            *row = Some(Ok(v));
            None
        }
        Err(e) if matches!(row, Some(Ok(_))) => Some(format!("{title}: {e} (showing saved results)")),
        Err(e) => {
            *row = Some(Err(e));
            None
        }
    }
}

impl HomeState {
    pub fn new(ctx: egui::Context) -> HomeState {
        let cache = std::fs::read_to_string(cache_path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        HomeState {
            account: None,
            recent: None,
            favorites: None,
            friends: None,
            charts: None,
            pending: 0,
            query: String::new(),
            search: None,
            searching: false,
            cache,
            images: ImageCache::new(ctx),
        }
    }

    fn save_cache(&self) {
        if let Ok(json) = serde_json::to_string(&self.cache) {
            let _ = std::fs::write(cache_path(), json);
        }
    }

    /// Switches to `account`, showing its saved rows (if any) while fresh ones load.
    fn show_saved(&mut self, account: u64) {
        self.account = Some(account);
        let rows = self.cache.accounts.get(&account);
        self.recent = rows.map(|r| Ok(r.recent.clone()));
        self.favorites = rows.map(|r| Ok(r.favorites.clone()));
        self.friends = rows.map(|r| Ok(r.friends.clone()));
        self.charts = (!self.cache.charts.is_empty()).then(|| Ok(self.cache.charts.clone()));
        self.pending = 4;
    }

    /// Takes in a loaded row. Returns an error worth logging, if any.
    pub fn receive(&mut self, account: u64, part: HomePart) -> Option<String> {
        if let HomePart::Search(r) = part {
            self.search = Some(r);
            self.searching = false;
            return None;
        }
        // Rows from an account that was switched away from mid-load are dropped.
        if self.account != Some(account) {
            return None;
        }
        self.pending = self.pending.saturating_sub(1);
        let saved = self.cache.accounts.entry(account).or_default();
        let error = match part {
            HomePart::Continue(r) => apply(&mut self.recent, r, &mut saved.recent, "Continue playing"),
            HomePart::Favorites(r) => apply(&mut self.favorites, r, &mut saved.favorites, "Favorites"),
            HomePart::Friends(r) => apply(&mut self.friends, r, &mut saved.friends, "Friends"),
            HomePart::Charts(r) => apply(&mut self.charts, r, &mut self.cache.charts, "Charts"),
            HomePart::Search(_) => None,
        };
        self.save_cache();
        error
    }
}

enum Action {
    Select(Game),
    Play(Game),
    Servers(Game),
    JoinFriend(FriendGame),
}

/// "950", "12.3K", "1.2M", "89.0B"
fn count(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", n as f64 / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", n as f64 / 1e6),
        _ => format!("{:.1}B", n as f64 / 1e9),
    }
}

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(12.0);
    ui.label(RichText::new(title).strong().size(16.0));
    ui.add_space(4.0);
}

/// A card picture: the image when there is one, a grey tile until it loads.
fn picture(ui: &mut egui::Ui, images: &mut ImageCache, url: Option<&str>) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(CARD, CARD), egui::Sense::click());
    ui.painter().rect_filled(rect, 8.0, ui.visuals().extreme_bg_color);
    if let Some(url) = url {
        egui::Image::new(images.uri(url)).corner_radius(8.0).paint_at(ui, rect);
    }
    if resp.hovered() {
        ui.painter().rect_stroke(rect, 8.0, egui::Stroke::new(2.0, BLUE), egui::StrokeKind::Inside);
    }
    resp
}

/// Shows a row's loading spinner, error or empty text. Returns the items when there are some.
fn row_state<'a, T>(ui: &mut egui::Ui, row: &'a Row<Vec<T>>, empty: &str) -> Option<&'a [T]> {
    match row {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new("Loading...").weak());
            });
            None
        }
        Some(Err(e)) => {
            ui.colored_label(RED, format!("Couldn't load: {e}"));
            None
        }
        Some(Ok(items)) if items.is_empty() => {
            ui.label(RichText::new(empty).weak());
            None
        }
        Some(Ok(items)) => Some(items),
    }
}

fn card_strip(ui: &mut egui::Ui, id: &str, add_cards: impl FnOnce(&mut egui::Ui)) {
    egui::ScrollArea::horizontal().id_salt(id).auto_shrink([false, true]).show(ui, |ui| {
        ui.horizontal_top(add_cards);
    });
}

fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).weak());
    ui.label(value);
    ui.end_row();
}

/// The hover card: everything known about a game, plus your playtime in it.
fn stats_ui(ui: &mut egui::Ui, g: &Game, history: &History, names: &HashMap<u64, String>) {
    ui.set_max_width(280.0);
    ui.label(RichText::new(&g.name).strong().size(15.0));
    if let Some(s) = g.stats.as_ref().filter(|s| !s.creator.is_empty()) {
        ui.label(RichText::new(format!("by {}", s.creator)).weak());
    }
    ui.add_space(4.0);
    egui::Grid::new(("stats", g.universe)).num_columns(2).spacing([14.0, 3.0]).show(ui, |ui| {
        if let Some(p) = g.playing {
            stat(ui, "Playing now", count(p));
        }
        if let Some(s) = &g.stats {
            let votes = s.up_votes + s.down_votes;
            if votes > 0 {
                let pct = s.up_votes as f64 * 100.0 / votes as f64;
                stat(ui, "Likes", format!("{pct:.0}%  ({} / {})", count(s.up_votes), count(s.down_votes)));
            }
            if s.visits > 0 {
                stat(ui, "Visits", count(s.visits));
            }
            if s.favorites > 0 {
                stat(ui, "Favorites", count(s.favorites));
            }
            if s.max_players > 0 {
                stat(ui, "Server size", format!("{} players", s.max_players));
            }
            if !s.genre.is_empty() {
                stat(ui, "Genre", s.genre.clone());
            }
            if !s.updated.is_empty() {
                stat(ui, "Updated", s.updated.clone());
            }
        }
    });
    if let Some(h) = history.get(g.place).filter(|h| h.seconds > 0) {
        ui.add_space(6.0);
        ui.label(RichText::new(format!("Your playtime: {}", history::duration(h.seconds))).color(GREEN));
        let mut per: Vec<(&u64, &u64)> = h.by_account.iter().collect();
        per.sort_by(|a, b| b.1.cmp(a.1));
        for (id, secs) in per {
            let name = match (id, names.get(id)) {
                (_, Some(n)) => n.clone(),
                (0, None) => "Unknown account".into(),
                (id, None) => format!("Account {id}"),
            };
            ui.label(RichText::new(format!("  {name}: {}", history::duration(*secs))).small());
        }
    }
    ui.add_space(4.0);
    ui.label(RichText::new("Click to select this game").weak().small());
}

struct CardCtx<'a> {
    history: &'a History,
    names: &'a HashMap<u64, String>,
    player: &'a str,
}

fn game_card(ui: &mut egui::Ui, g: &Game, c: &CardCtx, images: &mut ImageCache, action: &mut Option<Action>) {
    ui.vertical(|ui| {
        ui.set_width(CARD);
        let pic = picture(ui, images, g.icon.as_deref()).on_hover_ui(|ui| stats_ui(ui, g, c.history, c.names));
        if pic.clicked() {
            *action = Some(Action::Select(g.clone()));
        }
        ui.add(egui::Label::new(RichText::new(&g.name).strong()).truncate());
        let playing = g.playing.map(|p| format!("{} playing", count(p))).unwrap_or_default();
        ui.label(RichText::new(playing).weak().small());
        let played = c.history.get(g.place).filter(|h| h.seconds > 0);
        let played = played.map(|h| format!("{} played", history::duration(h.seconds))).unwrap_or_default();
        ui.label(RichText::new(played).small().color(GREEN));
        ui.horizontal(|ui| {
            let tip = format!(
                "Smart play: launches {} into the lowest-ping server that is busy but not full",
                c.player
            );
            if ui.small_button("Play").on_hover_text(tip).clicked() {
                *action = Some(Action::Play(g.clone()));
            }
            if ui.small_button("Servers").on_hover_text("Pick a server for this game").clicked() {
                *action = Some(Action::Servers(g.clone()));
            }
        });
        // Room for the strip's scroll bar, which is drawn over the bottom of the cards.
        ui.add_space(12.0);
    });
    ui.add_space(8.0);
}

fn friend_card(ui: &mut egui::Ui, f: &FriendGame, player: &str, images: &mut ImageCache, action: &mut Option<Action>) {
    ui.vertical(|ui| {
        ui.set_width(CARD);
        picture(ui, images, f.avatar.as_deref());
        let shown = if f.display.is_empty() { &f.name } else { &f.display };
        ui.add(egui::Label::new(RichText::new(shown).strong()).truncate());
        ui.add(egui::Label::new(RichText::new(format!("@{}", f.name)).weak().small()).truncate());
        let game = if f.game.is_empty() { "In a game (hidden)" } else { &f.game };
        ui.add(egui::Label::new(RichText::new(game).small()).truncate()).on_hover_text(game);
        if ui.small_button("Join").on_hover_text(format!("Launch {player} into their server")).clicked() {
            *action = Some(Action::JoinFriend(f.clone()));
        }
        ui.add_space(12.0);
    });
    ui.add_space(8.0);
}

fn game_row(
    ui: &mut egui::Ui,
    title: &str,
    row: &Row<Vec<Game>>,
    empty: &str,
    c: &CardCtx,
    images: &mut ImageCache,
    action: &mut Option<Action>,
) {
    section(ui, title);
    if let Some(games) = row_state(ui, row, empty) {
        card_strip(ui, title, |ui| {
            for g in games {
                game_card(ui, g, c, images, action);
            }
        });
    }
}

impl App {
    /// The account Home browses and plays as: the one picked on Home, else main, else the first.
    pub(crate) fn home_account_id(&self) -> Option<u64> {
        let exists = |id: &u64| self.cfg.accounts.iter().any(|a| a.user_id == *id);
        self.cfg
            .home_account
            .filter(exists)
            .or(self.cfg.main.filter(exists))
            .or_else(|| self.cfg.accounts.first().map(|a| a.user_id))
    }

    pub(crate) fn home_account(&mut self) -> Option<Acct> {
        let id = self.home_account_id()?;
        self.acct(id)
    }

    fn load_home(&mut self) {
        let Some(a) = self.home_account() else { return };
        let recent = self.history.recent(20).iter().map(|g| (g.place, g.universe)).collect();
        self.home.show_saved(a.id);
        jobs::load_home(&self.bg, a, recent);
    }

    fn start_search(&mut self) {
        let query = self.home.query.trim().to_string();
        if query.is_empty() {
            return;
        }
        let Some(a) = self.home_account() else { return };
        self.home.search = None;
        self.home.searching = true;
        jobs::search_games(&self.bg, a, query);
    }

    /// The row of account buttons that picks who Home plays as.
    fn account_switcher(&mut self, ui: &mut egui::Ui) {
        let current = self.home_account_id();
        let mut picked = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Playing as").weak());
            for a in &self.cfg.accounts {
                let id = a.user_id;
                let mut text = RichText::new(a.title());
                if self.cfg.main == Some(id) {
                    text = text.color(AMBER);
                }
                let button = match self.avatars.get(&id) {
                    Some(url) => {
                        let img = egui::Image::new(self.home.images.uri(url))
                            .fit_to_exact_size(egui::vec2(18.0, 18.0))
                            .corner_radius(9.0);
                        egui::Button::image_and_text(img, text)
                    }
                    None => egui::Button::new(text),
                };
                let hover = if self.cfg.main == Some(id) { "Main account" } else { "Browse and play as this account" };
                if ui.add(button.selected(current == Some(id))).on_hover_text(hover).clicked() {
                    picked = Some(id);
                }
            }
        });
        if let Some(id) = picked.filter(|id| current != Some(*id)) {
            self.cfg.home_account = Some(id);
            self.dirty = true;
            self.home.search = None;
            self.home.searching = false;
        }
    }

    pub(crate) fn home_panel(&mut self, ui: &mut egui::Ui) {
        let mut refresh = false;
        ui.horizontal(|ui| {
            self.tabs(ui);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                refresh = ui.button("Refresh").clicked();
                if self.home.pending > 0 {
                    ui.spinner();
                    ui.label(RichText::new("Updating...").weak());
                }
            });
        });
        if self.cfg.accounts.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Add an account first - Home browses Roblox as one of your accounts.").weak());
            return;
        }
        self.account_switcher(ui);
        if refresh || self.home.account != self.home_account_id() {
            self.load_home();
        }

        let mut search = false;
        let mut clear = false;
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.home.query)
                    .hint_text("Search Roblox games")
                    .desired_width(260.0),
            );
            search |= r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            search |= ui.button("Search").clicked();
            if (self.home.search.is_some() || self.home.searching) && ui.button("Clear").clicked() {
                clear = true;
            }
        });
        if search {
            self.start_search();
        }
        if clear {
            self.home.search = None;
            self.home.searching = false;
            self.home.query.clear();
        }

        let names: HashMap<u64, String> = self.cfg.accounts.iter().map(|a| (a.user_id, a.username.clone())).collect();
        let player = self
            .home_account_id()
            .and_then(|id| names.get(&id).cloned())
            .unwrap_or_default();
        let ctx = CardCtx { history: &self.history, names: &names, player: &player };
        let mut action = None;
        let h = &mut self.home;
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if h.searching || h.search.is_some() {
                game_row(ui, "Search results", &h.search, "No games found.", &ctx, &mut h.images, &mut action);
            }
            let empty = "Games you launch from here show up here.";
            game_row(ui, "Continue playing", &h.recent, empty, &ctx, &mut h.images, &mut action);

            section(ui, "Friends playing now");
            if let Some(friends) = row_state(ui, &h.friends, "None of this account's friends are in a game.") {
                card_strip(ui, "friends", |ui| {
                    for f in friends {
                        friend_card(ui, f, &player, &mut h.images, &mut action);
                    }
                });
            }

            game_row(ui, "Favorites", &h.favorites, "No favorites yet.", &ctx, &mut h.images, &mut action);

            match &h.charts {
                Some(Ok(rows)) if !rows.is_empty() => {
                    for (title, games) in rows {
                        let row = Some(Ok(games.clone()));
                        game_row(ui, title, &row, "", &ctx, &mut h.images, &mut action);
                    }
                }
                other => {
                    section(ui, "Charts");
                    let row: Row<Vec<()>> = match other {
                        None => None,
                        Some(Err(e)) => Some(Err(e.clone())),
                        Some(Ok(_)) => Some(Ok(Vec::new())),
                    };
                    row_state(ui, &row, "Roblox didn't return any charts.");
                }
            }
            ui.add_space(12.0);
        });

        if let Some(a) = action {
            self.home_action(a);
        }
    }

    fn select_game(&mut self, g: &Game) {
        self.cfg.place_id = g.place.to_string();
        self.cfg.job_id.clear();
        self.dirty = true;
        self.say(format!("Selected {}.", g.name));
    }

    fn home_action(&mut self, action: Action) {
        match action {
            Action::Select(g) => self.select_game(&g),
            Action::Play(g) => {
                self.select_game(&g);
                if let Some(a) = self.home_account() {
                    jobs::smart_play(&self.bg, a, g.place);
                }
            }
            Action::Servers(g) => {
                self.select_game(&g);
                self.load_servers();
            }
            Action::JoinFriend(f) => {
                let Some(a) = self.home_account() else { return };
                let shown = if f.display.is_empty() { f.name.clone() } else { f.display.clone() };
                // The exact server when it's visible; otherwise "follow user", which needs friendship.
                let target = match (f.place, f.job) {
                    (Some(place), Some(job)) => {
                        self.cfg.place_id = place.to_string();
                        self.cfg.job_id = job.clone();
                        self.dirty = true;
                        Target::Place { place, job: Some(job) }
                    }
                    _ => Target::Follow { user_id: f.user_id },
                };
                self.say(format!("Joining {shown}..."));
                jobs::launch_many(&self.bg, vec![a], target, 0);
            }
        }
    }
}
