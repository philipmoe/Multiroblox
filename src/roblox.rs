//! Talks to Roblox: validates a cookie, fetches presence and starts the client for an account.
//!
//! Launching works the way the website does: trade the account's session cookie for a
//! one-time authentication ticket, then hand that ticket to the `roblox-player:` protocol.

use crate::multi;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36";
const REFERER: &str = "https://www.roblox.com/games/4924922222/Brookhaven-RP";

pub enum ApiError {
    /// The cookie was rejected: the user logged out or the session expired.
    Expired,
    Other(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ApiError::Expired => write!(f, "session expired - re-add this account"),
            ApiError::Other(s) => write!(f, "{s}"),
        }
    }
}

type Res<T> = Result<T, ApiError>;

fn other<T: fmt::Display>(e: T) -> ApiError {
    ApiError::Other(e.to_string())
}

pub struct UserInfo {
    pub id: u64,
    pub name: String,
    pub display: String,
}

#[derive(Clone)]
pub struct Presence {
    /// 0 offline, 1 online, 2 in game, 3 in studio
    pub kind: u8,
    pub location: String,
    pub place_id: Option<u64>,
    pub game_id: Option<String>,
}

impl Presence {
    pub fn label(&self) -> String {
        match self.kind {
            0 => "Offline".into(),
            1 => "Online".into(),
            2 if self.location.is_empty() => "In game".into(),
            2 => format!("In game: {}", self.location),
            3 => "In Studio".into(),
            _ => "Unknown".into(),
        }
    }
}

#[derive(Clone)]
pub enum Target {
    Place { place: u64, job: Option<String> },
    /// A private server, from the `privateServerLinkCode` of its link.
    Private { place: u64, link_code: String },
    /// A roblox.com/share link; it is turned into `Private` with `resolve_share_link` before launching.
    ShareLink { code: String },
    Follow { user_id: u64 },
}

impl Target {
    /// The same game without pinning a server, for when the original server is gone.
    pub fn any_server(self) -> Target {
        match self {
            Target::Place { place, .. } => Target::Place { place, job: None },
            t => t,
        }
    }
}

pub struct Server {
    pub id: String,
    pub playing: u32,
    pub max: u32,
    pub ping: Option<u32>,
}

fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(20))
            .user_agent(UA)
            .build()
    })
}

/// Accepts the cookie as copied from the browser, with or without the `.ROBLOSECURITY=` prefix.
pub fn clean_cookie(raw: &str) -> String {
    let s = raw.trim().trim_matches(|c| c == '"' || c == '\'' || c == ';');
    let s = s.strip_prefix(".ROBLOSECURITY=").unwrap_or(s);
    s.trim_matches(|c| c == '"' || c == '\'' || c == ';').trim().to_string()
}

/// Both successful and HTTP-error responses come back as a Response so headers stay readable.
fn finish(r: Result<ureq::Response, ureq::Error>) -> Res<ureq::Response> {
    match r {
        Ok(r) => Ok(r),
        Err(ureq::Error::Status(_, r)) => Ok(r),
        Err(e) => Err(other(format!("network error: {e}"))),
    }
}

fn cookie_header(cookie: &str) -> String {
    format!(".ROBLOSECURITY={cookie}")
}

/// POST with the CSRF dance: the first attempt is rejected with a token that the retry carries.
fn csrf_post(cookie: &str, url: &str, json: Option<&str>) -> Res<ureq::Response> {
    let send = |token: Option<&str>| {
        let mut req = agent()
            .post(url)
            .set("Cookie", &cookie_header(cookie))
            .set("Referer", REFERER)
            .set("Origin", "https://www.roblox.com");
        if let Some(t) = token {
            req = req.set("X-CSRF-TOKEN", t);
        }
        req.set("Content-Type", "application/json").send_string(json.unwrap_or("{}"))
    };
    let first = finish(send(None))?;
    if first.status() == 403 {
        if let Some(token) = first.header("x-csrf-token") {
            let token = token.to_string();
            return finish(send(Some(&token)));
        }
    }
    Ok(first)
}

pub fn authenticated_user(cookie: &str) -> Res<UserInfo> {
    let r = finish(
        agent()
            .get("https://users.roblox.com/v1/users/authenticated")
            .set("Cookie", &cookie_header(cookie))
            .call(),
    )?;
    match r.status() {
        200 => {
            let v: serde_json::Value = serde_json::from_str(&r.into_string().map_err(other)?).map_err(other)?;
            Ok(UserInfo {
                id: v["id"].as_u64().ok_or_else(|| other("unexpected response"))?,
                name: v["name"].as_str().unwrap_or_default().to_string(),
                display: v["displayName"].as_str().unwrap_or_default().to_string(),
            })
        }
        401 | 403 => Err(ApiError::Expired),
        s => Err(other(format!("Roblox returned HTTP {s}"))),
    }
}

pub fn presence(cookie: &str, user_id: u64) -> Res<Presence> {
    let body = format!("{{\"userIds\":[{user_id}]}}");
    let r = csrf_post(cookie, "https://presence.roblox.com/v1/presence/users", Some(&body))?;
    match r.status() {
        200 => {
            let v: serde_json::Value = serde_json::from_str(&r.into_string().map_err(other)?).map_err(other)?;
            let p = &v["userPresences"][0];
            Ok(Presence {
                kind: p["userPresenceType"].as_u64().unwrap_or(0) as u8,
                location: p["lastLocation"].as_str().unwrap_or_default().to_string(),
                place_id: p["placeId"].as_u64(),
                game_id: p["gameId"].as_str().map(str::to_string),
            })
        }
        401 => Err(ApiError::Expired),
        s => Err(other(format!("presence lookup failed (HTTP {s})"))),
    }
}

fn auth_ticket(cookie: &str) -> Res<String> {
    let r = csrf_post(cookie, "https://auth.roblox.com/v1/authentication-ticket", None)?;
    if let Some(t) = r.header("rbx-authentication-ticket") {
        return Ok(t.to_string());
    }
    match r.status() {
        401 | 403 => Err(ApiError::Expired),
        s => Err(other(format!("no authentication ticket (HTTP {s})"))),
    }
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02x}")),
        }
    }
    out
}

fn tracker_id() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let mut x = nanos ^ (std::process::id() as u64) << 32 ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    format!("{}{}", 100_000 + x % 75_000, 100_000 + (x >> 24) % 800_000)
}

/// Looks up the private server behind a roblox.com/share?code=...&type=Server link.
pub fn resolve_share_link(cookie: &str, code: &str) -> Res<Target> {
    let body = serde_json::json!({ "linkId": code, "linkType": "Server" }).to_string();
    let r = csrf_post(cookie, "https://apis.roblox.com/sharelinks/v1/resolve-link", Some(&body))?;
    match r.status() {
        200 => {
            let v: serde_json::Value = serde_json::from_str(&r.into_string().map_err(other)?).map_err(other)?;
            let d = &v["privateServerInviteData"];
            let valid = d["status"].as_str().map_or(true, |s| s.eq_ignore_ascii_case("Valid"));
            match (d["placeId"].as_u64(), d["linkCode"].as_str()) {
                (Some(place), Some(code)) if valid => Ok(Target::Private { place, link_code: code.to_string() }),
                _ => Err(other("the private server link is invalid or has expired")),
            }
        }
        401 => Err(ApiError::Expired),
        s => Err(other(format!("could not open the private server link (HTTP {s})"))),
    }
}

/// Lists open public servers of a place, emptiest or fullest first.
pub fn servers(cookie: &str, place: u64, fewest_first: bool) -> Res<Vec<Server>> {
    let order = if fewest_first { "Asc" } else { "Desc" };
    let url = format!(
        "https://games.roblox.com/v1/games/{place}/servers/Public?sortOrder={order}&excludeFullGames=true&limit=100"
    );
    let r = finish(agent().get(&url).set("Cookie", &cookie_header(cookie)).call())?;
    match r.status() {
        200 => {
            let v: serde_json::Value = serde_json::from_str(&r.into_string().map_err(other)?).map_err(other)?;
            let list = v["data"].as_array().map(Vec::as_slice).unwrap_or_default();
            Ok(list
                .iter()
                .filter_map(|s| {
                    Some(Server {
                        id: s["id"].as_str()?.to_string(),
                        playing: s["playing"].as_u64().unwrap_or(0) as u32,
                        max: s["maxPlayers"].as_u64().unwrap_or(0) as u32,
                        ping: s["ping"].as_u64().map(|p| p as u32),
                    })
                })
                .collect())
        }
        401 => Err(ApiError::Expired),
        429 => Err(other("Roblox is rate limiting server lists - try again in a few seconds")),
        s => Err(other(format!("server list failed (HTTP {s})"))),
    }
}

pub fn launch(cookie: &str, target: &Target) -> Res<()> {
    let tracker = tracker_id();
    let launcher = match target {
        Target::Place { place, job: Some(job) } => format!(
            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGameJob&browserTrackerId={tracker}&placeId={place}&gameId={job}&isPlayTogetherGame=false"
        ),
        Target::Place { place, job: None } => format!(
            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestGame&browserTrackerId={tracker}&placeId={place}&isPlayTogetherGame=false"
        ),
        Target::Private { place, link_code } => format!(
            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&browserTrackerId={tracker}&placeId={place}&linkCode={}&isPlayTogetherGame=false",
            url_encode(link_code)
        ),
        Target::ShareLink { .. } => return Err(other("the share link has to be resolved before launching")),
        Target::Follow { user_id } => format!(
            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestFollowUser&userId={user_id}"
        ),
    };
    let ticket = auth_ticket(cookie)?;
    let now_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let uri = format!(
        "roblox-player:1+launchmode:play+gameinfo:{ticket}+launchtime:{now_ms}+placelauncherurl:{}+browsertrackerid:{tracker}+robloxLocale:en_us+gameLocale:en_us+channel:+LaunchExp:InApp",
        url_encode(&launcher)
    );
    if multi::shell_open(&uri) {
        Ok(())
    } else {
        Err(other("could not start Roblox - is it installed?"))
    }
}

/// Reads what to join from the game field: a place id, a game link (with or without a private
/// server link code) or a private server share link. `job` picks a public server.
pub fn parse_target(input: &str, job: &str) -> Option<Target> {
    let s = input.trim();
    if let Some(link_code) = query_param(s, "privateServerLinkCode") {
        return Some(Target::Private { place: parse_place(s)?, link_code });
    }
    let server_share = query_param(s, "type").map_or(true, |t| t.eq_ignore_ascii_case("Server"));
    if s.contains("/share") && server_share {
        if let Some(code) = query_param(s, "code") {
            return Some(Target::ShareLink { code });
        }
    }
    let job = job.trim();
    Some(Target::Place { place: parse_place(s)?, job: (!job.is_empty()).then(|| job.to_string()) })
}

fn query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split_once('?')?.1.split('#').next()?;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key && !v.is_empty()).then(|| v.to_string())
    })
}

/// Reads a place id from plain digits or a roblox.com/games/<id>/... link.
fn parse_place(input: &str) -> Option<u64> {
    let s = input.trim();
    if let Ok(n) = s.parse() {
        return Some(n);
    }
    let rest = &s[s.find("/games/")? + 7..];
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}
