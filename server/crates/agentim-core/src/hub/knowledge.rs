//! An assistant's knowledge (docs/en/design/assistants.md): files the app uploads (Markdown,
//! text, CSV, JSON, PDF). The text is taken out once, stored sealed, cut
//! into overlapping pieces and searched in memory (BM25), the way Sendbird
//! and Intercom answer from "sources": the best pieces go into the prompt,
//! and the assistant can be told to answer only from them.

use std::collections::HashMap;
use std::sync::Arc;

use serde_json::{Value, json};

use super::Hub;
use crate::db::ai::DocRow;
use crate::logic::{AdminOp, Op};
use crate::model::*;

pub const MAX_FILE_BYTES: usize = 5 * 1024 * 1024;
pub const MAX_FILES: usize = 50;
/// Text of all of one assistant's files together, in characters.
pub const MAX_CHARS: i64 = 2_000_000;
/// Pieces of about this many characters, overlapping by `OVERLAP`.
const PIECE: usize = 1200;
const OVERLAP: usize = 200;
/// Pieces put into a prompt.
pub const TOP: usize = 5;

/// The pieces of one assistant's files, ready to search.
pub struct Index {
    version: String,
    pieces: Vec<(String, String)>,
    terms: Vec<HashMap<String, u32>>,
    lens: Vec<usize>,
    df: HashMap<String, u32>,
    avg: f64,
}

/// Common English words that say nothing about what is asked.
const STOP: &[&str] = &[
    "a", "about", "after", "all", "also", "am", "an", "and", "any", "are", "as", "at", "be",
    "been", "but", "by", "can", "could", "did", "do", "does", "for", "from", "had", "has", "have",
    "he", "her", "his", "how", "if", "in", "into", "is", "it", "its", "just", "me", "my", "no",
    "not", "of", "on", "or", "our", "she", "so", "than", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "to", "too", "us", "was", "we", "were", "what", "when",
    "where", "which", "who", "why", "will", "with", "would", "you", "your",
];

/// Words, lower-cased, without common filler words; CJK and other scripts without spaces as single
/// characters and pairs.
pub fn tokens(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut word = String::new();
    let mut prev_cjk: Option<char> = None;
    let cjk = |c: char| matches!(c as u32, 0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF);
    let flush = |w: &mut String, out: &mut Vec<String>| {
        if w.chars().count() > 1 && !STOP.contains(&w.as_str()) {
            out.push(std::mem::take(w));
        } else {
            w.clear();
        }
    };
    for c in text.chars().flat_map(char::to_lowercase) {
        if cjk(c) {
            flush(&mut word, &mut out);
            out.push(c.to_string());
            if let Some(p) = prev_cjk {
                out.push(format!("{p}{c}"));
            }
            prev_cjk = Some(c);
        } else if c.is_alphanumeric() {
            prev_cjk = None;
            word.push(c);
        } else {
            prev_cjk = None;
            flush(&mut word, &mut out);
        }
    }
    flush(&mut word, &mut out);
    out
}

/// Overlapping pieces, cut at a line or word end where one is near.
fn pieces(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = vec![];
    let mut start = 0;
    while start < chars.len() {
        let mut end = (start + PIECE).min(chars.len());
        if end < chars.len() {
            let back = (end.saturating_sub(200)..end)
                .rev()
                .find(|&i| chars[i] == '\n')
                .or_else(|| {
                    (end.saturating_sub(100)..end)
                        .rev()
                        .find(|&i| chars[i].is_whitespace())
                });
            if let Some(b) = back.filter(|&b| b > start) {
                end = b + 1;
            }
        }
        let p: String = chars[start..end].iter().collect();
        if !p.trim().is_empty() {
            out.push(p.trim().to_string());
        }
        if end >= chars.len() {
            break;
        }
        start = end.saturating_sub(OVERLAP).max(start + 1);
    }
    out
}

impl Index {
    pub fn build(version: String, docs: &[DocRow]) -> Index {
        let mut ps = vec![];
        for d in docs {
            for p in pieces(&d.body) {
                ps.push((d.name.clone(), p));
            }
        }
        let mut terms = vec![];
        let mut lens = vec![];
        let mut df: HashMap<String, u32> = HashMap::new();
        for (_, p) in &ps {
            let t = tokens(p);
            lens.push(t.len());
            let mut tf: HashMap<String, u32> = HashMap::new();
            for w in t {
                *tf.entry(w).or_default() += 1;
            }
            for w in tf.keys() {
                *df.entry(w.clone()).or_default() += 1;
            }
            terms.push(tf);
        }
        let avg = if lens.is_empty() {
            1.0
        } else {
            lens.iter().sum::<usize>() as f64 / lens.len() as f64
        };
        Index {
            version,
            pieces: ps,
            terms,
            lens,
            df,
            avg: avg.max(1.0),
        }
    }

    /// The best pieces for a question, best first; none when nothing in
    /// them matches.
    pub fn search(&self, query: &str, n: usize) -> Vec<(String, String, f64)> {
        let q: Vec<String> = {
            let mut t = tokens(query);
            t.sort();
            t.dedup();
            t
        };
        let total = self.pieces.len() as f64;
        let mut scored: Vec<(usize, f64)> = (0..self.pieces.len())
            .map(|i| {
                let len = self.lens[i] as f64;
                let s: f64 = q
                    .iter()
                    .filter_map(|w| {
                        let f = *self.terms[i].get(w)? as f64;
                        let df = *self.df.get(w).unwrap_or(&1) as f64;
                        let idf = ((total - df + 0.5) / (df + 0.5) + 1.0).ln();
                        Some(idf * f * 2.2 / (f + 1.2 * (0.25 + 0.75 * len / self.avg)))
                    })
                    .sum();
                (i, s)
            })
            .filter(|(_, s)| *s > 0.0)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored
            .into_iter()
            .take(n)
            .map(|(i, s)| (self.pieces[i].0.clone(), self.pieces[i].1.clone(), s))
            .collect()
    }
}

/// The text of an uploaded file, by its name's extension.
pub fn extract(name: &str, bytes: &[u8]) -> ApiResult<String> {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let text = match ext.as_str() {
        "md" | "markdown" | "txt" | "csv" | "json" => {
            String::from_utf8(bytes.to_vec()).map_err(|_| {
                rule!(
                    "knowledge.encoding",
                    "bad_request",
                    "text files must be UTF-8"
                )
            })?
        }
        "pdf" => {
            let b = bytes.to_vec();
            // The PDF parser may panic on broken files: contained here.
            std::panic::catch_unwind(move || pdf_extract::extract_text_from_mem(&b))
                .ok()
                .and_then(|r| r.ok())
                .ok_or_else(|| {
                    rule!(
                        "knowledge.unreadable",
                        "bad_request",
                        "the PDF could not be read"
                    )
                })?
        }
        _ => {
            return Err(rule!(
                "knowledge.type",
                "bad_request",
                "knowledge files are .md, .txt, .csv, .json or .pdf"
            ));
        }
    };
    let text = text.replace('\0', "").replace("\r\n", "\n");
    if text.trim().is_empty() {
        return Err(rule!(
            "knowledge.empty",
            "bad_request",
            "the file has no text"
        ));
    }
    Ok(text)
}

fn check_name(name: &str) -> ApiResult<()> {
    let ok = !name.is_empty()
        && name.len() <= 200
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || " ._-()".contains(c));
    if ok {
        Ok(())
    } else {
        Err(rule!(
            "knowledge.name",
            "bad_request",
            "file names are 1..200 letters, digits, spaces and . _ - ( )"
        ))
    }
}

impl Hub {
    fn knowledge_agent(&self, agent_id: &str) -> ApiResult<User> {
        self.inner
            .sh
            .user(agent_id)
            .filter(|u| u.kind == UserKind::Agent && !self.is_destroyed(&u.id))
            .ok_or_else(|| rule!("agent.unknown", "not_found", "no such agent"))
    }

    pub async fn knowledge_list(&self, agent_id: &str) -> ApiResult<Vec<Value>> {
        let a = self.knowledge_agent(agent_id)?.id;
        let rows = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.knowledge(&a, false).await })
            })
            .await?;
        Ok(rows
            .iter()
            .map(|d| json!({"name": d.name, "bytes": d.bytes, "chars": d.chars, "created_at": d.created_at}))
            .collect())
    }

    /// Adds or replaces a file.
    pub async fn knowledge_put(
        &self,
        agent_id: &str,
        name: &str,
        bytes: &[u8],
    ) -> ApiResult<Value> {
        let agent = self.knowledge_agent(agent_id)?;
        check_name(name)?;
        if bytes.len() > MAX_FILE_BYTES {
            return Err(rule!(
                "knowledge.size",
                "bad_request",
                "a file is at most 5 MB"
            ));
        }
        let n = name.to_string();
        let b = bytes.to_vec();
        let text = tokio::task::spawn_blocking(move || extract(&n, &b))
            .await
            .map_err(|e| ApiError::new("internal", e.to_string()))??;
        let row = DocRow {
            agent_id: agent.id.clone(),
            name: name.to_string(),
            bytes: bytes.len() as i64,
            chars: text.chars().count() as i64,
            body: text,
            created_at: now_ms(),
        };
        let chars = row.chars;
        let v = self
            .control()
            .call(Op::Admin(AdminOp::KnowledgePut(row)))
            .await?;
        self.inner.ai.indexes.lock().unwrap().remove(&agent.id);
        Ok(json!({"name": name, "chars": chars, "created": v["created"]}))
    }

    pub async fn knowledge_delete(&self, agent_id: &str, name: &str) -> ApiResult<Value> {
        let agent = self.knowledge_agent(agent_id)?;
        let v = self
            .control()
            .call(Op::Admin(AdminOp::KnowledgeDelete {
                agent_id: agent.id.clone(),
                name: name.to_string(),
            }))
            .await?;
        self.inner.ai.indexes.lock().unwrap().remove(&agent.id);
        Ok(v)
    }

    /// The assistant's index, built once per change of its files.
    pub(super) async fn knowledge_index(&self, agent_id: &str) -> ApiResult<Option<Arc<Index>>> {
        let a = agent_id.to_string();
        let list = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.knowledge(&a, false).await })
            })
            .await?;
        if list.is_empty() {
            return Ok(None);
        }
        let version = list
            .iter()
            .map(|d| format!("{}@{}", d.name, d.created_at))
            .collect::<Vec<_>>()
            .join("|");
        if let Some(i) = self.inner.ai.indexes.lock().unwrap().get(agent_id)
            && i.version == version
        {
            return Ok(Some(i.clone()));
        }
        let a = agent_id.to_string();
        let docs = self
            .read(self.control(), move |db| {
                Box::pin(async move { db.knowledge(&a, true).await })
            })
            .await?;
        let idx = Arc::new(
            tokio::task::spawn_blocking(move || Index::build(version, &docs))
                .await
                .map_err(|e| ApiError::new("internal", e.to_string()))?,
        );
        self.inner
            .ai
            .indexes
            .lock()
            .unwrap()
            .insert(agent_id.to_string(), idx.clone());
        Ok(Some(idx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(name: &str, body: &str) -> DocRow {
        DocRow {
            agent_id: "a".into(),
            name: name.into(),
            bytes: 0,
            chars: 0,
            body: body.into(),
            created_at: 0,
        }
    }

    #[test]
    fn tokens_cover_words_and_cjk() {
        assert_eq!(
            tokens("Shipping to Canada, 3-5 days!"),
            ["shipping", "canada", "days"]
        );
        assert_eq!(
            tokens("退货政策"),
            ["退", "货", "退货", "政", "货政", "策", "政策"]
        );
        assert!(tokens("").is_empty());
    }

    #[test]
    fn pieces_overlap_and_cover_the_text() {
        let text = (0..400)
            .map(|i| format!("line {i} about something\n"))
            .collect::<String>();
        let ps = pieces(&text);
        assert!(ps.len() > 5);
        assert!(ps.iter().all(|p| p.chars().count() <= PIECE));
        assert!(ps[0].contains("line 0 ") && ps.last().unwrap().contains("line 399 "));
        assert_eq!(pieces("short"), ["short"]);
        assert!(pieces("   \n ").is_empty());
    }

    #[test]
    fn search_ranks_the_matching_piece_first() {
        let i = Index::build(
            "v".into(),
            &[
                doc(
                    "shipping.md",
                    "We ship to Canada and Mexico in 3 to 5 days.",
                ),
                doc("returns.md", "Returns are free within 30 days of delivery."),
                doc("退货.md", "退货政策：收货后三十天内可以免费退货。"),
            ],
        );
        let r = i.search("Do you ship to Canada?", 2);
        assert_eq!(r[0].0, "shipping.md");
        assert_eq!(i.search("how do returns work", 1)[0].0, "returns.md");
        assert_eq!(i.search("退货怎么办", 1)[0].0, "退货.md");
        assert!(i.search("quantum chromodynamics", 3).is_empty());
        assert!(
            i.search("What is the meaning of life?", 3).is_empty(),
            "filler words match nothing"
        );
        assert!(Index::build("v".into(), &[]).search("x", 3).is_empty());
    }

    #[test]
    fn extraction_by_type() {
        assert_eq!(extract("a.md", b"# Hi\r\nthere").unwrap(), "# Hi\nthere");
        assert_eq!(extract("a.TXT", b"x").unwrap(), "x");
        assert_eq!(
            extract("a.exe", b"x").unwrap_err().rule,
            Some("knowledge.type")
        );
        assert_eq!(extract("a", b"x").unwrap_err().rule, Some("knowledge.type"));
        assert_eq!(
            extract("a.txt", &[0xff, 0xfe]).unwrap_err().rule,
            Some("knowledge.encoding")
        );
        assert_eq!(
            extract("a.txt", b"  \n").unwrap_err().rule,
            Some("knowledge.empty")
        );
        assert_eq!(
            extract("a.pdf", b"not a pdf").unwrap_err().rule,
            Some("knowledge.unreadable")
        );
    }

    #[test]
    fn names() {
        assert!(check_name("FAQ (v2).md").is_ok());
        assert!(check_name("常见问题.md").is_ok());
        for bad in ["", ".env", "a/b.md", "a\\b", "x".repeat(201).as_str()] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
