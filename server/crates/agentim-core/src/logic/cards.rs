//! Cards (docs/03-agents.md §3): declarative JSON with a fixed
//! set of components, never scripts; members respond with a button or a
//! filled form and the response goes back to the sender.

use std::collections::HashSet;

use serde_json::{Map, Value, json};

use super::{Ctx, err};
use crate::model::*;

pub const KIND: &str = "agentim.card";
const MAX_BYTES: usize = 16 * 1024;
const MAX_BLOCKS: usize = 20;
const MAX_OPTIONS: usize = 10;
const MAX_FIELDS: usize = 10;
const LABEL_MAX: usize = 100;
const TEXT_MAX: usize = 4096;
const VALUE_MAX: usize = 1000;

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

fn only(o: &Map<String, Value>, keys: &[&str], what: &str) -> ApiResult<()> {
    match o.keys().find(|k| !keys.contains(&k.as_str())) {
        Some(k) => Err(bad(format!("{what} does not take {k}"))),
        None => Ok(()),
    }
}

/// Checks a card payload: the fixed components and their limits.
pub fn check_card(p: &Value) -> ApiResult<()> {
    let o = p
        .as_object()
        .ok_or_else(|| bad("a card is a JSON object"))?;
    if p.to_string().len() > MAX_BYTES {
        return Err(bad("a card is at most 16KB"));
    }
    only(o, &["title", "blocks", "once"], "a card")?;
    if let Some(t) = o.get("title") {
        check_label(t, "title")?;
    }
    if o.get("once").is_some_and(|v| !v.is_boolean()) {
        return Err(bad("once is true or false"));
    }
    let blocks = o
        .get("blocks")
        .and_then(Value::as_array)
        .filter(|b| !b.is_empty() && b.len() <= MAX_BLOCKS)
        .ok_or_else(|| bad("blocks: 1..20 components"))?;
    let mut ids = HashSet::new();
    let mut interactive = 0;
    for b in blocks {
        let bo = b.as_object().ok_or_else(|| bad("a block is an object"))?;
        match bo.get("type").and_then(Value::as_str) {
            Some("text") => {
                only(bo, &["type", "text"], "text")?;
                match bo.get("text").and_then(Value::as_str) {
                    Some(s) if !s.is_empty() && s.len() <= TEXT_MAX => {}
                    _ => return Err(bad("text blocks hold 1 byte..4KB")),
                }
            }
            Some("buttons") => {
                only(bo, &["type", "id", "options"], "buttons")?;
                check_id(bo.get("id").unwrap_or(&Value::Null), &mut ids)?;
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
                interactive += 1;
            }
            Some("form") => {
                only(bo, &["type", "id", "fields", "submit"], "form")?;
                check_id(bo.get("id").unwrap_or(&Value::Null), &mut ids)?;
                check_label(bo.get("submit").unwrap_or(&Value::Null), "submit")?;
                let fields = bo
                    .get("fields")
                    .and_then(Value::as_array)
                    .filter(|a| !a.is_empty() && a.len() <= MAX_FIELDS)
                    .ok_or_else(|| bad("form: 1..10 fields"))?;
                let mut own = HashSet::new();
                for f in fields {
                    let fo = f.as_object().ok_or_else(|| bad("a field is an object"))?;
                    only(
                        fo,
                        &["id", "label", "type", "required", "options", "max"],
                        "a field",
                    )?;
                    check_id(fo.get("id").unwrap_or(&Value::Null), &mut own)?;
                    check_label(fo.get("label").unwrap_or(&Value::Null), "a label")?;
                    if fo.get("required").is_some_and(|v| !v.is_boolean()) {
                        return Err(bad("required is true or false"));
                    }
                    match fo.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(m) = fo.get("max")
                                && !m
                                    .as_u64()
                                    .is_some_and(|m| (1..=VALUE_MAX as u64).contains(&m))
                            {
                                return Err(bad("max is 1..1000"));
                            }
                        }
                        Some("number" | "checkbox") => {}
                        Some("select") => {
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
                        _ => return Err(bad("a field is text, number, select or checkbox")),
                    }
                    if fo.contains_key("options")
                        && fo.get("type").and_then(Value::as_str) != Some("select")
                    {
                        return Err(bad("only select fields have options"));
                    }
                }
                interactive += 1;
            }
            _ => return Err(bad("a block is text, buttons or form")),
        }
    }
    let _ = interactive;
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
    match block["type"].as_str() {
        Some("buttons") => {
            let a = action.ok_or_else(|| bad("a button response gives action"))?;
            if values.is_some() {
                return Err(bad("a button response gives no values"));
            }
            if !block["options"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|o| o["id"] == a)
            {
                return Err(bad(format!("no button {a}")));
            }
        }
        Some("form") => {
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
                let ok = match f["type"].as_str() {
                    Some("text") => x.as_str().is_some_and(|s| {
                        let max = f["max"].as_u64().unwrap_or(VALUE_MAX as u64) as usize;
                        s.len() <= max
                            && !s.contains('\0')
                            && (!s.is_empty() || f["required"] != true)
                    }),
                    Some("number") => x.is_number(),
                    Some("checkbox") => x.is_boolean(),
                    Some("select") => f["options"]
                        .as_array()
                        .is_some_and(|o| o.iter().any(|y| y == x)),
                    _ => false,
                };
                if !ok {
                    return Err(bad(format!("{id} has an invalid value")));
                }
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
