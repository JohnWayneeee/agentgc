//! Integration tests for the compose file rendered by `task new` and removed by `task done`.
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

const CFG: &str = "[ports]\nnames=[\"web\",\"api\"]\nrange=[42100, 42200]\n[compose]\ntemplate=\"docker-compose.task.yml.tpl\"\noutput=\"docker-compose.task.yml\"\n[routes]\nweb=\"web\"\n";
const TPL: &str = "name: {{task}}\nservices:\n  web:\n    container_name: web-{{task}}\n    ports: [\"{{port.web}}:80\"]\n    environment:\n      DB: {{db}}\n      API: \"{{port.api}}\"\n";

fn make_repo(base: &Path, cfg: &str, tpl: &str) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
    fs::write(repo.join("docker-compose.task.yml.tpl"), tpl).unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "init"]);
    repo
}

fn agentgc(repo: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(env!("CARGO_BIN_EXE_agentgc"))
        .args(["task"])
        .args(args)
        .arg("--repo")
        .arg(repo)
        .output()
        .unwrap();
    let text = format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    (o.status.success(), text)
}

fn reg(repo: &Path, name: &str) -> PathBuf {
    repo.join(".agentgc").join("tasks").join(format!("{name}.json"))
}

#[test]
fn new_renders_file_routes_and_done_removes_only_that_file() {
    need_git!();
    let base = tmp("compose");
    let repo = make_repo(&base, CFG, TPL);
    let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(t.contains("web.one.localhost:"), "routes must be printed: {t}");
    let (ok, t) = agentgc(&repo, &["new", "two", "--yes"]);
    assert!(ok, "{t}");

    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(reg(&repo, "one")).unwrap()).unwrap();
    assert_eq!(v["compose_file"], "docker-compose.task.yml");
    let web1 = v["ports"]["web"].as_u64().unwrap();
    let api1 = v["ports"]["api"].as_u64().unwrap();
    let v2: serde_json::Value = serde_json::from_str(&fs::read_to_string(reg(&repo, "two")).unwrap()).unwrap();
    assert_ne!(web1, v2["ports"]["web"].as_u64().unwrap(), "tasks must not share ports");

    let wt = repo.join("worktrees/one");
    let rendered = fs::read_to_string(wt.join("docker-compose.task.yml")).unwrap();
    assert!(rendered.contains("name: one\n"), "{rendered}");
    assert!(rendered.contains("container_name: web-one"), "{rendered}");
    assert!(rendered.contains(&format!("\"{web1}:80\"")), "{rendered}");
    assert!(rendered.contains(&format!("API: \"{api1}\"")), "{rendered}");
    assert!(rendered.contains("DB: proj_one"), "{rendered}");
    assert!(!rendered.contains("{{"), "{rendered}");

    let env = fs::read_to_string(wt.join(".agentgc/task.env")).unwrap();
    assert!(env.contains("COMPOSE_PROJECT_NAME=agentgc-one\n"), "{env}");
    assert!(env.contains(&format!("AGENTGC_ROUTE_WEB=http://web.one.localhost:{web1}\n")), "{env}");

    let (ok, t) = agentgc(&repo, &["list"]);
    assert!(ok && t.contains(&format!("web=web.one.localhost:{web1}")), "{t}");

    // a file next to it must survive: done removes only the recorded output file
    let (ok, t) = agentgc(&repo, &["done", "one"]);
    assert!(ok && t.contains("docker compose -p agentgc-one down -v"), "{t}");
    assert!(wt.join("docker-compose.task.yml").exists(), "dry run must not delete");
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(t.contains("docker compose -p agentgc-one down -v"), "{t}");
    assert!(!wt.exists() && !reg(&repo, "one").exists());
    assert!(repo.join("docker-compose.task.yml.tpl").exists(), "template stays");
    assert!(repo.join("worktrees/two/docker-compose.task.yml").exists(), "other task untouched");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn unknown_placeholder_writes_nothing() {
    need_git!();
    let base = tmp("compose-bad");
    let repo = make_repo(&base, CFG, "x: {{port.nope}}\n");
    let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
    assert!(!ok && t.contains("{{port.nope}}"), "{t}");
    assert!(!repo.join("worktrees/one").exists());
    assert!(!reg(&repo, "one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn existing_output_is_never_overwritten() {
    need_git!();
    let base = tmp("compose-exist");
    let repo = make_repo(&base, CFG, TPL);
    fs::write(repo.join("docker-compose.task.yml"), "mine\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-m", "tracked output"]);
    let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
    assert!(!ok && t.contains("never overwritten"), "{t}");
    let out = repo.join("worktrees/one/docker-compose.task.yml");
    // git autocrlf may turn the line ending into CRLF on checkout
    assert_eq!(fs::read_to_string(&out).unwrap().trim(), "mine");
    // done cleans the incomplete task and does not delete a tracked file by itself
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!repo.join("worktrees/one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn bad_compose_config_is_rejected() {
    need_git!();
    let base = tmp("compose-cfg");
    let repo = make_repo(&base, CFG, TPL);
    for cfg in [
        "[compose]\ntemplate=\"t\"\noutput=\"../x.yml\"\n",
        "[ports]\nnames=[\"web\"]\n[routes]\nweb=\"nope\"\n",
        "[ports]\nnames=[\"web\"]\n[compose]\ntemplate=\"missing.tpl\"\noutput=\"x.yml\"\n",
    ] {
        fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
        let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
        assert!(!ok, "{cfg}: {t}");
        assert!(!reg(&repo, "one").exists() && !repo.join("worktrees/one").exists());
    }
    let _ = fs::remove_dir_all(&base);
}
