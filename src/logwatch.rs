//! Reads Roblox's own log files. Every Roblox client on this PC - started from here, the website
//! or anywhere else - writes a log with each game it joins and leaves. From that we know, for any
//! client on any account: which game and server it is in, the server's address, and how long it
//! has been there (for playtime).

use crate::jobs::{Bg, Msg};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// How often logs are read; joins show up within this.
const EVERY: Duration = Duration::from_secs(5);
/// How often collected playtime is handed over (and saved).
const FLUSH_EVERY: u64 = 60;
const FILE_SHARE_READ: u32 = 0x1;
const ERROR_SHARING_VIOLATION: i32 = 32;

/// A client in a game right now.
#[derive(Clone, PartialEq)]
pub struct Live {
    /// Unknown until the join finishes loading.
    pub user: Option<u64>,
    pub place: u64,
    pub universe: Option<u64>,
    pub job: String,
    /// Public address of the game server.
    pub ip: Option<String>,
    /// Unix seconds when the client joined.
    pub since: u64,
}

struct InGame {
    live: Live,
    /// Reported to the UI as a join yet.
    announced: bool,
    unflushed: u64,
}

#[derive(Default)]
struct Session {
    read_to: u64,
    game: Option<InGame>,
    /// Read before. A join found on the first read happened before we were watching.
    seen: bool,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn logs_dir() -> Option<PathBuf> {
    Some(PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox").join("logs"))
}

/// A log is in use while its client runs: Roblox keeps it open for writing, so opening it without
/// sharing write access fails with a sharing violation.
fn in_use(path: &Path) -> bool {
    match OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(path) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(ERROR_SHARING_VIOLATION),
    }
}

/// The number right after `key` in a log line.
fn num_after(line: &str, key: &str) -> Option<u64> {
    let rest = &line[line.find(key)? + key.len()..];
    rest.chars().take_while(char::is_ascii_digit).collect::<String>().parse().ok()
}

/// The unix time a log line starts with ("2026-10-31T10:06:08.699Z,...").
fn line_time(line: &str) -> Option<u64> {
    let n = |r: std::ops::Range<usize>| line.get(r)?.parse::<i64>().ok();
    let (y, m, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (hh, mm, ss) = (n(11..13)?, n(14..16)?, n(17..19)?);
    // Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3600 + mm * 60 + ss).ok()
}

/// Reads the lines written since last time and updates which game the client is in. Time collected
/// in a game is handed over when the client leaves it.
fn read_new(bg: &Bg, path: &Path, s: &mut Session) {
    let Ok(mut f) = File::open(path) else { return };
    if f.metadata().map(|m| m.len()).unwrap_or(0) < s.read_to {
        s.read_to = 0;
    }
    let mut buf = Vec::new();
    if f.seek(SeekFrom::Start(s.read_to)).is_err() || f.read_to_end(&mut buf).is_err() {
        return;
    }
    // Only whole lines; a line still being written is picked up next time.
    let end = buf.iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
    s.read_to += end as u64;
    for line in String::from_utf8_lossy(&buf[..end]).lines() {
        if line.contains("! Joining game") {
            let job = line.split('\'').nth(1).unwrap_or_default().to_string();
            if let Some(place) = num_after(line, " place ") {
                if let Some(g) = s.game.as_mut() {
                    flush(bg, g);
                }
                let since = line_time(line).unwrap_or_else(now);
                let live = Live { user: None, place, universe: None, job, ip: None, since };
                s.game = Some(InGame { live, announced: false, unflushed: 0 });
            }
        } else if line.contains("UDMUX Address = ") {
            if let Some(g) = s.game.as_mut() {
                let rest = &line[line.find("UDMUX Address = ").unwrap_or(0) + 16..];
                let ip: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '.').collect();
                g.live.ip = (!ip.is_empty()).then_some(ip);
            }
        } else if line.contains("game_join_loadtime") {
            if let Some(g) = s.game.as_mut() {
                g.live.universe = num_after(line, "universeid:");
                g.live.user = num_after(line, "userid:");
            }
        } else if line.contains("Client:Disconnect") || line.contains("leaveUGCGameInternal") {
            if let Some(g) = s.game.as_mut() {
                flush(bg, g);
            }
            s.game = None;
        }
    }
}

fn flush(bg: &Bg, g: &mut InGame) {
    if g.unflushed == 0 {
        return;
    }
    bg.send(Msg::Played {
        user: g.live.user.unwrap_or(0),
        place: g.live.place,
        universe: g.live.universe,
        name: String::new(),
        secs: g.unflushed,
    });
    g.unflushed = 0;
}

/// Starts the thread that reports joins, the clients in game and playtime to the UI.
pub fn start(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || {
        let mut sessions: HashMap<PathBuf, Session> = HashMap::new();
        let mut last = Instant::now();
        let mut last_live: Vec<Live> = Vec::new();
        loop {
            thread::sleep(EVERY);
            // Count the real gap, but not hours if the PC was asleep.
            let secs = last.elapsed().as_secs().min(2 * EVERY.as_secs());
            last = Instant::now();
            let Some(dir) = logs_dir() else { continue };
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            let day_ago = SystemTime::now() - Duration::from_secs(24 * 3600);

            let mut active = HashSet::new();
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.contains("_Player_") || name.contains("CrashHandler") || !name.ends_with(".log") {
                    continue;
                }
                let recent = e.metadata().and_then(|m| m.modified()).is_ok_and(|t| t > day_ago);
                let path = e.path();
                if !recent || !in_use(&path) {
                    continue;
                }
                active.insert(path.clone());
                let s = sessions.entry(path.clone()).or_default();
                // The time since the last look belongs to the game the client was in then.
                if let (true, Some(g)) = (s.seen, s.game.as_mut()) {
                    g.unflushed += secs;
                }
                read_new(&bg, &path, s);
                let fresh = s.seen;
                if let Some(g) = s.game.as_mut() {
                    // Announce once the join has loaded (the account is known then), or after 20 s.
                    let loaded = g.live.user.is_some() || now().saturating_sub(g.live.since) > 20;
                    if !g.announced && loaded {
                        g.announced = true;
                        bg.send(Msg::Joined { live: g.live.clone(), fresh });
                    }
                    if g.unflushed >= FLUSH_EVERY {
                        flush(&bg, g);
                    }
                }
                s.seen = true;
            }
            // Clients that closed: hand over their last bit of playtime.
            for (p, s) in sessions.iter_mut() {
                if !active.contains(p) {
                    if let Some(g) = s.game.as_mut() {
                        flush(&bg, g);
                    }
                }
            }
            sessions.retain(|p, _| active.contains(p));

            let live: Vec<Live> = sessions.values().filter_map(|s| s.game.as_ref().map(|g| g.live.clone())).collect();
            if live != last_live {
                bg.send(Msg::Live(live.clone()));
                last_live = live;
            }
        }
    });
}
