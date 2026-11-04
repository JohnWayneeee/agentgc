//! `agentgc up | down | status | logs | restart`: the user-facing side of the per-task supervisor.
//! The supervisor itself is `agentgc __supervise <worktree>` (see `sup_run.rs`).
use crate::lock;
use crate::out::outln;
use crate::proc_kill::{wait_gone, Killer, SysKiller};
use crate::sup::*;
use crate::task::{self, Entry};
use crate::task_config as tc;
use crate::task_done;
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

const START_WAIT_MS: u64 = 10_000;
const STOP_WAIT_MS: u64 = 10_000;
const TAIL_BYTES: u64 = 1024 * 1024;

/// Which task: the one around the current folder, or `--task NAME` from the registry.
#[derive(clap::Args, Debug, Clone, Default)]
pub struct Target {
    /// Task name from the registry (`<repo>/.agentgc/tasks/<name>.json`). Default: the task folder you are in.
    #[arg(long, value_name = "NAME")]
    pub task: Option<String>,
    /// With --task: the repo to read the registry from. Default: current directory.
    #[arg(long, requires = "task")]
    pub repo: Option<PathBuf>,
}

/// The worktree of the task. Never a path outside the registry's task folder.
pub fn resolve(t: &Target) -> Result<PathBuf, String> {
    match &t.task {
        Some(name) => {
            let name = tc::sanitize_name(name)?;
            let root = task::find_repo_root(t.repo.as_deref().unwrap_or(Path::new(".")))?;
            let (cfg, _) = task::load_config(&root, None)?;
            let reg = task::tasks_dir(&root).join(format!("{name}.json"));
            let text = fs::read_to_string(&reg)
                .map_err(|e| format!("task {name:?} is not in the registry ({}): {e}", reg.display()))?;
            let entry: Entry =
                serde_json::from_str(&text).map_err(|e| format!("bad registry entry {}: {e}", reg.display()))?;
            if entry.name != name {
                return Err(format!("registry entry {} holds the name {:?}; refusing", reg.display(), entry.name));
            }
            let target = task_done::guard(&root, &cfg.worktree.dir, &name, &entry.path)?;
            if !target.exists {
                return Err(format!("the worktree of task {name:?} is gone"));
            }
            Ok(plain(&target.path))
        }
        None => {
            let cwd = std::env::current_dir().map_err(|e| format!("cannot read the current folder: {e}"))?;
            for d in cwd.ancestors() {
                if d.join(".agentgc").join("services.json").is_file() {
                    let c = fs::canonicalize(d).map_err(|e| format!("cannot resolve {}: {e}", d.display()))?;
                    return Ok(plain(&c));
                }
            }
            Err("no task worktree here (no .agentgc/services.json at or above this folder); use --task NAME".into())
        }
    }
}

/// Is the supervisor of this worktree alive? The lock is the truth: the OS frees it when the holder dies.
pub fn supervisor_alive(wt: &Path) -> bool {
    lock::is_held(&lock_path(wt)).unwrap_or(false)
}

fn route_or_port(env: &[(String, String)], s: &SvcState) -> String {
    let key = crate::compose::route_env_key(&s.name);
    if let Some((_, v)) = env.iter().find(|(k, _)| *k == key) {
        return v.clone();
    }
    match s.port {
        Some(p) => format!("http://127.0.0.1:{p}"),
        None => "-".into(),
    }
}

fn print_services(wt: &Path, st: &DaemonState, supervisor_up: bool) {
    let env = load_env(wt);
    outln!("  {:<14} {:>7}  {:<9}  {:<7}  {:>8}  {:>5}  URL", "SERVICE", "PID", "STATE", "RUNNING", "RESTARTS", "PORT");
    for s in &st.services {
        let running = supervisor_up
            && s.pid.map_or(false, |p| {
                SysKiller.read(p).map_or(false, |l| check_identity(&l, s.start_time, &s.proc_name).is_ok())
            });
        outln!(
            "  {:<14} {:>7}  {:<9}  {:<7}  {:>8}  {:>5}  {}",
            s.name,
            s.pid.map_or("-".to_string(), |p| p.to_string()),
            s.state,
            if running { "yes" } else { "no" },
            s.restarts,
            s.port.map_or("-".to_string(), |p| p.to_string()),
            route_or_port(&env, s)
        );
    }
}

// ------------------------------------------------------------------ up

pub fn cmd_up(t: &Target) -> Result<(), String> {
    let wt = resolve(t)?;
    // refuse early: every service must be valid and stay inside the worktree
    let defs = load_services(&wt)?;
    if defs.is_empty() {
        return Err("services.json lists no service".into());
    }
    for d in &defs {
        resolve_cwd(&wt, &d.cwd).map_err(|e| format!("service {}: {e}", d.name))?;
    }
    if supervisor_alive(&wt) {
        let pid = read_state(&wt).map(|s| s.pid.to_string()).unwrap_or_else(|| "?".into());
        outln!("already running (supervisor pid {pid}) for {}", wt.display());
        return Ok(());
    }
    let _ = fs::remove_file(state_path(&wt)); // stale state of a dead supervisor
    fs::create_dir_all(logs_dir(&wt)).map_err(|e| format!("cannot create {}: {e}", logs_dir(&wt).display()))?;
    let sup_log = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(logs_dir(&wt).join("supervisor.log"))
        .map_err(|e| format!("cannot open supervisor.log: {e}"))?;
    let sup_err = sup_log.try_clone().map_err(|e| format!("cannot open supervisor.log: {e}"))?;
    let exe = std::env::current_exe().map_err(|e| format!("cannot find the agentgc binary: {e}"))?;
    let mut c = Command::new(exe);
    c.arg("__supervise").arg(&wt).stdin(Stdio::null()).stdout(Stdio::from(sup_log)).stderr(Stdio::from(sup_err));
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000 | 0x0000_0008); // CREATE_NO_WINDOW | DETACHED_PROCESS
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        c.process_group(0);
    }
    let mut child = c.spawn().map_err(|e| format!("cannot start the supervisor: {e}"))?;
    let pid = child.id();

    let mut waited = 0;
    loop {
        if let Some(st) = read_state(&wt) {
            if st.pid == pid {
                outln!("supervisor started (pid {pid}) for {}", wt.display());
                print_services(&wt, &st, true);
                outln!("Logs: {}   Stop: agentgc down", logs_dir(&wt).display());
                return Ok(());
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            if supervisor_alive(&wt) {
                outln!("already running for {}", wt.display());
                return Ok(());
            }
            if status.success() {
                // every service ended with exit code 0 before we looked: nothing is left to supervise
                outln!("the supervisor ran and ended already (all services finished). Logs: {}", logs_dir(&wt).display());
                return Ok(());
            }
            return Err(format!(
                "the supervisor ended at once ({status}); see {}",
                logs_dir(&wt).join("supervisor.log").display()
            ));
        }
        if waited >= START_WAIT_MS {
            return Err(format!(
                "the supervisor (pid {pid}) wrote no state in {} s; see {}",
                START_WAIT_MS / 1000,
                logs_dir(&wt).join("supervisor.log").display()
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
        waited += 100;
    }
}

// ------------------------------------------------------------------ down

fn print_outcomes(out: &[Outcome]) {
    for o in out {
        match &o.result {
            Ok(n) => outln!("  {} pid {}: {n}", o.what, o.pid),
            Err(e) => outln!("  {} pid {}: not signaled: {e}", o.what, o.pid),
        }
    }
}

fn wait_supervisor_gone(wt: &Path, total_ms: u64) -> bool {
    let mut waited = 0;
    while waited < total_ms {
        if !supervisor_alive(wt) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
        waited += 200;
    }
    !supervisor_alive(wt)
}

fn remove_own_markers(wt: &Path) {
    let _ = fs::remove_file(state_path(wt));
    let _ = fs::remove_file(stop_path(wt));
}

/// Stop the supervisor and its services. First the polite way (stop marker, 10 s), then by pid with the
/// identity check (pid + start time + name). No force unless `force`. Only pids from `daemon.json` are signaled.
/// `Ok(true)` when a supervisor was running, `Ok(false)` when there was none.
pub fn stop_supervisor(wt: &Path, force: bool, k: &dyn Killer) -> Result<bool, String> {
    let state = read_state(wt);
    if !supervisor_alive(wt) {
        if let Some(st) = state {
            outln!("supervisor is not running; daemon.json is stale. Checking its recorded pids:");
            print_outcomes(&stop_recorded(k, &st, force));
        }
        remove_own_markers(wt);
        return Ok(false);
    }
    fs::write(stop_path(wt), b"stop\n").map_err(|e| format!("cannot write {}: {e}", stop_path(wt).display()))?;
    if wait_supervisor_gone(wt, STOP_WAIT_MS) {
        remove_own_markers(wt);
        return Ok(true);
    }
    outln!("supervisor did not stop in {} s; stopping it by pid", STOP_WAIT_MS / 1000);
    let Some(st) = state.or_else(|| read_state(wt)) else {
        return Err("no daemon.json, so the supervisor identity cannot be checked; nothing was signaled".into());
    };
    let out = stop_recorded(k, &st, force);
    print_outcomes(&out);
    let mut pids = vec![st.pid];
    pids.extend(st.services.iter().filter_map(|s| s.pid));
    let alive = wait_gone(k, &pids, 3000);
    if supervisor_alive(wt) || alive.contains(&st.pid) {
        return Err(if force {
            "the supervisor is still running".into()
        } else {
            "the supervisor is still running; on Windows a stop by pid needs --force".into()
        });
    }
    if !alive.is_empty() {
        outln!("  still running after 3 s: {alive:?}");
    }
    remove_own_markers(wt);
    Ok(true)
}

pub fn cmd_down(t: &Target, force: bool) -> Result<(), String> {
    let wt = resolve(t)?;
    if stop_supervisor(&wt, force, &SysKiller)? {
        outln!("stopped the supervisor and its services for {}", wt.display());
    } else {
        outln!("supervisor is not running for {}", wt.display());
    }
    Ok(())
}

// ------------------------------------------------------------------ status, logs, restart

pub fn cmd_status(t: &Target) -> Result<(), String> {
    let wt = resolve(t)?;
    let alive = supervisor_alive(&wt);
    let state = read_state(&wt);
    outln!("worktree:   {}", wt.display());
    match (&state, alive) {
        (Some(st), true) => {
            outln!("supervisor: running (pid {}, up {} s)", st.pid, unix_secs().saturating_sub(st.started_at))
        }
        (None, true) => outln!("supervisor: starting (no daemon.json yet)"),
        (Some(_), false) => outln!("supervisor: not running (stale daemon.json)"),
        (None, false) => outln!("supervisor: not running"),
    }
    match state {
        Some(st) => print_services(&wt, &st, alive),
        None => {
            if let Ok(defs) = load_services(&wt) {
                let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
                outln!("services:   {} (not started; run `agentgc up`)", names.join(" "));
            }
        }
    }
    Ok(())
}

pub fn cmd_logs(t: &Target, name: &str, lines: usize) -> Result<(), String> {
    let name = tc::sanitize_name(name)?;
    let wt = resolve(t)?;
    let p = logs_dir(&wt).join(format!("{name}.log"));
    let mut f = fs::File::open(&p).map_err(|e| format!("no log for {name:?} ({}): {e}", p.display()))?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let cut = len > TAIL_BYTES;
    if cut {
        f.seek(SeekFrom::Start(len - TAIL_BYTES)).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
    for l in tail_lines(&buf, lines, cut) {
        outln!("{l}");
    }
    Ok(())
}

pub fn cmd_restart(t: &Target, name: &str) -> Result<(), String> {
    let name = tc::sanitize_name(name)?;
    let wt = resolve(t)?;
    let defs = load_services(&wt)?;
    if !defs.iter().any(|d| d.name == name) {
        let names: Vec<&str> = defs.iter().map(|d| d.name.as_str()).collect();
        return Err(format!("no service {name:?} (services: {})", names.join(", ")));
    }
    if !supervisor_alive(&wt) {
        return Err("the supervisor is not running; start it with `agentgc up`".into());
    }
    let m = restart_marker(&wt, &name);
    fs::write(&m, b"restart\n").map_err(|e| format!("cannot write {}: {e}", m.display()))?;
    outln!("restart of {name} requested (the supervisor picks it up within 1 s)");
    Ok(())
}
