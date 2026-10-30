//! Background work. Everything that touches the network or waits runs here so the UI never blocks;
//! results come back to the UI as `Msg` values.

use crate::roblox::{self, ApiError, Presence, Target};
use crate::store::{self, Account};
use eframe::egui;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub enum Msg {
    Log(String),
    Added(Account),
    AddFailed,
    Presence(u64, Presence),
    Expired(u64),
}

#[derive(Clone)]
pub struct Bg {
    pub tx: Sender<Msg>,
    pub ctx: egui::Context,
    pub jobs: Arc<AtomicUsize>,
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
}

/// An account with its cookie already decrypted, ready to be moved onto a worker thread.
#[derive(Clone)]
pub struct Acct {
    pub id: u64,
    pub name: String,
    pub cookie: String,
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

fn launch_one(bg: &Bg, a: &Acct, target: &Target) -> bool {
    match roblox::launch(&a.cookie, target) {
        Ok(()) => {
            bg.log(format!("Launching {}...", a.name));
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
    for (i, a) in list.iter().enumerate() {
        if i > 0 {
            sleep_secs(delay);
        }
        launch_one(bg, a, target);
    }
}

pub fn launch_many(bg: &Bg, list: Vec<Acct>, target: Target, delay: u32) {
    bg.spawn(move |bg| {
        let _g = lock_launches();
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
        if let Some(t) = &initial {
            if !launch_one(bg, &main, t) {
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
