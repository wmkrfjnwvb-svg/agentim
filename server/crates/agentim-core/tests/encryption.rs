//! Encryption at rest (docs/04-security-compliance.md §2.6), on by default:
//! no content readable in any table, restarts, master-key checks, the
//! generated key of a single SQLite database, tampered and moved values,
//! turning encryption on for an old database, key rotation,
//! crypto-shredding on dissolve, conversations moving between databases,
//! and one data key per conversation under concurrent writes.

mod common;

use std::sync::Arc;

use agentim_core::config::{Config, EncryptionMode, WebhookConfig};
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::{Connection, Row};

backend_tests!(
    nothing_is_readable_at_rest,
    the_master_key_is_checked,
    tampered_or_moved_content_is_refused,
    an_old_database_is_encrypted_in_place,
    rotating_the_master_key,
    dissolving_shreds_the_keys,
    race_one_data_key_per_conversation,
);

const MARK: &str = "Zebra-Quartz-Secret";

async fn raw(db: &TestDb) -> (sqlx::AnyConnection, agentim_core::storage::Dialect) {
    sqlx::any::install_default_drivers();
    let d = agentim_core::storage::Dialect::from_url(&db.url).unwrap();
    (
        sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
            .await
            .unwrap(),
        d,
    )
}

/// Every text value of every table that contains `needle`.
async fn find_everywhere(db: &TestDb, needle: &str) -> Vec<String> {
    let (mut c, _) = raw(db).await;
    let mut hits = vec![];
    for t in agentim_core::storage::TABLES {
        let rows = sqlx::query(sqlx::AssertSqlSafe(format!("SELECT * FROM {}", t.name)))
            .fetch_all(&mut c)
            .await
            .unwrap();
        for r in rows {
            for col in t.cols {
                if let Ok(Some(v)) = r.try_get::<Option<String>, _>(col.name)
                    && v.contains(needle)
                {
                    hits.push(format!("{}.{}", t.name, col.name));
                }
            }
        }
    }
    hits
}

async fn keys_of(db: &TestDb, conv: &str) -> i64 {
    let (mut c, d) = raw(db).await;
    sqlx::query(d.q("SELECT COUNT(*) FROM data_keys WHERE conv_id = ?"))
        .bind(conv.to_string())
        .fetch_one(&mut c)
        .await
        .unwrap()
        .get(0)
}

async fn count(db: &TestDb, sql: &str) -> i64 {
    let (mut c, _) = raw(db).await;
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_one(&mut c)
        .await
        .unwrap()
        .get(0)
}

fn with_key(mut c: Config, key: Option<&str>) -> Config {
    c.encryption.key = key.map(String::from);
    c
}

async fn everything(hub: &Hub, alice: &User, bob: &User, bot: &User) -> String {
    let a = Conn::new(hub, alice);
    let g = Conn::new(hub, bot);
    let conv = a.group(&[bob, bot]).await;
    a.send(&conv, &format!("{MARK} plain")).await;
    a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "k1", "text": "[x]", "kind": "com.acme.x", "payload": {"v": format!("{MARK} payload")}}))
        .await;
    a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "k2", "text": format!("{MARK} to bob"), "to_user": bob.id}))
        .await;
    a.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": bot.id, "text": format!("{MARK} whisper")}))
        .await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true, "trigger": "all"}))
        .await;
    let q = a.send(&conv, "question").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let d = g.reply(&conv, &format!("{MARK} draft"), &q).await;
    a.ok(json!({"op": "approve", "draft_id": d["draft_id"], "text": format!("{MARK} edited")}))
        .await;
    let e = a.send(&conv, "to edit").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    a.ok(json!({"op": "edit", "msg_id": e, "text": format!("{MARK} edit")}))
        .await;
    // A routing envelope.
    a.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "env", "text": "with envelope", "refs": [format!("{MARK} ref")]}))
        .await;
    // A request for a decision: question, context and answer.
    let r = g
        .ok(json!({"op": "request_human", "conv_id": conv, "kind": "answer", "title": format!("{MARK} question"), "context": {"c": format!("{MARK} context")}, "client_req_id": "enc"}))
        .await;
    a.ok(json!({"op": "decide", "request_id": r["request_id"], "decision": "answer", "text": format!("{MARK} answer")}))
        .await;
    // The agent's own summary (docs/09 §11.1).
    let last = *seqs(&a.pull(&conv, 0).await).last().unwrap();
    g.ok(json!({"op": "put_summary", "conv_id": conv, "from_seq": 1, "to_seq": last, "text": format!("{MARK} summary")}))
        .await;
    // A checkpoint, with an older version (docs/09 §11.2).
    g.ok(json!({"op": "state_put", "key": "plan", "value": {"v": format!("{MARK} state 1")}, "version": 0}))
        .await;
    g.ok(json!({"op": "state_put", "key": "plan", "value": {"v": format!("{MARK} state 2")}, "version": 1}))
        .await;
    conv
}

async fn check(hub: &Hub, alice: &User, bob: &User, conv: &str) {
    let p = Conn::new(hub, alice).pull(conv, 0).await.to_string();
    for w in ["plain", "payload", "edited", "edit", "ref"] {
        assert!(p.contains(&format!("{MARK} {w}")), "{w}: {p}");
    }
    let side = Conn::new(hub, bob)
        .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
        .await
        .to_string();
    assert!(side.contains(&format!("{MARK} to bob")));
    let q = Conn::new(hub, alice)
        .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
        .await
        .to_string();
    for w in ["question", "context", "answer"] {
        assert!(q.contains(&format!("{MARK} {w}")), "{w}: {q}");
    }
    let bot = hub.user_by_name("bot").unwrap();
    let c = Conn::new(hub, &bot)
        .ok(json!({"op": "context", "conv_id": conv}))
        .await;
    assert_eq!(c["summary"]["text"], json!(format!("{MARK} summary")));
    let g = Conn::new(hub, &bot);
    for v in [1, 2] {
        let s = g
            .ok(json!({"op": "state_get", "key": "plan", "version": v}))
            .await;
        assert_eq!(s["value"]["v"], json!(format!("{MARK} state {v}")));
    }
    let audit = hub.audit_log(100).await.unwrap();
    assert!(
        format!("{audit:?}").contains(&format!("{MARK} draft")),
        "audit snapshot readable"
    );
}

async fn nothing_is_readable_at_rest(db: TestDb) {
    // A webhook that cannot be reached keeps its events in the outbox.
    let mut cfg = single(&db.url);
    cfg.webhooks = vec![WebhookConfig {
        name: "down".into(),
        url: "http://127.0.0.1:9/".into(),
        secret: "0123456789abcdef".into(),
        events: vec!["*".into()],
        max_attempts: 1000,
        timeout_ms: 100,
        backoff_ms: 60_000,
    }];
    let hub = open(cfg.clone()).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let conv = everything(&hub, &alice, &bob, &bot).await;
    assert!(count(&db, "SELECT COUNT(*) FROM webhook_outbox").await > 0);
    assert!(count(&db, "SELECT COUNT(*) FROM audit").await > 0);
    assert_eq!(find_everywhere(&db, MARK).await, Vec::<String>::new());
    // Everyone still reads everything, also after a restart.
    check(&hub, &alice, &bob, &conv).await;
    drop(hub);
    check(&open(cfg).await, &alice, &bob, &conv).await;
}

async fn the_master_key_is_checked(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let conv = Conn::new(&hub, &alice).group(&[&alice]).await;
    Conn::new(&hub, &alice).send(&conv, MARK).await;
    drop(hub);
    let other = agentim_core::crypto::Kek::generate_base64();
    let e = Hub::open(with_key(single(&db.url), Some(&other)), hub_cfg())
        .await
        .err()
        .unwrap();
    assert!(e.message.contains("different master key"), "{}", e.message);
    let mut off = single(&db.url);
    off.encryption.mode = EncryptionMode::Off;
    let e = Hub::open(off, hub_cfg()).await.err().unwrap();
    assert!(
        e.message.contains("holds encrypted content"),
        "{}",
        e.message
    );
    for bad in ["not base64!", "c2hvcnQ="] {
        let e = Hub::open(with_key(single(&db.url), Some(bad)), hub_cfg())
            .await
            .err()
            .unwrap();
        assert_eq!(e.code, "config", "{bad}");
    }
    let mut missing = with_key(single(&db.url), None);
    missing.encryption.key_file = Some("/nonexistent/agentim.key".into());
    assert!(
        Hub::open(missing, hub_cfg())
            .await
            .err()
            .unwrap()
            .message
            .contains("key_file")
    );
    // The right key still works.
    let hub = open(single(&db.url)).await;
    assert!(
        Conn::new(&hub, &alice)
            .pull(&conv, 0)
            .await
            .to_string()
            .contains(MARK)
    );
}

async fn tampered_or_moved_content_is_refused(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&alice]).await;
    let m1 = a.send(&conv, "first").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let m2 = a.send(&conv, "second").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let (mut c, d) = raw(&db).await;
    // Move message 1's ciphertext onto message 2.
    let v1: String = sqlx::query(d.q("SELECT content FROM messages WHERE msg_id = ?"))
        .bind(m1.clone())
        .fetch_one(&mut c)
        .await
        .unwrap()
        .get(0);
    assert!(v1.starts_with("enc:v1:k_"));
    sqlx::query(d.q("UPDATE messages SET content = ? WHERE msg_id = ?"))
        .bind(v1.clone())
        .bind(m2.clone())
        .execute(&mut c)
        .await
        .unwrap();
    let r = a
        .call(json!({"op": "edit", "msg_id": m2, "text": "x"}))
        .await;
    assert_eq!(
        r["error"]["code"], "storage",
        "a moved value is not served: {r}"
    );
    // Change one character of an event.
    let (seq, p): (i64, String) = {
        let r = sqlx::query(
            d.q("SELECT seq, payload FROM events WHERE conv_id = ? ORDER BY seq DESC LIMIT 1"),
        )
        .bind(conv.clone())
        .fetch_one(&mut c)
        .await
        .unwrap();
        (r.get(0), r.get(1))
    };
    let mut t = p.clone();
    let last = t.pop().unwrap();
    t.push(if last == 'A' { 'B' } else { 'A' });
    sqlx::query(d.q("UPDATE events SET payload = ? WHERE conv_id = ? AND seq = ?"))
        .bind(t)
        .bind(conv.clone())
        .bind(seq)
        .execute(&mut c)
        .await
        .unwrap();
    let r = a
        .call(json!({"op": "pull", "conv_id": conv, "after_seq": 0}))
        .await;
    assert_eq!(r["error"]["code"], "storage", "fail closed: {r}");
    // Nothing is returned as if it were content.
    assert!(!r.to_string().contains("enc:v1"));
}

async fn an_old_database_is_encrypted_in_place(db: TestDb) {
    let mut off = single(&db.url);
    off.encryption.mode = EncryptionMode::Off;
    let hub = open(off).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let conv = everything(&hub, &alice, &bob, &bot).await;
    drop(hub);
    let plain_hits = find_everywhere(&db, MARK).await;
    assert!(
        plain_hits.iter().any(|h| h == "messages.content"),
        "off stores plaintext: {plain_hits:?}"
    );
    // Turned on: old content still reads, new content is encrypted.
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert!(
        a.pull(&conv, 0)
            .await
            .to_string()
            .contains(&format!("{MARK} plain"))
    );
    a.send(&conv, &format!("{MARK} new")).await;
    assert!(
        find_everywhere(&db, &format!("{MARK} new"))
            .await
            .is_empty()
    );
    // Encrypting the rest.
    let v = hub.encrypt_existing().await.unwrap();
    assert!(v["main"].as_u64().unwrap() > 5, "{v}");
    assert_eq!(find_everywhere(&db, MARK).await, Vec::<String>::new());
    assert_eq!(
        hub.encrypt_existing().await.unwrap()["main"],
        0,
        "repeatable"
    );
    let p = a.pull(&conv, 0).await.to_string();
    for w in ["plain", "payload", "edited", "new"] {
        assert!(p.contains(&format!("{MARK} {w}")), "{w}");
    }
    drop(a);
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(
        Conn::new(&hub, &alice)
            .pull(&conv, 0)
            .await
            .to_string()
            .contains(&format!("{MARK} edit"))
    );
}

async fn rotating_the_master_key(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let conv = Conn::new(&hub, &alice).group(&[&alice]).await;
    Conn::new(&hub, &alice).send(&conv, MARK).await;
    let keys_before = count(&db, "SELECT COUNT(*) FROM data_keys").await;
    let content_before: String = {
        let (mut c, _) = raw(&db).await;
        sqlx::query("SELECT content FROM messages")
            .fetch_one(&mut c)
            .await
            .unwrap()
            .get(0)
    };
    let new = agentim_core::crypto::Kek::generate_base64();
    assert_eq!(
        hub.rotate_master_key("bad").await.unwrap_err().code,
        "bad_request"
    );
    let v = hub.rotate_master_key(&new).await.unwrap();
    assert_eq!(v["main"].as_i64().unwrap(), keys_before);
    drop(hub);
    // Content was not re-encrypted; only the keys were re-wrapped.
    let content_after: String = {
        let (mut c, _) = raw(&db).await;
        sqlx::query("SELECT content FROM messages")
            .fetch_one(&mut c)
            .await
            .unwrap()
            .get(0)
    };
    assert_eq!(content_before, content_after);
    let e = Hub::open(single(&db.url), hub_cfg()).await.err().unwrap();
    assert!(
        e.message.contains("different master key"),
        "the old key no longer opens it"
    );
    let hub = Hub::open(with_key(single(&db.url), Some(&new)), hub_cfg())
        .await
        .unwrap();
    assert!(
        Conn::new(&hub, &alice)
            .pull(&conv, 0)
            .await
            .to_string()
            .contains(MARK)
    );
}

async fn dissolving_shreds_the_keys(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let keep = a.group(&[&alice]).await;
    let gone = a.group(&[&alice]).await;
    a.send(&keep, "stays").await;
    a.send(&gone, MARK).await;
    assert_eq!(keys_of(&db, &gone).await, 1);
    a.ok(json!({"op": "dissolve", "conv_id": gone})).await;
    assert_eq!(
        keys_of(&db, &gone).await,
        0,
        "keys deleted with the content"
    );
    assert_eq!(keys_of(&db, &keep).await, 1);
    // The audit of the dissolve stays readable (database key).
    assert!(
        hub.audit_log(10)
            .await
            .unwrap()
            .iter()
            .any(|e| e["action"] == "conv.dissolved" || e["conv_id"] == gone.as_str())
    );
    assert!(a.pull(&keep, 0).await.to_string().contains("stays"));
}

async fn race_one_data_key_per_conversation(db: TestDb) {
    let hub = Arc::new(open(single(&db.url)).await);
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let mut convs = vec![];
    for _ in 0..10 {
        convs.push(Conn::new(&hub, &alice).group(&[&alice]).await);
    }
    let mut tasks = vec![];
    for c in &convs {
        for i in 0..5 {
            let (hub, alice, c) = (hub.clone(), alice.clone(), c.clone());
            tasks.push(tokio::spawn(async move {
                Conn::new(&hub, &alice)
                    .send(&c, &format!("{MARK}-{i}"))
                    .await
            }));
        }
    }
    for t in tasks {
        assert_eq!(t.await.unwrap()["ok"], true);
    }
    for c in &convs {
        let (mut con, d) = raw(&db).await;
        let n: i64 = sqlx::query(d.q("SELECT COUNT(*) FROM data_keys WHERE conv_id = ?"))
            .bind(c.clone())
            .fetch_one(&mut con)
            .await
            .unwrap()
            .get(0);
        assert_eq!(n, 1, "{c}");
        let p = Conn::new(&hub, &alice).pull(c, 0).await;
        let texts: Vec<Value> = p["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|e| e["event"]["msg"]["content"].as_str().map(|s| json!(s)))
            .collect();
        assert_eq!(texts.len(), 5);
    }
    assert!(find_everywhere(&db, MARK).await.is_empty());
}

// ---- SQLite only --------------------------------------------------------

#[tokio::test]
async fn a_single_sqlite_database_gets_a_generated_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("im.db");
    let cfg = Config::single_sqlite(&path.to_string_lossy());
    let key_file = format!("{}.key", path.to_string_lossy());
    let hub = Hub::open(cfg.clone(), hub_cfg()).await.unwrap();
    let alice = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap()
        .0;
    let conv = Conn::new(&hub, &alice).group(&[&alice]).await;
    Conn::new(&hub, &alice).send(&conv, MARK).await;
    drop(hub);
    let key = std::fs::read_to_string(&key_file).unwrap();
    assert!(agentim_core::crypto::Kek::from_base64(&key).is_ok());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "readable only by the owner");
    }
    let bytes = std::fs::read(&path).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains(MARK));
    // Reopened with the same file.
    let hub = Hub::open(cfg.clone(), hub_cfg()).await.unwrap();
    assert!(
        Conn::new(&hub, &alice)
            .pull(&conv, 0)
            .await
            .to_string()
            .contains(MARK)
    );
    drop(hub);
    // Losing the key file is refused, not silently replaced.
    std::fs::remove_file(&key_file).unwrap();
    let e = Hub::open(cfg, hub_cfg()).await.err().unwrap();
    assert!(e.message.contains("different master key"), "{}", e.message);
}

#[tokio::test]
async fn conversations_move_between_databases_with_their_keys() {
    let main = new_db(Backend::Sqlite).await.unwrap();
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let hub = open(two(&main.url, &secret.url, "main")).await;
    let carol = user_in(&hub, "carol", "b").await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[&carol]).await;
    c.send(&conv, MARK).await;
    drop(c);
    drop(hub);
    let hub = open(two(&main.url, &secret.url, "secret")).await;
    hub.migrate_unit("b").await.unwrap();
    let p = Conn::new(&hub, &carol).pull(&conv, 0).await;
    assert!(p.to_string().contains(MARK), "{p}");
    assert_eq!(
        count(
            &secret,
            &format!("SELECT COUNT(*) FROM data_keys WHERE conv_id = '{conv}'")
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &main,
            &format!("SELECT COUNT(*) FROM data_keys WHERE conv_id = '{conv}'")
        )
        .await,
        0
    );
    assert!(find_everywhere(&secret, MARK).await.is_empty());
}
