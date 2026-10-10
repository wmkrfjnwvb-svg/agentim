//! Demo data and scripted agents. The agents talk to the hub through the same
//! JSON protocol a WebSocket client uses; they need no LLM.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::time::sleep;

use crate::hub::{Hub, SessionHandle};
use crate::model::*;
use crate::store::{NewMsg, Store};

pub fn seed(store: &mut Store) -> ApiResult<()> {
    if !store.is_empty()? {
        return Ok(());
    }
    let (alice, _) = store.create_user("alice", UserKind::Human, None)?;
    let (bob, _) = store.create_user("bob", UserKind::Human, None)?;
    let (carol, _) = store.create_user("carol", UserKind::Human, None)?;
    let (aria, _) = store.create_user("Aria", UserKind::Agent, Some(&alice.id))?;
    let (refund, _) = store.create_user("RefundBot", UserKind::Agent, Some(&alice.id))?;
    let (pong_a, _) = store.create_user("PingA", UserKind::Agent, Some(&alice.id))?;
    let (pong_b, _) = store.create_user("PongB", UserKind::Agent, Some(&bob.id))?;

    store.create_conv("dm", "Alice · Aria", &alice, std::slice::from_ref(&aria))?;
    let (group, _) = store.create_conv(
        "group",
        "产品讨论群",
        &alice,
        &[bob.clone(), carol.clone(), aria.clone()],
    )?;
    let (support, _) = store.create_conv(
        "group",
        "客服：退款工单",
        &bob,
        &[refund.clone(), alice.clone()],
    )?;
    store.set_agent_policy(
        &support,
        &refund.id,
        Some(true),
        Some(Trigger::All),
        &alice.id,
    )?;
    let (lab, _) = store.create_conv(
        "group",
        "Agent 循环实验室",
        &alice,
        &[pong_a.clone(), pong_b.clone()],
    )?;
    store.set_agent_policy(&lab, &pong_a.id, None, Some(Trigger::All), &alice.id)?;
    store.set_agent_policy(&lab, &pong_b.id, None, Some(Trigger::All), &bob.id)?;
    store.create_conv("dm", "Bob · Aria", &bob, std::slice::from_ref(&aria))?;

    let say = |store: &mut Store, conv: &str, who: &User, text: &str| -> ApiResult<()> {
        store.commit_message(NewMsg {
            conv_id: conv,
            sender: who,
            client_msg_id: None,
            on_behalf_of: None,
            kind: "text",
            content: text.to_string(),
            state: "committed",
            hop: 0,
            causation_id: None,
            reply_to: None,
            mentions: vec![],
        })?;
        Ok(())
    };
    say(
        store,
        &group,
        &alice,
        "欢迎来到 AgentIM 单机版演示。群里 @Aria 才会召唤 AI（群聊默认 trigger=mention）。",
    )?;
    say(
        store,
        &support,
        &bob,
        "你好，我的订单一直没发货，想申请退款。",
    )?;
    say(
        store,
        &lab,
        &alice,
        "这里有两个 Agent 会互相回复。发一条消息，看护栏如何在跳数上限处熔断。",
    )?;
    Ok(())
}

/// Minimal in-process client used by the scripted agents.
struct Client {
    session: SessionHandle,
    rid: Mutex<u64>,
}

impl Client {
    fn call(&self, mut v: Value) -> Value {
        let rid = {
            let mut r = self.rid.lock().unwrap();
            *r += 1;
            *r
        };
        v["rid"] = json!(rid);
        self.session.handle_text(&v.to_string())
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
    let users = hub.store(|s| s.list_users()).unwrap_or_default();
    for u in users.into_iter().filter(|u| u.kind == UserKind::Agent) {
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
                v = rx.recv() => match v { Some(v) => v, None => break },
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
    let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id}));
    if start["ok"] != true {
        return None;
    }
    let id = start["msg_id"].as_str()?.to_string();
    for (i, part) in chunks(text, 2).iter().enumerate() {
        sleep(Duration::from_millis(delay_ms)).await;
        if cancelled.lock().unwrap().remove(&id) {
            return Some(id);
        }
        let r = c.call(json!({"op": "stream_append", "msg_id": id, "idx": i, "text": part}));
        if r["ok"] != true {
            return Some(id);
        }
    }
    c.call(json!({"op": "stream_end", "msg_id": id}));
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
        let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id}));
        if let Some(id) = start["msg_id"].as_str() {
            for (i, p) in chunks("我开始回答了，但接下来会卡住不再输出……", 2)
                .iter()
                .enumerate()
            {
                sleep(Duration::from_millis(80)).await;
                c.call(json!({"op": "stream_append", "msg_id": id, "idx": i, "text": p}));
            }
        }
        return; // never ends: the server watchdog marks it failed(timeout)
    }
    if text.contains("/crash") {
        let start = c.call(json!({"op": "stream_start", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "reply_to": msg.msg_id}));
        if let Some(id) = start["msg_id"].as_str() {
            for (i, p) in chunks("我正在回答，然后进程会崩溃……", 2).iter().enumerate()
            {
                sleep(Duration::from_millis(80)).await;
                c.call(json!({"op": "stream_append", "msg_id": id, "idx": i, "text": p}));
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
    c.call(json!({"op": "send", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "reply_to": msg.msg_id}));
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
    c.call(json!({"op": "send", "conv_id": msg.conv_id, "client_msg_id": uuid::Uuid::new_v4().to_string(), "text": text, "reply_to": msg.msg_id}));
}
