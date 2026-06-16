//! Discovers running Claude Code sessions from `~/.claude/sessions/<pid>.json`,
//! confirms liveness via `/proc`, and joins each to its transcript-derived
//! model and live token burn.

use std::path::Path;

use serde::Deserialize;

use crate::transcripts::{self, Store};

#[derive(Deserialize)]
struct SessionFile {
    pid: i32,
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    cwd: Option<String>,
    #[serde(rename = "startedAt")]
    started_at: Option<i64>, // ms epoch
    status: Option<String>,
    kind: Option<String>,
}

pub struct LiveAgent {
    pub pid: i32,
    #[allow(dead_code)] // joined on during enrichment; kept for future drill-down
    pub session_id: String,
    pub project: String,
    pub model: String,
    pub status: String, // "busy" | "idle" | ...
    pub uptime_secs: i64,
    pub rss_kb: u64,
    pub burn_tps: f64,
    pub burn_hist: Vec<u64>,
    #[allow(dead_code)] // "interactive"/"cli"; reserved for a session-kind column
    pub kind: String,
}

/// Read all session files, drop dead ones, and enrich the survivors.
pub fn read(sessions_dir: &Path, store: &Store, now_ms: i64) -> Vec<LiveAgent> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(sessions_dir) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let txt = match std::fs::read_to_string(&p) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let sf: SessionFile = match serde_json::from_str(&txt) {
            Ok(s) => s,
            Err(_) => continue,
        };
        if !is_claude_alive(sf.pid) {
            continue; // stale registry entry for a dead process
        }
        let session_id = sf.session_id.unwrap_or_default();
        let cwd = sf.cwd.unwrap_or_default();
        let started = sf.started_at.unwrap_or(now_ms);
        let uptime_secs = ((now_ms - started) / 1000).max(0);

        let model = store
            .session_model
            .get(&session_id)
            .map(|(_, m)| m.clone())
            .unwrap_or_else(|| "—".to_string());

        let now_secs = now_ms / 1000;
        let (burn_tps, burn_hist) = if session_id.is_empty() {
            (0.0, vec![0u64; 8])
        } else {
            store.session_burn(&session_id, now_secs, 60, 8)
        };

        out.push(LiveAgent {
            pid: sf.pid,
            session_id,
            project: transcripts::project_name(&cwd).to_string(),
            model,
            status: sf.status.unwrap_or_else(|| "?".to_string()),
            uptime_secs,
            rss_kb: read_rss_kb(sf.pid).unwrap_or(0),
            burn_tps,
            burn_hist,
            kind: sf.kind.unwrap_or_default(),
        });
    }
    // Busy first, then by burn rate, then longest-running.
    out.sort_by(|a, b| {
        let ab = (a.status == "busy") as u8;
        let bb = (b.status == "busy") as u8;
        bb.cmp(&ab)
            .then(b.burn_tps.partial_cmp(&a.burn_tps).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.uptime_secs.cmp(&a.uptime_secs))
    });
    out
}

/// True if `/proc/<pid>/comm` reports a live `claude` process.
fn is_claude_alive(pid: i32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/comm")) {
        Ok(s) => s.trim() == "claude",
        Err(_) => false,
    }
}

/// Resident set size in KiB from `/proc/<pid>/statm` (field 2 = resident pages).
fn read_rss_kb(pid: i32) -> Option<u64> {
    let s = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    let resident_pages: u64 = s.split_whitespace().nth(1)?.parse().ok()?;
    // Linux page size is 4 KiB on this platform.
    Some(resident_pages * 4)
}
