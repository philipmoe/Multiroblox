//! Talks to Roblox: validates a cookie, fetches presence and starts the client for an account.
//!
//! Launching works the way the website does: trade the account's session cookie for a
//! one-time authentication ticket, then hand that ticket to the `roblox-player:` protocol.

use crate::multi;
use crate::store::ServerPick;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
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
    pub root_place_id: Option<u64>,
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
    /// A private server the account owns or was invited to, by its access code.
    PrivateAccess { place: u64, access_code: String },
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

fn json_of(r: ureq::Response) -> Res<Value> {
    match r.status() {
        200 => serde_json::from_str(&r.into_string().map_err(other)?).map_err(other),
        401 => Err(ApiError::Expired),
        429 => Err(other("Roblox is rate limiting requests - try again in a moment")),
        s => Err(other(format!("Roblox returned HTTP {s}"))),
    }
}

/// GET as the account that owns `cookie`, or signed out when it is empty.
fn get_json(cookie: &str, url: &str) -> Res<Value> {
    let mut req = agent().get(url);
    if !cookie.is_empty() {
        req = req.set("Cookie", &cookie_header(cookie));
    }
    json_of(finish(req.call())?)
}

/// Where a game server is, like "London, GB", from its public address.
pub fn locate(ip: &str) -> Res<String> {
    let v = json_of(finish(agent().get(&format!("https://ipinfo.io/{ip}/json")).call())?)?;
    let part = |k: &str| v[k].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    match (part("city"), part("country")) {
        (Some(city), Some(country)) => Ok(format!("{city}, {country}")),
        (None, Some(country)) => Ok(country),
        _ => Err(other("unknown location")),
    }
}

/// Name and icon of a game, looked up signed out.
pub fn game_brief(place: u64, universe: Option<u64>) -> Res<Game> {
    let universe = match universe {
        Some(u) => u,
        None => universe_of("", place)?,
    };
    let mut games = game_details("", &[universe])?;
    add_icons("", &mut games);
    games.pop().ok_or_else(|| other("unknown game"))
}

fn post_json(cookie: &str, url: &str, body: &Value) -> Res<Value> {
    json_of(csrf_post(cookie, url, Some(&body.to_string()))?)
}

/// Presence of several users, looked up as the account that owns `cookie`.
pub fn presence_many(cookie: &str, ids: &[u64]) -> Res<Vec<(u64, Presence)>> {
    let mut out = Vec::new();
    for chunk in ids.chunks(50) {
        let body = serde_json::json!({ "userIds": chunk });
        let v = post_json(cookie, "https://presence.roblox.com/v1/presence/users", &body)?;
        for p in v["userPresences"].as_array().into_iter().flatten() {
            let presence = Presence {
                kind: p["userPresenceType"].as_u64().unwrap_or(0) as u8,
                location: p["lastLocation"].as_str().unwrap_or_default().to_string(),
                place_id: p["placeId"].as_u64(),
                root_place_id: p["rootPlaceId"].as_u64(),
                game_id: p["gameId"].as_str().map(str::to_string),
            };
            out.push((p["userId"].as_u64().unwrap_or(0), presence));
        }
    }
    Ok(out)
}

pub fn presence(cookie: &str, user_id: u64) -> Res<Presence> {
    presence_many(cookie, &[user_id])?
        .into_iter()
        .next()
        .map(|(_, p)| p)
        .ok_or_else(|| other("presence lookup returned nothing"))
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
        Target::PrivateAccess { place, access_code } => format!(
            "https://assetgame.roblox.com/game/PlaceLauncher.ashx?request=RequestPrivateGame&browserTrackerId={tracker}&placeId={place}&accessCode={}&isPlayTogetherGame=false",
            url_encode(access_code)
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

/// A game as shown on a Home card.
#[derive(Clone, Serialize, Deserialize)]
pub struct Game {
    pub universe: u64,
    pub place: u64,
    pub name: String,
    pub playing: Option<u64>,
    pub icon: Option<String>,
    #[serde(default)]
    pub stats: Option<Stats>,
}

/// What the hover card shows about a game.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Stats {
    pub creator: String,
    pub genre: String,
    pub visits: u64,
    pub max_players: u64,
    pub favorites: u64,
    pub up_votes: u64,
    pub down_votes: u64,
    /// "YYYY-MM-DD"
    pub updated: String,
}

/// A friend who is in a game right now.
#[derive(Clone, Serialize, Deserialize)]
pub struct FriendGame {
    pub user_id: u64,
    pub name: String,
    pub display: String,
    pub avatar: Option<String>,
    /// Empty when the friend's privacy settings hide where they are.
    pub game: String,
    pub place: Option<u64>,
    pub job: Option<String>,
}

/// Reads a game from the metadata objects the discovery and search endpoints return.
fn game_from(v: &Value) -> Option<Game> {
    Some(Game {
        universe: v["universeId"].as_u64()?,
        place: v["rootPlaceId"].as_u64()?,
        name: v["name"].as_str()?.to_string(),
        playing: v["playerCount"].as_u64(),
        icon: None,
        stats: None,
    })
}

fn random_u64() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
    let mut x = nanos ^ COUNTER.fetch_add(1, Ordering::Relaxed).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03;
    for _ in 0..3 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
    }
    x
}

/// A random UUID-shaped id; the discovery endpoints want one per browsing session.
fn session_id() -> String {
    let (a, b) = (random_u64(), random_u64());
    format!(
        "{:08x}-{:04x}-4{:03x}-a{:03x}-{:012x}",
        a >> 32,
        (a >> 16) & 0xffff,
        a & 0xfff,
        b >> 52,
        b & 0xffff_ffff_ffff
    )
}

pub fn universe_of(cookie: &str, place: u64) -> Res<u64> {
    let v = get_json(cookie, &format!("https://apis.roblox.com/universes/v1/places/{place}/universe"))?;
    v["universeId"].as_u64().ok_or_else(|| other("unknown place"))
}

fn details_map(cookie: &str, universes: &[u64]) -> Res<HashMap<u64, Game>> {
    let mut found = HashMap::new();
    for chunk in universes.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let v = get_json(cookie, &format!("https://games.roblox.com/v1/games?universeIds={}", ids.join(",")))?;
        for g in v["data"].as_array().into_iter().flatten() {
            let (Some(universe), Some(place)) = (g["id"].as_u64(), g["rootPlaceId"].as_u64()) else { continue };
            let text = |k: &str| g[k].as_str().unwrap_or_default().to_string();
            let genre = [text("genre_l1"), text("genre")].into_iter().find(|s| !s.is_empty() && s != "All");
            let stats = Stats {
                creator: g["creator"]["name"].as_str().unwrap_or_default().to_string(),
                genre: genre.unwrap_or_default(),
                visits: g["visits"].as_u64().unwrap_or(0),
                max_players: g["maxPlayers"].as_u64().unwrap_or(0),
                favorites: g["favoritedCount"].as_u64().unwrap_or(0),
                updated: text("updated").chars().take(10).collect(),
                ..Stats::default()
            };
            let game = Game {
                universe,
                place,
                name: text("name"),
                playing: g["playing"].as_u64(),
                icon: None,
                stats: Some(stats),
            };
            found.insert(universe, game);
        }
    }
    Ok(found)
}

/// Names, live player counts and stats for games, in the order the universes were given.
pub fn game_details(cookie: &str, universes: &[u64]) -> Res<Vec<Game>> {
    let mut found = details_map(cookie, universes)?;
    Ok(universes.iter().filter_map(|u| found.remove(u)).collect())
}

/// Fills in whatever games are missing for their cards: stats, likes and icons. Lookups that fail
/// just leave the gaps.
pub fn enrich(cookie: &str, games: &mut [Game]) {
    let missing: Vec<u64> = games.iter().filter(|g| g.stats.is_none()).map(|g| g.universe).collect();
    if let Ok(found) = details_map(cookie, &missing) {
        for g in games.iter_mut() {
            if let Some(d) = found.get(&g.universe) {
                g.stats = d.stats.clone();
                g.playing = d.playing.or(g.playing);
            }
        }
    }
    let universes: Vec<u64> = games.iter().map(|g| g.universe).collect();
    for chunk in universes.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let Ok(v) = get_json(cookie, &format!("https://games.roblox.com/v1/games/votes?universeIds={}", ids.join(",")))
        else {
            continue;
        };
        for vote in v["data"].as_array().into_iter().flatten() {
            let Some(id) = vote["id"].as_u64() else { continue };
            for s in games.iter_mut().filter(|g| g.universe == id).filter_map(|g| g.stats.as_mut()) {
                s.up_votes = vote["upVotes"].as_u64().unwrap_or(0);
                s.down_votes = vote["downVotes"].as_u64().unwrap_or(0);
            }
        }
    }
    add_icons(cookie, games);
}

/// Avatar headshot URLs for users.
pub fn headshots(cookie: &str, ids: &[u64]) -> HashMap<u64, String> {
    let mut out = HashMap::new();
    for chunk in ids.chunks(100) {
        let list: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let url = format!(
            "https://thumbnails.roblox.com/v1/users/avatar-headshot?userIds={}&size=150x150&format=Png&isCircular=false",
            list.join(",")
        );
        let Ok(v) = get_json(cookie, &url) else { continue };
        for t in v["data"].as_array().into_iter().flatten() {
            if let (Some(id), Some(url)) = (t["targetId"].as_u64(), t["imageUrl"].as_str()) {
                out.insert(id, url.to_string());
            }
        }
    }
    out
}

/// Fills in game icons. Missing icons just stay empty.
fn add_icons(cookie: &str, games: &mut [Game]) {
    let universes: Vec<u64> = games.iter().filter(|g| g.icon.is_none()).map(|g| g.universe).collect();
    for chunk in universes.chunks(50) {
        let ids: Vec<String> = chunk.iter().map(u64::to_string).collect();
        let url = format!(
            "https://thumbnails.roblox.com/v1/games/icons?universeIds={}&returnPolicy=PlaceHolder&size=150x150&format=Png&isCircular=false",
            ids.join(",")
        );
        let Ok(v) = get_json(cookie, &url) else { continue };
        for t in v["data"].as_array().into_iter().flatten() {
            if let (Some(id), Some(url)) = (t["targetId"].as_u64(), t["imageUrl"].as_str()) {
                for g in games.iter_mut().filter(|g| g.universe == id) {
                    g.icon = Some(url.to_string());
                }
            }
        }
    }
}

/// Roblox's own "Continue" row for the account (an unofficial endpoint).
pub fn continue_list(cookie: &str) -> Res<Vec<Game>> {
    let body = serde_json::json!({ "pageType": "Home", "sessionId": session_id() });
    let v = post_json(cookie, "https://apis.roblox.com/discovery-api/omni-recommendation", &body)?;
    let meta = &v["contentMetadata"]["Game"];
    let sort = v["sorts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|s| s["topic"].as_str().is_some_and(|t| t.to_lowercase().contains("continue")));
    let Some(sort) = sort else { return Ok(Vec::new()) };
    Ok(sort["recommendationList"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| r["contentType"].as_str() == Some("Game"))
        .filter_map(|r| game_from(&meta[r["contentId"].as_u64()?.to_string()]))
        .collect())
}

/// Games the user favorited on Roblox, newest first.
pub fn favorites(cookie: &str, user_id: u64) -> Res<Vec<Game>> {
    let v = get_json(
        cookie,
        &format!("https://games.roblox.com/v2/users/{user_id}/favorite/games?limit=50&sortOrder=Desc"),
    )?;
    let universes: Vec<u64> = v["data"].as_array().into_iter().flatten().filter_map(|g| g["id"].as_u64()).collect();
    game_details(cookie, &universes)
}

/// The Charts page: (row title, games) for each of its rows (an unofficial endpoint).
pub fn charts(cookie: &str) -> Res<Vec<(String, Vec<Game>)>> {
    let url = format!(
        "https://apis.roblox.com/explore-api/v1/get-sorts?sessionId={}&device=computer&country=all",
        session_id()
    );
    let v = get_json(cookie, &url)?;
    Ok(v["sorts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| {
            let title = [&s["sortDisplayName"], &s["sortId"]]
                .iter()
                .find_map(|t| t.as_str().filter(|t| !t.is_empty()))?
                .to_string();
            let games: Vec<Game> = s["games"].as_array()?.iter().filter_map(game_from).collect();
            (!games.is_empty()).then_some((title, games))
        })
        .take(5)
        .collect())
}

/// Searches all of Roblox for games (an unofficial endpoint).
pub fn search(cookie: &str, query: &str) -> Res<Vec<Game>> {
    let url = format!(
        "https://apis.roblox.com/search-api/omni-search?searchQuery={}&sessionId={}&pageType=all",
        url_encode(query),
        session_id()
    );
    let v = get_json(cookie, &url)?;
    Ok(v["searchResults"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|g| g["contentGroupType"].as_str() == Some("Game"))
        .flat_map(|g| g["contents"].as_array().into_iter().flatten())
        .filter_map(game_from)
        .collect())
}

pub fn friend_ids(cookie: &str, user_id: u64) -> Res<Vec<u64>> {
    let v = get_json(cookie, &format!("https://friends.roblox.com/v1/users/{user_id}/friends"))?;
    Ok(v["data"].as_array().into_iter().flatten().filter_map(|f| f["id"].as_u64()).collect())
}

/// (username, display name) of users. Users that can't be looked up are left out.
pub fn user_names(cookie: &str, ids: &[u64]) -> HashMap<u64, (String, String)> {
    let mut names = HashMap::new();
    for chunk in ids.chunks(100) {
        let body = serde_json::json!({ "userIds": chunk, "excludeBannedUsers": false });
        let Ok(v) = post_json(cookie, "https://users.roblox.com/v1/users", &body) else { continue };
        for u in v["data"].as_array().into_iter().flatten() {
            if let Some(id) = u["id"].as_u64() {
                let name = u["name"].as_str().unwrap_or_default().to_string();
                names.insert(id, (name, u["displayName"].as_str().unwrap_or_default().to_string()));
            }
        }
    }
    names
}

/// Friends of the user who are in a game right now, with names and avatars.
pub fn friends_playing(cookie: &str, user_id: u64) -> Res<Vec<FriendGame>> {
    let ids = friend_ids(cookie, user_id)?;
    let playing: Vec<(u64, Presence)> = presence_many(cookie, &ids)?.into_iter().filter(|(_, p)| p.kind == 2).collect();
    if playing.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<u64> = playing.iter().map(|(id, _)| *id).collect();
    let mut names = user_names(cookie, &ids);
    let mut avatars = headshots(cookie, &ids);

    Ok(playing
        .into_iter()
        .map(|(id, p)| {
            let (name, display) = names.remove(&id).unwrap_or_default();
            FriendGame {
                user_id: id,
                name,
                display,
                avatar: avatars.remove(&id),
                game: p.location,
                place: p.root_place_id.or(p.place_id),
                job: p.game_id,
            }
        })
        .collect())
}

/// Lower is better. Ping counts as-is; a server outside the 50-85% full sweet spot pays up to
/// 200 ms extra, so a busy server with good ping beats an empty one with slightly better ping.
fn server_score(s: &Server) -> f64 {
    let fill = s.playing as f64 / s.max.max(1) as f64;
    let off = if fill < 0.5 {
        0.5 - fill
    } else if fill > 0.85 {
        fill - 0.85
    } else {
        0.0
    };
    s.ping.unwrap_or(250) as f64 + off * 400.0
}

/// A private server the account can join.
#[derive(Clone)]
pub struct PrivateServer {
    pub name: String,
    pub owner: String,
    /// True when the account itself owns it.
    pub mine: bool,
    pub access_code: String,
    pub playing: u32,
    pub max: u32,
}

/// The private servers of a game that the account owns or was given access to.
pub fn private_servers(cookie: &str, user_id: u64, place: u64) -> Res<Vec<PrivateServer>> {
    let mut out = Vec::new();
    let mut cursor = String::new();
    // A few pages at most; accounts rarely have more than a handful per game.
    for _ in 0..5 {
        let url = format!(
            "https://games.roblox.com/v1/games/{place}/private-servers?limit=100&sortOrder=Asc{}",
            if cursor.is_empty() { String::new() } else { format!("&cursor={}", url_encode(&cursor)) }
        );
        let v = get_json(cookie, &url)?;
        for s in v["data"].as_array().into_iter().flatten() {
            let Some(code) = s["accessCode"].as_str() else { continue };
            let owner = &s["owner"];
            let owner_name = [&owner["displayName"], &owner["name"]]
                .iter()
                .find_map(|n| n.as_str().filter(|n| !n.is_empty()))
                .unwrap_or("someone")
                .to_string();
            let players = s["players"].as_array().map_or(0, Vec::len).max(s["playerTokens"].as_array().map_or(0, Vec::len));
            out.push(PrivateServer {
                name: s["name"].as_str().filter(|n| !n.is_empty()).unwrap_or("Private server").to_string(),
                owner: owner_name,
                mine: owner["id"].as_u64() == Some(user_id),
                access_code: code.to_string(),
                playing: players as u32,
                max: s["maxPlayers"].as_u64().unwrap_or(0) as u32,
            });
        }
        match v["nextPageCursor"].as_str() {
            Some(c) if !c.is_empty() => cursor = c.to_string(),
            _ => break,
        }
    }
    // Your own servers first, then the busiest.
    out.sort_by(|a, b| b.mine.cmp(&a.mine).then(b.playing.cmp(&a.playing)));
    Ok(out)
}

/// Picks a public server with room for `need` more players, following `pick`.
pub fn pick_server(cookie: &str, place: u64, pick: ServerPick, need: u32) -> Res<Option<Server>> {
    if pick == ServerPick::Any {
        return Ok(None);
    }
    let fits = |s: &Server| s.max.saturating_sub(s.playing) >= need;
    let mut list: Vec<Server> = servers(cookie, place, pick == ServerPick::Fewest)?.into_iter().filter(fits).collect();
    match pick {
        ServerPick::LowestPing => list.sort_by_key(|s| s.ping.unwrap_or(u32::MAX)),
        ServerPick::Best => list.sort_by(|a, b| server_score(a).total_cmp(&server_score(b))),
        _ => {}
    }
    Ok(list.into_iter().next())
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
