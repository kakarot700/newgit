//! Hand-rolled argument parser (dependency policy D-002).
//!
//! Grammar:
//! * `--flag` → boolean flag
//! * `--key value` / `--key=value` → value flag (key must be declared)
//! * `-x` short aliases map to declared long names
//! * `--` ends flag parsing; the rest are positional
//! * undeclared `--key value` pairs: `--key` becomes a boolean flag and
//!   `value` a positional (strict mode rejects unknown flags instead).

use std::collections::BTreeMap;

use crate::error::{Error, Result};

#[derive(Clone, Debug, Default)]
pub struct Args {
    positional: Vec<String>,
    flags: BTreeMap<String, Option<String>>,
}

/// (short, long) aliases, e.g. ("m", "message").
pub type Aliases = &'static [(&'static str, &'static str)];

pub const COMMON_ALIASES: Aliases = &[
    ("m", "message"),
    ("w", "workspace"),
    ("n", "limit"),
    ("f", "force"),
    ("C", "repo"),
];

impl Args {
    pub fn parse(tokens: &[String], value_flags: &[&str], aliases: Aliases) -> Result<Args> {
        let mut a = Args::default();
        let mut only_positional = false;
        let mut i = 0;
        while i < tokens.len() {
            let t = &tokens[i];
            if only_positional {
                a.positional.push(t.clone());
                i += 1;
                continue;
            }
            if t == "--" {
                only_positional = true;
                i += 1;
                continue;
            }
            if let Some(rest) = t.strip_prefix("--") {
                let (key, inline_val) = match rest.split_once('=') {
                    Some((k, v)) => (k.to_string(), Some(v.to_string())),
                    None => (rest.to_string(), None),
                };
                if key.is_empty() {
                    return Err(Error::Invalid("empty flag name".into()));
                }
                if value_flags.contains(&key.as_str()) {
                    let v = match inline_val {
                        Some(v) => v,
                        None => {
                            i += 1;
                            match tokens.get(i) {
                                Some(v) => v.clone(),
                                None => {
                                    return Err(Error::Invalid(format!(
                                        "flag --{key} requires a value"
                                    )))
                                }
                            }
                        }
                    };
                    a.flags.insert(key, Some(v));
                } else {
                    if inline_val.is_some() {
                        return Err(Error::Invalid(format!(
                            "flag --{key} does not take a value"
                        )));
                    }
                    a.flags.insert(key, None);
                }
                i += 1;
                continue;
            }
            if t.len() >= 2 && t.starts_with('-') && !t.starts_with("--") {
                let short = &t[1..];
                if let Some((_, long)) = aliases.iter().find(|(s, _)| *s == short) {
                    let long = long.to_string();
                    if value_flags.contains(&long.as_str()) {
                        i += 1;
                        match tokens.get(i) {
                            Some(v) => {
                                a.flags.insert(long, Some(v.clone()));
                            }
                            None => {
                                return Err(Error::Invalid(format!(
                                    "flag -{short} requires a value"
                                )))
                            }
                        }
                    } else {
                        a.flags.insert(long, None);
                    }
                    i += 1;
                    continue;
                }
                return Err(Error::Invalid(format!("unknown short flag -{short}")));
            }
            a.positional.push(t.clone());
            i += 1;
        }
        Ok(a)
    }

    pub fn flag(&self, key: &str) -> bool {
        self.flags.contains_key(key)
    }

    pub fn opt(&self, key: &str) -> Option<&str> {
        self.flags.get(key).and_then(|v| v.as_deref())
    }

    pub fn req(&self, key: &str) -> Result<&str> {
        self.opt(key).ok_or_else(|| {
            Error::Invalid(format!("missing required flag --{key} (see `newgit help`)"))
        })
    }

    pub fn positional(&self) -> &[String] {
        &self.positional
    }

    pub fn pos(&self, i: usize) -> Option<&str> {
        self.positional.get(i).map(String::as_str)
    }

    pub fn pos_req(&self, i: usize, what: &str) -> Result<&str> {
        self.pos(i)
            .ok_or_else(|| Error::Invalid(format!("missing argument <{what}>")))
    }

    /// Reject flags that were not expected by this command (typo protection).
    pub fn reject_unknown(&self, known: &[&str]) -> Result<()> {
        for k in self.flags.keys() {
            if !known.contains(&k.as_str()) {
                return Err(Error::Invalid(format!(
                    "unknown flag --{k} (known: {})",
                    known.join(", ")
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn parse_basics() {
        let a = Args::parse(
            &toks("-m hello --workspace ws1 extra --force"),
            &["message", "workspace"],
            COMMON_ALIASES,
        )
        .unwrap();
        assert_eq!(a.opt("message"), Some("hello"));
        assert_eq!(a.opt("workspace"), Some("ws1"));
        assert!(a.flag("force"));
        assert_eq!(a.positional(), &["extra".to_string()]);
    }

    #[test]
    fn inline_values() {
        // inline values keep spaces (single argv token)
        let tokens: Vec<String> = vec!["--message=hi there".into(), "--limit=5".into()];
        let a = Args::parse(&tokens, &["message", "limit"], COMMON_ALIASES).unwrap();
        assert_eq!(a.opt("message"), Some("hi there"));
        assert_eq!(a.opt("limit"), Some("5"));
    }

    #[test]
    fn double_dash_ends_flags() {
        let a = Args::parse(&toks("-- --weird name"), &[], COMMON_ALIASES).unwrap();
        assert_eq!(a.positional(), &["--weird".to_string(), "name".to_string()]);
    }

    #[test]
    fn errors() {
        assert!(Args::parse(&toks("-m"), &["message"], COMMON_ALIASES).is_err());
        assert!(Args::parse(&toks("-Z x"), &[], COMMON_ALIASES).is_err());
        assert!(Args::parse(&toks("--force=1"), &[], COMMON_ALIASES).is_err());
        let a = Args::parse(&toks("--frobnicate"), &[], COMMON_ALIASES).unwrap();
        assert!(a.reject_unknown(&["json"]).is_err());
    }

    #[test]
    fn value_starting_with_dash() {
        let a = Args::parse(
            &toks("--message --not-a-flag"),
            &["message"],
            COMMON_ALIASES,
        )
        .unwrap();
        assert_eq!(a.opt("message"), Some("--not-a-flag"));
    }
}
