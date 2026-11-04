//! Compose file rendering for `task new`. Plain text replace, no template crate.
//! Placeholders: `{{task}}` `{{db}}` `{{worktree}}` `{{port.NAME}}` `{{env.NAME}}`.
use std::collections::BTreeMap;

/// Replace every `{{key}}` with its value. An unknown key is an error that names it.
/// A `{{` without a closing `}}` stays as it is.
pub fn render(tpl: &str, vars: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = tpl;
    while let Some(i) = rest.find("{{") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 2..];
        match after.find("}}") {
            Some(j) => {
                let key = after[..j].trim();
                match vars.get(key) {
                    Some(v) => out.push_str(v),
                    None => return Err(format!("unknown placeholder {{{{{key}}}}} in the compose template")),
                }
                rest = &after[j + 2..];
            }
            None => {
                out.push_str("{{");
                rest = after;
            }
        }
    }
    out.push_str(rest);
    Ok(out)
}

/// `AGENTGC_ROUTE_<NAME>` key for a route name.
pub fn route_env_key(name: &str) -> String {
    format!("AGENTGC_ROUTE_{}", name.to_uppercase().replace('-', "_"))
}

/// `<name>.<task>.localhost:<port>`
pub fn route_host(name: &str, task: &str, port: u16) -> String {
    format!("{name}.{task}.localhost:{port}")
}

pub fn project_name(task: &str) -> String {
    format!("agentgc-{task}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars() -> BTreeMap<String, String> {
        [("task", "demo"), ("port.web", "3101"), ("db", "p_demo"), ("env.FOO", "bar"), ("worktree", "/w/demo")]
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn replaces_all_placeholders() {
        let t = "name: {{task}}\nports: [\"{{port.web}}:80\"]\ndb: {{ db }}\nf: {{env.FOO}} {{worktree}} {{task}}\n";
        assert_eq!(
            render(t, &vars()).unwrap(),
            "name: demo\nports: [\"3101:80\"]\ndb: p_demo\nf: bar /w/demo demo\n"
        );
    }

    #[test]
    fn unknown_placeholder_names_it() {
        let e = render("a {{port.api}} b", &vars()).unwrap_err();
        assert!(e.contains("{{port.api}}"), "{e}");
        let e = render("{{nope}}", &vars()).unwrap_err();
        assert!(e.contains("{{nope}}"), "{e}");
    }

    #[test]
    fn leaves_other_text_alone() {
        assert_eq!(render("${HOME} {x} {{ open", &vars()).unwrap(), "${HOME} {x} {{ open");
        assert_eq!(render("", &vars()).unwrap(), "");
    }

    #[test]
    fn route_helpers() {
        assert_eq!(route_env_key("my-web"), "AGENTGC_ROUTE_MY_WEB");
        assert_eq!(route_host("web", "t1", 3100), "web.t1.localhost:3100");
        assert_eq!(project_name("t1"), "agentgc-t1");
    }
}
