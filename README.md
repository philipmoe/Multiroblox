# Multi Roblox

Run several Roblox clients side by side on one Windows PC, each signed in to a different account.

> **Not affiliated with, endorsed by or sponsored by Roblox Corporation.** "Roblox" is a trademark of
> Roblox Corporation. This is an unofficial tool; use it at your own risk.

## Features

- **Multiple clients at once.** Holds Roblox's single-instance lock so every new client can start,
  and releases it inside clients that were already running.
- **Account list.** Add accounts by their `.ROBLOSECURITY` cookie and see each one's online / in-game status.
- **Launch into a game.** Paste a place ID or a `roblox.com/games/...` link, optionally a job ID to pick
  a server, and launch the main account, the selected alts, or a single account.
- **Join the main account.** Alts can join the exact server the main account is in, or follow it as a friend.
- **Launch delay.** Waits a set number of seconds between launches so clients don't start all at once.
- **Kill all.** Closes every running Roblox client.

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
