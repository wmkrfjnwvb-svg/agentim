//! Agent roles as Markdown (docs/en/design/agents.md K1): a YAML header and the role's
//! instructions as the body, the same shape as a Claude Code subagent file
//! (`agents/*.md`: `name`, `description`, `tools`, `model`, then the
//! prompt), so those files load unchanged. The header is a small, strict
//! subset of YAML: `key: value`, `key: [a, b]`, a block list (`- a`) or a
//! one-level map under a key, and `#` comments. Anything else is refused,
//! never guessed.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

/// Largest role file.
pub const MAX_BYTES: usize = 64 * 1024;
/// Largest instructions (the body).
pub const MAX_INSTRUCTIONS: usize = 32 * 1024;
pub const MAX_DESCRIPTION: usize = 1024;
pub const MAX_TAGS: usize = 10;
pub const MAX_TAG: usize = 32;
/// Most entries in one list (tools, skills, mcp, apis).
pub const MAX_LIST: usize = 64;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Role {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    /// Declared for the agent's runtime (the IM does not enforce them).
    pub tools: Vec<String>,
    pub model: Option<String>,
    /// Capabilities by name (resolved when the library exists, K2).
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
    pub apis: Vec<String>,
    /// Every other header key, kept as written (e.g. Claude Code's `color`).
    pub metadata: BTreeMap<String, Value>,
    pub instructions: String,
}

/// Names like Claude Code's: lowercase letters, digits and `-`, 1..64, not
/// starting or ending with `-`.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 64
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !n.starts_with('-')
        && !n.ends_with('-')
}

fn scalar(raw: &str) -> String {
    let t = raw.trim();
    for q in ['"', '\''] {
        if t.len() >= 2 && t.starts_with(q) && t.ends_with(q) {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

fn inline_list(raw: &str) -> Option<Vec<String>> {
    let t = raw.trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    Some(
        inner
            .split(',')
            .map(scalar)
            .filter(|s| !s.is_empty())
            .collect(),
    )
}

/// The header as JSON values: strings, lists of strings, or maps of those.
fn header(text: &str) -> Result<BTreeMap<String, Value>, String> {
    let mut out: BTreeMap<String, Value> = BTreeMap::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].trim_end();
        i += 1;
        if line.trim().is_empty() || line.trim_start().starts_with('#') {
            continue;
        }
        if line.starts_with(' ') || line.starts_with('\t') {
            return Err(format!("header line {i}: unexpected indentation"));
        }
        let Some((k, v)) = line.split_once(':') else {
            return Err(format!("header line {i}: expected key: value"));
        };
        let key = k.trim().to_string();
        if key.is_empty()
            || !key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        {
            return Err(format!("header line {i}: invalid key {key:?}"));
        }
        if out.contains_key(&key) {
            return Err(format!("header: {key} appears twice"));
        }
        let v = v.trim();
        let value = if !v.is_empty() {
            if v.starts_with('[') {
                json!(inline_list(v).ok_or(format!("header line {i}: unclosed list"))?)
            } else if v.starts_with('{') || v.starts_with('|') || v.starts_with('>') {
                return Err(format!(
                    "header line {i}: {key}: only plain values and lists are supported"
                ));
            } else {
                json!(scalar(v))
            }
        } else {
            // A block list or a one-level map, indented under the key.
            let mut list = vec![];
            let mut map = serde_json::Map::new();
            while i < lines.len() {
                let l = lines[i];
                if l.trim().is_empty() || l.trim_start().starts_with('#') {
                    i += 1;
                    continue;
                }
                if !(l.starts_with(' ') || l.starts_with('\t')) {
                    break;
                }
                i += 1;
                let t = l.trim();
                if let Some(item) = t
                    .strip_prefix("- ")
                    .or(if t == "-" { Some("") } else { None })
                {
                    if !map.is_empty() {
                        return Err(format!("header line {i}: {key} mixes a list and a map"));
                    }
                    list.push(scalar(item));
                } else if let Some((mk, mv)) = t.split_once(':') {
                    if !list.is_empty() {
                        return Err(format!("header line {i}: {key} mixes a list and a map"));
                    }
                    let mv = mv.trim();
                    let mval = if mv.starts_with('[') {
                        json!(inline_list(mv).ok_or(format!("header line {i}: unclosed list"))?)
                    } else if mv.is_empty() {
                        return Err(format!(
                            "header line {i}: {key} nests deeper than one level"
                        ));
                    } else {
                        json!(scalar(mv))
                    };
                    map.insert(mk.trim().to_string(), mval);
                } else {
                    return Err(format!("header line {i}: expected - item or key: value"));
                }
            }
            if !map.is_empty() {
                Value::Object(map)
            } else {
                json!(list)
            }
        };
        out.insert(key, value);
    }
    Ok(out)
}

/// A list field: a YAML list, or a comma-separated string (Claude Code
/// writes `tools: Read, Grep, Glob`).
fn list_of(h: &BTreeMap<String, Value>, key: &str) -> Result<Vec<String>, String> {
    let v: Vec<String> = match h.get(key) {
        None => return Ok(vec![]),
        Some(Value::String(s)) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| x.as_str().unwrap_or("").to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        Some(_) => return Err(format!("{key}: a list is expected")),
    };
    if v.len() > MAX_LIST {
        return Err(format!("{key}: at most {MAX_LIST} entries"));
    }
    if v.iter()
        .any(|x: &String| x.len() > 255 || x.chars().any(char::is_control))
    {
        return Err(format!("{key}: entries are at most 255 characters"));
    }
    Ok(v)
}

/// Parses a role file. `name` is the name it is stored under: the header's
/// `name`, when present, must be the same.
pub fn parse(name: &str, md: &str) -> Result<Role, (&'static str, String)> {
    let bad = |r: &'static str, m: String| Err((r, m));
    if !valid_name(name) {
        return bad(
            "role.name",
            "role names are 1..64 of a-z 0-9 - (not starting or ending with -)".into(),
        );
    }
    if md.len() > MAX_BYTES {
        return bad(
            "role.size",
            format!("a role file is at most {MAX_BYTES} bytes"),
        );
    }
    let md = md
        .strip_prefix('\u{feff}')
        .unwrap_or(md)
        .replace("\r\n", "\n");
    let Some(rest) = md.strip_prefix("---\n") else {
        return bad(
            "role.header",
            "a role file starts with a --- header (name, description), then the instructions"
                .into(),
        );
    };
    let (head, body) = match rest.find("\n---\n") {
        Some(p) => (&rest[..p], &rest[p + 5..]),
        None => match rest.strip_suffix("\n---") {
            Some(h) => (h, ""),
            None if rest.starts_with("---\n") => ("", &rest[4..]),
            None => return bad("role.header", "the header is not closed with ---".into()),
        },
    };
    let h = match header(head) {
        Ok(h) => h,
        Err(e) => return bad("role.header", e),
    };
    let text = |k: &str| h.get(k).and_then(Value::as_str).map(str::to_string);
    if let Some(n) = text("name").filter(|n| n != name) {
        return bad(
            "role.name_mismatch",
            format!("the header names {n:?}, not {name:?}"),
        );
    }
    if h.contains_key("name") && text("name").is_none() {
        return bad("role.header", "name: a single value is expected".into());
    }
    let description = text("description").unwrap_or_default();
    if description.trim().is_empty() || description.len() > MAX_DESCRIPTION {
        return bad(
            "role.description",
            format!("description: 1..{MAX_DESCRIPTION} characters, what the role is for"),
        );
    }
    let instructions = body.trim().to_string();
    if instructions.len() > MAX_INSTRUCTIONS {
        return bad(
            "role.size",
            format!("the instructions are at most {MAX_INSTRUCTIONS} bytes"),
        );
    }
    let lists = (|| -> Result<_, String> {
        let tags = list_of(&h, "tags")?;
        if tags.len() > MAX_TAGS || tags.iter().any(|t| t.chars().count() > MAX_TAG) {
            return Err(format!(
                "tags: at most {MAX_TAGS}, each at most {MAX_TAG} characters"
            ));
        }
        Ok((
            tags,
            list_of(&h, "tools")?,
            list_of(&h, "skills")?,
            list_of(&h, "mcp")?,
            list_of(&h, "apis")?,
        ))
    })();
    let (tags, tools, skills, mcp, apis) = match lists {
        Ok(v) => v,
        Err(e) => return bad("role.header", e),
    };
    if h.contains_key("model") && text("model").is_none() {
        return bad("role.header", "model: a single value is expected".into());
    }
    let metadata = h
        .iter()
        .filter(|(k, _)| {
            !matches!(
                k.as_str(),
                "name" | "description" | "tags" | "tools" | "model" | "skills" | "mcp" | "apis"
            )
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    Ok(Role {
        name: name.to_string(),
        description,
        tags,
        tools,
        model: text("model"),
        skills,
        mcp,
        apis,
        metadata,
        instructions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_claude_code_subagent_file_loads() {
        let md = "---\nname: code-reviewer\ndescription: Reviews code for quality and security. Use after changes.\ntools: Read, Grep, Glob, Bash\nmodel: inherit\ncolor: blue\n---\n\nYou are a senior code reviewer.\n\nWhen invoked:\n1. Run git diff\n";
        let r = parse("code-reviewer", md).unwrap();
        assert_eq!(r.tools, vec!["Read", "Grep", "Glob", "Bash"]);
        assert_eq!(r.model.as_deref(), Some("inherit"));
        assert_eq!(r.metadata["color"], "blue");
        assert!(
            r.instructions
                .starts_with("You are a senior code reviewer.")
        );
        assert!(r.instructions.ends_with("1. Run git diff"));
    }

    #[test]
    fn our_fields_and_yaml_shapes() {
        let md = "\u{feff}---\r\n# a comment\r\ndescription: 'Sales helper: answers leads'\r\ntags: [sales, \"crm\"]\r\nskills:\r\n  - pricing\r\n  - - \r\napproval:\r\n  mode: rules\r\n  rules: [links, files]\r\n---\r\nBe brief.\r\n";
        let r = parse("sales", md).unwrap();
        assert_eq!(r.name, "sales");
        assert_eq!(r.description, "Sales helper: answers leads");
        assert_eq!(r.tags, vec!["sales", "crm"]);
        assert_eq!(r.skills, vec!["pricing", "-"]);
        assert_eq!(
            r.metadata["approval"],
            json!({"mode": "rules", "rules": ["links", "files"]})
        );
        assert_eq!(r.instructions, "Be brief.");
        // No body; an empty header except the description.
        let r = parse("x", "---\ndescription: d\n---").unwrap();
        assert_eq!(r.instructions, "");
    }

    #[test]
    fn refusals() {
        let ok = "---\ndescription: d\n---\nhi";
        assert!(parse("x", ok).is_ok());
        for (name, md, rule) in [
            ("", ok, "role.name"),
            ("Upper", ok, "role.name"),
            ("-x", ok, "role.name"),
            ("x-", ok, "role.name"),
            (&"a".repeat(65) as &str, ok, "role.name"),
            ("x", "no header", "role.header"),
            ("x", "---\ndescription: d\nhi", "role.header"),
            (
                "x",
                "---\nname: y\ndescription: d\n---\n",
                "role.name_mismatch",
            ),
            ("x", "---\nname: x\n---\n", "role.description"),
            ("x", "---\ndescription:   \n---\n", "role.description"),
            (
                "x",
                "---\ndescription: d\ndescription: e\n---\n",
                "role.header",
            ),
            (
                "x",
                "---\ndescription: d\n  indented: 1\n---\n",
                "role.header",
            ),
            ("x", "---\ndescription: d\nnokey\n---\n", "role.header"),
            ("x", "---\ndescription: |\n  long\n---\n", "role.header"),
            (
                "x",
                "---\ndescription: d\ntags: [a, b\n---\n",
                "role.header",
            ),
            (
                "x",
                "---\ndescription: d\nm:\n  a:\n    b: c\n---\n",
                "role.header",
            ),
            (
                "x",
                "---\ndescription: d\nm:\n  - a\n  b: c\n---\n",
                "role.header",
            ),
            ("x", "---\ndescription: d\nmodel: [a]\n---\n", "role.header"),
            ("x", "---\ndescription: d\nname: [x]\n---\n", "role.header"),
            (
                "x",
                "---\ndescription: d\ntools:\n  a: b\n---\n",
                "role.header",
            ),
        ] {
            assert_eq!(
                parse(name, md).map(|_| ()).unwrap_err().0,
                rule,
                "{name} {md:?}"
            );
        }
        let d = "d".repeat(MAX_DESCRIPTION);
        assert!(parse("x", &format!("---\ndescription: {d}\n---\n")).is_ok());
        let d = "d".repeat(MAX_DESCRIPTION + 1);
        assert_eq!(
            parse("x", &format!("---\ndescription: {d}\n---\n"))
                .unwrap_err()
                .0,
            "role.description"
        );
        let body = "b".repeat(MAX_INSTRUCTIONS);
        assert!(parse("x", &format!("---\ndescription: d\n---\n{body}")).is_ok());
        assert_eq!(
            parse("x", &format!("---\ndescription: d\n---\n{body}b"))
                .unwrap_err()
                .0,
            "role.size"
        );
        assert_eq!(
            parse("x", &"x".repeat(MAX_BYTES + 1)).unwrap_err().0,
            "role.size"
        );
        let tags: Vec<String> = (0..=MAX_TAGS).map(|i| format!("t{i}")).collect();
        assert_eq!(
            parse(
                "x",
                &format!("---\ndescription: d\ntags: [{}]\n---\n", tags.join(", "))
            )
            .unwrap_err()
            .0,
            "role.header"
        );
        let long = "t".repeat(MAX_TAG + 1);
        assert_eq!(
            parse("x", &format!("---\ndescription: d\ntags: [{long}]\n---\n"))
                .unwrap_err()
                .0,
            "role.header"
        );
        assert!(
            parse(
                "x",
                &format!(
                    "---\ndescription: d\ntags: [{}]\n---\n",
                    "t".repeat(MAX_TAG)
                )
            )
            .is_ok()
        );
    }
}
