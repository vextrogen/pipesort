//! Append-only log of every decision, one JSON object per line, at
//! `~/.local/state/pipesort/observations.jsonl`.
//!
//! This is what you review after a few days of normal use, e.g. with jq.

use crate::rules::home;
use serde_json::Value;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct Observations {
    file: File,
    pub path: PathBuf,
}

impl Observations {
    pub fn open() -> Option<Observations> {
        let dir = std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"))
            .join("pipesort");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            eprintln!(
                "warning: can't create {}: {e} (not logging to file)",
                dir.display()
            );
            return None;
        }
        let path = dir.join("observations.jsonl");
        match OpenOptions::new().create(true).append(true).open(&path) {
            Ok(file) => Some(Observations { file, path }),
            Err(e) => {
                eprintln!(
                    "warning: can't open {}: {e} (not logging to file)",
                    path.display()
                );
                None
            }
        }
    }

    pub fn write(&mut self, mut record: Value) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some(obj) = record.as_object_mut() {
            obj.insert("ts".into(), ts.into());
        }
        // A failed write isn't worth crashing over.
        let _ = writeln!(self.file, "{record}");
    }
}
