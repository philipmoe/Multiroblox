//! Games played and time spent in them, kept on this PC. Playtime only counts while Multi Roblox
//! is open, because Roblox doesn't share real playtime.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Clone)]
pub struct Played {
    /// Root place of the game.
    pub place: u64,
    pub universe: Option<u64>,
    pub name: String,
    /// Unix seconds.
    pub last_played: u64,
    pub seconds: u64,
    #[serde(default)]
    pub by_account: HashMap<u64, u64>,
}

#[derive(Serialize, Deserialize, Default)]
pub struct History {
    pub games: Vec<Played>,
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("MultiRoblox").join("history.json")
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl History {
    pub fn load() -> History {
        std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string(self) {
            let _ = std::fs::write(path(), json);
        }
    }

    fn entry(&mut self, place: u64) -> &mut Played {
        let i = match self.games.iter().position(|g| g.place == place) {
            Some(i) => i,
            None => {
                self.games.push(Played {
                    place,
                    universe: None,
                    name: String::new(),
                    last_played: 0,
                    seconds: 0,
                    by_account: HashMap::new(),
                });
                self.games.len() - 1
            }
        };
        &mut self.games[i]
    }

    pub fn launched(&mut self, place: u64) {
        self.entry(place).last_played = now();
    }

    /// `user` is 0 when the account isn't known. Time in a sub-place counts toward its game when
    /// the game is already known by its universe.
    pub fn add_time(&mut self, place: u64, universe: Option<u64>, name: &str, user: u64, secs: u64) {
        let known = universe.and_then(|u| self.games.iter().find(|g| g.universe == Some(u)).map(|g| g.place));
        let g = self.entry(known.unwrap_or(place));
        g.last_played = now();
        g.seconds += secs;
        *g.by_account.entry(user).or_default() += secs;
        if universe.is_some() {
            g.universe = universe;
        }
        if !name.is_empty() {
            g.name = name.to_string();
        }
    }

    /// Remembers what a lookup found so later lookups can skip it.
    pub fn set_meta(&mut self, place: u64, universe: u64, name: &str) {
        if let Some(g) = self.games.iter_mut().find(|g| g.place == place) {
            g.universe = Some(universe);
            g.name = name.to_string();
        }
    }

    /// Most recently played first.
    pub fn recent(&self, max: usize) -> Vec<&Played> {
        let mut list: Vec<&Played> = self.games.iter().collect();
        list.sort_by(|a, b| b.last_played.cmp(&a.last_played));
        list.truncate(max);
        list
    }

    pub fn get(&self, place: u64) -> Option<&Played> {
        self.games.iter().find(|g| g.place == place)
    }
}

/// "3h 12m", "45m" or "< 1m".
pub fn duration(secs: u64) -> String {
    let (h, m) = (secs / 3600, secs / 60 % 60);
    match (h, m) {
        (0, 0) => "< 1m".into(),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m}m"),
    }
}
