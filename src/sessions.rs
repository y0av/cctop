//! Discovers running Claude Code sessions from `~/.claude/sessions/<pid>.json`,
//! confirms liveness via the OS process table (cross-platform via `sysinfo`),
//! and joins each to its transcript-derived model and live token burn.

use std::path::PathBuf;

use serde::Deserialize;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};

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

/// Read all session files across every config dir, drop dead ones, and enrich
/// the survivors.
pub fn read(sessions_dirs: &[PathBuf], store: &Store, now_ms: i64) -> Vec<LiveAgent> {
    let mut out = Vec::new();

    // Parse every session file (across all config dirs) first, then resolve
    // liveness/memory for all PIDs in one OS process-table query (cheaper than
    // one query per PID). PIDs are OS-global, so sessions from different config
    // dirs never collide.
    let mut candidates: Vec<SessionFile> = Vec::new();
    for dir in sessions_dirs {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        candidates.extend(
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .filter_map(|p| std::fs::read_to_string(&p).ok())
                .filter_map(|txt| serde_json::from_str::<SessionFile>(&txt).ok()),
        );
    }

    let pids: Vec<Pid> = candidates.iter().map(|c| Pid::from_u32(c.pid as u32)).collect();
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing().with_memory()),
    );
    sys.refresh_processes(ProcessesToUpdate::Some(&pids), true);

    for sf in candidates {
        let Some(rss_kb) = process_info(&sys, sf.pid) else {
            continue; // no such process — stale registry entry for a dead process
        };
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
            rss_kb,
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

/// Look up a live PID in the refreshed process table, returning its resident
/// set size in KiB (the UI's unit; sysinfo reports bytes).
///
/// Returns `None` if no such process exists — a stale registry entry for a dead
/// session. We deliberately don't check the executable name: Claude Code may be
/// launched under a wrapper (e.g. a `node`-spawned process) whose name isn't
/// `claude`, and the session file is authored by Claude itself, so process
/// existence is a sufficient liveness signal. Works on Linux, macOS and Windows.
fn process_info(sys: &System, pid: i32) -> Option<u64> {
    if pid < 0 {
        return None;
    }
    let proc_ = sys.process(Pid::from_u32(pid as u32))?;
    Some(proc_.memory() / 1024)
}
