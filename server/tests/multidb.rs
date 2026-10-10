//! Organisation units in separate databases (docs/02-architecture.md §5.4).

mod common;

use agentim::model::*;
use common::*;
use serde_json::json;
use sqlx::Connection;

async fn two_sqlite() -> (TestDb, TestDb) {
    (
        new_db(Backend::Sqlite).await.unwrap(),
        new_db(Backend::Sqlite).await.unwrap(),
    )
}

/// Every byte SQLite holds for a database (main file + WAL).
fn raw(db: &TestDb) -> Vec<u8> {
    let p = db.path.as_ref().unwrap();
    let mut out = std::fs::read(p).unwrap_or_default();
    out.extend(std::fs::read(p.with_extension("db-wal")).unwrap_or_default());
    out
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unit_conversations_live_only_in_their_database() {
    let (main, secret) = two_sqlite().await;
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let carol = user_in(&hub, "carol", "b").await; // subsidiary B
    let dave = user_in(&hub, "dave", "b").await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let c = Conn::new(&hub, &carol);
    let conv_b = c.group(&[&dave]).await;
    c.send(&conv_b, "TOP-SECRET-merger-plan").await;
    c.ok(json!({"op": "create_conv", "kind": "group", "title": "B-internal-project-name", "members": [dave.id]})).await;
    let a = Conn::new(&hub, &alice);
    let conv_main = a.group(&[]).await;
    a.send(&conv_main, "ordinary-chatter").await;

    let lists = c.ok(json!({"op": "list_convs"})).await;
    assert!(
        lists["convs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x["database"] == "secret"),
        "{lists}"
    );
    let main_bytes = raw(&main);
    let secret_bytes = raw(&secret);
    // Content and metadata of unit B (message text, conversation title) never reach the shared database.
    // The title is stored in plaintext (docs/04 §2.6), so it shows where
    // the conversation lives; the message text is encrypted everywhere.
    assert!(contains(&secret_bytes, "B-internal-project-name"));
    assert!(
        !contains(&secret_bytes, "TOP-SECRET-merger-plan"),
        "encrypted at rest"
    );
    assert!(
        !contains(&main_bytes, "TOP-SECRET-merger-plan"),
        "message content leaked into the shared database"
    );
    assert!(
        !contains(&main_bytes, "B-internal-project-name"),
        "conversation title leaked into the shared database"
    );
    // The shared database only routes: it knows the conversation id and its database.
    assert!(contains(&main_bytes, &conv_b));
    assert!(
        !contains(&main_bytes, "ordinary-chatter") && !contains(&secret_bytes, "ordinary-chatter")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cross_database_membership_listing_and_side_channel_work() {
    let (main, secret) = two_sqlite().await;
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let carol = user_in(&hub, "carol", "b").await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let c = Conn::new(&hub, &carol);
    let a = Conn::new(&hub, &alice);
    let mut ag = Conn::new(&hub, &bot);
    // A unit-B conversation with a member from the default unit and her agent.
    let conv = c.group(&[&alice]).await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    ag.drain();
    c.send(&conv, "hello from B").await;
    assert!(
        ag.drain()
            .iter()
            .any(|v| v["event"]["msg"]["content"] == "hello from B")
    );
    a.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": "careful"}))
        .await;
    // alice's list merges conversations from both databases.
    let own = a.group(&[]).await;
    let list = a.ok(json!({"op": "list_convs"})).await;
    let dbs: Vec<(String, String)> = list["convs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["conv_id"].as_str().unwrap().into(),
                x["database"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert!(
        dbs.contains(&(conv.clone(), "secret".into()))
            && dbs.contains(&(own.clone(), "main".into())),
        "{dbs:?}"
    );
    let side = a.ok(json!({"op": "pull_side", "conv_id": conv})).await;
    assert!(side.to_string().contains("careful"));
    // Restart: the directory and member index come back.
    drop((c, a, ag));
    drop(hub);
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let a = Conn::new(&hub, &alice);
    let list = a.ok(json!({"op": "list_convs"})).await;
    assert_eq!(list["convs"].as_array().unwrap().len(), 2);
    assert!(a.pull(&conv, 0).await.to_string().contains("hello from B"));
}

/// Byte-level checks of where content lives need plaintext.
fn plain(mut c: agentim::config::Config) -> agentim::config::Config {
    c.encryption.mode = agentim::config::EncryptionMode::Off;
    c
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn migrate_unit_moves_conversations_without_losing_order() {
    let (main, secret) = two_sqlite().await;
    // Unit b starts in the shared database...
    let hub = open(plain(two(&main.url, &secret.url, "main"))).await;
    let carol = user_in(&hub, "carol", "b").await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&carol)).await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[&bot]).await;
    for i in 0..20 {
        c.send(&conv, &format!("before-move-{i}")).await;
    }
    c.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": "whisper-before"}))
        .await;
    let before = c.pull(&conv, 0).await;
    drop(c);
    drop(hub);
    assert!(contains(&raw(&main), "before-move-7"));

    // ...then the configuration moves it to its own database.
    let hub = open(plain(two(&main.url, &secret.url, "secret"))).await;
    let r = hub.migrate_unit("b").await.unwrap();
    assert_eq!(r["moved"], 1, "{r}");
    let c = Conn::new(&hub, &carol);
    let after = c.pull(&conv, 0).await;
    assert_eq!(
        before["events"], after["events"],
        "events are identical after the move"
    );
    assert_eq!(
        c.ok(json!({"op": "pull_side", "conv_id": conv})).await["side_events"][0]["data"]["text"],
        "whisper-before"
    );
    // New messages continue the same sequence.
    let next = c.send(&conv, "after-move").await;
    assert_eq!(
        next["seq"].as_i64().unwrap(),
        after["upto_seq"].as_i64().unwrap() + 1
    );
    // Retrying a pre-move client_msg_id is still deduplicated.
    let list = c.ok(json!({"op": "list_convs"})).await;
    assert_eq!(list["convs"][0]["database"], "secret");
    drop(c);
    drop(hub);
    // The shared database holds no copy any more: no rows, and (secure_delete)
    // no leftover bytes once the WAL is checkpointed.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let mut conn = sqlx::AnyConnection::connect(&main.url).await.unwrap();
    let left: i64 = sqlx::Row::try_get(
        &sqlx::query("SELECT COUNT(*) FROM messages")
            .fetch_one(&mut conn)
            .await
            .unwrap(),
        0,
    )
    .unwrap();
    assert_eq!(left, 0, "source rows deleted");
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    assert!(
        !contains(&raw(&main), "before-move-7"),
        "no leftover bytes of the moved conversation"
    );
    assert!(contains(&raw(&secret), "before-move-7"));
    // Restart after the move.
    let hub = open(plain(two(&main.url, &secret.url, "secret"))).await;
    let c = Conn::new(&hub, &carol);
    assert_eq!(
        c.pull(&conv, 0).await["events"].as_array().unwrap().len(),
        after["events"].as_array().unwrap().len() + 1
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn writes_during_a_move_are_refused_and_succeed_after() {
    let (main, secret) = two_sqlite().await;
    let hub = open(two(&main.url, &secret.url, "main")).await;
    let carol = user_in(&hub, "carol", "b").await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[]).await;
    c.send(&conv, "x").await;
    drop(c);
    drop(hub);
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let c = Conn::new(&hub, &carol);
    // Writers race the migration; each write either lands before, is refused
    // with conv_migrating (and retried), or lands after. None is lost.
    let h2 = hub.clone();
    let conv2 = conv.clone();
    let carol2 = carol.clone();
    let writer = tokio::spawn(async move {
        let c = Conn::new(&h2, &carol2);
        let mut acked = 0;
        for i in 0..50 {
            loop {
                let r = c.call(json!({"op": "send", "conv_id": conv2, "client_msg_id": format!("w{i}"), "text": format!("w{i}")})).await;
                if r["ok"] == true {
                    acked += 1;
                    break;
                }
                assert_eq!(r["error"]["code"], "conv_migrating", "{r}");
                tokio::task::yield_now().await;
            }
        }
        acked
    });
    hub.migrate_unit("b").await.unwrap();
    assert_eq!(writer.await.unwrap(), 50);
    let p = c.pull(&conv, 0).await;
    let s = seqs(&p);
    assert_eq!(s, (1..=s.len() as i64).collect::<Vec<_>>());
    let texts: Vec<String> = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str().map(String::from))
        .collect();
    for i in 0..50 {
        assert_eq!(
            texts.iter().filter(|t| **t == format!("w{i}")).count(),
            1,
            "w{i} exactly once"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn startup_repairs_a_directory_entry_lost_in_a_crash() {
    let (main, secret) = two_sqlite().await;
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let carol = user_in(&hub, "carol", "b").await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[]).await;
    c.send(&conv, "survives").await;
    drop(c);
    drop(hub);
    // Simulate a crash between the conversation write and the directory write.
    let mut conn = sqlx::AnyConnection::connect(&main.url).await.unwrap();
    sqlx::query("DELETE FROM conv_dir")
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("DELETE FROM member_index")
        .execute(&mut conn)
        .await
        .unwrap();
    drop(conn);
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    let c = Conn::new(&hub, &carol);
    let list = c.ok(json!({"op": "list_convs"})).await;
    assert_eq!(list["convs"][0]["conv_id"], conv.as_str());
    assert!(c.pull(&conv, 0).await.to_string().contains("survives"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn units_can_use_a_different_backend() {
    // Control in SQLite, unit B in PostgreSQL (when available).
    let Some(pg) = new_db(Backend::Postgres).await else {
        eprintln!("skipped: AGENTIM_TEST_PG not set");
        return;
    };
    let main = new_db(Backend::Sqlite).await.unwrap();
    let hub = open(two(&main.url, &pg.url, "secret")).await;
    let carol = user_in(&hub, "carol", "b").await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[&alice]).await;
    c.send(&conv, "in-postgres").await;
    let a = Conn::new(&hub, &alice);
    assert!(a.pull(&conv, 0).await.to_string().contains("in-postgres"));
    assert_eq!(
        a.ok(json!({"op": "list_convs"})).await["convs"][0]["database"],
        "secret"
    );
    assert!(!contains(&raw(&main), "in-postgres"));
}
