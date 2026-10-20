//! Working-tree status for the selected agent's repo. `git status` can take
//! a while on a big repo, so it runs on a background thread and the UI reads
//! whatever the last run found.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::model::GitStatus;

/// How long a result is reused before the next background refresh.
const TTL: Duration = Duration::from_secs(10);

struct Entry {
    /// When the last run finished.
    at: Option<Instant>,
    running: bool,
    status: Option<GitStatus>,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The last known status of the repo at `cwd` (None outside git or before
/// the first run finishes), kicking off a refresh when it has gone stale.
pub fn status(cwd: &str) -> Option<GitStatus> {
    if cwd.is_empty() {
        return None;
    }
    let mut c = cache().lock().ok()?;
    let e = c.entry(cwd.to_string()).or_insert(Entry { at: None, running: false, status: None });
    if !e.running && e.at.is_none_or(|t| t.elapsed() >= TTL) {
        e.running = true;
        let dir = cwd.to_string();
        std::thread::spawn(move || {
            let st = run(&dir);
            if let Ok(mut c) = cache().lock() {
                c.insert(dir, Entry { at: Some(Instant::now()), running: false, status: st });
            }
        });
    }
    e.status.clone()
}

fn run(cwd: &str) -> Option<GitStatus> {
    // --no-optional-locks: never contend with the agent's own git commands.
    let out = Command::new("git")
        .args(["--no-optional-locks", "-C", cwd, "status", "--porcelain=v1", "--branch"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(parse(&String::from_utf8_lossy(&out.stdout)))
}

/// `## main...origin/main [ahead 1, behind 2]` then one line per file.
fn parse(txt: &str) -> GitStatus {
    let mut st = GitStatus::default();
    for line in txt.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            let (name, track) = head.split_once(" [").unwrap_or((head, ""));
            let name = name.strip_prefix("No commits yet on ").unwrap_or(name);
            st.branch = name.split("...").next().unwrap_or(name).to_string();
            for part in track.trim_end_matches(']').split(", ") {
                if let Some(n) = part.strip_prefix("ahead ") {
                    st.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix("behind ") {
                    st.behind = n.parse().unwrap_or(0);
                }
            }
        } else if line.starts_with("??") {
            st.untracked += 1;
        } else if !line.is_empty() {
            st.changed += 1;
        }
    }
    st
}
