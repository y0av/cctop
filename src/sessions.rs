//! Discovers running Claude Code sessions from `~/.claude/sessions/<pid>.json`,
//! confirms liveness via the OS process table (cross-platform via `sysinfo`),
//! and joins each to its transcript-derived model and live token burn.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde::Deserialize;

use crate::model::ToolCall;
use crate::pricing;
use crate::procs::{ProcLine, Sampler};
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
    /// Session name. Claude Code derives one from the cwd at startup and
    /// rewrites the file when a session is renamed, so re-reading each tick is
    /// all it takes to track renames.
    name: Option<String>,
    /// "derived" for the auto-generated cwd-based name; anything else means the
    /// name was chosen (by the agent or the user).
    #[serde(rename = "nameSource")]
    name_source: Option<String>,
}

pub struct LiveAgent {
    pub pid: i32,
    pub session_id: String,
    /// What the table shows: a chosen name, else the session title Claude
    /// generated (or you set with /rename), else the cwd-derived name.
    pub name: String,
    /// The name was chosen rather than derived from the cwd — worth highlighting.
    pub named: bool,
    /// Session title from the transcript (`/rename` or the auto title).
    pub title: String,
    /// Short session handle (`project-a4`), kept for the drill-down.
    pub handle: String,
    pub project: String,
    /// Git repo the session runs in, with the folder when it differs
    /// (`hoteleak/wt-mt-0-3` for a worktree or subdir).
    pub repo: String,
    /// Which config dir (account env) this session belongs to.
    pub account: String,
    pub cwd: String,
    pub model: String,
    pub status: String, // "busy" | "idle" | ...
    pub uptime_secs: i64,
    /// Resident memory of the whole process tree (agent + everything it ran).
    pub rss_kb: u64,
    /// Tree CPU in percent of one core.
    pub cpu: f32,
    pub gpu: Option<f32>,
    pub gpu_mem_kb: u64,
    pub nprocs: usize,
    pub detached: usize,
    pub top: Vec<ProcLine>,
    pub burn_tps: f64,
    pub burn_hist: Vec<u64>,
    /// Context in play at the last turn, and the model's window.
    pub ctx_tokens: u64,
    pub ctx_window: u64,
    pub compactions: u32,
    /// The tool call it is executing, else its latest one.
    pub action: Option<ToolCall>,
    pub subagents_running: usize,
    pub ports: Vec<u16>,
    /// Every pid of the tree (see `procs::TreeUsage::pids`).
    pub pids: Vec<i32>,
    #[allow(dead_code)] // "interactive"/"cli"; reserved for a session-kind column
    pub kind: String,
}

/// Read all session files across every config dir (each tagged with a short
/// account label), drop dead ones, and enrich the survivors.
pub fn read(sessions_dirs: &[(PathBuf, String)], store: &Store, now_ms: i64,
            sampler: &Sampler) -> (Vec<LiveAgent>, HashMap<i32, LiveAgent>) {
    let mut out = Vec::new();
    // Session files whose process is gone, kept so orphaned children can be
    // labeled with the session they came from.
    let mut stale = HashMap::new();

    // Parse every session file (across all config dirs) first, then resolve
    // liveness/memory for all PIDs in one OS process-table query (cheaper than
    // one query per PID). PIDs are OS-global, so sessions from different config
    // dirs never collide.
    let mut candidates: Vec<(SessionFile, usize)> = Vec::new();
    for (di, (dir, _)) in sessions_dirs.iter().enumerate() {
        let rd = match std::fs::read_dir(dir) {
            Ok(rd) => rd,
            Err(_) => continue,
        };
        candidates.extend(
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
                .filter_map(|p| std::fs::read_to_string(&p).ok())
                .filter_map(|txt| serde_json::from_str::<SessionFile>(&txt).ok())
                .map(|sf| (sf, di)),
        );
    }

    for (sf, di) in candidates {
        // No such process: a stale registry entry for a dead session.
        let alive = sampler.rss_kb(sf.pid);
        let rss_kb = alive.unwrap_or(0);
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

        let handle = sf.name.unwrap_or_default();
        let chosen = !handle.is_empty() && sf.name_source.as_deref().unwrap_or("derived") != "derived";
        let (title, title_set) = store
            .session_title
            .get(&session_id)
            .map(|(prio, t)| (t.clone(), *prio >= transcripts::TITLE_CUSTOM))
            .unwrap_or_default();
        // Precedence: a name you or the agent chose, then the transcript title,
        // then the cwd-derived handle.
        let (name, named) = if chosen {
            (handle.clone(), true)
        } else if !title.is_empty() {
            (title.clone(), title_set)
        } else {
            (handle.clone(), false)
        };

        let st = store.sessions.get(&session_id);
        let ctx_tokens = st.map(|s| s.ctx_tokens).unwrap_or(0);
        let ctx_window = pricing::context_window(&model, ctx_tokens);
        let since = started / 1000;

        let agent = LiveAgent {
            pid: sf.pid,
            session_id,
            name,
            named,
            title,
            handle,
            repo: repo_label(&cwd),
            project: transcripts::project_name(&cwd).to_string(),
            account: sessions_dirs[di].1.clone(),
            cwd,
            model,
            status: sf.status.unwrap_or_else(|| "?".to_string()),
            uptime_secs,
            rss_kb,
            cpu: 0.0,
            gpu: None,
            gpu_mem_kb: 0,
            nprocs: 1,
            detached: 0,
            top: Vec::new(),
            burn_tps,
            burn_hist,
            ctx_tokens,
            ctx_window,
            compactions: st.map(|s| s.compactions).unwrap_or(0),
            action: st.and_then(|s| s.action()),
            subagents_running: st
                .map(|s| s.subagents(now_secs, since).iter().filter(|a| a.running).count())
                .unwrap_or(0),
            ports: Vec::new(),
            pids: vec![sf.pid],
            kind: sf.kind.unwrap_or_default(),
        };
        if alive.is_some() {
            out.push(agent);
        } else {
            stale.insert(agent.pid, agent);
        }
    }
    // Busy first, then by burn rate, then longest-running.
    out.sort_by(|a, b| {
        let ab = (a.status == "busy") as u8;
        let bb = (b.status == "busy") as u8;
        bb.cmp(&ab)
            .then(b.burn_tps.partial_cmp(&a.burn_tps).unwrap_or(std::cmp::Ordering::Equal))
            .then(b.uptime_secs.cmp(&a.uptime_secs))
    });
    (out, stale)
}

/// `repo` when the cwd is a git repo root, `repo/folder` when it is a linked
/// worktree or a subdirectory, the bare folder name outside git. Cached per
/// cwd: it only changes if a repo is created or moved under a running agent.
pub fn repo_label(cwd: &str) -> String {
    static CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(cwd).cloned()) {
        return v;
    }
    let v = compute_repo_label(cwd);
    if let Ok(mut c) = cache.lock() {
        c.insert(cwd.to_string(), v.clone());
    }
    v
}

fn compute_repo_label(cwd: &str) -> String {
    let folder = transcripts::project_name(cwd).to_string();
    if cwd.is_empty() {
        return folder;
    }
    let cwd_path = Path::new(cwd);
    let mut dir = cwd_path;
    loop {
        let dotgit = dir.join(".git");
        let repo = if dotgit.is_dir() {
            Some(leaf(dir))
        } else if dotgit.is_file() {
            // Linked worktree: `gitdir: /main/.git/worktrees/<name>`.
            Some(
                std::fs::read_to_string(&dotgit)
                    .ok()
                    .and_then(|t| t.trim().strip_prefix("gitdir:").map(|g| PathBuf::from(g.trim())))
                    .and_then(|g| {
                        g.ancestors()
                            .find(|a| a.file_name().map(|n| n == ".git").unwrap_or(false))
                            .and_then(Path::parent)
                            .map(leaf)
                    })
                    .unwrap_or_else(|| leaf(dir)),
            )
        } else {
            None
        };
        if let Some(repo) = repo {
            // repo, then the worktree dir when it isn't named like the repo,
            // then the subfolder the session sits in.
            let mut parts = vec![repo.clone()];
            let root = leaf(dir);
            if root != repo {
                parts.push(root);
            }
            if dir != cwd_path {
                parts.push(folder);
            }
            return parts.join("/");
        }
        match dir.parent() {
            Some(p) => dir = p,
            None => return folder,
        }
    }
}

fn leaf(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
}
