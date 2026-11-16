#!/usr/bin/env bash
# Install pipesort for the current user (no root needed):
#   - builds the binary and puts it in ~/.local/bin
#   - adds the four mixer sinks (PipeWire) and the routing script (WirePlumber)
#   - copies the rules template, if you don't have one yet
#   - installs and starts the systemd user service
#
# Existing files that differ from ours are backed up as FILE.bak-<date>, never
# overwritten silently. Run ./uninstall.sh to remove everything again.
set -euo pipefail
cd "$(dirname "$0")"

CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
BIN="$HOME/.local/bin"
STAMP="$(date +%Y%m%d-%H%M%S)"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# --- 1. Check what we depend on -------------------------------------------
for cmd in cargo pw-dump pw-metadata busctl systemctl wireplumber; do
    command -v "$cmd" >/dev/null || die "'$cmd' not found; install it first"
done
# The routing script and its loader use the WirePlumber 0.5 format;
# 0.4 (older Debian/Ubuntu) uses Lua config files and won't load them.
wp_version="$(wireplumber --version | sed -n 's/.*libwireplumber \([0-9.]*\).*/\1/p' | tail -n1)"
case "$wp_version" in
    0.5.* | 0.[6-9].* | [1-9].*) ;;
    *) die "WirePlumber 0.5 or newer is required (found '${wp_version:-unknown}')" ;;
esac

# --- 2. Build and install the binary --------------------------------------
say "Building pipesort (release)"
cargo build --release --locked
install -Dm755 target/release/pipesort "$BIN/pipesort"
say "Installed $BIN/pipesort"

# --- 3. Config files ------------------------------------------------------
# place SRC DEST: copy unless DEST is already identical; back up a different one.
place() {
    local src="$1" dest="$2"
    if [ -e "$dest" ]; then
        if cmp -s "$src" "$dest"; then
            say "Up to date: $dest"
            return
        fi
        mv "$dest" "$dest.bak-$STAMP"
        say "Backed up your old $dest to $dest.bak-$STAMP"
    fi
    install -Dm644 "$src" "$dest"
    say "Installed $dest"
}

place contrib/pipewire/10-mixer-channels.conf "$CONFIG/pipewire/pipewire.conf.d/10-mixer-channels.conf"
place contrib/wireplumber/mixer-channels.lua "$DATA/wireplumber/scripts/mixer-channels.lua"
place contrib/wireplumber/50-mixer-channels.conf "$CONFIG/wireplumber/wireplumber.conf.d/50-mixer-channels.conf"
place contrib/pipesort.service "$CONFIG/systemd/user/pipesort.service"

# Your rules are yours: only copy the template if there's nothing there yet.
rules="$CONFIG/pipesort/rules.toml"
if [ -e "$rules" ]; then
    say "Keeping your existing $rules"
else
    install -Dm644 examples/rules.toml "$rules"
    say "Installed rules template $rules"
fi

# --- 4. Restart audio so the sinks appear, then start the service ---------
say "Restarting PipeWire and WirePlumber (sound cuts out for a moment)"
systemctl --user restart pipewire wireplumber
# pipewire-pulse isn't on every system; restart it only if it's running.
systemctl --user try-restart pipewire-pulse 2>/dev/null || true

systemctl --user daemon-reload
systemctl --user enable pipesort
systemctl --user restart pipesort
say "pipesort service is running (logs: journalctl --user -u pipesort -f)"

case ":$PATH:" in
    *":$BIN:"*) ;;
    *) say "Note: $BIN isn't on your PATH, so add it to run 'pipesort' by name" ;;
esac
say "Done. Try 'pipesort --help'."
