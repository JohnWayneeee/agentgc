//! `agentgc hook`: Claude Code hooks that make agents use agentgc without trusting them.
//! `pre-tool-use` wraps known heavy Bash commands in `agentgc run --`. `session-start` prints the
//! task status and env. `install` prints the settings.json snippet; it edits a file only with `--apply`.

use crate::clean::homes_from;
use serde_json::{json, Value};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

pub const PRE_CMD: &str = "agentgc hook pre-tool-use";
pub const PRE_CMD_ALLOW: &str = "agentgc hook pre-tool-use --allow";
pub const START_CMD: &str = "agentgc hook session-start";

/// Shell syntax we never rewrite: the command is not one plain program call.
const SHELL_CHARS: &[char] =
    &['&', '|', ';', '<', '>', '$', '`', '(', ')', '\n', '\r', '\\', '"', '\'', '*', '?', '{', '}'];

/// Programs and the first words that make a call heavy. An empty list means any call is heavy.
const HEAVY: &[(&str, &[&str])] = &[
    ("cargo", &["test", "build", "check", "clippy", "bench", "nextest"]),
    ("npm", &["test", "build"]),
    ("pnpm", &["test", "build"]),
    ("yarn", &["test", "build"]),
    ("bun", &["test"]),
    ("go", &["test", "build"]),
    ("dotnet", &["test", "build"]),
    ("gradle", &["test", "build"]),
    ("mvn", &["test", "package", "verify"]),
    ("pytest", &[]),
    ("tsc", &[]),
];

/// Script names that count as heavy after `npm run`, `pnpm run`, `yarn run`, `bun run`.
const HEAVY_SCRIPTS: &[&str] = &["build", "test", "e2e", "lint", "typecheck"];

fn is_heavy(words: &[&str]) -> bool {
    let Some(prog) = words.first() else { return false };
    let Some((_, subs)) = HEAVY.iter().find(|(p, _)| p == prog) else { return false };
    if subs.is_empty() {
        return true;
    }
    let Some(sub) = words.get(1) else { return false };
    if subs.contains(sub) {
        return true;
    }
    *sub == "run"
        && matches!(*prog, "npm" | "pnpm" | "yarn" | "bun")
        && words.get(2).is_some_and(|s| HEAVY_SCRIPTS.contains(s))
}

/// The wrapped command, or None when the command must run as it is.
pub fn rewrite_command(cmd: &str) -> Option<String> {
    let t = cmd.trim();
    if t.is_empty() || t.contains(SHELL_CHARS) {
        return None;
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    if is_heavy(&words) {
        Some(format!("agentgc run -- {}", words.join(" ")))
    } else {
        None
    }
}

/// Hook input JSON in, hook output JSON out. None: no opinion, the tool call goes on unchanged.
pub fn pre_tool_use(input: &str, allow: bool) -> Option<String> {
    let v: Value = serde_json::from_str(input).ok()?;
    if v.get("tool_name")?.as_str()? != "Bash" {
        return None;
    }
    let ti = v.get("tool_input")?.as_object()?;
    let new = rewrite_command(ti.get("command")?.as_str()?)?;
    let mut updated = ti.clone();
    updated.insert("command".into(), Value::String(new.clone()));
    let out = json!({"hookSpecificOutput": {
        "hookEventName": "PreToolUse",
        "permissionDecision": if allow { "allow" } else { "ask" },
        "permissionDecisionReason": format!("agentgc hook: queued heavy command ({new})"),
        "updatedInput": updated,
    }});
    Some(out.to_string())
}

/// Text for the session start: status lines (may be empty) and the task env file text (may be empty).
pub fn session_text(status: &str, env: &str) -> String {
    let mut s = String::new();
    if !status.trim().is_empty() {
        s.push_str("agentgc status:\n");
        s.push_str(status.trim_end());
        s.push('\n');
    }
    if !env.trim().is_empty() {
        s.push_str("Task env (.agentgc/task.env): use these ports and names, never hard-code others.\n");
        s.push_str(env.trim_end());
        s.push('\n');
    }
    s.push_str("Run heavy commands (tests, builds) as `agentgc run -- COMMAND`.\n");
    s
}

pub fn snippet(allow: bool) -> Value {
    let pre = if allow { PRE_CMD_ALLOW } else { PRE_CMD };
    json!({"hooks": {
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": pre}]}],
        "SessionStart": [{"hooks": [{"type": "command", "command": START_CMD}]}],
    }})
}

fn entry_commands(e: &Value) -> Vec<&str> {
    e.get("hooks")
        .and_then(Value::as_array)
        .map(|hs| hs.iter().filter_map(|h| h.get("command").and_then(Value::as_str)).collect())
        .unwrap_or_default()
}

fn has_command(entries: &[Value], cmd: &str) -> bool {
    entries.iter().any(|e| entry_commands(e).contains(&cmd))
}

/// An agentgc pre-tool-use entry: the command is `agentgc hook pre-tool-use`, with or without flags.
fn is_our_pre(e: &Value) -> bool {
    entry_commands(e).iter().any(|c| *c == PRE_CMD || c.starts_with("agentgc hook pre-tool-use "))
}

/// Keeps entries that are not ours.
fn is_our_pre_not(e: &Value) -> bool {
    !is_our_pre(e)
}

/// Adds our two hook entries, replacing an agentgc pre-tool-use entry of the other mode to a settings object. Returns whether anything changed.
pub fn merge_settings(settings: &mut Value, allow: bool) -> Result<bool, String> {
    let root = settings.as_object_mut().ok_or("settings.json is not a JSON object")?;
    let hooks = root.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("\"hooks\" is not an object")?;
    let mut changed = false;
    let snip = snippet(allow);
    let pre = if allow { PRE_CMD_ALLOW } else { PRE_CMD };
    for (event, cmd) in [("PreToolUse", pre), ("SessionStart", START_CMD)] {
        let list = hooks.entry(event).or_insert_with(|| json!([]));
        let list = list.as_array_mut().ok_or_else(|| format!("hooks.{event} is not an array"))?;
        if has_command(list, cmd) && (event != "PreToolUse" || list.iter().filter(|e| is_our_pre(e)).count() == 1) {
            continue;
        }
        let new = snip["hooks"][event][0].clone();
        if event == "PreToolUse" {
            // replace our own entry (default <-> --allow), in place, never add a second one
            let first = list.iter().position(is_our_pre);
            list.retain(is_our_pre_not);
            match first {
                Some(i) => list.insert(i.min(list.len()), new),
                None => list.push(new),
            }
        } else {
            list.push(new);
        }
        changed = true;
    }
    Ok(changed)
}

pub struct InstallArgs {
    pub global: bool,
    pub dir: Option<PathBuf>,
    pub apply: bool,
    pub allow: bool,
}

fn settings_path(a: &InstallArgs, get: &dyn Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    if a.global {
        let home = homes_from(get).into_iter().next().filter(|h| h.is_dir());
        let home = home.ok_or("the home dir is unknown or missing (HOME / USERPROFILE)")?;
        return Ok(home.join(".claude").join("settings.json"));
    }
    let dir = a.dir.clone().unwrap_or_else(|| PathBuf::from("."));
    if !dir.is_dir() {
        return Err(format!("--dir {}: not a folder", dir.display()));
    }
    Ok(dir.join(".claude").join("settings.json"))
}

/// Prints the plan; writes only with `apply`. Returns the text printed.
pub fn run_install(a: &InstallArgs, get: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let path = settings_path(a, get)?;
    let mut out = format!("Target: {}\n", path.display());
    let meta = fs::symlink_metadata(&path);
    if let Ok(m) = &meta {
        if !m.is_file() {
            return Err(format!("{} is not a regular file (symlink or folder): refused", path.display()));
        }
    }
    let original = if meta.is_ok() {
        Some(fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?)
    } else {
        None
    };
    let mut settings = match &original {
        Some(t) if !t.trim().is_empty() => serde_json::from_str::<Value>(t)
            .map_err(|e| format!("{} is not valid JSON: {e}; refused", path.display()))?,
        _ => json!({}),
    };
    let changed = merge_settings(&mut settings, a.allow)?;
    if !changed {
        out.push_str("Already up to date: both hooks are present.\n");
        return Ok(out);
    }
    let text = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())? + "\n";
    if !a.apply {
        out.push_str("Dry run. Nothing was written. Add this to the settings (or run again with --apply):\n");
        out.push_str(&serde_json::to_string_pretty(&snippet(a.allow)).map_err(|e| e.to_string())?);
        out.push('\n');
        return Ok(out);
    }
    if let Some(orig) = &original {
        let mut n = path.file_name().map(|s| s.to_os_string()).unwrap_or_default();
        n.push(".agentgc.bak");
        let bak = path.with_file_name(n);
        if fs::symlink_metadata(&bak).is_ok() {
            return Err(format!("the backup {} already exists: refused", bak.display()));
        }
        fs::write(&bak, orig).map_err(|e| format!("backup failed: {e}"))?;
        out.push_str(&format!("Backup: {}\n", bak.display()));
    } else if let Some(p) = path.parent() {
        fs::create_dir_all(p).map_err(|e| format!("cannot create {}: {e}", p.display()))?;
    }
    fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    out.push_str("Written. Restart Claude Code so it reads the hooks.\n");
    Ok(out)
}

pub fn cmd_pre_tool_use(allow: bool) -> Result<(), String> {
    let mut s = String::new();
    // A hook must never break the tool call: unreadable input means no opinion.
    if std::io::stdin().read_to_string(&mut s).is_ok() {
        if let Some(o) = pre_tool_use(&s, allow) {
            println!("{o}");
        }
    }
    Ok(())
}

pub fn cmd_session_start() -> Result<(), String> {
    let status = std::env::current_exe()
        .ok()
        .and_then(|exe| std::process::Command::new(exe).arg("status").output().ok())
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let env = fs::read_to_string(PathBuf::from(".agentgc").join("task.env")).unwrap_or_default();
    print!("{}", session_text(&status, &env));
    Ok(())
}

pub fn cmd_install(a: &InstallArgs) -> Result<(), String> {
    let get = |k: &str| std::env::var(k).ok();
    print!("{}", run_install(a, &get)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("agentgc-hook-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn inst(dir: &PathBuf, apply: bool) -> Result<String, String> {
        run_install(&InstallArgs { global: false, dir: Some(dir.clone()), apply, allow: false }, &|_| None)
    }

    fn inst_allow(dir: &PathBuf) -> Result<String, String> {
        run_install(&InstallArgs { global: false, dir: Some(dir.clone()), apply: true, allow: true }, &|_| None)
    }

    #[test]
    fn heavy_commands_are_wrapped() {
        for (c, w) in [
            ("cargo test", "agentgc run -- cargo test"),
            ("  cargo  build --release ", "agentgc run -- cargo build --release"),
            ("npm run build", "agentgc run -- npm run build"),
            ("pnpm test", "agentgc run -- pnpm test"),
            ("pytest -q tests/", "agentgc run -- pytest -q tests/"),
            ("go test ./...", "agentgc run -- go test ./..."),
        ] {
            assert_eq!(rewrite_command(c).as_deref(), Some(w), "{c}");
        }
    }

    #[test]
    fn other_commands_and_shell_syntax_are_left_alone() {
        for c in [
            "",
            "ls -la",
            "cargo fmt",
            "npm run dev",
            "npm install",
            "git status",
            "agentgc run -- cargo test",
            "cargo test && rm -rf x",
            "cargo test | tee log",
            "cargo test; ls",
            "cargo test > out.txt",
            "cargo test $(ls)",
            "cargo test `ls`",
            "FOO=1 cargo test",
            "cargo",
            "echo \"cargo test\"",
        ] {
            assert_eq!(rewrite_command(c), None, "{c:?}");
        }
    }

    #[test]
    fn pre_tool_use_json() {
        let inp = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test","description":"d","timeout":5}}"#;
        let v: Value = serde_json::from_str(&pre_tool_use(inp, false).unwrap()).unwrap();
        let o = &v["hookSpecificOutput"];
        assert_eq!(o["hookEventName"], "PreToolUse");
        assert_eq!(o["updatedInput"]["command"], "agentgc run -- cargo test");
        assert_eq!(o["updatedInput"]["description"], "d");
        assert_eq!(o["updatedInput"]["timeout"], 5);
        assert_eq!(o["permissionDecision"], "ask");
    }

    #[test]
    fn pre_tool_use_allow_flag_sets_allow() {
        let inp = r#"{"tool_name":"Bash","tool_input":{"command":"cargo test"}}"#;
        let v: Value = serde_json::from_str(&pre_tool_use(inp, true).unwrap()).unwrap();
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "allow");
    }

    #[test]
    fn install_allow_writes_flag_into_command() {
        let d = tmp("allow");
        inst_allow(&d).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(d.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "agentgc hook pre-tool-use --allow");
        assert_eq!(v, snippet(true));
        let _ = fs::remove_dir_all(&d);
    }

    fn pre_cmds(d: &PathBuf) -> Vec<String> {
        let v: Value = serde_json::from_str(&fs::read_to_string(d.join(".claude/settings.json")).unwrap()).unwrap();
        v["hooks"]["PreToolUse"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|e| entry_commands(e).into_iter().map(String::from).collect::<Vec<_>>())
            .collect()
    }

    #[test]
    fn install_switches_between_default_and_allow() {
        let d = tmp("switch");
        inst(&d, true).unwrap();
        assert_eq!(pre_cmds(&d), [PRE_CMD]);
        // the backup rule blocks a second apply, so remove the backup between applies
        let bak = d.join(".claude/settings.json.agentgc.bak");
        let out = inst_allow(&d).unwrap();
        assert!(out.contains("Written"));
        assert_eq!(pre_cmds(&d), [PRE_CMD_ALLOW]);
        let _ = fs::remove_file(&bak);
        inst(&d, true).unwrap();
        assert_eq!(pre_cmds(&d), [PRE_CMD]);
        let _ = fs::remove_file(&bak);
        assert!(inst(&d, true).unwrap().contains("Already up to date"));
        let v: Value = serde_json::from_str(&fs::read_to_string(d.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["hooks"]["SessionStart"].as_array().unwrap().len(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn install_replace_keeps_foreign_entries() {
        let d = tmp("replace");
        fs::create_dir_all(d.join(".claude")).unwrap();
        let orig = r#"{"hooks":{"PreToolUse":[{"matcher":"Bash","hooks":[{"type":"command","command":"agentgc hook pre-tool-use --allow"}]},{"matcher":"Edit","hooks":[{"type":"command","command":"mine"}]}]}}"#;
        fs::write(d.join(".claude/settings.json"), orig).unwrap();
        inst(&d, true).unwrap();
        assert_eq!(pre_cmds(&d), [PRE_CMD, "mine"]);
        assert_eq!(fs::read_to_string(d.join(".claude/settings.json.agentgc.bak")).unwrap(), orig);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn pre_tool_use_no_opinion() {
        assert!(pre_tool_use(r#"{"tool_name":"Read","tool_input":{"command":"cargo test"}}"#, false).is_none());
        assert!(pre_tool_use(r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#, false).is_none());
        assert!(pre_tool_use("not json", false).is_none());
        assert!(pre_tool_use("{}", false).is_none());
    }

    #[test]
    fn session_text_shows_status_env_and_rule() {
        let t = session_text("worktree: /x\n", "PORT=3100\n");
        assert!(t.contains("agentgc status:\nworktree: /x"));
        assert!(t.contains("PORT=3100"));
        assert!(t.contains("agentgc run -- COMMAND"));
        let t = session_text("", "");
        assert!(!t.contains("status:") && !t.contains("Task env"));
        assert!(t.contains("agentgc run --"));
    }

    #[test]
    fn dry_run_writes_nothing_and_prints_snippet() {
        let d = tmp("dry");
        let out = inst(&d, false).unwrap();
        assert!(out.contains("Dry run") && out.contains("agentgc hook pre-tool-use"));
        assert!(out.contains("agentgc hook session-start"));
        assert!(!d.join(".claude").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_creates_then_is_up_to_date() {
        let d = tmp("apply");
        inst(&d, true).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(d.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v, snippet(false));
        assert!(inst(&d, true).unwrap().contains("Already up to date"));
        assert!(!d.join(".claude/settings.json.agentgc.bak").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn apply_keeps_user_settings_and_makes_a_backup() {
        let d = tmp("keep");
        fs::create_dir_all(d.join(".claude")).unwrap();
        let orig = r#"{"model":"x","hooks":{"PreToolUse":[{"matcher":"Edit","hooks":[{"type":"command","command":"mine"}]}]}}"#;
        fs::write(d.join(".claude/settings.json"), orig).unwrap();
        inst(&d, true).unwrap();
        let v: Value = serde_json::from_str(&fs::read_to_string(d.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(v["model"], "x");
        let pre = v["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 2);
        assert_eq!(pre[0]["hooks"][0]["command"], "mine");
        assert_eq!(fs::read_to_string(d.join(".claude/settings.json.agentgc.bak")).unwrap(), orig);
        // an existing backup blocks a second change
        fs::write(d.join(".claude/settings.json"), "{}").unwrap();
        assert!(inst(&d, true).unwrap_err().contains("already exists"));
        assert_eq!(fs::read_to_string(d.join(".claude/settings.json")).unwrap(), "{}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn invalid_json_and_bad_shapes_are_refused() {
        let d = tmp("bad");
        fs::create_dir_all(d.join(".claude")).unwrap();
        for body in ["{ nope", "[]", r#"{"hooks":[]}"#, r#"{"hooks":{"SessionStart":{}}}"#] {
            fs::write(d.join(".claude/settings.json"), body).unwrap();
            assert!(inst(&d, true).is_err(), "{body}");
            assert_eq!(fs::read_to_string(d.join(".claude/settings.json")).unwrap(), body);
        }
        assert!(!d.join(".claude/settings.json.agentgc.bak").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn global_uses_the_injected_home() {
        let h = tmp("home");
        let hs = h.to_string_lossy().into_owned();
        let get = move |k: &str| if k == "HOME" { Some(hs.clone()) } else { None };
        let a = InstallArgs { global: true, dir: None, apply: true, allow: false };
        run_install(&a, &get).unwrap();
        assert!(h.join(".claude/settings.json").is_file());
        assert!(run_install(&a, &|_| None).is_err());
        let _ = fs::remove_dir_all(&h);
    }
}
