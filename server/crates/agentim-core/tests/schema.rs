//! Schema versions and automatic migration (docs/02-architecture.md §5.3):
//! new databases, upgrades from older versions with their data, restarts in
//! the middle of a migration, concurrent startups, and databases written by a
//! newer binary. Run against every backend.

mod common;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::storage::{self, Dialect, MIGRATIONS, SCHEMA_VERSION, Step, TABLES};
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    a_new_database_gets_the_latest_schema,
    an_old_database_is_migrated_with_its_data,
    a_migration_interrupted_after_its_ddl_is_finished_on_restart,
    concurrent_startups_migrate_once,
    a_newer_compatible_database_is_used_as_is,
    an_incompatible_or_corrupt_version_is_refused,
);

async fn raw(db: &TestDb) -> (sqlx::AnyConnection, Dialect) {
    sqlx::any::install_default_drivers();
    let d = Dialect::from_url(&db.url).unwrap();
    let url = db.url.replace("mariadb://", "mysql://");
    (sqlx::AnyConnection::connect(&url).await.unwrap(), d)
}

async fn meta(db: &TestDb, k: &str) -> Option<String> {
    let (mut c, d) = raw(db).await;
    sqlx::query(d.q("SELECT v FROM meta WHERE k = ?"))
        .bind(k.to_string())
        .fetch_optional(&mut c)
        .await
        .unwrap()
        .map(|r| sqlx::Row::get::<String, _>(&r, 0))
}

async fn set_meta(db: &TestDb, k: &str, v: Option<&str>) {
    let (mut c, d) = raw(db).await;
    sqlx::query(d.q("DELETE FROM meta WHERE k = ?"))
        .bind(k.to_string())
        .execute(&mut c)
        .await
        .unwrap();
    if let Some(v) = v {
        sqlx::query(d.q("INSERT INTO meta (k, v) VALUES (?, ?)"))
            .bind(k.to_string())
            .bind(v.to_string())
            .execute(&mut c)
            .await
            .unwrap();
    }
}

/// Turns a database into what `version` would have left behind: the steps
/// of later migrations undone and the version rows of that time.
async fn downgrade(db: &TestDb, version: i64) {
    let (mut c, _) = raw(db).await;
    for m in MIGRATIONS.iter().rev().filter(|m| m.version > version) {
        for step in m.steps {
            match step {
                Step::AddColumn { table, column } => {
                    sqlx::query(sqlx::AssertSqlSafe(format!(
                        "ALTER TABLE {table} DROP COLUMN {column}"
                    )))
                    .execute(&mut c)
                    .await
                    .unwrap();
                }
                Step::HashLegacyTokens => {}
            }
        }
    }
    drop(c);
    set_meta(db, "schema_version", Some(&version.to_string())).await;
    // Version 1 databases predate compat tracking; later ones record the
    // compat of the migrations they had.
    let compat = MIGRATIONS
        .iter()
        .filter(|m| m.version <= version)
        .map(|m| m.compat)
        .max();
    set_meta(
        db,
        "schema_compat",
        compat.map(|c| c.to_string()).as_deref(),
    )
    .await;
}

/// Every table has exactly the columns `TABLES` declares.
async fn assert_latest_columns(db: &TestDb) {
    let (mut c, d) = raw(db).await;
    for t in TABLES {
        let mut want: Vec<String> = t.cols.iter().map(|c| c.name.to_string()).collect();
        want.sort();
        let have = storage::columns(&mut c, d, t.name).await.unwrap();
        assert_eq!(have, want, "table {}", t.name);
    }
    assert_eq!(
        meta(db, "schema_version").await.unwrap(),
        SCHEMA_VERSION.to_string()
    );
}

async fn a_new_database_gets_the_latest_schema(db: TestDb) {
    let hub = open(single(&db.url)).await;
    drop(hub);
    assert_latest_columns(&db).await;
    assert_eq!(
        meta(&db, "schema_compat").await.unwrap(),
        storage::current_compat().to_string()
    );
    // Restarting changes nothing.
    let hub = open(single(&db.url)).await;
    drop(hub);
    assert_latest_columns(&db).await;
}

async fn content(hub: &Hub, u: &User, conv: &str) -> Vec<Value> {
    let c = Conn::new(hub, u);
    c.pull(conv, 0).await["events"].as_array().unwrap().clone()
}

async fn an_old_database_is_migrated_with_its_data(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let first = a.send(&conv, "before the upgrade").await;
    a.ok(
        json!({"op": "edit", "conv_id": conv, "msg_id": first["msg_id"], "text": "edited before"}),
    )
    .await;
    a.send(&conv, "second").await;
    let mut before = content(&hub, &alice, &conv).await;
    drop(a);
    drop(hub);

    for from in 1..SCHEMA_VERSION {
        downgrade(&db, from).await;
        if from == 1 {
            // Version 1 kept plaintext tokens in users.token.
            let (mut c, d) = raw(&db).await;
            sqlx::query(d.q("UPDATE users SET token = ? WHERE id = ?"))
                .bind("legacy-plain-alice")
                .bind(alice.id.clone())
                .execute(&mut c)
                .await
                .unwrap();
            sqlx::query(d.q("DELETE FROM tokens WHERE user_id = ?"))
                .bind(alice.id.clone())
                .execute(&mut c)
                .await
                .unwrap();
        }
        let hub = open(single(&db.url)).await;
        if from == 1 {
            assert_eq!(
                hub.authenticate("legacy-plain-alice").unwrap().id,
                alice.id,
                "still logs in"
            );
            let (mut c, d) = raw(&db).await;
            let r = sqlx::query(d.q("SELECT token FROM users WHERE id = ?"))
                .bind(alice.id.clone())
                .fetch_one(&mut c)
                .await
                .unwrap();
            let t: String = sqlx::Row::get(&r, 0);
            assert_eq!(
                t,
                agentim_core::auth::legacy_placeholder(&alice.id),
                "plaintext gone"
            );
        }
        assert_latest_columns(&db).await;
        assert_eq!(
            meta(&db, "schema_compat").await.unwrap(),
            storage::current_compat().to_string(),
            "from {from}"
        );
        // Everything written before the upgrade reads back the same, for
        // both members, and the conversation keeps working.
        assert_eq!(content(&hub, &alice, &conv).await, before, "from {from}");
        // Bob sees the same; each sees only their own client_msg_id (M17).
        let strip = |mut v: Vec<Value>| {
            for e in &mut v {
                if let Some(m) = e["event"]["msg"].as_object_mut() {
                    m.remove("client_msg_id");
                }
            }
            v
        };
        assert_eq!(
            strip(content(&hub, &bob, &conv).await),
            strip(before.clone()),
            "from {from}"
        );
        let b = Conn::new(&hub, &bob);
        let r = b.send(&conv, "after the upgrade").await;
        assert_eq!(r["ok"], true, "{r}");
        let now = seqs(&b.pull(&conv, 0).await);
        assert_eq!(now.len(), before.len() + 1);
        assert!(now.windows(2).all(|w| w[1] == w[0] + 1), "{now:?}");
        before = content(&hub, &alice, &conv).await;
        drop(b);
        drop(hub);
    }
}

async fn a_migration_interrupted_after_its_ddl_is_finished_on_restart(db: TestDb) {
    // MySQL commits DDL on its own: a crash can leave a migration's columns
    // in place with the version row still old.
    drop(open(single(&db.url)).await);
    downgrade(&db, 1).await;
    let (mut c, d) = raw(&db).await;
    for m in MIGRATIONS {
        for step in m.steps {
            let Step::AddColumn { table, column } = step else {
                continue;
            };
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "ALTER TABLE {table} ADD COLUMN {}",
                storage::column_ddl(d, table, column)
            )))
            .execute(&mut c)
            .await
            .unwrap();
        }
    }
    drop(c);
    let hub = open(single(&db.url)).await;
    drop(hub);
    assert_latest_columns(&db).await;
}

async fn concurrent_startups_migrate_once(db: TestDb) {
    for round in 0..3 {
        drop(open(single(&db.url)).await);
        downgrade(&db, 1).await;
        let opens = (0..4).map(|_| {
            let url = db.url.clone();
            tokio::spawn(async move { Hub::open(single(&url), hub_cfg()).await })
        });
        let hubs: Vec<_> = futures_util::future::join_all(opens).await;
        for h in &hubs {
            if let Err(e) = h.as_ref().unwrap() {
                panic!("round {round}: {}: {}", e.code, e.message);
            }
        }
        drop(hubs);
        assert_latest_columns(&db).await;
    }
}

async fn a_newer_compatible_database_is_used_as_is(db: TestDb) {
    // Rolling upgrade: a node not yet upgraded opens a database a newer node
    // already migrated with additive changes only.
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let conv = Conn::new(&hub, &alice).group(&[&alice]).await;
    drop(hub);
    let newer = (SCHEMA_VERSION + 1).to_string();
    set_meta(&db, "schema_version", Some(&newer)).await;
    set_meta(&db, "schema_compat", Some(&SCHEMA_VERSION.to_string())).await;
    let hub = open(single(&db.url)).await;
    let a = Conn::new(&hub, &alice);
    assert_eq!(a.send(&conv, "still works").await["ok"], true);
    drop(a);
    drop(hub);
    assert_eq!(
        meta(&db, "schema_version").await.unwrap(),
        newer,
        "never lowered"
    );
    assert_eq!(
        meta(&db, "schema_compat").await.unwrap(),
        SCHEMA_VERSION.to_string()
    );
}

async fn an_incompatible_or_corrupt_version_is_refused(db: TestDb) {
    drop(open(single(&db.url)).await);
    let newer = (SCHEMA_VERSION + 1).to_string();
    let cases: &[(Option<&str>, Option<&str>, &str)] = &[
        // A breaking change made by a newer binary.
        (
            Some(&newer),
            Some(&newer),
            "needs a binary of schema version",
        ),
        // A newer database from before compat tracking is treated as breaking.
        (Some("999"), None, "needs a binary of schema version 999"),
        (Some("abc"), None, "is not a schema version"),
        (Some("0"), None, "is not a schema version"),
        (Some("-3"), None, "is not a schema version"),
        (Some("1"), Some("x"), "is not a schema version"),
    ];
    for (version, compat, want) in cases {
        set_meta(&db, "schema_version", *version).await;
        set_meta(&db, "schema_compat", *compat).await;
        let err = Hub::open(single(&db.url), hub_cfg())
            .await
            .err()
            .unwrap_or_else(|| panic!("{version:?}/{compat:?} must be refused"));
        assert_eq!(err.code, "schema_mismatch", "{}", err.message);
        assert!(
            err.message.contains(want),
            "{} should mention {want}",
            err.message
        );
        // The refusal changes nothing.
        assert_eq!(meta(&db, "schema_version").await.as_deref(), *version);
        assert_eq!(meta(&db, "schema_compat").await.as_deref(), *compat);
    }
    // Repaired by hand, the database opens again.
    set_meta(&db, "schema_version", Some(&SCHEMA_VERSION.to_string())).await;
    set_meta(
        &db,
        "schema_compat",
        Some(&storage::current_compat().to_string()),
    )
    .await;
    drop(open(single(&db.url)).await);
}

/// The metadata stripper exists twice (client core, server); they must not drift.
#[test]
fn media_code_is_the_same_in_the_client_core() {
    assert_eq!(
        include_str!("../src/media.rs"),
        include_str!("../../../../sdk/client-rust/core/src/media.rs"),
        "copy sdk/client-rust/core/src/media.rs to server/crates/agentim-core/src/media.rs"
    );
}
