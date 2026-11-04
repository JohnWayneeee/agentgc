//! Integration tests for `task review NAME`. Temp repos only. It must stay read-only.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

static N: AtomicU32 = AtomicU32::new(0);

fn git_ok() -> bool {
    Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
}

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("agentgc-rv-{tag}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
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

fn make_repo(base: &Path) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    fs::write(repo.join("agentgc.task.toml"), "[ports]\nnames=[\"web\"]\nrange=[42900, 43000]\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

fn run(args: &[&str], repo: &Path) -> (bool, String, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentgc")).arg("task").args(args).arg("--repo").arg(repo).output().unwrap();
    (o.status.success(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn review_summarises_diff_commits_and_stays_read_only() {
    if !git_ok() {
        return;
    }
    let repo = make_repo(&tmp("main"));
    let (ok, _, e) = run(&["new", "one", "--yes"], &repo);
    assert!(ok, "{e}");
    let wt = repo.join("worktrees/one");
    fs::write(wt.join("a.txt"), "1\n2\n").unwrap();
    fs::write(wt.join("b.txt"), "x\n").unwrap();
    git(&wt, &["add", "."]);
    git(&wt, &["commit", "-m", "add a and b"]);
    fs::write(wt.join("a.txt"), "1\n2\n3\n").unwrap();
    git(&wt, &["commit", "-am", "extend a"]);
    fs::write(wt.join("dirty.txt"), "d\n").unwrap();
    fs::write(repo.join("m.txt"), "m\n").unwrap();
    git(&repo, &["add", "m.txt"]);
    git(&repo, &["commit", "-m", "main moves"]);

    let head_before = git(&wt, &["rev-parse", "HEAD"]);
    let (ok, out, e) = run(&["review", "one", "--json"], &repo);
    assert!(ok, "{e}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["name"], "one");
    assert_eq!((v["ahead"].as_u64(), v["behind"].as_u64(), v["changed"].as_u64()), (Some(2), Some(1), Some(1)), "{v}");
    assert_eq!((v["files_changed"].as_u64(), v["insertions"].as_u64(), v["deletions"].as_u64()), (Some(2), Some(4), Some(0)), "{v}");
    assert_eq!(v["state"], "ready");
    let c = v["commits"].as_array().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0]["subject"], "extend a");
    assert!(v["merge_hint"].as_str().unwrap().contains("rebase or merge"));

    let (ok, out, _) = run(&["review", "one"], &repo);
    assert!(ok);
    assert!(out.contains("ahead/behind: 2/1") && out.contains("a.txt") && out.contains("extend a"), "{out}");
    assert!(out.contains("Nothing was changed."));
    assert_eq!(git(&wt, &["rev-parse", "HEAD"]), head_before);
}

#[test]
fn review_unknown_task_fails() {
    if !git_ok() {
        return;
    }
    let repo = make_repo(&tmp("unk"));
    let (ok, _, e) = run(&["review", "nope"], &repo);
    assert!(!ok);
    assert!(e.contains("no task named 'nope'"), "{e}");
}
