//! Background work. Everything that touches the network or waits runs here so the UI never blocks;
//! results come back to the UI as `Msg` values.

use crate::multi;
use crate::roblox::{self, ApiError, Presence, Server, Target};
use crate::store::{self, Account};
use eframe::egui;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

/// How often the watcher asks Roblox whether a tracked account is still in game.
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// How long a fresh client may take to load into a game before it counts as failed.
const LOAD_GRACE: Duration = Duration::from_secs(180);
/// Rejoins in a row that may fail before the watcher gives up on an account.
const MAX_ATTEMPTS: u8 = 3;

pub enum Msg {
    Log(String),
    Added(Account),
    AddFailed,
    Presence(u64, Presence),
    Expired(u64),
    /// Public servers for the server picker; `None` when the lookup failed.
    Servers(Option<Vec<Server>>),
}

#[derive(Clone)]
pub struct Bg {
    pub tx: Sender<Msg>,
    pub ctx: egui::Context,
    pub jobs: Arc<AtomicUsize>,
    pub watch: Arc<Mutex<Watch>>,
}

impl Bg {
    pub fn send(&self, m: Msg) {
        let _ = self.tx.send(m);
        self.ctx.request_repaint();
    }

    pub fn log(&self, s: impl Into<String>) {
        self.send(Msg::Log(s.into()));
    }

    pub fn spawn(&self, f: impl FnOnce(&Bg) + Send + 'static) {
        let bg = self.clone();
        bg.jobs.fetch_add(1, Ordering::Relaxed);
        thread::spawn(move || {
            f(&bg);
            bg.jobs.fetch_sub(1, Ordering::Relaxed);
            bg.ctx.request_repaint();
        });
    }

    pub fn watch(&self) -> MutexGuard<'_, Watch> {
        self.watch.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// An account with its cookie already decrypted, ready to be moved onto a worker thread.
#[derive(Clone)]
pub struct Acct {
    pub id: u64,
    pub name: String,
    pub cookie: String,
}

/// Clients this app started, keyed by account, plus the settings the watcher follows.
#[derive(Default)]
pub struct Watch {
    pub clients: HashMap<u64, Client>,
    pub auto_rejoin: bool,
    pub low_priority_alts: bool,
    pub main: Option<u64>,
}

/// A running client and what is needed to bring its account back after a disconnect.
pub struct Client {
    pub pid: u32,
    acct: Acct,
    target: Target,
    launched: Instant,
    last_check: Instant,
    seen_in_game: bool,
    misses: u8,
    attempts: u8,
    rejoining: bool,
    low_priority: Option<bool>,
}

/// Launches are serialised: Roblox's launcher trips over itself when started several times at once.
static LAUNCH_LOCK: Mutex<()> = Mutex::new(());

fn lock_launches() -> std::sync::MutexGuard<'static, ()> {
    LAUNCH_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn sleep_secs(s: u32) {
    thread::sleep(Duration::from_secs(s as u64));
}

pub fn add_account(bg: &Bg, raw_cookie: String) {
    bg.spawn(move |bg| {
        let cookie = roblox::clean_cookie(&raw_cookie);
        let result = roblox::authenticated_user(&cookie).and_then(|u| {
            let enc = store::encrypt(&cookie)
                .ok_or_else(|| ApiError::Other("could not encrypt the cookie".into()))?;
            Ok(Account { user_id: u.id, username: u.name, display_name: u.display, cookie: enc })
        });
        match result {
            Ok(acct) => bg.send(Msg::Added(acct)),
            Err(ApiError::Expired) => {
                bg.log("That cookie was rejected. Copy the full .ROBLOSECURITY value from a logged-in browser.");
                bg.send(Msg::AddFailed);
            }
            Err(e) => {
                bg.log(format!("Could not add account: {e}"));
                bg.send(Msg::AddFailed);
            }
        }
    });
}

pub fn refresh(bg: &Bg, list: Vec<Acct>) {
    bg.spawn(move |bg| {
        for a in list {
            match roblox::presence(&a.cookie, a.id) {
                Ok(p) => bg.send(Msg::Presence(a.id, p)),
                Err(ApiError::Expired) => bg.send(Msg::Expired(a.id)),
                Err(e) => bg.log(format!("{}: {e}", a.name)),
            }
        }
    });
}

pub fn servers(bg: &Bg, a: Acct, place: u64, fewest_first: bool) {
    bg.spawn(move |bg| {
        let list = match roblox::servers(&a.cookie, place, fewest_first) {
            Ok(list) => Some(list),
            Err(e) => {
                bg.log(format!("Server list: {e}"));
                None
            }
        };
        bg.send(Msg::Servers(list));
    });
}

/// Turns a share link into the private server it points to; other targets pass through.
fn resolve(bg: &Bg, a: &Acct, target: Target) -> Option<Target> {
    let Target::ShareLink { code } = &target else {
        return Some(target);
    };
    match roblox::resolve_share_link(&a.cookie, code) {
        Ok(t) => Some(t),
        Err(e) => {
            bg.log(format!("Private server link: {e}"));
            None
        }
    }
}

/// Waits for the client a launch just started. Launches are serialised, so the first new
/// process belongs to the account that was launched.
fn new_client(bg: &Bg, before: &[u32]) -> Option<u32> {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        thread::sleep(Duration::from_millis(500));
        let pids = multi::roblox_pids();
        let w = bg.watch();
        let known = |p: &u32| before.contains(p) || w.clients.values().any(|c| c.pid == *p);
        if let Some(pid) = pids.into_iter().find(|p| !known(p)) {
            return Some(pid);
        }
    }
    None
}

fn track(bg: &Bg, a: &Acct, pid: u32, target: &Target) {
    let mut w = bg.watch();
    // A rejoin keeps counting attempts so a server that keeps failing is eventually given up on.
    let attempts = w.clients.get(&a.id).filter(|c| c.rejoining).map_or(0, |c| c.attempts);
    let now = Instant::now();
    w.clients.insert(
        a.id,
        Client {
            pid,
            acct: a.clone(),
            target: target.clone(),
            launched: now,
            last_check: now,
            seen_in_game: false,
            misses: 0,
            attempts,
            rejoining: false,
            low_priority: None,
        },
    );
}

fn launch_one(bg: &Bg, a: &Acct, target: &Target) -> bool {
    let before = multi::roblox_pids();
    match roblox::launch(&a.cookie, target) {
        Ok(()) => {
            bg.log(format!("Launching {}...", a.name));
            match new_client(bg, &before) {
                Some(pid) => track(bg, a, pid, target),
                None => bg.log(format!(
                    "{}: could not find the new client, so it won't be renamed or rejoined.",
                    a.name
                )),
            }
            true
        }
        Err(ApiError::Expired) => {
            bg.send(Msg::Expired(a.id));
            bg.log(format!("{}: session expired - re-add this account", a.name));
            false
        }
        Err(e) => {
            bg.log(format!("{}: {e}", a.name));
            false
        }
    }
}

fn launch_all(bg: &Bg, list: &[Acct], target: &Target, delay: u32) {
    let mut last: Option<Instant> = None;
    for a in list {
        if let Some(t) = last {
            // Waiting for the previous client to appear already used up part of the delay.
            thread::sleep(Duration::from_secs(delay as u64).saturating_sub(t.elapsed()));
        }
        last = Some(Instant::now());
        launch_one(bg, a, target);
    }
}

pub fn launch_many(bg: &Bg, list: Vec<Acct>, target: Target, delay: u32) {
    bg.spawn(move |bg| {
        let _g = lock_launches();
        let Some(target) = list.first().and_then(|a| resolve(bg, a, target)) else {
            return;
        };
        launch_all(bg, &list, &target, delay);
    });
}

/// Brings `alts` into the server `main` is in.
///
/// With `initial` the main account is launched first. With `wait` the job keeps checking the main
/// account's presence until it has loaded into a game. With `follow` the alts use "join user"
/// instead of the exact server (this needs the main account to allow it, e.g. as a friend).
pub fn join_main(
    bg: &Bg,
    main: Acct,
    alts: Vec<Acct>,
    initial: Option<Target>,
    wait: bool,
    follow: bool,
    delay: u32,
) {
    bg.spawn(move |bg| {
        let _g = lock_launches();
        if let Some(t) = initial {
            let Some(t) = resolve(bg, &main, t) else {
                return;
            };
            if !launch_one(bg, &main, &t) {
                return;
            }
            // Alts can open the same private server link themselves; no need to wait for main.
            if let Target::Private { .. } = t {
                sleep_secs(delay);
                launch_all(bg, &alts, &t, delay);
                return;
            }
            sleep_secs(delay.min(10));
        }

        let started = Instant::now();
        let mut announced = false;
        let target = loop {
            match roblox::presence(&main.cookie, main.id) {
                Ok(p) => {
                    bg.send(Msg::Presence(main.id, p.clone()));
                    if p.kind == 2 {
                        break match (p.place_id, p.game_id) {
                            (Some(place), Some(job)) if !follow => Target::Place { place, job: Some(job) },
                            _ => Target::Follow { user_id: main.id },
                        };
                    }
                }
                Err(ApiError::Expired) => {
                    bg.send(Msg::Expired(main.id));
                    bg.log(format!("{}: session expired - re-add this account", main.name));
                    return;
                }
                Err(e) => bg.log(format!("{}: {e}", main.name)),
            }
            if !wait {
                bg.log(format!("{} is not in a game right now.", main.name));
                return;
            }
            if started.elapsed() > Duration::from_secs(150) {
                bg.log(format!("Gave up waiting for {} to join a game.", main.name));
                return;
            }
            if !announced {
                bg.log(format!("Waiting for {} to load into a game...", main.name));
                announced = true;
            }
            thread::sleep(Duration::from_secs(5));
        };

        if let Target::Follow { .. } = target {
            bg.log("Using \"follow user\" to join the main account.");
        }
        launch_all(bg, &alts, &target, delay);
    });
}

/// Starts the thread that looks after launched clients: it names their windows after the account,
/// sets alt priority and, with auto-rejoin on, relaunches accounts that drop out of their game.
pub fn start_watcher(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(3));
        tick(&bg);
    });
}

fn tick(bg: &Bg) {
    let alive = multi::roblox_pids();
    let mut closed = Vec::new();
    let mut windows = Vec::new();
    let mut due = Vec::new();
    {
        let mut w = bg.watch();
        let (low_alts, main, rejoin) = (w.low_priority_alts, w.main, w.auto_rejoin);
        w.clients.retain(|_, c| {
            let keep = c.rejoining || alive.contains(&c.pid);
            if !keep {
                closed.push(c.acct.name.clone());
            }
            keep
        });
        for (id, c) in w.clients.iter_mut() {
            if c.rejoining {
                continue;
            }
            let low = low_alts && Some(*id) != main;
            let priority = (c.low_priority != Some(low)).then(|| {
                c.low_priority = Some(low);
                low
            });
            windows.push((c.pid, format!("Roblox - {}", c.acct.name), priority));
            if rejoin && c.last_check.elapsed() >= CHECK_EVERY {
                c.last_check = Instant::now();
                due.push(c.acct.clone());
            }
        }
    }
    for name in closed {
        bg.log(format!("{name}'s client closed."));
    }
    // Window and process calls happen outside the lock so a busy client can't stall the UI.
    for (pid, title, priority) in windows {
        multi::set_client_title(pid, &title);
        if let Some(low) = priority {
            multi::set_low_priority(pid, low);
        }
    }
    for a in due {
        check(bg, a);
    }
}

/// Asks Roblox whether a tracked account is still in game and starts a rejoin when it is not.
///
/// A client that was disconnected or kicked (for example by the 20-minute idle kick) stays open on
/// its error screen, but the account's presence drops from "in game" to "online".
fn check(bg: &Bg, a: Acct) {
    let in_game = match roblox::presence(&a.cookie, a.id) {
        Ok(p) => {
            let in_game = p.kind == 2;
            bg.send(Msg::Presence(a.id, p));
            in_game
        }
        Err(ApiError::Expired) => {
            bg.send(Msg::Expired(a.id));
            return;
        }
        // A network hiccup is not a disconnect; look again next round.
        Err(_) => return,
    };

    let mut w = bg.watch();
    if !w.auto_rejoin {
        return;
    }
    let Some(c) = w.clients.get_mut(&a.id) else {
        return;
    };
    if in_game {
        c.seen_in_game = true;
        c.misses = 0;
        c.attempts = 0;
        return;
    }
    if c.seen_in_game {
        // One miss can be a teleport between places; two in a row means the client was dropped.
        c.misses += 1;
        if c.misses < 2 {
            return;
        }
    } else if c.launched.elapsed() < LOAD_GRACE {
        return;
    }
    if c.attempts >= MAX_ATTEMPTS {
        bg.log(format!("{}: gave up rejoining after {MAX_ATTEMPTS} tries.", a.name));
        w.clients.remove(&a.id);
        return;
    }
    c.attempts += 1;
    c.rejoining = true;
    let (pid, target, attempt) = (c.pid, c.target.clone(), c.attempts);
    drop(w);
    rejoin(bg, a, pid, target, attempt);
}

fn rejoin(bg: &Bg, a: Acct, old_pid: u32, target: Target, attempt: u8) {
    bg.spawn(move |bg| {
        bg.log(format!("{} dropped out of the game - rejoining (try {attempt} of {MAX_ATTEMPTS}).", a.name));
        multi::kill(old_pid);
        let _g = lock_launches();
        // The first try goes back to the same server; after that any server of the game will do.
        let target = if attempt > 1 { target.any_server() } else { target };
        launch_one(bg, &a, &target);
        // If no new client showed up, stop holding the entry; the next tick drops the dead client.
        if let Some(c) = bg.watch().clients.get_mut(&a.id) {
            if c.pid == old_pid {
                c.rejoining = false;
            }
        }
    });
}
