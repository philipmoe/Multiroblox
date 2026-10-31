//! Roblox keeps graphics quality and the frame-rate cap in one settings file that every client reads
//! when it starts. AFK mode writes low values into it just before an AFK client launches, and puts
//! the user's own values back before a launch that should look normal.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The fields AFK mode touches, as (XML type, property name).
const FIELDS: [(&str, &str); 3] = [
    ("token", "SavedQualityLevel"),
    ("int", "GraphicsQualityLevel"),
    ("int", "FramerateCap"),
];

type Values = [Option<String>; 3];

pub struct Profile {
    pub low_graphics: bool,
    pub fps_cap: Option<u32>,
}

impl Profile {
    pub fn any(&self) -> bool {
        self.low_graphics || self.fps_cap.is_some()
    }
}

/// Remembered between runs: the user's own values, and the last values written for AFK clients.
#[derive(Serialize, Deserialize, Default)]
struct Backup {
    normal: Option<Values>,
    last_low: Option<Values>,
}

fn backup_path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("MultiRoblox").join("graphics.json")
}

fn load_backup() -> Backup {
    std::fs::read_to_string(backup_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_backup(b: &Backup) {
    if let Ok(json) = serde_json::to_string_pretty(b) {
        let _ = std::fs::write(backup_path(), json);
    }
}

/// The newest GlobalBasicSettings_<n>.xml in %LOCALAPPDATA%\Roblox (not the Studio one).
fn settings_path() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("LOCALAPPDATA")?).join("Roblox");
    std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let n: u32 = name.strip_prefix("GlobalBasicSettings_")?.strip_suffix(".xml")?.parse().ok()?;
            Some((n, name))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, name)| dir.join(name))
}

fn read_field(xml: &str, kind: &str, name: &str) -> Option<String> {
    let open = format!("<{kind} name=\"{name}\">");
    let start = xml.find(&open)? + open.len();
    let end = start + xml[start..].find(&format!("</{kind}>"))?;
    Some(xml[start..end].to_string())
}

fn write_field(xml: &str, kind: &str, name: &str, value: &str) -> Option<String> {
    let open = format!("<{kind} name=\"{name}\">");
    if let Some(at) = xml.find(&open) {
        let start = at + open.len();
        let end = start + xml[start..].find(&format!("</{kind}>"))?;
        return Some(format!("{}{value}{}", &xml[..start], &xml[end..]));
    }
    let at = xml.find("</Properties>")?;
    Some(format!("{}\t{open}{value}</{kind}>\n\t\t{}", &xml[..at], &xml[at..]))
}

fn read_all(xml: &str) -> Values {
    FIELDS.map(|(kind, name)| read_field(xml, kind, name))
}

/// Gets the settings file ready for the next client: AFK values when `afk` is given, otherwise
/// the user's own. Only call this while holding the launch lock.
pub fn prepare(afk: Option<&Profile>) {
    let Some(path) = settings_path() else { return };
    let Ok(xml) = std::fs::read_to_string(&path) else { return };
    let mut backup = load_backup();
    if afk.is_none() && backup.normal.is_none() {
        return;
    }

    let current = read_all(&xml);
    // Whatever is in the file is the user's own setup unless it is exactly what we last wrote for
    // AFK clients. Adopting it means changes made in game on the main account stick.
    if backup.last_low.as_ref() != Some(&current) {
        backup.normal = Some(current.clone());
    }
    let normal = backup.normal.clone().unwrap_or_else(|| current.clone());

    let desired = match afk {
        Some(p) => {
            let mut v = normal;
            if p.low_graphics {
                v[0] = Some("1".into());
                v[1] = Some("1".into());
            }
            if let Some(cap) = p.fps_cap {
                v[2] = Some(cap.to_string());
            }
            backup.last_low = Some(v.clone());
            v
        }
        None => normal,
    };
    save_backup(&backup);
    if desired == current {
        return;
    }

    let mut out = xml;
    for ((kind, name), value) in FIELDS.iter().zip(&desired) {
        if let Some(value) = value {
            match write_field(&out, kind, name, value) {
                Some(x) => out = x,
                None => return,
            }
        }
    }
    let _ = std::fs::write(&path, out);
}
