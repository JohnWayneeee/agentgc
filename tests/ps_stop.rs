//! Integration tests for `ps --stop`. They never stop a real process.
//!
//! Skipped on purpose: a test that starts a sleeping child and stops it with
//! `--pid --yes --force`. The built-in classifier marks a plain `sleep` or `ping` as `other`,
//! which is never a stop candidate, so there is no safe way to stop it through the CLI.
use std::process::{Child, Command, Output, Stdio};

fn agentgc(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run agentgc")
}

fn sleeper() -> Child {
    let mut c = if cfg!(windows) {
        let mut c = Command::new("ping");
        c.args(["-n", "30", "127.0.0.1"]);
        c
    } else {
        let mut c = Command::new("sleep");
        c.arg("30");
        c
    };
    c.current_dir(std::env::temp_dir()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("spawn child")
}

#[test]
fn dry_run_stops_nothing() {
    let o = agentgc(&["ps", "--stop"]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let out = String::from_utf8_lossy(&o.stdout);
    assert!(out.contains("dry-run: nothing stopped") || out.contains("No stop candidates"), "{out}");
}

#[test]
fn non_candidate_pid_is_an_error_and_child_survives() {
    let mut child = sleeper();
    let pid = child.id().to_string();
    let o = agentgc(&["ps", "--stop", "--yes", "--force", "--pid", &pid]);
    assert!(!o.status.success());
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(err.contains("not a stop candidate"), "{err}");
    assert!(child.try_wait().unwrap().is_none(), "the child must still run");
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
fn stop_conflicts_with_json_and_flags_need_stop() {
    assert!(!agentgc(&["ps", "--stop", "--json"]).status.success());
    assert!(!agentgc(&["ps", "--yes"]).status.success());
    assert!(!agentgc(&["ps", "--pid", "1"]).status.success());
}
