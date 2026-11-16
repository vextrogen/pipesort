//! The live table shown when running in a terminal.
//!
//! Every time something changes, the whole screen is redrawn from the top.
//! Colours and cursor moves are plain ANSI escape codes (the `\x1b[...m`
//! strings), so no extra crate is needed.
//!
//! Layout, top to bottom: title bar, stream table, details of the selected
//! stream, and a status + key bar pinned to the bottom of the screen
//! (the htop / nano convention).

use crate::Seen;
use crate::classify::Confidence;
use crate::rules::Channel;
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::OnceLock;

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const RED: &str = "\x1b[1;31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const BLUE: &str = "\x1b[34m";
const MAGENTA: &str = "\x1b[35m";
const CYAN: &str = "\x1b[36m";
const REVERSE: &str = "\x1b[7m";

// Fixed column widths (in characters). STREAM and REASON share the rest.
const ID_W: usize = 6;
const NOW_W: usize = 7;
const DET_W: usize = 8;
const CONF_W: usize = 4;

/// What goes on the bottom bar.
pub struct Footer<'a> {
    /// Result of the last key action; empty for none.
    pub status: &'a str,
    pub status_is_error: bool,
    /// (key, what it does) pairs, e.g. ("q", "quit").
    pub keys: &'a [(&'a str, &'a str)],
}

/// `header` is "mode · details"; the mode part is shown as a coloured badge.
pub fn draw(
    header: &str,
    rows: &BTreeMap<u32, Seen>,
    verbose: bool,
    selected: Option<u32>,
    footer: &Footer,
) {
    let (height, width) = terminal_size();
    let width = width.max(60);
    // Two spaces of margin on the left (the selection marker goes there).
    let body = width - 2;
    let fixed = ID_W + NOW_W + 2 + DET_W + CONF_W + 5; // +2 for "✓ ", +5 spaces
    let flexible = body.saturating_sub(fixed);
    let name_w = (flexible * 2 / 5).clamp(12, 30);
    let reason_w = flexible.saturating_sub(name_w).max(8);

    let mut out = String::new();
    // \x1b[H moves the cursor to the top-left corner. Each line ends with
    // \x1b[K (clear rest of line) so nothing from the previous frame shows.
    out.push_str("\x1b[H");

    let (mode, details) = header.split_once(" · ").unwrap_or((header, ""));
    let badge = match mode {
        "sorting" => GREEN,
        "replay" => MAGENTA,
        _ => YELLOW,
    };
    line(
        &mut out,
        &format!("  {BOLD}pipesort{RESET}  {badge}{REVERSE} {mode} {RESET}  {DIM}{details}{RESET}"),
    );
    line(&mut out, "");

    let head = format!(
        "{} {} {} {} {} {}",
        pad("ID", ID_W),
        pad("STREAM", name_w),
        pad("NOW IN", NOW_W + 2),
        pad("DETECTED", DET_W),
        pad("CONF", CONF_W),
        pad("REASON", reason_w),
    );
    line(&mut out, &format!("  {BOLD}{head}{RESET}"));

    if rows.is_empty() {
        line(&mut out, "");
        line(
            &mut out,
            &format!("  {DIM}Nothing playing yet. Start some audio and it shows up here.{RESET}"),
        );
    }

    let mut differs = 0;
    for (id, seen) in rows {
        let (now, mark) = match &seen.routed {
            None => (paint("…", NOW_W, DIM), paint(" ", 1, "")),
            Some(r) => {
                let now = match r.channel {
                    Some(ch) => paint(ch.label(), NOW_W, colour(ch)),
                    // Linked to something that isn't one of our four sinks.
                    None => paint(&r.sink_label, NOW_W, MAGENTA),
                };
                if r.channel == Some(seen.verdict.channel) {
                    (now, paint("✓", 1, GREEN))
                } else {
                    differs += 1;
                    (now, paint("≠", 1, RED))
                }
            }
        };
        let ch = seen.verdict.channel;
        let row = format!(
            "{} {} {} {} {} {} {}",
            pad(&format!("#{id}"), ID_W),
            pad(&seen.name, name_w),
            now,
            mark,
            paint(ch.label(), DET_W, colour(ch)),
            paint(conf_word(seen.verdict.confidence), CONF_W, conf_style(seen)),
            pad(&seen.verdict.reason, reason_w),
        );
        if selected == Some(*id) {
            // Every cell ends in RESET, which would also end the highlight,
            // so turn reverse video back on after each one.
            let row = row.replace(RESET, &format!("{RESET}{REVERSE}"));
            line(&mut out, &format!("{BOLD}▶{RESET} {REVERSE}{row}{RESET}"));
        } else {
            line(&mut out, &format!("  {row}"));
        }
        if verbose {
            for f in seen.facts.lines() {
                line(&mut out, &paint(f, body, DIM));
            }
        }
    }

    // Details of the selected stream: the full reason (the table cuts it
    // short) and the facts behind it.
    if let Some((id, seen)) = selected.and_then(|id| rows.get_key_value(&id)) {
        line(&mut out, "");
        let title = format!(" #{id} {} ", seen.name);
        let rule = "─".repeat(body.saturating_sub(2 + title.chars().count()));
        line(
            &mut out,
            &format!("  {DIM}──{RESET}{BOLD}{title}{RESET}{DIM}{rule}{RESET}"),
        );
        let ch = seen.verdict.channel;
        let place = match &seen.routed {
            None => "not linked yet".to_string(),
            Some(r) => format!("now in \"{}\"", r.sink_label),
        };
        line(
            &mut out,
            &format!(
                "  {}{}{RESET} · {} confidence · {place}",
                colour(ch),
                ch.label(),
                format!("{:?}", seen.verdict.confidence).to_lowercase()
            ),
        );
        for l in wrap(&seen.verdict.reason, body - 2) {
            line(&mut out, &format!("  {l}"));
        }
        for f in seen.facts.lines() {
            line(
                &mut out,
                &format!("  {}", paint(f.trim_start(), body - 2, DIM)),
            );
        }
    }
    out.push_str("\x1b[J"); // clear everything below

    // Bottom bar: summary + status on one line, keys on the last line.
    // \x1b[ROW;1H jumps to that row. The last line has no "\n" so the
    // screen doesn't scroll up.
    let summary = if rows.is_empty() {
        String::new()
    } else if differs > 0 {
        format!(
            "{} streams · {RED}{differs} not in their channel{RESET}",
            rows.len()
        )
    } else {
        format!(
            "{} streams · {GREEN}all in their channel{RESET}",
            rows.len()
        )
    };
    let status_style = if footer.status_is_error { RED } else { GREEN };
    out.push_str(&format!("\x1b[{};1H", height.saturating_sub(1).max(1)));
    line(
        &mut out,
        &format!("  {summary}   {status_style}{}{RESET}", footer.status),
    );
    let mut keys = String::from(" ");
    for (key, action) in footer.keys {
        keys.push_str(&format!(" {BOLD}{key}{RESET} {DIM}{action}{RESET}  "));
    }
    out.push_str(&keys);
    out.push_str("\x1b[K");

    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(out.as_bytes());
    let _ = stdout.flush();
}

fn colour(ch: Channel) -> &'static str {
    match ch {
        Channel::Game => GREEN,
        Channel::Chat => CYAN,
        Channel::Music => BLUE,
        Channel::App => YELLOW,
    }
}

fn conf_word(c: Confidence) -> &'static str {
    match c {
        Confidence::High => "high",
        Confidence::Medium => "med",
        Confidence::Low => "low",
    }
}

fn conf_style(seen: &Seen) -> &'static str {
    match seen.verdict.confidence {
        Confidence::High => BOLD,
        Confidence::Medium => "",
        Confidence::Low => DIM,
    }
}

fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push_str("\x1b[K\n");
}

/// Cut or space-pad `text` to exactly `width` characters, then colour it.
/// Padding happens before colouring because escape codes take up bytes
/// but no space on screen, which would throw the alignment off.
fn paint(text: &str, width: usize, style: &str) -> String {
    format!("{style}{}{RESET}", pad(text, width))
}

fn pad(text: &str, width: usize) -> String {
    let count = text.chars().count();
    if count > width {
        let cut: String = text.chars().take(width.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        format!("{text}{}", " ".repeat(width - count))
    }
}

/// Split `text` into lines of at most `width` characters, breaking at
/// spaces. A single word longer than `width` is cut by `pad` later.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let needed = current.chars().count() + 1 + word.chars().count();
        if !current.is_empty() && needed > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines.into_iter().map(|l| pad(&l, width)).collect()
}

/// Ask `stty` for the terminal size; it prints "rows cols".
/// Falls back to 24×100 if that doesn't work.
fn terminal_size() -> (usize, usize) {
    let size = stty(&["size"]).and_then(|text| {
        let mut parts = text.split_whitespace();
        let rows = parts.next()?.parse().ok()?;
        let cols = parts.next()?.parse().ok()?;
        Some((rows, cols))
    });
    size.unwrap_or((24, 100))
}

/// Run `stty` on the real terminal and return what it printed.
fn stty(args: &[&str]) -> Option<String> {
    let tty = std::fs::File::open("/dev/tty").ok()?;
    let output = std::process::Command::new("stty")
        .args(args)
        .stdin(tty)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// The terminal settings from before raw mode, to put back on exit.
/// `OnceLock` is a global that can be set once, safely from any thread.
static SAVED_TTY: OnceLock<String> = OnceLock::new();

/// Raw mode: keys arrive one at a time without Enter, aren't echoed, and
/// Ctrl+C becomes a normal key so we can restore the terminal before quitting.
/// Also switches to the alternate screen (like htop or less), so quitting
/// gives back the shell exactly as it was.
/// ponytail: a panic skips the restore; run `reset` if the terminal acts odd.
pub fn enter_raw_mode() -> bool {
    let Some(saved) = stty(&["-g"]) else {
        return false;
    };
    let _ = SAVED_TTY.set(saved);
    if stty(&["-icanon", "-echo", "-isig", "min", "1"]).is_none() {
        return false;
    }
    print!("\x1b[?1049h\x1b[?25l"); // alternate screen, hide the cursor
    true
}

/// Undo `enter_raw_mode`. Does nothing if it was never entered.
pub fn restore_terminal() {
    if let Some(saved) = SAVED_TTY.get() {
        stty(&[saved]);
        print!("\x1b[?25h\x1b[?1049l"); // show the cursor, leave alternate screen
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::{pad, wrap};

    #[test]
    fn pad_truncates_and_pads() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 4), "abc…");
        assert_eq!(pad("ééé", 3), "ééé");
    }

    #[test]
    fn wrap_breaks_at_spaces() {
        assert_eq!(wrap("aa bb cc", 5), vec!["aa bb", "cc   "]);
        assert_eq!(wrap("", 5), Vec::<String>::new());
        assert_eq!(wrap("toolongword x", 4), vec!["too…", "x   "]);
    }
}
