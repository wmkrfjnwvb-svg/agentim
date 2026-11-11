//! The business rule catalog: docs/en/business-rules.md
//! is generated from the `rule!` calls in the code and must match it, every
//! id has one error code, refusals in the business code are named rules,
//! and (in CI, after the other tests) every rule is triggered by a test.
//!
//! `AGENTIM_WRITE_RULES=1 cargo test --test rules` rewrites the catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const DOC: &str = "../../../docs/en/business-rules.md";
/// Rules no test triggers yet (kept short; a rule leaves this list as soon
/// as a test reaches it, and the check says so).
const UNTESTED: &str = "../../ci/rules-untested.txt";
/// Codes of a business refusal: these must be named rules in business code.
const REFUSALS: &[&str] = &[
    "bad_request",
    "not_found",
    "forbidden",
    "conflict",
    "blocked",
    "policy_blocked",
    "rate_limited",
    "budget_exceeded",
];

struct Rule {
    code: String,
    texts: BTreeSet<String>,
    files: BTreeSet<String>,
}

fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }
    let mut files = vec![];
    walk(Path::new("src"), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let name = p
                .strip_prefix("src")
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // Windows checkouts have CRLF line ends.
            (
                name,
                std::fs::read_to_string(&p).unwrap().replace("\r\n", "\n"),
            )
        })
        .collect()
}

/// The next string literal in `s` (the message, or the first part of its
/// `format!`), unescaped enough for a table.
fn first_literal(s: &str) -> String {
    let Some(start) = s.find('"') else {
        return String::new();
    };
    let mut out = String::new();
    let mut chars = s[start + 1..].chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            }
            '"' => break,
            c => out.push(c),
        }
    }
    out
}

fn catalog() -> BTreeMap<String, Rule> {
    let mut rules: BTreeMap<String, Rule> = BTreeMap::new();
    for (file, src) in sources() {
        if file == "lib.rs" {
            continue;
        }
        for (at, _) in src.match_indices("rule!(") {
            let rest = &src[at + 6..];
            let id = first_literal(rest);
            let after_id = &rest[rest.find(&format!("\"{id}\"")).unwrap() + id.len() + 2..];
            let code = first_literal(after_id);
            let after_code =
                &after_id[after_id.find(&format!("\"{code}\"")).unwrap() + code.len() + 2..];
            // The message is the third argument; a message held in a
            // variable has no text of its own here.
            let arg = after_code.trim_start_matches([',', ' ', '\n']);
            let text = if arg.starts_with('"') || arg.starts_with("format!") {
                first_literal(arg)
            } else {
                String::new()
            };
            assert!(
                !id.is_empty()
                    && id.contains('.')
                    && id.bytes().all(|b| b.is_ascii_lowercase()
                        || b.is_ascii_digit()
                        || b == b'_'
                        || b == b'.'),
                "{file}: bad rule id {id:?}"
            );
            let r = rules.entry(id.clone()).or_insert_with(|| Rule {
                code: code.clone(),
                texts: BTreeSet::new(),
                files: BTreeSet::new(),
            });
            assert_eq!(r.code, code, "{id} is refused with two codes");
            if !text.is_empty() {
                r.texts.insert(text);
            }
            r.files.insert(file.clone());
        }
    }
    rules
}

/// Where a check runs (docs/en/design/tracing.md): the session layer before the writer,
/// the conversation's writer, or the file store.
fn stage(file: &str) -> &'static str {
    if file.starts_with("logic") {
        "⑦ writer"
    } else if file == "filestore.rs" {
        "file store"
    } else {
        "③ session"
    }
}

const DOMAINS: &[(&str, &str)] = &[
    ("conv", "Conversations"),
    ("member", "Members"),
    ("tenant", "Tenants"),
    ("webhook", "Tenant webhooks"),
    ("block", "Blocks"),
    ("friend", "Friends"),
    ("msg", "Messages"),
    ("thread", "Threads"),
    ("reaction", "Reactions"),
    ("pin", "Pinned messages"),
    ("social", "Threads, reactions and pins: reading"),
    ("stream", "Streaming replies"),
    ("draft", "Drafts and approval"),
    ("approval", "Approval rules"),
    ("agent", "Agents"),
    ("role", "Agent roles"),
    ("config", "Agent configuration"),
    ("model_service", "Model services"),
    ("assistant", "Built-in assistants"),
    ("knowledge", "Agent knowledge files"),
    ("skill", "Skills"),
    ("connection", "Connections: MCP servers and external APIs"),
    ("request", "Asking people to decide"),
    ("task", "Tasks, claims and turns"),
    ("card", "Cards"),
    ("file", "Files"),
    ("history", "History, context, search, summaries"),
    ("state", "Checkpoints"),
    ("sync", "Sync"),
    ("session", "Session layer"),
    ("account", "Accounts, devices, tokens"),
    ("apikey", "API keys"),
    ("signing", "Signing keys"),
    ("report", "Reports and moderation"),
    ("admin", "Console and admin"),
    ("dye", "Dye rules"),
];

fn render(rules: &BTreeMap<String, Rule>) -> String {
    let mut t = String::from(
        "# Business rule catalog\n\n\
         <!-- Generated from the `rule!` calls in the code by server/crates/agentim-core/tests/rules.rs. Do not edit: AGENTIM_WRITE_RULES=1 cargo test --test rules -->\n\n\
         Every business check is a numbered rule ([tracing and business rules](design/tracing.md)). A refused request carries the rule in its error (`error.rule` in the client protocol, `rule` in the open API, `structuredContent.error.rule` in MCP), and the metric `agentim_refusals_total{op, code, rule}` counts refusals by rule. Codes: `forbidden` not allowed or not a member, `not_found` unknown or gone, `conflict` the state does not allow it now, `bad_request` invalid input or not applicable.\n\n\
         Stage: ③ session layer (before the writer, `hub`); ⑦ writer (in the same transaction as the write, `logic`); file store. The text is the error message in the code (`{}` is the actual value).\n\n",
    );
    let total = rules.len();
    t += &format!("{total} rules.\n\n");
    for (prefix, title) in DOMAINS {
        let mine: Vec<(&String, &Rule)> = rules
            .iter()
            .filter(|(id, _)| id.split('.').next() == Some(*prefix))
            .collect();
        if mine.is_empty() {
            continue;
        }
        t += &format!("## {title} (`{prefix}.*`, {})\n\n", mine.len());
        t += "| Rule | Code | Stage | Text | Code location |\n|---|---|---|---|---|\n";
        for (id, r) in mine {
            let stages: BTreeSet<&str> = r.files.iter().map(|f| stage(f)).collect();
            let texts: Vec<String> = r.texts.iter().map(|x| x.replace('|', "\\|")).collect();
            let files: Vec<String> = r.files.iter().map(|f| format!("`{f}`")).collect();
            t += &format!(
                "| `{id}` | `{}` | {} | {} | {} |\n",
                r.code,
                stages.into_iter().collect::<Vec<_>>().join(", "),
                if texts.is_empty() {
                    "—".into()
                } else {
                    texts.join("; ")
                },
                files.join(" ")
            );
        }
        t += "\n";
    }
    t
}

#[test]
fn the_catalog_matches_the_code() {
    let rules = catalog();
    for id in rules.keys() {
        let prefix = id.split('.').next().unwrap();
        assert!(
            DOMAINS.iter().any(|(p, _)| *p == prefix),
            "{id}: add its domain to DOMAINS"
        );
    }
    let doc = render(&rules);
    if std::env::var_os("AGENTIM_WRITE_RULES").is_some() {
        std::fs::write(DOC, &doc).unwrap();
    }
    let on_disk = std::fs::read_to_string(DOC)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        on_disk == doc,
        "docs/en/business-rules.md is out of date: AGENTIM_WRITE_RULES=1 cargo test --test rules"
    );
}

/// The business code (`hub`, `logic`) refuses only through named rules.
#[test]
fn business_refusals_are_named_rules() {
    let mut unnamed = vec![];
    for (file, src) in sources() {
        if !(file.starts_with("hub") || file.starts_with("logic") || file == "filestore.rs") {
            continue;
        }
        for (n, line) in src.lines().enumerate() {
            for code in REFUSALS {
                if line.contains(&format!("err(\"{code}\"")) {
                    unnamed.push(format!("{file}:{}: {}", n + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        unnamed.is_empty(),
        "refusals without a rule id (use rule!):\n{}",
        unnamed.join("\n")
    );
}

/// After the other tests ran with `AGENTIM_RULES_HIT` set (CI does this on
/// three databases), every rule was triggered, except the listed ones.
#[test]
#[ignore = "run by CI after the suite, with AGENTIM_RULES_HIT"]
fn every_rule_is_triggered_by_a_test() {
    let hit_file = std::env::var("AGENTIM_RULES_HIT").expect("AGENTIM_RULES_HIT");
    let hit: BTreeSet<String> = std::fs::read_to_string(hit_file)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    let untested: BTreeSet<String> = std::fs::read_to_string(UNTESTED)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    let rules = catalog();
    let missing: Vec<&String> = rules
        .keys()
        .filter(|id| !hit.contains(*id) && !untested.contains(*id))
        .collect();
    let now_tested: Vec<&String> = untested.iter().filter(|id| hit.contains(*id)).collect();
    let gone: Vec<&String> = untested
        .iter()
        .filter(|id| !rules.contains_key(*id))
        .collect();
    let covered = rules.keys().filter(|id| hit.contains(*id)).count();
    eprintln!("{covered} of {} rules triggered by a test", rules.len());
    assert!(
        missing.is_empty() && now_tested.is_empty() && gone.is_empty(),
        "no test triggers: {missing:?}\nnow tested, remove from {UNTESTED}: {now_tested:?}\nno longer rules, remove from {UNTESTED}: {gone:?}"
    );
}
