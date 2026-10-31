//! Playtime from Roblox's own log files. Every Roblox client on this PC - started from here, the
//! website or anywhere else - writes a log with each game it joins and leaves, so this counts time
//! in any game on any account.

use crate::jobs::{Bg, Msg};
use std::collections::{HashMap, HashSet};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

const EVERY: Duration = Duration::from_secs(30);
const FILE_SHARE_READ: u32 = 0x1;
const ERROR_SHARING_VIOLATION: i32 = 32;

#[derive(Clone, Copy)]
struct InGame {
    place: u64,
    universe: Option<u64>,
    user: Option<u64>,
}

#[derive(Default)]
struct Session {
    read_to: u64,
    game: Option<InGame>,
    /// Time before the first look isn't counted: the client may have been running for hours.
    seen: bool,
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

/// Reads the lines written since last time and updates which game the client is in.
fn read_new(path: &Path, s: &mut Session) {
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
            if let Some(place) = num_after(line, " place ") {
                s.game = Some(InGame { place, universe: None, user: None });
            }
        } else if line.contains("game_join_loadtime") {
            if let Some(g) = s.game.as_mut() {
                g.universe = num_after(line, "universeid:");
                g.user = num_after(line, "userid:");
            }
        } else if line.contains("Client:Disconnect") || line.contains("leaveUGCGameInternal") {
            s.game = None;
        }
    }
}

/// Starts the thread that reports time in game to the UI every 30 seconds.
pub fn start(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || {
        let mut sessions: HashMap<PathBuf, Session> = HashMap::new();
        let mut last = Instant::now();
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
                read_new(&path, s);
                if let (true, Some(g)) = (s.seen, s.game) {
                    bg.send(Msg::Played {
                        user: g.user.unwrap_or(0),
                        place: g.place,
                        universe: g.universe,
                        name: String::new(),
                        secs,
                    });
                }
                s.seen = true;
            }
            sessions.retain(|p, _| active.contains(p));
        }
    });
}
