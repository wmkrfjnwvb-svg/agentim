//! Cards (docs/en/design/agents.md, docs/en/design/chat-components.md): declarative JSON with a fixed
//! set of components, never scripts. Members respond with a button, a
//! filled form or a picked list row; the response goes back to the sender
//! (and to webhooks, as the `card.response` event). Blocks and fields named
//! `x-…` are the developer's own: only their size is checked, clients that
//! don't know them show the `fallback`.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::{Ctx, err};
use crate::model::*;

pub const KIND: &str = "agentim.card";
const MAX_BYTES: usize = 16 * 1024;
const MAX_BLOCKS: usize = 20;
const MAX_OPTIONS: usize = 10;
const MAX_FIELDS: usize = 10;
const MAX_ROWS: usize = 50;
const MAX_COLUMNS: usize = 8;
const MAX_ITEMS: usize = 20;
const MAX_TO: usize = 20;
const LABEL_MAX: usize = 100;
const TEXT_MAX: usize = 4096;
const VALUE_MAX: usize = 1000;
const TEXTAREA_MAX: usize = 4000;
const CELL_MAX: usize = 500;
const TABLE_TEXT_MAX: usize = 8 * 1024;
const CUSTOM_DATA_MAX: usize = 8 * 1024;
const CUSTOM_VALUE_MAX: usize = 1024;
const PATTERN_MAX: usize = 200;
const TONES: &[&str] = &["neutral", "success", "warning", "danger", "info"];

fn bad(msg: impl Into<String>) -> ApiError {
    rule!("card.input", "bad_request", msg)
}

fn check_id(v: &Value, ids: &mut HashSet<String>) -> ApiResult<String> {
    let id = v
        .as_str()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 64
                && s.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        })
        .ok_or_else(|| bad("ids are 1..64 of a-z 0-9 _ -"))?;
    if !ids.insert(id.to_string()) {
        return Err(bad(format!("id {id} is used twice in this card")));
    }
    Ok(id.to_string())
}

fn check_label(v: &Value, what: &str) -> ApiResult<()> {
    match v.as_str() {
        Some(s) if !s.is_empty() && s.len() <= LABEL_MAX && !s.contains('\0') => Ok(()),
        _ => Err(bad(format!("{what} must be 1..100 bytes"))),
    }
}

fn check_str(v: Option<&Value>, max: usize, what: &str) -> ApiResult<()> {
    match v {
        None => Ok(()),
        Some(Value::String(s)) if !s.is_empty() && s.len() <= max && !s.contains('\0') => Ok(()),
        Some(_) => Err(bad(format!("{what} must be 1..{max} bytes"))),
    }
}

fn only(o: &Map<String, Value>, keys: &[&str], what: &str) -> ApiResult<()> {
    match o.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(bad(format!("{what} does not take {k}"))),
        None => Ok(()),
    }
}

/// `x-` and then a lowercase name (dots allowed: `x-acme.chart`).
fn is_custom(t: &str) -> bool {
    t.strip_prefix("x-").is_some_and(|n| {
        !n.is_empty()
            && n.len() <= 62
            && n.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && n.chars().all(|c| {
                c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-')
            })
    })
}

fn check_tone(v: Option<&Value>) -> ApiResult<()> {
    match v {
        None => Ok(()),
        Some(t) if t.as_str().is_some_and(|t| TONES.contains(&t)) => Ok(()),
        Some(_) => Err(bad("tone is neutral, success, warning, danger or info")),
    }
}

/// How a value is shown: formatted by the viewer's locale (docs/en/design/chat-components.md).
fn check_format(v: Option<&Value>) -> ApiResult<()> {
    let Some(v) = v else { return Ok(()) };
    let ok = match v.as_str() {
        Some("text" | "number" | "date" | "datetime" | "percent") => true,
        Some(f) => f
            .strip_prefix("currency:")
            .is_some_and(|c| c.len() == 3 && c.chars().all(|x| x.is_ascii_uppercase())),
        None => false,
    };
    if ok {
        Ok(())
    } else {
        Err(bad(
            "format is text, number, currency:XXX, date, datetime or percent",
        ))
    }
}

/// A shown value: text (≤ 500 bytes), a number or true/false.
fn check_cell(v: &Value) -> ApiResult<usize> {
    match v {
        Value::String(s) if s.len() <= CELL_MAX && !s.contains('\0') => Ok(s.len()),
        Value::Number(_) | Value::Bool(_) | Value::Null => Ok(8),
        Value::String(_) | Value::Array(_) | Value::Object(_) => Err(bad(
            "a value is text (at most 500 bytes), a number, true/false or null",
        )),
    }
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<u32>().ok());
    let (Some(y), Some(m), Some(d)) = (n(0..4), n(5..7), n(8..10)) else {
        return false;
    };
    if !s[0..4].bytes().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let days = match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&d)
}

fn is_time(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 5
        && b[2] == b':'
        && s[0..2].parse::<u32>().is_ok_and(|h| h < 24)
        && s[3..5].parse::<u32>().is_ok_and(|m| m < 60)
        && s[0..2]
            .bytes()
            .chain(s[3..5].bytes())
            .all(|c| c.is_ascii_digit())
}

fn is_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    s.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

/// A field's `pattern`, anchored, compiled with a size limit (the regex
/// crate runs in linear time, so a hostile pattern can't stall a check).
fn pattern(p: &str) -> Option<regex::Regex> {
    regex::RegexBuilder::new(&format!("^(?:{p})$"))
        .size_limit(1 << 16)
        .build()
        .ok()
}

fn check_field(f: &Value, own: &mut HashSet<String>) -> ApiResult<()> {
    let fo = f.as_object().ok_or_else(|| bad("a field is an object"))?;
    let ty = fo.get("type").and_then(Value::as_str).unwrap_or_default();
    let mut keys = vec![
        "id",
        "label",
        "type",
        "required",
        "placeholder",
        "hint",
        "default",
    ];
    match ty {
        "text" => keys.extend(["max", "pattern", "error"]),
        "textarea" => keys.push("max"),
        "number" | "date" | "time" => keys.extend(["min", "max"]),
        "select" => keys.push("options"),
        "checkbox" | "email" => {}
        t if is_custom(t) => keys.push("config"),
        _ => {
            return Err(bad(
                "a field is text, textarea, number, select, checkbox, date, time, email or x-…",
            ));
        }
    }
    only(fo, &keys, "this field")?;
    check_id(fo.get("id").unwrap_or(&Value::Null), own)?;
    check_label(fo.get("label").unwrap_or(&Value::Null), "a label")?;
    if fo.get("required").is_some_and(|v| !v.is_boolean()) {
        return Err(bad("required is true or false"));
    }
    check_str(fo.get("placeholder"), LABEL_MAX, "placeholder")?;
    check_str(fo.get("hint"), 200, "hint")?;
    check_str(fo.get("error"), 200, "error")?;
    let len_max = |cap: usize| -> ApiResult<()> {
        match fo.get("max") {
            None => Ok(()),
            Some(m) if m.as_u64().is_some_and(|m| (1..=cap as u64).contains(&m)) => Ok(()),
            Some(_) => Err(bad(format!("max is 1..{cap}"))),
        }
    };
    match ty {
        "text" => {
            len_max(VALUE_MAX)?;
            if let Some(p) = fo.get("pattern") {
                let p = p
                    .as_str()
                    .filter(|p| !p.is_empty() && p.len() <= PATTERN_MAX)
                    .ok_or_else(|| bad("pattern is 1..200 bytes"))?;
                if pattern(p).is_none() {
                    return Err(bad("pattern is not a valid regular expression"));
                }
            }
        }
        "textarea" => len_max(TEXTAREA_MAX)?,
        "number" => {
            for k in ["min", "max"] {
                if fo.get(k).is_some_and(|v| !v.is_number()) {
                    return Err(bad(format!("{k} is a number")));
                }
            }
            if let (Some(a), Some(b)) = (
                fo.get("min").and_then(Value::as_f64),
                fo.get("max").and_then(Value::as_f64),
            ) && a > b
            {
                return Err(bad("min is at most max"));
            }
        }
        "date" | "time" => {
            let ok = |v: &str| if ty == "date" { is_date(v) } else { is_time(v) };
            for k in ["min", "max"] {
                if let Some(v) = fo.get(k)
                    && !v.as_str().is_some_and(ok)
                {
                    return Err(bad(if ty == "date" {
                        "dates are YYYY-MM-DD"
                    } else {
                        "times are HH:MM"
                    }));
                }
            }
            if let (Some(a), Some(b)) = (
                fo.get("min").and_then(Value::as_str),
                fo.get("max").and_then(Value::as_str),
            ) && a > b
            {
                return Err(bad("min is at most max"));
            }
        }
        "select" => {
            let o = fo
                .get("options")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_OPTIONS)
                .ok_or_else(|| bad("select: 1..10 options"))?;
            let mut seen = HashSet::new();
            for x in o {
                check_label(x, "an option")?;
                if !seen.insert(x.as_str().unwrap_or_default()) {
                    return Err(bad("select options must differ"));
                }
            }
        }
        t if is_custom(t) => {
            if let Some(c) = fo.get("config")
                && c.to_string().len() > CUSTOM_VALUE_MAX
            {
                return Err(bad("config is at most 1KB"));
            }
        }
        _ => {}
    }
    // A default must itself be a valid answer.
    if let Some(d) = fo.get("default")
        && !d.is_null()
        && !valid_value(f, d)
    {
        return Err(bad("default is not a valid value of this field"));
    }
    Ok(())
}

/// Whether `x` is a valid answer to field `f` (already checked).
fn valid_value(f: &Value, x: &Value) -> bool {
    let req = f["required"] == true;
    let max = |cap: usize| f["max"].as_u64().unwrap_or(cap as u64) as usize;
    match f["type"].as_str().unwrap_or_default() {
        "text" => x.as_str().is_some_and(|s| {
            s.len() <= max(VALUE_MAX)
                && !s.contains('\0')
                && (!s.is_empty() || !req)
                && (s.is_empty()
                    || f["pattern"]
                        .as_str()
                        .is_none_or(|p| pattern(p).is_some_and(|r| r.is_match(s))))
        }),
        "textarea" => x.as_str().is_some_and(|s| {
            s.len() <= max(TEXTAREA_MAX) && !s.contains('\0') && (!s.is_empty() || !req)
        }),
        "number" => x.as_f64().is_some_and(|n| {
            f["min"].as_f64().is_none_or(|m| n >= m) && f["max"].as_f64().is_none_or(|m| n <= m)
        }),
        "checkbox" => x.is_boolean(),
        "select" => f["options"]
            .as_array()
            .is_some_and(|o| o.iter().any(|y| y == x)),
        t @ ("date" | "time") => x.as_str().is_some_and(|s| {
            (if t == "date" { is_date(s) } else { is_time(s) })
                && f["min"].as_str().is_none_or(|m| s >= m)
                && f["max"].as_str().is_none_or(|m| s <= m)
        }),
        "email" => x.as_str().is_some_and(is_email),
        t if is_custom(t) => x.to_string().len() <= CUSTOM_VALUE_MAX,
        _ => false,
    }
}

fn check_block(b: &Value, ids: &mut HashSet<String>) -> ApiResult<()> {
    let bo = b.as_object().ok_or_else(|| bad("a block is an object"))?;
    let ty = bo.get("type").and_then(Value::as_str).unwrap_or_default();
    match ty {
        "text" => {
            only(bo, &["type", "text"], "text")?;
            match bo.get("text").and_then(Value::as_str) {
                Some(s) if !s.is_empty() && s.len() <= TEXT_MAX => {}
                _ => return Err(bad("text blocks hold 1 byte..4KB")),
            }
        }
        "buttons" => {
            only(bo, &["type", "id", "options"], "buttons")?;
            check_id(bo.get("id").unwrap_or(&Value::Null), ids)?;
            let opts = bo
                .get("options")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_OPTIONS)
                .ok_or_else(|| bad("buttons: 1..10 options"))?;
            let mut own = HashSet::new();
            for op in opts {
                let oo = op
                    .as_object()
                    .ok_or_else(|| bad("an option is an object"))?;
                only(oo, &["id", "label", "style"], "an option")?;
                check_id(oo.get("id").unwrap_or(&Value::Null), &mut own)?;
                check_label(oo.get("label").unwrap_or(&Value::Null), "a label")?;
                if let Some(s) = oo.get("style")
                    && !matches!(s.as_str(), Some("primary" | "danger"))
                {
                    return Err(bad("style is primary or danger"));
                }
            }
        }
        "form" => {
            only(bo, &["type", "id", "fields", "submit"], "form")?;
            check_id(bo.get("id").unwrap_or(&Value::Null), ids)?;
            check_label(bo.get("submit").unwrap_or(&Value::Null), "submit")?;
            let fields = bo
                .get("fields")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_FIELDS)
                .ok_or_else(|| bad("form: 1..10 fields"))?;
            let mut own = HashSet::new();
            for f in fields {
                check_field(f, &mut own)?;
            }
        }
        "facts" => {
            only(bo, &["type", "items"], "facts")?;
            let items = bo
                .get("items")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_ITEMS)
                .ok_or_else(|| bad("facts: 1..20 items"))?;
            for it in items {
                let io = it.as_object().ok_or_else(|| bad("a fact is an object"))?;
                only(io, &["label", "value", "format", "tone"], "a fact")?;
                check_label(io.get("label").unwrap_or(&Value::Null), "a label")?;
                check_cell(io.get("value").unwrap_or(&Value::Null))?;
                check_format(io.get("format"))?;
                check_tone(io.get("tone"))?;
            }
        }
        "table" => {
            only(bo, &["type", "columns", "rows", "caption"], "table")?;
            check_str(bo.get("caption"), LABEL_MAX, "caption")?;
            let cols = bo
                .get("columns")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_COLUMNS)
                .ok_or_else(|| bad("table: 1..8 columns"))?;
            for c in cols {
                let co = c.as_object().ok_or_else(|| bad("a column is an object"))?;
                only(co, &["label", "align", "format"], "a column")?;
                check_label(co.get("label").unwrap_or(&Value::Null), "a label")?;
                if let Some(a) = co.get("align")
                    && !matches!(a.as_str(), Some("start" | "center" | "end"))
                {
                    return Err(bad("align is start, center or end"));
                }
                check_format(co.get("format"))?;
            }
            let rows = bo
                .get("rows")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_ROWS)
                .ok_or_else(|| bad("table: 1..50 rows"))?;
            let mut total = 0;
            for r in rows {
                let cells = r
                    .as_array()
                    .filter(|c| c.len() == cols.len())
                    .ok_or_else(|| bad("each row has one value per column"))?;
                for c in cells {
                    total += check_cell(c)?;
                }
            }
            if total > TABLE_TEXT_MAX {
                return Err(bad("a table holds at most 8KB of text"));
            }
        }
        "list" => {
            only(bo, &["type", "id", "select", "items"], "list")?;
            let select = match bo.get("select") {
                None => false,
                Some(Value::Bool(b)) => *b,
                Some(_) => return Err(bad("select is true or false")),
            };
            match (bo.get("id"), select) {
                (Some(id), _) => {
                    check_id(id, ids)?;
                }
                (None, true) => return Err(bad("a list people pick from needs an id")),
                (None, false) => {}
            }
            let items = bo
                .get("items")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty() && a.len() <= MAX_ITEMS)
                .ok_or_else(|| bad("list: 1..20 items"))?;
            let mut own = HashSet::new();
            for it in items {
                let io = it
                    .as_object()
                    .ok_or_else(|| bad("a list item is an object"))?;
                only(
                    io,
                    &["id", "title", "subtitle", "meta", "badge"],
                    "a list item",
                )?;
                check_id(io.get("id").unwrap_or(&Value::Null), &mut own)?;
                check_str(Some(io.get("title").unwrap_or(&Value::Null)), 200, "title")?;
                check_str(io.get("subtitle"), 200, "subtitle")?;
                check_str(io.get("meta"), LABEL_MAX, "meta")?;
                if let Some(b) = io.get("badge") {
                    let bo = b.as_object().ok_or_else(|| bad("a badge is an object"))?;
                    only(bo, &["label", "tone"], "a badge")?;
                    check_label(bo.get("label").unwrap_or(&Value::Null), "a badge label")?;
                    check_tone(bo.get("tone"))?;
                }
            }
        }
        t if is_custom(t) => {
            only(bo, &["type", "id", "data", "fallback"], "a custom block")?;
            if let Some(id) = bo.get("id") {
                check_id(id, ids)?;
            }
            match bo.get("fallback") {
                Some(Value::String(f))
                    if f == "drop" || (!f.is_empty() && f.len() <= VALUE_MAX) => {}
                _ => {
                    return Err(bad(
                        "a custom block needs fallback: a text of 1..1000 bytes or \"drop\"",
                    ));
                }
            }
            if bo
                .get("data")
                .is_some_and(|d| d.to_string().len() > CUSTOM_DATA_MAX)
            {
                return Err(bad("data is at most 8KB"));
            }
        }
        _ => {
            return Err(bad(
                "a block is text, buttons, form, facts, table, list or x-…",
            ));
        }
    }
    Ok(())
}

/// Checks a card payload: the fixed components and their limits.
pub fn check_card(p: &Value) -> ApiResult<()> {
    let o = p
        .as_object()
        .ok_or_else(|| bad("a card is a JSON object"))?;
    if p.to_string().len() > MAX_BYTES {
        return Err(bad("a card is at most 16KB"));
    }
    only(o, &["title", "blocks", "once", "to"], "a card")?;
    if let Some(t) = o.get("title") {
        check_label(t, "title")?;
    }
    if o.get("once").is_some_and(|v| !v.is_boolean()) {
        return Err(bad("once is true or false"));
    }
    if let Some(to) = o.get("to") {
        let list = to
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= MAX_TO)
            .ok_or_else(|| bad("to: 1..20 people"))?;
        for x in list {
            check_str(Some(x), 64, "a person in to")?;
        }
    }
    let blocks = o
        .get("blocks")
        .and_then(Value::as_array)
        .filter(|b| !b.is_empty() && b.len() <= MAX_BLOCKS)
        .ok_or_else(|| bad("blocks: 1..20 components"))?;
    let mut ids = HashSet::new();
    for b in blocks {
        check_block(b, &mut ids)?;
    }
    Ok(())
}

/// A response checked against the card: (block id, action, values).
fn check_response(
    card: &Value,
    block_id: &str,
    action: Option<&str>,
    values: Option<&Value>,
) -> ApiResult<()> {
    let block = card["blocks"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|b| b["id"] == block_id)
        .ok_or_else(|| bad(format!("no block {block_id} in this card")))?;
    let ty = block["type"].as_str().unwrap_or_default();
    match ty {
        "buttons" | "list" => {
            let a = action.ok_or_else(|| bad("this response gives action"))?;
            if values.is_some() {
                return Err(bad("this response gives no values"));
            }
            let (key, what) = if ty == "buttons" {
                ("options", "button")
            } else {
                if block["select"] != true {
                    return Err(bad(format!("{block_id} takes no response")));
                }
                ("items", "item")
            };
            if !block[key]
                .as_array()
                .into_iter()
                .flatten()
                .any(|o| o["id"] == a)
            {
                return Err(bad(format!("no {what} {a}")));
            }
        }
        "form" => {
            if action.is_some() {
                return Err(bad("a form response gives values, not action"));
            }
            let v = values
                .and_then(Value::as_object)
                .ok_or_else(|| bad("a form response gives values (an object)"))?;
            let fields = block["fields"].as_array().cloned().unwrap_or_default();
            for k in v.keys() {
                if !fields.iter().any(|f| f["id"] == k.as_str()) {
                    return Err(bad(format!("no field {k}")));
                }
            }
            for f in &fields {
                let id = f["id"].as_str().unwrap_or_default();
                let given = v.get(id).filter(|x| !x.is_null());
                let Some(x) = given else {
                    if f["required"] == true {
                        return Err(bad(format!("{id} is required")));
                    }
                    continue;
                };
                if !valid_value(f, x) {
                    return Err(bad(format!("{id} has an invalid value")));
                }
            }
        }
        t if is_custom(t) => {
            // The developer's own block: any action or values, size-checked.
            if action.is_none() && values.is_none() {
                return Err(bad("a response gives action or values"));
            }
            if action.is_some_and(|a| a.is_empty() || a.len() > 64)
                || values.is_some_and(|v| v.to_string().len() > CUSTOM_VALUE_MAX)
            {
                return Err(bad("action is 1..64 bytes and values at most 1KB"));
            }
        }
        _ => return Err(bad(format!("{block_id} takes no response"))),
    }
    Ok(())
}

impl Ctx<'_, '_> {
    pub(super) async fn respond(
        &mut self,
        me: &User,
        msg_id: &str,
        block_id: &str,
        action: Option<String>,
        values: Option<Value>,
    ) -> ApiResult<Value> {
        let msg = self
            .db
            .message(msg_id)
            .await?
            .ok_or_else(|| err("conv_moved", "message not in this database"))?;
        self.own_conv(&msg.conv_id)?;
        self.require_member(&msg.conv_id, &me.id).await?;
        if msg.kind != KIND || msg.state == "recalled" {
            return Err(bad("only a card takes responses"));
        }
        if msg.sender_id == me.id {
            return Err(bad("one does not respond to one's own card"));
        }
        let card = msg.payload.clone().unwrap_or(Value::Null);
        // A card for some people only (docs/en/design/chat-components.md): the others can't answer.
        if let Some(to) = card["to"].as_array()
            && !to
                .iter()
                .any(|x| x == me.id.as_str() || x == me.name.as_str())
        {
            return Err(rule!(
                "card.not_for_you",
                "forbidden",
                "this card is for someone else"
            ));
        }
        check_response(&card, block_id, action.as_deref(), values.as_ref())?;
        if card["once"] == true && self.db.card_responses(msg_id).await? > 0 {
            return Err(rule!(
                "card.single_response",
                "conflict",
                "this card took its one response"
            ));
        }
        if !self
            .db
            .put_card_response(msg_id, &msg.conv_id, &me.id)
            .await?
        {
            return Err(rule!(
                "card.responded",
                "conflict",
                "you already responded to this card"
            ));
        }
        let wake: Vec<String> = self
            .sh
            .user(&msg.sender_id)
            .filter(|u| u.kind == UserKind::Agent)
            .map(|u| vec![u.id])
            .unwrap_or_default();
        let ev = self
            .db
            .append_event(
                &msg.conv_id,
                "card.response",
                Some(&me.id),
                None,
                json!({"msg_id": msg_id, "block_id": block_id, "action": action, "values": values, "by": me.id, "to": msg.sender_id, "wake": wake}),
            )
            .await?;
        let seq = ev.seq;
        self.emit(ev).await?;
        Ok(json!({"seq": seq}))
    }
}
