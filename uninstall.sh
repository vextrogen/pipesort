#!/usr/bin/env bash
# Remove everything install.sh added. Your rules.toml and the observation log
# are kept unless you pass --purge.
set -euo pipefail

CONFIG="${XDG_CONFIG_HOME:-$HOME/.config}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
STATE="${XDG_STATE_HOME:-$HOME/.local/state}"

say() { printf '\033[1m==>\033[0m %s\n' "$*"; }

remove() {
    if [ -e "$1" ]; then
        rm -f -- "$1"
        say "Removed $1"
    fi
}

say "Stopping the pipesort service"
systemctl --user disable --now pipesort 2>/dev/null || true
remove "$CONFIG/systemd/user/pipesort.service"
systemctl --user daemon-reload

remove "$HOME/.local/bin/pipesort"
remove "$CONFIG/pipewire/pipewire.conf.d/10-mixer-channels.conf"
remove "$DATA/wireplumber/scripts/mixer-channels.lua"
remove "$CONFIG/wireplumber/wireplumber.conf.d/50-mixer-channels.conf"

if [ "${1:-}" = "--purge" ]; then
    rm -rf -- "$CONFIG/pipesort" "$STATE/pipesort"
    say "Removed your rules and observation log"
else
    say "Kept $CONFIG/pipesort/rules.toml and $STATE/pipesort (use --purge to remove)"
fi

# Streams that were in our sinks fall back to the default device.
say "Restarting PipeWire and WirePlumber so the sinks disappear"
systemctl --user restart pipewire wireplumber
systemctl --user try-restart pipewire-pulse 2>/dev/null || true
say "Done."
