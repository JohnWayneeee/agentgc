//! Per-task supervisor, real part: starts services, pumps their output into rotating logs,
//! holds the lock and the state file. No system service, no network port.
//! Windows: one Job Object per service with KILL_ON_JOB_CLOSE (the service tree dies when the
//! supervisor dies). Unix: one process group per service.
use crate::lock;
use crate::proc_kill::{Killer, SysKiller};
use crate::sup::*;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(windows)]
mod job {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job whose processes are killed when the last handle closes.
    pub struct Job(HANDLE);

    impl Job {
        pub fn new() -> Result<Job, String> {
            // SAFETY: plain Win32 calls with valid pointers; the handle is closed in Drop.
            unsafe {
                let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if h.is_null() {
                    return Err(format!("CreateJobObject failed: {}", std::io::Error::last_os_error()));
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    h,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let e = std::io::Error::last_os_error();
                    CloseHandle(h);
                    return Err(format!("SetInformationJobObject failed: {e}"));
                }
                Ok(Job(h))
            }
        }

        pub fn assign(&self, child: &std::process::Child) -> Result<(), String> {
            // SAFETY: the child handle is valid while `child` lives.
            let ok = unsafe { AssignProcessToJobObject(self.0, child.as_raw_handle() as HANDLE) };
            if ok == 0 {
                Err(format!("AssignProcessToJobObject failed: {}", std::io::Error::last_os_error()))
            } else {
                Ok(())
            }
        }

        pub fn kill(&self) {
            // SAFETY: valid job handle.
            unsafe {
                TerminateJobObject(self.0, 1);
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            // SAFETY: closes the handle once; KILL_ON_JOB_CLOSE ends any process still in the job.
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

pub struct SysClock(Instant);

impl SysClock {
    pub fn new() -> SysClock {
        SysClock(Instant::now())
    }
}

impl Clock for SysClock {
    fn now_ms(&self) -> u64 {
        self.0.elapsed().as_millis() as u64
    }
    fn sleep_ms(&self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

struct RealChild {
    child: Child,
    pid: u32,
    start: u64,
    name: String,
    #[cfg(windows)]
    job: Option<job::Job>,
}

impl ChildProc for RealChild {
    fn pid(&self) -> u32 {
        self.pid
    }
    fn start_time(&self) -> u64 {
        self.start
    }
    fn proc_name(&self) -> String {
        self.name.clone()
    }
    fn try_wait(&mut self) -> Option<Option<i32>> {
        match self.child.try_wait() {
            Ok(Some(st)) => Some(st.code()),
            Ok(None) => None,
            Err(_) => Some(None),
        }
    }

    #[cfg(windows)]
    fn terminate(&mut self, force: bool) -> bool {
        if !force {
            return false; // Windows has no graceful stop for a detached console tree
        }
        if let Some(j) = &self.job {
            j.kill();
        }
        let _ = self.child.kill();
        true
    }

    #[cfg(unix)]
    fn terminate(&mut self, force: bool) -> bool {
        // The child is ours and not yet reaped, so the pid (= its process group) is not reused.
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: plain kill(2) on the process group of our own child.
        unsafe {
            libc::kill(-(self.pid as i32), sig);
        }
        true
    }
}

pub struct RealSpawner {
    pub logs: PathBuf,
}

fn shell_command(cmd: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut c = Command::new("cmd");
        c.raw_arg(format!("/S /C \"{cmd}\""));
        c
    }
    #[cfg(not(windows))]
    {
        let mut c = Command::new("sh");
        c.arg("-c").arg(cmd);
        c
    }
}

fn pump(mut r: impl Read + Send + 'static, log: Arc<Mutex<RotatingLog>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut l) = log.lock() {
                        let _ = l.write(&buf[..n]);
                    }
                }
            }
        }
    });
}

impl Spawner for RealSpawner {
    fn spawn(&mut self, def: &ServiceDef, cwd: &Path, env: &[(String, String)]) -> Result<Box<dyn ChildProc>, String> {
        let log = RotatingLog::open(&self.logs.join(format!("{}.log", def.name)), LOG_MAX)?;
        let log = Arc::new(Mutex::new(log));
        let mut c = shell_command(&def.cmd);
        c.current_dir(cwd)
            .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            c.process_group(0);
        }
        let mut child = c.spawn().map_err(|e| format!("cannot start service {}: {e}", def.name))?;
        let pid = child.id();
        #[cfg(windows)]
        let job = {
            let j = job::Job::new()?;
            match j.assign(&child) {
                Ok(()) => Some(j),
                // a command that ended before the assignment needs no job
                Err(_) if child.try_wait().map_or(false, |s| s.is_some()) => None,
                Err(e) => {
                    let _ = child.kill();
                    return Err(format!("service {}: {e}", def.name));
                }
            }
        };
        if let Some(o) = child.stdout.take() {
            pump(o, log.clone());
        }
        if let Some(e) = child.stderr.take() {
            pump(e, log.clone());
        }
        if let Ok(mut l) = log.lock() {
            let _ = l.write(format!("[agentgc] started pid {pid}\n").as_bytes());
        }
        let (start, name) = SysKiller.read(pid).map(|l| (l.start_time, l.name)).unwrap_or((0, String::new()));
        Ok(Box::new(RealChild {
            child,
            pid,
            start,
            name,
            #[cfg(windows)]
            job,
        }))
    }
}

fn marker_requests(wt: &Path, names: &[String]) -> Vec<String> {
    let mut v = Vec::new();
    for n in names {
        let p = restart_marker(wt, n);
        if fs::symlink_metadata(&p).map_or(false, |m| m.is_file()) && fs::remove_file(&p).is_ok() {
            v.push(n.clone());
        }
    }
    v
}

fn publish(wt: &Path, me: &DaemonState, eng: &Engine, last: &mut Option<DaemonState>) {
    let mut st = me.clone();
    st.services = eng.snapshot();
    if last.as_ref() != Some(&st) {
        match write_state(wt, &st) {
            Ok(()) => *last = Some(st),
            Err(e) => eprintln!("warning: {e}"),
        }
    }
}

/// The supervisor process body (`agentgc __supervise <worktree>`).
/// A second supervisor for the same task finds the lock held and ends quietly.
pub fn run_supervisor(wt: &Path) -> Result<(), String> {
    let ad = agentgc_dir(wt);
    if !fs::symlink_metadata(&ad).map_or(false, |m| m.is_dir()) {
        return Err(format!("{} is not a folder", ad.display()));
    }
    let slot = lock::try_acquire(&lock_path(wt), true).map_err(|e| format!("cannot take the daemon lock: {e}"))?;
    let Some(_slot) = slot else { return Ok(()) };

    let defs = load_services(wt)?;
    let mut with_cwd = Vec::new();
    for d in defs {
        let cwd = resolve_cwd(wt, &d.cwd).map_err(|e| format!("service {}: {e}", d.name))?;
        with_cwd.push((d, cwd));
    }
    let names: Vec<String> = with_cwd.iter().map(|(d, _)| d.name.clone()).collect();
    // stale markers of a dead supervisor must not act on this one
    let _ = fs::remove_file(stop_path(wt));
    marker_requests(wt, &names);
    fs::create_dir_all(logs_dir(wt)).map_err(|e| format!("cannot create {}: {e}", logs_dir(wt).display()))?;

    let env = load_env(wt);
    let clock = SysClock::new();
    let mut spawner = RealSpawner { logs: logs_dir(wt) };
    let (start_time, proc_name) =
        SysKiller.read(std::process::id()).map(|l| (l.start_time, l.name)).unwrap_or((0, String::new()));
    let me = DaemonState { pid: std::process::id(), start_time, proc_name, started_at: unix_secs(), services: Vec::new() };

    let mut eng = Engine::new(with_cwd, env, &mut spawner, &clock);
    eng.start_all();
    let mut last: Option<DaemonState> = None;
    publish(wt, &me, &eng, &mut last);
    loop {
        clock.sleep_ms(POLL_MS);
        if fs::symlink_metadata(stop_path(wt)).is_ok() {
            break;
        }
        let reqs = marker_requests(wt, &names);
        eng.tick(&reqs);
        publish(wt, &me, &eng, &mut last);
        if eng.all_done() {
            break;
        }
    }
    for s in eng.snapshot() {
        if s.state == STATE_FAILED {
            eprintln!("service {} failed after {} restarts (last exit code {:?})", s.name, s.restarts, s.exit_code);
        }
    }
    eng.stop_all();
    // the lock is still held here: remove our own files, then release it
    let _ = fs::remove_file(state_path(wt));
    let _ = fs::remove_file(stop_path(wt));
    Ok(())
}
