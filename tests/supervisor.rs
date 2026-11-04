//! Integration tests for the per-task supervisor (`up`, `status`, `logs`, `down`, `task done`).
//! Temp repos only. The services are short harmless commands that print one line.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static N: AtomicU32 = AtomicU32::new(0);

fn git_ok() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

macro_rules! need_git {
    () => {
        if !git_ok() {
            eprintln!("note: git not available, skipping {}", module_path!());
            return;
        }
    };
}

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("agentgc-it-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

/// Prints `hi`, then stays alive for 20 s (killed by `down`).
fn long_cmd() -> &'static str {
    if cfg!(windows) {
        "echo hi & ping -n 20 127.0.0.1 >nul"
    } else {
        "echo hi; sleep 20"
    }
}

fn make_repo(base: &Path, service_cmd: &str) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    let cfg = format!(
        "[ports]\nnames=[\"web\"]\nrange=[42500, 42600]\n[services.web]\ncmd='{service_cmd}'\nport=\"web\"\n"
    );
    fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

/// Run agentgc with stdout and stderr in files, so a detached child can never hold a pipe open.
fn agentgc(base: &Path, args: &[&str]) -> (bool, String) {
    let n = N.fetch_add(1, Ordering::SeqCst);
    let out = base.join(format!("out{n}.txt"));
    let f = fs::File::create(&out).unwrap();
    let st = Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(f.try_clone().unwrap())
        .stderr(f)
        .status()
        .unwrap();
    (st.success(), fs::read_to_string(&out).unwrap_or_default())
}

fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(20) {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timeout waiting for {what}");
}

/// Read-only: is a process with this pid and start time still there?
fn same_process_alive(pid: u32, start: u64) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(pid)]), true);
    sys.process(Pid::from_u32(pid)).map_or(false, |p| p.start_time() == start)
}

fn new_task(base: &Path, repo: &Path) -> PathBuf {
    let r = repo.to_string_lossy().into_owned();
    let (ok, t) = agentgc(base, &["task", "new", "one", "--yes", "--repo", &r]);
    assert!(ok, "{t}");
    repo.join("worktrees/one")
}

#[test]
fn up_status_logs_down() {
    need_git!();
    let base = tmp("sup");
    let repo = make_repo(&base, long_cmd());
    let wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let t = ["--task", "one", "--repo", r.as_str()];
    let cmd = |name: &str, extra: &[&str]| -> Vec<String> {
        let mut v = vec![name.to_string()];
        v.extend(extra.iter().map(|s| s.to_string()));
        v.extend(t.iter().map(|s| s.to_string()));
        v
    };
    let run = |name: &str, extra: &[&str]| {
        let v = cmd(name, extra);
        let refs: Vec<&str> = v.iter().map(String::as_str).collect();
        agentgc(&base, &refs)
    };

    let (ok, s) = run("status", &[]);
    assert!(ok && s.contains("supervisor: not running") && s.contains("web"), "{s}");

    let (ok, s) = run("up", &[]);
    assert!(ok && s.contains("supervisor started") && s.contains("web"), "{s}");
    let log = wt.join(".agentgc/logs/web.log");
    wait_for("the service line in the log", || fs::read_to_string(&log).map_or(false, |t| t.contains("hi")));

    let (ok, s) = run("status", &[]);
    assert!(ok && s.contains("supervisor: running") && s.contains("running") && s.contains("web"), "{s}");

    // a second `up` says so and exits 0
    let (ok, s) = run("up", &[]);
    assert!(ok && s.contains("already running"), "{s}");

    let (ok, s) = run("logs", &["web", "--lines", "5"]);
    assert!(ok && s.contains("hi"), "{s}");
    let (ok, s) = run("restart", &["nope"]);
    assert!(!ok && s.contains("no service"), "{s}");

    let st: serde_json::Value = serde_json::from_str(&fs::read_to_string(wt.join(".agentgc/daemon.json")).unwrap()).unwrap();
    let svc_pid = st["services"][0]["pid"].as_u64().unwrap() as u32;
    let svc_start = st["services"][0]["start_time"].as_u64().unwrap();
    assert!(same_process_alive(svc_pid, svc_start));

    let (ok, s) = run("down", &[]);
    assert!(ok && s.contains("stopped the supervisor"), "{s}");
    assert!(!wt.join(".agentgc/daemon.json").exists() && !wt.join(".agentgc/daemon.stop").exists());
    wait_for("the service to end", || !same_process_alive(svc_pid, svc_start));
    let (ok, s) = run("status", &[]);
    assert!(ok && s.contains("supervisor: not running"), "{s}");
    let (ok, s) = run("down", &[]);
    assert!(ok && s.contains("not running"), "{s}");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn done_refuses_while_running_and_stops_services_on_request() {
    need_git!();
    let base = tmp("sup-done");
    let repo = make_repo(&base, long_cmd());
    let wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let (ok, s) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    assert!(ok && s.contains("supervisor started"), "{s}");

    let (ok, s) = agentgc(&base, &["task", "done", "one", "--yes", "--repo", &r]);
    assert!(!ok && s.contains("supervisor"), "{s}");
    assert!(wt.exists(), "nothing may change while the supervisor runs");
    // --force does not skip it either
    let (ok, _) = agentgc(&base, &["task", "done", "one", "--yes", "--force", "--repo", &r]);
    assert!(!ok && wt.exists());
    // the dry run names it
    let (ok, s) = agentgc(&base, &["task", "done", "one", "--repo", &r]);
    assert!(ok && s.contains("BLOCKED") && s.contains("--stop-services"), "{s}");

    let (ok, s) = agentgc(&base, &["task", "done", "one", "--yes", "--stop-services", "--repo", &r]);
    assert!(ok, "{s}");
    assert!(!wt.exists(), "worktree must be gone: {s}");
    assert!(!repo.join(".agentgc/tasks/one.json").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn supervisor_ends_when_every_service_finished() {
    need_git!();
    let base = tmp("sup-fin");
    let repo = make_repo(&base, "echo hi");
    let wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let (ok, s) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    assert!(ok, "{s}");
    let log = wt.join(".agentgc/logs/web.log");
    wait_for("the log line", || fs::read_to_string(&log).map_or(false, |t| t.contains("hi")));
    wait_for("the supervisor to end", || !wt.join(".agentgc/daemon.json").exists() && !wt.join(".agentgc/daemon.stop").exists());
    let (ok, s) = agentgc(&base, &["status", "--task", "one", "--repo", &r]);
    assert!(ok && s.contains("supervisor: not running"), "{s}");
    // task done removes the generated daemon files with the folder
    let (ok, s) = agentgc(&base, &["task", "done", "one", "--yes", "--repo", &r]);
    assert!(ok, "{s}");
    assert!(!wt.exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn up_refuses_a_cwd_outside_the_worktree() {
    need_git!();
    let base = tmp("sup-cwd");
    let repo = make_repo(&base, "echo hi");
    let wt = new_task(&base, &repo);
    let sj = wt.join(".agentgc/services.json");
    let text = fs::read_to_string(&sj).unwrap().replace("\"cwd\": \".\"", "\"cwd\": \"..\"");
    assert!(text.contains("\"cwd\": \"..\""), "{text}");
    fs::write(&sj, text).unwrap();
    let r = repo.to_string_lossy().into_owned();
    let (ok, s) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    assert!(!ok && s.contains("cwd"), "{s}");
    assert!(!wt.join(".agentgc/daemon.json").exists() && !wt.join(".agentgc/logs/web.log").exists());
    let _ = fs::remove_dir_all(&base);
}
