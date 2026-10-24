//! Inputs and effects as JSON (docs/12 §2.2: "the interface is messages"),
//! for runtimes that drive the core through a byte boundary (wasm in the
//! browser, Unity WebGL).
//!
//! Inputs: `{"command": {"id", "cmd"}}`, `{"token": "…"}`,
//! `{"token_error": {"refused": bool}}`, `"opened"`, `{"frame": "…"}`,
//! `{"closed": {"code", "reason"}}`,
//! `{"connect_failed": {"status"?, "retry_after_ms"?, "detail"}}`,
//! `{"network": {"online", "generation"}}`, `{"foreground": bool}`,
//! `"tick"`, `{"restored": snapshot}`,
//! `{"page_loaded": {"req", "conv_id", "messages"}}`,
//! `{"storage_failed": "full" | "corrupt" | "unavailable"}`,
//! `"storage_recovered"`, `{"persisted": id}`.
//!
//! Effects: `{"need_token": true}`, `{"connect": url}`, `{"send": text}`,
//! `{"close": {"code", "reason"}}`, `{"persist": batch}`,
//! `{"load_page": {"req", "conv_id", "before_seq", "limit"}}`,
//! `{"wipe_store": true}`, `{"emit": event}`,
//! `{"reply": {"id", "ok", "result" | "error"}}`.

use serde_json::{Value, json};

use crate::{Effect, Input};

pub fn input_from_json(v: &Value) -> Option<Input> {
    if let Some(s) = v.as_str() {
        return match s {
            "opened" => Some(Input::Opened),
            "tick" => Some(Input::Tick),
            "storage_recovered" => Some(Input::StorageRecovered),
            _ => None,
        };
    }
    let (k, x) = v.as_object()?.iter().next()?;
    Some(match k.as_str() {
        "command" => Input::Command {
            id: x["id"].as_u64()?,
            cmd: x["cmd"].clone(),
        },
        "token" => Input::Token(Ok(x.as_str()?.to_string())),
        "token_error" => Input::Token(Err(x["refused"] == true)),
        "frame" => Input::Frame(x.as_str()?.to_string()),
        "closed" => Input::Closed {
            code: x["code"].as_u64().unwrap_or(1006) as u16,
            reason: x["reason"].as_str().unwrap_or("").to_string(),
        },
        "connect_failed" => Input::ConnectFailed {
            status: x["status"].as_u64().map(|s| s as u16),
            retry_after_ms: x["retry_after_ms"].as_i64(),
            detail: x["detail"].as_str().unwrap_or("").to_string(),
        },
        "network" => Input::Network {
            online: x["online"] != false,
            generation: x["generation"].as_u64().unwrap_or(0),
        },
        "foreground" => Input::Foreground(x.as_bool()?),
        "restored" => Input::Restored(serde_json::from_value(x.clone()).ok()?),
        "page_loaded" => Input::PageLoaded {
            req: x["req"].as_u64()?,
            conv_id: x["conv_id"].as_str().unwrap_or("").to_string(),
            messages: x["messages"].as_array().cloned().unwrap_or_default(),
        },
        "storage_failed" => Input::StorageFailed {
            kind: x.as_str()?.to_string(),
        },
        "persisted" => Input::Persisted(x.as_u64()?),
        _ => return None,
    })
}

pub fn effect_to_json(e: &Effect) -> Value {
    match e {
        Effect::NeedToken => json!({"need_token": true}),
        Effect::Connect { url } => json!({"connect": url}),
        Effect::Send(t) => json!({"send": t}),
        Effect::Close { code, reason } => json!({"close": {"code": code, "reason": reason}}),
        Effect::Persist(b) => json!({"persist": b}),
        Effect::LoadPage {
            req,
            conv_id,
            before_seq,
            limit,
        } => {
            json!({"load_page": {"req": req, "conv_id": conv_id, "before_seq": before_seq, "limit": limit}})
        }
        Effect::WipeStore => json!({"wipe_store": true}),
        Effect::Emit(v) => json!({"emit": v}),
        Effect::Reply { id, result } => match result {
            Ok(v) => json!({"reply": {"id": id, "ok": true, "result": v}}),
            Err(e) => json!({"reply": {"id": id, "ok": false, "error": e.to_json()}}),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn every_input_and_effect_round_trips() {
        let inputs = [
            json!({"command": {"id": 3, "cmd": {"cmd": "connect"}}}),
            json!({"token": "t"}),
            json!({"token_error": {"refused": true}}),
            json!("opened"),
            json!({"frame": "{}"}),
            json!({"closed": {"code": 1012, "reason": "server_restart"}}),
            json!({"connect_failed": {"status": 503, "retry_after_ms": 1000, "detail": "busy"}}),
            json!({"network": {"online": false, "generation": 4}}),
            json!({"foreground": true}),
            json!("tick"),
            json!({"restored": {"account": null, "convs": [], "ops": [], "messages": []}}),
            json!({"page_loaded": {"req": 1, "conv_id": "c", "messages": []}}),
            json!({"storage_failed": "full"}),
            json!("storage_recovered"),
            json!({"persisted": 9}),
        ];
        for i in &inputs {
            assert!(input_from_json(i).is_some(), "{i}");
        }
        for bad in [
            json!("nope"),
            json!({"nope": 1}),
            json!(5),
            json!({"command": {"cmd": {}}}),
        ] {
            assert!(input_from_json(&bad).is_none(), "{bad}");
        }
        let effects = [
            Effect::NeedToken,
            Effect::Connect {
                url: "wss://x".into(),
            },
            Effect::Send("{}".into()),
            Effect::Close {
                code: 1000,
                reason: "closed".into(),
            },
            Effect::Persist(Batch::default()),
            Effect::LoadPage {
                req: 1,
                conv_id: "c".into(),
                before_seq: 5,
                limit: 50,
            },
            Effect::WipeStore,
            Effect::Emit(json!({"type": "x"})),
            Effect::Reply {
                id: 1,
                result: Ok(json!({})),
            },
            Effect::Reply {
                id: 2,
                result: Err(ClientError::new("timeout", "t")),
            },
        ];
        for e in &effects {
            let v = effect_to_json(e);
            assert_eq!(v.as_object().unwrap().len(), 1, "{v}");
        }
    }
}
