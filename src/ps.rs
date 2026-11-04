use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_PROC_RULES: &str = include_str!("../rules/processes.toml");

/// Kinds that can be flagged idle. Agents and databases never are.
const IDLE_KINDS: [&str; 4] = ["dev-server", "lsp", "test", "build"];
const IDLE_CPU_PERCENT: f32 = 1.0;
const CMD_MAX: usize = 80;
const UNKNOWN: &str = "unknown cwd";
const SECRET_FLAGS: [&str; 4] = ["token", "key", "password", "secret"];
const SKIP_PREFIXES: [&str; 8] = [
    "c:/windows",
    "c:/program files",
    "c:/program files (x86)",
    "c:/programdata",
    "/usr",
    "/system",
    "/library",
    "/proc",
];

/// One process as read from the system. Plain data, so tests can fake it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub cmd: String,
    pub cwd: Option<PathBuf>,
    pub rss_bytes: u64,
    pub cpu_percent: f32,
}

#[derive(Debug, Deserialize)]
pub struct KindRule {
    pub name: String,
    pub patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct KindFile {
    #[serde(default)]
    kind: Vec<KindRule>,
}

pub fn parse_proc_rules(text: &str) -> Result<Vec<KindRule>, String> {
    toml::from_str::<KindFile>(text)
        .map(|f| f.kind)
        .map_err(|e| format!("invalid process rules: {e}"))
}

/// First kind (file order) with a pattern found in name + command line. Else `other`.
pub fn classify(rules: &[KindRule], name: &str, cmd: &str) -> String {
    let hay = format!("{name} {cmd}").to_lowercase();
    for r in rules {
        if r.patterns.iter().any(|p| !p.is_empty() && hay.contains(&p.to_lowercase())) {
            return r.name.clone();
        }
    }
    "other".to_string()
}

pub fn is_idle(kind: &str, cpu_percent: f32) -> bool {
    IDLE_KINDS.contains(&kind) && cpu_percent < IDLE_CPU_PERCENT
}

pub(crate) fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/").to_lowercase();
    let s = s.strip_prefix("//?/").unwrap_or(&s).to_string();
    if s.len() > 1 {
        s.trim_end_matches('/').to_string()
    } else {
        s
    }
}

pub(crate) fn path_under(path: &str, base: &str) -> bool {
    path == base || path.strip_prefix(base).map_or(false, |r| r.starts_with('/'))
}

/// True for system folders whose processes are never listed.
pub fn is_system_path(p: &Path) -> bool {
    let n = norm(p);
    SKIP_PREFIXES.iter().any(|b| path_under(&n, b))
}

/// Nearest ancestor (cwd included) that has a `.git` file or folder.
pub fn task_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors().find(|d| d.join(".git").exists()).map(|d| d.to_path_buf())
}

/// Hide secrets: cut everything after a secret flag, blank values of `NAME=value`
/// env-like words and of secret flags written as `--token=value`.
pub fn sanitize_cmd(cmd: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    for w in cmd.split_whitespace() {
        let lw = w.to_lowercase();
        let is_flag = lw.starts_with('-');
        let flag_name = lw.trim_start_matches('-').split('=').next().unwrap_or("").to_string();
        if is_flag && SECRET_FLAGS.iter().any(|s| flag_name.contains(s)) {
            match w.split_once('=') {
                Some((k, _)) => out.push(format!("{k}=***")),
                None => {
                    out.push(w.to_string());
                    out.push("***".to_string());
                }
            }
            break;
        }
        if let Some((k, _)) = w.split_once('=') {
            let env_like = !k.is_empty()
                && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            if env_like {
                out.push(format!("{k}=***"));
                continue;
            }
        }
        out.push(w.to_string());
    }
    out.join(" ")
}

pub fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max.saturating_sub(3)).collect();
    format!("{head}...")
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Row {
    pub pid: u32,
    pub name: String,
    pub kind: String,
    pub rss_bytes: u64,
    pub cpu_percent: f32,
    pub idle: bool,
    pub cmd: String,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Group {
    pub path: String,
    pub unknown: bool,
    pub total_rss_bytes: u64,
    pub processes: Vec<Row>,
}

/// Group processes by task root. `self_pid` and system paths are skipped.
/// Groups: RAM descending. Rows in a group: RAM descending.
pub fn build_groups(procs: &[ProcInfo], rules: &[KindRule], self_pid: u32) -> Vec<Group> {
    let mut map: HashMap<String, Group> = HashMap::new();
    for p in procs {
        if p.pid == self_pid {
            continue;
        }
        let (key, unknown) = match &p.cwd {
            None => (UNKNOWN.to_string(), true),
            Some(cwd) => {
                if is_system_path(cwd) {
                    continue;
                }
                let cwd: PathBuf = cwd.components().collect();
                let root = task_root(&cwd).unwrap_or(cwd);
                (root.display().to_string(), false)
            }
        };
        let kind = classify(rules, &p.name, &p.cmd);
        let row = Row {
            pid: p.pid,
            name: p.name.clone(),
            idle: is_idle(&kind, p.cpu_percent),
            kind,
            rss_bytes: p.rss_bytes,
            cpu_percent: p.cpu_percent,
            cmd: shorten(&sanitize_cmd(&p.cmd), CMD_MAX),
        };
        let g = map.entry(key.clone()).or_insert_with(|| Group {
            path: key,
            unknown,
            total_rss_bytes: 0,
            processes: Vec::new(),
        });
        g.total_rss_bytes += row.rss_bytes;
        g.processes.push(row);
    }
    let mut groups: Vec<Group> = map.into_values().collect();
    for g in &mut groups {
        g.processes.sort_by(|a, b| b.rss_bytes.cmp(&a.rss_bytes).then(a.pid.cmp(&b.pid)));
    }
    groups.sort_by(|a, b| b.total_rss_bytes.cmp(&a.total_rss_bytes).then(a.path.cmp(&b.path)));
    groups
}

/// Apply `--all`, `--min-mb` and `--root`.
pub fn filter_groups(groups: Vec<Group>, all: bool, min_mb: u64, root: Option<&Path>) -> Vec<Group> {
    let min_bytes = min_mb.saturating_mul(1024 * 1024);
    let root_n = root.map(norm);
    groups
        .into_iter()
        .filter(|g| all || !g.unknown)
        .filter(|g| g.total_rss_bytes >= min_bytes)
        .filter(|g| match &root_n {
            Some(r) => !g.unknown && path_under(&norm(Path::new(&g.path)), r),
            None => true,
        })
        .collect()
}

/// Read all processes. Two CPU samples, 1 second apart. Read-only: no signals sent.
pub fn read_system() -> Vec<ProcInfo> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let kind = ProcessRefreshKind::nothing()
        .with_cpu()
        .with_memory()
        .with_cmd(UpdateKind::OnlyIfNotSet)
        .with_cwd(UpdateKind::OnlyIfNotSet);
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    std::thread::sleep(std::time::Duration::from_secs(1));
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    sys.processes()
        .iter()
        .map(|(pid, p)| ProcInfo {
            pid: pid.as_u32(),
            name: p.name().to_string_lossy().into_owned(),
            cmd: p.cmd().iter().map(|s| s.to_string_lossy()).collect::<Vec<_>>().join(" "),
            cwd: p.cwd().map(|c| c.to_path_buf()),
            rss_bytes: p.memory(),
            cpu_percent: p.cpu_usage(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn rules() -> Vec<KindRule> {
        parse_proc_rules(DEFAULT_PROC_RULES).unwrap()
    }

    fn pi(pid: u32, name: &str, cmd: &str, cwd: Option<&Path>, mb: u64, cpu: f32) -> ProcInfo {
        ProcInfo {
            pid,
            name: name.into(),
            cmd: cmd.into(),
            cwd: cwd.map(|c| c.to_path_buf()),
            rss_bytes: mb * 1024 * 1024,
            cpu_percent: cpu,
        }
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-ps-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn embedded_rules_have_all_kinds() {
        let r = rules();
        for k in ["agent", "dev-server", "lsp", "test", "build", "db"] {
            let kr = r.iter().find(|x| x.name == k).unwrap_or_else(|| panic!("missing {k}"));
            assert!(!kr.patterns.is_empty(), "{k} has no patterns");
        }
    }

    #[test]
    fn classifies_by_substring() {
        let r = rules();
        assert_eq!(classify(&r, "node", "node vite --port 5173"), "dev-server");
        assert_eq!(classify(&r, "rust-analyzer.exe", ""), "lsp");
        assert_eq!(classify(&r, "cargo", "cargo test --lib"), "test");
        assert_eq!(classify(&r, "cargo", "cargo build"), "build");
        assert_eq!(classify(&r, "postgres", ""), "db");
        assert_eq!(classify(&r, "CLAUDE.exe", ""), "agent");
        assert_eq!(classify(&r, "notepad", ""), "other");
    }

    #[test]
    fn idle_rules() {
        assert!(is_idle("dev-server", 0.2));
        assert!(is_idle("lsp", 0.0));
        assert!(!is_idle("dev-server", 1.0));
        assert!(!is_idle("agent", 0.0));
        assert!(!is_idle("db", 0.0));
        assert!(!is_idle("other", 0.0));
    }

    #[test]
    fn finds_git_root_file_and_folder() {
        let d = tmp("root");
        let main = d.join("main");
        let wt = d.join("wt");
        fs::create_dir_all(main.join(".git")).unwrap();
        fs::create_dir_all(main.join("src/deep")).unwrap();
        fs::create_dir_all(wt.join("pkg")).unwrap();
        fs::write(wt.join(".git"), "gitdir: nowhere").unwrap();
        assert_eq!(task_root(&main.join("src/deep")), Some(main.clone()));
        assert_eq!(task_root(&wt.join("pkg")), Some(wt.clone()));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn groups_sorted_and_filtered() {
        let d = tmp("groups");
        let a = d.join("a");
        let b = d.join("b");
        let plain = d.join("plain");
        fs::create_dir_all(a.join(".git")).unwrap();
        fs::create_dir_all(b.join(".git")).unwrap();
        fs::create_dir_all(&plain).unwrap();
        let procs = vec![
            pi(1, "node", "node vite", Some(&a.join("web")), 100, 0.0),
            pi(2, "tsserver", "tsserver", Some(&a), 300, 0.0),
            pi(3, "pytest", "pytest", Some(&b), 500, 20.0),
            pi(4, "foo", "foo", Some(&plain), 10, 0.0),
            pi(5, "secret", "x", None, 900, 0.0),
            pi(6, "svc", "svc", Some(Path::new("C:\\Windows\\System32")), 900, 0.0),
            pi(7, "svc", "svc", Some(Path::new("/usr/lib")), 900, 0.0),
            pi(99, "agentgc", "agentgc ps", Some(&a), 900, 0.0),
        ];
        let g = build_groups(&procs, &rules(), 99);
        assert_eq!(g.len(), 4); // a, b, plain, unknown
        assert_eq!(g[0].path, "unknown cwd");
        assert_eq!(g[1].path, b.display().to_string());
        assert_eq!(g[2].path, a.display().to_string());
        assert_eq!(g[2].total_rss_bytes, 400 * 1024 * 1024);
        assert_eq!(g[2].processes[0].pid, 2);
        assert!(g[2].processes.iter().all(|r| r.idle));
        assert!(!g[1].processes[0].idle);
        assert_eq!(g[3].path, plain.display().to_string());

        let f = filter_groups(build_groups(&procs, &rules(), 99), false, 50, None);
        assert_eq!(f.len(), 2); // unknown hidden, plain under 50 MB
        let f = filter_groups(build_groups(&procs, &rules(), 99), true, 50, None);
        assert_eq!(f.len(), 3);
        let f = filter_groups(build_groups(&procs, &rules(), 99), true, 0, Some(&a));
        assert_eq!(f.len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn system_paths_skipped() {
        assert!(is_system_path(Path::new("C:\\Program Files (x86)\\x")));
        assert!(is_system_path(Path::new("c:/ProgramData")));
        assert!(is_system_path(Path::new("/System/Library")));
        assert!(!is_system_path(Path::new("C:\\Windowsfoo")));
        assert!(!is_system_path(Path::new("/home/u/usr")));
    }

    #[test]
    fn sanitizes_secrets() {
        assert_eq!(sanitize_cmd("node app.js --token abc123 --port 1"), "node app.js --token ***");
        assert_eq!(sanitize_cmd("x --api-key=abc --y"), "x --api-key=***");
        assert_eq!(sanitize_cmd("env API_TOKEN=abc node a.js"), "env API_TOKEN=*** node a.js");
        assert_eq!(sanitize_cmd("node a.js --port=3000"), "node a.js --port=3000");
    }

    #[test]
    fn shortens_to_80() {
        let s = "a".repeat(200);
        let o = shorten(&s, 80);
        assert_eq!(o.chars().count(), 80);
        assert!(o.ends_with("..."));
        assert_eq!(shorten("abc", 80), "abc");
    }
}
