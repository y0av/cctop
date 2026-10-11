//! System load and per-agent resource attribution: which running Claude Code
//! session is eating the CPU, memory or GPU.
//!
//! Every agent is charged for its whole process tree (the `claude` process plus
//! every shell, test runner, dev server and browser it spawned). On Linux,
//! processes that escaped the tree — daemonized, `setsid`'d, or orphaned when
//! their session died — are still traced back through the `CLAUDE_PID`
//! variable Claude Code sets in every child's environment. Those whose session
//! is gone come back as orphan groups: the classic "my machine is crawling and
//! no agent is busy" case.

use std::collections::{HashMap, HashSet};

/// Time constant of the CPU moving average, in seconds.
const CPU_TAU_SECS: f32 = 3.0;
use std::time::Instant;

use sysinfo::{
    CpuRefreshKind, MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind,
    System, UpdateKind,
};

/// One process inside an agent's tree, for the drill-down's "top" list.
#[derive(Clone)]
pub struct ProcLine {
    pub pid: i32,
    pub cpu: f32,
    pub rss_kb: u64,
    pub gpu: Option<f32>,
    /// Short command: executable name plus the first meaningful args.
    pub cmd: String,
    /// Where it runs, as a repo label (`hoteleak/wt-mt-1-5/backend`). Only
    /// filled for the top processes of each agent.
    pub place: String,
}

/// Summed resource use of everything an agent owns.
#[derive(Clone, Default)]
pub struct TreeUsage {
    /// Percent of one core, like `top` (400 = four cores flat out).
    pub cpu: f32,
    /// Summed resident memory. Overcounts pages shared between forked workers.
    pub rss_kb: u64,
    /// GPU engine busy percent across the tree, when the platform exposes it.
    pub gpu: Option<f32>,
    /// GPU memory (VRAM + GTT) held by the tree, in KiB.
    pub gpu_mem_kb: u64,
    pub nprocs: usize,
    /// Processes attributed via `CLAUDE_PID` rather than parentage.
    pub detached: usize,
    /// Hottest processes first (by CPU, then memory).
    pub top: Vec<ProcLine>,
}

#[derive(Clone, Default)]
pub struct GpuStats {
    pub busy: Option<f32>,
    pub vram_used_kb: u64,
    pub vram_total_kb: u64,
    /// Shared system memory mapped for the GPU (APUs live here).
    pub gtt_used_kb: u64,
    pub gtt_total_kb: u64,
}

#[derive(Clone, Default)]
pub struct SysStats {
    /// Whole-machine CPU, 0..=100.
    pub cpu: f32,
    pub ncpu: usize,
    pub load: (f64, f64, f64),
    pub mem_used_kb: u64,
    pub mem_total_kb: u64,
    pub swap_used_kb: u64,
    pub swap_total_kb: u64,
    /// Linux pressure stall info, `some avg10` percent: time in the last 10s
    /// that at least one task waited on CPU / memory / IO. The best single
    /// signal for "the desktop is freezing".
    pub psi_cpu: Option<f32>,
    pub psi_mem: Option<f32>,
    pub psi_io: Option<f32>,
    pub gpu: Option<GpuStats>,
}

/// Processes carrying a `CLAUDE_PID` that isn't a registered live session.
pub struct Stray {
    pub claude_pid: i32,
    /// The owning claude process still runs but has no session file (e.g. a
    /// config dir cctop wasn't pointed at). False means the session is dead
    /// and these are orphans.
    pub owner_alive: bool,
    pub usage: TreeUsage,
    pub cwd: String,
}

pub struct Sample {
    pub sys: SysStats,
    pub trees: HashMap<i32, TreeUsage>,
    pub strays: Vec<Stray>,
}

pub struct Sampler {
    sys: System,
    /// (pid, start time) -> CLAUDE_PID from its environment, read once.
    env_cache: HashMap<(u32, u64), Option<i32>>,
    /// (pid, start time) -> smoothed CPU percent. Raw readings cover one
    /// tick, so a worker pool flickers; this is an exponential moving average.
    cpu_avg: HashMap<(u32, u64), f32>,
    sys_cpu_avg: Option<f32>,
    last_refresh: Option<Instant>,
    /// Weight of the newest reading this tick (depends on tick length).
    alpha: f32,
    gpu: GpuProbe,
}

impl Sampler {
    pub fn new() -> Self {
        let sys = System::new_with_specifics(
            RefreshKind::nothing()
                .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(MemoryRefreshKind::everything()),
        );
        Sampler {
            sys,
            env_cache: HashMap::new(),
            cpu_avg: HashMap::new(),
            sys_cpu_avg: None,
            last_refresh: None,
            alpha: 1.0,
            gpu: GpuProbe::new(),
        }
    }

    /// Refresh the process table, CPU and memory. CPU percentages are deltas
    /// against the previous call, so the first sample reads zero.
    pub fn refresh(&mut self) {
        self.sys.refresh_cpu_usage();
        self.sys.refresh_memory();
        self.sys.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .with_cmd(UpdateKind::OnlyIfNotSet)
                .without_tasks(),
        );
        let live: HashSet<(u32, u64)> =
            self.sys.processes().iter().map(|(p, pr)| (p.as_u32(), pr.start_time())).collect();
        self.env_cache.retain(|k, _| live.contains(k));

        // Smoothing: time constant ~3 s, so a sustained spike shows at half
        // strength after ~2 s and nearly full after ~6 s.
        let now = Instant::now();
        let dt = self.last_refresh.map(|t| now.duration_since(t).as_secs_f32()).unwrap_or(0.0);
        self.last_refresh = Some(now);
        self.alpha = if dt <= 0.0 { 1.0 } else { 1.0 - (-dt / CPU_TAU_SECS).exp() };
        let a = self.alpha;
        let mut next = HashMap::with_capacity(live.len());
        for (pid, pr) in self.sys.processes() {
            let key = (pid.as_u32(), pr.start_time());
            let raw = pr.cpu_usage();
            let v = match self.cpu_avg.get(&key) {
                Some(prev) => prev + a * (raw - prev),
                None => raw,
            };
            next.insert(key, v);
        }
        self.cpu_avg = next;
        let raw = self.sys.global_cpu_usage();
        self.sys_cpu_avg = Some(match self.sys_cpu_avg {
            Some(prev) if dt > 0.0 => prev + a * (raw - prev),
            _ => raw,
        });
    }

    fn smoothed_cpu(&self, pid: Pid, p: &sysinfo::Process) -> f32 {
        self.cpu_avg.get(&(pid.as_u32(), p.start_time())).copied().unwrap_or_else(|| p.cpu_usage())
    }

    /// Resident memory of a live PID in KiB, `None` when it doesn't exist.
    pub fn rss_kb(&self, pid: i32) -> Option<u64> {
        if pid < 0 {
            return None;
        }
        self.sys.process(Pid::from_u32(pid as u32)).map(|p| p.memory() / 1024)
    }

    /// Charge every process to an agent (by parentage, then by `CLAUDE_PID`)
    /// and read the system-wide gauges.
    pub fn attribute(&mut self, roots: &[i32]) -> Sample {
        let root_set: HashSet<i32> = roots.iter().copied().collect();
        let procs = self.sys.processes();

        let mut children: HashMap<i32, Vec<i32>> = HashMap::new();
        for (pid, p) in procs {
            if let Some(pp) = p.parent() {
                children.entry(pp.as_u32() as i32).or_default().push(pid.as_u32() as i32);
            }
        }

        // owner pid -> member pids; members flagged when found via env.
        let mut owner: HashMap<i32, i32> = HashMap::new();
        let mut via_env: HashSet<i32> = HashSet::new();
        for &r in roots {
            if self.sys.process(Pid::from_u32(r as u32)).is_none() {
                continue;
            }
            let mut stack = vec![r];
            while let Some(p) = stack.pop() {
                if owner.contains_key(&p) {
                    continue;
                }
                owner.insert(p, r);
                for &c in children.get(&p).map(Vec::as_slice).unwrap_or(&[]) {
                    // A nested claude with its own session owns its subtree.
                    if !root_set.contains(&c) {
                        stack.push(c);
                    }
                }
            }
        }
        // Everything else: trust the environment Claude Code stamped on it.
        let unowned: Vec<(i32, u64)> = procs
            .iter()
            .map(|(p, pr)| (p.as_u32() as i32, pr.start_time()))
            .filter(|(p, _)| !owner.contains_key(p))
            .collect();
        for (pid, start) in unowned {
            let cp = *self
                .env_cache
                .entry((pid as u32, start))
                .or_insert_with(|| claude_pid_of(pid));
            if let Some(cp) = cp {
                if cp != pid {
                    owner.insert(pid, cp);
                    via_env.insert(pid);
                }
            }
        }

        let gpu_by_pid = self.gpu.per_process(owner.keys().copied());

        let mut groups: HashMap<i32, TreeUsage> = HashMap::new();
        for (&pid, &own) in &owner {
            let Some(p) = self.sys.process(Pid::from_u32(pid as u32)) else { continue };
            let g = groups.entry(own).or_default();
            let cpu = self.smoothed_cpu(Pid::from_u32(pid as u32), p);
            let rss = p.memory() / 1024;
            let gpu = gpu_by_pid.get(&pid);
            g.cpu += cpu;
            g.rss_kb += rss;
            g.nprocs += 1;
            if via_env.contains(&pid) {
                g.detached += 1;
            }
            if let Some((busy, mem)) = gpu {
                g.gpu = Some(g.gpu.unwrap_or(0.0) + busy);
                g.gpu_mem_kb += mem;
            }
            g.top.push(ProcLine {
                pid,
                cpu,
                rss_kb: rss,
                gpu: gpu.map(|(b, _)| *b),
                cmd: short_cmd(p, Some(&self.sys)),
                place: String::new(),
            });
        }
        for g in groups.values_mut() {
            g.top.sort_by(|a, b| {
                b.cpu
                    .partial_cmp(&a.cpu)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.rss_kb.cmp(&a.rss_kb))
            });
            g.top.truncate(5);
            for t in &mut g.top {
                let cwd = proc_cwd(t.pid);
                if !cwd.is_empty() {
                    t.place = crate::sessions::repo_label(&cwd);
                }
            }
            if self.gpu.per_process_supported() && g.gpu.is_none() {
                g.gpu = Some(0.0);
            }
        }

        let mut trees = HashMap::new();
        let mut strays = Vec::new();
        for (own, usage) in groups {
            if root_set.contains(&own) {
                trees.insert(own, usage);
            } else {
                let owner_alive = self.sys.process(Pid::from_u32(own as u32)).is_some();
                let cwd = usage.top.first().map(|t| proc_cwd(t.pid)).unwrap_or_default();
                strays.push(Stray { claude_pid: own, owner_alive, usage, cwd });
            }
        }
        strays.sort_by(|a, b| b.usage.cpu.partial_cmp(&a.usage.cpu).unwrap_or(std::cmp::Ordering::Equal));

        Sample { sys: self.sys_stats(), trees, strays }
    }

    fn sys_stats(&mut self) -> SysStats {
        let load = System::load_average();
        let (psi_cpu, psi_mem, psi_io) = psi();
        SysStats {
            cpu: self.sys_cpu_avg.unwrap_or_else(|| self.sys.global_cpu_usage()),
            ncpu: self.sys.cpus().len().max(1),
            load: (load.one, load.five, load.fifteen),
            mem_used_kb: self.sys.used_memory() / 1024,
            mem_total_kb: self.sys.total_memory() / 1024,
            swap_used_kb: self.sys.used_swap() / 1024,
            swap_total_kb: self.sys.total_swap() / 1024,
            psi_cpu,
            psi_mem,
            psi_io,
            gpu: self.gpu.system(),
        }
    }
}

/// `name args…`, trimmed to what identifies the job. Two wrappers get
/// unwrapped: Claude's own shell (`zsh -c "source snapshot … && eval '<cmd>'"`)
/// shows `$ <cmd>`, and an interpreter running inline code (`python -c …`,
/// e.g. a pytest-xdist worker) shows the parent that launched it.
fn short_cmd(p: &sysinfo::Process, sys: Option<&System>) -> String {
    let name = p.name().to_string_lossy().to_string();
    let args: Vec<String> = p
        .cmd()
        .iter()
        .skip(1)
        .map(|a| a.to_string_lossy().to_string())
        .filter(|a| !a.is_empty())
        .collect();
    if args.is_empty() {
        return name;
    }
    if let Some(ci) = args.iter().position(|a| a == "-c") {
        let code = args.get(ci + 1).map(String::as_str).unwrap_or("");
        if let Some(inner) = claude_shell_command(code) {
            return format!("$ {inner}");
        }
        if is_shell(&name) {
            return format!("$ {}", code.split_whitespace().collect::<Vec<_>>().join(" "));
        }
        // Climb past nested inline-code interpreters (execnet spawns workers
        // from workers) to the command that started them, e.g. pytest.
        let mut parent = None;
        let mut cur = p;
        for _ in 0..6 {
            let Some(pp) = sys.and_then(|s| s.process(cur.parent()?)) else { break };
            let pc = short_cmd(pp, None);
            parent = Some(pc.clone());
            if !pc.ends_with(" -c") {
                break;
            }
            cur = pp;
        }
        return match parent {
            Some(pc) => format!("{name} -c ← {pc}"),
            None => format!("{name} -c"),
        };
    }
    let mut out = name;
    for a in args.iter().take(4) {
        // Paths collapse to their last component: `.venv/bin/pytest` → pytest.
        let a = if a.contains('/') && !a.contains(' ') {
            a.trim_end_matches('/').rsplit('/').next().unwrap_or(a)
        } else {
            a.as_str()
        };
        out.push(' ');
        out.push_str(a);
        if out.len() > 60 {
            break;
        }
    }
    out
}

fn is_shell(name: &str) -> bool {
    matches!(name, "sh" | "bash" | "zsh" | "dash" | "fish")
}

/// The user command inside Claude Code's shell wrapper, minus a leading
/// `cd <dir> &&` (the folder is shown separately).
fn claude_shell_command(code: &str) -> Option<String> {
    let start = code.find("eval '")? + "eval '".len();
    let rest = &code[start..];
    // The wrapper closes the quote before its own ` < /dev/null && pwd …`
    // tail; quotes inside are escaped as `'"'"'` (or `'\''`).
    let end = rest.rfind("' < /dev/null").or_else(|| rest.rfind('\'')).unwrap_or(rest.len());
    let mut cmd = rest[..end].replace("'\"'\"'", "'").replace("'\\''", "'");
    if cmd.starts_with("cd ") {
        if let Some(i) = cmd.find("&&") {
            cmd = cmd[i + 2..].trim_start().to_string();
        }
    }
    let cmd = cmd.split_whitespace().collect::<Vec<_>>().join(" ");
    (!cmd.is_empty()).then_some(cmd)
}

#[cfg(target_os = "linux")]
fn claude_pid_of(pid: i32) -> Option<i32> {
    let env = std::fs::read(format!("/proc/{pid}/environ")).ok()?;
    env.split(|b| *b == 0)
        .find_map(|kv| kv.strip_prefix(b"CLAUDE_PID="))
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|v| v.trim().parse().ok())
}

#[cfg(not(target_os = "linux"))]
fn claude_pid_of(_pid: i32) -> Option<i32> {
    None
}

#[cfg(target_os = "linux")]
fn proc_cwd(pid: i32) -> String {
    std::fs::read_link(format!("/proc/{pid}/cwd"))
        .map(|p| p.display().to_string())
        .unwrap_or_default()
}

#[cfg(not(target_os = "linux"))]
fn proc_cwd(_pid: i32) -> String {
    String::new()
}

#[cfg(target_os = "linux")]
fn psi() -> (Option<f32>, Option<f32>, Option<f32>) {
    let read = |k: &str| -> Option<f32> {
        let txt = std::fs::read_to_string(format!("/proc/pressure/{k}")).ok()?;
        let line = txt.lines().find(|l| l.starts_with("some"))?;
        line.split_whitespace()
            .find_map(|f| f.strip_prefix("avg10="))
            .and_then(|v| v.parse().ok())
    };
    (read("cpu"), read("memory"), read("io"))
}

#[cfg(not(target_os = "linux"))]
fn psi() -> (Option<f32>, Option<f32>, Option<f32>) {
    (None, None, None)
}

// ---------------------------------------------------------------------------
// GPU
// ---------------------------------------------------------------------------

/// GPU sources, best available per platform:
/// - Linux DRM (amdgpu, i915, xe, …): system busy % and VRAM/GTT from sysfs,
///   per-process engine time and memory from `/proc/<pid>/fdinfo`.
/// - NVIDIA: `nvidia-smi`, polled on a background thread so its ~100 ms spawn
///   never stalls a frame. Per-process SM % comes from `nvidia-smi pmon`.
struct GpuProbe {
    #[cfg(target_os = "linux")]
    drm: Option<std::path::PathBuf>,
    /// (drm client id, engine) -> (busy ns, when read). Client ids are unique
    /// per device and outlive which pid we happen to read them through.
    #[cfg(target_os = "linux")]
    prev: HashMap<(String, String), (u64, Instant)>,
    /// pid -> (last full fd scan, DRM fd numbers)
    #[cfg(target_os = "linux")]
    fds: HashMap<i32, (Instant, Vec<String>)>,
    nvidia: Option<std::sync::Arc<std::sync::Mutex<NvState>>>,
}

#[derive(Default)]
struct NvState {
    sys: Option<GpuStats>,
    per_pid: HashMap<i32, (f32, u64)>,
}

impl GpuProbe {
    fn new() -> Self {
        GpuProbe {
            #[cfg(target_os = "linux")]
            drm: find_drm_device(),
            #[cfg(target_os = "linux")]
            prev: HashMap::new(),
            #[cfg(target_os = "linux")]
            fds: HashMap::new(),
            nvidia: spawn_nvidia_poller(),
        }
    }

    fn per_process_supported(&self) -> bool {
        #[cfg(target_os = "linux")]
        if self.drm.is_some() {
            return true;
        }
        self.nvidia.is_some()
    }

    fn system(&self) -> Option<GpuStats> {
        if let Some(nv) = &self.nvidia {
            if let Some(s) = nv.lock().ok().and_then(|g| g.sys.clone()) {
                return Some(s);
            }
        }
        #[cfg(target_os = "linux")]
        if let Some(dev) = &self.drm {
            let rd = |f: &str| -> Option<u64> {
                std::fs::read_to_string(dev.join(f)).ok()?.trim().parse().ok()
            };
            return Some(GpuStats {
                busy: rd("gpu_busy_percent").map(|v| v as f32),
                vram_used_kb: rd("mem_info_vram_used").unwrap_or(0) / 1024,
                vram_total_kb: rd("mem_info_vram_total").unwrap_or(0) / 1024,
                gtt_used_kb: rd("mem_info_gtt_used").unwrap_or(0) / 1024,
                gtt_total_kb: rd("mem_info_gtt_total").unwrap_or(0) / 1024,
            });
        }
        None
    }

    /// pid -> (busy %, GPU memory KiB) for the given processes.
    fn per_process(&mut self, pids: impl Iterator<Item = i32>) -> HashMap<i32, (f32, u64)> {
        let pids: Vec<i32> = pids.collect();
        let mut out = HashMap::new();
        if let Some(nv) = &self.nvidia {
            if let Ok(g) = nv.lock() {
                for p in &pids {
                    if let Some(v) = g.per_pid.get(p) {
                        out.insert(*p, *v);
                    }
                }
            }
        }
        #[cfg(target_os = "linux")]
        if self.drm.is_some() {
            self.drm_per_process(&pids, &mut out);
        }
        out
    }

    #[cfg(target_os = "linux")]
    fn drm_per_process(&mut self, pids: &[i32], out: &mut HashMap<i32, (f32, u64)>) {
        let now = Instant::now();
        let live: HashSet<i32> = pids.iter().copied().collect();
        self.fds.retain(|p, _| live.contains(p));
        let mut prev_next: HashMap<(String, String), (u64, Instant)> = HashMap::new();
        let mut pids = pids.to_vec();
        pids.sort_unstable();
        // A DRM client (one open of the device) can be shared by several fds
        // and inherited across fork; count each client once.
        let mut seen_clients: HashSet<String> = HashSet::new();
        for pid in pids {
            let rescan = self
                .fds
                .get(&pid)
                .map(|(t, _)| now.duration_since(*t).as_secs() >= 5)
                .unwrap_or(true);
            if rescan {
                self.fds.insert(pid, (now, drm_fds(pid)));
            }
            let fds = self.fds.get(&pid).map(|(_, f)| f.clone()).unwrap_or_default();
            let mut busy = 0.0f32;
            let mut mem = 0u64;
            let mut any = false;
            for fd in fds {
                let Ok(info) = std::fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) else {
                    continue;
                };
                let mut client = String::new();
                let mut engines: Vec<(String, u64)> = Vec::new();
                let mut fd_mem = 0u64;
                for line in info.lines() {
                    let Some((k, v)) = line.split_once(':') else { continue };
                    let v = v.trim();
                    if k == "drm-client-id" {
                        client = v.to_string();
                    } else if let Some(eng) = k.strip_prefix("drm-engine-") {
                        if let Some(ns) = v.strip_suffix(" ns").and_then(|n| n.trim().parse().ok()) {
                            engines.push((eng.to_string(), ns));
                        }
                    } else if k == "drm-memory-vram" || k == "drm-memory-gtt" {
                        fd_mem += v.trim_end_matches("KiB").trim().parse::<u64>().unwrap_or(0);
                    }
                }
                if client.is_empty() || !seen_clients.insert(client.clone()) {
                    continue;
                }
                any = true;
                mem += fd_mem;
                // Busiest engine of this client, as a percent of wall time.
                let mut client_busy = 0.0f32;
                for (eng, ns) in engines {
                    let key = (client.clone(), eng);
                    if let Some((pns, pt)) = self.prev.get(&key) {
                        let dt = now.duration_since(*pt).as_nanos() as f64;
                        if dt > 0.0 && ns >= *pns {
                            let pct = ((ns - pns) as f64 / dt * 100.0) as f32;
                            client_busy = client_busy.max(pct.min(100.0));
                        }
                    }
                    prev_next.insert(key, (ns, now));
                }
                busy += client_busy;
            }
            if any {
                out.insert(pid, (busy, mem));
            }
        }
        self.prev = prev_next;
    }
}

#[cfg(target_os = "linux")]
fn find_drm_device() -> Option<std::path::PathBuf> {
    // Prefer the card that reports a busy percentage (amdgpu); otherwise any
    // card with a driver, so per-process fdinfo still works (Intel).
    let mut fallback = None;
    for e in std::fs::read_dir("/sys/class/drm").ok()?.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }
        let dev = e.path().join("device");
        if dev.join("gpu_busy_percent").exists() {
            return Some(dev);
        }
        if fallback.is_none() && dev.join("driver").exists() {
            fallback = Some(dev);
        }
    }
    fallback
}

#[cfg(target_os = "linux")]
fn drm_fds(pid: i32) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(format!("/proc/{pid}/fd")) else { return Vec::new() };
    rd.flatten()
        .filter(|e| {
            std::fs::read_link(e.path())
                .map(|t| t.starts_with("/dev/dri/"))
                .unwrap_or(false)
        })
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect()
}

fn spawn_nvidia_poller() -> Option<std::sync::Arc<std::sync::Mutex<NvState>>> {
    use std::process::{Command, Stdio};
    let run = |args: &[&str]| -> Option<String> {
        let out = Command::new("nvidia-smi").args(args).stderr(Stdio::null()).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string())
    };
    let query = ["--query-gpu=utilization.gpu,memory.used,memory.total", "--format=csv,noheader,nounits"];
    // Probe once; no binary or no device means no thread.
    run(&query)?;
    let state = std::sync::Arc::new(std::sync::Mutex::new(NvState::default()));
    let st = state.clone();
    std::thread::spawn(move || loop {
        let mut next = NvState::default();
        if let Some(txt) = run(&query) {
            // One line per GPU; sum memory, keep the busiest GPU.
            let mut g = GpuStats::default();
            for line in txt.lines() {
                let f: Vec<f64> = line.split(',').filter_map(|s| s.trim().parse().ok()).collect();
                if f.len() == 3 {
                    g.busy = Some(g.busy.unwrap_or(0.0).max(f[0] as f32));
                    g.vram_used_kb += (f[1] * 1024.0) as u64;
                    g.vram_total_kb += (f[2] * 1024.0) as u64;
                }
            }
            next.sys = Some(g);
        }
        // `pmon` columns: gpu pid type sm mem … ; "-" means idle.
        if let Some(txt) = run(&["pmon", "-c", "1", "-s", "u"]) {
            for line in txt.lines().filter(|l| !l.trim_start().starts_with('#')) {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() > 3 {
                    if let Ok(pid) = f[1].parse::<i32>() {
                        let sm = f[3].parse::<f32>().unwrap_or(0.0);
                        let e = next.per_pid.entry(pid).or_insert((0.0, 0));
                        e.0 += sm;
                    }
                }
            }
        }
        if let Ok(mut s) = st.lock() {
            *s = next;
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    });
    Some(state)
}

/// Display helpers shared by the TUI and `--once`.
/// CPU as cores in use: `11.8c` means twelve cores' worth of work. Under
/// a tenth of a core reads `0c`.
pub fn fmt_cpu(pct: f32) -> String {
    let c = pct.max(0.0) / 100.0;
    if c < 0.05 {
        "0c".into()
    } else if c >= 100.0 {
        format!("{c:.0}c")
    } else {
        format!("{c:.1}c")
    }
}

/// Long form for sentences: `11.8 cores`.
pub fn fmt_cores(pct: f32) -> String {
    let c = pct.max(0.0) / 100.0;
    if (c - 1.0).abs() < 0.05 { "1 core".into() } else { format!("{c:.1} cores") }
}

pub fn fmt_kb(kb: u64) -> String {
    let mb = kb as f64 / 1024.0;
    if mb >= 1024.0 {
        format!("{:.1}G", mb / 1024.0)
    } else {
        format!("{:.0}M", mb)
    }
}
