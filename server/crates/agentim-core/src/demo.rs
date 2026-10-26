//! Demo data and scripted agents. The agents talk to the hub through the same
//! JSON protocol a WebSocket client uses; they need no LLM.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::sleep;

use crate::hub::{Hub, Session};
use crate::model::*;

async fn call(s: &Session, v: Value) -> ApiResult<Value> {
    let r = s.handle_text(&v.to_string()).await;
    if r["ok"] != true {
        return Err(ApiError::new("seed", r.to_string()));
    }
    Ok(r)
}

/// Creates the demo users and conversations through the normal request path.
pub async fn seed(hub: &Hub) -> ApiResult<()> {
    // The demo signs its own client tokens, as a developer's backend
    // would (docs/13 §4.2).
    if hub
        .signing_keys()
        .await?
        .iter()
        .all(|k| !k["revoked_at"].is_null())
    {
        hub.create_signing_key("demo").await?;
    }
    if !hub.users().is_empty() {
        return Ok(());
    }
    let (alice, _) = hub
        .create_user("Alice", UserKind::Human, None, None)
        .await?;
    let (bob, _) = hub.create_user("Bob", UserKind::Human, None, None).await?;
    let (carol, _) = hub
        .create_user("Carol", UserKind::Human, None, None)
        .await?;
    let (aria, _) = hub
        .create_user("Aria", UserKind::Agent, Some(&alice.id), None)
        .await?;
    let (refund, _) = hub
        .create_user("RefundBot", UserKind::Agent, Some(&alice.id), None)
        .await?;
    let (ping, _) = hub
        .create_user("PingA", UserKind::Agent, Some(&alice.id), None)
        .await?;
    let (pong, _) = hub
        .create_user("PongB", UserKind::Agent, Some(&bob.id), None)
        .await?;

    let (a, _arx) = hub.connect(alice.clone());
    let (b, _brx) = hub.connect(bob.clone());
    let conv = |r: Value| r["conv_id"].as_str().unwrap_or_default().to_string();
    call(
        &a,
        json!({"op": "create_conv", "kind": "dm", "title": "Aria", "members": [aria.id]}),
    )
    .await?;
    let group = conv(call(&a, json!({"op": "create_conv", "kind": "group", "title": "Product team", "members": [bob.id, carol.id, aria.id]})).await?);
    let support = conv(call(&b, json!({"op": "create_conv", "kind": "group", "title": "Support: refund request", "members": [refund.id, alice.id]})).await?);
    call(&a, json!({"op": "set_agent_policy", "conv_id": support, "agent_id": refund.id, "approval": true, "trigger": "all"})).await?;
    let lab = conv(call(&a, json!({"op": "create_conv", "kind": "group", "title": "Agent loop lab", "members": [ping.id, pong.id, bob.id]})).await?);
    call(
        &a,
        json!({"op": "set_agent_policy", "conv_id": lab, "agent_id": ping.id, "trigger": "all"}),
    )
    .await?;
    call(
        &b,
        json!({"op": "set_agent_policy", "conv_id": lab, "agent_id": pong.id, "trigger": "all"}),
    )
    .await?;
    call(
        &b,
        json!({"op": "create_conv", "kind": "dm", "title": "Aria", "members": [aria.id]}),
    )
    .await?;
    let say = |conv: &str, text: &str| json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text});
    call(
        &a,
        say(
            &group,
            "Welcome to the AgentIM demo! In a group, mention @Aria to bring in the AI (groups default to trigger=mention).",
        ),
    )
    .await?;
    call(
        &b,
        say(
            &support,
            "Hi, my order still hasn’t shipped. I’d like a refund, please.",
        ),
    )
    .await?;
    call(
        &a,
        say(
            &lab,
            "Two agents here answer each other. Send a message and watch the guardrail stop them at the hop limit.",
        ),
    )
    .await?;
    Ok(())
}

/// Minimal in-process client used by the scripted agents.
struct Client {
    session: Session,
    rid: Mutex<u64>,
}

impl Client {
    async fn call(&self, mut v: Value) -> Value {
        let rid = {
            let mut r = self.rid.lock().unwrap();
            *r += 1;
            *r
        };
        v["rid"] = json!(rid);
        self.session.handle_text(&v.to_string()).await
    }
    fn me(&self) -> &User {
        &self.session.user
    }
}

#[derive(Clone, Copy)]
enum Behavior {
    Assistant,
    Refund,
    PingA,
    PongB,
}

pub fn spawn_agents(hub: &Hub) {
    for u in hub
        .users()
        .into_iter()
        .filter(|u| u.kind == UserKind::Agent)
    {
        let b = match u.name.as_str() {
            "Aria" => Behavior::Assistant,
            "RefundBot" => Behavior::Refund,
            "PingA" => Behavior::PingA,
            "PongB" => Behavior::PongB,
            _ => continue,
        };
        let hub = hub.clone();
        tokio::spawn(async move { run_agent(hub, u, b).await });
    }
}

async fn run_agent(hub: Hub, user: User, behavior: Behavior) {
    let whispers: Arc<Mutex<HashMap<String, String>>> = Arc::default();
    let cancelled: Arc<Mutex<HashSet<String>>> = Arc::default();
    let mut counter = 0u64;
    loop {
        let (session, mut rx) = hub.connect(user.clone());
        let client = Arc::new(Client {
            session,
            rid: Mutex::new(0),
        });
        let crash = Arc::new(tokio::sync::Notify::new());
        loop {
            let v = tokio::select! {
                v = rx.recv() => match v { Some(f) => serde_json::from_str::<Value>(&f).unwrap_or(Value::Null), None => break },
                _ = crash.notified() => break,
            };
            match v["type"].as_str() {
                Some("side") => {
                    let ev = &v["event"];
                    if ev["type"] == "whisper" {
                        whispers.lock().unwrap().insert(
                            ev["conv_id"].as_str().unwrap_or("").into(),
                            ev["data"]["text"].as_str().unwrap_or("").into(),
                        );
                    }
                }
                Some("stream_cancelled") => {
                    cancelled
                        .lock()
                        .unwrap()
                        .insert(v["msg_id"].as_str().unwrap_or("").into());
                }
                // A card answered (docs/03-agents.md §3): thank them, with sources.
                Some("event")
                    if v["event"]["type"] == "card.response"
                        && v["event"]["data"]["to"] == json!(client.me().id) =>
                {
                    let d = &v["event"]["data"];
                    let choice = d["action"]
                        .as_str()
                        .map(String::from)
                        .unwrap_or_else(|| d["values"].to_string());
                    client.call(json!({"op": "send", "conv_id": v["event"]["conv_id"], "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": format!("Got it. You chose “{choice}”."), "sources": [{"title": "How AgentIM cards work", "url": "https://github.com/wmkrfjnwvb-svg/agentim", "excerpt": "Declarative cards, no scripts"}]})).await;
                }
                Some("event") if v["event"]["type"] == "msg.created" => {
                    let msg: MsgView = match serde_json::from_value(v["event"]["msg"].clone()) {
                        Ok(m) => m,
                        Err(_) => continue,
                    };
                    if msg.sender_id == client.me().id {
                        continue;
                    }
                    let flags: Vec<String> =
                        serde_json::from_value(v["flags"].clone()).unwrap_or_default();
                    let whisper = whispers.lock().unwrap().remove(&msg.conv_id);
                    counter += 1;
                    let c = client.clone();
                    let cancelled = cancelled.clone();
                    let crash = crash.clone();
                    let n = counter;
                    tokio::spawn(async move {
                        match behavior {
                            Behavior::Assistant => {
                                assistant(c, msg, flags, whisper, cancelled, crash).await
                            }
                            Behavior::Refund => refund(c, msg, whisper, n).await,
                            Behavior::PingA | Behavior::PongB => {
                                pingpong(c, msg, behavior, n).await
                            }
                        }
                    });
                }
                _ => {}
            }
        }
        // Connection dropped (simulated crash): come back after a while.
        drop(client);
        drop(rx);
        sleep(Duration::from_secs(3)).await;
    }
}

/// Characters per streamed chunk.
const CHUNK: usize = 3;

fn chunks(text: &str, n: usize) -> Vec<String> {
    let cs: Vec<char> = text.chars().collect();
    cs.chunks(n).map(|c| c.iter().collect()).collect()
}

async fn stream_text(
    c: &Client,
    msg: &MsgView,
    text: &str,
    delay_ms: u64,
    cancelled: &Mutex<HashSet<String>>,
) -> Option<String> {
    let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id})).await;
    if start["ok"] != true {
        return None;
    }
    let id = start["msg_id"].as_str()?.to_string();
    for (i, part) in chunks(text, CHUNK).iter().enumerate() {
        sleep(Duration::from_millis(delay_ms)).await;
        if cancelled.lock().unwrap().remove(&id) {
            return Some(id);
        }
        let r = c
            .call(json!({"op": "stream_append", "msg_id": id, "idx": i, "text": part}))
            .await;
        if r["ok"] != true {
            return Some(id);
        }
    }
    c.call(json!({"op": "stream_end", "msg_id": id})).await;
    Some(id)
}

async fn assistant(
    c: Arc<Client>,
    msg: MsgView,
    flags: Vec<String>,
    whisper: Option<String>,
    cancelled: Arc<Mutex<HashSet<String>>>,
    crash: Arc<tokio::sync::Notify>,
) {
    sleep(Duration::from_millis(300)).await;
    let text = msg.content.trim();
    if flags.iter().any(|f| f == "possible_injection") {
        let t = "⚠️ That message matched a prompt-injection rule (trust=low), so I won’t follow its instructions.";
        stream_text(&c, &msg, t, 40, &cancelled).await;
        return;
    }
    if text.contains("/long") {
        let t =
            "This is a long streamed reply to show interrupting: press Stop on the message at any time. "
                .repeat(6);
        stream_text(&c, &msg, &t, 120, &cancelled).await;
        return;
    }
    if text.contains("/card") {
        c.call(json!({"op": "progress", "conv_id": msg.conv_id, "kind": "thinking", "text": "Preparing a card"}))
            .await;
        c.call(json!({"op": "send", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id, "text": "Want to continue? (yes / no)", "kind": "agentim.card", "payload": {"title": "Want to continue?", "blocks": [{"type": "text", "text": "Pick one and I’ll get your answer."}, {"type": "buttons", "id": "go", "options": [{"id": "yes", "label": "Continue", "style": "primary"}, {"id": "no", "label": "Not now"}]}]}}))
            .await;
        return;
    }
    if text.contains("/secret") {
        let t = "Sure, the test card number is 4111 1111 1111 1111. Keep it safe. This was going to go on for a while…";
        stream_text(&c, &msg, t, 60, &cancelled).await;
        return;
    }
    if text.contains("/stall") {
        let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id})).await;
        if let Some(id) = start["msg_id"].as_str() {
            for (i, p) in chunks("I’ve started answering, but I’m about to get stuck…", CHUNK)
                .iter()
                .enumerate()
            {
                sleep(Duration::from_millis(80)).await;
                c.call(json!({"op": "stream_append", "msg_id": id, "idx": i, "text": p}))
                    .await;
            }
        }
        return; // never ends: the server watchdog marks it failed(timeout)
    }
    if text.contains("/crash") {
        let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id})).await;
        if let Some(id) = start["msg_id"].as_str() {
            for (i, p) in chunks("I’m answering, and then my process will crash…", CHUNK)
                .iter()
                .enumerate()
            {
                sleep(Duration::from_millis(80)).await;
                c.call(json!({"op": "stream_append", "msg_id": id, "idx": i, "text": p}))
                    .await;
            }
        }
        crash.notify_one();
        return;
    }
    let mut reply = format!(
        "Hi {}! I’m Aria, an AI assistant. You said: “{}”.",
        msg.sender_name, text
    );
    if let Some(w) = whisper {
        reply.push_str(&format!(" (My owner whispered: {w})"));
    }
    reply.push_str(" This reply is streamed: the first chunk goes out at once, and the server merges the rest in short windows; when it ends, the message’s version goes up by one. Try /long, /card, /secret, /stall or /crash.");
    stream_text(&c, &msg, &reply, 45, &cancelled).await;
}

async fn refund(c: Arc<Client>, msg: MsgView, whisper: Option<String>, n: u64) {
    if msg.sender_type != UserKind::Human {
        return;
    }
    sleep(Duration::from_millis(500)).await;
    let mut text = format!(
        "Hi! I’ve filed a refund of ${}.00 for order #{}. It should reach your original payment method in 1–3 business days.",
        99 + n * 10,
        20260000 + n
    );
    if let Some(w) = whisper {
        text.push_str(&format!(" (Supervisor note applied: {w})"));
    }
    c.call(json!({"op": "send", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "reply_to": msg.msg_id})).await;
}

async fn pingpong(c: Arc<Client>, msg: MsgView, b: Behavior, n: u64) {
    // PongB only answers agents, so a human message starts exactly one chain.
    if matches!(b, Behavior::PongB) && msg.sender_type != UserKind::Agent {
        return;
    }
    sleep(Duration::from_millis(700)).await;
    let lines = [
        "I agree, and one more thing",
        "Fair point, but consider the flip side",
        "Good question, let’s dig deeper",
        "That’s a fresh angle, keep going",
        "Hmm, let me push that further",
    ];
    let line = lines[(n as usize) % lines.len()];
    let text = format!("@{} {} (round {})", msg.sender_name, line, msg.hop + 1);
    c.call(json!({"op": "send", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "reply_to": msg.msg_id})).await;
}
