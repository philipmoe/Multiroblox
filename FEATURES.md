# Multi Roblox – Features

## Multi-instance
- Run many Roblox clients at once, each on its own account
- **Release lock** (More menu) frees clients that were already running
- Windows renamed to **"Roblox - username"**

## Accounts
- Add by `.ROBLOSECURITY` cookie, stored encrypted (Windows DPAPI)
- Avatar, live status and server location on each account
- **Main** account (dropdown, or *Make main*), the rest are **Alts**
- **Stop** one client, **Stop alts**, **Kill all**

## Launching (Game panel)
- Place ID, game link, or private server link (`privateServerLinkCode` or `share?code=`)
- Optional **Job ID** for an exact server
- **Server pick** when no Job ID: Best (low ping, 50–85% full) · Lowest ping · Fewest · Most players
- Launch main, launch alts, *Main joins then alts follow*, *Alts join main's server*, *Alts follow main*
- Delay between launches

## Servers window (*Servers...*)
| Tab | What |
|---|---|
| Browse | Public servers with players and ping |
| Private | Private servers you own or were invited to, for the selected game |
| Recent | Last 30 servers any client was in; **Rejoin** |
| Saved | Named games, servers and private links; **Join** in one click |

## Home
- **Playing as** switcher: browse and play as any account
- Search, Continue playing, Friends playing now (Join), Favorites, Charts
- Hover a game for stats: likes, visits, favorites, server size, genre, updated, your playtime
- **Play** = smart play into the Best server
- Rows and images cached, so they show instantly

## AFK mode (Settings, alts only or *Include main*)
- Lowest graphics · FPS cap (1–240) · CPU limit per client
- Mute · Low CPU priority · Minimize after launch

## Staying in game
- **Anti-AFK**: Space or mouse nudge every 1–16 min, stops the 20-min kick
- **Auto-rejoin** when an account is disconnected or kicked
- **Keep PC awake**

## Live info
- Header shows who is in which game and where
- Notification on join with the **server location** (e.g. London, GB)
- **Friend alerts** when friends of any account start playing
- **Discord status** with a Join button (needs a Discord application ID)

## Tracking
- **Playtime** per game and per account, from any Roblox client on this PC
- Stored locally only

## App
- **Start with Windows** (opens minimized)
- Tray icon, dark theme, collapsible log

## Data on this PC
| File | Holds |
|---|---|
| `%APPDATA%\MultiRoblox\config.json` | Accounts (encrypted cookies), settings |
| `%APPDATA%\MultiRoblox\history.json` | Playtime, recent servers |
| `%APPDATA%\MultiRoblox\home_cache.json` | Home rows |
| `%APPDATA%\MultiRoblox\graphics.json` | Your own graphics settings, restored after AFK launches |
| `%LOCALAPPDATA%\MultiRoblox\images\` | Cached icons and avatars |
