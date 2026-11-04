use crate::task_config::{self as tc, Config};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_CONFIG: &str = "agentgc.task.toml";

pub struct NewArgs {
    pub name: String,
    pub repo: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub run_setup: bool,
    pub yes: bool,
}

/// Registry entry: `<repo>/.agentgc/tasks/<name>.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub branch: String,
    pub base: String,
    pub ports: BTreeMap<String, u16>,
    pub db: String,
    pub created_at: u64,
    pub setup_done: bool,
    pub drop: String,
}

/// Everything `new` will do, computed before anything changes.
#[derive(Debug)]
pub struct Plan {
    pub name: String,
    pub root: PathBuf,
    pub wt_path: PathBuf,
    pub wt_dir: String,
    pub branch: String,
    pub base: String,
    pub copy: Vec<(String, bool)>,
    pub ports: Vec<(String, u16)>,
    pub db: String,
    pub env: Vec<(String, String)>,
    pub create: Option<String>,
    pub setup: Vec<String>,
    pub drop_tpl: String,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn tasks_dir(root: &Path) -> PathBuf {
    root.join(".agentgc").join("tasks")
}

/// Nearest folder at or above `start` that has a `.git` directory.
pub fn find_repo_root(start: &Path) -> Result<PathBuf, String> {
    let abs = std::path::absolute(start).map_err(|e| format!("bad path {}: {e}", start.display()))?;
    let mut cur = Some(abs.as_path());
    while let Some(d) = cur {
        if d.join(".git").is_dir() {
            return Ok(d.to_path_buf());
        }
        cur = d.parent();
    }
    Err(format!("no git repository (folder with a .git directory) at or above {}", abs.display()))
}

pub fn read_registry(root: &Path) -> Vec<Entry> {
    let mut v = Vec::new();
    let Ok(rd) = fs::read_dir(tasks_dir(root)) else { return v };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = fs::read_to_string(&p) {
            if let Ok(en) = serde_json::from_str::<Entry>(&text) {
                v.push(en);
            }
        }
    }
    v.sort_by(|a, b| a.name.cmp(&b.name));
    v
}

fn port_is_free(p: u16) -> bool {
    TcpListener::bind(("127.0.0.1", p)).is_ok()
}

/// Split by whitespace into program and arguments. No shell, no quotes.
pub fn split_command(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

fn valid_branch(b: &str) -> bool {
    !b.is_empty()
        && !b.starts_with('-')
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.contains("..")
        && b.chars().all(|c| c.is_ascii_alphanumeric() || "._/-".contains(c))
}

fn port_env_key(name: &str) -> String {
    format!("PORT_{}", name.to_uppercase().replace('-', "_"))
}

/// Build the plan. `reserved` = ports of registered tasks. Touches nothing.
pub fn build_plan(
    cfg: &Config,
    name: &str,
    root: &Path,
    reserved: &HashSet<u16>,
    is_free: &dyn Fn(u16) -> bool,
) -> Result<Plan, String> {
    let name = tc::sanitize_name(name)?;
    let repo = tc::sanitize_repo(root.file_name().and_then(|n| n.to_str()).unwrap_or("repo"));
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("name".into(), name.clone());
    vars.insert("repo".into(), repo);

    let branch = tc::fill(&cfg.worktree.branch, &vars)?;
    if !valid_branch(&branch) {
        return Err(format!("branch {branch:?} is not allowed (letters, digits, . _ / - only)"));
    }
    let db = tc::fill(&cfg.db.name, &vars)?;
    vars.insert("db".into(), db.clone());

    let ports = tc::allocate_ports(
        &cfg.ports.names,
        (cfg.ports.range[0], cfg.ports.range[1]),
        reserved,
        is_free,
    )?;
    for (n, p) in &ports {
        vars.insert(format!("port.{n}"), p.to_string());
    }

    let mut env: Vec<(String, String)> = Vec::new();
    tc::put(&mut env, "TASK_ID", name.clone());
    tc::put(&mut env, "PREFIX", format!("{name}_"));
    tc::put(&mut env, "DB_NAME", db.clone());
    for (n, p) in &ports {
        tc::put(&mut env, &port_env_key(n), p.to_string());
    }
    for (k, v) in &cfg.env.vars {
        tc::put(&mut env, k, tc::fill(v, &vars)?);
    }
    for (k, v) in &cfg.env.dynamic {
        tc::put(&mut env, k, tc::fill(v, &vars)?);
    }
    tc::render_env(&env)?; // checks line breaks early

    let create = if cfg.db.create.trim().is_empty() {
        None
    } else {
        Some(tc::fill(&cfg.db.create, &vars)?)
    };
    let mut setup = Vec::new();
    for c in &cfg.setup.commands {
        setup.push(tc::fill(c, &vars)?);
    }
    let copy = cfg.copy.files.iter().map(|f| (f.clone(), root.join(f).is_file())).collect();

    Ok(Plan {
        wt_path: root.join(&cfg.worktree.dir).join(&name),
        wt_dir: cfg.worktree.dir.clone(),
        name,
        root: root.to_path_buf(),
        branch,
        base: cfg.worktree.base.clone(),
        copy,
        ports,
        db,
        env,
        create,
        setup,
        drop_tpl: cfg.db.drop.clone(),
    })
}

fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))
}

/// `Some(true)` ignored, `Some(false)` not ignored, `None` unknown.
fn is_ignored(root: &Path, rel: &str) -> Option<bool> {
    let o = git(root, &["check-ignore", "-q", "--", rel]).ok()?;
    match o.status.code() {
        Some(0) => Some(true),
        Some(1) => Some(false),
        _ => None,
    }
}

fn print_plan(p: &Plan, run_setup: bool, config_note: &str) {
    println!("Plan for task {:?} ({})", p.name, config_note);
    println!("  worktree: {}", p.wt_path.display());
    println!("  branch:   {}  (from {})", p.branch, p.base);
    println!("  command:  git worktree add -b {} {} {}", p.branch, p.wt_path.display(), p.base);
    println!("  worktree dir: {}/ (task folders live here)", p.wt_dir);
    println!("  copy files:");
    if p.copy.is_empty() {
        println!("    (none listed)");
    }
    for (f, ex) in &p.copy {
        println!("    {f}  {}", if *ex { "will copy" } else { "not found, skipped" });
    }
    if p.ports.is_empty() {
        println!("  ports:    (none)");
    } else {
        let s: Vec<String> = p.ports.iter().map(|(n, v)| format!("{n}={v}")).collect();
        println!("  ports:    {}", s.join("  "));
    }
    println!("  db name:  {}  (agentgc never connects to a database)", p.db);
    println!("  env file: {}", p.wt_path.join(".agentgc").join("task.env").display());
    for (k, v) in &p.env {
        println!("    {k}={}", tc::mask_value(k, v));
    }
    println!(
        "  registry: {}",
        tasks_dir(&p.root).join(format!("{}.json", p.name)).display()
    );
    let mut cmds: Vec<(&str, &String)> = Vec::new();
    if let Some(c) = &p.create {
        cmds.push(("create", c));
    }
    for c in &p.setup {
        cmds.push(("setup", c));
    }
    if cmds.is_empty() {
        println!("  commands: (none)");
    }
    for (kind, c) in cmds {
        if run_setup {
            println!("  {kind}: {c}  (will run)");
        } else {
            println!("  {kind}: {c}  (not run, use --run-setup)");
        }
    }
    if !p.drop_tpl.trim().is_empty() {
        println!("  drop: stored for the future `done`, never run by `new`");
    }
}

fn print_ignore_hints(p: &Plan) {
    let probe = format!("{}/{}/x", p.wt_dir.trim_end_matches('/'), p.name);
    if is_ignored(&p.root, &probe) == Some(false) {
        println!("Hint: {}/ is not git-ignored; add it to .gitignore (agentgc does not edit it)", p.wt_dir);
    }
    if is_ignored(&p.root, ".agentgc/tasks/x.json") == Some(false) {
        println!("Hint: .agentgc/ should be git-ignored; add it to .gitignore (agentgc does not edit it)");
    }
}

fn write_new(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    if let Some(d) = path.parent() {
        fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create {} (never overwritten): {e}", path.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Run one command: whitespace split, no shell, cwd = dir. Err names the failure.
fn run_command(cmd: &str, dir: &Path) -> Result<(), String> {
    let parts = split_command(cmd);
    let Some((prog, args)) = parts.split_first() else { return Ok(()) };
    println!("  running: {cmd}");
    let st = Command::new(prog)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("command {cmd:?} could not start: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("command {cmd:?} failed ({st})"))
    }
}

pub fn load_config(root: &Path, config: Option<&Path>) -> Result<(Config, String), String> {
    let (path, explicit) = match config {
        Some(p) => (p.to_path_buf(), true),
        None => (root.join(DEFAULT_CONFIG), false),
    };
    match fs::read_to_string(&path) {
        Ok(t) => Ok((tc::parse_config(&t)?, format!("config: {}", path.display()))),
        Err(e) if !explicit && e.kind() == std::io::ErrorKind::NotFound => {
            Ok((Config::default(), format!("no {DEFAULT_CONFIG}, built-in defaults")))
        }
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    }
}

/// Execute the plan. Never overwrites a file, never deletes.
pub fn execute(p: &Plan, run_setup: bool) -> Result<(), String> {
    let out = git(
        &p.root,
        &["worktree", "add", "-b", &p.branch, &p.wt_path.to_string_lossy(), &p.base],
    )?;
    if !out.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    println!("created worktree {} on branch {}", p.wt_path.display(), p.branch);

    for (f, ex) in &p.copy {
        if !*ex {
            continue;
        }
        let dest = p.wt_path.join(f);
        if dest.exists() {
            println!("  skip {f}: already exists in the worktree");
            continue;
        }
        if let Some(d) = dest.parent() {
            fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
        }
        fs::copy(p.root.join(f), &dest).map_err(|e| format!("cannot copy {f}: {e}"))?;
        println!("  copied {f}");
    }

    let env_path = p.wt_path.join(".agentgc").join("task.env");
    write_new(&env_path, &tc::render_env(&p.env)?)?;
    println!("wrote {}", env_path.display());

    let mut setup_done = false;
    let mut failure = None;
    if run_setup {
        let mut all: Vec<&String> = Vec::new();
        if let Some(c) = &p.create {
            all.push(c);
        }
        all.extend(p.setup.iter());
        setup_done = true;
        for c in all {
            if let Err(e) = run_command(c, &p.wt_path) {
                failure = Some(e);
                setup_done = false;
                break;
            }
        }
    } else {
        for c in p.create.iter().chain(p.setup.iter()) {
            println!("  not run (use --run-setup): {c}");
        }
    }

    let entry = Entry {
        name: p.name.clone(),
        path: p.wt_path.to_string_lossy().into_owned(),
        branch: p.branch.clone(),
        base: p.base.clone(),
        ports: p.ports.iter().cloned().collect(),
        db: p.db.clone(),
        created_at: now_secs(),
        setup_done,
        drop: p.drop_tpl.clone(),
    };
    let reg = tasks_dir(&p.root).join(format!("{}.json", p.name));
    write_new(&reg, &serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())?)?;
    println!("registered {}", reg.display());
    if let Some(e) = failure {
        return Err(format!("{e}; the task is registered, fix the problem and run the rest by hand"));
    }
    Ok(())
}

pub fn cmd_new(a: &NewArgs) -> Result<(), String> {
    let name = tc::sanitize_name(&a.name)?;
    let root = find_repo_root(a.repo.as_deref().unwrap_or(Path::new(".")))?;
    let (cfg, note) = load_config(&root, a.config.as_deref())?;
    let registry = read_registry(&root);
    if registry.iter().any(|e| e.name == name) || tasks_dir(&root).join(format!("{name}.json")).exists() {
        return Err(format!("task {name:?} already exists in the registry"));
    }
    let reserved: HashSet<u16> = registry.iter().flat_map(|e| e.ports.values().copied()).collect();
    let plan = build_plan(&cfg, &name, &root, &reserved, &port_is_free)?;
    if plan.wt_path.exists() {
        return Err(format!("target folder already exists: {}", plan.wt_path.display()));
    }
    print_plan(&plan, a.run_setup, &note);
    if !a.yes {
        println!("Dry run: nothing changed. Add --yes to execute.");
        print_ignore_hints(&plan);
        return Ok(());
    }
    execute(&plan, a.run_setup)?;
    print_ignore_hints(&plan);
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct Row {
    #[serde(flatten)]
    pub entry: Entry,
    pub age_days: u64,
    /// `ok`, `missing`, `dirty` or `unknown`; may hold `dirty` next to `ok`.
    pub status: Vec<String>,
}

fn status_of(path: &Path) -> Vec<String> {
    if !path.is_dir() || !path.join(".git").exists() {
        return vec!["missing".into()];
    }
    match git(path, &["status", "--porcelain"]) {
        Ok(o) if o.status.success() => {
            if o.stdout.iter().any(|b| !b.is_ascii_whitespace()) {
                vec!["ok".into(), "dirty".into()]
            } else {
                vec!["ok".into()]
            }
        }
        _ => vec!["ok".into(), "unknown".into()],
    }
}

pub fn collect_rows(root: &Path) -> Vec<Row> {
    let now = now_secs();
    read_registry(root)
        .into_iter()
        .map(|e| {
            let status = status_of(Path::new(&e.path));
            let age_days = now.saturating_sub(e.created_at) / 86400;
            Row { entry: e, age_days, status }
        })
        .collect()
}

pub fn cmd_list(repo: Option<PathBuf>, json: bool) -> Result<(), String> {
    let root = find_repo_root(repo.as_deref().unwrap_or(Path::new(".")))?;
    let rows = collect_rows(&root);
    if json {
        let v = serde_json::json!({ "tasks": rows });
        println!("{}", serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("No tasks registered.");
        return Ok(());
    }
    for r in &rows {
        let e = &r.entry;
        let ports: Vec<String> = e.ports.iter().map(|(n, p)| format!("{n}={p}")).collect();
        println!(
            "{}  [{}]  {}d\n    path:   {}\n    branch: {}\n    ports:  {}\n    db:     {}",
            e.name,
            r.status.join(","),
            r.age_days,
            e.path,
            e.branch,
            if ports.is_empty() { "-".into() } else { ports.join(" ") },
            e.db
        );
    }
    println!("{} tasks. Nothing was changed.", rows.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static N: AtomicU32 = AtomicU32::new(0);

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "agentgc-task-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn git_ok() -> bool {
        Command::new("git").arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
    }

    fn make_repo(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        let run = |args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        run(&["init", "-b", "main"]);
        run(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--allow-empty", "-m", "init"]);
    }

    fn cfg(text: &str) -> Config {
        tc::parse_config(text).unwrap()
    }

    #[test]
    fn plan_fills_everything() {
        let c = cfg(
            "[db]\ncreate=\"createdb {db}\"\n[env]\nFOO=\"{name}-x\"\n[env.dynamic]\nDATABASE_URL=\"postgres://u:p@localhost:{port.api}/{db}\"\n[setup]\ncommands=[\"echo {name}\"]\n",
        );
        let root = PathBuf::from("/tmp/My Repo");
        let reserved: HashSet<u16> = [3100].into_iter().collect();
        let p = build_plan(&c, "demo", &root, &reserved, &|_| true).unwrap();
        assert_eq!(p.db, "my_repo_demo");
        assert_eq!(p.branch, "demo");
        assert_eq!(p.ports, vec![("web".to_string(), 3101), ("api".to_string(), 3102)]);
        let env = tc::render_env(&p.env).unwrap();
        assert!(env.starts_with("TASK_ID=demo\nPREFIX=demo_\nDB_NAME=my_repo_demo\nPORT_WEB=3101\nPORT_API=3102\n"));
        assert!(env.contains("FOO=demo-x\n"));
        assert!(env.contains("DATABASE_URL=postgres://u:p@localhost:3102/my_repo_demo\n"));
        assert_eq!(p.create.as_deref(), Some("createdb my_repo_demo"));
        assert_eq!(p.setup, vec!["echo demo"]);
    }

    #[test]
    fn plan_rejects_bad_input() {
        let c = Config::default();
        let root = PathBuf::from("/tmp/r");
        assert!(build_plan(&c, "Bad Name", &root, &HashSet::new(), &|_| true).is_err());
        let c = cfg("[worktree]\nbranch=\"--evil\"");
        assert!(build_plan(&c, "ok", &root, &HashSet::new(), &|_| true).is_err());
        let c = cfg("[env]\nA=\"{nope}\"");
        assert!(build_plan(&c, "ok", &root, &HashSet::new(), &|_| true).is_err());
    }

    #[test]
    fn split_no_shell() {
        assert_eq!(split_command("  pnpm  install --frozen-lockfile "), vec!["pnpm", "install", "--frozen-lockfile"]);
        assert_eq!(split_command("echo a;rm -rf x"), vec!["echo", "a;rm", "-rf", "x"]);
    }

    #[test]
    fn new_yes_and_list() {
        if !git_ok() {
            eprintln!("note: git not available, skipping new_yes_and_list");
            return;
        }
        let base = tmp("flow");
        let repo = base.join("proj");
        make_repo(&repo);
        fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
        fs::write(
            repo.join(DEFAULT_CONFIG),
            "[ports]\nnames=[\"web\"]\nrange=[41200, 41300]\n[env.dynamic]\nDATABASE_URL=\"postgres://localhost/{db}\"\n",
        )
        .unwrap();
        let args = |name: &str, yes: bool| NewArgs {
            name: name.into(),
            repo: Some(repo.clone()),
            config: None,
            run_setup: false,
            yes,
        };
        // dry run changes nothing
        cmd_new(&args("one", false)).unwrap();
        assert!(!repo.join("worktrees/one").exists());
        assert!(read_registry(&repo).is_empty());
        // real run
        cmd_new(&args("one", true)).unwrap();
        let wt = repo.join("worktrees/one");
        assert!(wt.join(".git").exists());
        assert_eq!(fs::read_to_string(wt.join(".env")).unwrap(), "SECRET=1\n");
        let env = fs::read_to_string(wt.join(".agentgc/task.env")).unwrap();
        assert!(env.contains("TASK_ID=one\n") && env.contains("DB_NAME=proj_one\n"));
        assert!(env.contains("DATABASE_URL=postgres://localhost/proj_one\n"));
        let reg = read_registry(&repo);
        assert_eq!(reg.len(), 1);
        assert_eq!(reg[0].branch, "one");
        assert!(!reg[0].setup_done);
        let p1 = reg[0].ports["web"];
        // duplicate name refused
        assert!(cmd_new(&args("one", true)).unwrap_err().contains("already exists"));
        // second task gets another port
        cmd_new(&args("two", true)).unwrap();
        let reg = read_registry(&repo);
        assert_eq!(reg.len(), 2);
        assert_ne!(reg[1].ports["web"], p1);
        // existing folder refused
        fs::create_dir_all(repo.join("worktrees/three")).unwrap();
        assert!(cmd_new(&args("three", true)).unwrap_err().contains("already exists"));
        // list: ok, dirty, missing
        let rows = collect_rows(&repo);
        assert_eq!(rows[0].status, vec!["ok", "dirty"]); // .env + .agentgc untracked
        fs::remove_dir_all(repo.join("worktrees/two")).unwrap();
        let rows = collect_rows(&repo);
        assert_eq!(rows[1].status, vec!["missing"]);
        cmd_list(Some(repo.clone()), true).unwrap();
        cmd_list(Some(repo.clone()), false).unwrap();
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setup_runs_only_with_flag_and_stops_on_failure() {
        if !git_ok() {
            eprintln!("note: git not available, skipping setup test");
            return;
        }
        let base = tmp("setup");
        let repo = base.join("proj");
        make_repo(&repo);
        fs::write(
            repo.join(DEFAULT_CONFIG),
            "[ports]\nnames=[]\n[setup]\ncommands=[\"git --version\", \"git no-such-subcommand-xyz\"]\n",
        )
        .unwrap();
        let mk = |name: &str, run_setup: bool| NewArgs {
            name: name.into(),
            repo: Some(repo.clone()),
            config: None,
            run_setup,
            yes: true,
        };
        cmd_new(&mk("a", false)).unwrap();
        assert!(!read_registry(&repo)[0].setup_done);
        let e = cmd_new(&mk("b", true)).unwrap_err();
        assert!(e.contains("no-such-subcommand-xyz") && e.contains("failed"));
        let reg = read_registry(&repo);
        assert_eq!(reg.len(), 2);
        assert!(!reg[1].setup_done);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn not_a_repo_is_clear_error() {
        let d = tmp("norepo");
        let e = find_repo_root(&d).unwrap_err();
        assert!(e.contains("no git repository"));
        let _ = fs::remove_dir_all(&d);
    }
}
