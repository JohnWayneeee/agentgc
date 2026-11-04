//! Integration tests for `task new` and `task done` with real temp git repos.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
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

fn git_try(dir: &Path, args: &[&str]) -> bool {
    Command::new("git").arg("-C").arg(dir).args(args).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Repo `<base>/proj` with one commit on `main`, and a task config with no ports.
fn make_repo(base: &Path, extra_cfg: &str) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    fs::write(repo.join("agentgc.task.toml"), format!("[ports]\nnames=[]\n{extra_cfg}")).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

fn agentgc(repo: &Path, args: &[&str]) -> (bool, String) {
    let o: Output = Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .args(["task"])
        .args(args)
        .arg("--repo")
        .arg(repo)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    (o.status.success(), text)
}

fn new_task(repo: &Path, name: &str) {
    let (ok, t) = agentgc(repo, &["new", name, "--yes"]);
    assert!(ok, "new failed: {t}");
}

fn reg(repo: &Path, name: &str) -> PathBuf {
    repo.join(".agentgc").join("tasks").join(format!("{name}.json"))
}

#[test]
fn clean_task_is_removed() {
    need_git!();
    let base = tmp("clean");
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let wt = repo.join("worktrees").join("one");
    assert!(wt.join(".git").exists() && reg(&repo, "one").exists());
    assert!(git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(t.contains("freed about"), "{t}");
    assert!(!wt.exists(), "worktree folder must be gone");
    assert!(!git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]), "branch must be gone");
    assert!(!reg(&repo, "one").exists());
    // main repo untouched
    assert_eq!(fs::read_to_string(repo.join("README.md")).unwrap(), "hello\n");
    assert_eq!(git(&repo, &["rev-list", "--count", "main"]), "1");
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn dry_run_changes_nothing() {
    need_git!();
    let base = tmp("dry");
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let before = fs::read_to_string(reg(&repo, "one")).unwrap();
    let (ok, t) = agentgc(&repo, &["done", "one"]);
    assert!(ok, "{t}");
    assert!(t.contains("Dry run: nothing changed"), "{t}");
    assert!(repo.join("worktrees/one/.agentgc/task.env").exists());
    assert!(git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    assert_eq!(fs::read_to_string(reg(&repo, "one")).unwrap(), before);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn uncommitted_file_blocks_and_force_removes() {
    need_git!();
    let base = tmp("dirty");
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");
    fs::write(wt.join("notes.txt"), "precious").unwrap();
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(!ok, "{t}");
    assert!(t.contains("notes.txt") && t.contains("BLOCKER"), "{t}");
    assert!(wt.join("notes.txt").exists() && reg(&repo, "one").exists());
    assert!(wt.join(".agentgc/task.env").exists(), "blocked run must not delete anything");
    let (ok, t) = agentgc(&repo, &["done", "one", "--force", "--yes"]);
    assert!(ok, "{t}");
    assert!(t.contains("FORCE: this will discard"), "{t}");
    assert!(!wt.exists() && !reg(&repo, "one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn unmerged_commit_blocks_until_merged() {
    need_git!();
    let base = tmp("commit");
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");
    fs::write(wt.join("feature.txt"), "x").unwrap();
    git(&wt, &["add", "feature.txt"]);
    git(&wt, &["commit", "-m", "feature"]);
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(!ok, "{t}");
    assert!(t.contains("1 commits are neither merged into main nor pushed"), "{t}");
    assert!(wt.exists());
    git(&repo, &["merge", "one"]);
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!wt.exists() && !reg(&repo, "one").exists());
    assert!(!git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    assert!(repo.join("feature.txt").exists(), "merged work stays in the main checkout");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn path_guard_refuses_outside_path_even_with_force() {
    need_git!();
    let base = tmp("guard");
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let victim = base.join("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("keep.txt"), "keep").unwrap();
    let p = reg(&repo, "one");
    let text = fs::read_to_string(&p).unwrap();
    let real = repo.join("worktrees/one");
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    let old = json["path"].as_str().unwrap().to_string();
    assert!(Path::new(&old) == real || !old.is_empty());
    let mut v = json.clone();
    v["path"] = serde_json::Value::String(victim.to_string_lossy().into_owned());
    fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    for extra in [vec![], vec!["--force"]] {
        let mut args = vec!["done", "one", "--yes"];
        args.extend(extra);
        let (ok, t) = agentgc(&repo, &args);
        assert!(!ok && t.contains("PATH GUARD"), "{t}");
        assert!(victim.join("keep.txt").exists());
    }
    // the repo root itself and a parent path are refused too
    for bad in [repo.to_string_lossy().into_owned(), repo.join("worktrees").to_string_lossy().into_owned()] {
        v["path"] = serde_json::Value::String(bad);
        fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
        let (ok, t) = agentgc(&repo, &["done", "one", "--yes", "--force"]);
        assert!(!ok && t.contains("PATH GUARD"), "{t}");
        assert!(repo.join("README.md").exists() && real.exists());
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn drop_template_runs_with_yes() {
    need_git!();
    let base = tmp("drop");
    let repo = make_repo(&base, "[db]\ndrop=\"git config agentgc.dropped {db}\"\n");
    new_task(&repo, "one");
    let (ok, t) = agentgc(&repo, &["done", "one"]);
    assert!(ok && t.contains("git config agentgc.dropped proj_one"), "{t}");
    assert!(!git_try(&repo, &["config", "--get", "agentgc.dropped"]), "dry run must not run drop");
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert_eq!(git(&repo, &["config", "--get", "agentgc.dropped"]), "proj_one");
    assert!(!reg(&repo, "one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn disconnect_runs_before_drop_with_yes() {
    need_git!();
    let base = tmp("disc");
    let repo = make_repo(&base, "[db]
disconnect=\"git config agentgc.order {db}\"
drop=\"git config --add agentgc.order dropped\"
");
    new_task(&repo, "one");
    let (ok, t) = agentgc(&repo, &["done", "one"]);
    assert!(ok && t.contains("run disconnect command: git config agentgc.order proj_one"), "{t}");
    assert!(!git_try(&repo, &["config", "--get", "agentgc.order"]), "dry run must not run disconnect");
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    let got = git(&repo, &["config", "--get-all", "agentgc.order"]);
    let lines: Vec<&str> = got.lines().map(|l| l.trim()).collect();
    assert_eq!(lines, ["proj_one", "dropped"]);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn disconnect_failure_skips_drop() {
    need_git!();
    let base = tmp("discfail");
    let repo = make_repo(&base, "[db]
disconnect=\"git no-such-subcommand-xyz\"
drop=\"git config agentgc.dropped {db}\"
");
    new_task(&repo, "one");
    let (ok, _) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(!ok);
    assert!(!git_try(&repo, &["config", "--get", "agentgc.dropped"]));
    assert!(reg(&repo, "one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn incomplete_entry_can_be_removed() {
    need_git!();
    let base = tmp("creating");
    let repo = make_repo(&base, "");
    // case 1: real worktree, entry flipped to creating
    new_task(&repo, "one");
    let p = reg(&repo, "one");
    let mut v: serde_json::Value = serde_json::from_str(&fs::read_to_string(&p).unwrap()).unwrap();
    assert_eq!(v["state"], "ready");
    v["state"] = "creating".into();
    fs::write(&p, serde_json::to_string_pretty(&v).unwrap()).unwrap();
    let (_, t) = agentgc(&repo, &["list"]);
    assert!(t.contains("incomplete"), "{t}");
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!p.exists() && !repo.join("worktrees/one").exists());
    // case 2: crash before git worktree add: entry only, no folder, no branch
    let p2 = reg(&repo, "two");
    let entry = serde_json::json!({
        "name": "two",
        "path": repo.join("worktrees").join("two").to_string_lossy(),
        "branch": "two", "base": "main", "ports": {}, "db": "proj_two",
        "created_at": 1, "setup_done": false, "drop": "", "state": "creating"
    });
    fs::write(&p2, serde_json::to_string_pretty(&entry).unwrap()).unwrap();
    let (ok, t) = agentgc(&repo, &["done", "two", "--yes"]);
    assert!(ok, "{t}");
    assert!(!p2.exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn failed_new_leaves_creating_entry_and_done_cleans_up() {
    need_git!();
    let base = tmp("failnew");
    let repo = make_repo(&base, "[setup]\ncommands=[\"git no-such-subcommand-xyz\"]\n");
    let (ok, t) = agentgc(&repo, &["new", "one", "--yes", "--run-setup"]);
    assert!(!ok && t.contains("task done one"), "{t}");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(reg(&repo, "one")).unwrap()).unwrap();
    assert_eq!(v["state"], "creating");
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!reg(&repo, "one").exists() && !repo.join("worktrees/one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn bad_base_and_branch_from_config_are_rejected() {
    need_git!();
    let base = tmp("badref");
    let repo = make_repo(&base, "");
    for cfg in ["[worktree]\nbase=\"-x\"\n", "[worktree]\nbranch=\"a b\"\n", "[worktree]\nbase=\"a\\tb\"\n"] {
        fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
        let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
        assert!(!ok && t.contains("worktree."), "{cfg}: {t}");
        assert!(!reg(&repo, "one").exists());
    }
    let _ = fs::remove_dir_all(&base);
}

/// Stuck state: git removed the worktree, an empty folder, the branch and the registry file stay.
fn stuck_task(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let base = tmp(tag);
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let wt = repo.join("worktrees").join("one");
    fs::remove_dir_all(wt.join(".agentgc")).unwrap();
    git(&repo, &["worktree", "remove", wt.to_str().unwrap()]);
    fs::create_dir_all(&wt).unwrap();
    (base, repo, wt)
}

#[test]
fn resume_finishes_after_partial_failure() {
    need_git!();
    let (base, repo, wt) = stuck_task("resume");
    assert!(!git(&repo, &["worktree", "list", "--porcelain"]).contains("worktrees/one"));
    let (ok, t) = agentgc(&repo, &["done", "one"]);
    assert!(ok && t.contains("RESUME") && t.contains("Dry run: nothing changed"), "{t}");
    assert!(wt.is_dir() && reg(&repo, "one").exists());
    assert!(git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!wt.exists() && !reg(&repo, "one").exists());
    assert!(!git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    assert_eq!(git(&repo, &["status", "--porcelain"]), "");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn resume_refuses_non_empty_folder_without_git_file() {
    need_git!();
    let (base, repo, wt) = stuck_task("resume-full");
    fs::write(wt.join(".hidden"), "x").unwrap();
    for args in [vec!["done", "one"], vec!["done", "one", "--yes"], vec!["done", "one", "--yes", "--force"]] {
        let (ok, t) = agentgc(&repo, &args);
        assert!(!ok && t.contains("no `.git` file"), "{t}");
        assert!(wt.join(".hidden").exists() && reg(&repo, "one").exists());
        assert!(git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    }
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn resume_refuses_when_git_still_lists_worktree() {
    need_git!();
    let base = tmp("resume-listed");
    let repo = make_repo(&base, "");
    new_task(&repo, "one");
    let wt = repo.join("worktrees").join("one");
    // empty the folder but leave git's record in place
    fs::remove_dir_all(&wt).unwrap();
    fs::create_dir_all(&wt).unwrap();
    assert!(git(&repo, &["worktree", "list", "--porcelain"]).contains("worktrees/one"));
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes", "--force"]);
    assert!(!ok && t.contains("git still lists it"), "{t}");
    assert!(wt.is_dir() && reg(&repo, "one").exists());
    assert!(git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    let _ = fs::remove_dir_all(&base);
}

/// `git branch -d` refuses (pushed, not merged into HEAD): done stops, keeps the registry, resumes later.
#[test]
fn branch_delete_refusal_is_resumable() {
    need_git!();
    let base = tmp("branch-refuse");
    let repo = make_repo(&base, "");
    let remote = base.join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");
    fs::write(wt.join("f.txt"), "x").unwrap();
    git(&wt, &["add", "f.txt"]);
    git(&wt, &["commit", "-m", "feature"]);
    git(&wt, &["push", "origin", "one"]);
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(!ok && t.contains("step e"), "{t}");
    assert!(t.contains("task done one --yes"), "{t}");
    assert!(!wt.exists(), "worktree is gone");
    assert!(reg(&repo, "one").exists(), "registry stays");
    assert!(git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    // same refusal on resume
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(!ok && t.contains("step e"), "{t}");
    assert!(reg(&repo, "one").exists());
    // merge, then resume succeeds
    git(&repo, &["merge", "one"]);
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!reg(&repo, "one").exists());
    assert!(!git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn branch_delete_refusal_resumes_with_force() {
    need_git!();
    let base = tmp("branch-refuse-force");
    let repo = make_repo(&base, "");
    let remote = base.join("remote.git");
    fs::create_dir_all(&remote).unwrap();
    git(&remote, &["init", "--bare"]);
    git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
    new_task(&repo, "one");
    let wt = repo.join("worktrees/one");
    fs::write(wt.join("f.txt"), "x").unwrap();
    git(&wt, &["add", "f.txt"]);
    git(&wt, &["commit", "-m", "feature"]);
    git(&wt, &["push", "origin", "one"]);
    let (ok, _) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(!ok);
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes", "--force"]);
    assert!(ok, "{t}");
    assert!(!reg(&repo, "one").exists());
    assert!(!git_try(&repo, &["rev-parse", "--verify", "refs/heads/one"]));
    let _ = fs::remove_dir_all(&base);
}
