//! Variables in a script: `${fixture}` in, a path out.
//!
//! A test that runs against a directory made fresh for each run — a fixture
//! tree under the temp directory — would otherwise have to write that
//! directory's absolute path into every step that names a file, and replay
//! against nothing the next day. With variables the script says
//! `${fixture}/notes.txt` and the run says what `fixture` is:
//!
//! * replaying, `--var fixture=/tmp/run-7` (or `vars` on `replay_script`)
//!   puts the value into every string of every step's params;
//! * recording, `GPUI_MCP_RECORD_VARS=fixture=/tmp/run-7` (or `vars` on
//!   `start_recording`) does the reverse: every occurrence of the value in
//!   what is written down becomes `${fixture}`, so a recorded script is
//!   portable the moment it is written.
//!
//! Server-local, like the rest of the script machinery: the app only ever
//! sees the substituted params.
//!
//! A literal `${` in a script is written `$${`.

use std::collections::BTreeMap;

/// Variable name → value. Ordered so that a report or an error lists them
/// the same way every run.
pub type Vars = BTreeMap<String, String>;

/// Parse `name=value` — one `--var`, or one entry of
/// `GPUI_MCP_RECORD_VARS`.
pub fn parse_assignment(text: &str) -> anyhow::Result<(String, String)> {
    let (name, value) = text
        .split_once('=')
        .ok_or_else(|| anyhow::anyhow!("a variable is name=value, not {text:?}"))?;
    let name = name.trim();
    if !is_name(name) {
        anyhow::bail!(
            "{name:?} is not a variable name: letters, digits and _, not starting with a digit"
        );
    }
    Ok((name.to_string(), value.to_string()))
}

/// Parse `GPUI_MCP_RECORD_VARS`: assignments separated by `;`, the one
/// character no temp directory a test makes contains in practice and that
/// is not the platform's path-list separator on either side (`:` would
/// split a Windows drive letter).
pub fn parse_list(text: &str) -> anyhow::Result<Vars> {
    text.split(';')
        .filter(|entry| !entry.trim().is_empty())
        .map(parse_assignment)
        .collect()
}

/// Read a `vars` object off a tool's arguments: `{"fixture": "/tmp/x"}`.
pub fn from_arguments(arguments: &serde_json::Value) -> anyhow::Result<Vars> {
    let Some(vars) = arguments.get("vars").filter(|vars| !vars.is_null()) else {
        return Ok(Vars::new());
    };
    let object = vars
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("vars is an object of name → string, not {vars}"))?;
    object
        .iter()
        .map(|(name, value)| {
            if !is_name(name) {
                anyhow::bail!("{name:?} is not a variable name");
            }
            let value = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("the value of {name} must be a string"))?;
            Ok((name.clone(), value.to_string()))
        })
        .collect()
}

fn is_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Every variable a value uses, in order of first use, each once.
pub fn used(value: &serde_json::Value) -> Vec<String> {
    let mut names = Vec::new();
    visit_strings(value, &mut |text| {
        for piece in pieces(text) {
            if let Piece::Variable(name) = piece {
                if !names.iter().any(|known| known == name) {
                    names.push(name.to_string());
                }
            }
        }
    });
    names
}

/// Put the values in. Every `${name}` must have one — a caller checks with
/// [`used`] first and refuses the whole run, because a step that clicked
/// `${fixture}/a.txt` literally would fail somewhere far less obvious.
pub fn substitute(value: &serde_json::Value, vars: &Vars) -> serde_json::Value {
    map_strings(value, &|text| {
        if !text.contains('$') {
            return text.to_string();
        }
        pieces(text)
            .map(|piece| match piece {
                Piece::Text(text) => text.to_string(),
                Piece::Dollar => "$".to_string(),
                Piece::Variable(name) => vars
                    .get(name)
                    .cloned()
                    .unwrap_or_else(|| format!("${{{name}}}")),
            })
            .collect()
    })
}

/// Take the values out: what a recorder writes instead of the fixture's
/// path. A literal `${` already in the text is escaped first, so the result
/// reads back as exactly the text it came from.
///
/// Longer values first, so a variable for `/tmp/run/sub` is not eaten by one
/// for `/tmp/run`. Values are compared as given; a caller that knows a path
/// may come back canonicalised (`/private/tmp` on macOS) registers both
/// spellings under one name — see [`with_canonical_spellings`].
pub fn abstract_values(value: &serde_json::Value, vars: &[(String, String)]) -> serde_json::Value {
    let mut ordered: Vec<&(String, String)> =
        vars.iter().filter(|(_, value)| !value.is_empty()).collect();
    ordered.sort_by_key(|(_, value)| std::cmp::Reverse(value.len()));

    map_strings(value, &|text| {
        let mut text = text.replace("${", "$${");
        for (name, value) in &ordered {
            text = text.replace(value.as_str(), &format!("${{{name}}}"));
        }
        text
    })
}

/// The values to look for when recording: each one as given and, when it
/// names an existing path that canonicalises to something else, that too.
/// An app reports the directory it is in, which on macOS is `/private/var/…`
/// for a temp directory made as `/var/…`; both are the fixture.
pub fn with_canonical_spellings(vars: &Vars) -> Vec<(String, String)> {
    let mut spellings = Vec::new();
    for (name, value) in vars {
        spellings.push((name.clone(), value.clone()));
        if let Ok(canonical) = std::fs::canonicalize(value) {
            let canonical = canonical.to_string_lossy().to_string();
            if canonical != *value {
                spellings.push((name.clone(), canonical));
            }
        }
    }
    spellings
}

enum Piece<'a> {
    Text(&'a str),
    /// `$${`, which stands for a literal `${`: the `$` of it.
    Dollar,
    Variable(&'a str),
}

/// Split a string into text, escaped dollars and `${name}` references. A `$`
/// that starts neither stays text, as does a `${` that is not closed or does
/// not hold a name — a script about shell snippets should not break.
fn pieces(text: &str) -> impl Iterator<Item = Piece<'_>> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        if rest.starts_with("$${") {
            // `$$` stands for one `$`; the `{` after it is plain text, so
            // what follows cannot be read as a reference.
            rest = &rest[2..];
            return Some(Piece::Dollar);
        }
        if let Some(after) = rest.strip_prefix("${") {
            if let Some(end) = after.find('}') {
                let name = &after[..end];
                if is_name(name) {
                    rest = &after[end + 1..];
                    return Some(Piece::Variable(name));
                }
            }
        }
        // Text up to the next `$` that could start something, keeping at
        // least one character so a lone `$` moves on.
        let next = rest[1..].find('$').map_or(rest.len(), |at| at + 1);
        let (text, after) = rest.split_at(next);
        rest = after;
        Some(Piece::Text(text))
    })
}

fn visit_strings(value: &serde_json::Value, visit: &mut impl FnMut(&str)) {
    match value {
        serde_json::Value::String(text) => visit(text),
        serde_json::Value::Array(items) => items.iter().for_each(|item| visit_strings(item, visit)),
        serde_json::Value::Object(object) => {
            object.values().for_each(|item| visit_strings(item, visit))
        }
        _ => {}
    }
}

fn map_strings(value: &serde_json::Value, map: &impl Fn(&str) -> String) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => serde_json::Value::String(map(text)),
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.iter().map(|item| map_strings(item, map)).collect())
        }
        serde_json::Value::Object(object) => serde_json::Value::Object(
            object
                .iter()
                .map(|(key, item)| (key.clone(), map_strings(item, map)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn vars(entries: &[(&str, &str)]) -> Vars {
        entries
            .iter()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect()
    }

    #[test]
    fn a_variable_is_put_into_every_string_of_the_params() {
        let params = json!({
            "app_state_path": "/app/panels/left/path",
            "app_state_equals": "${fixture}/sub",
            "steps": [{ "method": "type_text", "params": { "text": "${fixture}" } }],
            "timeout_ms": 500,
        });
        let out = substitute(&params, &vars(&[("fixture", "/tmp/run-7")]));
        assert_eq!(out["app_state_equals"], "/tmp/run-7/sub");
        assert_eq!(out["steps"][0]["params"]["text"], "/tmp/run-7");
        assert_eq!(out["timeout_ms"], 500);
        assert_eq!(out["app_state_path"], "/app/panels/left/path");
    }

    #[test]
    fn used_lists_each_variable_once_in_order() {
        let params = json!({ "a": "${b} ${a}", "c": ["${b}", "$${literal}", "${not a name}"] });
        assert_eq!(used(&params), vec!["b".to_string(), "a".to_string()]);
    }

    #[test]
    fn a_literal_dollar_brace_survives_both_directions() {
        let recorded = abstract_values(
            &json!("echo ${HOME} in /tmp/run"),
            &[("fixture".into(), "/tmp/run".into())],
        );
        assert_eq!(recorded, json!("echo $${HOME} in ${fixture}"));
        assert!(used(&recorded).contains(&"fixture".to_string()));
        assert!(!used(&recorded).contains(&"HOME".to_string()));
        let replayed = substitute(&recorded, &vars(&[("fixture", "/tmp/run")]));
        assert_eq!(replayed, json!("echo ${HOME} in /tmp/run"));
    }

    #[test]
    fn the_longer_value_wins() {
        let recorded = abstract_values(
            &json!(["/tmp/run/sub/x", "/tmp/run/y"]),
            &[
                ("root".into(), "/tmp/run".into()),
                ("sub".into(), "/tmp/run/sub".into()),
            ],
        );
        assert_eq!(recorded, json!(["${sub}/x", "${root}/y"]));
    }

    #[test]
    fn a_lone_dollar_is_text() {
        let out = substitute(&json!("costs $5 and ${"), &vars(&[]));
        assert_eq!(out, json!("costs $5 and ${"));
    }

    #[test]
    fn assignments_are_name_equals_value() {
        assert_eq!(
            parse_assignment("fixture=/tmp/a=b").unwrap(),
            ("fixture".to_string(), "/tmp/a=b".to_string())
        );
        assert!(parse_assignment("fixture").is_err());
        assert!(parse_assignment("1st=/tmp").is_err());
        assert_eq!(
            parse_list("a=/x; b=/y;").unwrap(),
            vars(&[("a", "/x"), ("b", "/y")])
        );
        assert_eq!(
            from_arguments(&json!({ "vars": { "a": "/x" } })).unwrap(),
            vars(&[("a", "/x")])
        );
        assert!(from_arguments(&json!({ "vars": { "a": 3 } })).is_err());
        assert!(from_arguments(&json!({})).unwrap().is_empty());
    }

    #[test]
    fn a_canonical_spelling_is_recorded_under_the_same_name() {
        let dir = std::env::temp_dir();
        let given = dir.to_string_lossy().to_string();
        let spellings = with_canonical_spellings(&vars(&[("fixture", &given)]));
        assert!(spellings.contains(&("fixture".to_string(), given.clone())));
        let canonical = std::fs::canonicalize(&dir).unwrap();
        assert!(spellings
            .iter()
            .any(|(_, value)| std::path::Path::new(value) == canonical));
    }
}
