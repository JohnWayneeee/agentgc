//! `agentgc doctor`: read-only search for hard-coded ports and database names in tracked text files.
use crate::out::outln;
use crate::task::{self, DEFAULT_CONFIG};
use crate::task_config as tc;
use std::path::{Path, PathBuf};

const MAX_FILE_BYTES: u64 = 1_000_000;
const DB_HINTS: &[&str] = &[
    "database", "db_name", "dbname", "db=", "createdb", "dropdb", "postgres://", "postgresql://",
    "mysql://", "mongodb://", "sqlite:", "db_url",
];

pub struct DoctorArgs {
    pub repo: Option<PathBuf>,
    pub config: Option<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    pub kind: &'static str,
    pub what: String,
    pub text: String,
}

/// A path is skipped when any folder is `target` or `node_modules`, or it is the task config itself.
pub fn skip_path(rel: &str) -> bool {
    rel == DEFAULT_CONFIG || rel.split(['/', '\\']).any(|c| c == "target" || c == "node_modules")
}

/// Ports from `range` that appear as a whole number on the line, after `:` or on a line that says "port".
pub fn find_ports(line: &str, range: (u16, u16)) -> Vec<u16> {
    let b = line.as_bytes();
    let has_port_word = line.to_ascii_lowercase().contains("port");
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if !b[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let s = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        let before = if s > 0 { b[s - 1] } else { b' ' };
        let after = if i < b.len() { b[i] } else { b' ' };
        // not part of a longer token, a version (1.3100) or a decimal (3100.5)
        if before.is_ascii_alphanumeric() || before == b'_' || before == b'.' || before == b'-' {
            continue;
        }
        let decimal = after == b'.' && b.get(i + 1).is_some_and(|c| c.is_ascii_digit());
        if after.is_ascii_alphanumeric() || after == b'_' || decimal {
            continue;
        }
        if i - s > 5 {
            continue;
        }
        if let Ok(n) = line[s..i].parse::<u16>() {
            if n >= range.0 && n <= range.1 && (before == b':' || has_port_word) && !out.contains(&n) {
                out.push(n);
            }
        }
    }
    out
}

/// True when the line has a database hint and `base` as a whole word or as `base_suffix`.
pub fn has_db_name(line: &str, base: &str) -> bool {
    if base.is_empty() {
        return false;
    }
    let low = line.to_ascii_lowercase();
    if !DB_HINTS.iter().any(|h| low.contains(h)) {
        return false;
    }
    let lb = low.as_bytes();
    let mut from = 0;
    while let Some(p) = low[from..].find(base) {
        let s = from + p;
        let e = s + base.len();
        let before_ok = s == 0 || !(lb[s - 1].is_ascii_alphanumeric() || lb[s - 1] == b'_' || lb[s - 1] == b'-');
        let after_ok = e == lb.len() || !lb[e].is_ascii_alphanumeric();
        let suffix_ok = e < lb.len() && lb[e] == b'_';
        if before_ok && (after_ok || suffix_ok) {
            return true;
        }
        from = e;
    }
    false
}

pub fn scan_text(file: &str, text: &str, range: (u16, u16), db_base: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, l) in text.lines().enumerate() {
        let shown: String = l.trim().chars().take(120).collect();
        for p in find_ports(l, range) {
            hits.push(Hit { file: file.into(), line: i + 1, kind: "port", what: p.to_string(), text: shown.clone() });
        }
        if has_db_name(l, db_base) {
            hits.push(Hit { file: file.into(), line: i + 1, kind: "db", what: db_base.into(), text: shown.clone() });
        }
    }
    hits
}

fn tracked_files(root: &Path) -> Result<Vec<String>, String> {
    let o = task::git(root, &["ls-files", "-z"])?;
    if !o.status.success() {
        return Err(format!("git ls-files failed: {}", String::from_utf8_lossy(&o.stderr).trim()));
    }
    Ok(o.stdout
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect())
}

/// Text of a file, or `None` for binary, non-UTF-8, unreadable or large files.
fn read_text(p: &Path) -> Option<String> {
    let m = std::fs::metadata(p).ok()?;
    if !m.is_file() || m.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(p).ok()?;
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

pub fn run_doctor(root: &Path, config: Option<&Path>) -> Result<Vec<Hit>, String> {
    let (cfg, _) = task::load_config(root, config)?;
    let range = (cfg.ports.range[0], cfg.ports.range[1]);
    let name = root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let db_base = tc::sanitize_repo_db(&name);
    let mut hits = Vec::new();
    for rel in tracked_files(root)? {
        if skip_path(&rel) {
            continue;
        }
        if let Some(t) = read_text(&root.join(&rel)) {
            hits.extend(scan_text(&rel, &t, range, &db_base));
        }
    }
    Ok(hits)
}

/// Work tree root by `git rev-parse --show-toplevel` (works in a linked worktree, where `.git` is a file).
/// Falls back to `find_repo_root` when git cannot tell.
fn resolve_root(start: &Path) -> Result<PathBuf, String> {
    if let Ok(o) = task::git(start, &["rev-parse", "--show-toplevel"]) {
        if o.status.success() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return Ok(PathBuf::from(s));
            }
        }
    }
    task::find_repo_root(start)
}

pub fn cmd_doctor(a: &DoctorArgs) -> Result<(), String> {
    let root = resolve_root(a.repo.as_deref().unwrap_or(Path::new(".")))?;
    let hits = run_doctor(&root, a.config.as_deref())?;
    for h in &hits {
        outln!("{}:{}  {} {}  {}", h.file, h.line, h.kind, h.what, h.text);
    }
    if hits.is_empty() {
        outln!("No hard-coded ports or database names found.");
    } else {
        outln!(
            "{} places. Use the PORT_* and DB_NAME values from .agentgc/task.env instead. Nothing was changed.",
            hits.len()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_port_after_colon_and_port_word() {
        assert_eq!(find_ports("http://localhost:3100/x", (3100, 3999)), vec![3100]);
        assert_eq!(find_ports("PORT=3200", (3100, 3999)), vec![3200]);
    }

    #[test]
    fn ignores_other_numbers() {
        assert!(find_ports("width 3200 px", (3100, 3999)).is_empty());
        assert!(find_ports("port 4000", (3100, 3999)).is_empty());
        assert!(find_ports("port 13200 v1.3200", (3100, 3999)).is_empty());
        assert!(find_ports("port 3200.5", (3100, 3999)).is_empty());
    }

    #[test]
    fn finds_db_name() {
        assert!(has_db_name("DATABASE_URL=postgres://u@h/myapp_dev", "myapp"));
        assert!(has_db_name("createdb myapp", "myapp"));
        assert!(!has_db_name("DATABASE_URL=postgres://u@h/myapplication", "myapp"));
        assert!(!has_db_name("let myapp = 1", "myapp"));
    }

    #[test]
    fn skips_dirs_and_config() {
        assert!(skip_path("target/debug/x.rs"));
        assert!(skip_path("web/node_modules/a/b.js"));
        assert!(skip_path("agentgc.task.toml"));
        assert!(!skip_path("src/target_x.rs"));
    }

    #[test]
    fn scan_text_reports_lines() {
        let h = scan_text("a.env", "A=1\nPORT=3300\ndatabase: myapp\n", (3100, 3999), "myapp");
        assert_eq!(h.len(), 2);
        assert_eq!((h[0].line, h[0].kind), (2, "port"));
        assert_eq!((h[1].line, h[1].kind), (3, "db"));
    }

    #[test]
    fn binary_is_skipped() {
        let d = std::env::temp_dir().join(format!("agentgc_doctor_{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("b.bin");
        std::fs::write(&p, b"\0PORT=3300").unwrap();
        assert!(read_text(&p).is_none());
        let t = d.join("t.txt");
        std::fs::write(&t, "PORT=3300").unwrap();
        assert!(read_text(&t).is_some());
        std::fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn resolves_root_in_linked_worktree() {
        use std::process::Command;
        let git = |dir: &Path, args: &[&str]| {
            let o = Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        if Command::new("git").arg("--version").output().is_err() {
            return;
        }
        let d = std::env::temp_dir().join(format!("agentgc_doctor_wt_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let main = d.join("main");
        let wt = d.join("wt");
        std::fs::create_dir_all(&main).unwrap();
        git(&main, &["init", "-b", "main"]);
        std::fs::write(main.join("a.env"), "PORT=3300
").unwrap();
        git(&main, &["add", "."]);
        git(&main, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-m", "init"]);
        git(&main, &["worktree", "add", wt.to_str().unwrap(), "-b", "feat"]);
        assert!(wt.join(".git").is_file());
        assert!(task::find_repo_root(&wt).is_err() || task::find_repo_root(&wt).unwrap() != wt);
        let sub = wt.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        for start in [&wt, &sub] {
            let root = resolve_root(start).unwrap();
            assert_eq!(root.canonicalize().unwrap(), wt.canonicalize().unwrap());
        }
        let hits = run_doctor(&resolve_root(&wt).unwrap(), None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].file, "a.env");
        let _ = std::fs::remove_dir_all(&d);
    }
}
