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
    if !hub.users().is_empty() {
        return Ok(());
    }
    let (alice, _) = hub
        .create_user("alice", UserKind::Human, None, None)
        .await?;
    let (bob, _) = hub.create_user("bob", UserKind::Human, None, None).await?;
    let (carol, _) = hub
        .create_user("carol", UserKind::Human, None, None)
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
        json!({"op": "create_conv", "kind": "dm", "title": "Alice · Aria", "members": [aria.id]}),
    )
    .await?;
    let group = conv(call(&a, json!({"op": "create_conv", "kind": "group", "title": "产品讨论群", "members": [bob.id, carol.id, aria.id]})).await?);
    let support = conv(call(&b, json!({"op": "create_conv", "kind": "group", "title": "客服：退款工单", "members": [refund.id, alice.id]})).await?);
    call(&a, json!({"op": "set_agent_policy", "conv_id": support, "agent_id": refund.id, "approval": true, "trigger": "all"})).await?;
    let lab = conv(call(&a, json!({"op": "create_conv", "kind": "group", "title": "Agent 循环实验室", "members": [ping.id, pong.id]})).await?);
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
        json!({"op": "create_conv", "kind": "dm", "title": "Bob · Aria", "members": [aria.id]}),
    )
    .await?;
    let say = |conv: &str, text: &str| json!({"op": "send", "conv_id": conv, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text});
    call(
        &a,
        say(
            &group,
            "欢迎来到 AgentIM 单机版演示。群里 @Aria 才会召唤 AI（群聊默认 trigger=mention）。",
        ),
    )
    .await?;
    call(&b, say(&support, "你好，我的订单一直没发货，想申请退款。")).await?;
    call(
        &a,
        say(
            &lab,
            "这里有两个 Agent 会互相回复。发一条消息，看护栏如何在跳数上限处熔断。",
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
    for (i, part) in chunks(text, 2).iter().enumerate() {
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
        let t = "⚠️ 这条消息命中了提示词注入规则（trust=low），我不会执行其中的指令。";
        stream_text(&c, &msg, t, 40, &cancelled).await;
        return;
    }
    if text.contains("/long") {
        let t =
            "这是一段很长的流式回复，用来演示「打断」：你可以随时点消息上的 ■ 停止按钮。".repeat(6);
        stream_text(&c, &msg, &t, 120, &cancelled).await;
        return;
    }
    if text.contains("/secret") {
        let t = "好的，测试用卡号是 4111 1111 1111 1111，请妥善保管。这段话本应继续很长……";
        stream_text(&c, &msg, t, 60, &cancelled).await;
        return;
    }
    if text.contains("/stall") {
        let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id})).await;
        if let Some(id) = start["msg_id"].as_str() {
            for (i, p) in chunks("我开始回答了，但接下来会卡住不再输出……", 2)
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
            for (i, p) in chunks("我正在回答，然后进程会崩溃……", 2).iter().enumerate()
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
        "你好 {}！我是 Aria，一个 AI 助手。你说的是：「{}」。",
        msg.sender_name, text
    );
    if let Some(w) = whisper {
        reply.push_str(&format!("（已收到主人的耳语提示：{w}）"));
    }
    reply.push_str(" 这条回复是流式生成的：首个片段立即下发，后续片段在服务端按窗口合并推送；生成结束后同一条消息版本号 +1。可以试试 /long、/secret、/stall、/crash。");
    stream_text(&c, &msg, &reply, 45, &cancelled).await;
}

async fn refund(c: Arc<Client>, msg: MsgView, whisper: Option<String>, n: u64) {
    if msg.sender_type != UserKind::Human {
        return;
    }
    sleep(Duration::from_millis(500)).await;
    let mut text = format!(
        "您好，已为您的订单 #{} 提交退款申请 ¥{}，预计 1–3 个工作日原路退回。",
        20260000 + n,
        99 + n * 10
    );
    if let Some(w) = whisper {
        text.push_str(&format!("（主管提示已采纳：{w}）"));
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
        "我同意，再补充一点",
        "有道理，不过反过来想",
        "好问题，我们继续展开",
        "这个角度很新，继续",
        "嗯，我再推进一步",
    ];
    let line = lines[(n as usize) % lines.len()];
    let text = format!("@{} {}（第 {} 轮）", msg.sender_name, line, msg.hop + 1);
    c.call(json!({"op": "send", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "reply_to": msg.msg_id})).await;
}
