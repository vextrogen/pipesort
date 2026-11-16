//! pipesort: watch new audio streams and log which mixer channel
//! (Game, Chat, Music or App) each one *should* go to, and move it there if
//! WirePlumber's fast path put it in a different channel.
//! `--observe` (and `--replay`) only watch and never move audio.
//!
//! How the pieces fit:
//!   main.rs         – reads pw-dump, reacts to events, prints results
//!   graph.rs        – keeps track of PipeWire clients, nodes and links
//!   facts.rs        – reads /proc to learn about the process behind a stream
//!   classify.rs     – the decision list (the file you'll edit most)
//!   rules.rs        – built-in app lists + your rules.toml
//!   observations.rs – the JSONL log for later review
//!   table.rs        – the live table shown when run in a terminal

mod classify;
mod facts;
mod graph;
mod observations;
mod rules;
mod table;

use classify::Verdict;
use graph::{Event, Graph};
use rules::{Channel, Rules};
use serde_json::json;
use std::collections::BTreeMap;
use std::io::{BufReader, IsTerminal, Read};
use std::process::{Command, Stdio};

const HELP: &str = "\
pipesort – classify PipeWire playback streams as Game, Chat, Music or App

USAGE:
    pipesort [OPTIONS]

OPTIONS:
    -v, --verbose        Also print the facts behind each decision
        --observe        Only watch; don't move streams into their channel
        --set NAME CHANNEL
                         Always put apps matching NAME in CHANNEL (game, chat,
                         music or app): saves an override to rules.toml and moves
                         matching streams that are playing right now
        --no-log         Don't write observations.jsonl
        --replay FILE    Read saved `pw-dump` output instead of watching live
        --plain          Print one line per event instead of the live table
                         (automatic when output isn't a terminal, e.g. systemd)

KEYS (live table only):
    ↑/↓ or j/k           Select a stream (its details show below the table)
    g / c / m / a        Move it to Game / Chat / Music / App now
    G / C / M / A        Same, and always: saves an override like --set
    v                    Show the facts under every stream (like -v)
    q or Ctrl+C          Quit
    (On --replay only browsing works; nothing is moved.)
    -h, --help           Show this help
";

/// What we remember about each stream we've classified.
struct Seen {
    name: String,
    verdict: Verdict,
    /// The `-v` facts, already formatted.
    facts: String,
    /// Where WirePlumber linked it. `None` until the first link.
    routed: Option<Routed>,
}

struct Routed {
    /// `None` means it went somewhere other than our three sinks.
    channel: Option<Channel>,
    sink_label: String,
}

struct App {
    rules: Rules,
    verbose: bool,
    /// Live table (true) or one log line per event (false).
    table: bool,
    header: String,
    /// Move Game/Chat streams that landed in the wrong channel.
    act: bool,
    log: Option<observations::Observations>,
    /// A BTreeMap keeps streams sorted by id, so table rows don't jump around.
    seen: BTreeMap<u32, Seen>,
    /// Keyboard control is on (live table, not a replay).
    interactive: bool,
    /// The stream picked with ↑/↓.
    selected: Option<u32>,
    /// Result of the last key action, shown on the bottom bar.
    status: String,
    status_is_error: bool,
    /// Reading a saved dump: browsing works, moving doesn't.
    replay: bool,
}

/// Everything the main loop reacts to. One thread reads pw-dump and another
/// reads keys; both send into a channel (`mpsc`), so the main thread can
/// wait on either without async.
enum Msg {
    Batch(serde_json::Value),
    /// pw-dump or the replay file ended; carries the parse error, if any.
    InputEnded(Option<String>),
    Key(char),
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print!("{HELP}");
        return;
    }
    let verbose = args.iter().any(|a| a == "-v" || a == "--verbose");
    let no_log = args.iter().any(|a| a == "--no-log");
    let table = !args.iter().any(|a| a == "--plain") && std::io::stdout().is_terminal();
    let replay = args.iter().position(|a| a == "--replay").map(|i| {
        args.get(i + 1)
            .cloned()
            .unwrap_or_else(|| die("--replay needs a file"))
    });

    if let Some(i) = args.iter().position(|a| a == "--set") {
        let (Some(name), Some(channel)) = (args.get(i + 1), args.get(i + 2)) else {
            die("usage: --set NAME CHANNEL");
        };
        set_override(name, channel);
        return;
    }

    // Never act on replayed dumps: their node ids belong to an old session
    // and could now point at unrelated live streams.
    let act = replay.is_none() && !args.iter().any(|a| a == "--observe");

    let (rules, rules_source) = Rules::load();
    let log = if no_log {
        None
    } else {
        observations::Observations::open()
    };
    let mode = if replay.is_some() {
        "replay"
    } else if act {
        "sorting"
    } else {
        "observe-only"
    };
    let mut header = format!("{mode} · rules: {rules_source}");
    if let Some(l) = &log {
        header.push_str(&format!(" · log: {}", l.path.display()));
    }
    // Show paths under the home directory as ~/… to keep the line short.
    if let Ok(home) = std::env::var("HOME") {
        header = header.replace(&format!("{home}/"), "~/");
    }
    if !table {
        eprintln!("{header}");
    }

    // Where the JSON comes from: a saved file, or a live `pw-dump --monitor`.
    // `Box<dyn Read + Send>` lets both be treated the same way below, and
    // `Send` allows handing it to another thread.
    let mut child = None;
    let input: Box<dyn Read + Send> = match &replay {
        Some(path) => Box::new(
            std::fs::File::open(path).unwrap_or_else(|e| die(&format!("can't open {path}: {e}"))),
        ),
        None => {
            let mut c = Command::new("pw-dump")
                .args(["--monitor", "--no-colors"])
                .stdout(Stdio::piped())
                .spawn()
                .unwrap_or_else(|e| {
                    die(&format!("can't run pw-dump: {e} (is pipewire installed?)"))
                });
            let stdout = c.stdout.take().expect("stdout was piped");
            child = Some(c);
            Box::new(stdout)
        }
    };
    let mut app = App {
        rules,
        verbose,
        table,
        header,
        act,
        log,
        seen: BTreeMap::new(),
        interactive: table && table::enter_raw_mode(),
        selected: None,
        status: String::new(),
        status_is_error: false,
        // Replayed node ids belong to an old session, so keys never move them.
        replay: replay.is_some(),
    };
    if table {
        print!("\x1b[2J"); // clear the screen once; redraws then overwrite in place
        app.redraw();
    } else {
        eprintln!("watching for new playback streams… (Ctrl+C to stop)\n");
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let pw_tx = tx.clone();
    std::thread::spawn(move || {
        // pw-dump prints one JSON array after another; this reads them one at a time.
        let stream = serde_json::Deserializer::from_reader(BufReader::new(input))
            .into_iter::<serde_json::Value>();
        for batch in stream {
            match batch {
                Ok(v) => {
                    if pw_tx.send(Msg::Batch(v)).is_err() {
                        return;
                    }
                }
                Err(e) => {
                    let _ = pw_tx.send(Msg::InputEnded(Some(e.to_string())));
                    return;
                }
            }
        }
        let _ = pw_tx.send(Msg::InputEnded(None));
    });
    if app.interactive {
        std::thread::spawn(move || read_keys(tx));
    } else {
        drop(tx); // so the loop below ends when the pw-dump thread is done
    }

    let mut graph = Graph::default();
    for msg in rx {
        match msg {
            Msg::Batch(batch) => {
                let Some(objects) = batch.as_array() else {
                    continue;
                };
                for obj in objects {
                    for event in graph.apply(obj) {
                        app.handle(event, &graph);
                    }
                }
            }
            Msg::Key('q') => {
                table::restore_terminal();
                if let Some(mut c) = child {
                    let _ = c.kill();
                }
                return;
            }
            Msg::Key(key) => app.on_key(key),
            Msg::InputEnded(Some(e)) => die(&format!("couldn't parse pw-dump output: {e}")),
            Msg::InputEnded(None) => {
                // A finished replay stays on screen for browsing until q.
                if app.replay && app.interactive {
                    app.status = "end of replay".to_string();
                    app.redraw();
                    continue;
                }
                break;
            }
        }
    }

    // Live mode only ends if pw-dump stops (e.g. PipeWire restarted).
    // Exiting non-zero lets systemd restart us.
    if let Some(mut c) = child {
        let status = c.wait();
        die(&format!("pw-dump exited ({status:?})"));
    }
}

impl App {
    fn handle(&mut self, event: Event, graph: &Graph) {
        match event {
            Event::NewPlayback(node) => self.on_new_playback(node, graph),
            Event::Linked { stream, sink } => self.on_linked(stream, sink, graph),
            Event::Removed(id) => {
                if self.seen.remove(&id).is_none() {
                    return; // not one of ours, nothing to redraw
                }
            }
        }
        self.keep_selection();
        self.redraw();
    }

    /// Make sure something is selected while there are streams, so keys
    /// work straight away. If the selected stream went away, select the
    /// next one down (or the last one) instead of losing the cursor.
    fn keep_selection(&mut self) {
        if !self.interactive || self.selected.is_some_and(|id| self.seen.contains_key(&id)) {
            return;
        }
        let from = self.selected.unwrap_or(0);
        // `range(from..)` walks the sorted ids starting at `from`.
        let next = self
            .seen
            .range(from..)
            .next()
            .or(self.seen.iter().next_back());
        self.selected = next.map(|(id, _)| *id);
    }

    fn redraw(&self) {
        if !self.table {
            return;
        }
        let keys: &[(&str, &str)] = if !self.interactive {
            &[("Ctrl+C", "quit")]
        } else if self.replay {
            &[("↑↓", "select"), ("v", "facts"), ("q", "quit")]
        } else {
            &[
                ("↑↓", "select"),
                ("g c m a", "move to Game/Chat/Music/App"),
                ("G C M A", "always"),
                ("v", "facts"),
                ("q", "quit"),
            ]
        };
        let footer = table::Footer {
            status: &self.status,
            status_is_error: self.status_is_error,
            keys,
        };
        table::draw(
            &self.header,
            &self.seen,
            self.verbose,
            self.selected,
            &footer,
        );
    }

    fn on_key(&mut self, key: char) {
        let ids: Vec<u32> = self.seen.keys().copied().collect();
        let pos = self.selected.and_then(|s| ids.iter().position(|&i| i == s));
        match key {
            'j' | 'k' => {
                if ids.is_empty() {
                    return;
                }
                let next = match (pos, key) {
                    (None, _) => 0,
                    (Some(p), 'j') => (p + 1).min(ids.len() - 1),
                    (Some(p), _) => p.saturating_sub(1),
                };
                self.selected = Some(ids[next]);
            }
            'v' => self.verbose = !self.verbose,
            'g' | 'c' | 'm' | 'a' | 'G' | 'C' | 'M' | 'A' => {
                // Assume it fails; `assign` clears this when it works.
                self.status_is_error = true;
                self.assign(key);
            }
            _ => return,
        }
        self.redraw();
    }

    /// Move the selected stream to a channel; uppercase keys also save an
    /// override so it goes there every time (same as `--set`).
    fn assign(&mut self, key: char) {
        let channel = match key.to_ascii_lowercase() {
            'g' => Channel::Game,
            'c' => Channel::Chat,
            'm' => Channel::Music,
            _ => Channel::App,
        };
        let always = key.is_ascii_uppercase();
        if self.replay {
            self.status = "this is a replay: nothing to move".to_string();
            return;
        }
        let Some(id) = self.selected.filter(|id| self.seen.contains_key(id)) else {
            self.status = "nothing to move yet".to_string();
            return;
        };
        let Some(seen) = self.seen.get_mut(&id) else {
            return;
        };

        let mut msg = String::new();
        if always {
            if seen.name == "(unnamed)" {
                self.status = "this stream has no name to make a rule from".to_string();
                return;
            }
            match rules::add_override(&seen.name, channel) {
                Ok(_) => msg = format!("saved rule '{}' → {} · ", seen.name, channel.label()),
                Err(e) => {
                    self.status = format!("couldn't save the rule: {e}");
                    return;
                }
            }
        }
        match move_stream(id, channel) {
            Ok(()) => {
                msg.push_str(&format!("moved #{id} {} → {}", seen.name, channel.label()));
                self.status_is_error = false;
                seen.routed = Some(Routed {
                    channel: Some(channel),
                    sink_label: channel.label().to_string(),
                });
            }
            Err(e) => msg.push_str(&format!("move failed: {e}")),
        }
        if let Some(log) = &mut self.log {
            log.write(json!({
                "kind": "manual",
                "node_id": id,
                "name": seen.name,
                "channel": channel,
                "always": always,
            }));
        }
        self.status = msg;
    }

    fn on_new_playback(&mut self, node: u32, graph: &Graph) {
        // Loopbacks (our Game/Chat/App outputs), filters and echo-cancel all
        // carry node.link-group. They're plumbing, not apps.
        if graph.node_prop(node, "node.link-group").is_some() {
            return;
        }

        // Re-read rules.toml for every new stream so overrides added with
        // `--set` (or by hand) apply without restarting. The file is tiny.
        self.rules = Rules::load().0;
        let facts = facts::gather(graph, node, &facts::gamemode_pids());
        let verdict = classify::classify(&facts, &self.rules);
        let name = facts.display_name().to_string();

        let facts_text = facts_text(&facts);
        if !self.table {
            println!(
                "#{node:<5} {name:<28} → {:<4} ({:?}) {}",
                verdict.channel.label(),
                verdict.confidence,
                verdict.reason
            );
            if self.verbose {
                println!("{facts_text}");
            }
        }

        if let Some(log) = &mut self.log {
            log.write(json!({ "kind": "classified", "facts": facts, "verdict": verdict }));
        }
        self.seen.insert(
            node,
            Seen {
                name,
                verdict,
                facts: facts_text,
                routed: None,
            },
        );
    }

    fn on_linked(&mut self, stream: u32, sink: u32, graph: &Graph) {
        let Some(seen) = self.seen.get_mut(&stream) else {
            return;
        };

        let sink_name = graph.node_prop(sink, "node.name").unwrap_or_default();
        let sink_label = graph
            .node_prop(sink, "node.description")
            .unwrap_or_else(|| sink_name.clone());
        let mut actual = Channel::from_sink_name(&sink_name);
        let would = seen.verdict.channel;

        let mut note = match actual {
            Some(ch) if ch == would => "agrees".to_string(),
            Some(ch) => format!("DIFFERS: WirePlumber chose {}", ch.label()),
            None => "DIFFERS: bypassing the channels (straight to hardware?)".to_string(),
        };

        // Only move streams WirePlumber put in one of *our* sinks. Anything
        // that went elsewhere was sent there on purpose by the app or user.
        // The Low verdict is the "no signals, so App" fallback, the same
        // guess the Lua script makes, so it never overrides a Lua pattern
        // match. Everything else (incl. your overrides, which are High) moves.
        let wrong_channel = actual.is_some() && actual != Some(would);
        let moved_from = actual;
        let mut moved = false;
        if self.act && wrong_channel && seen.verdict.confidence != classify::Confidence::Low {
            match move_stream(stream, would) {
                Ok(()) => {
                    note = format!("MOVED from {} to {}", sink_label, would.label());
                    actual = Some(would);
                    moved = true;
                }
                Err(e) => note = format!("{note} (move failed: {e})"),
            }
        }
        if !self.table {
            println!(
                "#{stream:<5} {:<28}   linked to \"{sink_label}\" – {note}",
                seen.name
            );
        }

        if let Some(log) = &mut self.log {
            log.write(json!({
                "kind": "linked",
                "node_id": stream,
                "name": seen.name,
                "sink": sink_name,
                "actual": moved_from,
                "would": would,
                "moved": moved,
            }));
        }
        seen.routed = Some(Routed {
            channel: actual,
            sink_label,
        });
    }
}

/// `--set NAME CHANNEL`: save an override, then move matching live streams.
fn set_override(name: &str, channel: &str) {
    let channel = match channel.to_lowercase().as_str() {
        "game" => Channel::Game,
        "chat" => Channel::Chat,
        "music" => Channel::Music,
        "app" => Channel::App,
        _ => die(&format!(
            "unknown channel '{channel}' (use game, chat, music or app)"
        )),
    };
    match rules::add_override(name, channel) {
        Ok(path) => println!(
            "saved: '{name}' → {} in {}",
            channel.label(),
            path.display()
        ),
        Err(e) => die(&format!("couldn't save the override: {e}")),
    }

    // One snapshot of the graph (no --monitor) to find what's playing now.
    let output = Command::new("pw-dump")
        .arg("--no-colors")
        .output()
        .unwrap_or_else(|e| die(&format!("can't run pw-dump: {e}")));
    let Ok(serde_json::Value::Array(objects)) = serde_json::from_slice(&output.stdout) else {
        die("couldn't parse pw-dump output");
    };
    let mut graph = Graph::default();
    let mut playing = Vec::new();
    for obj in &objects {
        for event in graph.apply(obj) {
            if let Event::NewPlayback(node) = event {
                playing.push(node);
            }
        }
    }

    let pattern = name.to_lowercase();
    let gamemode = facts::gamemode_pids();
    let mut moved = 0;
    for node in playing {
        if graph.node_prop(node, "node.link-group").is_some() {
            continue; // plumbing, never moved
        }
        let facts = facts::gather(&graph, node, &gamemode);
        if !facts.names().iter().any(|n| n.contains(&pattern)) {
            continue;
        }
        match move_stream(node, channel) {
            Ok(()) => {
                println!(
                    "moved #{node} {} → {}",
                    facts.display_name(),
                    channel.label()
                );
                moved += 1;
            }
            Err(e) => eprintln!("couldn't move #{node} {}: {e}", facts.display_name()),
        }
    }
    if moved == 0 {
        println!("nothing matching '{name}' is playing right now; it applies next time");
    }
}

/// Point a stream at a channel's sink the same way pavucontrol does: set
/// `target.object` in PipeWire's `default` metadata. WirePlumber relinks it
/// and stream-restore remembers, so next launch starts in the right place.
fn move_stream(stream: u32, channel: Channel) -> Result<(), String> {
    let sink = match channel {
        Channel::Game => "sink_game",
        Channel::Chat => "sink_chat",
        Channel::Music => "sink_music",
        Channel::App => "sink_app",
    };
    let status = Command::new("pw-metadata")
        .args(["-n", "default", &stream.to_string(), "target.object", sink])
        .stdout(Stdio::null())
        .status()
        .map_err(|e| format!("can't run pw-metadata: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("pw-metadata exited with {status}"))
    }
}

fn facts_text(f: &facts::Facts) -> String {
    let ancestry: Vec<&str> = f.ancestry.iter().map(|(_, n)| n.as_str()).collect();
    format!(
        "        pid={} exe={} argv0={} binary={}\n\
         \x20       steam={} wine_prefix={} gamemode={} mic={} role={} flatpak={}\n\
         \x20       parents: {}",
        f.pid.map(|p| p.to_string()).unwrap_or("?".into()),
        f.exe,
        facts::basename(&f.argv0),
        f.binary,
        f.steam_app_id.as_deref().unwrap_or("-"),
        f.wine_prefix,
        f.gamemode,
        f.has_mic_stream,
        f.media_role.as_deref().unwrap_or("-"),
        f.flatpak_id.as_deref().unwrap_or("-"),
        ancestry.join(" < "),
    )
}

/// Turn keypresses on the terminal into `Msg::Key`s. Arrow keys arrive as
/// three bytes (ESC [ A/B) and are mapped to k/j; Ctrl+C is a plain byte 3
/// in raw mode and is mapped to q so the terminal gets restored.
fn read_keys(tx: std::sync::mpsc::Sender<Msg>) {
    let Ok(tty) = std::fs::File::open("/dev/tty") else {
        return;
    };
    let mut bytes = BufReader::new(tty).bytes();
    while let Some(Ok(b)) = bytes.next() {
        let key = match b {
            3 => 'q',
            27 => match (bytes.next(), bytes.next()) {
                (Some(Ok(b'[')), Some(Ok(b'A'))) => 'k',
                (Some(Ok(b'[')), Some(Ok(b'B'))) => 'j',
                _ => continue,
            },
            _ => b as char,
        };
        if tx.send(Msg::Key(key)).is_err() {
            return;
        }
    }
}

/// Print an error and exit. The `!` return type means "never returns",
/// which lets it be used where any value is expected.
fn die(msg: &str) -> ! {
    table::restore_terminal();
    eprintln!("error: {msg}");
    std::process::exit(1);
}
