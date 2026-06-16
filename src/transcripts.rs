//! Reads Claude Code session transcripts (`~/.claude/projects/*/*.jsonl`),
//! incrementally (only new bytes per refresh), de-duplicates repeated usage
//! lines, and aggregates token/cost totals.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use chrono::DateTime;
use serde_json::Value;

use crate::model::Tokens;
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

struct FileState {
    offset: u64,
}

pub struct Store {
    root: PathBuf,
    files: HashMap<PathBuf, FileState>,
    seen: HashSet<u64>,
    pub records: Vec<Record>,
    /// session id -> (latest ts, model) for joining live agents to their model.
    pub session_model: HashMap<String, (i64, String)>,
}

impl Store {
    pub fn new(root: PathBuf) -> Self {
        Store {
            root,
            files: HashMap::new(),
            seen: HashSet::new(),
            records: Vec::new(),
            session_model: HashMap::new(),
        }
    }

    /// Walk the projects tree and ingest any newly-appended bytes.
    pub fn refresh(&mut self) {
        let mut paths = Vec::new();
        collect_jsonl(&self.root, &mut paths);
        for path in paths {
            if let Err(_e) = self.ingest_file(&path) {
                // A transient read error (file rotated mid-read) just means we
                // retry next refresh; never fatal.
            }
        }
    }

    fn ingest_file(&mut self, path: &Path) -> std::io::Result<()> {
        let len = std::fs::metadata(path)?.len();
        let state = self.files.entry(path.to_path_buf()).or_insert(FileState { offset: 0 });
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
            .or_insert(FileState { offset: 0 })
            .offset = consumed;
        Ok(())
    }

    fn ingest_line(&mut self, line: &str) {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => return,
        };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            return;
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => return,
        };

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
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        if !self.seen.insert(hasher.finish()) {
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
        let session = v.get("sessionId").and_then(Value::as_str).unwrap_or("").to_string();
        let sidechain = v.get("isSidechain").and_then(Value::as_bool).unwrap_or(false);
        let ts = v
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.timestamp())
            .unwrap_or(0);

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

        Aggregates {
            by_model,
            by_project,
            main_tok: main.total(),
            agent_tok: agents.total(),
            today_tok,
            today_cost,
            buckets24,
            last5h_tok: last5h,
            last7d_tok: last7d,
            grand_tok: grand.total(),
            grand_cost,
        }
    }
}

pub struct Aggregates {
    pub by_model: Vec<(String, u64, f64)>,
    pub by_project: Vec<(String, u64, f64)>,
    pub main_tok: u64,
    pub agent_tok: u64,
    pub today_tok: u64,
    pub today_cost: f64,
    pub buckets24: Vec<u64>,
    pub last5h_tok: u64,
    pub last7d_tok: u64,
    pub grand_tok: u64,
    pub grand_cost: f64,
}

/// Last path component of a cwd, for compact display.
pub fn project_name(cwd: &str) -> &str {
    if cwd.is_empty() {
        return "—";
    }
    cwd.trim_end_matches('/').rsplit('/').next().unwrap_or(cwd)
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) {
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    for entry in rd.flatten() {
        let p = entry.path();
        if p.is_dir() {
            collect_jsonl(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("jsonl") {
            out.push(p);
        }
    }
}
