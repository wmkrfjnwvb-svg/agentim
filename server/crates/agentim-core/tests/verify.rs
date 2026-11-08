//! `agentim verify` (docs/en/design/tracing.md). Every three-database test already
//! runs it on what it leaves behind (`common::verified`); here each kind of
//! damage is made to a database on purpose and the verifier must name it
//! (the mutation check that proves each invariant can fail).

mod common;

use agentim_core::hub::Hub;
use agentim_core::hub::verify::CHECKS;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    each_kind_of_damage_is_found,
    a_clean_database_has_nothing_to_report
);

/// Runs one statement with the conversation id bound to its `?`.
async fn exec(db: &TestDb, sql: &str, conv: &str) {
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url).await.unwrap();
    let q = if db.url.starts_with("postgres") {
        sql.replace('?', "$1")
    } else {
        sql.to_string()
    };
    sqlx::query(sqlx::AssertSqlSafe(q))
        .bind(conv.to_string())
        .execute(&mut c)
        .await
        .unwrap();
}

/// A conversation with a little of everything; returns it and the seqs and
/// ids the damage aims at.
struct Story {
    conv: String,
    join: i64,
    unsilence: i64,
    m1: String,
    edit1: i64,
    m2: String,
    edit2: i64,
    recall2: i64,
}

fn seq_of(log: &Value, kind: &str, nth: usize) -> i64 {
    log["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["type"] == kind)
        .nth(nth)
        .unwrap_or_else(|| panic!("no {kind} #{nth}"))["event"]["seq"]
        .as_i64()
        .unwrap()
}

async fn story(hub: &Hub, alice: &User, carol: &User, bot: &User) -> Story {
    let a = Conn::new(hub, alice);
    let conv = a.group(&[bot]).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    hub.silence(&conv, carol, Some(60), "admin:t", Some("calm".into()))
        .await
        .unwrap();
    hub.silence(&conv, carol, None, "admin:t", None)
        .await
        .unwrap();
    let c = Conn::new(hub, carol);
    assert_eq!(c.send(&conv, "hello").await["ok"], true);
    let m1 = a.send(&conv, "one").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "edit", "msg_id": m1, "text": "one!"}))
        .await;
    let m2 = a.send(&conv, "two").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "edit", "msg_id": m2, "text": "two!"}))
        .await;
    a.ok(json!({"op": "recall", "msg_id": m2})).await;
    a.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": "psst"}))
        .await;
    let log = a.pull(&conv, 0).await;
    Story {
        join: seq_of(&log, "member.joined", 0),
        unsilence: seq_of(&log, "member.unsilenced", 0),
        edit1: seq_of(&log, "msg.updated", 0),
        edit2: seq_of(&log, "msg.updated", 1),
        recall2: seq_of(&log, "msg.recalled", 0),
        conv,
        m1,
        m2,
    }
}

async fn checks(hub: &Hub, conv: Option<&str>) -> Vec<&'static str> {
    let mut v: Vec<&'static str> = hub
        .verify(conv, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|f| conv.is_some() || f.check == "order.orphan")
        .map(|f| f.check)
        .collect();
    v.sort();
    v.dedup();
    v
}

async fn each_kind_of_damage_is_found(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let mut found: Vec<&'static str> = vec![];
    // Each damage on a conversation of its own: (statement, expected check).
    let damage = |s: &Story| -> Vec<(String, &'static str)> {
        vec![
            (
                format!("DELETE FROM events WHERE conv_id = ? AND seq = {}", s.edit1),
                "order.gap",
            ),
            (
                "UPDATE events SET type = 'conv.created' WHERE conv_id = ? AND seq = 2".into(),
                "order.created",
            ),
            (
                format!(
                    "UPDATE events SET type = 'member.left' WHERE conv_id = ? AND seq = {}",
                    s.join
                ),
                "member.sender",
            ),
            (
                format!(
                    "UPDATE events SET type = 'x.noop' WHERE conv_id = ? AND seq = {}",
                    s.unsilence
                ),
                "member.silenced",
            ),
            (
                format!(
                    "DELETE FROM members WHERE conv_id = ? AND user_id = '{}'",
                    carol.id
                ),
                "member.state",
            ),
            (
                format!(
                    "DELETE FROM messages WHERE conv_id = ? AND msg_id = '{}'",
                    s.m1
                ),
                "msg.event",
            ),
            (
                format!(
                    "UPDATE events SET type = CASE WHEN seq = {} THEN 'msg.recalled' ELSE 'msg.updated' END WHERE conv_id = ? AND seq IN ({}, {})",
                    s.edit2, s.edit2, s.recall2
                ),
                "msg.after_recall",
            ),
            (
                format!(
                    "UPDATE events SET actor_id = 'u_nobody' WHERE conv_id = ? AND seq = {}",
                    s.edit1
                ),
                "msg.editor",
            ),
            (
                format!(
                    "UPDATE messages SET version = version + 5 WHERE conv_id = ? AND msg_id = '{}'",
                    s.m1
                ),
                "msg.version",
            ),
            (
                format!(
                    "UPDATE messages SET state = 'committed' WHERE conv_id = ? AND msg_id = '{}'",
                    s.m2
                ),
                "msg.recalled",
            ),
            (
                "UPDATE dedup SET result = '{\"msg_id\":\"m_nope\"}' WHERE conv_id = ?".into(),
                "dedup.target",
            ),
            (
                format!(
                    "UPDATE messages SET state = 'streaming', updated_at = 1 WHERE conv_id = ? AND msg_id = '{}'",
                    s.m1
                ),
                "stream.stuck",
            ),
            (
                "UPDATE side_events SET audience = '[\"u_nobody\"]' WHERE conv_id = ?".into(),
                "side.audience",
            ),
            (
                format!(
                    "INSERT INTO files (file_id, conv_id, uploader_id, name, size, mime, kind, thumb_size, state, part_size, ref_id, ai, created_at) VALUES ('f_x', ?, 'u_nobody', 'x', 1, 'text/plain', 'file', 0, 'ready', 0, '{}', 0, 1)",
                    s.m1
                ),
                "file.ref",
            ),
        ]
    };
    let probe = story(&hub, &alice, &carol, &bot).await;
    let list = damage(&probe);
    assert_eq!(
        checks(&hub, Some(&probe.conv)).await,
        Vec::<&str>::new(),
        "clean before"
    );
    for (_, want) in list {
        let s = story(&hub, &alice, &carol, &bot).await;
        let sql = damage(&s).into_iter().find(|d| d.1 == want).unwrap().0;
        assert!(checks(&hub, Some(&s.conv)).await.is_empty());
        exec(&db, &sql, &s.conv).await;
        let got = checks(&hub, Some(&s.conv)).await;
        assert!(got.contains(&want), "{want}: {sql}\n found {got:?}");
        found.push(want);
    }
    // A direct conversation with a third person.
    let a = Conn::new(&hub, &alice);
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "d", "members": [carol.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    exec(
        &db,
        "INSERT INTO members (conv_id, user_id, name, kind, role, trigger_mode, approval, join_seq) VALUES (?, 'u_x', 'x', 'human', 'member', 'all', 0, 1)",
        &dm,
    )
    .await;
    assert!(checks(&hub, Some(&dm)).await.contains(&"member.dm"));
    found.push("member.dm");
    // Rows of a conversation that is gone.
    let gone = story(&hub, &alice, &carol, &bot).await;
    exec(&db, "DELETE FROM conversations WHERE id = ?", &gone.conv).await;
    assert!(checks(&hub, None).await.contains(&"order.orphan"));
    found.push("order.orphan");
    // Every check the verifier has is proven here.
    let mut all: Vec<&str> = CHECKS.iter().map(|c| c.0).collect();
    all.sort();
    found.sort();
    found.dedup();
    assert_eq!(found, all, "a check without a mutation test");
}

async fn a_clean_database_has_nothing_to_report(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    for _ in 0..3 {
        story(&hub, &alice, &carol, &bot).await;
    }
    assert_eq!(hub.verify(None, None).await.unwrap(), vec![]);
    // Only the conversations active since a time.
    let later = now_ms() + 60_000;
    assert_eq!(hub.verify(None, Some(later)).await.unwrap(), vec![]);
}
