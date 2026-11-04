use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};
use std::path::{Component, Path};

/// Declarative task config (`agentgc.task.toml`). Every field is optional.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub worktree: WorktreeCfg,
    pub copy: CopyCfg,
    pub ports: PortsCfg,
    pub db: DbCfg,
    pub env: EnvCfg,
    pub setup: SetupCfg,
    pub compose: Option<ComposeCfg>,
    pub routes: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeCfg {
    pub dir: String,
    pub branch: String,
    pub base: String,
}

impl Default for WorktreeCfg {
    fn default() -> Self {
        Self { dir: "worktrees".into(), branch: "{name}".into(), base: "main".into() }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct CopyCfg {
    pub files: Vec<String>,
}

impl Default for CopyCfg {
    fn default() -> Self {
        Self { files: vec![".env".into(), ".env.local".into()] }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct PortsCfg {
    pub names: Vec<String>,
    pub range: Vec<u16>,
}

impl Default for PortsCfg {
    fn default() -> Self {
        Self { names: vec!["web".into(), "api".into()], range: vec![3100, 3999] }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct DbCfg {
    pub name: String,
    pub create: String,
    pub drop: String,
}

impl Default for DbCfg {
    fn default() -> Self {
        Self { name: "{repo}_{name}".into(), create: String::new(), drop: String::new() }
    }
}

/// `[env]` plain values plus the `[env.dynamic]` table.
#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub struct EnvCfg {
    #[serde(default)]
    pub dynamic: BTreeMap<String, String>,
    #[serde(flatten)]
    pub vars: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SetupCfg {
    pub commands: Vec<String>,
}

/// `[compose]`: render one compose file per task from a template.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ComposeCfg {
    /// Path relative to the repo root.
    pub template: String,
    /// Path relative to the worktree.
    pub output: String,
}

pub fn parse_config(text: &str) -> Result<Config, String> {
    let c: Config = toml::from_str(text).map_err(|e| format!("bad task config: {e}"))?;
    validate(&c)?;
    Ok(c)
}

fn validate(c: &Config) -> Result<(), String> {
    if !safe_rel(&c.worktree.dir) {
        return Err(format!("worktree.dir must be a relative path without '..': {:?}", c.worktree.dir));
    }
    for f in &c.copy.files {
        if !safe_rel(f) {
            return Err(format!("copy.files entry must be a relative path without '..': {f:?}"));
        }
    }
    if bad_ref(&c.worktree.base) {
        return Err(format!(
            "worktree.base is not a valid branch (empty, starts with '-', or has whitespace/control characters): {:?}",
            c.worktree.base
        ));
    }
    if bad_ref(&c.worktree.branch) {
        return Err(format!(
            "worktree.branch is not valid (empty, starts with '-', or has whitespace/control characters): {:?}",
            c.worktree.branch
        ));
    }
    if c.ports.range.len() != 2 || c.ports.range[0] == 0 || c.ports.range[0] > c.ports.range[1] {
        return Err("ports.range must be [low, high] with 1 <= low <= high <= 65535".into());
    }
    let mut seen = HashSet::new();
    for n in &c.ports.names {
        sanitize_name(n).map_err(|e| format!("ports.names {n:?}: {e}"))?;
        if !seen.insert(n.clone()) {
            return Err(format!("ports.names has {n:?} twice"));
        }
    }
    if c.ports.names.len() > (c.ports.range[1] - c.ports.range[0]) as usize + 1 {
        return Err("ports.range is smaller than the number of ports.names".into());
    }
    if let Some(cp) = &c.compose {
        if !safe_rel(&cp.template) {
            return Err(format!("compose.template must be a relative path without '..': {:?}", cp.template));
        }
        if !safe_rel(&cp.output) {
            return Err(format!("compose.output must be a relative path without '..': {:?}", cp.output));
        }
    }
    for (r, port) in &c.routes {
        sanitize_name(r).map_err(|e| format!("routes {r:?}: {e}"))?;
        if !c.ports.names.contains(port) {
            return Err(format!("routes.{r} points to port {port:?}, which is not in ports.names"));
        }
    }
    for k in c.env.vars.keys().chain(c.env.dynamic.keys()) {
        if !valid_env_key(k) {
            return Err(format!("env key {k:?} must match [A-Za-z_][A-Za-z0-9_]*"));
        }
    }
    Ok(())
}

/// A branch or ref value we refuse: empty, starts with `-`, has whitespace or control characters.
pub fn bad_ref(s: &str) -> bool {
    s.is_empty() || s.starts_with('-') || s.chars().any(|c| c.is_whitespace() || c.is_control())
}

pub fn valid_env_key(k: &str) -> bool {
    let mut ch = k.chars();
    match ch.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    ch.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Relative path with at least one normal part and no `..`, root or drive.
pub fn safe_rel(p: &str) -> bool {
    let path = Path::new(p);
    let mut normal = false;
    for c in path.components() {
        match c {
            Component::Normal(_) => normal = true,
            Component::CurDir => {}
            _ => return false,
        }
    }
    normal && !p.contains('\0')
}

/// Task name: lowercase letters, digits, `-`, `_`. Rejects everything else.
pub fn sanitize_name(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("name is empty".into());
    }
    if s.len() > 64 {
        return Err(format!("name {s:?} is longer than 64 characters"));
    }
    if let Some(bad) = s
        .chars()
        .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-' || *c == '_'))
    {
        return Err(format!(
            "name {s:?} has the character {bad:?}; allowed: lowercase letters a-z, digits, '-' and '_'"
        ));
    }
    if s.starts_with('-') {
        return Err(format!("name {s:?} must not start with '-'"));
    }
    Ok(s.to_string())
}

/// Repo folder name as a template value: lowercase, other characters become `_`.
pub fn sanitize_repo(s: &str) -> String {
    let r: String = s
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let r = r.trim_start_matches('-').to_string();
    if r.is_empty() {
        "repo".into()
    } else {
        r
    }
}

/// Repo name for a database name: like `sanitize_repo`, with `-` turned into `_`.
pub fn sanitize_repo_db(s: &str) -> String {
    sanitize_repo(s).replace('-', "_")
}

fn is_var(k: &str) -> bool {
    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-')
}

/// Replace `{var}` with values. An unknown variable is an error. A `{` that does
/// not open a variable (for example `{ "a": 1 }`) stays as it is.
pub fn fill(tpl: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find('{') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('}') {
            Some(j) if is_var(&after[..j]) => {
                let k = &after[..j];
                match vars.get(k) {
                    Some(v) => out.push_str(v),
                    None => {
                        let known: Vec<String> = vars.keys().map(|k| format!("{{{k}}}")).collect();
                        return Err(format!(
                            "unknown variable {{{k}}} in {tpl:?} (known: {})",
                            known.join(" ")
                        ));
                    }
                }
                rest = &after[j + 1..];
            }
            _ => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// First free port in `range` for each name. `is_free` tests the OS, `reserved`
/// holds ports of other tasks. A port is never given twice.
pub fn allocate_ports(
    names: &[String],
    range: (u16, u16),
    reserved: &HashSet<u16>,
    is_free: &dyn Fn(u16) -> bool,
) -> Result<Vec<(String, u16)>, String> {
    let mut used: HashSet<u16> = reserved.clone();
    let mut out = Vec::new();
    for n in names {
        let found = (range.0..=range.1).find(|p| !used.contains(p) && is_free(*p));
        match found {
            Some(p) => {
                used.insert(p);
                out.push((n.clone(), p));
            }
            None => return Err(format!("no free port for {n:?} in {}-{}", range.0, range.1)),
        }
    }
    Ok(out)
}

/// `KEY=VALUE` lines.
pub fn render_env(pairs: &[(String, String)]) -> Result<String, String> {
    let mut s = String::new();
    for (k, v) in pairs {
        if v.contains('\n') || v.contains('\r') {
            return Err(format!("env value of {k} has a line break"));
        }
        s.push_str(&format!("{k}={v}\n"));
    }
    Ok(s)
}

/// Add or replace a pair, keeping the first position of the key.
pub fn put(pairs: &mut Vec<(String, String)>, k: &str, v: String) {
    match pairs.iter_mut().find(|(pk, _)| pk == k) {
        Some(p) => p.1 = v,
        None => pairs.push((k.to_string(), v)),
    }
}

/// Value safe to print in a plan: hides `user:pass@` in URLs and values of keys
/// that look like secrets.
pub fn mask_value(key: &str, value: &str) -> String {
    let up = key.to_uppercase();
    if ["SECRET", "TOKEN", "PASSWORD", "PASSWD", "KEY"].iter().any(|w| up.contains(w)) {
        return "***".into();
    }
    mask_url(value)
}

pub fn mask_url(value: &str) -> String {
    let Some(i) = value.find("://") else { return value.to_string() };
    let start = i + 3;
    let rest = &value[start..];
    let auth_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    match rest[..auth_end].rfind('@') {
        Some(at) => format!("{}***@{}", &value[..start], &rest[at + 1..]),
        None => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(p: &[(&str, &str)]) -> BTreeMap<String, String> {
        p.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn name_rules() {
        assert_eq!(sanitize_name("fix-bug_1").unwrap(), "fix-bug_1");
        for bad in ["", "Fix", "a b", "a/b", "a.b", "-x", "ä", "a;rm"] {
            assert!(sanitize_name(bad).is_err(), "{bad}");
        }
        assert!(sanitize_name("Fix").unwrap_err().contains("allowed"));
        assert!(sanitize_name(&"a".repeat(65)).is_err());
    }

    #[test]
    fn repo_sanitized() {
        assert_eq!(sanitize_repo("My Repo.git"), "my_repo_git");
        assert_eq!(sanitize_repo("..."), "___");
        assert_eq!(sanitize_repo(""), "repo");
    }

    #[test]
    fn fill_templates() {
        let v = vars(&[("name", "x"), ("repo", "r"), ("port.web", "3100")]);
        assert_eq!(fill("{repo}_{name}", &v).unwrap(), "r_x");
        assert_eq!(fill("http://localhost:{port.web}/", &v).unwrap(), "http://localhost:3100/");
        assert_eq!(fill("{ \"a\": 1 } {", &v).unwrap(), "{ \"a\": 1 } {");
        assert_eq!(fill("plain", &v).unwrap(), "plain");
        let e = fill("{db}", &v).unwrap_err();
        assert!(e.contains("unknown variable {db}") && e.contains("{name}"));
    }

    #[test]
    fn ports_skip_reserved_and_busy() {
        let names = vec!["web".to_string(), "api".to_string()];
        let reserved: HashSet<u16> = [3100, 3102].into_iter().collect();
        let got = allocate_ports(&names, (3100, 3110), &reserved, &|p| p != 3101).unwrap();
        assert_eq!(got, vec![("web".to_string(), 3103), ("api".to_string(), 3104)]);
        let got = allocate_ports(&names, (5, 6), &HashSet::new(), &|_| true).unwrap();
        assert_eq!(got[0].1, 5);
        assert_eq!(got[1].1, 6);
        let e = allocate_ports(&names, (5, 5), &HashSet::new(), &|_| true).unwrap_err();
        assert!(e.contains("api"));
    }

    #[test]
    fn env_render_and_put() {
        let mut p = vec![("A".to_string(), "1".to_string())];
        put(&mut p, "B", "2".into());
        put(&mut p, "A", "3".into());
        assert_eq!(render_env(&p).unwrap(), "A=3\nB=2\n");
        assert!(render_env(&[("A".into(), "x\ny".into())]).is_err());
    }

    #[test]
    fn masking() {
        assert_eq!(mask_url("postgres://user:pass@localhost:5432/db"), "postgres://***@localhost:5432/db");
        assert_eq!(mask_url("postgres://localhost/db"), "postgres://localhost/db");
        assert_eq!(mask_url("https://h/p?x=a@b"), "https://h/p?x=a@b");
        assert_eq!(mask_url("plain"), "plain");
        assert_eq!(mask_value("API_KEY", "abc"), "***");
        assert_eq!(mask_value("DATABASE_URL", "pg://u:p@h/d"), "pg://***@h/d");
        assert_eq!(mask_value("TASK_ID", "x"), "x");
    }

    #[test]
    fn config_defaults_and_errors() {
        let c = parse_config("").unwrap();
        assert_eq!(c.worktree.dir, "worktrees");
        assert_eq!(c.ports.range, vec![3100, 3999]);
        let c = parse_config("[env]\nTASK_ID=\"{name}\"\n[env.dynamic]\nU=\"x\"\n").unwrap();
        assert_eq!(c.env.vars.get("TASK_ID").unwrap(), "{name}");
        assert_eq!(c.env.dynamic.get("U").unwrap(), "x");
        assert!(parse_config("[worktree]\ndir=\"../x\"").is_err());
        assert!(parse_config("[worktree]\ndir=\"/abs\"").is_err());
        assert!(parse_config("[ports]\nrange=[5,1]").is_err());
        assert!(parse_config("[nope]\nx=1").is_err());
        for bad in ["-x", "a b", "a\tb", "", "a\u{7f}"] {
            assert!(parse_config(&format!("[worktree]\nbase={bad:?}")).is_err(), "base {bad:?}");
            assert!(parse_config(&format!("[worktree]\nbranch={bad:?}")).is_err(), "branch {bad:?}");
        }
        assert!(parse_config("[worktree]\nbase=\"release/1.0\"\nbranch=\"task/{name}\"").is_ok());
        assert_eq!(sanitize_repo_db("My-Repo.x"), "my_repo_x");
        assert!(parse_config("[copy]\nfiles=[\"../.env\"]").is_err());
    }
}
