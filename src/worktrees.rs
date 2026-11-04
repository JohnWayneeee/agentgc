use serde::Serialize;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Directories never entered while walking the roots.
const SKIP_DIRS: [&str; 5] = ["node_modules", ".git", "target", ".next", ".venv"];

#[derive(Debug, Serialize, PartialEq)]
pub struct Orphan {
    pub path: String,
    /// Total size, node_modules included.
    pub bytes: u64,
    /// Size of the top-level `node_modules`, if present.
    pub node_modules_bytes: Option<u64>,
    /// Regular files, node_modules excluded.
    pub files: u64,
    /// Age in days of the newest file (node_modules excluded). None when no files.
    pub newest_age_days: Option<u64>,
    pub missing_gitdir: String,
}

/// If `dir/.git` is a FILE whose first line is `gitdir: <path>` and `<path>` does
/// not exist, return that path. Relative paths resolve against `dir`.
pub fn missing_gitdir(dir: &Path) -> Option<String> {
    let git = dir.join(".git");
    let meta = fs::symlink_metadata(&git).ok()?;
    if !meta.is_file() {
        return None;
    }
    let bytes = fs::read(&git).ok()?;
    let head = &bytes[..bytes.len().min(4096)];
    let text = String::from_utf8_lossy(head);
    let line = text.lines().next()?;
    let target = line.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let p = Path::new(target);
    let full = if p.is_absolute() { p.to_path_buf() } else { dir.join(p) };
    if full.exists() {
        None
    } else {
        Some(target.to_string())
    }
}

struct Measure {
    bytes: u64,
    nm_bytes: Option<u64>,
    files: u64,
    newest: Option<SystemTime>,
}

/// Measure an orphan. Symlinks are not followed. Unreadable entries are skipped.
fn measure(dir: &Path) -> Measure {
    let mut m = Measure { bytes: 0, nm_bytes: None, files: 0, newest: None };
    let mut stack = vec![(dir.to_path_buf(), true)];
    while let Some((d, top)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for entry in rd.flatten() {
            let p = entry.path();
            let Ok(meta) = fs::symlink_metadata(&p) else { continue };
            if meta.is_dir() {
                if top && entry.file_name() == "node_modules" {
                    let n = crate::scan::dir_size(&p);
                    m.nm_bytes = Some(n);
                    m.bytes += n;
                } else {
                    stack.push((p, false));
                }
            } else if meta.is_file() {
                m.bytes += meta.len();
                m.files += 1;
                if let Ok(t) = meta.modified() {
                    if m.newest.map_or(true, |n| t > n) {
                        m.newest = Some(t);
                    }
                }
            }
        }
    }
    m
}

/// Walk `roots` down to `depth` levels and return orphan worktrees, biggest first.
pub fn find_orphans(roots: &[PathBuf], depth: usize) -> Vec<Orphan> {
    let now = SystemTime::now();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    for root in roots {
        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((dir, level)) = stack.pop() {
            match fs::symlink_metadata(&dir) {
                Ok(m) if m.is_dir() => {}
                _ => continue,
            }
            if !seen.insert(dir.clone()) {
                continue;
            }
            if let Some(missing) = missing_gitdir(&dir) {
                let m = measure(&dir);
                out.push(Orphan {
                    path: dir.display().to_string(),
                    bytes: m.bytes,
                    node_modules_bytes: m.nm_bytes,
                    files: m.files,
                    newest_age_days: m
                        .newest
                        .map(|t| now.duration_since(t).map(|d| d.as_secs() / 86400).unwrap_or(0)),
                    missing_gitdir: missing,
                });
                continue;
            }
            if level >= depth {
                continue;
            }
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for entry in rd.flatten() {
                let name = entry.file_name();
                if SKIP_DIRS.iter().any(|s| name == *s) {
                    continue;
                }
                if let Ok(m) = fs::symlink_metadata(entry.path()) {
                    if m.is_dir() {
                        stack.push((entry.path(), level + 1));
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-wt-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn orphan(root: &Path, name: &str) -> PathBuf {
        let d = root.join(name);
        fs::create_dir_all(&d).unwrap();
        let gone = root.join("gone").join(name);
        fs::write(d.join(".git"), format!("gitdir: {}\n", gone.display())).unwrap();
        d
    }

    #[test]
    fn lists_orphan_only() {
        let r = tmp("lists");
        let o = orphan(&r, "orph");
        fs::write(o.join("a.txt"), vec![0u8; 100]).unwrap();
        // healthy worktree: gitdir exists
        let real = r.join("realgit");
        fs::create_dir_all(&real).unwrap();
        let h = r.join("healthy");
        fs::create_dir_all(&h).unwrap();
        fs::write(h.join(".git"), format!("gitdir: {}\n", real.display())).unwrap();
        // plain repo: .git is a directory
        let p = r.join("plain");
        fs::create_dir_all(p.join(".git")).unwrap();
        let found = find_orphans(&[r.clone()], 6);
        assert_eq!(found.len(), 1);
        assert!(found[0].path.ends_with("orph"));
        assert_eq!(found[0].files, 2); // a.txt + .git file
        assert!(found[0].missing_gitdir.contains("gone"));
        assert!(found[0].newest_age_days.is_some());
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn reports_node_modules_size() {
        let r = tmp("nm");
        let o = orphan(&r, "orph");
        let git_len = fs::metadata(o.join(".git")).unwrap().len();
        fs::write(o.join("a.txt"), vec![0u8; 100]).unwrap();
        fs::create_dir_all(o.join("node_modules/pkg")).unwrap();
        fs::write(o.join("node_modules/pkg/x.js"), vec![0u8; 1000]).unwrap();
        let f = find_orphans(&[r.clone()], 6);
        assert_eq!(f[0].node_modules_bytes, Some(1000));
        assert_eq!(f[0].files, 2); // a.txt + .git, node_modules excluded
        assert_eq!(f[0].bytes, 100 + 1000 + git_len);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn relative_gitdir_and_depth_and_skip() {
        let r = tmp("rel");
        // relative gitdir that exists: healthy
        let h = r.join("h");
        fs::create_dir_all(r.join("h/real")).unwrap();
        fs::write(h.join(".git"), "gitdir: real\n").unwrap();
        // orphan inside node_modules must be skipped
        let nm = r.join("node_modules");
        fs::create_dir_all(&nm).unwrap();
        orphan(&nm, "hidden");
        // orphan too deep
        let deep = r.join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        orphan(&deep, "deep");
        assert!(find_orphans(&[r.clone()], 6).len() == 1); // only "deep"
        assert!(find_orphans(&[r.clone()], 2).is_empty());
        let _ = fs::remove_dir_all(&r);
    }
}
