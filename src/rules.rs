//! Rules: the built-in lists plus your own `rules.toml`.
//!
//! The config file lives at `~/.config/pipesort/rules.toml`.
//! Everything in it is optional; anything you add is appended to the
//! built-in lists below rather than replacing them.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// The four mixer channels. `Copy` means it's cheap to pass around by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Channel {
    Game,
    Chat,
    Music,
    App,
}

impl Channel {
    pub fn label(self) -> &'static str {
        match self {
            Channel::Game => "GAME",
            Channel::Chat => "CHAT",
            Channel::Music => "MUSIC",
            Channel::App => "APP",
        }
    }

    /// Which channel a sink belongs to, from its `node.name`
    /// (the names we gave the loopbacks in the PipeWire config).
    pub fn from_sink_name(name: &str) -> Option<Channel> {
        match name {
            "sink_game" => Some(Channel::Game),
            "sink_chat" => Some(Channel::Chat),
            "sink_music" => Some(Channel::Music),
            "sink_app" => Some(Channel::App),
            _ => None,
        }
    }
}

/// Apps that are always Chat. Matched case-insensitively as a substring of
/// the app name, binary, exe filename or Flatpak ID.
const BUILTIN_CHAT: &[&str] = &[
    "webrtc voiceengine", // Discord's voice output stream
    "discord",
    "vesktop",
    "teamspeak",
    "ts3client",
    "mumble",
    "zoom",
    "signal",
];

/// Music players that are always Music. Same matching as the chat list.
/// Video players (mpv, vlc) are left out on purpose: they play films too.
const BUILTIN_MUSIC: &[&str] = &[
    "spotify", // also covers spotify-qt, spotify_player
    "ncspot",
    "tidal",
    "feishin",
    "cider",
    "strawberry",
    "rhythmbox",
    "lollypop",
    "elisa",
    "amberol",
    "audacious",
    "deadbeef",
    "quod libet",
    "clementine",
    "cmus",
    "mpd",
    "youtube-music",
];

/// Things that look like games (they run under Wine, or come from a
/// launcher) but aren't: stores, launchers, the Steam client itself.
const BUILTIN_NOT_GAMES: &[&str] = &[
    "steamwebhelper",
    "battle.net",
    "eadesktop",
    "eaapp",
    "epicgameslauncher",
    "galaxyclient",
    "ubisoftconnect",
    "upc.exe",
];

/// One `[[override]]` entry from rules.toml.
#[derive(Debug, Clone, Deserialize)]
pub struct Override {
    /// `match` is a reserved word in Rust, so the field is called `pattern`
    /// here and renamed to `match` in the file.
    #[serde(rename = "match")]
    pub pattern: String,
    pub channel: Channel,
}

/// The shape of rules.toml. `#[serde(default)]` means "empty if missing".
#[derive(Debug, Default, Deserialize)]
struct RulesFile {
    #[serde(default, rename = "override")]
    overrides: Vec<Override>,
    #[serde(default)]
    chat_apps: Vec<String>,
    #[serde(default)]
    music_apps: Vec<String>,
    #[serde(default)]
    not_games: Vec<String>,
}

/// The merged rules the classifier uses. All patterns are lowercase.
#[derive(Debug, Clone)]
pub struct Rules {
    pub overrides: Vec<Override>,
    pub chat_apps: Vec<String>,
    pub music_apps: Vec<String>,
    pub not_games: Vec<String>,
}

impl Rules {
    /// Built-in rules only (used by tests and when there's no config file).
    pub fn builtin() -> Rules {
        Rules::merge(RulesFile::default())
    }

    /// Load `rules.toml` if it exists. A broken file prints a warning and
    /// falls back to the built-ins, so a typo never stops the program.
    pub fn load() -> (Rules, String) {
        let path = config_path();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(_) => {
                return (
                    Rules::builtin(),
                    format!("{} (not found, using built-ins)", path.display()),
                );
            }
        };
        match toml::from_str::<RulesFile>(&text) {
            Ok(file) => (Rules::merge(file), path.display().to_string()),
            Err(e) => {
                eprintln!("warning: could not parse {}: {e}", path.display());
                (
                    Rules::builtin(),
                    format!("{} (invalid, using built-ins)", path.display()),
                )
            }
        }
    }

    /// Overrides are checked from the bottom of the file up, so the newest
    /// one (e.g. just added by `--set`) wins over older ones.
    fn merge(file: RulesFile) -> Rules {
        let lower = |list: &[&str], extra: Vec<String>| -> Vec<String> {
            list.iter()
                .map(|s| s.to_string())
                .chain(extra)
                .map(|s| s.to_lowercase())
                .collect()
        };
        Rules {
            overrides: file
                .overrides
                .into_iter()
                .rev()
                .map(|o| Override {
                    pattern: o.pattern.to_lowercase(),
                    channel: o.channel,
                })
                .collect(),
            chat_apps: lower(BUILTIN_CHAT, file.chat_apps),
            music_apps: lower(BUILTIN_MUSIC, file.music_apps),
            not_games: lower(BUILTIN_NOT_GAMES, file.not_games),
        }
    }
}

/// Append an `[[override]]` to rules.toml, creating the file if needed.
/// Appending keeps your comments and layout intact; since overrides are read
/// bottom-up, the new one beats any older rule for the same app.
pub fn add_override(pattern: &str, channel: Channel) -> Result<PathBuf, String> {
    use std::io::Write;
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    // A toml::Value prints as a properly quoted and escaped TOML string.
    let pattern = toml::Value::String(pattern.to_string());
    let channel = toml::Value::String(channel.label().to_lowercase());
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    writeln!(
        file,
        "\n[[override]]\nmatch = {pattern}\nchannel = {channel}"
    )
    .map_err(|e| e.to_string())?;
    Ok(path)
}

fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"));
    base.join("pipesort").join("rules.toml")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}
