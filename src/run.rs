//! `agentgc run`: a machine-wide queue for heavy commands and exclusive resources.
//! No daemon. Slots are advisory file locks (see lock.rs); the OS frees them when the holder dies.
use crate::lock::{self, Slot};
use crate::out::outln;
use clap::Args;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const POLL: Duration = Duration::from_millis(200);
const EXIT_TIMEOUT: i32 = 75;
const EXIT_NOT_STARTED: i32 = 127;
const MAX_SLOTS: usize = 1024;

#[derive(Args, Debug, Default, Clone)]
pub struct RunArgs {
    /// Resource to take. Repeatable. Default: "default". Several are taken in sorted order.
    #[arg(long)]
    pub resource: Vec<String>,
    /// Slots per resource. Default: a quarter of the logical cores, at least 1 (env AGENTGC_SLOTS).
    #[arg(long)]
    pub slots: Option<usize>,
    /// Wait until at least this many seconds have passed since the last start on each resource (env AGENTGC_MIN_INTERVAL).
    /// For a rate-limited service. The start time is stored next to the slot files.
    #[arg(long)]
    pub min_interval: Option<f64>,
    /// Pool name. Default: "default" (env AGENTGC_POOL).
    #[arg(long)]
    pub pool: Option<String>,
    /// Folder for slot files. Default: <temp dir>/agentgc-run (env AGENTGC_DIR).
    #[arg(long)]
    pub dir: Option<PathBuf>,
    /// Stop waiting after this many seconds (env AGENTGC_TIMEOUT).
    #[arg(long)]
    pub timeout: Option<u64>,
    /// After a timeout: "fail" (exit 75, default) or "run" (run the command without slots) (env AGENTGC_ON_TIMEOUT).
    #[arg(long)]
    pub on_timeout: Option<String>,
    /// Append one JSON line per queued run to this file (env AGENTGC_LOG).
    #[arg(long)]
    pub log: Option<PathBuf>,
    /// Queue even when the CI env var is set.
    #[arg(long)]
    pub no_skip_ci: bool,
    /// Show slots and holders instead of running a command. Read-only.
    #[arg(long)]
    pub status: bool,
    /// With --status: print JSON.
    #[arg(long)]
    pub json: bool,
    /// The command and its arguments, after `--`. Run without a shell.
    #[arg(last = true, allow_hyphen_values = true)]
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum OnTimeout {
    Fail,
    Run,
}

struct Config {
    resources: Vec<String>,
    slots: usize,
    pool: String,
    dir: PathBuf,
    timeout: Option<Duration>,
    on_timeout: OnTimeout,
    min_interval: Option<Duration>,
    log: Option<PathBuf>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Info {
    pid: u32,
    command: String,
    cwd: String,
    start: u64,
}

// ---------- pure helpers ----------

/// Lowercase letters, digits, '-' and '_' only. Not empty, at most 64 chars.
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// Sorted by name, duplicates removed. Every caller takes resources in this order.
pub fn sort_resources(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}

/// A quarter of the logical cores, at least 1.
pub fn default_slots() -> usize {
    let n = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
    (n / 4).max(1)
}

/// `AGENTGC_HELD` is `<pool>:<res>[,<res>...]`; several pools are joined with `;`.
pub fn parse_held(s: &str) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for part in s.split(';') {
        if let Some((pool, list)) = part.split_once(':') {
            let e = m.entry(pool.trim().to_string()).or_default();
            for r in list.split(',').map(str::trim).filter(|r| !r.is_empty()) {
                if !e.iter().any(|x| x == r) {
                    e.push(r.to_string());
                }
            }
        }
    }
    m
}

pub fn format_held(m: &BTreeMap<String, Vec<String>>) -> String {
    m.iter().map(|(p, r)| format!("{p}:{}", r.join(","))).collect::<Vec<_>>().join(";")
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn human_age(secs: u64) -> String {
    if secs < 120 {
        format!("{secs} s")
    } else if secs < 7200 {
        format!("{} min", secs / 60)
    } else {
        format!("{} h", secs / 3600)
    }
}

fn env_nonempty(k: &str) -> Option<String> {
    std::env::var(k).ok().filter(|v| !v.is_empty())
}

fn resolve(a: &RunArgs) -> Result<Config, String> {
    let pool = a.pool.clone().or_else(|| env_nonempty("AGENTGC_POOL")).unwrap_or_else(|| "default".into());
    if !valid_name(&pool) {
        return Err(format!("bad pool name '{pool}': use lowercase letters, digits, '-' and '_'"));
    }
    let mut resources = a.resource.clone();
    if resources.is_empty() {
        resources.push("default".into());
    }
    for r in &resources {
        if !valid_name(r) {
            return Err(format!("bad resource name '{r}': use lowercase letters, digits, '-' and '_'"));
        }
    }
    let slots = match (a.slots, env_nonempty("AGENTGC_SLOTS")) {
        (Some(n), _) => n,
        (None, Some(s)) => s.parse::<usize>().map_err(|_| format!("bad AGENTGC_SLOTS '{s}'"))?,
        (None, None) => default_slots(),
    };
    if slots == 0 || slots > MAX_SLOTS {
        return Err(format!("--slots must be 1..{MAX_SLOTS}"));
    }
    let dir = a
        .dir
        .clone()
        .or_else(|| env_nonempty("AGENTGC_DIR").map(PathBuf::from))
        .unwrap_or_else(|| std::env::temp_dir().join("agentgc-run"));
    let timeout = match (a.timeout, env_nonempty("AGENTGC_TIMEOUT")) {
        (Some(n), _) => Some(n),
        (None, Some(s)) => Some(s.parse::<u64>().map_err(|_| format!("bad AGENTGC_TIMEOUT '{s}'"))?),
        (None, None) => None,
    }
    .map(Duration::from_secs);
    let ot = a.on_timeout.clone().or_else(|| env_nonempty("AGENTGC_ON_TIMEOUT")).unwrap_or_else(|| "fail".into());
    let on_timeout = match ot.as_str() {
        "fail" => OnTimeout::Fail,
        "run" => OnTimeout::Run,
        other => return Err(format!("bad --on-timeout '{other}': use fail or run")),
    };
    let mi = match (a.min_interval, env_nonempty("AGENTGC_MIN_INTERVAL")) {
        (Some(n), _) => Some(n),
        (None, Some(s)) => Some(s.parse::<f64>().map_err(|_| format!("bad AGENTGC_MIN_INTERVAL '{s}'"))?),
        (None, None) => None,
    };
    let min_interval = match mi {
        Some(n) if !n.is_finite() || !(0.0..=86400.0).contains(&n) => {
            return Err("--min-interval must be 0..86400 seconds".into())
        }
        Some(n) if n > 0.0 => Some(Duration::from_secs_f64(n)),
        _ => None,
    };
    let log = a.log.clone().or_else(|| env_nonempty("AGENTGC_LOG").map(PathBuf::from));
    Ok(Config { resources: sort_resources(resources), slots, pool, dir, timeout, on_timeout, min_interval, log })
}

// ---------- queue ----------

struct Held {
    lock_path: PathBuf,
    _slot: Slot,
}

fn read_info(lock_path: &Path) -> Option<Info> {
    let t = fs::read_to_string(lock::info_path(lock_path)).ok()?;
    serde_json::from_str(&t).ok()
}

fn describe_holders(cfg: &Config, resource: &str) {
    eprintln!(
        "agentgc run: all {} slot(s) of '{}' (pool '{}') are busy, waiting (the queue is not fair)",
        cfg.slots, resource, cfg.pool
    );
    for i in 0..cfg.slots {
        let p = lock::slot_path(&cfg.dir, &cfg.pool, resource, i);
        match read_info(&p) {
            Some(inf) => eprintln!(
                "  slot {i}: pid {}, held {}, {}",
                inf.pid,
                human_age(now_secs().saturating_sub(inf.start)),
                inf.command
            ),
            None => eprintln!("  slot {i}: held, no holder info"),
        }
    }
}

enum Acquired {
    Got(Vec<Held>, Duration),
    TimedOut(Duration),
}

fn acquire_all(cfg: &Config, need: &[String], command: &[String]) -> Result<Acquired, String> {
    let t0 = Instant::now();
    let mut held: Vec<Held> = Vec::new();
    for r in need {
        let mut announced = false;
        let rt = Instant::now();
        loop {
            let mut got = None;
            for i in 0..cfg.slots {
                let p = lock::slot_path(&cfg.dir, &cfg.pool, r, i);
                if let Some(s) = lock::try_acquire(&p, true).map_err(|e| format!("lock {}: {e}", p.display()))? {
                    got = Some((i, p, s));
                    break;
                }
            }
            if let Some((i, p, s)) = got {
                write_info(&p, command);
                if let Some(iv) = cfg.min_interval {
                    if !wait_interval(cfg, r, iv, t0)? {
                        held.push(Held { lock_path: p, _slot: s });
                        release(held);
                        return Ok(Acquired::TimedOut(t0.elapsed()));
                    }
                }
                if announced {
                    eprintln!("agentgc run: got {r} slot {i} after {:.1} s", rt.elapsed().as_secs_f64());
                }
                held.push(Held { lock_path: p, _slot: s });
                break;
            }
            if !announced {
                describe_holders(cfg, r);
                announced = true;
            }
            if let Some(t) = cfg.timeout {
                if t0.elapsed() >= t {
                    release(held);
                    return Ok(Acquired::TimedOut(t0.elapsed()));
                }
            }
            std::thread::sleep(POLL);
        }
    }
    Ok(Acquired::Got(held, t0.elapsed()))
}

/// Wait until `iv` has passed since the last start on the resource, then stamp the start time now.
/// A short gate lock makes concurrent waiters start one interval apart. `Ok(false)` means the timeout hit.
fn wait_interval(cfg: &Config, r: &str, iv: Duration, t0: Instant) -> Result<bool, String> {
    let gp = lock::gate_path(&cfg.dir, &cfg.pool, r);
    let lp = lock::last_path(&cfg.dir, &cfg.pool, r);
    let timed_out = || cfg.timeout.is_some_and(|t| t0.elapsed() >= t);
    let _gate = loop {
        if let Some(g) = lock::try_acquire(&gp, true).map_err(|e| format!("lock {}: {e}", gp.display()))? {
            break g;
        }
        if timed_out() {
            return Ok(false);
        }
        std::thread::sleep(POLL);
    };
    let mut announced = false;
    loop {
        let rem = lock::remaining_ms(lock::read_last_ms(&lp), now_ms(), iv.as_millis() as u64);
        if rem == 0 {
            break;
        }
        if !announced {
            eprintln!("agentgc run: waiting up to {:.1} s: min interval of '{r}'", rem as f64 / 1000.0);
            announced = true;
        }
        if timed_out() {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(rem).min(POLL));
    }
    lock::write_last_ms(&lp, now_ms());
    Ok(true)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn write_info(lock_path: &Path, command: &[String]) {
    let inf = Info {
        pid: std::process::id(),
        command: command.join(" "),
        cwd: std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default(),
        start: now_secs(),
    };
    if let Ok(t) = serde_json::to_string(&inf) {
        let _ = fs::write(lock::info_path(lock_path), t);
    }
}

/// Remove our own `.info` first, then drop the lock: nobody else can have written theirs yet.
fn release(held: Vec<Held>) {
    for h in &held {
        let _ = fs::remove_file(lock::info_path(&h.lock_path));
    }
    drop(held);
}

// ---------- running the command ----------

/// On Windows, Rust finds only `.exe` by bare name. Look for `.cmd` and `.bat` (npm, pnpm) too.
#[cfg(windows)]
fn resolve_program(prog: &str) -> std::ffi::OsString {
    let p = Path::new(prog);
    if p.extension().is_some() || prog.contains('/') || prog.contains('\\') {
        return prog.into();
    }
    if let Some(path) = std::env::var_os("PATH") {
        for d in std::env::split_paths(&path) {
            for ext in ["exe", "com", "cmd", "bat"] {
                let c = d.join(format!("{prog}.{ext}"));
                if c.is_file() {
                    return c.into_os_string();
                }
            }
        }
    }
    prog.into()
}

#[cfg(not(windows))]
fn resolve_program(prog: &str) -> std::ffi::OsString {
    prog.into()
}

fn exit_code(st: std::process::ExitStatus) -> i32 {
    if let Some(c) = st.code() {
        return c;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(s) = st.signal() {
            return 128 + s;
        }
    }
    1
}

fn spawn(command: &[String], held_env: Option<&str>) -> i32 {
    let mut c = Command::new(resolve_program(&command[0]));
    c.args(&command[1..]);
    if let Some(h) = held_env {
        c.env("AGENTGC_HELD", h);
    }
    match c.status() {
        Ok(st) => exit_code(st),
        Err(e) => {
            eprintln!("agentgc run: cannot start '{}': {e}", command[0]);
            EXIT_NOT_STARTED
        }
    }
}

fn write_log(path: &Path, v: &serde_json::Value) {
    let r = (|| -> std::io::Result<()> {
        if let Some(p) = path.parent() {
            if !p.as_os_str().is_empty() {
                fs::create_dir_all(p)?;
            }
        }
        let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{v}")
    })();
    if let Err(e) = r {
        eprintln!("agentgc run: cannot write log {}: {e}", path.display());
    }
}

/// Returns the exit code of the agentgc process.
pub fn cmd_run(a: &RunArgs) -> Result<i32, String> {
    let cfg = resolve(a)?;
    if a.status {
        return status(&cfg, a);
    }
    if a.command.is_empty() {
        return Err("no command: use `agentgc run [flags] -- COMMAND [ARGS...]`".into());
    }
    let command = &a.command;

    // CI: no queue.
    if env_nonempty("CI").is_some() && !a.no_skip_ci {
        return Ok(spawn(command, None));
    }

    // Nested call: resources already held by a parent run are not taken again.
    let mut held_map = parse_held(&std::env::var("AGENTGC_HELD").unwrap_or_default());
    let have = held_map.get(&cfg.pool).cloned().unwrap_or_default();
    let need: Vec<String> = cfg.resources.iter().filter(|r| !have.contains(r)).cloned().collect();
    if need.is_empty() {
        return Ok(spawn(command, None));
    }

    let started = now_secs();
    let (held, waited, timed_out) = match acquire_all(&cfg, &need, command)? {
        Acquired::Got(h, w) => (h, w, false),
        Acquired::TimedOut(w) => (Vec::new(), w, true),
    };
    let mut ran = Duration::ZERO;
    let code = if timed_out && cfg.on_timeout == OnTimeout::Fail {
        eprintln!("agentgc run: gave up waiting after {:.1} s", waited.as_secs_f64());
        EXIT_TIMEOUT
    } else {
        if timed_out {
            eprintln!("agentgc run: gave up waiting after {:.1} s, running without slots", waited.as_secs_f64());
        } else {
            let e = held_map.entry(cfg.pool.clone()).or_default();
            for r in &need {
                if !e.contains(r) {
                    e.push(r.clone());
                }
            }
        }
        let env_val = format_held(&held_map);
        let t = Instant::now();
        let c = spawn(command, if env_val.is_empty() { None } else { Some(&env_val) });
        ran = t.elapsed();
        c
    };
    release(held);

    if let Some(lp) = &cfg.log {
        let cwd = std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default();
        write_log(
            lp,
            &serde_json::json!({
                "start": started,
                "pool": cfg.pool,
                "resources": cfg.resources,
                "command": command,
                "cwd": cwd,
                "slots": cfg.slots,
                "waited_ms": waited.as_millis() as u64,
                "ran_ms": ran.as_millis() as u64,
                "timed_out": timed_out,
                "exit": code,
            }),
        );
    }
    Ok(code)
}

// ---------- status ----------

#[derive(Serialize)]
struct SlotView {
    slot: usize,
    held: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    age_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<String>,
}

#[derive(Serialize)]
struct ResView {
    resource: String,
    slots: Vec<SlotView>,
}

#[derive(Serialize)]
struct PoolView {
    pool: String,
    resources: Vec<ResView>,
}

fn collect_status(dir: &Path, only_pool: Option<&str>) -> Vec<PoolView> {
    let mut pools = Vec::new();
    let mut pdirs: Vec<_> = match fs::read_dir(dir) {
        Ok(rd) => rd.flatten().filter(|e| e.path().is_dir()).collect(),
        Err(_) => return pools,
    };
    pdirs.sort_by_key(|e| e.file_name());
    for pd in pdirs {
        let pname = pd.file_name().to_string_lossy().to_string();
        if only_pool.is_some_and(|p| p != pname) {
            continue;
        }
        let mut map: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        if let Ok(rd) = fs::read_dir(pd.path()) {
            for f in rd.flatten() {
                let n = f.file_name().to_string_lossy().to_string();
                let Some(stem) = n.strip_suffix(".lock") else { continue };
                let Some((r, i)) = stem.rsplit_once('.') else { continue };
                if let Ok(i) = i.parse::<usize>() {
                    map.entry(r.to_string()).or_default().push(i);
                }
            }
        }
        let mut resources = Vec::new();
        for (r, mut idx) in map {
            idx.sort_unstable();
            let slots = idx
                .into_iter()
                .map(|i| {
                    let p = lock::slot_path(dir, &pname, &r, i);
                    let held = lock::is_held(&p).unwrap_or(false);
                    let inf = if held { read_info(&p) } else { None };
                    SlotView {
                        slot: i,
                        held,
                        pid: inf.as_ref().map(|x| x.pid),
                        age_secs: inf.as_ref().map(|x| now_secs().saturating_sub(x.start)),
                        command: inf.map(|x| x.command),
                    }
                })
                .collect();
            resources.push(ResView { resource: r, slots });
        }
        if !resources.is_empty() {
            pools.push(PoolView { pool: pname, resources });
        }
    }
    pools
}

fn status(cfg: &Config, a: &RunArgs) -> Result<i32, String> {
    // Without an explicit --pool/AGENTGC_POOL, show every pool.
    let only = a.pool.clone().or_else(|| env_nonempty("AGENTGC_POOL"));
    let pools = collect_status(&cfg.dir, only.as_deref());
    if a.json {
        let v = serde_json::json!({ "dir": cfg.dir.display().to_string(), "pools": pools });
        outln!("{}", serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?);
        return Ok(0);
    }
    if pools.is_empty() {
        outln!("No slots found under {}.", cfg.dir.display());
        return Ok(0);
    }
    for p in &pools {
        outln!("pool {}", p.pool);
        for r in &p.resources {
            let n = r.slots.iter().filter(|s| s.held).count();
            outln!("  resource {}: {} of {} slots held", r.resource, n, r.slots.len());
            for s in &r.slots {
                if !s.held {
                    outln!("    slot {}  free", s.slot);
                } else {
                    match (s.pid, s.age_secs, &s.command) {
                        (Some(pid), Some(age), Some(c)) => {
                            outln!("    slot {}  held  pid {}  {}  {}", s.slot, pid, human_age(age), c)
                        }
                        _ => outln!("    slot {}  held  (no holder info)", s.slot),
                    }
                }
            }
        }
    }
    outln!("Read-only. Nothing was stopped.");
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        for ok in ["default", "phone", "a-b_c9"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "Phone", "a b", "a.b", "a/b", "ä", "a:b"] {
            assert!(!valid_name(bad), "{bad}");
        }
    }

    #[test]
    fn sorting() {
        let v = sort_resources(vec!["web".into(), "phone".into(), "db".into(), "phone".into()]);
        assert_eq!(v, ["db", "phone", "web"]);
    }

    #[test]
    fn slots_at_least_one() {
        assert!(default_slots() >= 1);
    }

    #[test]
    fn held_roundtrip() {
        let m = parse_held("default:a,b;other:c");
        assert_eq!(m["default"], ["a", "b"]);
        assert_eq!(format_held(&m), "default:a,b;other:c");
        assert!(parse_held("").is_empty());
    }
}
