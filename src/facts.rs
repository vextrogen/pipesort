//! Gather facts about the process behind a stream.
//!
//! Almost everything comes from `/proc/<pid>/...`, which is plain text the
//! kernel exposes for every process you own. Nothing here decides anything;
//! that's `classify.rs`. Every lookup is allowed to fail (process already
//! exited, permission denied) and just leaves the fact empty.

use crate::graph::Graph;
use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::process::Command;

#[derive(Debug, Default, Clone, Serialize)]
pub struct Facts {
    pub node_id: u32,
    pub pid: Option<u32>,
    /// `application.name` as the app reported it.
    pub app_name: String,
    /// `application.process.binary` as the app reported it.
    pub binary: String,
    /// Real executable path from `/proc/<pid>/exe`.
    pub exe: String,
    /// argv[0] from `/proc/<pid>/cmdline` (for Wine this is often the .exe).
    pub argv0: String,
    pub flatpak_id: Option<String>,
    pub media_role: Option<String>,
    /// SteamAppId / SteamGameId / STEAM_COMPAT_APP_ID from the environment.
    pub steam_app_id: Option<String>,
    /// WINEPREFIX is set in the environment.
    pub wine_prefix: bool,
    /// Parent, grandparent, ... as (pid, name), nearest first.
    pub ancestry: Vec<(u32, String)>,
    /// This process or an ancestor is registered with Feral gamemode.
    pub gamemode: bool,
    /// The same process also has a microphone stream open.
    pub has_mic_stream: bool,
}

impl Facts {
    /// All the names this app goes by, lowercased, for pattern matching.
    pub fn names(&self) -> Vec<String> {
        let mut names = vec![
            self.app_name.to_lowercase(),
            self.binary.to_lowercase(),
            basename(&self.exe).to_lowercase(),
            basename(&self.argv0).to_lowercase(),
        ];
        if let Some(id) = &self.flatpak_id {
            names.push(id.to_lowercase());
        }
        names.retain(|n| !n.is_empty());
        names.dedup();
        names
    }

    /// Short label for log output.
    pub fn display_name(&self) -> &str {
        if !self.app_name.is_empty() {
            &self.app_name
        } else if !self.binary.is_empty() {
            &self.binary
        } else {
            "(unnamed)"
        }
    }
}

pub fn gather(graph: &Graph, node: u32, gamemode_pids: &HashSet<u32>) -> Facts {
    let mut f = Facts {
        node_id: node,
        pid: graph.pid_of(node),
        app_name: graph.prop(node, "application.name").unwrap_or_default(),
        binary: graph
            .prop(node, "application.process.binary")
            .unwrap_or_default(),
        flatpak_id: graph
            .prop(node, "pipewire.access.portal.app_id")
            .or_else(|| graph.prop(node, "application.id")),
        media_role: graph.prop(node, "media.role"),
        ..Facts::default()
    };

    let Some(pid) = f.pid else {
        return f; // no PID, nothing more we can learn
    };

    f.exe = fs::read_link(format!("/proc/{pid}/exe"))
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    f.argv0 = argv0(pid).unwrap_or_default();

    let env = environ(pid);
    f.steam_app_id = ["SteamAppId", "SteamGameId", "STEAM_COMPAT_APP_ID"]
        .iter()
        .find_map(|key| env.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
        .filter(|v| !v.is_empty() && v != "0");
    f.wine_prefix = env.iter().any(|(k, _)| k == "WINEPREFIX");

    if f.flatpak_id.is_none() {
        f.flatpak_id = flatpak_id(pid);
    }

    f.ancestry = ancestry(pid);
    f.gamemode =
        gamemode_pids.contains(&pid) || f.ancestry.iter().any(|(p, _)| gamemode_pids.contains(p));
    f.has_mic_stream = graph.capture_pids().contains(&pid);
    f
}

/// Process name: argv[0]'s filename, or the kernel's short `comm` name.
fn proc_name(pid: u32) -> String {
    argv0(pid)
        .map(|a| basename(&a).to_string())
        .filter(|a| !a.is_empty())
        .or_else(|| {
            fs::read_to_string(format!("/proc/{pid}/comm"))
                .ok()
                .map(|s| s.trim().to_string())
        })
        .unwrap_or_default()
}

fn argv0(pid: u32) -> Option<String> {
    let raw = fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let first = raw.split(|b| *b == 0).next()?;
    Some(String::from_utf8_lossy(first).into_owned())
}

/// The environment is NUL-separated KEY=VALUE pairs.
fn environ(pid: u32) -> Vec<(String, String)> {
    let Ok(raw) = fs::read(format!("/proc/{pid}/environ")) else {
        return Vec::new();
    };
    raw.split(|b| *b == 0)
        .filter_map(|entry| {
            let entry = String::from_utf8_lossy(entry);
            let (k, v) = entry.split_once('=')?;
            Some((k.to_string(), v.to_string()))
        })
        .collect()
}

/// Flatpak apps have a `.flatpak-info` file at the root of their sandbox.
fn flatpak_id(pid: u32) -> Option<String> {
    let info = fs::read_to_string(format!("/proc/{pid}/root/.flatpak-info")).ok()?;
    info.lines()
        .find_map(|l| l.strip_prefix("name="))
        .map(|s| s.trim().to_string())
}

/// Walk up the process tree until init (PID 1).
fn ancestry(pid: u32) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut current = parent_of(pid);
    while let Some(p) = current {
        if p <= 1 || out.len() >= 24 {
            break;
        }
        out.push((p, proc_name(p)));
        current = parent_of(p);
    }
    out
}

/// `/proc/<pid>/stat` looks like `1234 (name) S 1000 ...`; the 4th field is
/// the parent PID. The name can contain spaces and brackets, so we start
/// after the *last* `)`.
fn parent_of(pid: u32) -> Option<u32> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_name = stat.get(stat.rfind(')')? + 1..)?;
    after_name.split_whitespace().nth(1)?.parse().ok()
}

/// Ask Feral gamemode which PIDs are registered as games, via `busctl`.
/// Output looks like: `a(io) 2 1234 "/com/..." 5678 "/com/..."`.
/// If gamemode isn't installed or running, this is just an empty set.
pub fn gamemode_pids() -> HashSet<u32> {
    let output = Command::new("busctl")
        .args([
            "--user",
            "call",
            "com.feralinteractive.GameMode",
            "/com/feralinteractive/GameMode",
            "com.feralinteractive.GameMode",
            "ListGames",
        ])
        .output();
    let Ok(output) = output else {
        return HashSet::new();
    };
    if !output.status.success() {
        return HashSet::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .skip(2) // "a(io)" and the count
        .filter_map(|tok| tok.parse().ok())
        .collect()
}

/// Last path component, treating both `/` and `\` (Wine paths) as separators.
pub fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
