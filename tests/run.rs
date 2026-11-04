//! Integration tests for `agentgc run` (binary, temp --dir, real git).
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

#[allow(dead_code)]
#[path = "../src/lock.rs"]
mod lock;

static N: AtomicU32 = AtomicU32::new(0);

fn git_ok() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

macro_rules! need_git {
    () => {
        if !git_ok() {
            eprintln!("note: git not available, skipping");
            return;
        }
    };
}

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("agentgc-run-it-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn agentgc(dir: &PathBuf) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_agentgc"));
    c.arg("run").arg("--dir").arg(dir);
    for k in ["CI", "AGENTGC_HELD", "AGENTGC_SLOTS", "AGENTGC_DIR", "AGENTGC_POOL", "AGENTGC_TIMEOUT", "AGENTGC_ON_TIMEOUT", "AGENTGC_LOG", "AGENTGC_MIN_INTERVAL"] {
        c.env_remove(k);
    }
    c
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

/// Hold slot 0 of default/default, with a fake info file.
fn hold(dir: &PathBuf) -> lock::Slot {
    let p = lock::slot_path(dir, "default", "default", 0);
    let s = lock::try_acquire(&p, true).unwrap().expect("slot free");
    fs::write(
        lock::info_path(&p),
        r#"{"pid":4242,"command":"fake heavy job","cwd":"x","start":1}"#,
    )
    .unwrap();
    s
}

#[test]
fn runs_command_and_exits_zero() {
    need_git!();
    let d = tmp("ok");
    let o = agentgc(&d).args(["--slots", "1", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    assert!(String::from_utf8_lossy(&o.stdout).contains("git version"));
    // free slot: nothing printed to stderr
    assert!(o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
    // own info removed
    assert!(!lock::info_path(&lock::slot_path(&d, "default", "default", 0)).exists());
}

#[test]
fn passes_exit_code_through() {
    need_git!();
    let d = tmp("code");
    let direct = Command::new("git").arg("definitely-not-a-subcommand").output().unwrap();
    assert_ne!(code(&direct), 0);
    let o = agentgc(&d).args(["--slots", "1", "--", "git", "definitely-not-a-subcommand"]).output().unwrap();
    assert_eq!(code(&o), code(&direct));
}

#[test]
fn missing_program_gives_127() {
    let d = tmp("missing");
    let o = agentgc(&d).args(["--slots", "1", "--", "agentgc-no-such-program-xyz"]).output().unwrap();
    assert_eq!(code(&o), 127);
}

#[test]
fn timeout_fail_exits_75() {
    need_git!();
    let d = tmp("tfail");
    let _s = hold(&d);
    let o = agentgc(&d)
        .args(["--slots", "1", "--timeout", "1", "--on-timeout", "fail", "--", "git", "--version"])
        .output()
        .unwrap();
    assert_eq!(code(&o), 75);
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("gave up waiting"), "{err}");
    assert!(err.contains("pid 4242") && err.contains("fake heavy job"), "{err}");
}

#[test]
fn timeout_run_runs_anyway() {
    need_git!();
    let d = tmp("trun");
    let _s = hold(&d);
    let o = agentgc(&d)
        .args(["--slots", "1", "--timeout", "1", "--on-timeout", "run", "--", "git", "--version"])
        .output()
        .unwrap();
    assert_eq!(code(&o), 0);
    assert!(String::from_utf8_lossy(&o.stdout).contains("git version"));
    // the foreign info file stays untouched
    assert!(lock::info_path(&lock::slot_path(&d, "default", "default", 0)).exists());
}

#[test]
fn nested_run_does_not_wait() {
    need_git!();
    let d = tmp("nested");
    let _s = hold(&d);
    let o = agentgc(&d)
        .env("AGENTGC_HELD", "default:default")
        .args(["--slots", "1", "--timeout", "5", "--", "git", "--version"])
        .output()
        .unwrap();
    assert_eq!(code(&o), 0);
    assert!(o.stderr.is_empty(), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn ci_skips_queue_unless_no_skip_ci() {
    need_git!();
    let d = tmp("ci");
    let _s = hold(&d);
    let o = agentgc(&d).env("CI", "true").args(["--slots", "1", "--timeout", "5", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    let o = agentgc(&d)
        .env("CI", "true")
        .args(["--slots", "1", "--timeout", "1", "--no-skip-ci", "--", "git", "--version"])
        .output()
        .unwrap();
    assert_eq!(code(&o), 75);
}

#[test]
fn log_writes_json_line_for_queued_run() {
    need_git!();
    let d = tmp("log");
    let log = d.join("runs.jsonl");
    let o = agentgc(&d).arg("--log").arg(&log).args(["--slots", "1", "--resource", "phone", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    let text = fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    for k in ["start", "pool", "resources", "command", "cwd", "slots", "waited_ms", "ran_ms", "timed_out", "exit"] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(v["resources"][0], "phone");
    assert_eq!(v["exit"], 0);
    assert_eq!(v["timed_out"], false);

    // a run that skipped the queue is not logged
    let o = agentgc(&d).env("CI", "1").arg("--log").arg(&log).args(["--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1);
}

#[test]
fn status_shows_held_and_free() {
    let d = tmp("status");
    let _s = hold(&d);
    let free = lock::slot_path(&d, "default", "default", 1);
    drop(lock::try_acquire(&free, true).unwrap());
    let o = agentgc(&d).args(["--status", "--json"]).output().unwrap();
    assert_eq!(code(&o), 0);
    let v: serde_json::Value = serde_json::from_slice(&o.stdout).unwrap();
    let slots = &v["pools"][0]["resources"][0]["slots"];
    assert_eq!(slots[0]["held"], true);
    assert_eq!(slots[0]["pid"], 4242);
    assert_eq!(slots[1]["held"], false);
}

#[test]
fn bad_names_are_refused() {
    let d = tmp("names");
    let o = agentgc(&d).args(["--resource", "Bad Name", "--", "git", "--version"]).output().unwrap();
    assert_ne!(code(&o), 0);
    let o = agentgc(&d).args(["--pool", "a/b", "--", "git", "--version"]).output().unwrap();
    assert_ne!(code(&o), 0);
}

#[test]
fn min_interval_spaces_out_starts() {
    need_git!();
    let d = tmp("mi");
    let o = agentgc(&d).args(["--slots", "1", "--min-interval", "1", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    assert!(lock::last_path(&d, "default", "default").exists());
    let t = std::time::Instant::now();
    let o = agentgc(&d).args(["--slots", "1", "--min-interval", "1", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    assert!(t.elapsed() >= std::time::Duration::from_millis(800), "{:?}", t.elapsed());
    assert!(String::from_utf8_lossy(&o.stderr).contains("min interval"));
}

#[test]
fn min_interval_timeout_exits_75() {
    need_git!();
    let d = tmp("mit");
    let o = agentgc(&d).args(["--slots", "1", "--min-interval", "30", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 0);
    let o = agentgc(&d).args(["--slots", "1", "--min-interval", "30", "--timeout", "1", "--", "git", "--version"]).output().unwrap();
    assert_eq!(code(&o), 75);
}

#[test]
fn min_interval_rejects_bad_value() {
    let d = tmp("mib");
    let o = agentgc(&d).args(["--min-interval", "-1", "--", "git", "--version"]).output().unwrap();
    assert_ne!(code(&o), 0);
}
