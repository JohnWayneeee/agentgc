use crate::rules::Rule;
use serde::Serialize;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize, PartialEq)]
pub struct Finding {
    pub id: String,
    pub name: String,
    pub safety: &'static str,
    pub category: String,
    pub path: String,
    pub bytes: u64,
    pub note: String,
    pub clean_cmd: Option<String>,
}

/// Expand `%VAR%`, `$VAR`, `${VAR}` and a leading `~`.
/// Returns None when any referenced variable is unset or empty.
pub fn expand_with<F: Fn(&str) -> Option<String>>(input: &str, get: F) -> Option<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '~' && i == 0 && (chars.len() == 1 || chars[1] == '/' || chars[1] == '\\') {
            let home = nonempty(get("HOME")).or_else(|| nonempty(get("USERPROFILE")))?;
            out.push_str(&home);
            i += 1;
        } else if c == '%' {
            let end = chars[i + 1..].iter().position(|&x| x == '%')? + i + 1;
            let name: String = chars[i + 1..end].iter().collect();
            out.push_str(&nonempty(get(&name))?);
            i = end + 1;
        } else if c == '$' {
            let (name, next) = if chars.get(i + 1) == Some(&'{') {
                let end = chars[i + 2..].iter().position(|&x| x == '}')? + i + 2;
                (chars[i + 2..end].iter().collect::<String>(), end + 1)
            } else {
                let mut j = i + 1;
                while j < chars.len() && (chars[j].is_alphanumeric() || chars[j] == '_') {
                    j += 1;
                }
                (chars[i + 1..j].iter().collect::<String>(), j)
            };
            if name.is_empty() {
                out.push('$');
                i += 1;
            } else {
                out.push_str(&nonempty(get(&name))?);
                i = next;
            }
        } else {
            out.push(c);
            i += 1;
        }
    }
    Some(out)
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.is_empty())
}

/// Total size of regular files under `path`. Symlinks are not followed.
/// Unreadable entries are skipped.
pub fn dir_size(path: &Path) -> u64 {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let rd = match fs::read_dir(&dir) {
            Ok(r) => r,
            Err(_) => continue,
        };
        for entry in rd.flatten() {
            let m = match fs::symlink_metadata(entry.path()) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if m.is_dir() {
                stack.push(entry.path());
            } else if m.is_file() {
                total += m.len();
            }
        }
    }
    total
}

/// Measure every existing path of every rule. Duplicate paths are counted once.
pub fn scan_rules<F: Fn(&str) -> Option<String>>(rules: &[Rule], get: F) -> Vec<Finding> {
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut out = Vec::new();
    for rule in rules {
        for raw in &rule.paths {
            let Some(expanded) = expand_with(raw, &get) else { continue };
            let p = PathBuf::from(&expanded);
            if !p.exists() || seen.contains(&p) {
                continue;
            }
            seen.push(p.clone());
            out.push(Finding {
                id: rule.id.clone(),
                name: rule.name.clone(),
                safety: rule.safety.as_str(),
                category: rule.id.clone(),
                path: expanded,
                bytes: dir_size(&p),
                note: rule.note.clone(),
                clean_cmd: rule.clean_cmd.clone(),
            });
        }
    }
    out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
    out
}

pub fn human(bytes: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.2} GB", b / GB)
    } else if b >= MB {
        format!("{:.1} MB", b / MB)
    } else if bytes >= 1024 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::parse_rules;
    use std::collections::HashMap;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-test-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let m: HashMap<String, String> =
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        move |k| m.get(k).cloned()
    }

    #[test]
    fn expands_percent_and_dollar() {
        let g = env(&[("A", "/a"), ("B", "b")]);
        assert_eq!(expand_with("%A%/x", &g).as_deref(), Some("/a/x"));
        assert_eq!(expand_with("$A/$B", &g).as_deref(), Some("/a/b"));
        assert_eq!(expand_with("${A}/y", &g).as_deref(), Some("/a/y"));
    }

    #[test]
    fn expands_tilde() {
        let g = env(&[("HOME", "/home/u")]);
        assert_eq!(expand_with("~/.npm", &g).as_deref(), Some("/home/u/.npm"));
        let w = env(&[("USERPROFILE", "C:\\Users\\u")]);
        assert_eq!(expand_with("~/.npm", &w).as_deref(), Some("C:\\Users\\u/.npm"));
    }

    #[test]
    fn unset_var_gives_none() {
        let g = env(&[]);
        assert_eq!(expand_with("%NOPE%/x", &g), None);
        assert_eq!(expand_with("$NOPE/x", &g), None);
    }

    #[test]
    fn dir_size_sums_nested_files() {
        let d = tmp("size");
        fs::write(d.join("a"), vec![0u8; 100]).unwrap();
        fs::create_dir_all(d.join("sub/deep")).unwrap();
        fs::write(d.join("sub/b"), vec![0u8; 50]).unwrap();
        fs::write(d.join("sub/deep/c"), vec![0u8; 25]).unwrap();
        assert_eq!(dir_size(&d), 175);
        assert_eq!(dir_size(&d.join("missing")), 0);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn scan_skips_missing_and_sorts() {
        let d = tmp("scan");
        fs::create_dir_all(d.join("small")).unwrap();
        fs::create_dir_all(d.join("big")).unwrap();
        fs::write(d.join("small/f"), vec![0u8; 10]).unwrap();
        fs::write(d.join("big/f"), vec![0u8; 1000]).unwrap();
        let rules = parse_rules(
            "[[rule]]\nid=\"s\"\nname=\"S\"\npaths=[\"$R/small\",\"$R/none\"]\nsafety=\"safe\"\n\
             [[rule]]\nid=\"b\"\nname=\"B\"\npaths=[\"$R/big\"]\nsafety=\"ask\"\n",
        )
        .unwrap();
        let g = env(&[("R", d.to_str().unwrap())]);
        let f = scan_rules(&rules, g);
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].id, "b");
        assert_eq!(f[0].bytes, 1000);
        assert_eq!(f[1].bytes, 10);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn human_units() {
        assert_eq!(human(10), "10 B");
        assert_eq!(human(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human(3 * 1024 * 1024 * 1024), "3.00 GB");
    }
}
