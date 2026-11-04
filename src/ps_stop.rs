//! `agentgc ps --stop`: stop idle dev servers, language servers, tests and builds.
//! Dry run by default. Every signal goes through the `Killer` trait, so tests never touch
//! real processes.
use crate::proc_kill::{wait_gone, Killer, Live, SysKiller};
use crate::ps::{self, KindRule, ProcInfo};
use crate::scan;
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::PathBuf;

/// Kinds `--stop` may stop. `agent`, `db` and `other` never.
const STOP_KINDS: [&str; 4] = ["dev-server", "lsp", "test", "build"];
const WAIT_MS: u64 = 3000;

pub struct StopArgs {
    pub yes: bool,
    pub pids: Vec<u32>,
    pub task: Option<String>,
    pub force: bool,
}

/// What the user was shown. The identity check compares a fresh read with this.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub pid: u32,
    pub kind: String,
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub cpu_percent: f32,
    pub rss_bytes: u64,
    pub start_time: u64,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub stopped: Vec<u32>,
    pub still_running: Vec<u32>,
    pub refused: Vec<(u32, String)>,
    pub freed_bytes: u64,
}

/// The pid itself and every parent up to the root. Loops are cut.
fn own_chain(procs: &[ProcInfo], self_pid: u32) -> HashSet<u32> {
    let parent_of: HashMap<u32, Option<u32>> = procs.iter().map(|p| (p.pid, p.parent)).collect();
    let mut set = HashSet::new();
    let mut cur = Some(self_pid);
    while let Some(p) = cur {
        if !set.insert(p) {
            break;
        }
        cur = parent_of.get(&p).copied().flatten();
    }
    set
}

/// Lowercase folder name of the task root (nearest `.git`, else the cwd itself).
fn task_name(cwd: &std::path::Path) -> Option<String> {
    let cwd: PathBuf = cwd.components().collect();
    let root = ps::task_root(&cwd).unwrap_or(cwd);
    root.file_name().map(|n| n.to_string_lossy().to_lowercase())
}

/// Idle processes of a stoppable kind with a readable, non-system cwd.
/// Never the agentgc process or its parent chain. Pure: no system calls except the `.git` lookup.
pub fn select_candidates(procs: &[ProcInfo], rules: &[KindRule], self_pid: u32, task: Option<&str>) -> Vec<Candidate> {
    let chain = own_chain(procs, self_pid);
    let want = task.map(|t| t.to_lowercase());
    let mut v: Vec<Candidate> = Vec::new();
    for p in procs {
        if chain.contains(&p.pid) {
            continue;
        }
        let Some(cwd) = p.cwd.as_deref() else { continue };
        if ps::is_system_path(cwd) {
            continue;
        }
        let kind = ps::classify(rules, &p.name, &p.cmd);
        if !STOP_KINDS.contains(&kind.as_str()) || !ps::is_idle(&kind, p.cpu_percent) {
            continue;
        }
        if let Some(w) = &want {
            if task_name(cwd).as_deref() != Some(w.as_str()) {
                continue;
            }
        }
        v.push(Candidate {
            pid: p.pid,
            kind,
            name: p.name.clone(),
            cwd: p.cwd.clone(),
            cpu_percent: p.cpu_percent,
            rss_bytes: p.rss_bytes,
            start_time: p.start_time,
        });
    }
    v.sort_by_key(|c| c.pid);
    v
}

/// Pid reuse guard: the process now must be the one that was shown.
fn check_identity(live: &Live, c: &Candidate) -> Result<(), String> {
    if live.start_time != c.start_time {
        return Err("start time changed (pid was reused)".into());
    }
    if live.name != c.name {
        return Err(format!("name changed to {:?}", live.name));
    }
    if live.cwd != c.cwd {
        return Err("cwd changed".into());
    }
    Ok(())
}

fn stop_one(k: &dyn Killer, c: &Candidate, force: bool) -> Result<(), String> {
    let live = k.read(c.pid).ok_or("process is already gone")?;
    check_identity(&live, c)?;
    k.signal(c.pid, force)
}

fn read_answer(input: &mut dyn BufRead) -> Option<String> {
    let mut l = String::new();
    match input.read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim().to_lowercase()),
    }
}

pub struct Env<'a> {
    /// stdin is a terminal.
    pub interactive: bool,
    /// The system supports a graceful terminate (false on Windows).
    pub graceful_supported: bool,
    pub killer: &'a dyn Killer,
}

macro_rules! say {
    ($out:expr, $($a:tt)*) => {{ let _ = writeln!($out, $($a)*); }};
}

pub fn run_stop(
    procs: &[ProcInfo],
    rules: &[KindRule],
    self_pid: u32,
    a: &StopArgs,
    env: &Env,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Summary, String> {
    let mut sum = Summary::default();
    let cands = select_candidates(procs, rules, self_pid, a.task.as_deref());
    let unknown: Vec<String> = a
        .pids
        .iter()
        .filter(|p| !cands.iter().any(|c| c.pid == **p))
        .map(|p| p.to_string())
        .collect();
    if !unknown.is_empty() {
        return Err(format!(
            "pid {} is not a stop candidate (idle dev-server, lsp, test or build, not agentgc or its parent); nothing was stopped",
            unknown.join(", ")
        ));
    }
    if cands.is_empty() {
        say!(out, "No stop candidates (idle dev-server, lsp, test or build). Nothing stopped.");
        return Ok(sum);
    }
    say!(out, "Stop candidates ({}):", cands.len());
    for c in &cands {
        let cwd = c.cwd.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        say!(
            out,
            "  {:>7}  {:<10}  {:<20}  {:>5.1}%  {:>10}  {}",
            c.pid,
            c.kind,
            ps::shorten(&c.name, 20),
            c.cpu_percent,
            scan::human(c.rss_bytes),
            cwd
        );
    }
    if !a.yes {
        say!(out, "dry-run: nothing stopped");
        say!(out, "Add --yes to stop. In a terminal each process is asked; otherwise name pids with --pid.");
        return Ok(sum);
    }
    if !a.force && !env.graceful_supported {
        say!(out, "Graceful stop is not supported on this system: use --force. Nothing was stopped.");
        return Ok(sum);
    }

    let mut targets: Vec<&Candidate> = Vec::new();
    if env.interactive {
        for c in cands.iter().filter(|c| a.pids.is_empty() || a.pids.contains(&c.pid)) {
            let _ = write!(out, "Stop pid {} {}? [y/N/q] ", c.pid, c.name);
            let _ = out.flush();
            match read_answer(input).as_deref() {
                Some("y") => targets.push(c),
                Some("q") | None => break,
                _ => {}
            }
        }
    } else {
        if a.pids.is_empty() {
            return Err("stdin is not a terminal: name the pids to stop with --pid; nothing was stopped".into());
        }
        targets.extend(cands.iter().filter(|c| a.pids.contains(&c.pid)));
    }

    let k = env.killer;
    let mut signaled: Vec<&Candidate> = Vec::new();
    for c in targets {
        match stop_one(k, c, a.force) {
            Ok(()) => signaled.push(c),
            Err(why) => {
                say!(out, "  refused pid {} {}: {why}", c.pid, c.name);
                sum.refused.push((c.pid, why));
            }
        }
    }
    let ids: Vec<u32> = signaled.iter().map(|c| c.pid).collect();
    let alive = if ids.is_empty() { Vec::new() } else { wait_gone(k, &ids, WAIT_MS) };
    for c in signaled {
        if alive.contains(&c.pid) {
            say!(out, "  still running after 3 s: pid {} {} (try --force)", c.pid, c.name);
            sum.still_running.push(c.pid);
        } else {
            say!(out, "  stopped pid {} {}", c.pid, c.name);
            sum.stopped.push(c.pid);
            sum.freed_bytes += c.rss_bytes;
        }
    }
    say!(
        out,
        "Summary: stopped {}, still running {}, refused {}, memory freed about {}",
        sum.stopped.len(),
        sum.still_running.len(),
        sum.refused.len(),
        scan::human(sum.freed_bytes)
    );
    Ok(sum)
}

/// Real run: read the system, ask on a terminal, signal through `SysKiller`.
pub fn cmd_stop(a: &StopArgs, rules: &[KindRule]) -> Result<(), String> {
    use std::io::IsTerminal;
    let procs = ps::read_system();
    let env = Env {
        interactive: std::io::stdin().is_terminal(),
        graceful_supported: !cfg!(windows),
        killer: &SysKiller,
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    run_stop(&procs, rules, std::process::id(), a, &env, &mut input, &mut out).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::io::Cursor;
    use std::time::Duration;

    fn rules() -> Vec<KindRule> {
        ps::parse_proc_rules(ps::DEFAULT_PROC_RULES).unwrap()
    }

    fn pi(pid: u32, name: &str, cmd: &str, cpu: f32, parent: Option<u32>) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmd: cmd.into(),
            cwd: Some(PathBuf::from("/w/app")),
            rss_bytes: 10 * 1024 * 1024,
            cpu_percent: cpu,
            parent,
            start_time: 1000 + pid as u64,
        }
    }

    fn fake_live(p: &ProcInfo) -> Live {
        Live { name: p.name.clone(), cwd: p.cwd.clone(), start_time: p.start_time }
    }

    struct Fake {
        live: RefCell<HashMap<u32, Live>>,
        signals: RefCell<Vec<(u32, bool)>>,
        dies: bool,
    }

    impl Fake {
        fn new(procs: &[ProcInfo], dies: bool) -> Fake {
            Fake {
                live: RefCell::new(procs.iter().map(|p| (p.pid, fake_live(p))).collect()),
                signals: RefCell::new(Vec::new()),
                dies,
            }
        }
    }

    impl Killer for Fake {
        fn read(&self, pid: u32) -> Option<Live> {
            self.live.borrow().get(&pid).cloned()
        }
        fn signal(&self, pid: u32, force: bool) -> Result<(), String> {
            self.signals.borrow_mut().push((pid, force));
            if self.dies {
                self.live.borrow_mut().remove(&pid);
            }
            Ok(())
        }
        fn alive(&self, pid: u32) -> bool {
            self.live.borrow().contains_key(&pid)
        }
        fn sleep(&self, _d: Duration) {}
    }

    fn args(yes: bool, pids: &[u32], force: bool) -> StopArgs {
        StopArgs { yes, pids: pids.to_vec(), task: None, force }
    }

    fn run(
        procs: &[ProcInfo],
        k: &Fake,
        a: &StopArgs,
        interactive: bool,
        graceful: bool,
        input: &str,
    ) -> (Result<Summary, String>, String) {
        let env = Env { interactive, graceful_supported: graceful, killer: k };
        let mut out: Vec<u8> = Vec::new();
        let r = run_stop(procs, &rules(), 99, a, &env, &mut Cursor::new(input.to_string()), &mut out);
        (r, String::from_utf8(out).unwrap())
    }

    fn sample() -> Vec<ProcInfo> {
        vec![
            pi(1, "node", "node vite --port 5173", 0.0, None),
            pi(2, "rust-analyzer", "rust-analyzer", 0.0, None),
            pi(3, "pytest", "pytest -x", 0.0, None),
            pi(4, "cargo", "cargo build", 0.0, None),
            pi(5, "claude", "claude --resume", 0.0, None),
            pi(6, "postgres", "postgres -D data", 0.0, None),
            pi(7, "notepad", "notepad x.txt", 0.0, None),
            pi(8, "node", "node vite", 50.0, None),
            pi(99, "agentgc", "agentgc ps --stop", 0.0, Some(98)),
            pi(98, "node", "node vite", 0.0, Some(97)),
            pi(97, "tsserver", "tsserver", 0.0, None),
        ]
    }

    #[test]
    fn selects_only_idle_stoppable_kinds() {
        let ids: Vec<u32> = select_candidates(&sample(), &rules(), 99, None).iter().map(|c| c.pid).collect();
        assert_eq!(ids, vec![1, 2, 3, 4]);
    }

    #[test]
    fn never_agent_db_other_or_busy() {
        for c in select_candidates(&sample(), &rules(), 99, None) {
            assert!(!["agent", "db", "other"].contains(&c.kind.as_str()), "{c:?}");
        }
        let ids: Vec<u32> = select_candidates(&sample(), &rules(), 99, None).iter().map(|c| c.pid).collect();
        for bad in [5, 6, 7, 8] {
            assert!(!ids.contains(&bad), "pid {bad} selected");
        }
    }

    #[test]
    fn never_self_or_parent_chain() {
        let ids: Vec<u32> = select_candidates(&sample(), &rules(), 99, None).iter().map(|c| c.pid).collect();
        for bad in [99, 98, 97] {
            assert!(!ids.contains(&bad), "pid {bad} selected");
        }
    }

    #[test]
    fn unknown_cwd_and_system_paths_skipped() {
        let mut a = pi(1, "node", "node vite", 0.0, None);
        a.cwd = None;
        let mut b = pi(2, "node", "node vite", 0.0, None);
        b.cwd = Some(PathBuf::from("C:\\Windows\\System32"));
        assert!(select_candidates(&[a, b], &rules(), 99, None).is_empty());
    }

    #[test]
    fn task_filter_is_case_insensitive() {
        let p = sample();
        assert_eq!(select_candidates(&p, &rules(), 99, Some("APP")).len(), 4);
        assert!(select_candidates(&p, &rules(), 99, Some("other")).is_empty());
    }

    #[test]
    fn dry_run_signals_nothing() {
        let p = sample();
        let k = Fake::new(&p, true);
        let (r, out) = run(&p, &k, &args(false, &[], false), true, true, "y\ny\ny\ny\n");
        assert!(r.is_ok());
        assert!(out.contains("dry-run: nothing stopped"), "{out}");
        assert!(k.signals.borrow().is_empty());
    }

    #[test]
    fn non_candidate_pid_is_error_and_nothing_stops() {
        let p = sample();
        for bad in [5u32, 6, 7, 8, 98, 99, 12345] {
            let k = Fake::new(&p, true);
            let (r, _) = run(&p, &k, &args(true, &[1, bad], true), false, true, "");
            assert!(r.unwrap_err().contains(&bad.to_string()));
            assert!(k.signals.borrow().is_empty(), "pid {bad} caused a signal");
        }
    }

    #[test]
    fn non_terminal_stops_only_named_pids() {
        let p = sample();
        let k = Fake::new(&p, true);
        let (r, _) = run(&p, &k, &args(true, &[2], false), false, true, "");
        let s = r.unwrap();
        assert_eq!(s.stopped, vec![2]);
        assert_eq!(*k.signals.borrow(), vec![(2, false)]);
        assert_eq!(s.freed_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn non_terminal_without_pid_is_error() {
        let p = sample();
        let k = Fake::new(&p, true);
        let (r, _) = run(&p, &k, &args(true, &[], false), false, true, "");
        assert!(r.is_err());
        assert!(k.signals.borrow().is_empty());
    }

    #[test]
    fn terminal_asks_each_and_q_quits() {
        let p = sample();
        let k = Fake::new(&p, true);
        let (r, out) = run(&p, &k, &args(true, &[], false), true, true, "y\nn\nq\ny\n");
        assert_eq!(r.unwrap().stopped, vec![1]);
        assert!(out.contains("Stop pid 1 node? [y/N/q]"), "{out}");
        assert_eq!(*k.signals.borrow(), vec![(1, false)]);
    }

    #[test]
    fn terminal_with_pid_prompts_only_named() {
        let p = sample();
        let k = Fake::new(&p, true);
        let (r, out) = run(&p, &k, &args(true, &[3], false), true, true, "y\n");
        assert_eq!(r.unwrap().stopped, vec![3]);
        assert!(!out.contains("Stop pid 1 "), "{out}");
    }

    #[test]
    fn pid_reuse_is_refused() {
        let p = sample();
        for change in 0..3 {
            let k = Fake::new(&p, true);
            {
                let mut l = k.live.borrow_mut();
                let e = l.get_mut(&1).unwrap();
                match change {
                    0 => e.start_time += 1,
                    1 => e.name = "other.exe".into(),
                    _ => e.cwd = Some(PathBuf::from("/w/elsewhere")),
                }
            }
            let (r, _) = run(&p, &k, &args(true, &[1], true), false, true, "");
            let s = r.unwrap();
            assert!(s.stopped.is_empty());
            assert_eq!(s.refused.len(), 1);
            assert!(k.signals.borrow().is_empty(), "signal sent on change {change}");
        }
    }

    #[test]
    fn gone_process_is_refused() {
        let p = sample();
        let k = Fake::new(&p, true);
        k.live.borrow_mut().remove(&1);
        let (r, _) = run(&p, &k, &args(true, &[1], true), false, true, "");
        assert_eq!(r.unwrap().refused.len(), 1);
    }

    #[test]
    fn no_graceful_support_needs_force() {
        let p = sample();
        let k = Fake::new(&p, true);
        let (r, out) = run(&p, &k, &args(true, &[1], false), false, false, "");
        assert!(r.unwrap().stopped.is_empty());
        assert!(out.contains("use --force"), "{out}");
        assert!(k.signals.borrow().is_empty());
        let (r, _) = run(&p, &k, &args(true, &[1], true), false, false, "");
        assert_eq!(r.unwrap().stopped, vec![1]);
        assert_eq!(*k.signals.borrow(), vec![(1, true)]);
    }

    #[test]
    fn survivors_are_reported() {
        let p = sample();
        let k = Fake::new(&p, false);
        let (r, out) = run(&p, &k, &args(true, &[1], false), false, true, "");
        let s = r.unwrap();
        assert_eq!(s.still_running, vec![1]);
        assert!(s.stopped.is_empty());
        assert_eq!(s.freed_bytes, 0);
        assert!(out.contains("still running"), "{out}");
        assert!(out.contains("Summary: stopped 0, still running 1, refused 0"), "{out}");
    }

    #[test]
    fn secrets_never_printed() {
        let mut p = sample();
        p[0].cmd = "node vite --token SECRETVALUE".into();
        let k = Fake::new(&p, true);
        let (_, out) = run(&p, &k, &args(false, &[], false), false, true, "");
        assert!(!out.contains("SECRETVALUE"), "{out}");
    }
}
