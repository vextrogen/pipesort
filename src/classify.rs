//! The classifier: facts in, verdict out.
//!
//! This is the file you'll edit most. It's one ordered list of checks;
//! the first one that matches wins, and each check records *why* it
//! matched so the log can explain every decision.

use crate::facts::Facts;
use crate::rules::{Channel, Rules};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
    pub channel: Channel,
    pub confidence: Confidence,
    pub reason: String,
}

fn verdict(channel: Channel, confidence: Confidence, reason: impl Into<String>) -> Verdict {
    Verdict {
        channel,
        confidence,
        reason: reason.into(),
    }
}

/// Process names (lowercase prefixes) that mean "launched by a game launcher".
/// Note: the Steam client itself is deliberately absent; Steam starts games
/// through `reaper`, while its own UI audio comes from `steamwebhelper`.
const LAUNCHERS: &[&str] = &[
    "reaper",
    "lutris",
    "heroic",
    "bottles",
    "umu-run",
    "legendary",
    "gogdl",
];

pub fn classify(f: &Facts, rules: &Rules) -> Verdict {
    use Channel::*;
    use Confidence::*;

    let names = f.names();
    // Returns the first pattern that appears in any of the app's names.
    let find = |patterns: &[String]| -> Option<String> {
        patterns
            .iter()
            .find(|p| names.iter().any(|n| n.contains(p.as_str())))
            .cloned()
    };

    // 1. Your own rules always win.
    for o in &rules.overrides {
        if names.iter().any(|n| n.contains(&o.pattern)) {
            return verdict(o.channel, High, format!("user rule '{}'", o.pattern));
        }
    }

    // 2. Known chat apps.
    if let Some(p) = find(&rules.chat_apps) {
        return verdict(Chat, High, format!("known chat app ('{p}')"));
    }

    // 3. Known music players.
    if let Some(p) = find(&rules.music_apps) {
        return verdict(Music, High, format!("known music app ('{p}')"));
    }

    // 4. Known launchers/stores that would otherwise look like games.
    if let Some(p) = find(&rules.not_games) {
        return verdict(App, High, format!("launcher/store, not a game ('{p}')"));
    }

    // 5. Steam sets SteamAppId for every game it launches, native or Proton.
    if let Some(id) = &f.steam_app_id {
        return verdict(Game, High, format!("Steam game (AppId {id})"));
    }

    // 6. The game (or its parent) registered with Feral gamemode.
    if f.gamemode {
        return verdict(Game, High, "registered with gamemode");
    }

    // 7. Running inside gamescope.
    if let Some((_, name)) = f
        .ancestry
        .iter()
        .find(|(_, n)| n.to_lowercase().starts_with("gamescope"))
    {
        return verdict(Game, High, format!("running inside {name}"));
    }

    // 8. Started by a game launcher.
    if let Some((_, name)) = f
        .ancestry
        .iter()
        .find(|(_, n)| LAUNCHERS.iter().any(|l| n.to_lowercase().starts_with(l)))
    {
        return verdict(Game, Medium, format!("started by launcher '{name}'"));
    }

    // 9. The app says it's a call/voice stream.
    if let Some(role) = &f.media_role {
        let role = role.to_lowercase();
        if role == "communication" || role == "phone" {
            return verdict(Chat, Medium, format!("media.role = {role}"));
        }
    }

    // 10. Wine: a WINEPREFIX, a .exe name, or wine/proton in the process tree.
    let exe_name = names.iter().find(|n| n.ends_with(".exe"));
    let wine_parent = f.ancestry.iter().find(|(_, n)| {
        let n = n.to_lowercase();
        n.starts_with("wine") || n.contains("proton")
    });
    if f.wine_prefix || exe_name.is_some() || wine_parent.is_some() {
        let why = if let Some(n) = exe_name {
            format!("Windows program '{n}'")
        } else if let Some((_, n)) = wine_parent {
            format!("runs under '{n}'")
        } else {
            "WINEPREFIX set".to_string()
        };
        return verdict(Game, Medium, format!("Wine: {why}"));
    }

    // 11. Nothing matched. An open mic is deliberately *not* treated as
    // Chat: browsers, recorders and dictation tools hold one too.
    if f.has_mic_stream {
        return verdict(
            App,
            Low,
            "no game or chat signals (mic open, not enough for Chat)",
        );
    }
    verdict(App, Low, "no game or chat signals")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(app_name: &str) -> Facts {
        Facts {
            app_name: app_name.to_string(),
            pid: Some(1000),
            ..Facts::default()
        }
    }

    fn rules() -> Rules {
        Rules::builtin()
    }

    #[test]
    fn plain_app_goes_to_app() {
        let v = classify(&facts("Helium"), &rules());
        assert_eq!(v.channel, Channel::App);
    }

    #[test]
    fn spotify_is_music() {
        let v = classify(&facts("Spotify"), &rules());
        assert_eq!(v.channel, Channel::Music);
    }

    #[test]
    fn discord_voice_is_chat() {
        let v = classify(&facts("WEBRTC VoiceEngine"), &rules());
        assert_eq!(v.channel, Channel::Chat);
    }

    #[test]
    fn steam_game_is_game() {
        let f = Facts {
            steam_app_id: Some("1091500".into()),
            ..facts("Cyberpunk2077.exe")
        };
        let v = classify(&f, &rules());
        assert_eq!(v.channel, Channel::Game);
        assert!(v.reason.contains("1091500"));
    }

    #[test]
    fn exe_without_steam_is_wine_game() {
        let v = classify(&facts("Game.exe"), &rules());
        assert_eq!(v.channel, Channel::Game);
        assert_eq!(v.confidence, Confidence::Medium);
    }

    #[test]
    fn battle_net_launcher_is_not_a_game() {
        let f = Facts {
            wine_prefix: true,
            ..facts("Battle.net.exe")
        };
        assert_eq!(classify(&f, &rules()).channel, Channel::App);
    }

    #[test]
    fn launcher_ancestry_is_game() {
        let f = Facts {
            ancestry: vec![(50, "reaper".into()), (40, "steam".into())],
            ..facts("factorio")
        };
        assert_eq!(classify(&f, &rules()).channel, Channel::Game);
    }

    #[test]
    fn steam_client_itself_is_not_a_game() {
        let f = Facts {
            ancestry: vec![(40, "steam".into())],
            ..facts("steamwebhelper")
        };
        assert_eq!(classify(&f, &rules()).channel, Channel::App);
    }

    #[test]
    fn mic_stream_alone_is_not_chat() {
        // e.g. the Helium browser with a tab using the mic
        let f = Facts {
            has_mic_stream: true,
            ..facts("Helium")
        };
        let v = classify(&f, &rules());
        assert_eq!(v.channel, Channel::App);
        assert_eq!(v.confidence, Confidence::Low);
    }

    #[test]
    fn user_override_beats_everything() {
        let file: &str = "[[override]]\nmatch = \"Game.exe\"\nchannel = \"app\"\n";
        let path = std::env::temp_dir().join("pipesort-test-rules");
        std::fs::create_dir_all(path.join("pipesort")).unwrap();
        std::fs::write(path.join("pipesort/rules.toml"), file).unwrap();
        // SAFETY: tests in this module don't read XDG_CONFIG_HOME concurrently.
        unsafe { std::env::set_var("XDG_CONFIG_HOME", &path) };
        let (rules, _) = Rules::load();
        let v = classify(&facts("Game.exe"), &rules);
        assert_eq!(v.channel, Channel::App);
        assert!(v.reason.contains("user rule"));

        // `--set` appends a newer override, which must beat the older one.
        crate::rules::add_override("game", Channel::Game).unwrap();
        let (rules, _) = Rules::load();
        assert_eq!(classify(&facts("Game.exe"), &rules).channel, Channel::Game);
    }
}
