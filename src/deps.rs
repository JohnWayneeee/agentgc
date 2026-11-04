use crate::worktrees::Member;
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Dependency folders measured at each member root.
const DEP_DIRS: [&str; 5] = ["node_modules", ".venv", "vendor/bundle", "target", ".next"];

#[derive(Debug, Serialize, PartialEq)]
pub struct DepSize {
    /// Path relative to the member root, with `/`.
    pub path: String,
    pub bytes: u64,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct MemberDeps {
    pub path: String,
    pub orphan: bool,
    pub total_bytes: u64,
    pub deps: Vec<DepSize>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct RepoDeps {
    pub repo: String,
    pub member_count: usize,
    pub total_bytes: u64,
    /// Members that hold at least one dependency folder.
    pub copies: usize,
    /// Bytes in `node_modules` folders (what a pnpm-style store could hold).
    pub shareable_bytes: u64,
    /// Estimate of disk a shared store would save: files with the same folder, path and
    /// size in 2 or more members count once. Not a hash compare, so it is a rough number.
    pub savings_bytes: u64,
    pub members: Vec<MemberDeps>,
}

fn key(p: &Path) -> String {
    let s = p.display().to_string().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// Folders never entered while looking for dependency folders.
const WALK_SKIP: [&str; 5] = [".git", "node_modules", ".venv", "target", ".next"];

/// Find dependency folders at the root and in subfolders. A dependency folder sits at
/// depth <= `dep_depth` (root children are depth 1). Symlinks are not followed.
/// Other member roots, `.git` and found dependency folders are not entered.
fn member_deps(path: &Path, orphan: bool, dep_depth: usize, others: &HashSet<String>) -> MemberDeps {
    let mut deps = Vec::new();
    let mut found: HashSet<PathBuf> = HashSet::new();
    let mut stack = vec![(path.to_path_buf(), 0usize)];
    while let Some((dir, level)) = stack.pop() {
        if level == 0 || level < dep_depth {
            for d in DEP_DIRS {
                let p = dir.join(d);
                if fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
                    let rel = p
                        .strip_prefix(path)
                        .unwrap_or(&p)
                        .display()
                        .to_string()
                        .replace('\\', "/");
                    deps.push(DepSize { path: rel, bytes: crate::scan::dir_size(&p) });
                    found.insert(p);
                }
            }
        }
        if level + 1 >= dep_depth {
            continue;
        }
        let Ok(rd) = fs::read_dir(&dir) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let name = e.file_name();
            if WALK_SKIP.iter().any(|s| name == *s) || found.contains(&p) {
                continue;
            }
            if !fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
                continue;
            }
            if others.contains(&key(&p)) {
                continue;
            }
            stack.push((p, level + 1));
        }
    }
    deps.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
    let total_bytes = deps.iter().map(|d| d.bytes).sum();
    MemberDeps { path: path.display().to_string(), orphan, total_bytes, deps }
}

fn is_shareable(rel: &str) -> bool {
    rel == "node_modules" || rel.ends_with("/node_modules")
}

/// Record every file under `dir` as (key, size). Symlinks are not followed.
fn collect_files(dir: &Path, prefix: &str, out: &mut Vec<(String, u64)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        let rel = format!("{prefix}/{}", e.file_name().to_string_lossy());
        if m.is_dir() {
            collect_files(&e.path(), &rel, out);
        } else if m.is_file() {
            out.push((rel, m.len()));
        }
    }
}

/// (shareable bytes, estimated savings) for the members of one repo.
fn shared_estimate(members: &[MemberDeps]) -> (u64, u64) {
    let mut seen: BTreeMap<(String, u64), u64> = BTreeMap::new();
    let mut shareable = 0u64;
    for m in members {
        for d in m.deps.iter().filter(|d| is_shareable(&d.path)) {
            shareable += d.bytes;
            let mut files = Vec::new();
            collect_files(&Path::new(&m.path).join(&d.path), &d.path, &mut files);
            for (rel, size) in files {
                *seen.entry((rel, size)).or_insert(0) += 1;
            }
        }
    }
    let savings = seen.iter().map(|((_, size), n)| size * (n - 1)).sum();
    (shareable, savings)
}

/// Group members by repo. Keep repos with 2 or more members. Biggest first.
pub fn duplicate_deps(members: &[Member], dep_depth: usize) -> Vec<RepoDeps> {
    let all: HashSet<String> = members.iter().map(|m| key(&m.path)).collect();
    let mut groups: BTreeMap<String, (String, BTreeMap<String, &Member>)> = BTreeMap::new();
    for m in members {
        let g = groups
            .entry(key(&m.repo))
            .or_insert_with(|| (m.repo.display().to_string(), BTreeMap::new()));
        g.1.entry(key(&m.path)).or_insert(m);
    }
    let mut out = Vec::new();
    for (_, (repo, ms)) in groups {
        if ms.len() < 2 {
            continue;
        }
        let mut members: Vec<MemberDeps> =
            ms.values()
                .map(|m| {
                    let mut others = all.clone();
                    others.remove(&key(&m.path));
                    member_deps(&m.path, m.orphan, dep_depth, &others)
                })
                .collect();
        members.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then_with(|| a.path.cmp(&b.path)));
        let (shareable_bytes, savings_bytes) = shared_estimate(&members);
        out.push(RepoDeps {
            repo,
            shareable_bytes,
            savings_bytes,
            member_count: members.len(),
            total_bytes: members.iter().map(|m| m.total_bytes).sum(),
            copies: members.iter().filter(|m| !m.deps.is_empty()).count(),
            members,
        });
    }
    out.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then_with(|| a.repo.cmp(&b.repo)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worktrees::survey;
    use std::path::PathBuf;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-deps-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn groups_members_and_sizes() {
        let r = tmp("grp");
        let repo = r.join("repo");
        fs::create_dir_all(repo.join(".git/worktrees/w1")).unwrap();
        fs::create_dir_all(repo.join("node_modules/a")).unwrap();
        fs::write(repo.join("node_modules/a/x.js"), vec![0u8; 1000]).unwrap();
        let w = r.join("w1");
        fs::create_dir_all(w.join("node_modules")).unwrap();
        fs::create_dir_all(w.join("target")).unwrap();
        fs::write(w.join("node_modules/y.js"), vec![0u8; 300]).unwrap();
        fs::write(w.join("target/z.o"), vec![0u8; 50]).unwrap();
        fs::write(
            w.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/w1").display()),
        )
        .unwrap();
        // lone repo: 1 member, must not be listed
        fs::create_dir_all(r.join("lone/.git")).unwrap();
        let sv = survey(&[r.clone()], 6);
        let g = duplicate_deps(&sv.members, 3);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].member_count, 2);
        assert_eq!(g[0].total_bytes, 1350);
        assert_eq!(g[0].copies, 2);
        assert_eq!(g[0].members[0].total_bytes, 1000);
        assert_eq!(g[0].members[1].deps[0], DepSize { path: "node_modules".into(), bytes: 300 });
        assert_eq!(g[0].members[1].deps[1].path, "target");
        let _ = fs::remove_dir_all(&r);
    }

    fn main_repo(r: &Path) -> PathBuf {
        let repo = r.join("repo");
        fs::create_dir_all(repo.join(".git/worktrees/w1")).unwrap();
        let w = r.join("w1");
        fs::create_dir_all(&w).unwrap();
        fs::write(w.join(".git"), format!("gitdir: {}\n", repo.join(".git/worktrees/w1").display()))
            .unwrap();
        repo
    }

    #[test]
    fn finds_subfolder_deps_by_depth() {
        let r = tmp("depth");
        let repo = main_repo(&r);
        fs::create_dir_all(repo.join("app/node_modules")).unwrap();
        fs::write(repo.join("app/node_modules/a.js"), vec![0u8; 200]).unwrap();
        fs::create_dir_all(repo.join("a/b/c/node_modules")).unwrap(); // depth 4
        fs::write(repo.join("a/b/c/node_modules/b.js"), vec![0u8; 500]).unwrap();
        let sv = survey(&[r.clone()], 6);
        let g = duplicate_deps(&sv.members, 3);
        let main = g[0].members.iter().find(|m| m.path.ends_with("repo")).unwrap();
        assert_eq!(main.deps, vec![DepSize { path: "app/node_modules".into(), bytes: 200 }]);
        let g4 = duplicate_deps(&sv.members, 4);
        let main4 = g4[0].members.iter().find(|m| m.path.ends_with("repo")).unwrap();
        assert_eq!(main4.total_bytes, 700);
        assert_eq!(main4.deps[0].path, "a/b/c/node_modules");
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn nested_member_counted_once() {
        let r = tmp("nested");
        let repo = main_repo(&r);
        let nested = repo.join(".claude/worktrees/n1");
        fs::create_dir_all(repo.join(".git/worktrees/n1")).unwrap();
        fs::create_dir_all(nested.join("node_modules")).unwrap();
        fs::write(nested.join("node_modules/x.js"), vec![0u8; 400]).unwrap();
        fs::write(
            nested.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/n1").display()),
        )
        .unwrap();
        let sv = survey(&[r.clone()], 6);
        let g = duplicate_deps(&sv.members, 3);
        assert_eq!(g[0].total_bytes, 400);
        let main = g[0].members.iter().find(|m| m.path.ends_with("repo")).unwrap();
        assert_eq!(main.total_bytes, 0);
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn estimates_shared_store_savings() {
        let r = tmp("share");
        let repo = main_repo(&r);
        let w = r.join("w1");
        for base in [&repo, &w] {
            fs::create_dir_all(base.join("node_modules/p")).unwrap();
            fs::write(base.join("node_modules/p/same.js"), vec![0u8; 100]).unwrap();
            fs::create_dir_all(base.join("target")).unwrap();
            fs::write(base.join("target/t.o"), vec![0u8; 900]).unwrap();
        }
        fs::write(repo.join("node_modules/p/diff.js"), vec![0u8; 10]).unwrap();
        fs::write(w.join("node_modules/p/diff.js"), vec![0u8; 20]).unwrap();
        let sv = survey(&[r.clone()], 6);
        let g = duplicate_deps(&sv.members, 3);
        assert_eq!(g[0].shareable_bytes, 100 + 10 + 100 + 20);
        assert_eq!(g[0].savings_bytes, 100); // target is not shareable; sizes differ for diff.js
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn no_savings_with_one_copy() {
        let r = tmp("share1");
        let repo = main_repo(&r);
        fs::create_dir_all(repo.join("node_modules")).unwrap();
        fs::write(repo.join("node_modules/a.js"), vec![0u8; 100]).unwrap();
        let sv = survey(&[r.clone()], 6);
        let g = duplicate_deps(&sv.members, 3);
        assert_eq!(g[0].shareable_bytes, 100);
        assert_eq!(g[0].savings_bytes, 0);
        let _ = fs::remove_dir_all(&r);
    }
}
