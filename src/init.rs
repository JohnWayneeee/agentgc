//! `agentgc init`: install the agent rules from `integrations/` into a project, only after a confirmation.
//! The two texts are embedded in the binary. Target names and paths are fixed: no user-given file path.
//! SAFETY FIRST. User text is never overwritten. When in doubt, refuse.

use crate::clean::{homes_from, read_answer};
use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

pub const BEGIN: &str = "<!-- agentgc:begin -->";
pub const END: &str = "<!-- agentgc:end -->";

const AGENTS_RAW: &str = include_str!("../integrations/AGENTS.md");
const SKILL_RAW: &str = include_str!("../integrations/claude-code/SKILL.md");

const NAMES: &[&str] = &["agents-md", "claude-md", "skill", "global-skill"];

macro_rules! w {
    ($out:expr) => {{
        let _ = writeln!($out);
    }};
    ($out:expr, $($a:tt)*) => {{
        let _ = writeln!($out, $($a)*);
    }};
}

#[derive(Default)]
pub struct InitArgs {
    pub agents_md: bool,
    pub claude_md: bool,
    pub skill: bool,
    pub global_skill: bool,
    pub dir: Option<PathBuf>,
    pub yes_target: Vec<String>,
    pub print: bool,
}

pub struct Ctx<'a> {
    /// Environment getter (HOME / USERPROFILE). Injected so tests never touch the real home.
    pub get: &'a dyn Fn(&str) -> Option<String>,
    pub interactive: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub name: &'static str,
    /// written | skipped | up-to-date | refused | failed
    pub result: &'static str,
}

fn lf(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// The AGENTS.md snippet without a trailing newline.
fn snippet() -> String {
    lf(AGENTS_RAW).trim_end().to_string()
}

fn block() -> String {
    format!("{BEGIN}\n{}\n{END}\n", snippet())
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    /// Marker block inside a user file (AGENTS.md, CLAUDE.md).
    Block,
    /// A whole file we own only when we create it (SKILL.md).
    Whole,
}

struct Target {
    name: &'static str,
    /// None: the home dir is unknown, the target is refused.
    path: Option<PathBuf>,
    kind: Kind,
    /// Canonical `--dir` for project targets: the parent folder must stay inside it.
    root: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
enum Action {
    Create,
    Append,
    Update,
    UpToDate,
    Refuse(String),
}

struct Plan {
    name: &'static str,
    path: Option<PathBuf>,
    action: Action,
    original: Vec<u8>,
    new: String,
}

impl Plan {
    fn writable(&self) -> bool {
        matches!(self.action, Action::Create | Action::Append | Action::Update)
    }
}

fn bak_path(p: &Path) -> PathBuf {
    let mut n = p.file_name().map(|s| s.to_os_string()).unwrap_or_default();
    n.push(".agentgc.bak");
    p.with_file_name(n)
}

fn tmp_path(p: &Path) -> PathBuf {
    let mut n = std::ffi::OsString::from(".");
    n.push(p.file_name().unwrap_or_default());
    n.push(".agentgc.tmp");
    p.with_file_name(n)
}

/// Windows canonical paths start with `\\?\`; strip it for display and joins.
fn strip_verbatim(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p.to_path_buf(),
    }
}

// ---------------------------------------------------------------- guards and plan

/// Never skippable: the target is a regular file or absent; for project targets the
/// deepest existing parent resolves inside `--dir`.
fn check_guards(t: &Target, path: &Path) -> Result<(), String> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => return Err("the target is a symlink".into()),
        Ok(m) if m.is_dir() => return Err("the target is a folder".into()),
        Ok(m) if !m.is_file() => return Err("the target is not a regular file".into()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("cannot read the target: {e}")),
    }
    if let Some(root) = &t.root {
        let mut a = path.parent();
        let anc = loop {
            match a {
                Some(x) if fs::symlink_metadata(x).is_ok() => break x,
                Some(x) => a = x.parent(),
                None => return Err("no existing parent folder".into()),
            }
        };
        let c = fs::canonicalize(anc).map_err(|e| format!("cannot resolve {}: {e}", anc.display()))?;
        if !c.is_dir() {
            return Err(format!("{} is not a folder", anc.display()));
        }
        if !c.starts_with(root) {
            return Err(format!("the parent folder {} resolves outside --dir", anc.display()));
        }
    }
    Ok(())
}

fn plan_target(t: &Target) -> Plan {
    let mut p = Plan {
        name: t.name,
        path: t.path.clone(),
        action: Action::UpToDate,
        original: Vec::new(),
        new: String::new(),
    };
    let refuse = |mut p: Plan, why: String| {
        p.action = Action::Refuse(why);
        p
    };
    let Some(path) = &t.path else {
        return refuse(p, "the home dir is unknown or missing (HOME / USERPROFILE)".into());
    };
    if let Err(e) = check_guards(t, path) {
        return refuse(p, e);
    }
    let exists = fs::symlink_metadata(path).is_ok();
    match (t.kind, exists) {
        (Kind::Block, false) => {
            p.action = Action::Create;
            p.new = block();
        }
        (Kind::Whole, false) => {
            p.action = Action::Create;
            p.new = lf(SKILL_RAW);
        }
        (kind, true) => {
            let bytes = match fs::read(path) {
                Ok(b) => b,
                Err(e) => return refuse(p, format!("cannot read the file: {e}")),
            };
            let Ok(text) = String::from_utf8(bytes.clone()) else {
                return refuse(p, "the file is not valid UTF-8".into());
            };
            p.original = bytes;
            if kind == Kind::Whole {
                if lf(&text) == lf(SKILL_RAW) {
                    p.action = Action::UpToDate;
                    return p;
                }
                return refuse(p, "the file exists and differs from the embedded text; never overwritten".into());
            }
            let nb = text.matches(BEGIN).count();
            let ne = text.matches(END).count();
            let (action, new) = match (nb, ne) {
                (0, 0) => {
                    let mut s = text.clone();
                    if !s.is_empty() {
                        if !s.ends_with('\n') {
                            s.push('\n');
                        }
                        s.push('\n');
                    }
                    s.push_str(&block());
                    (Action::Append, s)
                }
                (1, 1) => {
                    let (bi, ei) = (text.find(BEGIN).unwrap_or(0), text.find(END).unwrap_or(0));
                    let start = bi + BEGIN.len();
                    if start > ei {
                        return refuse(p, "the markers are in the wrong order".into());
                    }
                    if lf(&text[start..ei]) == format!("\n{}\n", snippet()) {
                        p.action = Action::UpToDate;
                        return p;
                    }
                    (Action::Update, format!("{}\n{}\n{}", &text[..start], snippet(), &text[ei..]))
                }
                _ => {
                    return refuse(
                        p,
                        format!("the markers are unbalanced or duplicated ({nb} begin, {ne} end)"),
                    )
                }
            };
            if fs::symlink_metadata(bak_path(path)).is_ok() {
                return refuse(p, format!("the backup {} already exists", bak_path(path).display()));
            }
            p.action = action;
            p.new = new;
        }
    }
    p
}

// ---------------------------------------------------------------- writing

fn create_new_file(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| format!("cannot create {}: {e}", path.display()))?;
    if let Err(e) = f.write_all(data).and_then(|_| f.sync_all()) {
        drop(f);
        let _ = fs::remove_file(path); // our own half-written file
        return Err(format!("cannot write {}: {e}", path.display()));
    }
    Ok(())
}

fn write_existing(path: &Path, original: &[u8], new: &str) -> Result<(), String> {
    let bak = bak_path(path);
    create_new_file(&bak, original).map_err(|e| format!("backup failed: {e}"))?;
    let tmp = tmp_path(path);
    create_new_file(&tmp, new.as_bytes())?;
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp); // our own temp file
        return Err(format!("cannot replace {}: {e}", path.display()));
    }
    Ok(())
}

fn apply(plan: &Plan) -> Result<(), String> {
    let path = plan.path.as_ref().ok_or("no path")?;
    match plan.action {
        Action::Create => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create folder {}: {e}", parent.display()))?;
            }
            create_new_file(path, plan.new.as_bytes())
        }
        Action::Append | Action::Update => write_existing(path, &plan.original, &plan.new),
        _ => Ok(()),
    }
}

// ---------------------------------------------------------------- run

fn describe(p: &Plan) -> String {
    let old = p.original.len();
    let new = p.new.len();
    let bak = p.path.as_ref().map(|x| bak_path(x).display().to_string()).unwrap_or_default();
    match &p.action {
        Action::Create => format!("would create the file ({new} bytes)"),
        Action::Append => format!(
            "would append a block ({new} bytes after the {old} you have; {:+} bytes); backup: {bak}",
            new as i64 - old as i64
        ),
        Action::Update => format!(
            "would update the block ({old} -> {new} bytes; {:+} bytes); backup: {bak}",
            new as i64 - old as i64
        ),
        Action::UpToDate => "already up to date".into(),
        Action::Refuse(w) => format!("REFUSED: {w}"),
    }
}

pub fn run_init(
    ctx: &Ctx,
    a: &InitArgs,
    input: &mut dyn BufRead,
    out: &mut dyn Write,
) -> Result<Vec<Outcome>, String> {
    for n in &a.yes_target {
        if !NAMES.contains(&n.as_str()) {
            return Err(format!("--yes-target {n}: unknown target name (use: {})", NAMES.join(", ")));
        }
    }
    if a.print {
        w!(out, "{}", snippet());
        return Ok(Vec::new());
    }
    let dir = a.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    match fs::metadata(&dir) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(format!("--dir {}: not a folder", dir.display())),
        Err(e) => return Err(format!("--dir {}: {e}", dir.display())),
    }
    let root = fs::canonicalize(&dir).map_err(|e| format!("--dir {}: {e}", dir.display()))?;
    let base = strip_verbatim(&root);

    let any_project = a.agents_md || a.claude_md || a.skill;
    let proj = |name, rel: &[&str], kind| {
        let mut p = base.clone();
        for r in rel {
            p.push(r);
        }
        Target { name, path: Some(p), kind, root: Some(root.clone()) }
    };
    let mut targets = Vec::new();
    if a.agents_md || !any_project {
        targets.push(proj("agents-md", &["AGENTS.md"], Kind::Block));
    }
    if a.claude_md || !any_project {
        targets.push(proj("claude-md", &["CLAUDE.md"], Kind::Block));
    }
    if a.skill || !any_project {
        targets.push(proj("skill", &[".claude", "skills", "agentgc", "SKILL.md"], Kind::Whole));
    }
    if a.global_skill {
        let home = homes_from(ctx.get).into_iter().next().filter(|h| h.is_dir());
        let path = home.map(|h| h.join(".claude").join("skills").join("agentgc").join("SKILL.md"));
        targets.push(Target { name: "global-skill", path, kind: Kind::Whole, root: None });
    }

    let plans: Vec<Plan> = targets.iter().map(plan_target).collect();
    let total = plans.len();
    for (i, p) in plans.iter().enumerate() {
        let shown = p.path.as_ref().map(|x| x.display().to_string()).unwrap_or_else(|| "(unknown)".into());
        w!(out);
        w!(out, "[{}/{total}] {}  {shown}", i + 1, p.name);
        w!(out, "  {}", describe(p));
    }
    w!(out);
    if ctx.interactive && !a.yes_target.is_empty() {
        w!(out, "Note: --yes-target applies only when stdin is not a terminal. Every target is asked.");
    }
    if !ctx.interactive && plans.iter().any(Plan::writable) {
        w!(out, "stdin is not a terminal: nothing is asked; only targets named with --yes-target are written.");
    }

    let mut results: Vec<Outcome> = Vec::new();
    let mut stopped = false;
    for (p, t) in plans.iter().zip(&targets) {
        let name = p.name;
        let result = match &p.action {
            Action::UpToDate => "up-to-date",
            Action::Refuse(_) => "refused",
            _ if stopped => "skipped",
            _ => {
                let yes = if ctx.interactive {
                    let shown = p.path.as_ref().map(|x| x.display().to_string()).unwrap_or_default();
                    w!(out, "{name}: {shown}");
                    let _ = write!(out, "Write this? [y/N/q] ");
                    let _ = out.flush();
                    match read_answer(input) {
                        None => {
                            w!(out);
                            stopped = true;
                            false
                        }
                        Some(ans) => match ans.trim() {
                            "y" => true,
                            "q" => {
                                stopped = true;
                                false
                            }
                            _ => false,
                        },
                    }
                } else {
                    a.yes_target.iter().any(|n| n == name)
                };
                if !yes {
                    "skipped"
                } else {
                    // plan again right before the write: the file may have changed meanwhile
                    let again = plan_target(t);
                    if again.action != p.action || again.new != p.new {
                        w!(out, "  REFUSED: the file changed since the plan was shown. Nothing was written.");
                        "refused"
                    } else {
                        match apply(&again) {
                            Ok(()) => {
                                w!(out, "  Written.");
                                "written"
                            }
                            Err(e) => {
                                w!(out, "  ERROR: {e}");
                                "failed"
                            }
                        }
                    }
                }
            }
        };
        results.push(Outcome { name, result });
    }

    w!(out);
    for r in &results {
        w!(out, "{:<12}  {}", r.name, r.result);
    }
    if results.iter().any(|r| r.result == "written") {
        w!(out, "Next step: restart your agent or open a new session so it re-reads the rules.");
    } else if results.iter().any(|r| r.result == "skipped") {
        w!(out, "Nothing was written. Run again in a terminal and answer y, or name targets with --yes-target NAME.");
    } else {
        w!(out, "Nothing was written.");
    }
    Ok(results)
}

pub fn cmd_init(a: &InitArgs) -> Result<(), String> {
    use std::io::IsTerminal;
    let get = |k: &str| std::env::var(k).ok();
    let ctx = Ctx { get: &get, interactive: std::io::stdin().is_terminal() };
    let stdin = std::io::stdin();
    let mut input = stdin.lock();
    let mut out = std::io::stdout();
    let res = run_init(&ctx, a, &mut input, &mut out)?;
    let failed = res.iter().filter(|r| r.result == "failed").count();
    if failed > 0 {
        return Err(format!("{failed} target(s) failed"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-init-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn args(dir: &Path) -> InitArgs {
        InitArgs { dir: Some(dir.to_path_buf()), ..Default::default() }
    }

    fn go(
        a: &InitArgs,
        home: Option<&Path>,
        interactive: bool,
        input: &str,
    ) -> (Result<Vec<Outcome>, String>, String) {
        let h = home.map(|p| p.to_string_lossy().into_owned());
        let get = move |k: &str| if k == "HOME" { h.clone() } else { None };
        let ctx = Ctx { get: &get, interactive };
        let mut out: Vec<u8> = Vec::new();
        let r = run_init(&ctx, a, &mut Cursor::new(input.as_bytes().to_vec()), &mut out);
        (r, String::from_utf8_lossy(&out).into_owned())
    }

    fn entries(d: &Path) -> usize {
        fs::read_dir(d).unwrap().count()
    }

    fn skill_path(d: &Path) -> PathBuf {
        d.join(".claude/skills/agentgc/SKILL.md")
    }

    fn result_of(r: &[Outcome], name: &str) -> &'static str {
        r.iter().find(|o| o.name == name).unwrap().result
    }

    #[test]
    fn non_terminal_without_yes_target_writes_nothing() {
        let d = tmp("dry");
        let (r, out) = go(&args(&d), None, false, "y\ny\ny\n");
        let r = r.unwrap();
        assert_eq!(entries(&d), 0);
        assert!(r.iter().all(|o| o.result == "skipped"));
        assert!(out.contains("would create the file"));
        assert!(out.contains("not a terminal"));
        assert!(out.contains("Nothing was written"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn interactive_creates_all_three_project_targets() {
        let d = tmp("create");
        let (r, out) = go(&args(&d), None, true, "y\ny\ny\n");
        let r = r.unwrap();
        assert!(r.iter().all(|o| o.result == "written"), "{r:?}");
        for f in ["AGENTS.md", "CLAUDE.md"] {
            let t = fs::read_to_string(d.join(f)).unwrap();
            assert_eq!(t, block());
            assert!(t.starts_with(BEGIN) && t.trim_end().ends_with(END));
        }
        assert_eq!(fs::read_to_string(skill_path(&d)).unwrap(), lf(SKILL_RAW));
        assert!(out.contains("restart your agent"));
        assert_eq!(out.matches("Write this? [y/N/q]").count(), 3);
        assert!(!d.join("AGENTS.md.agentgc.bak").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn append_keeps_text_and_makes_a_backup() {
        let d = tmp("append");
        fs::write(d.join("AGENTS.md"), "my rules\nline two").unwrap();
        let mut a = args(&d);
        a.agents_md = true;
        let (r, _) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "agents-md"), "written");
        let t = fs::read_to_string(d.join("AGENTS.md")).unwrap();
        assert_eq!(t, format!("my rules\nline two\n\n{}", block()));
        assert_eq!(fs::read_to_string(d.join("AGENTS.md.agentgc.bak")).unwrap(), "my rules\nline two");
        assert!(!d.join(".AGENTS.md.agentgc.tmp").exists());
        assert!(!d.join("CLAUDE.md").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn second_run_is_up_to_date_and_asks_nothing() {
        let d = tmp("twice");
        go(&args(&d), None, true, "y\ny\ny\n").0.unwrap();
        let before = fs::read_to_string(d.join("AGENTS.md")).unwrap();
        let (r, out) = go(&args(&d), None, true, "y\ny\ny\n");
        let r = r.unwrap();
        assert!(r.iter().all(|o| o.result == "up-to-date"), "{r:?}");
        assert!(out.contains("already up to date"));
        assert!(!out.contains("Write this?"));
        assert_eq!(fs::read_to_string(d.join("AGENTS.md")).unwrap(), before);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn update_replaces_only_the_block() {
        let d = tmp("update");
        let original = format!("top\n{BEGIN}\nold text\n{END}\nbottom\n");
        fs::write(d.join("CLAUDE.md"), &original).unwrap();
        let mut a = args(&d);
        a.claude_md = true;
        let (r, out) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "claude-md"), "written");
        assert!(out.contains("would update the block"));
        let t = fs::read_to_string(d.join("CLAUDE.md")).unwrap();
        assert_eq!(t, format!("top\n{BEGIN}\n{}\n{END}\nbottom\n", snippet()));
        assert_eq!(fs::read_to_string(d.join("CLAUDE.md.agentgc.bak")).unwrap(), original);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unbalanced_or_duplicated_markers_refuse() {
        for (i, body) in [
            format!("a\n{BEGIN}\nb\n"),
            format!("a\n{END}\nb\n"),
            format!("{END}\n{BEGIN}\n"),
            format!("{BEGIN}\nx\n{END}\n{BEGIN}\ny\n{END}\n"),
        ]
        .iter()
        .enumerate()
        {
            let d = tmp(&format!("unbal{i}"));
            fs::write(d.join("AGENTS.md"), body).unwrap();
            let mut a = args(&d);
            a.agents_md = true;
            let (r, out) = go(&a, None, true, "y\n");
            assert_eq!(result_of(&r.unwrap(), "agents-md"), "refused", "{body:?}");
            assert!(out.contains("REFUSED"));
            assert_eq!(&fs::read_to_string(d.join("AGENTS.md")).unwrap(), body);
            assert!(!d.join("AGENTS.md.agentgc.bak").exists());
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn existing_backup_refuses() {
        let d = tmp("bak");
        fs::write(d.join("AGENTS.md"), "mine\n").unwrap();
        fs::write(d.join("AGENTS.md.agentgc.bak"), "older backup").unwrap();
        let mut a = args(&d);
        a.agents_md = true;
        let (r, out) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "agents-md"), "refused");
        assert!(out.contains("backup") && out.contains("already exists"));
        assert_eq!(fs::read_to_string(d.join("AGENTS.md")).unwrap(), "mine\n");
        assert_eq!(fs::read_to_string(d.join("AGENTS.md.agentgc.bak")).unwrap(), "older backup");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn differing_skill_is_refused_and_equal_skill_is_up_to_date() {
        let d = tmp("skill");
        fs::create_dir_all(skill_path(&d).parent().unwrap()).unwrap();
        fs::write(skill_path(&d), "my own skill").unwrap();
        let mut a = args(&d);
        a.skill = true;
        let (r, out) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "skill"), "refused");
        assert!(out.contains("differs"));
        assert_eq!(fs::read_to_string(skill_path(&d)).unwrap(), "my own skill");
        fs::write(skill_path(&d), lf(SKILL_RAW)).unwrap();
        let (r, _) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "skill"), "up-to-date");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn yes_target_writes_only_the_named_target() {
        let d = tmp("yes");
        let mut a = args(&d);
        a.yes_target = vec!["agents-md".into()];
        let (r, out) = go(&a, None, false, "");
        let r = r.unwrap();
        assert_eq!(result_of(&r, "agents-md"), "written");
        assert_eq!(result_of(&r, "claude-md"), "skipped");
        assert!(d.join("AGENTS.md").exists());
        assert!(!d.join("CLAUDE.md").exists());
        assert!(!skill_path(&d).exists());
        assert!(out.contains("restart your agent"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_target_name_is_an_error() {
        let d = tmp("unknown");
        let mut a = args(&d);
        a.yes_target = vec!["agents-md".into(), "bogus".into()];
        let e = go(&a, None, false, "").0.unwrap_err();
        assert!(e.contains("bogus"), "{e}");
        assert_eq!(entries(&d), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn terminal_ignores_yes_target_and_prints_a_note() {
        let d = tmp("tty");
        let mut a = args(&d);
        a.yes_target = vec!["agents-md".into()];
        let (r, out) = go(&a, None, true, "n\nn\nn\n");
        assert!(r.unwrap().iter().all(|o| o.result == "skipped"));
        assert!(out.contains("Note: --yes-target applies only when stdin is not a terminal"));
        assert_eq!(entries(&d), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn q_and_end_of_input_stop_the_run() {
        let d = tmp("q");
        let (r, _) = go(&args(&d), None, true, "y\nq\ny\n");
        let r = r.unwrap();
        assert_eq!(result_of(&r, "agents-md"), "written");
        assert_eq!(result_of(&r, "claude-md"), "skipped");
        assert_eq!(result_of(&r, "skill"), "skipped");
        assert!(!d.join("CLAUDE.md").exists());
        let d2 = tmp("eof");
        let (r, out) = go(&args(&d2), None, true, "");
        assert!(r.unwrap().iter().all(|o| o.result == "skipped"));
        assert_eq!(out.matches("Write this?").count(), 1);
        assert_eq!(entries(&d2), 0);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&d2);
    }

    #[test]
    fn print_shows_the_snippet_and_writes_nothing() {
        let d = tmp("print");
        let mut a = args(&d);
        a.print = true;
        a.yes_target = vec!["agents-md".into()];
        let (r, out) = go(&a, None, false, "");
        assert!(r.unwrap().is_empty());
        assert_eq!(out, format!("{}\n", snippet()));
        assert_eq!(entries(&d), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn global_skill_goes_to_the_injected_home_only() {
        let d = tmp("g-proj");
        let h = tmp("g-home");
        let mut a = args(&d);
        a.global_skill = true;
        a.yes_target = vec!["global-skill".into()];
        let (r, _) = go(&a, Some(&h), false, "");
        let r = r.unwrap();
        assert_eq!(result_of(&r, "global-skill"), "written");
        assert_eq!(fs::read_to_string(skill_path(&h)).unwrap(), lf(SKILL_RAW));
        assert_eq!(entries(&d), 0);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn global_skill_with_unknown_home_is_refused() {
        let d = tmp("g-nohome");
        let mut a = args(&d);
        a.global_skill = true;
        a.yes_target = vec!["global-skill".into()];
        let (r, out) = go(&a, None, false, "");
        assert_eq!(result_of(&r.unwrap(), "global-skill"), "refused");
        assert!(out.contains("home dir is unknown"));
        assert_eq!(entries(&d), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn dir_must_exist_and_be_a_folder() {
        let d = tmp("dir");
        let missing = args(&d.join("nope"));
        assert!(go(&missing, None, false, "").0.is_err());
        fs::write(d.join("file"), "x").unwrap();
        assert!(go(&args(&d.join("file")), None, false, "").0.unwrap_err().contains("not a folder"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn folder_target_is_refused() {
        let d = tmp("folder");
        fs::create_dir_all(d.join("AGENTS.md")).unwrap();
        let mut a = args(&d);
        a.agents_md = true;
        let (r, out) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "agents-md"), "refused");
        assert!(out.contains("is a folder"));
        let _ = fs::remove_dir_all(&d);
    }

    fn try_symlink(target: &Path, link: &Path, dir: bool) -> bool {
        #[cfg(unix)]
        {
            let _ = dir;
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            if dir {
                std::os::windows::fs::symlink_dir(target, link).is_ok()
            } else {
                std::os::windows::fs::symlink_file(target, link).is_ok()
            }
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link, dir);
            false
        }
    }

    #[test]
    fn symlink_target_is_refused() {
        let d = tmp("link");
        let real = d.join("real.txt");
        fs::write(&real, "keep").unwrap();
        if !try_symlink(&real, &d.join("AGENTS.md"), false) {
            eprintln!("skipped: cannot create a symlink here");
            let _ = fs::remove_dir_all(&d);
            return;
        }
        let mut a = args(&d);
        a.agents_md = true;
        let (r, out) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "agents-md"), "refused");
        assert!(out.contains("symlink"));
        assert_eq!(fs::read_to_string(&real).unwrap(), "keep");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn parent_symlink_leading_outside_dir_is_refused() {
        let d = tmp("plink");
        let outside = tmp("plink-out");
        if !try_symlink(&outside, &d.join(".claude"), true) {
            eprintln!("skipped: cannot create a symlink here");
            let _ = fs::remove_dir_all(&d);
            let _ = fs::remove_dir_all(&outside);
            return;
        }
        let mut a = args(&d);
        a.skill = true;
        let (r, out) = go(&a, None, true, "y\n");
        assert_eq!(result_of(&r.unwrap(), "skill"), "refused");
        assert!(out.contains("outside --dir"));
        assert_eq!(entries(&outside), 0);
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&outside);
    }
}
