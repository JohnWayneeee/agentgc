//! Integration tests for the state columns of `task list`: CHANGED, AHEAD/BEHIND, SERVICES.
//! Temp repos only. `task list` must stay read-only.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

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

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

const CFG: &str = "[ports]\nnames=[\"web\"]\nrange=[42700, 42800]\n\
[services.web]\ncmd=\"echo web\"\nport=\"web\"\n";

fn make_repo(base: &Path) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    fs::write(repo.join("agentgc.task.toml"), CFG).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

fn agentgc(repo: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .arg("task")
        .args(args)
        .arg("--repo")
        .arg(repo)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    (o.status.success(), text)
}

fn list_json(repo: &Path) -> serde_json::Value {
    let o = Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .args(["task", "list", "--json", "--repo"])
        .arg(repo)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    serde_json::from_slice(&o.stdout).unwrap()
}

fn task<'a>(v: &'a serde_json::Value, name: &str) -> &'a serde_json::Value {
    v["tasks"].as_array().unwrap().iter().find(|t| t["name"] == name).unwrap_or_else(|| panic!("no task {name}"))
}

fn new_task(repo: &Path, name: &str) {
    let (ok, t) = agentgc(repo, &["new", name, "--yes"]);
    assert!(ok, "{t}");
}

/// Files under `dir` (relative path, length, mtime), for the read-only check.
fn snapshot(dir: &Path) -> Vec<(String, u64, std::time::SystemTime)> {
    let mut v = Vec::new();
    fn walk(root: &Path, d: &Path, v: &mut Vec<(String, u64, std::time::SystemTime)>) {
        let Ok(rd) = fs::read_dir(d) else { return };
        for e in rd.flatten() {
            let p = e.path();
            let m = e.metadata().unwrap();
            if m.is_dir() {
                walk(root, &p, v);
            } else {
                let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                v.push((rel, m.len(), m.modified().unwrap()));
            }
        }
    }
    walk(dir, dir, &mut v);
    v.sort_by(|a, b| a.0.cmp(&b.0));
    v
}

#[test]
fn counts_changed_files_and_ahead_behind() {
    need_git!();
    let base = tmp("list-state");
    let repo = make_repo(&base);
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");

    // clean task: nothing changed, nothing ahead or behind, one declared service that is not running
    let v = list_json(&repo);
    let t = task(&v, "one");
    assert_eq!((t["changed"].as_u64(), t["ahead"].as_u64(), t["behind"].as_u64()), (Some(0), Some(0), Some(0)), "{t}");
    assert_eq!((t["services_running"].as_u64(), t["services_total"].as_u64()), (Some(0), Some(1)));
    assert_eq!(t["status"], serde_json::json!(["ok"]));

    // two uncommitted files
    fs::write(wt.join("a.txt"), "a\n").unwrap();
    fs::write(wt.join("b.txt"), "b\n").unwrap();
    let t = task(&list_json(&repo), "one").clone();
    assert_eq!(t["changed"].as_u64(), Some(2));

    // one commit on the task branch (ahead 1, one file left), one new commit on main (behind 1)
    git(&wt, &["add", "a.txt"]);
    git(&wt, &["commit", "-m", "a"]);
    fs::write(repo.join("main.txt"), "m\n").unwrap();
    git(&repo, &["add", "main.txt"]);
    git(&repo, &["commit", "-m", "m"]);
    let v = list_json(&repo);
    let t = task(&v, "one");
    assert_eq!((t["changed"].as_u64(), t["ahead"].as_u64(), t["behind"].as_u64()), (Some(1), Some(1), Some(1)), "{t}");

    // the text output shows the same columns
    let (ok, text) = agentgc(&repo, &["list"]);
    assert!(ok, "{text}");
    assert!(text.contains("CHANGED 1  AHEAD/BEHIND 1/1  SERVICES -") || text.contains("CHANGED 1  AHEAD/BEHIND 1/1  SERVICES 0/1"), "{text}");
    assert!(text.contains("path:") && text.contains("branch:") && text.contains("ports:") && text.contains("[ok,dirty]"), "{text}");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn list_is_read_only() {
    need_git!();
    let base = tmp("list-ro");
    let repo = make_repo(&base);
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");
    fs::write(wt.join("a.txt"), "a\n").unwrap();
    let before = (snapshot(&repo.join(".git")), snapshot(&repo.join(".agentgc")), snapshot(&wt));
    for _ in 0..2 {
        let _ = list_json(&repo);
        let (ok, _) = agentgc(&repo, &["list"]);
        assert!(ok);
    }
    let after = (snapshot(&repo.join(".git")), snapshot(&repo.join(".agentgc")), snapshot(&wt));
    assert_eq!(before.0, after.0, ".git changed");
    assert_eq!(before.1, after.1, ".agentgc changed");
    assert_eq!(before.2, after.2, "worktree changed");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn missing_folder_shows_dash_and_git_error_shows_question_mark() {
    need_git!();
    let base = tmp("list-miss");
    let repo = make_repo(&base);
    new_task(&repo, "one");
    new_task(&repo, "two");
    // task two: the folder is gone
    fs::remove_dir_all(repo.join("worktrees/two")).unwrap();
    // task one: the base branch does not exist, so rev-list fails
    let reg = repo.join(".agentgc/tasks/one.json");
    let text = fs::read_to_string(&reg).unwrap().replace("\"base\": \"main\"", "\"base\": \"no-such-base\"");
    assert!(text.contains("no-such-base"));
    fs::write(&reg, text).unwrap();

    let v = list_json(&repo);
    let two = task(&v, "two");
    assert!(two["changed"].is_null() && two["ahead"].is_null() && two["behind"].is_null(), "{two}");
    assert_eq!(two["status"], serde_json::json!(["missing"]));
    let one = task(&v, "one");
    assert_eq!(one["changed"].as_u64(), Some(0));
    assert!(one["ahead"].is_null() && one["behind"].is_null(), "{one}");

    let (ok, text) = agentgc(&repo, &["list"]);
    assert!(ok, "the command must not fail: {text}");
    let block = |name: &str| -> String {
        let i = text.find(&format!("{name}  [")).unwrap();
        text[i..].lines().take(9).collect::<Vec<_>>().join("\n")
    };
    assert!(block("two").contains("CHANGED -  AHEAD/BEHIND -  SERVICES -"), "{text}");
    assert!(block("one").contains("CHANGED 0  AHEAD/BEHIND ?"), "{text}");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn running_services_come_from_daemon_json_with_identity_check() {
    need_git!();
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let base = tmp("list-svc");
    let repo = make_repo(&base);
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");

    // a recorded pid that is this test process with its real identity counts as running
    let me = std::process::id();
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[Pid::from_u32(me)]), true);
    let p = sys.process(Pid::from_u32(me)).unwrap();
    let (start, name) = (p.start_time(), p.name().to_string_lossy().into_owned());
    let state = |start_time: u64| {
        serde_json::json!({
            "pid": 1, "start_time": 1, "proc_name": "x", "started_at": 1,
            "services": [{"name": "web", "pid": me, "start_time": start_time, "proc_name": name,
                          "restarts": 0, "state": "running", "port": null}]
        })
        .to_string()
    };
    fs::write(wt.join(".agentgc/daemon.json"), state(start)).unwrap();
    let t = task(&list_json(&repo), "one").clone();
    assert_eq!((t["services_running"].as_u64(), t["services_total"].as_u64()), (Some(1), Some(1)), "{t}");
    let (_, text) = agentgc(&repo, &["list"]);
    assert!(text.contains("SERVICES 1/1"), "{text}");

    // same pid, other start time: a reused pid does not count
    fs::write(wt.join(".agentgc/daemon.json"), state(start + 1)).unwrap();
    let t = task(&list_json(&repo), "one").clone();
    assert_eq!(t["services_running"].as_u64(), Some(0), "{t}");

    // a broken state file counts as nothing running and does not fail the command
    fs::write(wt.join(".agentgc/daemon.json"), "{ nope").unwrap();
    let t = task(&list_json(&repo), "one").clone();
    assert_eq!((t["services_running"].as_u64(), t["services_total"].as_u64()), (Some(0), Some(1)));
    let _ = fs::remove_dir_all(&base);
}
