//! Integration tests for `agentgc hook session-start` through the real binary.
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("agentgc-hs-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&p);
    fs::create_dir_all(&p).unwrap();
    p
}

fn session_start(dir: &PathBuf) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .args(["hook", "session-start"])
        .current_dir(dir)
        .output()
        .unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned())
}

#[test]
fn task_folder_prints_env_and_rule() {
    let d = tmp("task");
    fs::create_dir_all(d.join(".agentgc")).unwrap();
    fs::write(d.join(".agentgc/task.env"), "PORT_WEB=42901\nDB_NAME=proj_one\n").unwrap();
    let (ok, out) = session_start(&d);
    assert!(ok, "{out}");
    assert!(out.contains("Task env (.agentgc/task.env)"), "{out}");
    assert!(out.contains("PORT_WEB=42901") && out.contains("DB_NAME=proj_one"), "{out}");
    assert!(out.contains("agentgc run -- COMMAND"), "{out}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn plain_folder_prints_only_the_rule() {
    let d = tmp("plain");
    let (ok, out) = session_start(&d);
    assert!(ok, "{out}");
    assert!(!out.contains("Task env"), "{out}");
    assert!(out.contains("agentgc run -- COMMAND"), "{out}");
    let _ = fs::remove_dir_all(&d);
}
