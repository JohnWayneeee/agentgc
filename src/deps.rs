use crate::worktrees::Member;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Dependency folders measured at each member root.
const DEP_DIRS: [&str; 5] = ["node_modules", ".venv", "vendor/bundle", "target", ".next"];

#[derive(Debug, Serialize, PartialEq)]
pub struct DepSize {
    pub dir: String,
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

fn member_deps(path: &Path, orphan: bool) -> MemberDeps {
    let mut deps = Vec::new();
    for d in DEP_DIRS {
        let p = path.join(d);
        if fs::symlink_metadata(&p).map_or(false, |m| m.is_dir()) {
            deps.push(DepSize { dir: d.to_string(), bytes: crate::scan::dir_size(&p) });
        }
    }
    deps.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.dir.cmp(&b.dir)));
    let total_bytes = deps.iter().map(|d| d.bytes).sum();
    MemberDeps { path: path.display().to_string(), orphan, total_bytes, deps }
}

/// Group members by repo. Keep repos with 2 or more members. Biggest first.
pub fn duplicate_deps(members: &[Member]) -> Vec<RepoDeps> {
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
            ms.values().map(|m| member_deps(&m.path, m.orphan)).collect();
        members.sort_by(|a, b| b.total_bytes.cmp(&a.total_bytes).then_with(|| a.path.cmp(&b.path)));
        out.push(RepoDeps {
            repo,
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
        let g = duplicate_deps(&sv.members);
        assert_eq!(g.len(), 1);
        assert_eq!(g[0].member_count, 2);
        assert_eq!(g[0].total_bytes, 1350);
        assert_eq!(g[0].copies, 2);
        assert_eq!(g[0].members[0].total_bytes, 1000);
        assert_eq!(g[0].members[1].deps[0], DepSize { dir: "node_modules".into(), bytes: 300 });
        assert_eq!(g[0].members[1].deps[1].dir, "target");
        let _ = fs::remove_dir_all(&r);
    }
}
