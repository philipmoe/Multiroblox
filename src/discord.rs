//! Shows what you're playing on your Discord profile, through the Discord desktop app's local IPC
//! pipe. Discord needs an application ID (made at discord.com/developers); the application's name
//! is the title Discord shows, e.g. "Playing Roblox".

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

#[derive(Clone, PartialEq)]
pub struct Activity {
    pub details: String,
    pub state: String,
    /// Unix seconds; Discord shows the time elapsed since.
    pub start: u64,
    pub image: Option<String>,
    pub image_text: String,
    pub join_url: Option<String>,
}

/// What the Discord thread reports back for the Settings page.
pub enum Status {
    Off,
    Waiting,
    Connected,
    Error(String),
}

fn frame(op: u32, json: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + json.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(json.len() as u32).to_le_bytes());
    out.extend_from_slice(json.as_bytes());
    out
}

/// Reads one reply. Opcode 1 is a normal reply; 2 means Discord closed the connection.
fn read_frame(pipe: &mut File) -> std::io::Result<(u32, serde_json::Value)> {
    let mut head = [0u8; 8];
    pipe.read_exact(&mut head)?;
    let op = u32::from_le_bytes([head[0], head[1], head[2], head[3]]);
    let len = u32::from_le_bytes([head[4], head[5], head[6], head[7]]) as usize;
    let mut body = vec![0u8; len.min(1 << 20)];
    pipe.read_exact(&mut body)?;
    Ok((op, serde_json::from_slice(&body).unwrap_or_default()))
}

/// Connects and says hello with the application ID. Discord answers READY, or an error.
fn connect(app_id: &str) -> Result<File, String> {
    let mut last = "Discord isn't running".to_string();
    for i in 0..10 {
        let Ok(mut pipe) = OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{i}")) else {
            continue;
        };
        let hello = serde_json::json!({ "v": 1, "client_id": app_id }).to_string();
        if pipe.write_all(&frame(0, &hello)).is_err() {
            continue;
        }
        match read_frame(&mut pipe) {
            Ok((1, v)) if v["evt"] == "READY" => return Ok(pipe),
            Ok((_, v)) => {
                last = v["message"].as_str().unwrap_or("Discord refused the application ID").to_string();
            }
            Err(e) => last = e.to_string(),
        }
    }
    Err(last)
}

fn set_activity(pipe: &mut File, a: Option<&Activity>, nonce: u64) -> Result<(), String> {
    let activity = a.map(|a| {
        let mut v = serde_json::json!({
            "details": a.details.chars().take(128).collect::<String>(),
            "state": a.state.chars().take(128).collect::<String>(),
            "timestamps": { "start": a.start },
            "type": 0,
        });
        if let Some(img) = &a.image {
            v["assets"] = serde_json::json!({ "large_image": img, "large_text": a.image_text });
        }
        if let Some(url) = &a.join_url {
            v["buttons"] = serde_json::json!([{ "label": "Join server", "url": url }]);
        }
        v
    });
    let cmd = serde_json::json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": std::process::id(), "activity": activity },
        "nonce": nonce.to_string(),
    });
    pipe.write_all(&frame(1, &cmd.to_string())).map_err(|e| e.to_string())?;
    match read_frame(pipe).map_err(|e| e.to_string())? {
        (1, v) if v["evt"] == "ERROR" => Err(v["data"]["message"].as_str().unwrap_or("Discord said no").to_string()),
        (1, _) => Ok(()),
        _ => Err("Discord closed the connection".into()),
    }
}

/// Keeps one connection to Discord and shows whatever activity it was last given.
fn run(rx: Receiver<Option<(String, Activity)>>, status: Sender<Status>) {
    let mut wanted: Option<(String, Activity)> = None;
    let mut pipe: Option<(String, File)> = None;
    let mut shown = false;
    let mut nonce = 0u64;
    loop {
        // Re-check every 15 s, so Discord being started later is picked up.
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(w) => {
                wanted = w;
                shown = false;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        if shown {
            continue;
        }
        let Some((app_id, activity)) = &wanted else {
            if let Some((_, p)) = pipe.as_mut() {
                nonce += 1;
                let _ = set_activity(p, None, nonce);
            }
            pipe = None;
            shown = true;
            let _ = status.send(Status::Off);
            continue;
        };
        if pipe.as_ref().is_none_or(|(id, _)| id != app_id) {
            match connect(app_id) {
                Ok(p) => pipe = Some((app_id.clone(), p)),
                Err(e) => {
                    pipe = None;
                    let _ = status.send(if e.contains("isn't running") { Status::Waiting } else { Status::Error(e) });
                    continue;
                }
            }
        }
        if let Some((_, p)) = pipe.as_mut() {
            nonce += 1;
            match set_activity(p, Some(activity), nonce) {
                Ok(()) => {
                    shown = true;
                    let _ = status.send(Status::Connected);
                }
                Err(e) => {
                    pipe = None;
                    let _ = status.send(Status::Error(e));
                }
            }
        }
    }
}

pub struct Discord {
    tx: Sender<Option<(String, Activity)>>,
    pub status: Receiver<Status>,
    last: Option<(String, Activity)>,
}

impl Discord {
    pub fn start() -> Discord {
        let (tx, rx) = channel();
        let (status_tx, status) = channel();
        thread::spawn(move || run(rx, status_tx));
        Discord { tx, status, last: None }
    }

    /// Shows `activity` with application `app_id`, or clears it with `None`. Repeats are ignored.
    pub fn set(&mut self, wanted: Option<(String, Activity)>) {
        if wanted != self.last {
            self.last = wanted.clone();
            let _ = self.tx.send(wanted);
        }
    }
}
