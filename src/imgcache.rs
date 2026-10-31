//! Keeps game icons and avatars on disk so Home shows them instantly on the next start. An image
//! is shown from the web the first time, while a background thread saves a copy.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::time::Duration;

pub struct ImageCache {
    dir: PathBuf,
    /// Web URL -> the URI to show it from (a local file once saved).
    resolved: HashMap<String, String>,
    queued: HashSet<String>,
    downloads: Sender<(String, PathBuf)>,
}

fn dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("MultiRoblox").join("images")
}

/// FNV-1a, enough to give each URL its own file name.
fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
}

fn download(url: &str, path: &PathBuf) {
    let agent = ureq::AgentBuilder::new().timeout(Duration::from_secs(20)).build();
    let Ok(resp) = agent.get(url).call() else { return };
    let mut bytes = Vec::new();
    if std::io::Read::read_to_end(&mut resp.into_reader(), &mut bytes).is_err() || bytes.is_empty() {
        return;
    }
    // Write under a temporary name first so a half-written file is never shown.
    let tmp = path.with_extension("part");
    if std::fs::write(&tmp, &bytes).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

impl ImageCache {
    pub fn new() -> ImageCache {
        let dir = dir();
        let _ = std::fs::create_dir_all(&dir);
        let (tx, rx) = channel::<(String, PathBuf)>();
        thread::spawn(move || {
            for (url, path) in rx {
                download(&url, &path);
            }
        });
        ImageCache { dir, resolved: HashMap::new(), queued: HashSet::new(), downloads: tx }
    }

    /// The URI to show `url` from: the saved copy when there is one, otherwise the web URL (and a
    /// copy is saved for next time).
    pub fn uri(&mut self, url: &str) -> String {
        if let Some(u) = self.resolved.get(url) {
            return u.clone();
        }
        let path = self.dir.join(format!("{:016x}.png", hash(url)));
        let uri = if path.exists() {
            format!("file://{}", path.display().to_string().replace('\\', "/"))
        } else {
            if self.queued.insert(url.to_string()) {
                let _ = self.downloads.send((url.to_string(), path));
            }
            url.to_string()
        };
        self.resolved.insert(url.to_string(), uri.clone());
        uri
    }
}
