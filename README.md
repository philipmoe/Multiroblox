# Multi Roblox

Run several Roblox clients side by side on one Windows PC, each signed in to a different account.

> **Not affiliated with, endorsed by or sponsored by Roblox Corporation.** "Roblox" is a trademark of
> Roblox Corporation. This is an unofficial tool; use it at your own risk.

## Features

### Home

A discovery page for browsing and joining games without opening the Roblox website:

- **Search** all of Roblox.
- **Continue playing**: games you launched from here, plus Roblox's own recently played list.
- **Friends playing now**: your main account's friends who are in a game, with a Join button.
- **Favorites** and Roblox's **Charts** rows (Top Trending, Up-and-Coming, ...).
- **Playtime** per game and per account, from any Roblox client on this PC (also ones not started
  from here). It's read from Roblox's own log files while Multi Roblox is open and stored only on
  this PC. Hover a game to see it with the game's stats.
- **Server pick**: when no job ID is given, join the **Best** server (lowest ping among servers that
  are 50-85% full), or the one with the lowest ping, the fewest or the most players. Ping is the
  average of the players already in that server, so it's a good guess rather than your own ping.

Click a game to select it, or use **Play** (smart play: the Best server) to launch straight in. Search, Charts and
the Continue list use unofficial Roblox endpoints, so a row may stop loading if Roblox changes them.

### Accounts and launching

- **Multiple clients at once.** Holds Roblox's single-instance lock so every new client can start,
  and releases it inside clients that were already running.
- **Account list.** Add accounts by their `.ROBLOSECURITY` cookie and see each one's online / in-game status.
- **Launch into a game.** Paste a place ID or a `roblox.com/games/...` link, optionally a job ID to pick
  a server, and launch the main account, the selected alts, or a single account.
- **Server picker.** Browse the game's open servers by player count and pick one with a click.
- **Private servers.** Paste a private server link (`?privateServerLinkCode=...` or a
  `roblox.com/share?code=...&type=Server` link) and every account joins it.
- **Join the main account.** Alts can join the exact server the main account is in, or follow it as a friend.
- **Named windows.** Each client's window is titled "Roblox - username", so you can tell them apart.
- **Stop clients.** Close one account's client, all alts, or every client.
- **Launch delay.** Waits a set number of seconds between launches so clients don't start all at once.

### Settings for AFK players

**AFK mode** applies to alts, or to every account with **Include main**:

- **Lowest graphics** and **FPS cap**: the client starts at graphics quality 1 and a frame-rate limit
  (1-240 FPS). Roblox reads these from one shared settings file when a client starts, so Multi Roblox
  writes the AFK values just before an AFK client launches and puts your own values back before a
  normal launch. Roblox's own menu stops at 30 FPS, so the client may ignore 1-15; check with Shift+F5.
- **CPU limit**: Windows caps how much of the PC's total CPU each client may use (1-50%). It applies
  right away and works whatever Roblox's settings say. Too low can make Roblox lag so badly it
  disconnects, so start around 10%.
- **Mute** in the Windows volume mixer.
- **Low CPU priority**, so main and anything else you run stay smooth.
- **Minimize after launch.**

**Staying in game:**

- **Anti-AFK.** Every few minutes (1-16), briefly brings each Roblox window to the front and presses
  Space or nudges the mouse, so the 20-minute idle kick never happens. It waits for a pause in your typing,
  only sends input while a Roblox window is in front, and puts your window back. It can't work while the
  PC is locked.
- **Auto-rejoin.** Every 30 seconds it checks that each launched account is still in game. If one was
  disconnected or kicked, its client is closed and launched again.
- **Keep PC awake.** Stops Windows from sleeping while the app is open.
- **Start with Windows.** Opens Multi Roblox minimized when you sign in.

## Download

Get `MultiRoblox.exe` from the [Releases](../../releases) page and run it. No install is needed.

Windows SmartScreen or your antivirus may warn about it, because it is unsigned and works with
other processes' handles. The full source is here, so you can read it or build it yourself.

## Adding an account

1. Sign in to the account at roblox.com in a browser.
2. Press F12, open **Application > Cookies > https://www.roblox.com** and copy the value of `.ROBLOSECURITY`.
3. In Multi Roblox, click **Add account** and paste it.

**Treat that cookie like a password.** Anyone who has it can use the account. Only paste it into
programs you trust.

## Privacy

- Cookies are encrypted with Windows DPAPI, so only your Windows user on this PC can decrypt them.
- Settings and accounts are saved in `%APPDATA%\MultiRoblox\config.json`.
- The app only talks to `roblox.com` servers. It has no telemetry and sends nothing anywhere else.

## Building from source

Requires Windows and a recent [Rust](https://rustup.rs) toolchain.

```bash
cargo build --release
```

The program is written to `target/release/multi-roblox.exe`.

## License

[MIT](LICENSE)
