//! Integration tests for service order and readiness (`depends_on`, `ready_timeout`, READY column).
//! Temp repos only. The only network code is a TCP listener on 127.0.0.1 that the test opens itself.
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant, SystemTime};

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

/// `lo`: first port of the range; every test uses its own range, so parallel tests never share a port.
fn make_repo(base: &Path, lo: u16, services: &str) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    let cfg = format!("[ports]\nnames=[\"db\"]\nrange=[{lo}, {hi}]\n{services}", hi = lo + 20);
    fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

/// db has a port, api waits for it.
fn db_api(db_timeout: u64) -> String {
    let c = long_cmd();
    format!(
        "[services.db]\ncmd='{c}'\nport=\"db\"\nready_timeout={db_timeout}\n\
         [services.api]\ncmd='{c}'\ndepends_on=[\"db\"]\n"
    )
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

fn new_task(base: &Path, repo: &Path) -> PathBuf {
    let r = repo.to_string_lossy().into_owned();
    let (ok, t) = agentgc(base, &["task", "new", "one", "--yes", "--repo", &r]);
    assert!(ok, "{t}");
    repo.join("worktrees/one")
}

fn port_db(wt: &Path) -> u16 {
    let env = fs::read_to_string(wt.join(".agentgc/task.env")).unwrap();
    env.lines().find_map(|l| l.strip_prefix("PORT_DB=")).unwrap().trim().parse().unwrap()
}

/// Runs `agentgc down` when the test ends, also after a failed assert.
struct Down {
    base: PathBuf,
    repo: PathBuf,
}
impl Drop for Down {
    fn drop(&mut self) {
        let r = self.repo.to_string_lossy().into_owned();
        let _ = agentgc(&self.base, &["down", "--task", "one", "--repo", &r, "--force"]);
    }
}

/// The columns of one `status` row: SERVICE PID STATE RUNNING READY ...
fn row(status: &str, name: &str) -> Vec<String> {
    status
        .lines()
        .find(|l| l.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no row {name} in:\n{status}"))
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

#[test]
fn dependent_starts_only_after_the_port_answers() {
    need_git!();
    let base = tmp("ready-ok");
    let repo = make_repo(&base, 42900, &db_api(30));
    let wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let _down = Down { base: base.clone(), repo: repo.clone() };
    let port = port_db(&wt);

    // open the port 2 s after `up` begins
    let opener = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(2000));
        let l = TcpListener::bind(("127.0.0.1", port)).expect("bind the db port");
        (l, SystemTime::now())
    });
    let t0 = Instant::now();
    let (ok, up) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    let took = t0.elapsed();
    let (_listener, opened_at) = opener.join().unwrap();
    assert!(ok, "{up}");
    assert!(took >= Duration::from_millis(1500), "up returned before the port opened: {took:?}\n{up}");

    let (ok, st) = agentgc(&base, &["status", "--task", "one", "--repo", &r]);
    assert!(ok, "{st}");
    assert!(st.contains("READY"), "{st}");
    let (db, api) = (row(&st, "db"), row(&st, "api"));
    assert_eq!((db[2].as_str(), db[3].as_str(), db[4].as_str()), ("running", "yes", "yes"), "{st}");
    assert_eq!((api[2].as_str(), api[3].as_str(), api[4].as_str()), ("running", "yes", "yes"), "{st}");

    // api started after the port opened; db started before
    let mtime = |n: &str| fs::metadata(wt.join(format!(".agentgc/logs/{n}.log"))).unwrap().modified().unwrap();
    let slack = Duration::from_millis(100);
    assert!(mtime("api") + slack >= opened_at, "api log is older than the open port");
    assert!(mtime("db") < opened_at, "db log is not older than the open port");
}

#[test]
fn timeout_reports_the_service_and_does_not_start_dependents() {
    need_git!();
    let base = tmp("ready-timeout");
    let repo = make_repo(&base, 42930, &db_api(2));
    let wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let _down = Down { base: base.clone(), repo: repo.clone() };

    let (ok, up) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    assert!(!ok, "up must fail when a dependency is not ready:\n{up}");
    assert!(up.contains("service db is not ready after 2 s"), "{up}");
    assert!(up.contains("api"), "{up}");

    let (ok, st) = agentgc(&base, &["status", "--task", "one", "--repo", &r]);
    assert!(ok, "{st}");
    let (db, api) = (row(&st, "db"), row(&st, "api"));
    assert_eq!((db[2].as_str(), db[3].as_str(), db[4].as_str()), ("running", "yes", "no"), "{st}");
    assert_eq!((api[1].as_str(), api[2].as_str(), api[3].as_str(), api[4].as_str()), ("-", "blocked", "no", "-"), "{st}");
    assert!(st.contains("api: service db is not ready"), "{st}");
    assert!(!wt.join(".agentgc/logs/api.log").exists(), "the blocked service was started");

    let (ok, d) = agentgc(&base, &["down", "--task", "one", "--repo", &r]);
    assert!(ok, "{d}");
}

#[test]
fn dependency_without_a_port_is_ready_when_running() {
    need_git!();
    let base = tmp("ready-noport");
    let c = long_cmd();
    let svc = format!(
        "[services.db]\ncmd='{c}'\n[services.api]\ncmd='{c}'\ndepends_on=[\"db\"]\n"
    );
    let repo = make_repo(&base, 42960, &svc);
    let _wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let _down = Down { base: base.clone(), repo: repo.clone() };

    let (ok, up) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    assert!(ok, "{up}");
    let (_, st) = agentgc(&base, &["status", "--task", "one", "--repo", &r]);
    for n in ["db", "api"] {
        let x = row(&st, n);
        assert_eq!((x[2].as_str(), x[3].as_str(), x[4].as_str()), ("running", "yes", "yes"), "{st}");
    }
    let (ok, d) = agentgc(&base, &["down", "--task", "one", "--repo", &r]);
    assert!(ok, "{d}");
}

#[test]
fn bad_dependencies_are_load_time_errors() {
    need_git!();
    let cases = [
        ("unknown", "[services.a]\ncmd=\"x\"\ndepends_on=[\"zz\"]\n", "unknown service"),
        ("self", "[services.a]\ncmd=\"x\"\ndepends_on=[\"a\"]\n", "depends on itself"),
        (
            "cycle",
            "[services.a]\ncmd=\"x\"\ndepends_on=[\"b\"]\n[services.b]\ncmd=\"x\"\ndepends_on=[\"a\"]\n",
            "dependency cycle: a -> b -> a",
        ),
    ];
    for (tag, svc, want) in cases {
        let base = tmp(&format!("ready-bad-{tag}"));
        let repo = make_repo(&base, 42990, svc);
        let r = repo.to_string_lossy().into_owned();
        let (ok, t) = agentgc(&base, &["task", "new", "one", "--yes", "--repo", &r]);
        assert!(!ok && t.contains(want), "{tag}: {t}");
        assert!(!repo.join("worktrees/one").exists(), "{tag}: nothing may be created");
        let _ = fs::remove_dir_all(&base);
    }
}

#[test]
fn up_refuses_a_cyclic_services_json_and_starts_nothing() {
    need_git!();
    let base = tmp("ready-json");
    let repo = make_repo(&base, 43020, &db_api(30));
    let wt = new_task(&base, &repo);
    let r = repo.to_string_lossy().into_owned();
    let _down = Down { base: base.clone(), repo: repo.clone() };
    let bad = r#"[{"name":"db","cmd":"x","depends_on":["api"]},{"name":"api","cmd":"x","depends_on":["db"]}]"#;
    fs::write(wt.join(".agentgc/services.json"), bad).unwrap();
    let (ok, up) = agentgc(&base, &["up", "--task", "one", "--repo", &r]);
    assert!(!ok && up.contains("cycle"), "{up}");
    assert!(!wt.join(".agentgc/daemon.json").exists());
    assert!(!wt.join(".agentgc/logs").exists());
}
