use crate::compose;
use crate::out::outln;
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
    /// Command run before `drop`. Old entries have none.
    #[serde(default)]
    pub disconnect: String,
    /// `creating` while `new` runs (or after it failed), `ready` when done.
    /// Old entries have no state and count as ready.
    #[serde(default = "ready_state")]
    pub state: String,
    /// Rendered compose file, relative to the worktree. Empty when none.
    #[serde(default)]
    pub compose_file: String,
    /// Route name -> `<name>.<task>.localhost:<port>`.
    #[serde(default)]
    pub routes: BTreeMap<String, String>,
    /// Files `new` generated, relative to the worktree (services files). `done` removes them.
    #[serde(default)]
    pub generated_files: Vec<String>,
    /// Names of `[services.*]`.
    #[serde(default)]
    pub services: Vec<String>,
}

fn ready_state() -> String {
    "ready".into()
}

impl Entry {
    pub fn is_incomplete(&self) -> bool {
        self.state == "creating"
    }
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
    pub disconnect_tpl: String,
    pub compose: Option<ComposePlan>,
    /// Route name and `<name>.<task>.localhost:<port>`.
    pub routes: Vec<(String, String)>,
    pub services: Vec<ServicePlan>,
}

/// A service command rendered in memory. agentgc prints it and writes it to files; never runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServicePlan {
    pub name: String,
    pub cmd: String,
    pub cwd: String,
    pub port: Option<u16>,
    pub depends_on: Vec<String>,
    pub ready_timeout: u64,
}

const SERVICES_JSON: &str = ".agentgc/services.json";
const SERVICES_SH: &str = ".agentgc/services.sh";
const SERVICES_PS1: &str = ".agentgc/services.ps1";

fn services_files(services: &[ServicePlan]) -> Vec<String> {
    if services.is_empty() {
        Vec::new()
    } else {
        vec![SERVICES_JSON.into(), SERVICES_SH.into(), SERVICES_PS1.into()]
    }
}

/// Compose file already rendered in memory; written only by `execute`.
#[derive(Debug)]
pub struct ComposePlan {
    pub output: String,
    pub rendered: String,
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub(crate) fn tasks_dir(root: &Path) -> PathBuf {
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

pub(crate) fn valid_branch(b: &str) -> bool {
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
    // `{repo}` inside the db name uses `_` only: `-` is not safe in a db name.
    let mut db_vars = vars.clone();
    db_vars.insert("repo".into(), tc::sanitize_repo_db(root.file_name().and_then(|n| n.to_str()).unwrap_or("repo")));
    let db = tc::fill(&cfg.db.name, &db_vars)?;
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
    let mut routes: Vec<(String, String)> = Vec::new();
    for (r, port_name) in &cfg.routes {
        let port = ports
            .iter()
            .find(|(n, _)| n == port_name)
            .map(|(_, p)| *p)
            .ok_or_else(|| format!("routes.{r} points to unknown port {port_name:?}"))?;
        let host = compose::route_host(r, &name, port);
        tc::put(&mut env, &compose::route_env_key(r), format!("http://{host}"));
        routes.push((r.clone(), host));
    }
    // A service with a `port` adds a route named like the service, unless [routes] has that name.
    for (s, sc) in &cfg.services {
        let Some(port_name) = &sc.port else { continue };
        if routes.iter().any(|(r, _)| r == s) {
            continue;
        }
        let port = ports
            .iter()
            .find(|(n, _)| n == port_name)
            .map(|(_, p)| *p)
            .ok_or_else(|| format!("services.{s} points to unknown port {port_name:?}"))?;
        let host = compose::route_host(s, &name, port);
        tc::put(&mut env, &compose::route_env_key(s), format!("http://{host}"));
        routes.push((s.clone(), host));
    }
    let wt_path = root.join(&cfg.worktree.dir).join(&name);
    let compose_plan = match &cfg.compose {
        None => None,
        Some(cp) => {
            tc::put(&mut env, "COMPOSE_PROJECT_NAME", compose::project_name(&name));
            let tpl_path = root.join(&cp.template);
            if !tpl_path.is_file() {
                return Err(format!("compose template not found: {}", tpl_path.display()));
            }
            let tpl = fs::read_to_string(&tpl_path)
                .map_err(|e| format!("cannot read compose template {}: {e}", tpl_path.display()))?;
            let cv = placeholder_vars(&name, &db, &wt_path, &ports, &env);
            let rendered = compose::render(&tpl, &cv).map_err(|e| format!("{e} in the compose template"))?;
            Some(ComposePlan { output: cp.output.clone(), rendered })
        }
    };
    let mut services: Vec<ServicePlan> = Vec::new();
    if !cfg.services.is_empty() {
        let cv = placeholder_vars(&name, &db, &wt_path, &ports, &env);
        for (s, sc) in &cfg.services {
            let cmd = compose::render(&sc.cmd, &cv).map_err(|e| format!("services.{s}.cmd: {e}"))?;
            if cmd.trim().is_empty() || cmd.contains('\n') || cmd.contains('\r') {
                return Err(format!("services.{s}.cmd must render to one non-empty line"));
            }
            let port = sc.port.as_ref().and_then(|pn| ports.iter().find(|(n, _)| n == pn).map(|(_, p)| *p));
            services.push(ServicePlan {
                name: s.clone(),
                cmd,
                cwd: sc.cwd.clone(),
                port,
                depends_on: sc.depends_on.clone(),
                ready_timeout: sc.ready_timeout,
            });
        }
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
        wt_path,
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
        disconnect_tpl: cfg.db.disconnect.clone(),
        compose: compose_plan,
        routes,
        services,
    })
}

/// Values for `{{...}}` placeholders: task, db, worktree, port.NAME, env.NAME.
fn placeholder_vars(
    task: &str,
    db: &str,
    wt: &Path,
    ports: &[(String, u16)],
    env: &[(String, String)],
) -> BTreeMap<String, String> {
    let mut cv: BTreeMap<String, String> = BTreeMap::new();
    cv.insert("task".into(), task.to_string());
    cv.insert("db".into(), db.to_string());
    cv.insert("worktree".into(), wt.to_string_lossy().into_owned());
    for (n, p) in ports {
        cv.insert(format!("port.{n}"), p.to_string());
    }
    for (k, v) in env {
        cv.insert(format!("env.{k}"), v.clone());
    }
    cv
}

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<std::process::Output, String> {
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
    outln!("Plan for task {:?} ({})", p.name, config_note);
    outln!("  worktree: {}", p.wt_path.display());
    outln!("  branch:   {}  (from {})", p.branch, p.base);
    outln!("  command:  git worktree add -b {} {} {}", p.branch, p.wt_path.display(), p.base);
    outln!("  worktree dir: {}/ (task folders live here)", p.wt_dir);
    outln!("  copy files:");
    if p.copy.is_empty() {
        outln!("    (none listed)");
    }
    for (f, ex) in &p.copy {
        outln!("    {f}  {}", if *ex { "will copy" } else { "not found, skipped" });
    }
    if p.ports.is_empty() {
        outln!("  ports:    (none)");
    } else {
        let s: Vec<String> = p.ports.iter().map(|(n, v)| format!("{n}={v}")).collect();
        outln!("  ports:    {}", s.join("  "));
    }
    outln!("  db name:  {}  (agentgc never connects to a database)", p.db);
    outln!("  env file: {}", p.wt_path.join(".agentgc").join("task.env").display());
    for (k, v) in &p.env {
        outln!("    {k}={}", tc::mask_value(k, v));
    }
    if let Some(c) = &p.compose {
        outln!(
            "  compose:  {}  (rendered from the template; project {})",
            p.wt_path.join(&c.output).display(),
            compose::project_name(&p.name)
        );
    }
    for (r, h) in &p.routes {
        outln!("  route:    {r} -> {h}");
    }
    print_services(p);
    outln!(
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
        outln!("  commands: (none)");
    }
    for (kind, c) in cmds {
        if run_setup {
            outln!("  {kind}: {c}  (will run)");
        } else {
            outln!("  {kind}: {c}  (not run, use --run-setup)");
        }
    }
    if !p.disconnect_tpl.trim().is_empty() {
        outln!("  disconnect: stored for `task done --yes`, never run by `new`");
    }
    if !p.drop_tpl.trim().is_empty() {
        outln!("  drop: stored for `task done --yes`, never run by `new`");
    }
}

fn print_services(p: &Plan) {
    for s in &p.services {
        outln!("  service:  {}  (cwd: {})  [printed only, agentgc never starts it]", s.name, s.cwd);
        outln!("            {}", s.cmd);
        if !s.depends_on.is_empty() {
            outln!("            depends on: {} (ready timeout {} s)", s.depends_on.join(" "), s.ready_timeout);
        }
    }
}

pub(crate) fn branch_exists(root: &Path, branch: &str) -> bool {
    git(root, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn print_ignore_hints(p: &Plan) {
    let probe = format!("{}/{}/x", p.wt_dir.trim_end_matches('/'), p.name);
    if is_ignored(&p.root, &probe) == Some(false) {
        outln!("Hint: {}/ is not git-ignored; add it to .gitignore (agentgc does not edit it)", p.wt_dir);
    }
    if is_ignored(&p.root, ".agentgc/tasks/x.json") == Some(false) {
        outln!("Hint: .agentgc/ should be git-ignored; add it to .gitignore (agentgc does not edit it)");
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

/// Replace a file: write a temp file in the same folder, then rename it over.
fn write_replace(path: &Path, text: &str) -> Result<(), String> {
    use std::io::Write;
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    let mut f = fs::File::create(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    f.write_all(text.as_bytes()).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    f.sync_all().map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    drop(f);
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot replace {}: {e}", path.display())
    })
}

/// Run one command: whitespace split, no shell, cwd = dir. Err names the failure.
pub(crate) fn run_command(cmd: &str, dir: &Path) -> Result<(), String> {
    let parts = split_command(cmd);
    let Some((prog, args)) = parts.split_first() else { return Ok(()) };
    outln!("  running: {cmd}");
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

/// Execute the plan. Never deletes. The registry entry is written first as
/// `creating`; the one overwrite is the final rewrite as `ready`.
pub fn execute(p: &Plan, run_setup: bool) -> Result<(), String> {
    let mut entry = Entry {
        name: p.name.clone(),
        path: p.wt_path.to_string_lossy().into_owned(),
        branch: p.branch.clone(),
        base: p.base.clone(),
        ports: p.ports.iter().cloned().collect(),
        db: p.db.clone(),
        created_at: now_secs(),
        setup_done: false,
        drop: p.drop_tpl.clone(),
        disconnect: p.disconnect_tpl.clone(),
        state: "creating".into(),
        compose_file: p.compose.as_ref().map(|c| c.output.clone()).unwrap_or_default(),
        routes: p.routes.iter().cloned().collect(),
        generated_files: services_files(&p.services),
        services: p.services.iter().map(|s| s.name.clone()).collect(),
    };
    let reg = tasks_dir(&p.root).join(format!("{}.json", p.name));
    write_new(&reg, &serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())?)?;
    outln!("registered {} (state: creating)", reg.display());
    let hint = format!("task {:?} stays registered as incomplete; run `agentgc task done {}` to clean up", p.name, p.name);
    build(p, run_setup, &mut entry).map_err(|e| format!("{e}; {hint}"))?;
    entry.state = "ready".into();
    write_replace(&reg, &serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())?)
        .map_err(|e| format!("{e}; {hint}"))?;
    for (r, h) in &p.routes {
        outln!("route {r}: {h}");
    }
    outln!("task {} is ready", p.name);
    Ok(())
}

fn build(p: &Plan, run_setup: bool, entry: &mut Entry) -> Result<(), String> {
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
    outln!("created worktree {} on branch {}", p.wt_path.display(), p.branch);

    for (f, ex) in &p.copy {
        if !*ex {
            continue;
        }
        let dest = p.wt_path.join(f);
        if dest.exists() {
            outln!("  skip {f}: already exists in the worktree");
            continue;
        }
        if let Some(d) = dest.parent() {
            fs::create_dir_all(d).map_err(|e| format!("cannot create {}: {e}", d.display()))?;
        }
        fs::copy(p.root.join(f), &dest).map_err(|e| format!("cannot copy {f}: {e}"))?;
        outln!("  copied {f}");
    }

    if let Some(c) = &p.compose {
        let out_path = p.wt_path.join(&c.output);
        write_new(&out_path, &c.rendered)?;
        outln!("wrote {} (agentgc never starts containers)", out_path.display());
    }

    let env_path = p.wt_path.join(".agentgc").join("task.env");
    write_new(&env_path, &tc::render_env(&p.env)?)?;
    outln!("wrote {}", env_path.display());

    if !p.services.is_empty() {
        write_services(p)?;
        print_services(p);
    }

    if run_setup {
        let mut all: Vec<&String> = Vec::new();
        if let Some(c) = &p.create {
            all.push(c);
        }
        all.extend(p.setup.iter());
        for c in all {
            run_command(c, &p.wt_path)?;
        }
        entry.setup_done = true;
    } else {
        for c in p.create.iter().chain(p.setup.iter()) {
            outln!("  not run (use --run-setup): {c}");
        }
    }
    Ok(())
}

/// Write services.json, services.sh and services.ps1 with `create_new`. Never runs a command.
fn write_services(p: &Plan) -> Result<(), String> {
    let json: Vec<serde_json::Value> = p
        .services
        .iter()
        .map(|s| serde_json::json!({
                "name": s.name, "cmd": s.cmd, "cwd": s.cwd, "port": s.port,
                "depends_on": s.depends_on, "ready_timeout": s.ready_timeout
            }))
        .collect();
    let sh: String = p.services.iter().map(|s| format!("{}\n", compose::sh_line(&s.cwd, &s.cmd))).collect();
    let ps: String = p.services.iter().map(|s| format!("{}\n", compose::ps_line(&s.cwd, &s.cmd))).collect();
    let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?;
    for (rel, body) in [(SERVICES_JSON, text), (SERVICES_SH, sh), (SERVICES_PS1, ps)] {
        let path = p.wt_path.join(rel);
        write_new(&path, &body)?;
        outln!("wrote {} (agentgc never starts services)", path.display());
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
    if branch_exists(&root, &plan.branch) {
        return Err(format!("branch {:?} already exists; pick another name or branch template", plan.branch));
    }
    print_plan(&plan, a.run_setup, &note);
    if !a.yes {
        outln!("Dry run: nothing changed. Add --yes to execute.");
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
    /// Uncommitted files (lines of `git status --porcelain`). `null`: folder missing or git failed.
    pub changed: Option<u64>,
    /// Commits on the task branch that the base branch lacks. `null`: folder missing or git failed.
    pub ahead: Option<u64>,
    /// Commits on the base branch that the task branch lacks. `null`: folder missing or git failed.
    pub behind: Option<u64>,
    /// Services with a live recorded process (pid + start time + name), from `.agentgc/daemon.json`.
    pub services_running: u64,
    /// Services declared for the task.
    pub services_total: u64,
}

impl Row {
    fn missing(&self) -> bool {
        self.status.iter().any(|s| s == "missing")
    }
}

/// Read-only probe of one worktree: status words and the number of uncommitted files.
/// `--no-optional-locks` keeps `git status` from refreshing the index.
fn status_of(path: &Path) -> (Vec<String>, Option<u64>) {
    if !path.is_dir() || !path.join(".git").exists() {
        return (vec!["missing".into()], None);
    }
    match git(path, &["--no-optional-locks", "status", "--porcelain"]) {
        Ok(o) if o.status.success() => {
            let n = String::from_utf8_lossy(&o.stdout).lines().filter(|l| !l.trim().is_empty()).count() as u64;
            if n > 0 {
                (vec!["ok".into(), "dirty".into()], Some(n))
            } else {
                (vec!["ok".into()], Some(0))
            }
        }
        _ => (vec!["ok".into(), "unknown".into()], None),
    }
}

/// `(ahead, behind)` of the task branch against its base, from `git rev-list --left-right --count`.
fn ahead_behind(path: &Path, branch: &str, base: &str) -> Option<(u64, u64)> {
    let range = format!("refs/heads/{branch}...{base}");
    let o = git(path, &["rev-list", "--left-right", "--count", &range, "--"]).ok()?;
    if !o.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&o.stdout);
    let mut it = text.split_whitespace();
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    Some((a, b))
}

pub fn collect_rows(root: &Path) -> Vec<Row> {
    let now = now_secs();
    read_registry(root)
        .into_iter()
        .map(|e| {
            let path = Path::new(&e.path);
            let (status, changed) = status_of(path);
            let ab = if status.iter().any(|s| s == "missing") { None } else { ahead_behind(path, &e.branch, &e.base) };
            let age_days = now.saturating_sub(e.created_at) / 86400;
            let (running, total) = if status.iter().any(|s| s == "missing") {
                (0, e.services.len())
            } else {
                crate::sup_cmd::service_counts(path, e.services.len())
            };
            Row {
                entry: e,
                age_days,
                status,
                changed,
                ahead: ab.map(|x| x.0),
                behind: ab.map(|x| x.1),
                services_running: running as u64,
                services_total: total as u64,
            }
        })
        .collect()
}

fn cell(missing: bool, v: Option<u64>) -> String {
    match (missing, v) {
        (true, _) => "-".into(),
        (false, Some(n)) => n.to_string(),
        (false, None) => "?".into(),
    }
}

/// The `state:` line of `task list`: CHANGED, AHEAD/BEHIND, SERVICES.
fn state_line(r: &Row) -> String {
    let m = r.missing();
    let ab = if m {
        "-".to_string()
    } else {
        match (r.ahead, r.behind) {
            (Some(a), Some(b)) => format!("{a}/{b}"),
            _ => "?".into(),
        }
    };
    let svc = if m || r.services_total == 0 {
        "-".to_string()
    } else {
        format!("{}/{}", r.services_running, r.services_total)
    };
    format!("CHANGED {}  AHEAD/BEHIND {}  SERVICES {}", cell(m, r.changed), ab, svc)
}

pub fn cmd_list(repo: Option<PathBuf>, json: bool) -> Result<(), String> {
    let root = find_repo_root(repo.as_deref().unwrap_or(Path::new(".")))?;
    let rows = collect_rows(&root);
    if json {
        let v = serde_json::json!({ "tasks": rows });
        outln!("{}", serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?);
        return Ok(());
    }
    if rows.is_empty() {
        outln!("No tasks registered.");
        return Ok(());
    }
    for r in &rows {
        let e = &r.entry;
        let mut status = r.status.clone();
        if e.is_incomplete() {
            status.insert(0, "incomplete".into());
        }
        let ports: Vec<String> = e.ports.iter().map(|(n, p)| format!("{n}={p}")).collect();
        let routes: Vec<String> = e.routes.iter().map(|(n, h)| format!("{n}={h}")).collect();
        outln!(
            "{}  [{}]  {}d\n    path:   {}\n    branch: {}\n    ports:  {}\n    db:     {}\n    routes: {}\n    services: {}
    state:  {}",
            e.name,
            status.join(","),
            r.age_days,
            e.path,
            e.branch,
            if ports.is_empty() { "-".into() } else { ports.join(" ") },
            e.db,
            if routes.is_empty() { "-".into() } else { routes.join(" ") },
            if e.services.is_empty() { "-".into() } else { e.services.join(" ") },
            state_line(r)
        );
    }
    outln!("{} tasks. Nothing was changed.", rows.len());
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
    fn db_name_uses_underscore_for_repo_only() {
        let c = cfg("[env]\nR=\"{repo}\"\n");
        let p = build_plan(&c, "d", Path::new("/tmp/my-repo"), &HashSet::new(), &|_| true).unwrap();
        assert_eq!(p.db, "my_repo_d");
        assert!(tc::render_env(&p.env).unwrap().contains("R=my-repo\n"));
    }

    #[test]
    fn plan_rejects_bad_input() {
        let c = Config::default();
        let root = PathBuf::from("/tmp/r");
        assert!(build_plan(&c, "Bad Name", &root, &HashSet::new(), &|_| true).is_err());
        assert!(tc::parse_config("[worktree]\nbranch=\"--evil\"").is_err());
        let c = cfg("[worktree]\nbranch=\"a;b\"");
        assert!(build_plan(&c, "ok", &root, &HashSet::new(), &|_| true).is_err());
        let c = cfg("[env]\nA=\"{nope}\"");
        assert!(build_plan(&c, "ok", &root, &HashSet::new(), &|_| true).is_err());
    }

    #[test]
    fn old_registry_entry_loads() {
        let old = r#"{"name":"a","path":"/x","branch":"a","base":"main","ports":{},"db":"d","created_at":1,"setup_done":false,"drop":""}"#;
        let e: Entry = serde_json::from_str(old).unwrap();
        assert_eq!(e.compose_file, "");
        assert!(e.routes.is_empty());
        assert_eq!(e.state, "ready");
        assert!(e.generated_files.is_empty() && e.services.is_empty());
    }

    #[test]
    fn plan_renders_services_and_routes() {
        let c = cfg(
            "[ports]\nnames=[\"web\",\"api\"]\n[routes]\napi=\"api\"\n[services.web]\ncmd=\"npm run dev -- --port {{port.web}} --task {{task}}\"\nport=\"web\"\n[services.api]\ncmd=\"cargo run\"\ncwd=\"apps/api\"\nport=\"api\"\n",
        );
        let p = build_plan(&c, "demo", Path::new("/tmp/r"), &HashSet::new(), &|_| true).unwrap();
        assert_eq!(p.services.len(), 2);
        let api = &p.services[0];
        assert_eq!((api.name.as_str(), api.cwd.as_str(), api.port), ("api", "apps/api", Some(3101)));
        let web = &p.services[1];
        assert_eq!(web.cmd, "npm run dev -- --port 3100 --task demo");
        assert_eq!(web.cwd, ".");
        // the service route is added; [routes] keeps its own `api` entry
        assert!(p.routes.contains(&("web".to_string(), "web.demo.localhost:3100".to_string())));
        assert_eq!(p.routes.iter().filter(|(r, _)| r == "api").count(), 1);
        assert!(tc::render_env(&p.env).unwrap().contains("AGENTGC_ROUTE_WEB=http://web.demo.localhost:3100\n"));
        assert_eq!(services_files(&p.services).len(), 3);
        assert!(services_files(&[]).is_empty());
    }

    #[test]
    fn service_unknown_placeholder_names_it() {
        let c = cfg("[services.web]\ncmd=\"run {{port.nope}}\"\n");
        let e = build_plan(&c, "demo", Path::new("/tmp/r"), &HashSet::new(), &|_| true).unwrap_err();
        assert!(e.contains("{{port.nope}}") && e.contains("services.web"), "{e}");
        let c = cfg("[services.web]\ncmd=\"{{env.TASK_ID}} {{db}} {{worktree}}\"\n");
        let p = build_plan(&c, "demo", Path::new("/tmp/r"), &HashSet::new(), &|_| true).unwrap();
        assert!(p.services[0].cmd.starts_with("demo r_demo "), "{:?}", p.services[0].cmd);
    }

    #[test]
    fn plan_renders_compose_and_routes() {
        let base = tmp("compose-plan");
        fs::write(base.join("t.tpl"), "n: {{task}}\nw: {{port.web}}\nd: {{db}}\ne: {{env.COMPOSE_PROJECT_NAME}}\n").unwrap();
        let c = cfg("[ports]\nnames=[\"web\"]\n[compose]\ntemplate=\"t.tpl\"\noutput=\"dc.yml\"\n[routes]\nweb=\"web\"\n");
        let p = build_plan(&c, "demo", &base, &HashSet::new(), &|_| true).unwrap();
        let cp = p.compose.as_ref().unwrap();
        assert!(cp.rendered.starts_with("n: demo\nw: 3100\nd: "));
        assert!(cp.rendered.contains("e: agentgc-demo\n"));
        assert_eq!(p.routes, vec![("web".to_string(), "web.demo.localhost:3100".to_string())]);
        let env = tc::render_env(&p.env).unwrap();
        assert!(env.contains("COMPOSE_PROJECT_NAME=agentgc-demo\n"));
        assert!(env.contains("AGENTGC_ROUTE_WEB=http://web.demo.localhost:3100\n"));
        // unknown placeholder: error names it
        fs::write(base.join("t.tpl"), "{{port.nope}}").unwrap();
        let e = build_plan(&c, "demo", &base, &HashSet::new(), &|_| true).unwrap_err();
        assert!(e.contains("{{port.nope}}"), "{e}");
        // missing template
        fs::remove_file(base.join("t.tpl")).unwrap();
        let e = build_plan(&c, "demo", &base, &HashSet::new(), &|_| true).unwrap_err();
        assert!(e.contains("template not found"), "{e}");
        let _ = fs::remove_dir_all(&base);
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
