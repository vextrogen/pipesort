# pipesort

Per-app audio channels for Linux, like SteelSeries Sonar or Elgato Wave Link.

pipesort splits your PipeWire audio into four virtual outputs (**Game**,
**Chat**, **Music** and **App**), each with its own volume, and sorts every
new app into the right one automatically. Discord goes to Chat, Spotify to
Music, Steam and Wine games to Game, everything else to App.

## Requirements

- PipeWire with WirePlumber **0.5 or newer**
- Rust (`cargo`), to build it
- systemd (for the background service)

## Install

```bash
git clone https://github.com/vextrogen/pipesort && cd pipesort
./install.sh
```

This builds the binary into `~/.local/bin`, adds the four sinks and a
WirePlumber routing script, and starts a `pipesort` user service. No root
needed. Your audio cuts out for a moment while PipeWire restarts. Any file
it would replace is backed up as `FILE.bak-<date>` first.

Set the volume of each channel in your desktop's sound settings, pavucontrol,
or any mixer that controls PipeWire sinks. Your speakers/headphones stay the
default output, so switching devices works as usual.

To remove it all:

```bash
./uninstall.sh            # keeps your rules and log
./uninstall.sh --purge    # removes those too
```

## Usage

Run `pipesort` in a terminal to see a live table of playing streams: where
each one is now, where pipesort thinks it belongs (`≠` marks a mismatch), and
why. The selected stream's full reason and process facts show below the table.

| Key | Action |
|---|---|
| ↑/↓ or j/k | Select a stream |
| g / c / m / a | Move it to Game / Chat / Music / App now |
| G / C / M / A | Same, and always put this app there |
| v | Show the facts under every stream |
| q or Ctrl+C | Quit |

`--replay` opens the same view for browsing; nothing is moved.

From the command line:

```bash
pipesort --set factorio game     # always put matching apps in Game
pipesort --observe               # watch only, don't move anything
pipesort -v                      # show the facts behind each decision
journalctl --user -u pipesort -f # what the background service is doing
```

## Rules

Your rules live in `~/.config/pipesort/rules.toml` (see
`examples/rules.toml`). They're added to the built-in lists and apply to the
next new stream, no restart needed. Names match case-insensitively as
substrings of the app name, binary or Flatpak ID; `pipesort -v` shows the
names an app goes by.

```toml
chat_apps  = ["element"]
music_apps = ["my-player"]
not_games  = ["itch"]       # looks like a game, isn't

[[override]]                # beats everything; the last match wins
match = "helium"
channel = "app"
```

## How it decides

The first matching check wins:

1. Your `[[override]]` rules
2. Known chat apps → Chat
3. Known music players (Spotify, Tidal, Strawberry…) → Music
4. Known launchers/stores (Battle.net, EA app, Steam client UI…) → App
5. `SteamAppId` in the process environment → Game
6. Registered with Feral gamemode → Game
7. Running inside gamescope → Game
8. Started by a launcher (`reaper`, lutris, heroic, bottles, umu…) → Game
9. `media.role` is communication/phone → Chat
10. Wine (`WINEPREFIX`, a `.exe` name, wine/proton parent) → Game
11. Otherwise → App

Apps sent straight to a device are never moved. If pipesort keeps moving an
app you want elsewhere, pin it with `--set` or the G/C/M/A keys.
Every decision is logged to `~/.local/state/pipesort/observations.jsonl`.

## Limitations

- A browser is one stream: Discord or Meet in a browser tab can't be split
  from the rest of the browser.
- A game with built-in voice chat is one stream, so it goes to Game.

## License

MIT, see [LICENSE](LICENSE).
