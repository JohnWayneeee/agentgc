use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Rule {
    pub id: String,
    pub name: String,
    pub paths: Vec<String>,
    pub safety: Safety,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub clean_cmd: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Safety {
    Safe,
    Ask,
}

impl Safety {
    pub fn as_str(&self) -> &'static str {
        match self {
            Safety::Safe => "safe",
            Safety::Ask => "ask",
        }
    }
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    #[serde(default)]
    rule: Vec<Rule>,
}

pub const DEFAULT_RULES: &str = include_str!("../rules/default.toml");

pub fn parse_rules(text: &str) -> Result<Vec<Rule>, String> {
    toml::from_str::<RuleFile>(text)
        .map(|f| f.rule)
        .map_err(|e| format!("invalid rules: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_rule() {
        let r = parse_rules("[[rule]]\nid=\"a\"\nname=\"A\"\npaths=[\"x\"]\nsafety=\"ask\"\n").unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].safety, Safety::Ask);
        assert_eq!(r[0].note, "");
        assert!(r[0].clean_cmd.is_none());
    }

    #[test]
    fn parses_clean_cmd() {
        let r = parse_rules(
            "[[rule]]\nid=\"a\"\nname=\"A\"\npaths=[]\nsafety=\"safe\"\nnote=\"n\"\nclean_cmd=\"c\"\n",
        )
        .unwrap();
        assert_eq!(r[0].clean_cmd.as_deref(), Some("c"));
    }

    #[test]
    fn rejects_bad_safety() {
        assert!(parse_rules("[[rule]]\nid=\"a\"\nname=\"A\"\npaths=[]\nsafety=\"nope\"\n").is_err());
    }

    #[test]
    fn default_rules_parse() {
        let r = parse_rules(DEFAULT_RULES).unwrap();
        assert!(r.len() >= 15);
        assert!(r.iter().all(|x| !x.paths.is_empty()));
    }
}
