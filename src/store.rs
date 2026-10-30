//! Saved accounts and settings. Cookies are encrypted with Windows DPAPI, so the file is only
//! readable by this Windows user on this machine.

use serde::{Deserialize, Serialize};
use std::ffi::c_void;
use std::path::PathBuf;
use std::ptr::{null, null_mut};

#[repr(C)]
struct DataBlob {
    cb: u32,
    pb: *mut u8,
}

#[link(name = "crypt32")]
extern "system" {
    fn CryptProtectData(
        input: *const DataBlob,
        desc: *const u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        out: *mut DataBlob,
    ) -> i32;
    fn CryptUnprotectData(
        input: *const DataBlob,
        desc: *mut *mut u16,
        entropy: *const DataBlob,
        reserved: *mut c_void,
        prompt: *mut c_void,
        flags: u32,
        out: *mut DataBlob,
    ) -> i32;
}

#[link(name = "kernel32")]
extern "system" {
    fn LocalFree(mem: *mut c_void) -> *mut c_void;
}

const UI_FORBIDDEN: u32 = 0x1;
const ENTROPY: &[u8] = b"MultiRoblox.rs";

fn blob(data: &[u8]) -> DataBlob {
    DataBlob { cb: data.len() as u32, pb: data.as_ptr() as *mut u8 }
}

fn take(out: DataBlob) -> Vec<u8> {
    let v = unsafe { std::slice::from_raw_parts(out.pb, out.cb as usize).to_vec() };
    unsafe { LocalFree(out.pb as *mut c_void) };
    v
}

fn to_hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(s.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect()
}

pub fn encrypt(plain: &str) -> Option<String> {
    let (input, entropy) = (blob(plain.as_bytes()), blob(ENTROPY));
    let mut out = DataBlob { cb: 0, pb: null_mut() };
    let ok = unsafe {
        CryptProtectData(&input, null(), &entropy, null_mut(), null_mut(), UI_FORBIDDEN, &mut out)
    };
    (ok != 0).then(|| to_hex(&take(out)))
}

pub fn decrypt(hex: &str) -> Option<String> {
    let raw = from_hex(hex)?;
    let (input, entropy) = (blob(&raw), blob(ENTROPY));
    let mut out = DataBlob { cb: 0, pb: null_mut() };
    let ok = unsafe {
        CryptUnprotectData(&input, null_mut(), &entropy, null_mut(), null_mut(), UI_FORBIDDEN, &mut out)
    };
    if ok == 0 {
        return None;
    }
    String::from_utf8(take(out)).ok()
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Account {
    pub user_id: u64,
    pub username: String,
    pub display_name: String,
    /// DPAPI-encrypted .ROBLOSECURITY cookie, hex encoded.
    pub cookie: String,
}

impl Account {
    pub fn cookie_plain(&self) -> Option<String> {
        decrypt(&self.cookie)
    }

    pub fn title(&self) -> &str {
        if self.display_name.is_empty() {
            &self.username
        } else {
            &self.display_name
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub accounts: Vec<Account>,
    pub main: Option<u64>,
    pub selected: Vec<u64>,
    pub place_id: String,
    pub job_id: String,
    pub delay_secs: u32,
    pub auto_rejoin: bool,
    pub low_priority_alts: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            accounts: Vec::new(),
            main: None,
            selected: Vec::new(),
            place_id: String::new(),
            job_id: String::new(),
            delay_secs: 8,
            auto_rejoin: false,
            low_priority_alts: false,
        }
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("MultiRoblox").join("config.json")
}

pub fn load() -> Config {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(cfg: &Config) {
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = p.with_extension("tmp");
    if let Ok(json) = serde_json::to_string_pretty(cfg) {
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &p);
        }
    }
}
