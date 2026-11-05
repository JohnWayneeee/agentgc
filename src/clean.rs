//! `agentgc clean`: delete the junk that `scan` lists, one item at a time, each with its own confirmation.
//! SAFETY FIRST. The hard guards below are never skipped by any flag. When in doubt, refuse.

use crate::ps::{self, ProcInfo};
use crate::rules::{Rule, Safety};
use crate::scan::{dir_size, expand_with, human};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A path with one of these components (case-insensitive) is never touched.
/// The same names are refused anywhere inside a path too.
const DENY: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".git",
    "documents",
    "desktop",
    "pictures",
    "videos",
    "downloads",
    "onedrive",
    "user data",
    "profile",
    "profiles",
    "login data",
    "cookies",
    "local storage",
    "indexeddb",
    "wallets",
    "wallet",
    "keystore",
    "application support",
];

const MIN_COMPONENTS: usize = 4;
const TOP_CHILDREN: usize = 5;
const DAY: u64 = 86_400;

pub struct CleanArgs {
    pub rules: Option<PathBuf>,
    pub only: Vec<String>,
    pub yes_item: Vec<String>,
    pub allow_ask: Vec<String>,
    pub dry_run: bool,
    pub log: Option<PathBuf>,
}

/// The only code that removes anything. Tests use a fake that records calls.
pub trait Remover {
    /// Delete for good. Returns the number of files that were skipped (locked, no access).
    fn permanent(&self, path: &Path) -> Result<u64, String>;
    /// Move to the Recycle Bin / Trash.
    fn to_trash(&self, path: &Path) -> Result<(), String>;
}

pub struct RealRemover;

impl Remover for RealRemover {
    fn permanent(&self, path: &Path) -> Result<u64, String> {
        remove_tree(path)
    }
    fn to_trash(&self, path: &Path) -> Result<(), String> {
        trash::delete(path).map_err(|e| format!("cannot move {} to the bin: {e}", path.display()))
    }
}

fn remove_link(p: &Path) -> std::io::Result<()> {
    fs::remove_file(p).or_else(|_| fs::remove_dir(p))
}

/// Remove a tree without following links: a symlink or junction is removed itself.
/// A file that cannot be removed is skipped and counted. No retries, no force tricks.
fn remove_tree(path: &Path) -> Result<u64, String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if meta.file_type().is_symlink() {
        return remove_link(path)
            .map(|_| 0)
            .map_err(|e| format!("cannot remove link {}: {e}", path.display()));
    }
    if !meta.is_dir() {
        return Ok(match fs::remove_file(path) {
            Ok(()) => 0,
            Err(_) => 1,
        });
    }
    let mut skipped = 0;
    if let Ok(rd) = fs::read_dir(path) {
        for entry in rd.flatten() {
            let p = entry.path();
            let is_real_dir = entry.file_type().map(|t| t.is_dir() && !t.is_symlink()).unwrap_or(false);
            if is_real_dir {
                skipped += remove_tree(&p).unwrap_or(1);
            } else if remove_link(&p).is_err() {
                skipped += 1;
            }
        }
    } else {
        skipped += 1;
    }
    let _ = fs::remove_dir(path); // fails when skipped files remain; that is expected
    Ok(skipped)
}

// ---------------------------------------------------------------- guards

fn denied_name(name: &str) -> bool {
    let l = name.to_lowercase();
    DENY.contains(&l.as_str()) || l.starts_with("profile ")
}

/// Home dir candidates from the environment (HOME and USERPROFILE).
pub fn homes_from<F: Fn(&str) -> Option<String>>(get: F) -> Vec<PathBuf> {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|k| get(k).filter(|s| !s.is_empty()))
        .map(PathBuf::from)
        .collect()
}

/// Static path guard. Err holds the reason. Never skippable.
pub fn guard_path(path: &Path, homes: &[PathBuf]) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("the path is not absolute".into());
    }
    let s = path.to_string_lossy();
    if s.split(['/', '\\']).any(|seg| seg == "." || seg == "..") {
        return Err("the path has '.' or '..' parts".into());
    }
    let n = path.components().count();
    if n < MIN_COMPONENTS || path.parent().is_none() {
        return Err(format!(
            "the path has {n} components, at least {MIN_COMPONENTS} are needed (no drive root or top folder)"
        ));
    }
    if homes.is_empty() {
        return Err("the home dir cannot be determined (HOME / USERPROFILE unset)".into());
    }
    let np = ps::norm(path);
    for h in homes {
        let nh = ps::norm(h);
        if np == nh {
            return Err("the path is the home dir".into());
        }
        if ps::path_under(&nh, &np) {
            return Err("the path is a parent of the home dir".into());
        }
    }
    for c in path.components() {
        if let Component::Normal(name) = c {
            let name = name.to_string_lossy();
            if denied_name(&name) {
                return Err(format!("the path has the protected component {name:?}"));
            }
        }
    }
    Ok(())
}

/// Content guard: a folder that holds a `.git` entry or any protected name anywhere inside is a
/// project or user data, not a cache. A folder that cannot be read is refused too.
pub fn guard_tree(path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => {}
        _ => return Ok(()),
    }
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(format!("cannot read {} ({e}); it may hide protected data", dir.display()))
            }
        };
        for entry in rd {
            let entry = entry.map_err(|e| format!("cannot read an entry in {}: {e}", dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if denied_name(&name) {
                return Err(format!("it contains {name:?} at {}", entry.path().display()));
            }
            if entry.file_type().map(|t| t.is_dir() && !t.is_symlink()).unwrap_or(false) {
                stack.push(entry.path());
            }
        }
    }
    Ok(())
}

fn refusal(rule_id: &str, rule_name: &str, path: &Path, why: &str) -> String {
    format!(
        "GUARD: rule {rule_id:?} ({rule_name}), path {}: refused: {why}. Nothing was touched",
        path.display()
    )
}

fn guard_all(id: &str, name: &str, path: &Path, homes: &[PathBuf]) -> Result<(), String> {
    guard_path(path, homes)
        .and_then(|_| guard_tree(path))
        .map_err(|why| refusal(id, name, path, &why))
}

/// Load-time check of every path of every rule that can be expanded.
pub fn validate_rules(
    rules: &[Rule],
    get: &dyn Fn(&str) -> Option<String>,
    homes: &[PathBuf],
) -> Result<(), String> {
    for r in rules {
        for raw in &r.paths {
            let Some(exp) = expand_with(raw, get) else { continue };
            guard_all(&r.id, &r.name, Path::new(&exp), homes)?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- items

#[derive(Debug, Clone)]
pub struct Item {
    pub id: String,
    pub name: String,
    pub safety: Safety,
    pub note: String,
    pub paths: Vec<(PathBuf, u64)>,
    pub bytes: u64,
}

/// One item per rule: its existing, non-empty paths. Duplicate paths count once. Largest first.
pub fn build_items(rules: &[Rule], get: &dyn Fn(&str) -> Option<String>) -> Vec<Item> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for r in rules {
        let mut paths = Vec::new();
        for raw in &r.paths {
            let Some(exp) = expand_with(raw, get) else { continue };
            let p = PathBuf::from(exp);
            if fs::symlink_metadata(&p).is_err() || seen.contains(&p) {
                continue;
            }
            seen.push(p.clone());
            let b = dir_size(&p);
            if b > 0 {
                paths.push((p, b));
            }
        }
        if paths.is_empty() {
            continue;
        }
        let bytes = paths.iter().map(|(_, b)| *b).sum();
        out.push(Item {
            id: r.id.clone(),
            name: r.name.clone(),
            safety: r.safety,
            note: r.note.clone(),
            paths,
            bytes,
        });
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

fn now_unix() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Age in days of the newest file under `path`. None when there is no file.
fn newest_age_days(path: &Path) -> Option<u64> {
    let mut newest: Option<SystemTime> = None;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(m) = fs::symlink_metadata(&p) else { continue };
        if m.is_dir() {
            if let Ok(rd) = fs::read_dir(&p) {
                stack.extend(rd.flatten().map(|e| e.path()));
            }
        } else if m.is_file() {
            if let Ok(t) = m.modified() {
                if newest.map_or(true, |n| t > n) {
                    newest = Some(t);
                }
            }
        }
    }
    let t = newest?;
    Some(SystemTime::now().duration_since(t).map(|d| d.as_secs() / DAY).unwrap_or(0))
}

fn top_children(path: &Path) -> Vec<(String, u64)> {
    let Ok(rd) = fs::read_dir(path) else { return Vec::new() };
    let mut v: Vec<(String, u64)> = rd
        .flatten()
        .map(|e| (e.file_name().to_string_lossy().into_owned(), dir_size(&e.path())))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(TOP_CHILDREN);
    v
}

macro_rules! say {
    ($out:expr) => {{
        let _ = writeln!($out);
    }};
    ($out:expr, $($a:tt)*) => {{
        let _ = writeln!($out, $($a)*);
    }};
}

fn show_item(out: &mut dyn Write, idx: usize, total: usize, it: &Item, procs: &[ProcInfo]) {
    say!(out);
    say!(
        out,
        "[{idx}/{total}] {}  (id: {})  safety: {}  total {}",
        it.name,
        it.id,
        it.safety.as_str(),
        human(it.bytes)
    );
    for (p, b) in &it.paths {
        say!(out, "  path: {}  ({})", p.display(), human(*b));
    }
    if !it.note.is_empty() {
        say!(out, "  note: {}", it.note);
    }
    for (p, _) in &it.paths {
        let ch = top_children(p);
        if !ch.is_empty() {
            say!(out, "  largest in {}:", p.display());
            for (n, b) in ch {
                say!(out, "    {:>10}  {n}", human(b));
            }
        }
    }
    match it.paths.iter().filter_map(|(p, _)| newest_age_days(p)).min() {
        Some(d) => say!(out, "  newest file: {d} days old"),
        None => say!(out, "  newest file: none"),
    }
    for (p, _) in &it.paths {
        let base = ps::norm(p);
        for pr in procs {
            if let Some(cwd) = &pr.cwd {
                if ps::path_under(&ps::norm(cwd), &base) {
                    say!(out, "  WARNING: running process inside: pid {} {} (nothing is stopped)", pr.pid, pr.name);
                }
            }
        }
    }
}

// ---------------------------------------------------------------- decisions

#[derive(Debug, Clone, PartialEq)]
enum Decision {
    Delete,
    Trash,
    Skip(String),
    Refuse(String),
    Quit,
}

/// Non-interactive decision, from flags only.
fn flag_decision(it: &Item, a: &CleanArgs) -> Decision {
    if !a.yes_item.contains(&it.id) {
        return Decision::Skip("not named with --yes-item (non-interactive)".into());
    }
    match it.safety {
        Safety::Safe => Decision::Delete,
        Safety::Ask if a.allow_ask.contains(&it.id) => Decision::Trash,
        Safety::Ask => Decision::Refuse(format!("ASK item needs --allow-ask {} as well", it.id)),
    }
}

pub fn read_answer(input: &mut dyn BufRead) -> Option<String> {
    let mut l = String::new();
    match input.read_line(&mut l) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(l.trim_end_matches(['\r', '\n']).to_string()),
    }
}

fn prompt(out: &mut dyn Write, input: &mut dyn BufRead, it: &Item) -> Decision {
    match it.safety {
        Safety::Safe => {
            let _ = write!(out, "Delete this? [y/N/q] ");
        }
        Safety::Ask => {
            say!(out, "  ASK item: may hold user data (accounts, sessions, projects)");
            say!(out, "  It goes to the Recycle Bin / Trash, never to permanent delete.");
            let _ = write!(
                out,
                "Type the item id \"{}\" exactly to continue (anything else skips, q quits): ",
                it.id
            );
        }
    }
    let _ = out.flush();
    let Some(ans) = read_answer(input) else {
        say!(out);
        return Decision::Quit; // end of input: stop the run
    };
    match it.safety {
        Safety::Safe => match ans.trim() {
            "y" => Decision::Delete,
            "q" => Decision::Quit,
            _ => Decision::Skip("answered no".into()),
        },
        Safety::Ask => {
            if ans == it.id {
                Decision::Trash
            } else if ans.trim() == "q" {
                Decision::Quit
            } else {
                Decision::Skip("the id was not typed exactly".into())
            }
        }
    }
}

// ---------------------------------------------------------------- execution

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ItemResult {
    pub id: String,
    pub action: &'static str, // deleted | trashed | skipped | refused
    pub freed: u64,
    pub trashed: u64,
    pub skipped_files: u64,
    pub errors: u64,
}

fn sum_sizes(it: &Item) -> u64 {
    it.paths.iter().map(|(p, _)| dir_size(p)).sum()
}

/// Guards again right before the removal, then the removal. Never permanent for `ask`.
fn execute(
    it: &Item,
    dec: &Decision,
    homes: &[PathBuf],
    remover: &dyn Remover,
    out: &mut dyn Write,
) -> ItemResult {
    let mut res = ItemResult { id: it.id.clone(), action: "refused", ..Default::default() };
    if it.safety == Safety::Ask && *dec == Decision::Delete {
        say!(out, "  REFUSED: ask items only go to the trash, never to permanent delete");
        return res;
    }
    for (p, _) in &it.paths {
        if let Err(e) = guard_all(&it.id, &it.name, p, homes) {
            say!(out, "  REFUSED: {e}");
            return res;
        }
    }
    let before = sum_sizes(it);
    for (p, _) in &it.paths {
        match dec {
            Decision::Delete => match remover.permanent(p) {
                Ok(s) => res.skipped_files += s,
                Err(e) => {
                    res.errors += 1;
                    say!(out, "  ERROR: {e}");
                }
            },
            Decision::Trash => {
                if let Err(e) = remover.to_trash(p) {
                    res.errors += 1;
                    say!(out, "  ERROR: {e}");
                }
            }
            _ => return res,
        }
    }
    let gone = before.saturating_sub(sum_sizes(it));
    if *dec == Decision::Delete {
        res.action = "deleted";
        res.freed = gone;
        say!(out, "  Deleted {}; skipped {} locked file(s).", human(gone), res.skipped_files);
    } else {
        res.action = "trashed";
        res.trashed = gone;
        say!(
            out,
            "  Moved {} to the Recycle Bin / Trash. Space is freed only after the bin is emptied.",
            human(gone)
        );
    }
    res
}

fn write_log(path: &Path, r: &ItemResult) -> Result<(), String> {
    let line = serde_json::json!({
        "item": r.id,
        "action": r.action,
        "bytes": if r.action == "trashed" { r.trashed } else { r.freed },
        "skipped": r.skipped_files,
        "time": now_unix(),
    });
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("cannot open log {}: {e}", path.display()))?;
    writeln!(f, "{line}").map_err(|e| format!("cannot write log: {e}"))
}

pub struct Ctx<'a> {
    pub args: &'a CleanArgs,
    pub get: &'a dyn Fn(&str) -> Option<String>,
    pub homes: &'a [PathBuf],
    pub procs: &'a [ProcInfo],
    pub interactive: bool,
    pub free_space: &'a dyn Fn() -> Option<u64>,
}

/// The whole run. Input and output are injected so tests can drive it.
pub fn run_clean(
    ctx: &Ctx,
    rules: &[Rule],
    remover: &dyn Remover,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<ItemResult>, String> {
    let a = ctx.args;
    for (flag, ids) in [("--only", &a.only), ("--yes-item", &a.yes_item), ("--allow-ask", &a.allow_ask)] {
        for id in ids.iter() {
            if !rules.iter().any(|r| &r.id == id) {
                return Err(format!("{flag} {id}: no rule with this id"));
            }
        }
    }
    let selected: Vec<Rule> = rules
        .iter()
        .filter(|r| a.only.is_empty() || a.only.contains(&r.id))
        .cloned()
        .collect();
    validate_rules(&selected, ctx.get, ctx.homes)?;

    let items = build_items(&selected, ctx.get);
    if items.is_empty() {
        say!(out, "Nothing to clean.");
        return Ok(Vec::new());
    }
    if ctx.interactive && (!a.yes_item.is_empty() || !a.allow_ask.is_empty()) {
        say!(out, "Note: --yes-item and --allow-ask apply only when stdin is not a terminal. Every item is asked.");
    }
    if !ctx.interactive && !a.dry_run {
        say!(out, "stdin is not a terminal: nothing is asked; only items named with --yes-item are removed.");
    }
    let free_before = (ctx.free_space)();
    let mut results: Vec<ItemResult> = Vec::new();
    let total = items.len();
    for (i, it) in items.iter().enumerate() {
        show_item(out, i + 1, total, it, ctx.procs);
        let dec = if a.dry_run {
            let plan = if ctx.interactive {
                "would ask for confirmation".to_string()
            } else {
                match flag_decision(it, a) {
                    Decision::Delete => "would delete permanently".into(),
                    Decision::Trash => "would move to the Recycle Bin / Trash".into(),
                    Decision::Skip(w) => format!("would skip: {w}"),
                    Decision::Refuse(w) => format!("would refuse: {w}"),
                    Decision::Quit => String::new(),
                }
            };
            say!(out, "  [dry-run] {plan}. Nothing is asked or deleted.");
            Decision::Skip("dry-run".into())
        } else if ctx.interactive {
            prompt(out, input, it)
        } else {
            flag_decision(it, a)
        };
        let res = match &dec {
            Decision::Quit => {
                say!(out, "  Stopped by the user.");
                break;
            }
            Decision::Skip(why) => {
                if !a.dry_run {
                    say!(out, "  Skipped: {why}.");
                }
                ItemResult { id: it.id.clone(), action: "skipped", ..Default::default() }
            }
            Decision::Refuse(why) => {
                say!(out, "  REFUSED: {why}.");
                ItemResult { id: it.id.clone(), action: "refused", ..Default::default() }
            }
            Decision::Delete | Decision::Trash => execute(it, &dec, ctx.homes, remover, out),
        };
        if let (Some(lp), false) = (&a.log, a.dry_run) {
            if let Err(e) = write_log(lp, &res) {
                say!(out, "  warning: {e}");
            }
        }
        results.push(res);
    }
    summary(out, &results, free_before, (ctx.free_space)(), a.dry_run);
    Ok(results)
}

fn summary(out: &mut dyn Write, rs: &[ItemResult], before: Option<u64>, after: Option<u64>, dry: bool) {
    say!(out);
    if dry {
        say!(out, "Dry run: nothing was deleted.");
    }
    for r in rs {
        let b = if r.action == "trashed" { r.trashed } else { r.freed };
        let note = if r.action == "trashed" { " (in the bin; not freed yet)" } else { "" };
        say!(out, "{:>10}  {:<8}  {}  skipped locked files: {}{note}", human(b), r.action, r.id, r.skipped_files);
    }
    let freed: u64 = rs.iter().map(|r| r.freed).sum();
    let trashed: u64 = rs.iter().map(|r| r.trashed).sum();
    let skipped: u64 = rs.iter().map(|r| r.skipped_files).sum();
    say!(out, "{:>10}  TOTAL freed, {} locked file(s) skipped", human(freed), skipped);
    if trashed > 0 {
        say!(
            out,
            "{:>10}  moved to the Recycle Bin / Trash: space is freed only after the bin is emptied",
            human(trashed)
        );
    }
    if let (Some(b), Some(a)) = (before, after) {
        say!(out, "System drive free space: {} before, {} after", human(b), human(a));
    }
}

/// Free bytes on the system drive (SystemDrive on Windows, `/` elsewhere). None when unreadable.
pub fn system_drive_free() -> Option<u64> {
    use sysinfo::Disks;
    let disks = Disks::new_with_refreshed_list();
    let want = std::env::var("SystemDrive")
        .ok()
        .map(|d| format!("{}\\", d.trim_end_matches(['\\', '/']).to_lowercase()));
    disks
        .list()
        .iter()
        .find(|d| {
            let mp = d.mount_point().to_string_lossy().to_lowercase();
            match &want {
                Some(w) => &mp == w,
                None => mp == "/",
            }
        })
        .map(|d| d.available_space())
}

pub fn cmd_clean(a: &CleanArgs) -> Result<(), String> {
    use std::io::IsTerminal;
    let text = match &a.rules {
        Some(p) => fs::read_to_string(p).map_err(|e| format!("cannot read {}: {e}", p.display()))?,
        None => crate::rules::DEFAULT_RULES.to_string(),
    };
    let rules = crate::rules::parse_rules(&text)?;
    let get = |k: &str| std::env::var(k).ok();
    let homes = homes_from(get);
    let interactive = std::io::stdin().is_terminal();
    let procs = ps::read_system();
    let ctx = Ctx {
        args: a,
        get: &get,
        homes: &homes,
        procs: &procs,
        interactive,
        free_space: &system_drive_free,
    };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    run_clean(&ctx, &rules, &RealRemover, &mut input, &mut out).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse_rules;
    use std::cell::RefCell;
    use std::io::Cursor;

    /// Absolute test path on any OS.
    fn abs(s: &str) -> PathBuf {
        if cfg!(windows) {
            PathBuf::from(format!("C:{s}"))
        } else {
            PathBuf::from(s)
        }
    }

    fn homes() -> Vec<PathBuf> {
        vec![abs("/fakehome/u")]
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-clean-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn fwd(p: &Path) -> String {
        p.to_string_lossy().replace('\\', "/")
    }

    /// Records calls. Really removes files in temp dirs, except a "locked" file name.
    struct Fake {
        calls: RefCell<Vec<(&'static str, PathBuf)>>,
        locked: Option<&'static str>,
    }
    impl Fake {
        fn new() -> Self {
            Fake { calls: RefCell::new(Vec::new()), locked: None }
        }
        fn n(&self, kind: &str) -> usize {
            self.calls.borrow().iter().filter(|(k, _)| *k == kind).count()
        }
    }
    impl Remover for Fake {
        fn permanent(&self, path: &Path) -> Result<u64, String> {
            self.calls.borrow_mut().push(("permanent", path.to_path_buf()));
            let mut skipped = 0;
            for e in fs::read_dir(path).unwrap().flatten() {
                if Some(e.file_name().to_string_lossy().as_ref()) == self.locked {
                    skipped += 1;
                } else {
                    fs::remove_file(e.path()).unwrap();
                }
            }
            Ok(skipped)
        }
        fn to_trash(&self, path: &Path) -> Result<(), String> {
            self.calls.borrow_mut().push(("trash", path.to_path_buf()));
            Ok(())
        }
    }

    struct World {
        root: PathBuf,
        rules: Vec<Rule>,
    }

    /// big (safe, 300 B), small (safe, 100 B), acct (ask, 200 B).
    fn world(name: &str) -> World {
        let root = tmp(name);
        for (d, n) in [("big", 300), ("small", 100), ("acct", 200)] {
            fs::create_dir_all(root.join(d)).unwrap();
            fs::write(root.join(d).join("f.bin"), vec![1u8; n]).unwrap();
        }
        let r = fwd(&root);
        let text = format!(
            "[[rule]]\nid=\"big\"\nname=\"Big\"\npaths=[\"{r}/big\"]\nsafety=\"safe\"\n\
             [[rule]]\nid=\"small\"\nname=\"Small\"\npaths=[\"{r}/small\"]\nsafety=\"safe\"\n\
             [[rule]]\nid=\"acct\"\nname=\"Acct\"\npaths=[\"{r}/acct\"]\nsafety=\"ask\"\n"
        );
        World { root, rules: parse_rules(&text).unwrap() }
    }

    fn args() -> CleanArgs {
        CleanArgs { rules: None, only: vec![], yes_item: vec![], allow_ask: vec![], dry_run: false, log: None }
    }

    fn go(
        w: &World,
        a: &CleanArgs,
        interactive: bool,
        input: &str,
        fake: &Fake,
    ) -> (Result<Vec<ItemResult>, String>, String) {
        let h = homes();
        let get = |_: &str| None;
        let free = || Some(1000u64);
        let ctx = Ctx { args: a, get: &get, homes: &h, procs: &[], interactive, free_space: &free };
        let mut out: Vec<u8> = Vec::new();
        let r = run_clean(&ctx, &w.rules, fake, &mut Cursor::new(input.as_bytes().to_vec()), &mut out);
        (r, String::from_utf8_lossy(&out).into_owned())
    }

    #[test]
    fn y_deletes_safe_item_others_do_not() {
        for (ans, deleted) in [("y\n", true), ("n\n", false), ("\n", false), ("q\n", false), ("yes\n", false)] {
            let w = world("yn");
            let mut a = args();
            a.only = vec!["small".into()];
            let f = Fake::new();
            let (r, _) = go(&w, &a, true, ans, &f);
            r.unwrap();
            assert_eq!(f.n("permanent"), usize::from(deleted), "answer {ans:?}");
            assert_eq!(!w.root.join("small/f.bin").exists(), deleted);
            let _ = fs::remove_dir_all(&w.root);
        }
    }

    #[test]
    fn q_stops_the_loop_and_order_is_by_size() {
        let w = world("q");
        let mut a = args();
        a.only = vec!["big".into(), "small".into()];
        let f = Fake::new();
        let (r, out) = go(&w, &a, true, "q\ny\n", &f);
        r.unwrap();
        assert_eq!(f.calls.borrow().len(), 0);
        assert!(w.root.join("small/f.bin").exists());
        assert_eq!(out.matches("Delete this?").count(), 1);
        // big (300 B) comes first, so "n" skips big and "y" deletes small
        let f2 = Fake::new();
        let (r, _) = go(&w, &a, true, "n\ny\n", &f2);
        r.unwrap();
        assert_eq!(f2.n("permanent"), 1);
        assert!(f2.calls.borrow()[0].1.ends_with("small"));
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn end_of_input_stops_without_deleting() {
        let w = world("eof");
        let f = Fake::new();
        go(&w, &args(), true, "", &f).0.unwrap();
        assert_eq!(f.calls.borrow().len(), 0);
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn ask_item_needs_exact_id_and_goes_to_trash_only() {
        let mut a = args();
        a.only = vec!["acct".into()];
        for ans in ["y\n", "Y\n", "yes\n", "ACCT\n", "acct x\n", "\n", "n\n"] {
            let w = world("ask1");
            let f = Fake::new();
            go(&w, &a, true, ans, &f).0.unwrap();
            assert_eq!(f.calls.borrow().len(), 0, "answer {ans:?}");
            let _ = fs::remove_dir_all(&w.root);
        }
        let w = world("ask2");
        let f = Fake::new();
        let (r, out) = go(&w, &a, true, "acct\n", &f);
        assert_eq!(r.unwrap()[0].action, "trashed");
        assert_eq!(f.n("trash"), 1);
        assert_eq!(f.n("permanent"), 0);
        assert!(out.contains("ASK item: may hold user data (accounts, sessions, projects)"));
        assert!(out.contains("only after the bin is emptied"));
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn execute_never_deletes_ask_item_permanently() {
        let w = world("ask3");
        let it = build_items(&w.rules, &|_: &str| None).into_iter().find(|i| i.id == "acct").unwrap();
        let f = Fake::new();
        let mut out: Vec<u8> = Vec::new();
        let r = execute(&it, &Decision::Delete, &homes(), &f, &mut out);
        assert_eq!(r.action, "refused");
        assert_eq!(f.calls.borrow().len(), 0);
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn non_interactive_deletes_nothing_by_default() {
        let w = world("ni1");
        let f = Fake::new();
        let (r, _) = go(&w, &args(), false, "y\ny\ny\n", &f);
        assert!(r.unwrap().iter().all(|x| x.action == "skipped"));
        assert_eq!(f.calls.borrow().len(), 0);
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn non_interactive_yes_item_and_ask_gate() {
        let w = world("ni2");
        let mut a = args();
        a.yes_item = vec!["big".into(), "acct".into()];
        let f = Fake::new();
        let (r, out) = go(&w, &a, false, "", &f);
        let r = r.unwrap();
        assert_eq!(f.n("permanent"), 1);
        assert_eq!(f.n("trash"), 0);
        assert!(r.iter().any(|x| x.id == "acct" && x.action == "refused"));
        assert!(out.contains("--allow-ask acct"));
        assert!(w.root.join("small/f.bin").exists());
        // with --allow-ask the ask item goes to the trash, never permanent
        a.allow_ask = vec!["acct".into()];
        let f2 = Fake::new();
        go(&w, &a, false, "", &f2).0.unwrap();
        assert_eq!(f2.n("trash"), 1);
        assert_eq!(f2.n("permanent"), 0);
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn allow_ask_alone_does_nothing() {
        let w = world("ni3");
        let mut a = args();
        a.allow_ask = vec!["acct".into()];
        let f = Fake::new();
        go(&w, &a, false, "", &f).0.unwrap();
        assert_eq!(f.calls.borrow().len(), 0);
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn unknown_ids_are_errors() {
        let w = world("ids");
        let mut a = args();
        a.yes_item = vec!["nope".into()];
        assert!(go(&w, &a, false, "", &Fake::new()).0.unwrap_err().contains("--yes-item nope"));
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn dry_run_never_calls_remover_or_asks() {
        for interactive in [true, false] {
            let w = world("dry");
            let mut a = args();
            a.dry_run = true;
            a.yes_item = vec!["big".into(), "acct".into()];
            a.allow_ask = vec!["acct".into()];
            let f = Fake::new();
            let (r, out) = go(&w, &a, interactive, "y\ny\nacct\n", &f);
            r.unwrap();
            assert_eq!(f.calls.borrow().len(), 0);
            assert!(!out.contains("Delete this?"));
            assert!(out.contains("[dry-run]"));
            assert!(w.root.join("big/f.bin").exists());
            let _ = fs::remove_dir_all(&w.root);
        }
    }

    #[test]
    fn sizes_before_after_and_skipped_counter() {
        let w = world("lock");
        fs::write(w.root.join("big/locked.bin"), vec![1u8; 50]).unwrap();
        let mut a = args();
        a.only = vec!["big".into()];
        let f = Fake { calls: RefCell::new(Vec::new()), locked: Some("locked.bin") };
        let (r, out) = go(&w, &a, true, "y\n", &f);
        let r = r.unwrap();
        assert_eq!(r[0].action, "deleted");
        assert_eq!(r[0].freed, 300); // 350 before, 50 left
        assert_eq!(r[0].skipped_files, 1);
        assert!(w.root.join("big/locked.bin").exists());
        assert!(out.contains("skipped 1 locked"));
        assert!(out.contains("TOTAL freed, 1 locked file(s) skipped"));
        assert!(out.contains("System drive free space"));
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn log_gets_one_json_line_per_item() {
        let w = world("log");
        let lp = w.root.join("clean.log");
        let mut a = args();
        a.log = Some(lp.clone());
        a.yes_item = vec!["big".into()];
        go(&w, &a, false, "", &Fake::new()).0.unwrap();
        let text = fs::read_to_string(&lp).unwrap();
        assert_eq!(text.lines().count(), 3);
        let v: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
        assert_eq!(v["item"], "big");
        assert_eq!(v["action"], "deleted");
        assert_eq!(v["bytes"], 300);
        assert!(v["time"].as_u64().unwrap() > 0);
        let _ = fs::remove_dir_all(&w.root);
    }

    // ---- guards

    #[test]
    fn guard_accepts_a_normal_cache_path() {
        assert!(guard_path(&abs("/fakehome/u/.cache/uv"), &homes()).is_ok());
    }

    #[test]
    fn guard_refuses_root_like_and_short_paths() {
        let h = homes();
        // on Windows "C:" and "\\" count as components, so "C:\a\b" already has 4
        let mut short = vec![abs("/"), abs("/Users")];
        if cfg!(unix) {
            short.push(abs("/a/b"));
        }
        for p in short {
            assert!(guard_path(&p, &h).is_err(), "{}", p.display());
        }
        assert!(guard_path(Path::new("relative/dir/x/y"), &h).is_err());
        assert!(guard_path(Path::new(""), &h).is_err());
        // exactly the minimum passes
        let min = if cfg!(windows) { abs("/a/b") } else { abs("/a/b/c") };
        assert!(guard_path(&min, &h).is_ok());
    }

    #[test]
    fn guard_refuses_dot_and_dotdot() {
        let h = homes();
        assert!(guard_path(&abs("/fakehome/u/../v/x/y"), &h).unwrap_err().contains("'..'"));
        assert!(guard_path(&abs("/fakehome/u/./x/y"), &h).unwrap_err().contains("'.'"));
    }

    #[test]
    fn guard_refuses_home_and_its_parents() {
        let h = vec![abs("/data/users/u/home")];
        assert!(guard_path(&abs("/data/users/u/home"), &h).unwrap_err().contains("home dir"));
        assert!(guard_path(&abs("/DATA/Users/U/Home/"), &h).is_err());
        assert!(guard_path(&abs("/data/users/u"), &h).unwrap_err().contains("parent of the home"));
        assert!(guard_path(&abs("/data/users/u/home/.npm"), &h).is_ok());
        assert!(guard_path(&abs("/data/users/u/home/.npm"), &[]).is_err());
    }

    #[test]
    fn guard_refuses_every_deny_component() {
        let h = homes();
        for c in DENY.iter().copied().chain(["Profile 1", "profile 7"]) {
            for case in [c.to_string(), c.to_uppercase()] {
                let p = abs(&format!("/fakehome/u/{case}/x"));
                let e = guard_path(&p, &h).unwrap_err();
                assert!(e.contains("protected component"), "{case}: {e}");
            }
        }
        // a longer name that only starts like a protected one is fine
        assert!(guard_path(&abs("/fakehome/u/Profilex/x"), &h).is_ok());
    }

    #[test]
    fn guard_tree_refuses_git_and_protected_names_inside() {
        let d = tmp("tree");
        fs::create_dir_all(d.join("cache/deep/.git")).unwrap();
        fs::write(d.join("cache/deep/f"), b"x").unwrap();
        assert!(guard_tree(&d.join("cache")).unwrap_err().contains(".git"));
        let d2 = tmp("tree2");
        fs::create_dir_all(d2.join("c/sub")).unwrap();
        fs::write(d2.join("c/sub/Cookies"), b"x").unwrap();
        assert!(guard_tree(&d2.join("c")).unwrap_err().contains("Cookies"));
        fs::remove_file(d2.join("c/sub/Cookies")).unwrap();
        assert!(guard_tree(&d2.join("c")).is_ok());
        // a .git file (worktree) counts too
        fs::write(d2.join("c/.git"), b"gitdir: x").unwrap();
        assert!(guard_tree(&d2.join("c")).is_err());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn rules_pointing_at_protected_paths_are_refused_at_load() {
        let w = world("load");
        let r = fwd(&w.root);
        fs::create_dir_all(w.root.join("proj/.git")).unwrap(); // a project inside the path
        fs::write(w.root.join("proj/f"), vec![1u8; 10]).unwrap();
        fs::create_dir_all(w.root.join("Documents/x")).unwrap(); // protected component in the path
        fs::write(w.root.join("Documents/x/f"), vec![1u8; 10]).unwrap();
        for (path, want) in [
            (format!("{r}/proj"), ".git"),
            (format!("{r}/Documents/x"), "Documents"),
            (format!("{r}/../escape"), "'..'"),
            ("relative/a/b/c".to_string(), "not absolute"),
        ] {
            let text = format!(
                "[[rule]]\nid=\"bad\"\nname=\"Bad rule\"\npaths=[\"{path}\"]\nsafety=\"safe\"\n\
                 [[rule]]\nid=\"small\"\nname=\"Small\"\npaths=[\"{r}/small\"]\nsafety=\"safe\"\n"
            );
            let w2 = World { root: w.root.clone(), rules: parse_rules(&text).unwrap() };
            let f = Fake::new();
            let e = go(&w2, &args(), false, "", &f).0.unwrap_err();
            assert!(e.contains("\"bad\"") && e.contains("Bad rule") && e.contains(want), "{e}");
            assert!(e.contains(&Path::new(&path).display().to_string()), "{e}");
            assert_eq!(f.calls.borrow().len(), 0);
        }
        // --only leaves the bad rule out of the load check
        let text = format!(
            "[[rule]]\nid=\"bad\"\nname=\"Bad\"\npaths=[\"{r}/proj\"]\nsafety=\"safe\"\n\
             [[rule]]\nid=\"small\"\nname=\"Small\"\npaths=[\"{r}/small\"]\nsafety=\"safe\"\n"
        );
        let w3 = World { root: w.root.clone(), rules: parse_rules(&text).unwrap() };
        let mut a = args();
        a.only = vec!["small".into()];
        assert!(go(&w3, &a, true, "n\n", &Fake::new()).0.is_ok());
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn guards_run_again_right_before_deletion() {
        let w = world("late");
        let it = build_items(&w.rules, &|_: &str| None).into_iter().find(|i| i.id == "small").unwrap();
        // a project appears after the screen was shown
        fs::create_dir_all(w.root.join("small/.git")).unwrap();
        let f = Fake::new();
        let mut out: Vec<u8> = Vec::new();
        let r = execute(&it, &Decision::Delete, &homes(), &f, &mut out);
        assert_eq!(r.action, "refused");
        assert_eq!(f.calls.borrow().len(), 0);
        assert!(String::from_utf8_lossy(&out).contains("GUARD"));
        let _ = fs::remove_dir_all(&w.root);
    }

    #[test]
    fn default_rules_pass_guards_and_user_data_rules_are_ask() {
        let rules = parse_rules(crate::rules::DEFAULT_RULES).unwrap();
        let b = fwd(&abs("/Users/u"));
        let get = move |k: &str| match k {
            "HOME" | "USERPROFILE" => Some(b.clone()),
            "LOCALAPPDATA" => Some(format!("{b}/AppData/Local")),
            "APPDATA" => Some(format!("{b}/AppData/Roaming")),
            "TEMP" => Some(format!("{b}/AppData/Local/Temp")),
            "XDG_CACHE_HOME" => Some(format!("{b}/.cache")),
            _ => None,
        };
        let h = homes_from(&get);
        for r in &rules {
            for raw in &r.paths {
                if let Some(p) = expand_with(raw, &get) {
                    guard_path(Path::new(&p), &h).unwrap_or_else(|e| panic!("{} {p}: {e}", r.id));
                }
            }
        }
        for id in ["gemini-cli", "claude-projects", "docker-wsl", "huggingface", "codex-junk"] {
            let r = rules.iter().find(|r| r.id == id).unwrap();
            assert_eq!(r.safety, Safety::Ask, "{id}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_removed_not_followed() {
        let d = tmp("link");
        let target = tmp("link-target");
        fs::write(target.join("keep"), b"x").unwrap();
        fs::create_dir_all(d.join("c")).unwrap();
        std::os::unix::fs::symlink(&target, d.join("c/l")).unwrap();
        remove_tree(&d.join("c")).unwrap();
        assert!(target.join("keep").exists());
        assert!(!d.join("c").exists());
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&target);
    }

    #[cfg(windows)]
    fn try_junction(target: &Path, link: &Path) -> bool {
        std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[cfg(windows)]
    #[test]
    fn junctions_are_removed_not_followed() {
        let d = tmp("junction");
        let target = tmp("junction-target");
        fs::write(target.join("keep"), b"x").unwrap();
        fs::create_dir_all(d.join("c")).unwrap();
        if !try_junction(&target, &d.join("c").join("l")) {
            eprintln!("skipped: cannot create a junction here");
            let _ = fs::remove_dir_all(&d);
            let _ = fs::remove_dir_all(&target);
            return;
        }
        remove_tree(&d.join("c")).unwrap();
        assert!(target.join("keep").exists());
        assert!(!d.join("c").exists());

        // Direct call on the junction path.
        let j = d.join("j");
        if try_junction(&target, &j) {
            remove_tree(&j).unwrap();
            assert!(!j.exists());
            assert!(target.join("keep").exists());
            // Safety net: never remove_dir_all over a live junction.
            let _ = fs::remove_dir(&j);
        }
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&target);
    }

    #[test]
    fn real_remover_removes_a_temp_tree() {
        let d = tmp("real");
        fs::create_dir_all(d.join("c/sub")).unwrap();
        fs::write(d.join("c/sub/f"), b"x").unwrap();
        assert_eq!(RealRemover.permanent(&d.join("c")).unwrap(), 0);
        assert!(!d.join("c").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
