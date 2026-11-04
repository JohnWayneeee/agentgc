//! `agentgc task review NAME`: short read-only summary of one task for the human reviewer.
//! Only `git diff`, `git log`, `git rev-list` and `git status` are run. Nothing is written.
use crate::out::outln;
use crate::task::{self, Row};
use serde::Serialize;
use std::path::{Path, PathBuf};

const MAX_COMMITS: usize = 5;
const MAX_TEXT_FILES: usize = 20;

#[derive(Debug, Serialize, PartialEq)]
pub struct FileStat {
    pub path: String,
    /// `null` for a binary file.
    pub added: Option<u64>,
    pub deleted: Option<u64>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
}

#[derive(Debug, Serialize)]
pub struct Review {
    pub name: String,
    pub branch: String,
    pub base: String,
    pub path: String,
    pub status: Vec<String>,
    pub state: String,
    /// Uncommitted files. `null`: folder missing or git failed.
    pub changed: Option<u64>,
    pub ahead: Option<u64>,
    pub behind: Option<u64>,
    /// Files that differ between the merge base and the task branch (committed work only).
    pub files_changed: Option<u64>,
    pub insertions: Option<u64>,
    pub deletions: Option<u64>,
    pub files: Vec<FileStat>,
    /// Newest first, at most 5.
    pub commits: Vec<Commit>,
    /// A hint only: whether the base has moved since the task branch started.
    pub merge_hint: String,
}

/// Parse `git diff --numstat` output. Binary files print `-\t-\tpath`.
pub fn parse_numstat(text: &str) -> Vec<FileStat> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.splitn(3, '\t');
            let (a, d, p) = (it.next()?, it.next()?, it.next()?);
            Some(FileStat { path: p.to_string(), added: a.parse().ok(), deleted: d.parse().ok() })
        })
        .collect()
}

pub fn parse_log(text: &str) -> Vec<Commit> {
    text.lines()
        .filter_map(|l| {
            let (h, s) = l.split_once('\t')?;
            Some(Commit { hash: h.to_string(), subject: s.to_string() })
        })
        .collect()
}

fn git_text(path: &Path, args: &[&str]) -> Option<String> {
    let o = task::git(path, args).ok()?;
    if !o.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&o.stdout).into_owned())
}

fn hint(ahead: Option<u64>, behind: Option<u64>) -> String {
    match (ahead, behind) {
        (Some(0), _) => "nothing to merge".into(),
        (Some(_), Some(0)) => "ready to merge: the base has no new commits".into(),
        (Some(_), Some(b)) => format!("rebase or merge the base first: it has {b} new commit(s)"),
        _ => "unknown".into(),
    }
}

pub fn build(row: Row) -> Review {
    let path = PathBuf::from(&row.entry.path);
    let missing = row.missing();
    let branch_ref = format!("refs/heads/{}", row.entry.branch);
    let mut files = Vec::new();
    let mut files_changed = None;
    let mut commits = Vec::new();
    if !missing {
        let range = format!("{}...{}", row.entry.base, branch_ref);
        if let Some(t) = git_text(&path, &["diff", "--numstat", "--no-renames", &range, "--"]) {
            files = parse_numstat(&t);
            files_changed = Some(files.len() as u64);
        }
        let range = format!("{}..{}", row.entry.base, branch_ref);
        let n = format!("-n{MAX_COMMITS}");
        if let Some(t) = git_text(&path, &["log", "--format=%h%x09%s", &n, &range, "--"]) {
            commits = parse_log(&t);
        }
    }
    let ins = files_changed.map(|_| files.iter().filter_map(|f| f.added).sum());
    let del = files_changed.map(|_| files.iter().filter_map(|f| f.deleted).sum());
    let state = if row.entry.is_incomplete() { "incomplete" } else { "ready" };
    Review {
        name: row.entry.name.clone(),
        branch: row.entry.branch.clone(),
        base: row.entry.base.clone(),
        path: row.entry.path.clone(),
        status: row.status.clone(),
        state: state.into(),
        changed: row.changed,
        ahead: row.ahead,
        behind: row.behind,
        files_changed,
        insertions: ins,
        deletions: del,
        files,
        commits,
        merge_hint: hint(row.ahead, row.behind),
    }
}

fn num(v: Option<u64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "?".into())
}

pub fn render(r: &Review) -> String {
    let mut s = format!(
        "{}  [{}]  state: {}\n    branch: {} (base {})\n    ahead/behind: {}/{}   uncommitted files: {}\n    diff vs base: {} files, +{} -{}\n",
        r.name,
        r.status.join(","),
        r.state,
        r.branch,
        r.base,
        num(r.ahead),
        num(r.behind),
        num(r.changed),
        num(r.files_changed),
        num(r.insertions),
        num(r.deletions),
    );
    for f in r.files.iter().take(MAX_TEXT_FILES) {
        let (a, d) = match (f.added, f.deleted) {
            (Some(a), Some(d)) => (format!("+{a}"), format!("-{d}")),
            _ => ("bin".into(), String::new()),
        };
        s.push_str(&format!("      {a:>6} {d:>6}  {}\n", f.path));
    }
    if r.files.len() > MAX_TEXT_FILES {
        s.push_str(&format!("      ... and {} more (see --json)\n", r.files.len() - MAX_TEXT_FILES));
    }
    s.push_str("    last commits:\n");
    if r.commits.is_empty() {
        s.push_str("      -\n");
    }
    for c in &r.commits {
        s.push_str(&format!("      {}  {}\n", c.hash, c.subject));
    }
    s.push_str(&format!("    merge: {}\n", r.merge_hint));
    s
}

pub fn cmd_review(name: &str, repo: Option<PathBuf>, json: bool) -> Result<(), String> {
    let root = task::find_repo_root(repo.as_deref().unwrap_or(Path::new(".")))?;
    let entry = task::read_registry(&root)
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("no task named '{name}' (see `agentgc task list`)"))?;
    let r = build(task::row_for(entry));
    if json {
        outln!("{}", serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?);
    } else {
        outln!("{}Nothing was changed.", render(&r));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numstat_parses_text_and_binary() {
        let v = parse_numstat("3\t1\tsrc/a.rs\n-\t-\timg.png\n");
        assert_eq!(v.len(), 2);
        assert_eq!((v[0].added, v[0].deleted, v[0].path.as_str()), (Some(3), Some(1), "src/a.rs"));
        assert_eq!((v[1].added, v[1].deleted), (None, None));
    }

    #[test]
    fn log_parses_subjects_with_tabs() {
        let v = parse_log("abc1234\tfix: a\tb\n");
        assert_eq!(v, vec![Commit { hash: "abc1234".into(), subject: "fix: a\tb".into() }]);
    }

    #[test]
    fn hint_cases() {
        assert_eq!(hint(Some(0), Some(2)), "nothing to merge");
        assert!(hint(Some(1), Some(0)).starts_with("ready"));
        assert!(hint(Some(1), Some(3)).contains("3 new"));
        assert_eq!(hint(None, None), "unknown");
    }
}
