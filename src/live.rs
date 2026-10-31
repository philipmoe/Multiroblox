//! What's happening right now: the clients in a game (from Roblox's logs), notifications for joins
//! with the server's location and for friends who start playing, and the Discord status.

use crate::discord::{Activity, Discord, Status};
use crate::history::Visit;
use crate::jobs;
use crate::logwatch::Live;
use crate::roblox::Game;
use crate::tray::Tray;
use crate::App;
use std::collections::HashMap;

pub struct LiveState {
    /// Clients in a game right now.
    pub sessions: Vec<Live>,
    /// Place -> (name, icon URL) of games seen in sessions.
    pub games: HashMap<u64, (String, Option<String>)>,
    /// Server address -> "City, CC".
    pub locations: HashMap<String, String>,
    pub tray: Tray,
    pub discord: Discord,
    pub discord_status: String,
}

impl LiveState {
    pub fn new() -> LiveState {
        LiveState {
            sessions: Vec::new(),
            games: HashMap::new(),
            locations: HashMap::new(),
            tray: Tray::start(),
            discord: Discord::start(),
            discord_status: String::new(),
        }
    }

    pub fn game_name(&self, place: u64) -> Option<&str> {
        self.games.get(&place).map(|(n, _)| n.as_str())
    }

    pub fn location(&self, live: &Live) -> Option<&str> {
        live.ip.as_ref().and_then(|ip| self.locations.get(ip)).map(String::as_str)
    }
}

impl App {
    /// A saved account's username, or a stand-in for accounts that aren't saved here.
    pub(crate) fn account_name(&self, user: Option<u64>) -> String {
        match user {
            Some(id) => match self.cfg.accounts.iter().find(|a| a.user_id == id) {
                Some(a) => a.username.clone(),
                None => format!("Account {id}"),
            },
            None => "Roblox".into(),
        }
    }

    pub(crate) fn on_joined(&mut self, live: Live, fresh: bool) {
        let game = self.live.game_name(live.place).unwrap_or_default().to_string();
        let location = self.live.location(&live).unwrap_or_default().to_string();
        let visit = Visit {
            place: live.place,
            job: live.job.clone(),
            user: live.user,
            game: game.clone(),
            location: location.clone(),
            at: live.since,
        };
        self.history.add_visit(visit);
        self.history.save();
        let known_game = !game.is_empty();
        if known_game && (!location.is_empty() || live.ip.is_none()) {
            if fresh {
                self.notify_join(&live);
            }
        } else {
            jobs::join_info(&self.bg, live, fresh, known_game);
        }
    }

    pub(crate) fn on_join_info(&mut self, live: Live, fresh: bool, game: Option<Game>, location: Option<String>) {
        if let Some(g) = game {
            self.live.games.insert(live.place, (g.name, g.icon));
        }
        if let (Some(ip), Some(loc)) = (&live.ip, location) {
            self.live.locations.insert(ip.clone(), loc);
        }
        let name = self.live.game_name(live.place).unwrap_or_default().to_string();
        let loc = self.live.location(&live).unwrap_or_default().to_string();
        self.history.describe_visit(&live.job, &name, &loc);
        self.history.save();
        if fresh {
            self.notify_join(&live);
        }
    }

    fn notify_join(&mut self, live: &Live) {
        let game = self.live.game_name(live.place).unwrap_or("a game").to_string();
        let who = self.account_name(live.user);
        let body = match self.live.location(live) {
            Some(loc) => format!("{who} · Server in {loc}"),
            None => who,
        };
        self.say(format!("Joined {game}: {body}"));
        if self.cfg.notify_joins {
            self.live.tray.notify(format!("Joined {game}"), body);
        }
    }

    pub(crate) fn on_friends_playing(&mut self, list: Vec<(String, String)>) {
        for (name, game) in &list {
            self.say(format!("Friend online: {name} is playing {game}."));
        }
        if !self.cfg.friend_alerts {
            return;
        }
        match list.as_slice() {
            [(name, game)] => self.live.tray.notify(format!("{name} is playing"), game.clone()),
            many => {
                let lines: Vec<String> = many.iter().take(4).map(|(n, g)| format!("{n}: {g}")).collect();
                let more = if many.len() > 4 { format!("\n+{} more", many.len() - 4) } else { String::new() };
                self.live.tray.notify(format!("{} friends started playing", many.len()), lines.join("\n") + &more);
            }
        }
    }

    /// Keeps the Discord status in line with the settings and the client in game. The main
    /// account's client is shown when it's playing, otherwise the first client in a game.
    pub(crate) fn sync_discord(&mut self) {
        while let Ok(s) = self.live.discord.status.try_recv() {
            self.live.discord_status = match s {
                Status::Off => String::new(),
                Status::Waiting => "Waiting for the Discord app to open".into(),
                Status::Connected => "Connected".into(),
                Status::Error(e) => format!("Error: {e}"),
            };
        }
        let app_id = self.cfg.discord_app_id.trim().to_string();
        if !self.cfg.discord_on || app_id.is_empty() {
            self.live.discord.set(None);
            return;
        }
        let main = self.cfg.main;
        let session = self.live.sessions.iter().find(|s| s.user.is_some() && s.user == main).or(self.live.sessions.first());
        let Some(s) = session else {
            self.live.discord.set(None);
            return;
        };
        let (name, icon) = self.live.games.get(&s.place).cloned().unwrap_or_default();
        let details = if name.is_empty() { "In a game".to_string() } else { name.clone() };
        let state = match self.live.location(s) {
            Some(loc) => format!("Server in {loc}"),
            None => "Playing Roblox".into(),
        };
        let activity = Activity {
            details,
            state,
            start: s.since,
            image: icon,
            image_text: name,
            join_url: Some(format!(
                "https://www.roblox.com/games/start?placeId={}&gameInstanceId={}",
                s.place, s.job
            )),
        };
        self.live.discord.set(Some((app_id, activity)));
    }
}
