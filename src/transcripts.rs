//! Reads Claude Code session transcripts (`~/.claude/projects/*/*.jsonl`),
//! incrementally (only new bytes per refresh), de-duplicates repeated usage
//! lines, and aggregates token/cost totals.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use chrono::DateTime;
use serde_json::Value;

use crate::model::{Series, SubAgentView, ToolCall, Tokens};
use crate::pricing;

/// One de-duplicated assistant turn.
pub struct Record {
    pub ts: i64, // unix seconds
    pub model: String,
    pub cwd: String,
    pub session: String,
    pub sidechain: bool,
    pub tok: Tokens,
    pub cost: f64,
}

/// Live state of one session, from the tail of its transcript: what the
/// context weighs, what it is running, which subagents it spawned.
#[derive(Default)]
pub struct SessionState {
    /// Tokens in play at the latest main-chain turn: everything the model
    /// read (input + cache reads and writes) plus what it wrote.
    pub ctx_tokens: u64,
    ctx_ts: i64,
    /// Auto or manual `/compact` runs.
    pub compactions: u32,
    last_tool: Option<ToolCall>,
    /// Main-chain tool calls still waiting for their result, by tool_use id.
    pending: HashMap<String, ToolCall>,
    subagents: HashMap<String, SubAgent>,
}

struct SubAgent {
    kind: String,
    desc: String,
    /// The parent's Agent tool call that started it; pending while it runs
    /// in the foreground.
    tool_use_id: String,
    tokens: u64,
    last_ts: i64,
}

/// A subagent counts as running while its parent waits on it, or while its
/// own transcript is still moving (background agents return at once).
const SUBAGENT_LIVE_SECS: i64 = 60;

impl SessionState {
    /// The call the agent is executing, else its latest one.
    pub fn action(&self) -> Option<ToolCall> {
        self.pending
            .values()
            .max_by_key(|c| c.ts)
            .or(self.last_tool.as_ref())
            .cloned()
    }

    /// Subagents touched since `since`, running first, then most recent.
    pub fn subagents(&self, now: i64, since: i64) -> Vec<SubAgentView> {
        let mut v: Vec<(&SubAgent, bool)> = self
            .subagents
            .values()
            .filter(|a| a.last_ts >= since)
            .map(|a| {
                let running = self.pending.contains_key(&a.tool_use_id)
                    || now - a.last_ts < SUBAGENT_LIVE_SECS;
                (a, running)
            })
            .collect();
        v.sort_by(|x, y| y.1.cmp(&x.1).then(y.0.last_ts.cmp(&x.0.last_ts)));
        v.into_iter()
            .map(|(a, running)| SubAgentView {
                kind: a.kind.clone(),
                desc: a.desc.clone(),
                tokens: a.tokens,
                running,
            })
            .collect()
    }
}

/// What a subagent's `.meta.json` says about it.
struct AgentMeta {
    kind: String,
    desc: String,
    tool_use_id: String,
}

struct FileState {
    offset: u64,
    /// Last modification seen; files untouched for `HOT_SECS` drop out of the
    /// per-tick check.
    mtime: SystemTime,
}

/// Files modified this recently are re-checked every tick.
const HOT_SECS: u64 = 600;
/// Every this often, stat every file anyway: the safety net for a cold file
/// that starts growing without its session being live (rare).
const FULL_SWEEP_SECS: u64 = 60;

pub struct Store {
    roots: Vec<PathBuf>,
    files: HashMap<PathBuf, FileState>,
    /// Directory -> mtime. A new file (new session, new subagent) changes its
    /// directory's mtime, so watching ~hundreds of dirs finds new transcripts
    /// without stat'ing tens of thousands of files.
    dirs: HashMap<PathBuf, SystemTime>,
    /// Files re-checked every tick: recently modified ones.
    hot: HashSet<PathBuf>,
    /// Transcript files by stem, so a live (e.g. resumed) session's cold file
    /// is checked every tick too.
    by_stem: HashMap<String, Vec<PathBuf>>,
    last_sweep: Option<Instant>,
    seen: HashSet<u64>,
    pub records: Vec<Record>,
    /// session id -> (latest ts, model) for joining live agents to their model.
    pub session_model: HashMap<String, (i64, String)>,
    /// session id -> (priority, title). A title you set beats the generated
    /// one; within a priority the latest line wins.
    pub session_title: HashMap<String, (u8, String)>,
    pub sessions: HashMap<String, SessionState>,
    /// Subagent id -> its meta file, read once when the transcript appears.
    agent_meta: HashMap<String, AgentMeta>,
}

/// Title priorities: Claude's generated title < an agent name < `/rename`.
pub const TITLE_AI: u8 = 1;
pub const TITLE_AGENT: u8 = 2;
pub const TITLE_CUSTOM: u8 = 3;

impl Store {
    pub fn new(roots: Vec<PathBuf>) -> Self {
        Store {
            roots,
            files: HashMap::new(),
            dirs: HashMap::new(),
            hot: HashSet::new(),
            by_stem: HashMap::new(),
            last_sweep: None,
            seen: HashSet::new(),
            records: Vec::new(),
            session_model: HashMap::new(),
            session_title: HashMap::new(),
            sessions: HashMap::new(),
            agent_meta: HashMap::new(),
        }
    }

    /// Ingest newly-appended bytes. Per-file offsets and the dedup set are
    /// keyed globally, so overlapping roots (or a file reachable from two
    /// roots) are counted exactly once.
    ///
    /// Cost: a full sweep (walk + stat every file) runs on the first call and
    /// then once a minute. Every other tick stats only the directories, the
    /// recently modified files, and the files of `live_sessions`.
    pub fn refresh(&mut self, live_sessions: &[String]) {
        let sweep = self
            .last_sweep
            .map(|t| t.elapsed().as_secs() >= FULL_SWEEP_SECS)
            .unwrap_or(true);
        if sweep {
            self.full_sweep();
            self.last_sweep = Some(Instant::now());
            return;
        }

        // New files: re-list only directories whose mtime moved.
        let changed: Vec<PathBuf> = self
            .dirs
            .iter()
            .filter(|(d, m)| std::fs::metadata(d).and_then(|md| md.modified()).ok() != Some(**m))
            .map(|(d, _)| d.clone())
            .collect();
        for d in changed {
            let mut found = Vec::new();
            self.list_dir(&d, &mut found, false);
            for p in found {
                if !self.files.contains_key(&p) {
                    self.hot.insert(p);
                }
            }
        }

        let mut check: Vec<PathBuf> = self.hot.iter().cloned().collect();
        for sid in live_sessions {
            if let Some(ps) = self.by_stem.get(sid) {
                check.extend(ps.iter().filter(|p| !self.hot.contains(*p)).cloned());
            }
        }
        let now = SystemTime::now();
        for path in check {
            match std::fs::metadata(&path) {
                Ok(md) => {
                    let mtime = md.modified().unwrap_or(now);
                    // A transient read error (file rotated mid-read) just means
                    // we retry next refresh; never fatal.
                    let _ = self.ingest_file(&path, md.len(), mtime);
                    let cold = now.duration_since(mtime).map(|d| d.as_secs() > HOT_SECS).unwrap_or(false);
                    if cold {
                        self.hot.remove(&path);
                    } else {
                        self.hot.insert(path);
                    }
                }
                Err(_) => {
                    self.hot.remove(&path);
                }
            }
        }
    }

    /// Walk every root, stat every transcript, ingest what grew, and rebuild
    /// the directory and hot-file sets.
    fn full_sweep(&mut self) {
        let mut paths = Vec::new();
        self.dirs.clear();
        for root in self.roots.clone() {
            self.list_dir(&root, &mut paths, true);
        }
        let now = SystemTime::now();
        self.hot.clear();
        for path in paths {
            let Ok(md) = std::fs::metadata(&path) else { continue };
            let mtime = md.modified().unwrap_or(now);
            let _ = self.ingest_file(&path, md.len(), mtime);
            if now.duration_since(mtime).map(|d| d.as_secs() <= HOT_SECS).unwrap_or(true) {
                self.hot.insert(path);
            }
        }
    }

    /// List `dir` (recursing into subdirectories when `recurse`, or when a
    /// subdirectory is new to us), recording directory mtimes and pushing every
    /// `.jsonl` path. Uses the entry's file type, which costs no extra stat.
    fn list_dir(&mut self, dir: &Path, out: &mut Vec<PathBuf>, recurse: bool) {
        let Ok(md) = std::fs::metadata(dir) else {
            self.dirs.remove(dir);
            return;
        };
        if let Ok(m) = md.modified() {
            self.dirs.insert(dir.to_path_buf(), m);
        }
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let p = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            let is_dir = if ft.is_symlink() { p.is_dir() } else { ft.is_dir() };
            if is_dir {
                if recurse || !self.dirs.contains_key(&p) {
                    self.list_dir(&p, out, true);
                }
            } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                    let v = self.by_stem.entry(stem.to_string()).or_default();
                    if !v.contains(&p) {
                        v.push(p.clone());
                    }
                }
                out.push(p);
            }
        }
    }

    fn ingest_file(&mut self, path: &Path, len: u64, mtime: SystemTime) -> std::io::Result<()> {
        if !self.files.contains_key(path) {
            self.read_agent_meta(path);
        }
        let state = self
            .files
            .entry(path.to_path_buf())
            .or_insert(FileState { offset: 0, mtime });
        state.mtime = mtime;
        if len < state.offset {
            // File was truncated/rotated — start over.
            state.offset = 0;
        }
        if len == state.offset {
            return Ok(());
        }
        let mut f = File::open(path)?;
        f.seek(SeekFrom::Start(state.offset))?;
        let mut reader = BufReader::new(f);
        let mut consumed = state.offset;
        let mut line = String::new();
        loop {
            line.clear();
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break;
            }
            if !line.ends_with('\n') {
                // Incomplete trailing line (file is mid-write) — don't consume;
                // we'll re-read it once it's flushed.
                break;
            }
            consumed += n as u64;
            self.ingest_line(&line);
        }
        // Re-borrow: `reader` held `f`; update offset after the loop.
        self.files
            .entry(path.to_path_buf())
            .or_insert(FileState { offset: 0, mtime })
            .offset = consumed;
        Ok(())
    }

    /// Subagent transcripts live at `<session>/subagents/agent-<id>.jsonl`
    /// next to an `agent-<id>.meta.json` naming the agent type and task.
    fn read_agent_meta(&mut self, path: &Path) {
        let in_subagents = path.parent().and_then(Path::file_name).is_some_and(|n| n == "subagents");
        let Some(id) = path.file_stem().and_then(|s| s.to_str()).and_then(|s| s.strip_prefix("agent-")) else {
            return;
        };
        if !in_subagents {
            return;
        }
        let Some(v) = std::fs::read_to_string(path.with_extension("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            return;
        };
        let get = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        self.agent_meta.insert(
            id.to_string(),
            AgentMeta { kind: get("agentType"), desc: get("description"), tool_use_id: get("toolUseId") },
        );
    }

    /// Insert into the dedup set; false when the key was already counted.
    fn first_sight(&mut self, key: &str) -> bool {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        self.seen.insert(hasher.finish())
    }

    fn ingest_line(&mut self, line: &str) {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return,
        };
        let kind = v.get("type").and_then(Value::as_str).unwrap_or("");
        let title = match kind {
            "custom-title" => Some((TITLE_CUSTOM, "customTitle")),
            "agent-name" => Some((TITLE_AGENT, "agentName")),
            "ai-title" => Some((TITLE_AI, "aiTitle")),
            _ => None,
        };
        if let Some((prio, field)) = title {
            let session = v.get("sessionId").and_then(Value::as_str).unwrap_or("");
            let text = v.get(field).and_then(Value::as_str).unwrap_or("").trim();
            if !session.is_empty() && !text.is_empty() {
                let e = self.session_title.entry(session.to_string()).or_insert((0, String::new()));
                if prio >= e.0 {
                    *e = (prio, text.to_string());
                }
            }
            return;
        }
        let session = v.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
        let sidechain = v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
            .unwrap_or(0);
        let content = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array);
        match kind {
            "system" => {
                let compact = v.get("subtype").and_then(Value::as_str) == Some("compact_boundary");
                let uuid = v.get("uuid").and_then(Value::as_str).unwrap_or("");
                if compact && !sidechain && !session.is_empty() && self.first_sight(uuid) {
                    self.sessions.entry(session).or_default().compactions += 1;
                }
                return;
            }
            "user" => {
                // Tool results close the calls they answer.
                if sidechain || session.is_empty() {
                    return;
                }
                let Some(blocks) = content else { return };
                let Some(st) = self.sessions.get_mut(&session) else { return };
                for b in blocks {
                    if b.get("type").and_then(Value::as_str) == Some("tool_result") {
                        if let Some(id) = b.get("tool_use_id").and_then(Value::as_str) {
                            st.pending.remove(id);
                        }
                    }
                }
                return;
            }
            "assistant" => {}
            _ => return,
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => return,
        };
        let agent_id = v.get("agentId").and_then(Value::as_str).filter(|_| sidechain);

        // Tool calls come before the dedup below: Claude Code writes one line
        // per content block, all sharing the message id.
        if !session.is_empty() {
            let calls: Vec<(String, ToolCall)> = content
                .into_iter()
                .flatten()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
                .map(|b| {
                    let id = b.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    let name = b.get("name").and_then(Value::as_str).unwrap_or("?");
                    let (tool, arg) = describe_tool(name, b.get("input").unwrap_or(&Value::Null));
                    (id, ToolCall { tool, arg, ts, pending: true })
                })
                .collect();
            let st = self.sessions.entry(session.clone()).or_default();
            match agent_id {
                Some(aid) => {
                    let meta = self.agent_meta.get(aid);
                    let sa = st.subagents.entry(aid.to_string()).or_insert_with(|| SubAgent {
                        kind: meta.map(|m| m.kind.clone()).unwrap_or_default(),
                        desc: meta.map(|m| m.desc.clone()).unwrap_or_default(),
                        tool_use_id: meta.map(|m| m.tool_use_id.clone()).unwrap_or_default(),
                        tokens: 0,
                        last_ts: 0,
                    });
                    sa.last_ts = sa.last_ts.max(ts);
                }
                None if !sidechain => {
                    for (id, call) in calls {
                        if st.last_tool.as_ref().is_none_or(|t| ts >= t.ts) {
                            st.last_tool = Some(ToolCall { pending: false, ..call.clone() });
                        }
                        if !id.is_empty() {
                            st.pending.insert(id, call);
                        }
                    }
                    // An interrupted run can leave calls unanswered; keep
                    // the set from growing without bound.
                    while st.pending.len() > 32 {
                        let oldest = st.pending.iter().min_by_key(|(_, c)| c.ts).map(|(k, _)| k.clone());
                        match oldest {
                            Some(k) => st.pending.remove(&k),
                            None => break,
                        };
                    }
                }
                None => {}
            }
        }

        // Dedup on (message.id, requestId): the same turn is written multiple
        // times across streaming/resume. Fall back to the line uuid if absent.
        let msg_id = msg.get("id").and_then(Value::as_str).unwrap_or("");
        let req_id = v.get("requestId").and_then(Value::as_str).unwrap_or("");
        let key = if msg_id.is_empty() && req_id.is_empty() {
            v.get("uuid").and_then(Value::as_str).unwrap_or("").to_string()
        } else {
            format!("{msg_id}|{req_id}")
        };
        if key.is_empty() {
            return;
        }
        if !self.first_sight(&key) {
            return; // already counted
        }

        let usage = match msg.get("usage") {
            Some(u) => u,
            None => return,
        };
        let g = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        let cc = usage.get("cache_creation");
        let ccg = |k: &str| {
            cc.and_then(|c| c.get(k)).and_then(Value::as_u64).unwrap_or(0)
        };
        let tok = Tokens {
            input: g("input_tokens"),
            output: g("output_tokens"),
            cache_read: g("cache_read_input_tokens"),
            cache_write_5m: ccg("ephemeral_5m_input_tokens"),
            cache_write_1h: ccg("ephemeral_1h_input_tokens"),
        };

        let model = msg.get("model").and_then(Value::as_str).unwrap_or("?").to_string();
        let cwd = v.get("cwd").and_then(Value::as_str).unwrap_or("").to_string();

        // Track the session's model from any line (incl. zero-token ones) so a
        // freshly-started agent still resolves a model in the live table.
        if !session.is_empty() && model != "<synthetic>" {
            let e = self.session_model.entry(session.clone()).or_insert((0, model.clone()));
            if ts >= e.0 {
                *e = (ts, model.clone());
            }
        }

        // Zero-token turns (e.g. "<synthetic>" injected messages) carry no usage
        // and only add noise to the breakdowns — skip them.
        if tok.total() == 0 {
            return;
        }

        if let Some(st) = self.sessions.get_mut(&session) {
            match agent_id {
                Some(aid) => {
                    if let Some(sa) = st.subagents.get_mut(aid) {
                        sa.tokens += tok.total();
                    }
                }
                None if !sidechain && ts >= st.ctx_ts => {
                    st.ctx_tokens = tok.total();
                    st.ctx_ts = ts;
                }
                None => {}
            }
        }

        let cost = pricing::cost(&model, &tok);
        self.records.push(Record { ts, model, cwd, session, sidechain, tok, cost });
    }

    /// Output-token burn rate (tokens/sec) and a recent-activity sparkline for
    /// one session, over the trailing `window` seconds.
    pub fn session_burn(&self, session: &str, now: i64, window: i64, buckets: usize) -> (f64, Vec<u64>) {
        let start = now - window;
        let mut out: u64 = 0;
        let mut hist = vec![0u64; buckets];
        let span = (window / buckets as i64).max(1);
        for r in &self.records {
            if r.session != session || r.ts < start || r.ts > now {
                continue;
            }
            out += r.tok.output;
            let idx = ((r.ts - start) / span).clamp(0, buckets as i64 - 1) as usize;
            hist[idx] += r.tok.output;
        }
        let tps = out as f64 / window as f64;
        (tps, hist)
    }

    /// Token throughput across every session over the trailing `window`
    /// seconds, in `points` buckets for the live graph. Buckets are anchored
    /// to absolute time so the graph scrolls instead of reshuffling, and
    /// each is a trailing ~30 s average: a turn lands its tokens at one
    /// instant, which would otherwise draw as a lone spike.
    pub fn token_series(&self, now: i64, window: i64, points: usize) -> Series {
        let points = points.max(2);
        let span = (window as f64 / points as f64).max(1.0);
        let last = (now as f64 / span).floor() as i64;
        let first = last - points as i64 + 1;
        let start = (first as f64 * span) as i64;
        let mut out = vec![0f64; points];
        let mut inp = vec![0f64; points];
        let (mut out_min, mut inp_min, mut tok, mut cost) = (0u64, 0u64, 0u64, 0f64);
        for r in &self.records {
            if r.ts < start || r.ts > now {
                continue;
            }
            let read = r.tok.input + r.tok.cache_read + r.tok.cache_write_5m + r.tok.cache_write_1h;
            let i = ((r.ts as f64 / span).floor() as i64 - first).clamp(0, points as i64 - 1) as usize;
            out[i] += r.tok.output as f64;
            inp[i] += read as f64;
            tok += r.tok.total();
            cost += r.cost;
            if now - r.ts < 60 {
                out_min += r.tok.output;
                inp_min += read;
            }
        }
        let k = ((30.0 / span).round() as usize).max(1);
        let smooth = |v: &[f64]| -> Vec<f64> {
            (0..v.len())
                .map(|i| v[i.saturating_sub(k - 1)..=i].iter().sum::<f64>() / (k as f64 * span))
                .collect()
        };
        Series {
            out: smooth(&out),
            inp: smooth(&inp),
            window_secs: window,
            out_now: out_min as f64 / 60.0,
            inp_now: inp_min as f64 / 60.0,
            tok,
            cost,
        }
    }

    /// Roll up everything into the numbers the UI panels need.
    pub fn aggregate(&self, now: i64, today_start: i64) -> Aggregates {
        let mut by_model: HashMap<&str, (Tokens, f64)> = HashMap::new();
        let mut by_project: HashMap<&str, (Tokens, f64)> = HashMap::new();
        let mut main = Tokens::default();
        let mut agents = Tokens::default();
        let mut grand = Tokens::default();
        let mut grand_cost = 0.0;
        let mut today_tok = 0u64;
        let mut today_cost = 0.0;
        let mut last5h = 0u64;
        let mut last7d = 0u64;
        let mut last7d_cost = 0.0f64;
        let mut last7d_family: HashMap<&'static str, u64> = HashMap::new();
        let mut buckets24 = vec![0u64; 24];

        let h5 = now - 5 * 3600;
        let d7 = now - 7 * 86400;
        let day = now - 24 * 3600;

        for r in &self.records {
            grand.add(&r.tok);
            grand_cost += r.cost;

            let em = by_model.entry(r.model.as_str()).or_default();
            em.0.add(&r.tok);
            em.1 += r.cost;

            let proj = project_name(&r.cwd);
            let ep = by_project.entry(proj).or_default();
            ep.0.add(&r.tok);
            ep.1 += r.cost;

            if r.sidechain {
                agents.add(&r.tok);
            } else {
                main.add(&r.tok);
            }

            if r.ts >= today_start {
                today_tok += r.tok.total();
                today_cost += r.cost;
            }
            if r.ts >= h5 {
                last5h += r.tok.total();
            }
            if r.ts >= d7 {
                last7d += r.tok.total();
                last7d_cost += r.cost;
                if let Some(fam) = family(&r.model) {
                    *last7d_family.entry(fam).or_default() += r.tok.total();
                }
            }
            if r.ts >= day {
                let idx = ((r.ts - day) / 3600).clamp(0, 23) as usize;
                buckets24[idx] += r.tok.total();
            }
        }

        let mut by_model: Vec<(String, u64, f64)> =
            by_model.into_iter().map(|(k, v)| (k.to_string(), v.0.total(), v.1)).collect();
        by_model.sort_by(|a, b| b.1.cmp(&a.1));

        let mut by_project: Vec<(String, u64, f64)> =
            by_project.into_iter().map(|(k, v)| (k.to_string(), v.0.total(), v.1)).collect();
        by_project.sort_by(|a, b| b.1.cmp(&a.1));

        let mut last7d_by_family: Vec<(&'static str, u64)> = last7d_family.into_iter().collect();
        last7d_by_family.sort_by(|a, b| b.1.cmp(&a.1));

        Aggregates {
            by_model,
            by_project,
            last7d_by_family,
            main_tok: main.total(),
            agent_tok: agents.total(),
            today_tok,
            today_cost,
            buckets24,
            last5h_tok: last5h,
            last7d_tok: last7d,
            last7d_cost,
            grand_tok: grand.total(),
            grand_cost,
        }
    }
}

pub struct Aggregates {
    pub by_model: Vec<(String, u64, f64)>,
    pub by_project: Vec<(String, u64, f64)>,
    /// Trailing-7-day tokens per model family — the offline stand-in for the
    /// API's model-scoped weekly windows.
    pub last7d_by_family: Vec<(&'static str, u64)>,
    pub main_tok: u64,
    pub agent_tok: u64,
    pub today_tok: u64,
    pub today_cost: f64,
    pub buckets24: Vec<u64>,
    pub last5h_tok: u64,
    pub last7d_tok: u64,
    pub last7d_cost: f64,
    pub grand_tok: u64,
    pub grand_cost: f64,
}

/// A tool call as `(tool, what it touches)`: `Bash` + the command, `Edit` +
/// the file name, `Agent` + the task description, `mcp__srv__op` → `srv:op`.
fn describe_tool(name: &str, input: &Value) -> (String, String) {
    let s = |k: &str| input.get(k).and_then(Value::as_str).unwrap_or("");
    let file = |k: &str| s(k).trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string();
    let arg = match name {
        "Bash" => {
            let first = s("command").lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            let mut cmd = first.split_whitespace().collect::<Vec<_>>().join(" ");
            // The folder shows elsewhere; `cd x && make` reads as `make`.
            if cmd.starts_with("cd ") {
                if let Some(i) = cmd.find("&& ") {
                    cmd = cmd[i + 3..].to_string();
                }
            }
            cmd
        }
        "Read" | "Write" | "Edit" | "MultiEdit" => file("file_path"),
        "NotebookEdit" => file("notebook_path"),
        "Grep" | "Glob" => s("pattern").to_string(),
        "WebSearch" => s("query").to_string(),
        "WebFetch" => {
            let url = s("url");
            url.split("://").nth(1).unwrap_or(url).split('/').next().unwrap_or("").to_string()
        }
        "Agent" | "Task" => {
            let d = s("description");
            if d.is_empty() { s("subagent_type").to_string() } else { d.to_string() }
        }
        "Skill" => s("skill").to_string(),
        _ => String::new(),
    };
    let tool = match name.strip_prefix("mcp__") {
        Some(rest) => rest.replacen("__", ":", 1),
        None => name.to_string(),
    };
    // Bounded: a heredoc command can be kilobytes.
    (tool, arg.chars().take(160).collect())
}

/// Model family label for the scoped-weekly estimate rows.
fn family(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    if m.contains("fable") || m.contains("mythos") {
        Some("FABLE")
    } else if m.contains("opus") {
        Some("OPUS")
    } else if m.contains("sonnet") {
        Some("SONNET")
    } else if m.contains("haiku") {
        Some("HAIKU")
    } else {
        None
    }
}

/// Last path component of a cwd, for compact display.
pub fn project_name(cwd: &str) -> &str {
    if cwd.is_empty() {
        return "—";
    }
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd)
}

