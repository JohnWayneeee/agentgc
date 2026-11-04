//! Per-task supervisor, core part: service list, state file, restart policy, log rotation,
//! and the `Engine` that drives children through the `Spawner`, `ChildProc` and `Clock` traits.
//! Nothing here starts a real process; the real parts live in `sup_run.rs`, so tests use fakes.
use crate::proc_kill::{Killer, Live};
use crate::task_config as tc;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};

pub const BACKOFF_MS: [u64; 4] = [1000, 2000, 4000, 8000];
/// More restarts than this inside `WINDOW_MS` mark the service `failed`.
pub const MAX_RESTARTS: usize = 5;
pub const WINDOW_MS: u64 = 60_000;
/// Wait between the graceful stop and the kill.
pub const GRACE_MS: u64 = 5000;
pub const LOG_MAX: u64 = 5 * 1024 * 1024;
pub const POLL_MS: u64 = 500;

pub const STATE_RUNNING: &str = "running";
pub const STATE_BACKOFF: &str = "backoff";
pub const STATE_FINISHED: &str = "finished";
pub const STATE_FAILED: &str = "failed";
pub const STATE_STOPPED: &str = "stopped";

// ------------------------------------------------------------------ paths

pub fn agentgc_dir(wt: &Path) -> PathBuf {
    wt.join(".agentgc")
}
pub fn logs_dir(wt: &Path) -> PathBuf {
    agentgc_dir(wt).join("logs")
}
pub fn state_path(wt: &Path) -> PathBuf {
    agentgc_dir(wt).join("daemon.json")
}
pub fn lock_path(wt: &Path) -> PathBuf {
    agentgc_dir(wt).join("daemon.lock")
}
pub fn stop_path(wt: &Path) -> PathBuf {
    agentgc_dir(wt).join("daemon.stop")
}
pub fn restart_marker(wt: &Path, name: &str) -> PathBuf {
    agentgc_dir(wt).join(format!("restart.{name}"))
}

/// Drop the Windows `\\?\` prefix: `cmd.exe` refuses such a current directory.
pub fn plain(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(r) if r.as_bytes().get(1) == Some(&b':') => PathBuf::from(r),
        _ => p.to_path_buf(),
    }
}

pub fn unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ------------------------------------------------------------------ service list

/// One entry of `.agentgc/services.json`, written by `task new`.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ServiceDef {
    pub name: String,
    pub cmd: String,
    #[serde(default = "dot")]
    pub cwd: String,
    #[serde(default)]
    pub port: Option<u16>,
}

fn dot() -> String {
    ".".into()
}

pub fn parse_services(text: &str) -> Result<Vec<ServiceDef>, String> {
    let defs: Vec<ServiceDef> = serde_json::from_str(text).map_err(|e| format!("bad services.json: {e}"))?;
    let mut seen: Vec<&str> = Vec::new();
    for d in &defs {
        tc::sanitize_name(&d.name).map_err(|e| format!("services.json: service name {:?}: {e}", d.name))?;
        if d.cmd.trim().is_empty() {
            return Err(format!("services.json: service {:?} has an empty cmd", d.name));
        }
        if seen.contains(&d.name.as_str()) {
            return Err(format!("services.json: duplicate service {:?}", d.name));
        }
        seen.push(&d.name);
    }
    Ok(defs)
}

pub fn load_services(wt: &Path) -> Result<Vec<ServiceDef>, String> {
    let p = agentgc_dir(wt).join("services.json");
    let text = fs::read_to_string(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    parse_services(&text)
}

/// `KEY=VALUE` lines of `task.env`. Blank lines, `#` lines and bad keys are skipped.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for l in text.lines() {
        let l = l.trim_end_matches('\r');
        if l.trim().is_empty() || l.trim_start().starts_with('#') {
            continue;
        }
        if let Some((k, val)) = l.split_once('=') {
            if tc::valid_env_key(k) {
                v.push((k.to_string(), val.to_string()));
            }
        }
    }
    v
}

pub fn load_env(wt: &Path) -> Vec<(String, String)> {
    fs::read_to_string(agentgc_dir(wt).join("task.env")).map(|t| parse_env(&t)).unwrap_or_default()
}

/// The working dir of a service: a relative path that resolves to a folder inside the worktree.
/// `..`, drive and root parts are refused, and so is anything that resolves outside (symlinks).
pub fn resolve_cwd(wt: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if rel.is_empty()
        || p.is_absolute()
        || p.components().any(|c| matches!(c, Component::ParentDir | Component::Prefix(_) | Component::RootDir))
    {
        return Err(format!("cwd {rel:?} must be a relative path without '..'; refusing"));
    }
    let wt_c = fs::canonicalize(wt).map_err(|e| format!("cannot resolve the worktree {}: {e}", wt.display()))?;
    let c = fs::canonicalize(wt.join(rel)).map_err(|e| format!("cwd {rel:?} cannot be resolved: {e}"))?;
    if !c.starts_with(&wt_c) {
        return Err(format!("cwd {rel:?} resolves outside the worktree ({}); refusing", c.display()));
    }
    if !c.is_dir() {
        return Err(format!("cwd {rel:?} is not a folder"));
    }
    Ok(plain(&c))
}

// ------------------------------------------------------------------ state file

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SvcState {
    pub name: String,
    pub pid: Option<u32>,
    #[serde(default)]
    pub start_time: u64,
    /// Process name as the system reports it; part of the identity check.
    #[serde(default)]
    pub proc_name: String,
    #[serde(default)]
    pub restarts: u32,
    pub state: String,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub exit_code: Option<i32>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DaemonState {
    pub pid: u32,
    #[serde(default)]
    pub start_time: u64,
    #[serde(default)]
    pub proc_name: String,
    #[serde(default)]
    pub started_at: u64,
    pub services: Vec<SvcState>,
}

/// Atomic write: temp file in the same folder, then rename over `daemon.json`.
pub fn write_state(wt: &Path, st: &DaemonState) -> Result<(), String> {
    let ad = agentgc_dir(wt);
    let tmp = ad.join("daemon.json.tmp");
    let fin = state_path(wt);
    let text = serde_json::to_string_pretty(st).map_err(|e| e.to_string())?;
    let mut f = File::create(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    f.sync_all().map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    drop(f);
    fs::rename(&tmp, &fin).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", fin.display())
    })
}

pub fn read_state(wt: &Path) -> Option<DaemonState> {
    serde_json::from_str(&fs::read_to_string(state_path(wt)).ok()?).ok()
}

/// Pids named in `<root>/.agentgc/daemon.json` (supervisor and services). Read-only; for the `ps` mark.
pub fn recorded_pids(root: &Path) -> Vec<u32> {
    match read_state(root) {
        Some(st) => std::iter::once(st.pid).chain(st.services.iter().filter_map(|s| s.pid)).collect(),
        None => Vec::new(),
    }
}

// ------------------------------------------------------------------ log rotation

/// Append-only log with one old file: at `max` bytes `<name>.log` becomes `<name>.log.1`
/// (an older `.1` is replaced).
pub struct RotatingLog {
    path: PathBuf,
    old: PathBuf,
    max: u64,
    file: Option<File>,
    size: u64,
}

impl RotatingLog {
    pub fn open(path: &Path, max: u64) -> Result<RotatingLog, String> {
        let mut o = path.as_os_str().to_owned();
        o.push(".1");
        let mut l = RotatingLog { path: path.to_path_buf(), old: PathBuf::from(o), max, file: None, size: 0 };
        l.reopen().map_err(|e| format!("cannot open log {}: {e}", path.display()))?;
        Ok(l)
    }

    fn reopen(&mut self) -> io::Result<()> {
        let f = OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = f.metadata().map(|m| m.len()).unwrap_or(0);
        self.file = Some(f);
        Ok(())
    }

    fn rotate(&mut self) {
        self.file = None; // close first: Windows cannot rename an open file
        let _ = fs::remove_file(&self.old);
        let renamed = fs::rename(&self.path, &self.old).is_ok();
        let _ = self.reopen();
        if !renamed {
            self.size = 0; // do not retry on every write
        }
    }

    pub fn write(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.size > 0 && self.size + buf.len() as u64 > self.max {
            self.rotate();
        }
        if self.file.is_none() {
            self.reopen()?;
        }
        let f = self.file.as_mut().ok_or_else(|| io::Error::other("log is closed"))?;
        f.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(())
    }
}

/// The last `n` lines of `data`, lossy UTF-8. A cut first line is dropped when `cut` is true.
pub fn tail_lines(data: &[u8], n: usize, cut: bool) -> Vec<String> {
    let text = String::from_utf8_lossy(data);
    let mut lines: Vec<&str> = text.lines().collect();
    if cut && !lines.is_empty() {
        lines.remove(0);
    }
    let start = lines.len().saturating_sub(n);
    lines[start..].iter().map(|s| s.to_string()).collect()
}

// ------------------------------------------------------------------ restart policy

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Restart { after_ms: u64 },
    Failed,
    Finished,
}

/// Backoff 1, 2, 4, 8, 8 s. A crash while 5 restarts sit inside the last 60 s means `failed`.
/// Exit code 0 means finished: no restart.
#[derive(Debug, Default)]
pub struct RestartPolicy {
    recent: Vec<u64>,
}

impl RestartPolicy {
    pub fn on_exit(&mut self, now_ms: u64, code: Option<i32>) -> Verdict {
        if code == Some(0) {
            return Verdict::Finished;
        }
        self.recent.retain(|t| now_ms.saturating_sub(*t) < WINDOW_MS);
        if self.recent.len() >= MAX_RESTARTS {
            return Verdict::Failed;
        }
        let after_ms = BACKOFF_MS[self.recent.len().min(BACKOFF_MS.len() - 1)];
        self.recent.push(now_ms);
        Verdict::Restart { after_ms }
    }

    pub fn reset(&mut self) {
        self.recent.clear();
    }
}

// ------------------------------------------------------------------ engine

pub trait ChildProc {
    fn pid(&self) -> u32;
    fn start_time(&self) -> u64;
    fn proc_name(&self) -> String;
    /// `Some(code)` once the process has ended (`code` is `None` after a signal).
    fn try_wait(&mut self) -> Option<Option<i32>>;
    /// Stop it (its whole tree). `force` = kill. Returns false when a graceful stop is not supported.
    fn terminate(&mut self, force: bool) -> bool;
}

pub trait Spawner {
    fn spawn(&mut self, def: &ServiceDef, cwd: &Path, env: &[(String, String)]) -> Result<Box<dyn ChildProc>, String>;
}

pub trait Clock {
    fn now_ms(&self) -> u64;
    fn sleep_ms(&self, ms: u64);
}

struct Svc {
    def: ServiceDef,
    cwd: PathBuf,
    policy: RestartPolicy,
    child: Option<Box<dyn ChildProc>>,
    st: SvcState,
    retry_at: Option<u64>,
}

pub struct Engine<'a> {
    svcs: Vec<Svc>,
    env: Vec<(String, String)>,
    spawner: &'a mut dyn Spawner,
    clock: &'a dyn Clock,
}

impl<'a> Engine<'a> {
    pub fn new(
        defs: Vec<(ServiceDef, PathBuf)>,
        env: Vec<(String, String)>,
        spawner: &'a mut dyn Spawner,
        clock: &'a dyn Clock,
    ) -> Engine<'a> {
        let svcs = defs
            .into_iter()
            .map(|(def, cwd)| Svc {
                st: SvcState {
                    name: def.name.clone(),
                    pid: None,
                    start_time: 0,
                    proc_name: String::new(),
                    restarts: 0,
                    state: STATE_BACKOFF.into(),
                    port: def.port,
                    exit_code: None,
                    error: None,
                },
                def,
                cwd,
                policy: RestartPolicy::default(),
                child: None,
                retry_at: None,
            })
            .collect();
        Engine { svcs, env, spawner, clock }
    }

    pub fn start_all(&mut self) {
        for i in 0..self.svcs.len() {
            self.start(i);
        }
    }

    fn start(&mut self, i: usize) {
        let now = self.clock.now_ms();
        let s = &mut self.svcs[i];
        match self.spawner.spawn(&s.def, &s.cwd, &self.env) {
            Ok(c) => {
                s.st.pid = Some(c.pid());
                s.st.start_time = c.start_time();
                s.st.proc_name = c.proc_name();
                s.st.state = STATE_RUNNING.into();
                s.st.exit_code = None;
                s.st.error = None;
                s.child = Some(c);
                s.retry_at = None;
            }
            Err(e) => {
                s.st.pid = None;
                s.st.error = Some(e);
                Self::exited(s, now, None);
            }
        }
    }

    fn exited(s: &mut Svc, now: u64, code: Option<i32>) {
        s.child = None;
        s.st.pid = None;
        s.st.exit_code = code;
        match s.policy.on_exit(now, code) {
            Verdict::Finished => s.st.state = STATE_FINISHED.into(),
            Verdict::Failed => s.st.state = STATE_FAILED.into(),
            Verdict::Restart { after_ms } => {
                s.st.state = STATE_BACKOFF.into();
                s.retry_at = Some(now + after_ms);
            }
        }
    }

    /// One pass: collect exits, start restarts that are due, serve manual restart requests.
    pub fn tick(&mut self, requests: &[String]) {
        for name in requests {
            if let Some(i) = self.svcs.iter().position(|s| &s.def.name == name) {
                self.manual_restart(i);
            }
        }
        let now = self.clock.now_ms();
        for i in 0..self.svcs.len() {
            let s = &mut self.svcs[i];
            if let Some(c) = s.child.as_mut() {
                if let Some(code) = c.try_wait() {
                    Self::exited(s, now, code);
                }
            } else if s.st.state == STATE_BACKOFF && s.retry_at.map_or(false, |t| now >= t) {
                s.st.restarts += 1;
                self.start(i);
            }
        }
    }

    fn manual_restart(&mut self, i: usize) {
        if self.svcs[i].child.is_some() {
            self.stop_children(&[i]);
        }
        let s = &mut self.svcs[i];
        s.policy.reset();
        s.st.restarts += 1;
        self.start(i);
    }

    /// Stop the listed children: graceful first, kill after `GRACE_MS`.
    fn stop_children(&mut self, idx: &[usize]) {
        for &i in idx {
            if let Some(c) = self.svcs[i].child.as_mut() {
                if !c.terminate(false) {
                    c.terminate(true);
                }
            }
        }
        let mut waited = 0;
        loop {
            let mut any = false;
            for &i in idx {
                let s = &mut self.svcs[i];
                if let Some(c) = s.child.as_mut() {
                    if c.try_wait().is_some() {
                        s.child = None;
                    } else {
                        any = true;
                    }
                }
            }
            if !any || waited >= GRACE_MS {
                break;
            }
            self.clock.sleep_ms(100);
            waited += 100;
        }
        for &i in idx {
            let s = &mut self.svcs[i];
            if let Some(c) = s.child.as_mut() {
                c.terminate(true);
                for _ in 0..20 {
                    if c.try_wait().is_some() {
                        break;
                    }
                    self.clock.sleep_ms(100);
                }
            }
            s.child = None;
        }
    }

    /// Stop every running service and mark it `stopped`.
    pub fn stop_all(&mut self) {
        let idx: Vec<usize> = (0..self.svcs.len()).filter(|i| self.svcs[*i].child.is_some()).collect();
        self.stop_children(&idx);
        for i in idx {
            let s = &mut self.svcs[i];
            s.st.pid = None;
            s.st.state = STATE_STOPPED.into();
        }
    }

    /// No service runs and none waits for a restart.
    pub fn all_done(&self) -> bool {
        self.svcs.iter().all(|s| s.child.is_none() && (s.st.state == STATE_FINISHED || s.st.state == STATE_FAILED))
    }

    pub fn snapshot(&self) -> Vec<SvcState> {
        self.svcs.iter().map(|s| s.st.clone()).collect()
    }
}

// ------------------------------------------------------------------ identity check before a signal

/// What `down` did for one recorded pid.
#[derive(Debug, PartialEq)]
pub struct Outcome {
    pub what: String,
    pub pid: u32,
    /// `Ok(note)` signaled or already gone; `Err(why)` refused or failed.
    pub result: Result<&'static str, String>,
}

/// The process now must be the one the supervisor recorded: pid, start time and name.
/// An unreadable start time or name means it cannot be verified, so it is refused.
pub fn check_identity(live: &Live, start_time: u64, proc_name: &str) -> Result<(), String> {
    if start_time == 0 || proc_name.is_empty() {
        return Err("identity was not recorded".into());
    }
    if live.start_time != start_time {
        return Err("start time changed (pid was reused)".into());
    }
    if live.name != proc_name {
        return Err(format!("name changed to {:?}", live.name));
    }
    Ok(())
}

fn stop_one(k: &dyn Killer, pid: u32, start_time: u64, name: &str, force: bool) -> Result<&'static str, String> {
    if pid == std::process::id() {
        return Err("refusing to signal agentgc itself".into());
    }
    let Some(live) = k.read(pid) else { return Ok("already gone") };
    check_identity(&live, start_time, name)?;
    k.signal(pid, force)?;
    Ok("signaled")
}

/// Signal only the pids recorded in `daemon.json`, supervisor first, each after a fresh identity check.
pub fn stop_recorded(k: &dyn Killer, st: &DaemonState, force: bool) -> Vec<Outcome> {
    let mut v = Vec::new();
    v.push(Outcome {
        what: "supervisor".into(),
        pid: st.pid,
        result: stop_one(k, st.pid, st.start_time, &st.proc_name, force),
    });
    for s in &st.services {
        if let Some(pid) = s.pid {
            v.push(Outcome {
                what: format!("service {}", s.name),
                pid,
                result: stop_one(k, pid, s.start_time, &s.proc_name, force),
            });
        }
    }
    v
}

// ------------------------------------------------------------------ cleanup for `task done`

/// Remove the generated daemon files in `<worktree>/.agentgc`: only known names, `remove_file` only,
/// then `remove_dir` for `logs` (never recursive). Returns what was deleted.
pub fn remove_daemon_files(ad: &Path) -> Result<Vec<PathBuf>, String> {
    let mut gone = Vec::new();
    let mut rm = |p: PathBuf| -> Result<(), String> {
        if fs::symlink_metadata(&p).map_or(false, |m| m.is_file()) {
            fs::remove_file(&p).map_err(|e| format!("cannot delete {}: {e}", p.display()))?;
            gone.push(p);
        }
        Ok(())
    };
    for n in ["daemon.json", "daemon.json.tmp", "daemon.lock", "daemon.stop"] {
        rm(ad.join(n))?;
    }
    if let Ok(rd) = fs::read_dir(ad) {
        let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        for n in names {
            if let Some(svc) = n.strip_prefix("restart.") {
                if tc::sanitize_name(svc).is_ok() {
                    rm(ad.join(&n))?;
                }
            }
        }
    }
    let logs = ad.join("logs");
    if fs::symlink_metadata(&logs).map_or(false, |m| m.is_dir()) {
        if let Ok(rd) = fs::read_dir(&logs) {
            let mut names: Vec<String> = rd.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            for n in names {
                if n.ends_with(".log") || n.ends_with(".log.1") {
                    rm(logs.join(&n))?;
                }
            }
        }
        let _ = fs::remove_dir(&logs); // fails when unknown files remain: they stay
    }
    Ok(gone)
}

// ------------------------------------------------------------------ tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::collections::HashMap;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::Duration;

    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("agentgc-sup-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- fakes

    struct FakeClock(Cell<u64>);
    impl Clock for FakeClock {
        fn now_ms(&self) -> u64 {
            self.0.get()
        }
        fn sleep_ms(&self, ms: u64) {
            self.0.set(self.0.get() + ms);
        }
    }

    /// How a fake child behaves.
    #[derive(Clone, Copy)]
    enum Mode {
        /// Exits with this code on the first `try_wait` after `after_ms` of life.
        ExitAfter(u64, i32),
        /// Runs until a graceful terminate.
        Graceful,
        /// Ignores a graceful terminate, dies on kill.
        Stubborn,
        /// Graceful terminate is not supported.
        NoGraceful,
    }

    #[derive(Default)]
    struct Log {
        spawns: Vec<u64>,
        terminates: Vec<(u32, bool)>,
    }

    struct FakeChild {
        pid: u32,
        born: u64,
        mode: Mode,
        dead: Option<i32>,
        clock: Rc<FakeClock>,
        log: Rc<RefCell<Log>>,
    }

    impl ChildProc for FakeChild {
        fn pid(&self) -> u32 {
            self.pid
        }
        fn start_time(&self) -> u64 {
            1000 + self.pid as u64
        }
        fn proc_name(&self) -> String {
            "fake".into()
        }
        fn try_wait(&mut self) -> Option<Option<i32>> {
            if let (None, Mode::ExitAfter(after, code)) = (self.dead, self.mode) {
                if self.clock.now_ms() >= self.born + after {
                    self.dead = Some(code);
                }
            }
            self.dead.map(Some)
        }
        fn terminate(&mut self, force: bool) -> bool {
            self.log.borrow_mut().terminates.push((self.pid, force));
            match self.mode {
                Mode::Graceful if !force => self.dead = Some(0),
                Mode::Stubborn if force => self.dead = Some(1),
                Mode::NoGraceful if force => self.dead = Some(1),
                Mode::NoGraceful => return false,
                Mode::ExitAfter(..) => self.dead = Some(1),
                _ => {}
            }
            true
        }
    }

    struct FakeSpawner {
        mode: Mode,
        next_pid: u32,
        clock: Rc<FakeClock>,
        log: Rc<RefCell<Log>>,
        fail_spawn: bool,
    }

    impl Spawner for FakeSpawner {
        fn spawn(&mut self, _d: &ServiceDef, _c: &Path, _e: &[(String, String)]) -> Result<Box<dyn ChildProc>, String> {
            self.log.borrow_mut().spawns.push(self.clock.now_ms());
            if self.fail_spawn {
                return Err("cannot start".into());
            }
            self.next_pid += 1;
            Ok(Box::new(FakeChild {
                pid: self.next_pid,
                born: self.clock.now_ms(),
                mode: self.mode,
                dead: None,
                clock: self.clock.clone(),
                log: self.log.clone(),
            }))
        }
    }

    fn def(name: &str) -> (ServiceDef, PathBuf) {
        (ServiceDef { name: name.into(), cmd: "x".into(), cwd: ".".into(), port: None }, PathBuf::from("."))
    }

    struct Rig {
        clock: Rc<FakeClock>,
        log: Rc<RefCell<Log>>,
        sp: FakeSpawner,
    }

    fn rig(mode: Mode) -> Rig {
        let clock = Rc::new(FakeClock(Cell::new(0)));
        let log = Rc::new(RefCell::new(Log::default()));
        let sp = FakeSpawner { mode, next_pid: 100, clock: clock.clone(), log: log.clone(), fail_spawn: false };
        Rig { clock, log, sp }
    }

    /// Tick every 500 ms until `ms` of fake time pass.
    fn run_for(e: &mut Engine, clock: &FakeClock, ms: u64) {
        let end = clock.now_ms() + ms;
        while clock.now_ms() < end {
            clock.sleep_ms(POLL_MS);
            e.tick(&[]);
        }
    }

    // ---- restart policy

    #[test]
    fn backoff_then_failed_after_five_restarts_in_a_minute() {
        let mut p = RestartPolicy::default();
        let mut got = Vec::new();
        for i in 0..5 {
            got.push(p.on_exit(i * 100, Some(1)));
        }
        let after: Vec<u64> = got
            .iter()
            .map(|v| match v {
                Verdict::Restart { after_ms } => *after_ms,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(after, vec![1000, 2000, 4000, 8000, 8000]);
        assert_eq!(p.on_exit(600, Some(1)), Verdict::Failed);
        assert_eq!(p.on_exit(700, None), Verdict::Failed);
    }

    #[test]
    fn crashes_spread_over_more_than_a_minute_keep_restarting() {
        let mut p = RestartPolicy::default();
        for i in 0..20u64 {
            assert!(matches!(p.on_exit(i * 61_000, Some(2)), Verdict::Restart { after_ms: 1000 }));
        }
    }

    #[test]
    fn exit_zero_is_finished_and_signal_death_is_a_crash() {
        let mut p = RestartPolicy::default();
        assert_eq!(p.on_exit(0, Some(0)), Verdict::Finished);
        assert!(matches!(p.on_exit(1, None), Verdict::Restart { .. }));
        p.reset();
        assert!(matches!(p.on_exit(2, Some(1)), Verdict::Restart { after_ms: 1000 }));
    }

    // ---- engine with fake spawner and clock

    #[test]
    fn crashing_service_restarts_with_backoff_then_fails() {
        let mut r = rig(Mode::ExitAfter(0, 1));
        let mut e = Engine::new(vec![def("web")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        run_for(&mut e, &r.clock, 120_000);
        let spawns = r.log.borrow().spawns.clone();
        assert_eq!(spawns.len(), 6, "{spawns:?}");
        // gaps between starts follow 1, 2, 4, 8, 8 s (+ up to one 500 ms poll)
        let gaps: Vec<u64> = spawns.windows(2).map(|w| w[1] - w[0]).collect();
        for (g, want) in gaps.iter().zip([1000u64, 2000, 4000, 8000, 8000]) {
            assert!(*g >= want && *g <= want + POLL_MS, "gap {g} want {want}: {gaps:?}");
        }
        let s = &e.snapshot()[0];
        assert_eq!(s.state, STATE_FAILED);
        assert_eq!(s.restarts, 5);
        assert!(e.all_done());
    }

    #[test]
    fn exit_zero_is_not_restarted() {
        let mut r = rig(Mode::ExitAfter(1000, 0));
        let mut e = Engine::new(vec![def("job")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        assert!(!e.all_done());
        run_for(&mut e, &r.clock, 10_000);
        assert_eq!(r.log.borrow().spawns.len(), 1);
        let s = &e.snapshot()[0];
        assert_eq!((s.state.as_str(), s.restarts, s.exit_code), (STATE_FINISHED, 0, Some(0)));
        assert!(e.all_done());
    }

    #[test]
    fn spawn_error_counts_as_a_crash() {
        let mut r = rig(Mode::Graceful);
        r.sp.fail_spawn = true;
        let mut e = Engine::new(vec![def("web")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        let s = &e.snapshot()[0];
        assert_eq!(s.state, STATE_BACKOFF);
        assert_eq!(s.error.as_deref(), Some("cannot start"));
        run_for(&mut e, &r.clock, 120_000);
        assert_eq!(e.snapshot()[0].state, STATE_FAILED);
    }

    #[test]
    fn manual_restart_revives_a_failed_service() {
        let mut r = rig(Mode::ExitAfter(0, 1));
        let mut e = Engine::new(vec![def("web")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        run_for(&mut e, &r.clock, 120_000);
        assert_eq!(e.snapshot()[0].state, STATE_FAILED);
        r.clock.sleep_ms(500);
        e.tick(&["web".to_string(), "nope".to_string()]);
        let s = &e.snapshot()[0];
        assert_ne!(s.state, STATE_FAILED); // started again (it crashes at once, so it is in backoff)
        assert_eq!(s.restarts, 6);
        assert_eq!(r.log.borrow().spawns.len(), 7);
    }

    #[test]
    fn manual_restart_stops_the_old_child_first() {
        let mut r = rig(Mode::Graceful);
        let mut e = Engine::new(vec![def("web")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        e.tick(&["web".to_string()]);
        assert_eq!(r.log.borrow().terminates, vec![(101, false)]);
        assert_eq!(e.snapshot()[0].pid, Some(102));
    }

    #[test]
    fn stop_all_is_graceful_when_it_works() {
        let mut r = rig(Mode::Graceful);
        let mut e = Engine::new(vec![def("a"), def("b")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        e.stop_all();
        let t = r.log.borrow().terminates.clone();
        assert!(t.iter().all(|(_, force)| !force), "{t:?}");
        assert!(e.snapshot().iter().all(|s| s.state == STATE_STOPPED && s.pid.is_none()));
    }

    #[test]
    fn stop_all_kills_after_five_seconds() {
        let mut r = rig(Mode::Stubborn);
        let mut e = Engine::new(vec![def("a")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        let t0 = r.clock.now_ms();
        e.stop_all();
        assert_eq!(r.log.borrow().terminates, vec![(101, false), (101, true)]);
        let waited = r.clock.now_ms() - t0;
        assert!(waited >= GRACE_MS, "waited {waited}");
        assert_eq!(e.snapshot()[0].state, STATE_STOPPED);
    }

    #[test]
    fn stop_all_kills_at_once_when_graceful_is_unsupported() {
        let mut r = rig(Mode::NoGraceful);
        let mut e = Engine::new(vec![def("a")], vec![], &mut r.sp, &*r.clock);
        e.start_all();
        e.stop_all();
        assert_eq!(r.log.borrow().terminates, vec![(101, false), (101, true)]);
        assert!(r.clock.now_ms() < GRACE_MS);
    }

    // ---- log rotation

    #[test]
    fn log_rotates_and_keeps_one_old_file() {
        let d = tmp("log");
        let p = d.join("web.log");
        let mut l = RotatingLog::open(&p, 10).unwrap();
        l.write(b"aaaaaa").unwrap(); // 6
        l.write(b"bbbbbb").unwrap(); // would be 12 > 10: rotate first
        assert_eq!(fs::read_to_string(d.join("web.log.1")).unwrap(), "aaaaaa");
        assert_eq!(fs::read_to_string(&p).unwrap(), "bbbbbb");
        l.write(b"cccccc").unwrap(); // rotate again: the old file is replaced
        assert_eq!(fs::read_to_string(d.join("web.log.1")).unwrap(), "bbbbbb");
        assert_eq!(fs::read_to_string(&p).unwrap(), "cccccc");
        assert!(!d.join("web.log.2").exists());
        // reopening an existing file continues its size
        drop(l);
        let mut l = RotatingLog::open(&p, 10).unwrap();
        l.write(b"dddddd").unwrap();
        assert_eq!(fs::read_to_string(d.join("web.log.1")).unwrap(), "cccccc");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        assert_eq!(tail_lines(b"a\nb\nc\nd\n", 2, false), vec!["c", "d"]);
        assert_eq!(tail_lines(b"a\nb", 5, false), vec!["a", "b"]);
        assert_eq!(tail_lines(b"cut\nb\nc", 5, true), vec!["b", "c"]);
        assert!(tail_lines(b"", 5, false).is_empty());
    }

    // ---- state file

    fn sample_state() -> DaemonState {
        DaemonState {
            pid: 10,
            start_time: 99,
            proc_name: "agentgc.exe".into(),
            started_at: 5,
            services: vec![SvcState {
                name: "web".into(),
                pid: Some(11),
                start_time: 100,
                proc_name: "cmd.exe".into(),
                restarts: 2,
                state: STATE_RUNNING.into(),
                port: Some(3100),
                exit_code: None,
                error: None,
            }],
        }
    }

    #[test]
    fn state_round_trip_is_atomic() {
        let wt = tmp("state");
        fs::create_dir_all(agentgc_dir(&wt)).unwrap();
        assert!(read_state(&wt).is_none());
        let mut st = sample_state();
        write_state(&wt, &st).unwrap();
        assert_eq!(read_state(&wt).unwrap(), st);
        st.services[0].restarts = 3;
        st.services[0].state = STATE_BACKOFF.into();
        write_state(&wt, &st).unwrap();
        assert_eq!(read_state(&wt).unwrap(), st);
        assert!(!agentgc_dir(&wt).join("daemon.json.tmp").exists());
        fs::write(state_path(&wt), "{ not json").unwrap();
        assert!(read_state(&wt).is_none());
        let _ = fs::remove_dir_all(&wt);
    }

    // ---- service list, env, cwd

    #[test]
    fn services_json_is_validated() {
        let ok = r#"[{"name":"web","cmd":"npm run dev","cwd":".","port":3100},{"name":"api","cmd":"x","cwd":"apps/api","port":null}]"#;
        let v = parse_services(ok).unwrap();
        assert_eq!((v[0].port, v[1].port), (Some(3100), None));
        assert!(parse_services(r#"[{"name":"Web","cmd":"x"}]"#).is_err());
        assert!(parse_services(r#"[{"name":"../x","cmd":"x"}]"#).is_err());
        assert!(parse_services(r#"[{"name":"web","cmd":"  "}]"#).is_err());
        assert!(parse_services(r#"[{"name":"web","cmd":"x"},{"name":"web","cmd":"y"}]"#).is_err());
        assert!(parse_services("nope").is_err());
        assert_eq!(parse_services(r#"[{"name":"web","cmd":"x"}]"#).unwrap()[0].cwd, ".");
    }

    #[test]
    fn env_lines_parse() {
        let v = parse_env("TASK_ID=demo\n# c\n\nBAD KEY=1\nA=b=c\r\nPORT_WEB=3100\n");
        assert_eq!(
            v,
            vec![("TASK_ID".into(), "demo".into()), ("A".into(), "b=c".into()), ("PORT_WEB".into(), "3100".into())]
        );
    }

    #[test]
    fn cwd_outside_the_worktree_is_refused() {
        let base = tmp("cwd");
        let wt = base.join("wt");
        fs::create_dir_all(wt.join("apps/api")).unwrap();
        fs::create_dir_all(base.join("other")).unwrap();
        fs::write(wt.join("file.txt"), "x").unwrap();
        assert!(resolve_cwd(&wt, ".").is_ok());
        let api = resolve_cwd(&wt, "apps/api").unwrap();
        assert!(api.ends_with("api"));
        assert!(resolve_cwd(&wt, "./apps/../apps/api").is_err()); // `..` anywhere is refused
        for bad in ["..", "../other", "apps/../../other", "", "missing", "file.txt"] {
            assert!(resolve_cwd(&wt, bad).is_err(), "{bad:?} accepted");
        }
        let abs_out = base.join("other");
        let e = resolve_cwd(&wt, &abs_out.to_string_lossy()).unwrap_err();
        assert!(e.contains("relative"), "{e}");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn cwd_symlink_out_of_the_worktree_is_refused() {
        let base = tmp("cwdlink");
        let wt = base.join("wt");
        fs::create_dir_all(&wt).unwrap();
        fs::create_dir_all(base.join("other")).unwrap();
        std::os::unix::fs::symlink(base.join("other"), wt.join("link")).unwrap();
        assert!(resolve_cwd(&wt, "link").unwrap_err().contains("outside"));
        let _ = fs::remove_dir_all(&base);
    }

    // ---- identity check before a signal

    struct FakeKiller {
        live: RefCell<HashMap<u32, Live>>,
        signals: RefCell<Vec<(u32, bool)>>,
    }
    impl Killer for FakeKiller {
        fn read(&self, pid: u32) -> Option<Live> {
            self.live.borrow().get(&pid).cloned()
        }
        fn signal(&self, pid: u32, force: bool) -> Result<(), String> {
            self.signals.borrow_mut().push((pid, force));
            Ok(())
        }
        fn alive(&self, pid: u32) -> bool {
            self.live.borrow().contains_key(&pid)
        }
        fn sleep(&self, _d: Duration) {}
    }

    fn killer_for(st: &DaemonState) -> FakeKiller {
        let mut m = HashMap::new();
        m.insert(st.pid, Live { name: st.proc_name.clone(), cwd: None, start_time: st.start_time });
        for s in &st.services {
            m.insert(s.pid.unwrap(), Live { name: s.proc_name.clone(), cwd: None, start_time: s.start_time });
        }
        FakeKiller { live: RefCell::new(m), signals: RefCell::new(Vec::new()) }
    }

    #[test]
    fn recorded_pids_are_signaled_supervisor_first() {
        let st = sample_state();
        let k = killer_for(&st);
        let out = stop_recorded(&k, &st, true);
        assert!(out.iter().all(|o| o.result == Ok("signaled")), "{out:?}");
        assert_eq!(*k.signals.borrow(), vec![(10, true), (11, true)]);
    }

    #[test]
    fn changed_identity_is_never_signaled() {
        for change in 0..2 {
            let st = sample_state();
            let k = killer_for(&st);
            {
                let mut l = k.live.borrow_mut();
                for pid in [10u32, 11] {
                    let e = l.get_mut(&pid).unwrap();
                    if change == 0 {
                        e.start_time += 1;
                    } else {
                        e.name = "notepad.exe".into();
                    }
                }
            }
            let out = stop_recorded(&k, &st, true);
            assert!(out.iter().all(|o| o.result.is_err()), "{out:?}");
            assert!(k.signals.borrow().is_empty(), "signal sent on change {change}");
        }
    }

    #[test]
    fn unrecorded_identity_gone_and_self_are_handled() {
        let mut st = sample_state();
        st.services[0].start_time = 0; // could not be read at start
        let k = killer_for(&st);
        let out = stop_recorded(&k, &st, false);
        assert!(out[1].result.is_err());
        // a gone process is not an error and not signaled
        k.live.borrow_mut().remove(&10);
        let out = stop_recorded(&k, &st, false);
        assert_eq!(out[0].result, Ok("already gone"));
        // agentgc itself is never signaled
        let mut me = sample_state();
        me.pid = std::process::id();
        let k = killer_for(&me);
        let out = stop_recorded(&k, &me, true);
        assert!(out[0].result.is_err());
        assert!(!k.signals.borrow().iter().any(|(p, _)| *p == std::process::id()));
    }

    // ---- cleanup list for `task done`

    #[test]
    fn cleanup_removes_only_known_daemon_files() {
        let wt = tmp("clean");
        let ad = agentgc_dir(&wt);
        fs::create_dir_all(ad.join("logs")).unwrap();
        for f in [
            "daemon.json", "daemon.lock", "daemon.stop", "restart.web", "logs/web.log", "logs/web.log.1", "logs/supervisor.log",
        ] {
            fs::write(ad.join(f), "x").unwrap();
        }
        for keep in ["task.env", "services.json", "restart.Bad Name", "logs/notes.txt"] {
            fs::write(ad.join(keep), "x").unwrap();
        }
        fs::create_dir_all(ad.join("restart.dir")).unwrap(); // a folder is never removed
        let gone = remove_daemon_files(&ad).unwrap();
        assert_eq!(gone.len(), 7, "{gone:?}");
        for keep in ["task.env", "services.json", "restart.Bad Name", "logs/notes.txt"] {
            assert!(ad.join(keep).exists(), "{keep} was removed");
        }
        assert!(ad.join("restart.dir").is_dir());
        assert!(ad.join("logs").is_dir(), "logs holds an unknown file, so it stays");
        // with nothing unknown left, the logs folder goes (remove_dir)
        fs::remove_file(ad.join("logs/notes.txt")).unwrap();
        remove_daemon_files(&ad).unwrap();
        assert!(!ad.join("logs").exists());
        let _ = fs::remove_dir_all(&wt);
    }
}
