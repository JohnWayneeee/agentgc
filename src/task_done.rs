//! `agentgc task done`: the only command that deletes things. Safety first:
//! read-only checks, dry run by default, refuse when in doubt.
use crate::out::{self, outln};
use crate::proc_kill::{wait_gone, Killer, SysKiller};
use crate::ps::{self, KindRule, ProcInfo};
use crate::scan;
use crate::sup;
use crate::sup_cmd;
use crate::task::{self, Entry};
use crate::task_config as tc;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Component, Path, PathBuf};

pub struct DoneArgs {
    pub name: String,
    pub repo: Option<PathBuf>,
    pub config: Option<PathBuf>,
    pub yes: bool,
    pub kill_procs: bool,
    pub force: bool,
    /// Stop the supervisor and its services first (like `agentgc down`).
    pub stop_services: bool,
}

/// Kinds `done --kill-procs` may stop. `agent`, `db` and `other` never.
const STOPPABLE: [&str; 4] = ["dev-server", "lsp", "test", "build"];
const SHOW_FILES: usize = 10;

pub fn is_stoppable(kind: &str) -> bool {
    STOPPABLE.contains(&kind)
}

/// A process whose cwd is inside the worktree.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub pid: u32,
    pub name: String,
    pub kind: String,
    pub cmd: String,
    pub stoppable: bool,
}

/// True when `cwd` is the worktree or below it.
pub fn inside(cwd: &Path, wt: &Path) -> bool {
    ps::path_under(&ps::norm(cwd), &ps::norm(wt))
}

/// Processes with a readable cwd inside `wt`, never `self_pid`. Pure: no system calls.
pub fn select_procs(procs: &[ProcInfo], rules: &[KindRule], wt: &Path, self_pid: u32) -> Vec<Found> {
    let mut v: Vec<Found> = procs
        .iter()
        .filter(|p| p.pid != self_pid)
        .filter(|p| p.cwd.as_deref().map_or(false, |c| inside(c, wt)))
        .map(|p| {
            let kind = ps::classify(rules, &p.name, &p.cmd);
            Found {
                pid: p.pid,
                name: p.name.clone(),
                stoppable: is_stoppable(&kind),
                kind,
                cmd: ps::shorten(&ps::sanitize_cmd(&p.cmd), 80),
            }
        })
        .collect();
    v.sort_by_key(|f| f.pid);
    v
}

#[derive(Debug)]
pub struct Target {
    pub path: PathBuf,
    pub exists: bool,
}

/// PATH GUARD. The task path must be a direct child of `<root>/<wt_dir>`, named like the task.
/// Hard error, `--force` never skips it.
pub fn guard(root: &Path, wt_dir: &str, name: &str, entry_path: &str) -> Result<Target, String> {
    let p = Path::new(entry_path);
    let refuse = |why: &str| {
        format!("PATH GUARD: refusing {entry_path:?}: {why}. Nothing was touched; fix or delete the registry entry by hand")
    };
    if !p.is_absolute() {
        return Err(refuse("the path is not absolute"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err(refuse("the path has '.' or '..' parts"));
    }
    let dir = root.join(wt_dir);
    match fs::symlink_metadata(p) {
        Ok(md) => {
            if md.file_type().is_symlink() {
                return Err(refuse("it is a symlink or junction"));
            }
            let canon = fs::canonicalize(p).map_err(|e| refuse(&format!("cannot resolve it: {e}")))?;
            let dir_c = fs::canonicalize(&dir)
                .map_err(|e| refuse(&format!("cannot resolve the worktree dir {}: {e}", dir.display())))?;
            if canon == dir_c || canon.parent() != Some(dir_c.as_path()) {
                return Err(refuse(&format!("it is not a task folder directly inside {}", dir_c.display())));
            }
            if canon.file_name().and_then(|n| n.to_str()) != Some(name) {
                return Err(refuse("the folder name does not match the task name"));
            }
            Ok(Target { path: canon, exists: true })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if ps::norm(p) != ps::norm(&dir.join(name)) {
                return Err(refuse(&format!("it is not <{}>/{name}", dir.display())));
            }
            Ok(Target { path: p.to_path_buf(), exists: false })
        }
        Err(e) => Err(refuse(&format!("cannot read it: {e}"))),
    }
}

/// Drop the Windows `\\?\` prefix so git gets a plain path.
fn plain(p: &Path) -> String {
    let s = p.to_string_lossy().into_owned();
    match s.strip_prefix(r"\\?\") {
        Some(r) if r.as_bytes().get(1) == Some(&b':') => r.to_string(),
        _ => s,
    }
}

/// True when `git worktree list --porcelain` lists `path`.
fn worktree_listed(root: &Path, path: &Path) -> Result<bool, String> {
    let o = task::git(root, &["worktree", "list", "--porcelain"])?;
    if !o.status.success() {
        return Err(format!("cannot list git worktrees: {}", stderr_of(&o)));
    }
    let want = ps::norm(Path::new(&plain(path)));
    Ok(String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix("worktree "))
        .any(|p| ps::norm(Path::new(p.trim())) == want))
}

/// True when the folder has no entries at all, hidden ones included.
fn dir_is_empty(path: &Path) -> Result<bool, String> {
    let mut rd = fs::read_dir(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    Ok(rd.next().is_none())
}

fn stderr_of(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).trim().to_string()
}

/// Changed files, ignoring `.agentgc/`.
/// An untracked entry equal to `ignore` (the rendered compose file) is skipped too.
fn dirty_files(wt: &Path, ignore: Option<&str>) -> Result<Vec<String>, String> {
    let o = task::git(wt, &["status", "--porcelain"])?;
    if !o.status.success() {
        return Err(stderr_of(&o));
    }
    let mut v = Vec::new();
    for l in String::from_utf8_lossy(&o.stdout).lines() {
        if l.len() < 4 {
            continue;
        }
        let path = &l[3..];
        let last = path.rsplit(" -> ").next().unwrap_or(path).trim_matches('"');
        if last == ".agentgc" || last.starts_with(".agentgc/") {
            continue;
        }
        if let Some(ig) = ignore {
            if l.starts_with("??") && last.replace('\\', "/") == ig {
                continue;
            }
        }
        v.push(path.to_string());
    }
    Ok(v)
}

/// Commits on `branch` that are neither in `base` nor in any remote branch.
fn unsaved_commits(dir: &Path, branch: &str, base: &str) -> Result<u64, String> {
    let r = format!("refs/heads/{branch}");
    let o = task::git(dir, &["rev-list", "--count", &r, "--not", base, "--remotes"])?;
    if !o.status.success() {
        return Err(stderr_of(&o));
    }
    String::from_utf8_lossy(&o.stdout)
        .trim()
        .parse::<u64>()
        .map_err(|e| format!("bad rev-list output: {e}"))
}

/// Stop processes. Re-checks the cwd right before the signal. Waits up to 3 s.
/// Returns the pids still alive.
fn kill_procs(pids: &[u32], wt: &Path) -> Vec<u32> {
    kill_procs_with(&SysKiller, pids, wt)
}

fn kill_procs_with(k: &dyn Killer, pids: &[u32], wt: &Path) -> Vec<u32> {
    let self_pid = std::process::id();
    let ids: Vec<u32> = pids.iter().copied().filter(|p| *p != self_pid).collect();
    for id in &ids {
        if let Some(live) = k.read(*id) {
            if live.cwd.as_deref().map_or(false, |c| inside(c, wt)) {
                let _ = k.signal(*id, true);
            }
        }
    }
    wait_gone(k, &ids, 3000)
}

fn tag(ok: bool) -> &'static str {
    if ok {
        "[ok]     "
    } else {
        "[BLOCKER]"
    }
}

fn compose_hint(name: &str) -> String {
    format!("docker compose -p {} down -v", crate::compose::project_name(name))
}

pub fn cmd_done(a: &DoneArgs) -> Result<(), String> {
    let name = tc::sanitize_name(&a.name)?;
    let root = task::find_repo_root(a.repo.as_deref().unwrap_or(Path::new(".")))?;
    let (cfg, note) = task::load_config(&root, a.config.as_deref())?;
    let reg = task::tasks_dir(&root).join(format!("{name}.json"));
    let text = fs::read_to_string(&reg)
        .map_err(|e| format!("task {name:?} is not in the registry ({}): {e}", reg.display()))?;
    let entry: Entry =
        serde_json::from_str(&text).map_err(|e| format!("bad registry entry {}: {e}", reg.display()))?;
    if entry.name != name {
        return Err(format!("registry entry {} holds the name {:?}; refusing", reg.display(), entry.name));
    }
    if !task::valid_branch(&entry.branch) {
        return Err(format!("registry branch {:?} is not allowed; refusing", entry.branch));
    }
    if tc::bad_ref(&entry.base) {
        return Err(format!("registry base {:?} is not allowed; refusing", entry.base));
    }
    let mut t = guard(&root, &cfg.worktree.dir, &name, &entry.path)?;
    // Resume state: git already dropped the worktree, only an empty folder is left.
    let mut resume_dir = false;
    if t.exists && !t.path.join(".git").is_file() {
        let refuse = |why: &str| {
            format!(
                "{} has no `.git` file (not a linked git worktree){why}; refusing to touch it",
                t.path.display()
            )
        };
        if worktree_listed(&root, &t.path)? {
            return Err(refuse(", and git still lists it (run `git worktree prune` if the folder is stale)"));
        }
        if !dir_is_empty(&t.path)? {
            return Err(refuse(", and the folder is not empty"));
        }
        resume_dir = true;
        // Downstream checks must not run git inside this folder (it would see the parent repo).
        t.exists = false;
    }

    // Rendered compose file recorded by `new`. Only a safe relative path counts.
    let compose_rel: Option<String> = if entry.compose_file.is_empty() {
        None
    } else if tc::safe_rel(&entry.compose_file) {
        Some(entry.compose_file.replace('\\', "/").trim_start_matches("./").to_string())
    } else {
        return Err(format!("registry compose_file {:?} is not a safe relative path; refusing", entry.compose_file));
    };

    // Generated files recorded by `new` (services files). Same rule: safe relative paths only.
    let mut generated: Vec<String> = Vec::new();
    for g in &entry.generated_files {
        if !tc::safe_rel(g) {
            return Err(format!("registry generated_files entry {g:?} is not a safe relative path; refusing"));
        }
        generated.push(g.replace('\\', "/").trim_start_matches("./").to_string());
    }

    let size = if t.exists { scan::dir_size(&t.path) } else { 0 };
    let branch_exists = task::branch_exists(&root, &entry.branch);
    outln!("Finish task {name:?} (state: {}, {note})", entry.state);
    if resume_dir {
        outln!("RESUME: a partly finished task. Git no longer lists the worktree and its folder is empty.");
        outln!("        Only the remaining steps run: remove the empty folder, delete the branch, delete the registry file.");
    }
    outln!("  repo:     {}", root.display());
    outln!(
        "  worktree: {}  ({})",
        plain(&t.path),
        if t.exists {
            "exists"
        } else if resume_dir {
            "empty folder left over"
        } else {
            "folder already gone"
        }
    );
    outln!(
        "  branch:   {}  (base {}; {})",
        entry.branch,
        entry.base,
        if branch_exists { "exists" } else { "not found" }
    );
    outln!("  size:     about {}", scan::human(size));
    outln!("Checks:");
    outln!(
        "  {} registry entry found, path guard passed (task folder is directly inside {}/)",
        tag(true),
        cfg.worktree.dir
    );

    // blockers: (check text, what is lost with --force)
    let mut blockers: Vec<(String, String)> = Vec::new();
    let mut found: Vec<Found> = Vec::new();
    if t.exists {
        match dirty_files(&t.path, compose_rel.as_deref()) {
            Ok(f) if f.is_empty() => outln!("  {} no uncommitted work", tag(true)),
            Ok(f) => {
                let mut shown: Vec<String> = f.iter().take(SHOW_FILES).cloned().collect();
                if f.len() > SHOW_FILES {
                    shown.push(format!("+{} more", f.len() - SHOW_FILES));
                }
                outln!("  {} uncommitted work: {} entries", tag(false), f.len());
                for s in &shown {
                    outln!("              {s}");
                }
                blockers.push((
                    format!("{} uncommitted entries", f.len()),
                    format!("{} uncommitted files ({})", f.len(), shown.join(", ")),
                ));
            }
            Err(e) => {
                outln!("  {} cannot read git status: {e}", tag(false));
                blockers.push(("git status failed".into(), format!("unchecked changes (git status failed: {e})")));
            }
        }
    } else {
        outln!("  {} no worktree folder, so no uncommitted work", tag(true));
    }
    if branch_exists {
        let dir = if t.exists { t.path.as_path() } else { root.as_path() };
        match unsaved_commits(dir, &entry.branch, &entry.base) {
            Ok(0) => outln!("  {} every commit is merged into {} or pushed", tag(true), entry.base),
            Ok(n) => {
                let m = format!("{n} commits are neither merged into {} nor pushed", entry.base);
                outln!("  {} {m}", tag(false));
                blockers.push((m.clone(), m));
            }
            Err(e) => {
                outln!("  {} cannot count unsaved commits: {e}", tag(false));
                blockers.push((
                    "commit check failed".into(),
                    format!("possibly unsaved commits on {} (check failed: {e})", entry.branch),
                ));
            }
        }
    } else {
        outln!("  {} branch not found, no commits to lose", tag(true));
    }

    let rules = ps::parse_proc_rules(ps::DEFAULT_PROC_RULES)?;
    if t.exists {
        found = select_procs(&ps::read_system(), &rules, &t.path, std::process::id());
    }
    outln!("Processes with their cwd inside the worktree: {}", found.len());
    for f in &found {
        let what = if f.stoppable { "can stop" } else { "left running" };
        outln!("    {:>7}  {:<10}  {:<12}  {}  {}", f.pid, f.kind, what, f.name, f.cmd);
    }
    if found.iter().any(|f| !f.stoppable) {
        outln!("  Left running: agent, db and other processes are never stopped. On Windows the folder may stay locked.");
    }
    outln!("  (processes whose cwd cannot be read are not checked)");

    // supervisor of the task's services: refuse while it runs, unless --stop-services
    let sup_alive = t.exists && sup_cmd::supervisor_alive(&t.path);
    let mut sup_blocked = false;
    if sup_alive {
        let pid = sup::read_state(&t.path).map(|s| s.pid.to_string()).unwrap_or_else(|| "?".into());
        if a.stop_services {
            outln!("  [note]    the services supervisor runs (pid {pid}); --stop-services stops it first");
        } else {
            outln!("  {} the services supervisor runs (pid {pid}): use --stop-services or `agentgc down`", tag(false));
            sup_blocked = true;
        }
    }

    // drop command
    let mut vars: BTreeMap<String, String> = BTreeMap::new();
    vars.insert("name".into(), name.clone());
    vars.insert("db".into(), entry.db.clone());
    vars.insert("repo".into(), tc::sanitize_repo_db(root.file_name().and_then(|n| n.to_str()).unwrap_or("repo")));
    let mut drop_cmd: Option<String> = None;
    if !entry.drop.trim().is_empty() {
        match tc::fill(&entry.drop, &vars) {
            Ok(c) => drop_cmd = Some(c),
            Err(e) => {
                outln!("  {} the drop template does not fill: {e}", tag(false));
                blockers.push(("drop template does not fill".into(), "the db drop step (skipped)".into()));
            }
        }
    }

    let mut disconnect_cmd: Option<String> = None;
    if !entry.disconnect.trim().is_empty() {
        match tc::fill(&entry.disconnect, &vars) {
            Ok(c) => disconnect_cmd = Some(c),
            Err(e) => {
                outln!("  {} the disconnect template does not fill: {e}", tag(false));
                blockers.push(("disconnect template does not fill".into(), "the db disconnect step (skipped)".into()));
            }
        }
    }

    let stoppable: Vec<&Found> = found.iter().filter(|f| f.stoppable).collect();
    outln!("Steps with --yes:");
    if sup_alive && a.stop_services {
        outln!("  0. stop the supervisor and its services (like `agentgc down`{})", if a.force { " --force" } else { "" });
    }
    if a.kill_procs && !stoppable.is_empty() {
        outln!("  a. stop {} process(es) (dev-server, lsp, test, build only)", stoppable.len());
    } else if !stoppable.is_empty() {
        outln!("  a. not stopped (use --kill-procs)");
    } else {
        outln!("  a. no process to stop");
    }
    if let Some(c) = &disconnect_cmd {
        outln!("  b. run disconnect command: {c}");
    }
    match &drop_cmd {
        Some(c) => outln!("  b. run drop command: {c}"),
        None => outln!("  b. no drop command"),
    }
    outln!("  c. delete {}/.agentgc/task.env, then .agentgc if empty", plain(&t.path));
    outln!("     delete the supervisor files if present (daemon.json, daemon.lock, daemon.stop, restart.*, logs/*.log, logs/*.log.1), then the empty logs folder");
    if let Some(c) = &compose_rel {
        outln!("     delete the rendered compose file {c} (one file, only if untracked and inside the worktree)");
        outln!("     hint (never run by agentgc): {}", compose_hint(&name));
    }
    for g in &generated {
        outln!("     delete the generated file {g} (only if untracked, a plain file, inside the worktree)");
    }
    outln!(
        "  d. {}",
        if t.exists {
            format!("git worktree remove{} {}", if a.force { " --force" } else { "" }, plain(&t.path))
        } else if resume_dir {
            format!("git already removed the worktree; remove the empty folder {} (remove_dir)", plain(&t.path))
        } else {
            "worktree folder is gone: nothing to remove".into()
        }
    );
    outln!(
        "  e. {}",
        if branch_exists {
            format!("git branch {} {}", if a.force { "-D" } else { "-d" }, entry.branch)
        } else {
            "no branch to delete".into()
        }
    );
    outln!("  f. delete registry file {}", reg.display());
    outln!("  Never touched: anything outside the task folder, the main checkout, other tasks, remote branches.");

    if sup_blocked {
        outln!("BLOCKED: the supervisor runs. --force does not skip this; use --stop-services.");
    }
    if !blockers.is_empty() {
        if a.force {
            let lost: Vec<String> = blockers.iter().map(|b| b.1.clone()).collect();
            outln!("FORCE: this will discard: {}", lost.join("; "));
        } else {
            let list: Vec<String> = blockers.iter().map(|b| b.0.clone()).collect();
            outln!("BLOCKED: {}. Fix this, or use --force to discard it.", list.join("; "));
        }
    }
    if !a.yes {
        outln!("Dry run: nothing changed. Add --yes to execute.");
        return Ok(());
    }
    if sup_blocked {
        return Err("the services supervisor runs; use --stop-services (or `agentgc down`); nothing was changed".into());
    }
    if !blockers.is_empty() && !a.force {
        return Err("blocked by the checks above; nothing was changed".into());
    }

    out::set_critical(true);
    let mut done: Vec<String> = Vec::new();
    let res = execute(a, &root, &entry, &t, resume_dir, &found, disconnect_cmd.as_deref(), drop_cmd.as_deref(), branch_exists, &reg, compose_rel.as_deref(), &generated, &mut done);
    if let Err(e) = &res {
        outln!("Stopped at the first failure. Already done:");
        if done.is_empty() {
            outln!("  (nothing)");
        }
        for d in &done {
            outln!("  - {d}");
        }
        outln!(
            "To resume, run: agentgc task done {name} --yes{}",
            if a.force { " --force" } else { "" }
        );
        return Err(e.clone());
    }
    if compose_rel.is_some() {
        outln!("Containers are not stopped by agentgc. If you started them, run: {}", compose_hint(&name));
    }
    outln!("Task {name:?} finished; freed about {}", scan::human(size));
    Ok(())
}

/// Remove one file recorded by `new` (compose file, services files). Never a folder, never a
/// symlink, never a tracked file, never a path that resolves outside the worktree.
fn remove_recorded_file(wt: &Path, rel: &str, done: &mut Vec<String>) -> Result<(), String> {
    let p = wt.join(rel);
    match fs::symlink_metadata(&p) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            outln!("  kept {}: not a plain file", plain(&p));
            return Ok(());
        }
        Err(_) => return Ok(()),
    }
    let inside_wt = match (fs::canonicalize(&p), fs::canonicalize(wt)) {
        (Ok(c), Ok(w)) => c.starts_with(&w),
        _ => false,
    };
    if !inside_wt {
        outln!("  kept {}: not inside the worktree", plain(&p));
        return Ok(());
    }
    let tracked = task::git(wt, &["ls-files", "--error-unmatch", "--", rel])
        .map(|o| o.status.success())
        .unwrap_or(true);
    if tracked {
        outln!("  kept {}: tracked by git", plain(&p));
        return Ok(());
    }
    fs::remove_file(&p).map_err(|e| format!("cannot delete {}: {e}", p.display()))?;
    done.push(format!("deleted {}", plain(&p)));
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn execute(
    a: &DoneArgs,
    root: &Path,
    entry: &Entry,
    t: &Target,
    resume_dir: bool,
    found: &[Found],
    disconnect_cmd: Option<&str>,
    drop_cmd: Option<&str>,
    branch_exists: bool,
    reg: &Path,
    compose_rel: Option<&str>,
    generated: &[String],
    done: &mut Vec<String>,
) -> Result<(), String> {
    // 0. the supervisor and its services, like `agentgc down`
    if a.stop_services && t.exists && sup_cmd::supervisor_alive(&t.path) {
        sup_cmd::stop_supervisor(&t.path, a.force, &SysKiller)
            .map_err(|e| format!("cannot stop the supervisor, nothing was changed: {e}"))?;
        if sup_cmd::supervisor_alive(&t.path) {
            return Err("the supervisor is still running; nothing was changed".into());
        }
        done.push("stopped the supervisor and its services".into());
    }

    // a. processes
    let stoppable: Vec<&Found> = found.iter().filter(|f| f.stoppable).collect();
    let mut left: Vec<&Found> = found.iter().filter(|f| !f.stoppable).collect();
    if !stoppable.is_empty() {
        if a.kill_procs {
            let pids: Vec<u32> = stoppable.iter().map(|f| f.pid).collect();
            let alive = kill_procs(&pids, &t.path);
            for f in &stoppable {
                if alive.contains(&f.pid) {
                    outln!("  still running after 3 s: pid {} {}", f.pid, f.name);
                    left.push(f);
                } else {
                    outln!("  stopped pid {} {}", f.pid, f.name);
                    done.push(format!("stopped pid {} {}", f.pid, f.name));
                }
            }
        } else {
            for f in &stoppable {
                outln!("  not stopped (use --kill-procs): pid {} {}", f.pid, f.name);
            }
        }
    }
    if !left.is_empty() {
        outln!("  {} process(es) left running; the folder may stay locked on Windows", left.len());
    }

    // b. disconnect, then drop
    if let Some(c) = disconnect_cmd {
        match task::run_command(c, root) {
            Ok(()) => done.push(format!("ran disconnect command: {c}")),
            Err(e) if a.force => outln!("  FORCE: disconnect failed, continuing: {e}"),
            Err(e) => return Err(format!("disconnect failed, nothing else was changed: {e}")),
        }
    }
    if let Some(c) = drop_cmd {
        match task::run_command(c, root) {
            Ok(()) => done.push(format!("ran drop command: {c}")),
            Err(e) if a.force => outln!("  FORCE: drop failed, continuing: {e}"),
            Err(e) => return Err(format!("drop failed, nothing else was changed: {e}")),
        }
    }

    // c. own files only. No recursive delete anywhere.
    if t.exists {
        if let Some(rel) = compose_rel {
            remove_recorded_file(&t.path, rel, done)?;
        }
        for rel in generated {
            remove_recorded_file(&t.path, rel, done)?;
        }
        let ad = t.path.join(".agentgc");
        if fs::symlink_metadata(&ad).map_or(false, |m| m.is_dir()) {
            for p in sup::remove_daemon_files(&ad)? {
                done.push(format!("deleted {}", plain(&p)));
            }
            let env = ad.join("task.env");
            if fs::symlink_metadata(&env).map_or(false, |m| m.is_file()) {
                fs::remove_file(&env).map_err(|e| format!("cannot delete {}: {e}", env.display()))?;
                done.push(format!("deleted {}", plain(&env)));
            }
            match fs::remove_dir(&ad) {
                Ok(()) => done.push(format!("deleted {}", plain(&ad))),
                Err(_) => outln!("  kept {} (not empty)", plain(&ad)),
            }
        }

        // d. git removes the worktree
        let wt = plain(&t.path);
        let mut args = vec!["worktree", "remove"];
        if a.force {
            args.push("--force");
        }
        args.push(&wt);
        let o = task::git(root, &args)?;
        if !o.status.success() {
            return Err(format!(
                "step d (git worktree remove) failed: {}. A program may hold the folder (a shell with its cwd inside, an editor): close it first",
                stderr_of(&o)
            ));
        }
        done.push(format!("git worktree remove {wt}"));
        if t.path.exists() {
            outln!("  note: git left the folder {wt}; agentgc does not delete it by other means");
        }
    } else if resume_dir {
        // Only an empty folder is left. `remove_dir` fails on a non-empty folder; never recursive.
        fs::remove_dir(&t.path).map_err(|e| {
            format!(
                "step d (remove the empty folder {}) failed: {e}. A program may hold the folder (a shell with its cwd inside, an editor): close it first",
                plain(&t.path)
            )
        })?;
        done.push(format!("removed the empty folder {}", plain(&t.path)));
    } else {
        outln!("  worktree folder is already gone; run `git worktree prune` if git still lists it");
    }

    // e. branch
    if branch_exists {
        let flag = if a.force { "-D" } else { "-d" };
        let o = task::git(root, &["branch", flag, &entry.branch])?;
        if o.status.success() {
            done.push(format!("git branch {flag} {}", entry.branch));
        } else {
            return Err(format!(
                "step e (git branch {flag} {}) failed: {}. The worktree is gone; the registry entry stays so the run can resume. Merge the branch and run again, or use --force (-D)",
                entry.branch,
                stderr_of(&o)
            ));
        }
    }

    // f. registry
    fs::remove_file(reg).map_err(|e| format!("step f (delete the registry file) failed: cannot delete {}: {e}", reg.display()))?;
    done.push(format!("deleted {}", reg.display()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pi(pid: u32, name: &str, cmd: &str, cwd: Option<&str>) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmd: cmd.into(),
            cwd: cwd.map(PathBuf::from),
            rss_bytes: 0,
            cpu_percent: 0.0,
            parent: None,
            start_time: 0,
        }
    }

    #[test]
    fn selects_only_stoppable_kinds_inside_worktree() {
        let rules = ps::parse_proc_rules(ps::DEFAULT_PROC_RULES).unwrap();
        let wt = Path::new("/r/worktrees/a");
        let procs = vec![
            pi(1, "node", "node vite --port 5173", Some("/r/worktrees/a/web")),
            pi(2, "rust-analyzer", "rust-analyzer", Some("/r/worktrees/a")),
            pi(3, "pytest", "pytest -x", Some("/r/worktrees/a")),
            pi(4, "cargo", "cargo build", Some("/r/worktrees/a/target")),
            pi(5, "claude", "claude --resume", Some("/r/worktrees/a")),
            pi(6, "postgres", "postgres -D data", Some("/r/worktrees/a")),
            pi(7, "notepad", "notepad x.txt", Some("/r/worktrees/a")),
            pi(8, "node", "node vite", Some("/r/worktrees/ab")), // sibling with the same prefix
            pi(9, "node", "node vite", Some("/r/other")),
            pi(10, "node", "node vite", None),
            pi(99, "agentgc", "agentgc task done a", Some("/r/worktrees/a")), // self
            pi(11, "tsc", "tsc --watch", Some("/R/Worktrees/A/pkg")), // case differs
        ];
        let got = select_procs(&procs, &rules, wt, 99);
        let ids: Vec<u32> = got.iter().map(|f| f.pid).collect();
        assert_eq!(ids, vec![1, 2, 3, 4, 5, 6, 7, 11]);
        let stop: Vec<u32> = got.iter().filter(|f| f.stoppable).map(|f| f.pid).collect();
        assert_eq!(stop, vec![1, 2, 3, 4, 11]);
        for f in got.iter().filter(|f| ["agent", "db", "other"].contains(&f.kind.as_str())) {
            assert!(!f.stoppable, "{f:?}");
        }
        assert_eq!(got.iter().find(|f| f.pid == 5).unwrap().kind, "agent");
        assert_eq!(got.iter().find(|f| f.pid == 6).unwrap().kind, "db");
        assert_eq!(got.iter().find(|f| f.pid == 7).unwrap().kind, "other");
    }

    #[test]
    fn stoppable_kinds() {
        for k in ["dev-server", "lsp", "test", "build"] {
            assert!(is_stoppable(k));
        }
        for k in ["agent", "db", "other", ""] {
            assert!(!is_stoppable(k));
        }
    }

    #[test]
    fn guard_refuses_relative_dotdot_and_outside_missing() {
        let root = std::env::temp_dir().join("agentgc-no-such-root");
        let wt = root.join("worktrees");
        let s = |p: PathBuf| p.to_string_lossy().into_owned();
        assert!(guard(&root, "worktrees", "a", "relative/a").is_err());
        assert!(guard(&root, "worktrees", "a", &s(wt.join("..").join("a"))).is_err());
        assert!(guard(&root, "worktrees", "a", &s(std::env::temp_dir().join("elsewhere").join("a"))).is_err());
        assert!(guard(&root, "worktrees", "a", &s(wt.join("b"))).is_err());
        let ok = guard(&root, "worktrees", "a", &s(wt.join("a")));
        assert!(ok.is_ok() && !ok.unwrap().exists);
    }
}
