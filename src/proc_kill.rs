//! Shared process stopping. One trait (`Killer`) so tests never signal real processes.
use std::path::PathBuf;
use std::time::Duration;

/// A process as read right now.
#[derive(Debug, Clone, PartialEq)]
pub struct Live {
    pub name: String,
    pub cwd: Option<PathBuf>,
    pub start_time: u64,
}

pub trait Killer {
    /// Fresh read of one process. `None` when it is gone.
    fn read(&self, pid: u32) -> Option<Live>;
    /// Send the stop signal. `force` = hard kill. Without it: graceful terminate.
    fn signal(&self, pid: u32, force: bool) -> Result<(), String>;
    fn alive(&self, pid: u32) -> bool;
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// The real implementation, on top of sysinfo. No shell, no external command.
pub struct SysKiller;

fn refresh_one(pid: u32) -> sysinfo::System {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let kind = ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always);
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true, kind);
    sys
}

impl Killer for SysKiller {
    fn read(&self, pid: u32) -> Option<Live> {
        let sys = refresh_one(pid);
        let p = sys.process(sysinfo::Pid::from_u32(pid))?;
        Some(Live {
            name: p.name().to_string_lossy().into_owned(),
            cwd: p.cwd().map(|c| c.to_path_buf()),
            start_time: p.start_time(),
        })
    }

    fn signal(&self, pid: u32, force: bool) -> Result<(), String> {
        if pid == std::process::id() {
            return Err("refusing to signal agentgc itself".into());
        }
        let sys = refresh_one(pid);
        let p = sys.process(sysinfo::Pid::from_u32(pid)).ok_or("process is gone")?;
        if force {
            return if p.kill() { Ok(()) } else { Err("the system refused the kill".into()) };
        }
        match p.kill_with(sysinfo::Signal::Term) {
            Some(true) => Ok(()),
            Some(false) => Err("the system refused the terminate signal".into()),
            None => Err("graceful terminate is not supported on this system; use --force".into()),
        }
    }

    fn alive(&self, pid: u32) -> bool {
        refresh_one(pid).process(sysinfo::Pid::from_u32(pid)).is_some()
    }
}

/// Poll until every pid is gone or `total_ms` pass. Returns the pids still alive.
pub fn wait_gone(k: &dyn Killer, pids: &[u32], total_ms: u64) -> Vec<u32> {
    let mut alive: Vec<u32> = pids.to_vec();
    let step = 200;
    let mut waited = 0;
    while waited < total_ms {
        k.sleep(Duration::from_millis(step));
        waited += step;
        alive.retain(|p| k.alive(*p));
        if alive.is_empty() {
            break;
        }
    }
    alive
}
