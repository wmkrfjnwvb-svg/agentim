//! The business rule catalog (docs/14 §5.1, D142): docs/15-business-rules.md
//! is generated from the `rule!` calls in the code and must match it, every
//! id has one error code, refusals in the business code are named rules,
//! and (in CI, after the other tests) every rule is triggered by a test.
//!
//! `AGENTIM_WRITE_RULES=1 cargo test --test rules` rewrites the catalog.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const DOC: &str = "../../../docs/15-business-rules.md";
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
            (name, std::fs::read_to_string(&p).unwrap())
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

/// Where a check runs (docs/14 §3): the session layer before the writer,
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
    ("conv", "会话"),
    ("member", "成员"),
    ("block", "拉黑"),
    ("msg", "消息"),
    ("stream", "流式回复"),
    ("draft", "草稿和审批"),
    ("agent", "Agent"),
    ("request", "请人决定"),
    ("task", "任务、抢单、发言权"),
    ("card", "卡片"),
    ("file", "文件"),
    ("history", "历史、上下文、搜索、摘要"),
    ("state", "检查点"),
    ("sync", "同步"),
    ("session", "会话层"),
    ("account", "账号、设备、令牌"),
    ("apikey", "API 密钥"),
    ("signing", "签名密钥"),
    ("report", "举报和审核"),
    ("admin", "控制台和管理"),
    ("dye", "染色规则"),
];

fn render(rules: &BTreeMap<String, Rule>) -> String {
    let mut t = String::from(
        "# 15 业务规则目录\n\n\
         <!-- 由 server/crates/agentim-core/tests/rules.rs 从代码里的 `rule!` 生成，不要手改：AGENTIM_WRITE_RULES=1 cargo test --test rules -->\n\n\
         每一处业务检查都是一条有编号的规则（[14 §5.1](14-tracing-and-business-rules.md)，D142）。被拒绝的请求返回的错误里带 `rule`（客户端协议的 `error.rule`、开放 API 的 `rule`、MCP 的 `structuredContent.error.rule`），指标 `agentim_refusals_total{op, code, rule}` 按规则计数。错误码的含义见 CLAUDE.md：`forbidden` 不允许或不是成员，`not_found` 不存在或已删除，`conflict` 当前状态不允许，`bad_request` 输入无效或不适用。\n\n\
         环节：③ 会话层（进写线程之前，`hub`）；⑦ 写线程（和写入在同一个事务里，`logic`）；file store 文件存储。说明是代码里的报错文字（`{}` 处是具体值）。\n\n",
    );
    let total = rules.len();
    t += &format!("共 {total} 条。\n\n");
    for (prefix, title) in DOMAINS {
        let mine: Vec<(&String, &Rule)> = rules
            .iter()
            .filter(|(id, _)| id.split('.').next() == Some(*prefix))
            .collect();
        if mine.is_empty() {
            continue;
        }
        t += &format!("## {title}（`{prefix}.*`，{} 条）\n\n", mine.len());
        t += "| 规则 | 错误码 | 环节 | 说明 | 代码 |\n|---|---|---|---|---|\n";
        for (id, r) in mine {
            let stages: BTreeSet<&str> = r.files.iter().map(|f| stage(f)).collect();
            let texts: Vec<String> = r.texts.iter().map(|x| x.replace('|', "\\|")).collect();
            let files: Vec<String> = r.files.iter().map(|f| format!("`{f}`")).collect();
            t += &format!(
                "| `{id}` | `{}` | {} | {} | {} |\n",
                r.code,
                stages.into_iter().collect::<Vec<_>>().join("、"),
                if texts.is_empty() {
                    "—".into()
                } else {
                    texts.join("；")
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
        "docs/15-business-rules.md is out of date: AGENTIM_WRITE_RULES=1 cargo test --test rules"
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
