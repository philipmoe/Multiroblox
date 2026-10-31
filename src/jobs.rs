//! Background work. Everything that touches the network or waits runs here so the UI never blocks;
//! results come back to the UI as `Msg` values.

use crate::gamesettings::{self, Profile};
use crate::logwatch::Live;
use crate::roblox::{self, ApiError, FriendGame, Game, Presence, Server, Target};
use crate::store::{self, Account, ServerPick};
use crate::{audio, multi};
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
/// How long anti-AFK may hold off while the user is typing before it nudges anyway.
const AFK_MAX_WAIT: Duration = Duration::from_secs(180);
/// How long "minimize after launch" waits, so the client is past its splash screen.
const MINIMIZE_AFTER: Duration = Duration::from_secs(15);
/// How long a client gets to read the graphics settings before the next launch may rewrite them.
const SETTINGS_READ_TIME: Duration = Duration::from_secs(4);
/// How often every saved account's status is refreshed.
const STATUS_EVERY: Duration = Duration::from_secs(60);

pub enum Msg {
    Log(String),
    Added(Account),
    AddFailed,
    Presence(u64, Presence),
    Expired(u64),
    /// Public servers for the server picker; `None` when the lookup failed.
    Servers(Option<Vec<Server>>),
    /// A launch into this root place started.
    Launched(u64),
    /// An account was seen in a game; `secs` is the time since the last look.
    Played { user: u64, place: u64, universe: Option<u64>, name: String, secs: u64 },
    /// A Home row, loaded as `account`.
    Home { account: u64, part: HomePart },
    /// Avatar headshot URLs of the saved accounts.
    Avatars(HashMap<u64, String>),
    /// The clients in a game right now, from Roblox's logs.
    Live(Vec<Live>),
    /// A client joined a server. `fresh` is false for joins from before the app was watching.
    Joined { live: Live, fresh: bool },
    /// What was looked up about a join: the game, and where its server is.
    JoinInfo { live: Live, fresh: bool, game: Option<Game>, location: Option<String> },
    /// Friends of saved accounts who just started playing: (name, game).
    FriendsPlaying(Vec<(String, String)>),
}

/// One row of the Home page, or why it couldn't load.
pub enum HomePart {
    Continue(Result<Vec<Game>, String>),
    Favorites(Result<Vec<Game>, String>),
    Friends(Result<Vec<FriendGame>, String>),
    Charts(Result<Vec<(String, Vec<Game>)>, String>),
    Search(Result<Vec<Game>, String>),
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

    fn settings(&self) -> Settings {
        self.watch().settings.clone()
    }
}

/// An account with its cookie already decrypted, ready to be moved onto a worker thread.
#[derive(Clone)]
pub struct Acct {
    pub id: u64,
    pub name: String,
    pub cookie: String,
}

/// Clients this app started, keyed by account, plus the settings the background threads follow.
#[derive(Default)]
pub struct Watch {
    pub clients: HashMap<u64, Client>,
    pub settings: Settings,
    /// Every saved account, for status refreshes. The UI refreshes it when accounts change.
    pub accounts: Vec<Acct>,
}

/// The user's settings as the background threads need them; the UI copies them in every frame.
#[derive(Clone, Default)]
pub struct Settings {
    pub main: Option<u64>,
    pub server_pick: ServerPick,
    pub include_main: bool,
    pub low_graphics: bool,
    pub fps_cap: Option<u32>,
    /// Percent of the PC's total CPU each AFK client may use.
    pub cpu_limit: Option<u32>,
    pub mute: bool,
    pub low_priority: bool,
    pub minimize: bool,
    pub auto_rejoin: bool,
    pub anti_afk: bool,
    pub afk_minutes: u32,
    pub afk_jump: bool,
    pub friend_alerts: bool,
}

impl Settings {
    /// AFK mode covers every alt, and the main account only with "include main".
    pub fn afk_applies(&self, id: u64) -> bool {
        self.include_main || Some(id) != self.main
    }

    fn profile(&self) -> Profile {
        Profile { low_graphics: self.low_graphics, fps_cap: self.fps_cap }
    }
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
    muted: bool,
    minimized: bool,
    cpu_job: Option<multi::CpuLimit>,
    /// The CPU limit last applied (`Some(None)` = explicitly unlimited), or `None` if never touched.
    cpu_applied: Option<Option<u32>>,
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

/// With a server pick mode set and no job ID given, chooses the public server to launch into.
fn pick_server(bg: &Bg, a: &Acct, target: Target, need: usize, pick: ServerPick) -> Target {
    let Target::Place { place, job: None } = target else {
        return target;
    };
    match roblox::pick_server(&a.cookie, place, pick, need as u32) {
        Ok(Some(s)) => {
            let ping = s.ping.map_or(String::new(), |p| format!(", {p} ms"));
            bg.log(format!("{}: picked a server with {}/{} players{ping}.", pick.label(), s.playing, s.max));
            Target::Place { place, job: Some(s.id) }
        }
        Ok(None) => {
            if pick != ServerPick::Any {
                bg.log(format!("{}: no server with room for {need} found - Roblox will pick one.", pick.label()));
            }
            Target::Place { place, job: None }
        }
        Err(e) => {
            bg.log(format!("Server pick failed ({e}) - Roblox will pick one."));
            Target::Place { place, job: None }
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
            muted: false,
            minimized: false,
            cpu_job: None,
            cpu_applied: None,
        },
    );
}

fn launch_one(bg: &Bg, a: &Acct, target: &Target) -> bool {
    let s = bg.settings();
    let profile = s.profile();
    gamesettings::prepare((profile.any() && s.afk_applies(a.id)).then_some(&profile));
    let before = multi::roblox_pids();
    match roblox::launch(&a.cookie, target) {
        Ok(()) => {
            bg.log(format!("Launching {}...", a.name));
            if let Target::Place { place, .. } | Target::Private { place, .. } = target {
                bg.send(Msg::Launched(*place));
            }
            match new_client(bg, &before) {
                Some(pid) => {
                    track(bg, a, pid, target);
                    if profile.any() {
                        thread::sleep(SETTINGS_READ_TIME);
                    }
                }
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
    let pick = bg.settings().server_pick;
    launch_picked(bg, list, target, delay, pick);
}

/// The Home cards' Play button: one account into the best-scoring server of a game.
pub fn smart_play(bg: &Bg, a: Acct, place: u64) {
    launch_picked(bg, vec![a], Target::Place { place, job: None }, 0, ServerPick::Best);
}

fn launch_picked(bg: &Bg, list: Vec<Acct>, target: Target, delay: u32, pick: ServerPick) {
    bg.spawn(move |bg| {
        let _g = lock_launches();
        let Some(first) = list.first() else {
            return;
        };
        let Some(target) = resolve(bg, first, target) else {
            return;
        };
        let target = pick_server(bg, first, target, list.len(), pick);
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
            let t = pick_server(bg, &main, t, 1 + alts.len(), bg.settings().server_pick);
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
/// applies AFK mode (mute, priority, minimize) and, with auto-rejoin on, relaunches accounts that
/// drop out of their game.
pub fn start_watcher(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(3));
        tick(&bg);
    });
}

/// Starts the anti-AFK loop: every few minutes each Roblox window gets a short key press or mouse
/// nudge, which resets Roblox's idle timer before its 20-minute kick.
pub fn start_anti_afk(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || {
        let mut last = Instant::now();
        loop {
            thread::sleep(Duration::from_secs(5));
            let s = bg.settings();
            if !s.anti_afk {
                last = Instant::now();
                continue;
            }
            let every = Duration::from_secs(s.afk_minutes.max(1) as u64 * 60);
            if last.elapsed() < every {
                continue;
            }
            // Wait for a pause in the user's own typing so focus isn't pulled away mid-word, but
            // never so long that the idle kick could land first.
            if multi::user_idle_secs() < 5 && last.elapsed() < every + AFK_MAX_WAIT {
                continue;
            }
            last = Instant::now();
            let pids = multi::roblox_pids();
            if pids.is_empty() {
                continue;
            }
            let n = multi::nudge_clients(&pids, s.afk_jump);
            if n == 0 {
                bg.log("Anti-AFK: could not bring any Roblox window to the front (is the PC locked?).");
            } else {
                bg.log(format!("Anti-AFK: nudged {n} window(s)."));
            }
        }
    });
}

/// Keeps every saved account's status fresh: once a minute each account's presence is checked.
/// (Playtime comes from Roblox's log files instead; see `logwatch`.)
pub fn start_status_refresh(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || loop {
        thread::sleep(STATUS_EVERY);
        let accounts = bg.watch().accounts.clone();
        for a in accounts {
            if let Ok(p) = roblox::presence(&a.cookie, a.id) {
                bg.send(Msg::Presence(a.id, p));
            }
        }
    });
}

/// Loads the Home page rows, each on its own thread so a slow row doesn't hold up the others.
/// `recent` is the local history as (root place, universe if known), newest first.
pub fn load_home(bg: &Bg, a: Acct, recent: Vec<(u64, Option<u64>)>) {
    let err = |e: ApiError| e.to_string();
    let account = a.id;

    let acct = a.clone();
    bg.spawn(move |bg| {
        let mut universes = Vec::new();
        for (place, universe) in recent {
            match universe {
                Some(u) => universes.push(u),
                None => universes.extend(roblox::universe_of(&acct.cookie, place).ok()),
            }
        }
        let result = roblox::game_details(&acct.cookie, &universes).map(|mut games| {
            // Roblox's own list adds games played outside this app.
            for g in roblox::continue_list(&acct.cookie).unwrap_or_default() {
                if !games.iter().any(|x| x.universe == g.universe) {
                    games.push(g);
                }
            }
            roblox::enrich(&acct.cookie, &mut games);
            games
        });
        bg.send(Msg::Home { account, part: HomePart::Continue(result.map_err(err)) });
    });

    let acct = a.clone();
    bg.spawn(move |bg| {
        let result = roblox::favorites(&acct.cookie, acct.id).map(|mut games| {
            roblox::enrich(&acct.cookie, &mut games);
            games
        });
        bg.send(Msg::Home { account, part: HomePart::Favorites(result.map_err(err)) });
    });

    let acct = a.clone();
    bg.spawn(move |bg| {
        let result = roblox::friends_playing(&acct.cookie, acct.id);
        bg.send(Msg::Home { account, part: HomePart::Friends(result.map_err(err)) });
    });

    bg.spawn(move |bg| {
        let result = roblox::charts(&a.cookie).map(|mut rows| {
            for (_, games) in rows.iter_mut() {
                roblox::enrich(&a.cookie, games);
            }
            rows
        });
        bg.send(Msg::Home { account, part: HomePart::Charts(result.map_err(err)) });
    });
}

pub fn search_games(bg: &Bg, a: Acct, query: String) {
    bg.spawn(move |bg| {
        let result = roblox::search(&a.cookie, &query).map(|mut games| {
            roblox::enrich(&a.cookie, &mut games);
            games
        });
        bg.send(Msg::Home { account: a.id, part: HomePart::Search(result.map_err(|e| e.to_string())) });
    });
}

/// Looks up the game and server location of a join. Neither needs an account.
pub fn join_info(bg: &Bg, live: Live, fresh: bool, known_game: bool) {
    bg.spawn(move |bg| {
        let game = if known_game { None } else { roblox::game_brief(live.place, live.universe).ok() };
        let location = live.ip.as_deref().and_then(|ip| roblox::locate(ip).ok());
        bg.send(Msg::JoinInfo { live, fresh, game, location });
    });
}

/// How often friend alerts look at who is playing.
const FRIENDS_EVERY: Duration = Duration::from_secs(60);
/// How long a friend list is reused before it's fetched again.
const FRIEND_LIST_FOR: Duration = Duration::from_secs(600);

/// Starts friend alerts: once a minute, checks which friends of the saved accounts are in a game
/// and reports the ones who weren't the time before.
pub fn start_friend_alerts(bg: &Bg) {
    let bg = bg.clone();
    thread::spawn(move || {
        let mut lists: HashMap<u64, (Instant, Vec<u64>)> = HashMap::new();
        // Friend -> game, from the last complete look. `None` until the first one.
        let mut before: Option<HashMap<u64, String>> = None;
        loop {
            thread::sleep(FRIENDS_EVERY);
            if !bg.settings().friend_alerts {
                before = None;
                continue;
            }
            let accounts = bg.watch().accounts.clone();
            let own: Vec<u64> = accounts.iter().map(|a| a.id).collect();
            let mut playing = HashMap::new();
            let mut complete = true;
            for a in &accounts {
                let stale = lists.get(&a.id).is_none_or(|(at, _)| at.elapsed() > FRIEND_LIST_FOR);
                if stale {
                    match roblox::friend_ids(&a.cookie, a.id) {
                        Ok(ids) => {
                            lists.insert(a.id, (Instant::now(), ids));
                        }
                        Err(_) => complete = false,
                    }
                }
                let Some((_, ids)) = lists.get(&a.id) else { continue };
                // Presence is looked up as the friend's own friend, so their game is visible.
                match roblox::presence_many(&a.cookie, ids) {
                    Ok(list) => {
                        for (id, p) in list.into_iter().filter(|(id, p)| p.kind == 2 && !own.contains(id)) {
                            let game = if p.location.is_empty() { "a game".to_string() } else { p.location };
                            playing.insert(id, game);
                        }
                    }
                    Err(_) => complete = false,
                }
            }
            // A failed lookup would look like friends leaving and joining again, so skip the round.
            if !complete {
                continue;
            }
            if let (Some(prev), Some(a)) = (&before, accounts.first()) {
                let new: Vec<(u64, String)> =
                    playing.iter().filter(|(id, _)| !prev.contains_key(id)).map(|(id, g)| (*id, g.clone())).collect();
                if !new.is_empty() {
                    let ids: Vec<u64> = new.iter().map(|(id, _)| *id).collect();
                    let names = roblox::user_names(&a.cookie, &ids);
                    let list = new
                        .into_iter()
                        .map(|(id, game)| {
                            let name = names
                                .get(&id)
                                .map(|(n, d)| if d.is_empty() { n.clone() } else { d.clone() })
                                .unwrap_or_else(|| "A friend".into());
                            (name, game)
                        })
                        .collect();
                    bg.send(Msg::FriendsPlaying(list));
                }
            }
            before = Some(playing);
        }
    });
}

pub fn load_avatars(bg: &Bg, a: Acct, ids: Vec<u64>) {
    bg.spawn(move |bg| {
        let found = roblox::headshots(&a.cookie, &ids);
        if !found.is_empty() {
            bg.send(Msg::Avatars(found));
        }
    });
}

/// What one tick does to one client, worked out under the lock and carried out after it.
struct Upkeep {
    id: u64,
    pid: u32,
    title: String,
    priority: Option<bool>,
    mute: Option<bool>,
    minimize: bool,
}

fn tick(bg: &Bg) {
    let alive = multi::roblox_pids();
    let mut closed = Vec::new();
    let mut failed_cpu = Vec::new();
    let mut work = Vec::new();
    let mut due = Vec::new();
    {
        let mut w = bg.watch();
        let s = w.settings.clone();
        w.clients.retain(|_, c| {
            let keep = c.rejoining || alive.contains(&c.pid);
            if !keep {
                closed.push(c.acct.name.clone());
            }
            keep
        });
        for (&id, c) in w.clients.iter_mut() {
            if c.rejoining {
                continue;
            }
            let afk = s.afk_applies(id);
            let low = s.low_priority && afk;
            let priority = (c.low_priority != Some(low)).then(|| {
                c.low_priority = Some(low);
                low
            });
            // Keep muting so sound that starts later is caught; only ever unmute what we muted.
            let mute = if s.mute && afk {
                c.muted = true;
                Some(true)
            } else if c.muted {
                c.muted = false;
                Some(false)
            } else {
                None
            };
            let cpu = s.cpu_limit.filter(|_| afk);
            if c.cpu_applied != Some(cpu) && (cpu.is_some() || c.cpu_job.is_some()) {
                c.cpu_applied = Some(cpu);
                if c.cpu_job.is_none() {
                    c.cpu_job = multi::CpuLimit::attach(c.pid);
                }
                let ok = c.cpu_job.as_ref().is_some_and(|j| j.set(cpu));
                if !ok {
                    failed_cpu.push(c.acct.name.clone());
                }
            }
            let minimize = s.minimize && afk && !c.minimized && c.launched.elapsed() >= MINIMIZE_AFTER;
            let title = format!("Roblox - {}", c.acct.name);
            work.push(Upkeep { id, pid: c.pid, title, priority, mute, minimize });
            if s.auto_rejoin && c.last_check.elapsed() >= CHECK_EVERY {
                c.last_check = Instant::now();
                due.push(c.acct.clone());
            }
        }
    }
    for name in closed {
        bg.log(format!("{name}'s client closed."));
    }
    for name in failed_cpu {
        bg.log(format!("{name}: could not change the CPU limit on its client."));
    }

    // Window, process and audio calls happen outside the lock so a busy client can't stall the UI.
    let mut mutes = Vec::new();
    let mut minimized = Vec::new();
    for u in work {
        multi::set_client_title(u.pid, &u.title);
        if let Some(low) = u.priority {
            multi::set_low_priority(u.pid, low);
        }
        if let Some(m) = u.mute {
            mutes.push((u.pid, m));
        }
        if u.minimize && multi::minimize_client(u.pid) > 0 {
            minimized.push(u.id);
        }
    }
    if !mutes.is_empty() {
        audio::set_mute(&mutes);
    }
    if !minimized.is_empty() {
        let mut w = bg.watch();
        for id in minimized {
            if let Some(c) = w.clients.get_mut(&id) {
                c.minimized = true;
            }
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
    if !w.settings.auto_rejoin {
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
