//! The live table shown when running in a terminal.
//!
//! Every time something changes, the whole table is redrawn from the top of
//! the screen. Colours and the cursor moves are plain ANSI escape codes
//! (the `\x1b[...m` strings), so no extra crate is needed.

use crate::Seen;
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

// Column widths (in characters). The reason column gets whatever is left.
const ID_W: usize = 6;
const NAME_W: usize = 26;
const WP_W: usize = 12;
const CLS_W: usize = 11;
const CONF_W: usize = 7;

/// `selected` is highlighted; `footer` lines go under the table.
pub fn draw(
    header: &str,
    rows: &BTreeMap<u32, Seen>,
    verbose: bool,
    selected: Option<u32>,
    footer: &str,
) {
    // Width inside the box: terminal width minus "│ " and " │".
    let inner = terminal_width().saturating_sub(4).max(60);
    let fixed = ID_W + NAME_W + WP_W + CLS_W + CONF_W + 5; // +5 single spaces
    let reason_w = inner.saturating_sub(fixed).max(10);

    let mut out = String::new();
    // \x1b[H moves the cursor to the top-left corner. Each line ends with
    // \x1b[K (clear rest of line) so nothing from the previous frame shows.
    out.push_str("\x1b[H");
    line(
        &mut out,
        &format!("  {BOLD}pipesort{RESET} {DIM}{header}{RESET}"),
    );
    line(&mut out, "");
    let head = format!(
        "{} {} {} {} {} {}",
        pad("ID", ID_W),
        pad("STREAM", NAME_W),
        pad("WIREPLUMBER", WP_W),
        pad("CLASSIFIER", CLS_W),
        pad("CONF", CONF_W),
        pad("REASON", reason_w),
    );
    line(&mut out, &format!("  {BOLD}{head}{RESET}"));

    let mut differs = 0;
    for (id, seen) in rows {
        let wirepl = match &seen.routed {
            None => paint("…", WP_W, DIM),
            Some(r) => match r.channel {
                Some(ch) => paint(ch.label(), WP_W, colour(ch)),
                // Linked to something that isn't one of our three sinks.
                None => paint(&r.sink_label, WP_W, MAGENTA),
            },
        };
        let conf = format!("{:?}", seen.verdict.confidence);
        let conf_style = match seen.verdict.confidence {
            crate::classify::Confidence::High => BOLD,
            crate::classify::Confidence::Medium => "",
            crate::classify::Confidence::Low => DIM,
        };
        let mut lines = vec![format!(
            "{} {} {} {} {} {}",
            paint(
                &format!("#{id}"),
                ID_W,
                if selected == Some(*id) { REVERSE } else { "" }
            ),
            pad(&seen.name, NAME_W),
            wirepl,
            paint(
                seen.verdict.channel.label(),
                CLS_W,
                colour(seen.verdict.channel)
            ),
            paint(&conf, CONF_W, conf_style),
            pad(&seen.verdict.reason, reason_w),
        )];
        if verbose {
            for f in seen.facts.lines() {
                lines.push(paint(f, inner, DIM));
            }
        }

        // Only a linked stream can differ; "…" means we don't know yet.
        let differ = seen
            .routed
            .as_ref()
            .is_some_and(|r| r.channel != Some(seen.verdict.channel));
        if differ {
            differs += 1;
            let bar = "─".repeat(inner + 2);
            line(&mut out, &format!("{RED}┌{bar}┐{RESET}"));
            for l in &lines {
                line(&mut out, &format!("{RED}│{RESET} {l} {RED}│{RESET}"));
            }
            line(&mut out, &format!("{RED}└{bar}┘{RESET}"));
        } else {
            for l in &lines {
                line(&mut out, &format!("  {l}"));
            }
        }
    }

    line(&mut out, "");
    let summary = if differs > 0 {
        format!("{RED}{differs} differ{RESET}")
    } else {
        "all agree".to_string()
    };
    line(&mut out, &format!("  {} streams · {summary}", rows.len()));
    for f in footer.lines() {
        line(&mut out, &format!("  {DIM}{f}{RESET}"));
    }
    out.push_str("\x1b[J"); // clear everything below the table

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

/// Ask `stty` for the terminal size; it prints "rows cols".
/// Falls back to 100 columns if that doesn't work.
fn terminal_width() -> usize {
    stty(&["size"])
        .and_then(|text| text.split_whitespace().nth(1)?.parse().ok())
        .unwrap_or(100)
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
/// ponytail: a panic skips the restore; run `reset` if the terminal acts odd.
pub fn enter_raw_mode() -> bool {
    let Some(saved) = stty(&["-g"]) else {
        return false;
    };
    let _ = SAVED_TTY.set(saved);
    if stty(&["-icanon", "-echo", "-isig", "min", "1"]).is_none() {
        return false;
    }
    print!("\x1b[?25l"); // hide the cursor
    true
}

/// Undo `enter_raw_mode`. Does nothing if it was never entered.
pub fn restore_terminal() {
    if let Some(saved) = SAVED_TTY.get() {
        stty(&[saved]);
        println!("\x1b[?25h"); // show the cursor again
        let _ = std::io::stdout().flush();
    }
}

#[cfg(test)]
mod tests {
    use super::pad;

    #[test]
    fn pad_truncates_and_pads() {
        assert_eq!(pad("abc", 5), "abc  ");
        assert_eq!(pad("abcdef", 4), "abc…");
        assert_eq!(pad("ééé", 3), "ééé");
    }
}
