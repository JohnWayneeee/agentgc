//! Integration tests for `[services.*]`: `task new` prints and writes commands, `task done` removes the files.
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

const CFG: &str = "[ports]\nnames=[\"web\",\"api\"]\nrange=[42300, 42400]\n\
[services.web]\ncmd=\"npm run dev -- --port {{port.web}}\"\nport=\"web\"\n\
[services.api]\ncmd=\"cargo run -- --db {{db}} --task {{task}}\"\ncwd=\"apps/api\"\nport=\"api\"\n";

fn make_repo(base: &Path, cfg: &str) -> PathBuf {
    let repo = base.join("proj");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-b", "main"]);
    fs::write(repo.join("README.md"), "hello\n").unwrap();
    fs::write(repo.join(".gitignore"), ".agentgc/\nworktrees/\n").unwrap();
    fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
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
fn new_writes_services_and_done_removes_them() {
    need_git!();
    let base = tmp("services");
    let repo = make_repo(&base, CFG);

    // dry run prints the commands and writes nothing
    let (ok, t) = agentgc(&repo, &["new", "one"]);
    assert!(ok && t.contains("npm run dev -- --port 42"), "{t}");
    assert!(!repo.join("worktrees/one").exists() && !reg(&repo, "one").exists());

    let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(t.contains("cargo run -- --db proj_one --task one"), "{t}");
    assert!(t.contains("web.one.localhost:") && t.contains("api.one.localhost:"), "{t}");

    let wt = repo.join("worktrees/one");
    let v: serde_json::Value = serde_json::from_str(&fs::read_to_string(reg(&repo, "one")).unwrap()).unwrap();
    let web = v["ports"]["web"].as_u64().unwrap();
    assert_eq!(v["services"], serde_json::json!(["api", "web"]));
    assert_eq!(v["generated_files"].as_array().unwrap().len(), 3);

    let s: serde_json::Value = serde_json::from_str(&fs::read_to_string(wt.join(".agentgc/services.json")).unwrap()).unwrap();
    let web_svc = s.as_array().unwrap().iter().find(|x| x["name"] == "web").unwrap();
    assert_eq!(web_svc["cmd"], format!("npm run dev -- --port {web}"));
    assert_eq!(web_svc["cwd"], ".");
    assert_eq!(web_svc["port"].as_u64().unwrap(), web);

    let sh = fs::read_to_string(wt.join(".agentgc/services.sh")).unwrap();
    assert!(sh.contains(&format!("cd . && npm run dev -- --port {web}\n")), "{sh}");
    assert!(sh.contains("cd apps/api && cargo run -- --db proj_one --task one\n"), "{sh}");
    assert!(!sh.contains('#'), "{sh}");
    let ps = fs::read_to_string(wt.join(".agentgc/services.ps1")).unwrap();
    assert!(ps.contains(&format!("Set-Location .; npm run dev -- --port {web}\n")), "{ps}");
    assert!(ps.contains("Set-Location apps/api; cargo run"), "{ps}");

    let env = fs::read_to_string(wt.join(".agentgc/task.env")).unwrap();
    assert!(env.contains(&format!("AGENTGC_ROUTE_WEB=http://web.one.localhost:{web}\n")), "{env}");

    let (ok, t) = agentgc(&repo, &["list"]);
    assert!(ok && t.contains("services: api web"), "{t}");

    // dry run keeps the files, --yes removes them with the folder
    let (ok, t) = agentgc(&repo, &["done", "one"]);
    assert!(ok && t.contains("services.json"), "{t}");
    assert!(wt.join(".agentgc/services.sh").exists());
    let (ok, t) = agentgc(&repo, &["done", "one", "--yes"]);
    assert!(ok, "{t}");
    assert!(!wt.join(".agentgc").exists(), "generated files and .agentgc must be gone");
    assert!(!wt.exists() && !reg(&repo, "one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn unknown_placeholder_in_service_writes_nothing() {
    need_git!();
    let base = tmp("services-bad");
    let repo = make_repo(&base, "[services.web]\ncmd=\"run {{port.nope}}\"\n");
    let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
    assert!(!ok && t.contains("{{port.nope}}"), "{t}");
    assert!(!repo.join("worktrees/one").exists());
    assert!(!reg(&repo, "one").exists());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn bad_services_config_is_rejected() {
    need_git!();
    let base = tmp("services-cfg");
    let repo = make_repo(&base, CFG);
    for cfg in [
        "[services.web]\ncmd=\"\"\n",
        "[services.web]\ncmd=\"x\"\ncwd=\"../x\"\n",
        "[services.web]\ncmd=\"x\"\nport=\"nope\"\n",
        "[services.Web]\ncmd=\"x\"\n",
    ] {
        fs::write(repo.join("agentgc.task.toml"), cfg).unwrap();
        let (ok, t) = agentgc(&repo, &["new", "one", "--yes"]);
        assert!(!ok, "{cfg}: {t}");
        assert!(!reg(&repo, "one").exists() && !repo.join("worktrees/one").exists());
    }
    let _ = fs::remove_dir_all(&base);
}
