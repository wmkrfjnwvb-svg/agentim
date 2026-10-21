//! Problems other IM servers had, checked against ours
//! (docs/research/im-issues.md): each test reproduces one of them and
//! fails without its fix.

mod common;

use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};
use sqlx::Connection;

backend_tests!(
    deactivating_an_owner_stops_their_agents,
    deactivated_people_cannot_be_added,
    an_own_agent_sees_no_more_than_its_adder,
    a_persons_agents_leave_with_them,
    unread_counts_start_at_what_a_member_can_see,
    edits_of_older_messages_stay_hidden_from_new_members,
    a_recalled_message_leaves_no_draft_copy,
    a_failed_index_write_is_repaired,
    race_remove_and_add_back_keep_the_index_true,
    a_retry_gets_its_original_result_whatever_changed,
    targeted_messages_and_drafts_are_deduplicated,
    search_and_export_see_the_latest_text,
    text_that_looks_encrypted_is_kept_as_text,
    revocations_by_another_process_take_effect,
    a_persons_limits_count_all_their_connections,
    groups_have_a_size_limit,
    retention_takes_a_prefix_and_what_derives_from_it,
    edits_recompute_mentions,
    edits_can_require_a_version,
    writing_counts_as_reading,
    idle_and_old_tokens_expire,
    a_lock_held_elsewhere_fails_the_batch_not_the_database,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn last_seq(c: &Conn, conv: &str) -> i64 {
    let p = c.pull(conv, 0).await;
    seqs(&p).last().copied().unwrap_or(0)
}

/// H1: the agents' keys are in the deactivated person's hands.
async fn deactivating_an_owner_stops_their_agents(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let other = user(&hub, "other", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let g = Conn::new(&hub, &bot);
    g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "hi"}))
        .await;
    hub.set_deactivated(&alice.id, true).await.unwrap();
    assert!(hub.is_stopped(&bot.id), "the owner's agent is stopped");
    assert!(!hub.is_stopped(&other.id), "someone else's agent is not");
    let g = Conn::new(&hub, &bot);
    let r = g
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "2", "text": "still here"}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    // Deactivating again, and reactivating, leave it stopped: the owner
    // decides when it runs again.
    hub.set_deactivated(&alice.id, true).await.unwrap();
    hub.set_deactivated(&alice.id, false).await.unwrap();
    assert!(hub.is_stopped(&bot.id));
    // It survives a restart.
    drop((a, g));
    let hub = open(single(&db.url)).await;
    assert!(hub.is_stopped(&bot.id));
}

async fn deactivated_people_cannot_be_added(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    hub.set_deactivated(&carol.id, true).await.unwrap();
    let head = last_seq(&a, &conv).await;
    let r = a
        .call(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    assert_eq!(code(&r), "not_found", "{r}");
    assert_eq!(last_seq(&a, &conv).await, head, "a refusal leaves no trace");
    hub.set_deactivated(&carol.id, false).await.unwrap();
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id}))
        .await;
}

/// H6: a plain member who sees nothing from before they joined cannot read
/// it through an agent they add with history.
async fn an_own_agent_sees_no_more_than_its_adder(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let alicebot = user(&hub, "alicebot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&carol]).await;
    for i in 0..5 {
        a.send(&conv, &format!("before bob {i}")).await;
    }
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id, "history": 0}))
        .await;
    let joined = last_seq(&a, &conv).await;
    let b = Conn::new(&hub, &bob);
    b.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bobbot.id, "history": 200}))
        .await;
    let g = Conn::new(&hub, &bobbot);
    let h = g.ok(json!({"op": "history", "conv_id": conv})).await;
    let early: Vec<&Value> = h["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["event"]["seq"].as_i64().unwrap() < joined)
        .collect();
    assert!(early.is_empty(), "{early:?}");
    // The owner (a manager) can still share history with their agent.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": alicebot.id, "history": 200}))
        .await;
    let h = Conn::new(&hub, &alicebot)
        .ok(json!({"op": "history", "conv_id": conv}))
        .await;
    assert!(
        h["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["event"]["seq"].as_i64().unwrap() < joined)
    );
}

/// M3: a new member's unread and mention counts start where they can see.
async fn unread_counts_start_at_what_a_member_can_see(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    for i in 0..5 {
        a.send(&conv, &format!("old {i}")).await;
    }
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": dave.id, "history": 2}))
        .await;
    let c = Conn::new(&hub, &carol);
    let u = c.ok(json!({"op": "unread"})).await;
    assert_eq!(
        (u["total"].clone(), u["mentions"].clone()),
        (json!(0), json!(0)),
        "{u}"
    );
    let d = Conn::new(&hub, &dave);
    let u = d.ok(json!({"op": "unread"})).await;
    assert!(u["total"].as_i64().unwrap() <= 2, "{u}");
    // New messages count as usual.
    a.send(&conv, "new").await;
    assert_eq!(c.ok(json!({"op": "unread"})).await["total"], 1);
    // Removed, mentioned while away, and added back without history.
    a.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": carol.id}))
        .await;
    a.send(&conv, "@carol while you were away").await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": carol.id, "history": 0}))
        .await;
    let u = c.ok(json!({"op": "unread"})).await;
    assert_eq!(
        (u["total"].clone(), u["mentions"].clone()),
        (json!(0), json!(0)),
        "{u}"
    );
}

/// H6: removed or gone, a person can no longer read or speak through their
/// agents in that conversation.
async fn a_persons_agents_leave_with_them(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bobbot = user(&hub, "bobbot", UserKind::Agent, Some(&bob)).await;
    let carolbot = user(&hub, "carolbot", UserKind::Agent, Some(&carol)).await;
    let alicebot = user(&hub, "alicebot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a
        .group(&[&bob, &carol, &bobbot, &carolbot, &alicebot])
        .await;
    let r = a
        .ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    assert_eq!(r["agents"].as_array().unwrap().len(), 1, "{r}");
    Conn::new(&hub, &carol)
        .ok(json!({"op": "leave", "conv_id": conv}))
        .await;
    let members: Vec<String> = a.ok(json!({"op": "list_convs"})).await["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["conv_id"] == conv)
        .unwrap()["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["user_id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        !members.contains(&bobbot.id) && !members.contains(&carolbot.id),
        "{members:?}"
    );
    assert!(members.contains(&alicebot.id), "other people's agents stay");
    for bot in [&bobbot, &carolbot] {
        let r = Conn::new(&hub, bot)
            .call(json!({"op": "history", "conv_id": conv}))
            .await;
        assert_eq!(code(&r), "forbidden", "{r}");
    }
    // The log says why the agents left.
    let p = a.pull(&conv, 0).await;
    let reasons = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| {
            e["event"]["type"] == "member.removed" && e["event"]["data"]["reason"] == "owner_left"
        })
        .count();
    assert_eq!(reasons, 2);
}

/// H3: editing or recalling a message from before someone joined (without
/// history) tells them nothing: not live, not in sync, not in reads, and
/// not to an agent that hears everything.
async fn edits_of_older_messages_stay_hidden_from_new_members(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (carol, token) = hub
        .create_user("carol", UserKind::Human, None, None)
        .await
        .unwrap();
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob]).await;
    let old = a.send(&conv, "secret plan").await["msg_id"].clone();
    let gone = a.send(&conv, "another secret").await["msg_id"].clone();
    for u in [&carol, &bot] {
        a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": u.id, "history": 0}))
            .await;
    }
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    let mut c = Conn::new(&hub, &carol);
    let mut g = Conn::new(&hub, &bot);
    c.drain();
    g.drain();
    let new = a.send(&conv, "after carol").await["msg_id"].clone();
    a.ok(json!({"op": "edit", "msg_id": old, "text": "secret plan v2"}))
        .await;
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    a.ok(json!({"op": "edit", "msg_id": new, "text": "after carol v2"}))
        .await;
    let ids = |evs: Vec<Value>| -> Vec<String> {
        evs.iter()
            .filter(|f| f["type"] == "event")
            .filter_map(|f| f["event"]["msg"]["msg_id"].as_str().map(String::from))
            .collect()
    };
    let want = vec![new.as_str().unwrap().to_string(); 2];
    assert_eq!(ids(c.drain()), want, "live, person");
    assert_eq!(ids(g.drain()), want, "live, agent with trigger all");
    // A device syncs, the agent syncs, reads go through the server.
    let _ = token;
    let device = hub
        .authenticate_client(&client_token(&hub, &carol).await, Some("c1"), None, None)
        .await
        .unwrap()
        .device_id;
    for (who, conn) in [
        ("sync, person", Conn::device(&hub, &carol, &device)),
        ("sync, agent", Conn::new(&hub, &bot)),
    ] {
        let s = conn.ok(json!({"op": "sync"})).await;
        let evs: Vec<Value> = s["convs"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["events"].as_array().unwrap().clone())
            .collect();
        assert!(ids(evs).iter().all(|i| *i == new), "{who}: {s}");
    }
    for conn in [Conn::new(&hub, &carol), Conn::new(&hub, &bot)] {
        let p = conn.pull(&conv, 0).await;
        let evs = p["events"].as_array().unwrap().clone();
        assert!(ids(evs).iter().all(|i| *i == new), "{p}");
    }
    let h = Conn::new(&hub, &bot)
        .ok(json!({"op": "history", "conv_id": conv}))
        .await;
    assert!(!h.to_string().contains("secret"), "{h}");
    // Members who could see the message still get the edit.
    let p = Conn::new(&hub, &bob).pull(&conv, 0).await;
    assert!(p.to_string().contains("secret plan v2"));
}

/// H4: recalling an approved agent message also empties its draft, in the
/// side channel the owner and the agent sync from.
async fn a_recalled_message_leaves_no_draft_copy(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let g = Conn::new(&hub, &bot);
    let mut drafts = vec![];
    for text in ["recall me", "keep me"] {
        let d = g
            .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": text, "text": text}))
            .await["draft_id"]
            .clone();
        a.ok(json!({"op": "approve", "draft_id": d})).await;
        drafts.push(d);
    }
    let p = a.pull(&conv, 0).await;
    let id = |t: &str| {
        p["events"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["event"]["msg"]["content"] == t)
            .unwrap()["event"]["msg"]["msg_id"]
            .clone()
    };
    let gone = id("recall me");
    // Recalled by its owner, and by the agent itself, are both covered.
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    for c in [&a, &g] {
        let side = c
            .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
            .await
            .to_string();
        assert!(!side.contains("recall me"), "{side}");
        assert!(side.contains("keep me"), "other drafts stay: {side}");
    }
    let s = Conn::new(&hub, &bot)
        .ok(json!({"op": "sync"}))
        .await
        .to_string();
    assert!(!s.contains("recall me") && s.contains("keep me"), "{s}");
    // Recalling again is a conflict and changes nothing.
    assert_eq!(
        code(&a.call(json!({"op": "recall", "msg_id": gone})).await),
        "conflict"
    );
}

/// Rows of `sql` (one bound user id and conversation) in a database.
async fn rows(db: &TestDb, sql: &str, conv: &str, user: &str) -> i64 {
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    let q = if db.url.starts_with("postgres") {
        sql.replacen('?', "$1", 1).replacen('?', "$2", 1)
    } else {
        sql.to_string()
    };
    sqlx::Row::get(
        &sqlx::query(sqlx::AssertSqlSafe(q))
            .bind(conv.to_string())
            .bind(user.to_string())
            .fetch_one(&mut c)
            .await
            .unwrap(),
        0,
    )
}

/// What the control database says about one member: (indexed, departed).
async fn control_view(db: &TestDb, conv: &str, user: &str) -> (i64, i64) {
    (
        rows(
            db,
            "SELECT COUNT(*) FROM member_index WHERE conv_id = ? AND user_id = ?",
            conv,
            user,
        )
        .await,
        rows(
            db,
            "SELECT COUNT(*) FROM departures WHERE conv_id = ? AND user_id = ?",
            conv,
            user,
        )
        .await,
    )
}

fn lists(v: &Value, conv: &str) -> bool {
    v["convs"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["conv_id"] == conv)
}

/// H5: a member change committed in a unit's database stands when the
/// control database then fails; nobody removed keeps seeing the
/// conversation, and the index is repaired (in the background or at the
/// next start).
async fn a_failed_index_write_is_repaired(db: TestDb) {
    let Some(secret) = new_db(Backend::Sqlite).await else {
        return;
    };
    let cfg = two(&db.url, &secret.url, "secret");
    let hub = open(cfg.clone()).await;
    let carol = user_in(&hub, "carol", "b").await;
    let dave = user_in(&hub, "dave", "b").await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[&dave]).await;
    let d = Conn::new(&hub, &dave);
    assert!(lists(&d.ok(json!({"op": "list_convs"})).await, &conv));
    // Removed: the control write fails, the removal is still reported done.
    hub.inject_job_failures("main", 1);
    c.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": dave.id}))
        .await;
    assert_eq!(control_view(&db, &conv, &dave.id).await, (1, 0), "stale");
    assert!(!lists(&d.ok(json!({"op": "list_convs"})).await, &conv));
    assert_eq!(code(&d.send(&conv, "still here?").await), "forbidden");
    hub.repair_indexes().await;
    assert_eq!(control_view(&db, &conv, &dave.id).await, (0, 1), "repaired");
    // Added back, same failure: repaired, and visible again.
    hub.inject_job_failures("main", 1);
    c.ok(json!({"op": "add_member", "conv_id": conv, "user_id": dave.id}))
        .await;
    assert_eq!(control_view(&db, &conv, &dave.id).await, (0, 1), "stale");
    hub.repair_indexes().await;
    assert_eq!(control_view(&db, &conv, &dave.id).await, (1, 0));
    assert!(lists(&d.ok(json!({"op": "list_convs"})).await, &conv));
    // Repairing again changes nothing.
    hub.repair_indexes().await;
    assert_eq!(control_view(&db, &conv, &dave.id).await, (1, 0));
    // The control database fails for good (a failed commit stops its
    // writes until a restart): the next start repairs.
    hub.inject_commit_failures("main", 1);
    d.ok(json!({"op": "leave", "conv_id": conv})).await;
    drop((c, d));
    drop(hub);
    let hub = open(cfg).await;
    assert_eq!(control_view(&db, &conv, &dave.id).await, (0, 1));
    assert_eq!(control_view(&db, &conv, &carol.id).await, (1, 0));
    let _ = hub;
}

/// H5: removing someone and adding them back at the same time leaves the
/// index agreeing with the membership, whichever wins.
async fn race_remove_and_add_back_keep_the_index_true(db: TestDb) {
    let Some(secret) = new_db(Backend::Sqlite).await else {
        return;
    };
    let hub = open(two(&db.url, &secret.url, "secret")).await;
    let carol = user_in(&hub, "carol", "b").await;
    let dave = user_in(&hub, "dave", "b").await;
    let c = Conn::new(&hub, &carol);
    let conv = c.group(&[&dave]).await;
    let d = Conn::new(&hub, &dave);
    for round in 0..30 {
        let remove = json!({"op": "remove_member", "conv_id": conv, "user_id": dave.id});
        let add = json!({"op": "add_member", "conv_id": conv, "user_id": dave.id});
        let (r1, r2) = if round % 2 == 0 {
            tokio::join!(c.call(remove), c.call(add))
        } else {
            let (a, r) = tokio::join!(c.call(add), c.call(remove));
            (r, a)
        };
        let _ = (r1, r2);
        let member = c.ok(json!({"op": "list_convs"})).await["convs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["conv_id"] == conv)
            .unwrap()["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["user_id"] == json!(dave.id));
        let want = if member { (1, 0) } else { (0, 1) };
        assert_eq!(
            control_view(&db, &conv, &dave.id).await,
            want,
            "round {round}"
        );
        assert_eq!(
            lists(&d.ok(json!({"op": "list_convs"})).await, &conv),
            member
        );
        // Start every round from "member".
        if !member {
            c.ok(json!({"op": "add_member", "conv_id": conv, "user_id": dave.id}))
                .await;
        }
    }
}

/// M1: a retry of a committed message returns its result, even after the
/// sender was removed, and does not count again against guards.
async fn a_retry_gets_its_original_result_whatever_changed(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&bob)).await;
    let b = Conn::new(&hub, &bob);
    let conv = b.group(&[&alice, &bot]).await;
    let a = Conn::new(&hub, &alice);
    let msg = json!({"op": "send", "conv_id": conv, "client_msg_id": "m1", "text": "hello"});
    let first = a.ok(msg.clone()).await;
    b.ok(json!({"op": "remove_member", "conv_id": conv, "user_id": alice.id}))
        .await;
    let head = last_seq(&b, &conv).await;
    let again = a.ok(msg).await;
    assert_eq!(
        (again["seq"].clone(), again["msg_id"].clone()),
        (first["seq"].clone(), first["msg_id"].clone())
    );
    assert_eq!(again["duplicate"], true);
    assert_eq!(last_seq(&b, &conv).await, head, "nothing new");
    // A new message is refused as before.
    assert_eq!(code(&a.send(&conv, "new").await), "forbidden");
    // An agent's retries do not trip the repeat guard (three equal texts).
    let g = Conn::new(&hub, &bot);
    let m = json!({"op": "send", "conv_id": conv, "client_msg_id": "g1", "text": "same"});
    let first = g.ok(m.clone()).await;
    for _ in 0..5 {
        assert_eq!(g.ok(m.clone()).await["msg_id"], first["msg_id"]);
    }
    g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "g2", "text": "other"}))
        .await;
    // An emergency stop is different: a stopped agent gets nothing back.
    b.ok(json!({"op": "stop_agent", "agent_id": bot.id})).await;
    assert_eq!(code(&Conn::new(&hub, &bot).call(m).await), "conflict");
}

/// M2: retries of a message to one person and of a message waiting for
/// approval do not deliver or queue it twice.
async fn targeted_messages_and_drafts_are_deduplicated(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let to_bob = json!({"op": "send", "conv_id": conv, "client_msg_id": "w1", "text": "just you", "to_user": bob.id});
    let first = a.ok(to_bob.clone()).await;
    let again = a.ok(to_bob).await;
    assert_eq!(again["side_seq"], first["side_seq"]);
    assert_eq!(again["duplicate"], true);
    let side = Conn::new(&hub, &bob)
        .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
        .await;
    let n = side["side_events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["data"]["text"] == "just you")
        .count();
    assert_eq!(n, 1, "{side}");
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let g = Conn::new(&hub, &bot);
    let d = json!({"op": "send", "conv_id": conv, "client_msg_id": "d1", "text": "draft"});
    let first = g.ok(d.clone()).await;
    assert_eq!(g.ok(d).await["draft_id"], first["draft_id"]);
    let s = json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s1"});
    let first_s = g.ok(s.clone()).await;
    assert_eq!(g.ok(s).await["msg_id"], first_s["msg_id"]);
    let drafts = a.ok(json!({"op": "list_drafts"})).await;
    assert_eq!(drafts["drafts"].as_array().unwrap().len(), 1, "{drafts}");
}

/// M4: search and export find edited text and completed streams, not the
/// old or recalled text.
async fn search_and_export_see_the_latest_text(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "trigger": "all"}))
        .await;
    let m = a.send(&conv, "first draft wording").await["msg_id"].clone();
    a.ok(json!({"op": "edit", "msg_id": m, "text": "final wording"}))
        .await;
    let gone = a.send(&conv, "recalled wording").await["msg_id"].clone();
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    let g = Conn::new(&hub, &bot);
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s"}))
        .await["msg_id"]
        .clone();
    g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "streamed wording"}))
        .await;
    g.ok(json!({"op": "stream_end", "msg_id": s})).await;
    let find = |q: &str| json!({"op": "search", "query": q, "conv_id": conv});
    let hits = |v: Value| v["hits"].as_array().unwrap().len();
    assert_eq!(hits(g.ok(find("final")).await), 1);
    assert_eq!(
        hits(g.ok(find("first draft")).await),
        0,
        "the old text is gone"
    );
    assert_eq!(hits(g.ok(find("recalled")).await), 0);
    let streamed = g.ok(find("streamed")).await;
    assert_eq!(hits(streamed.clone()), 1);
    assert_eq!(streamed["hits"][0]["msg_id"], s);
    // One hit per message, at the message's own seq.
    assert_eq!(hits(g.ok(find("wording")).await), 2);
    let pkg = hub.export_agent(&alice, &bot.id, true).await.unwrap();
    let text = pkg["conversations"][0]["events"].to_string();
    assert!(
        text.contains("final wording") && text.contains("streamed wording"),
        "{text}"
    );
    assert!(
        !text.contains("first draft") && !text.contains("recalled"),
        "{text}"
    );
}

/// M11: with encryption off, text shaped like ciphertext is just text.
async fn text_that_looks_encrypted_is_kept_as_text(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.encryption.mode = agentim_core::config::EncryptionMode::Off;
    let hub = open(cfg.clone()).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    for t in ["enc:v1:k_x:AAAA", "enc:raw:literally", "enc:"] {
        a.send(&conv, t).await;
    }
    let p = a.pull(&conv, 0).await;
    let texts: Vec<&str> = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["event"]["msg"]["content"].as_str())
        .collect();
    assert_eq!(texts, ["enc:v1:k_x:AAAA", "enc:raw:literally", "enc:"]);
    Conn::new(&hub, &bot)
        .ok(json!({"op": "history", "conv_id": conv}))
        .await;
    // Encrypting later keeps the text as written.
    drop(a);
    drop(hub);
    cfg.encryption = test_encryption();
    let hub = open(cfg).await;
    hub.encrypt_existing().await.unwrap();
    let p = Conn::new(&hub, &alice).pull(&conv, 0).await.to_string();
    assert!(
        p.contains("enc:v1:k_x:AAAA") && p.contains("enc:raw:literally"),
        "{p}"
    );
}

/// M10: a logout, a revoked agent key or a deactivation done by another
/// process on the same database (the CLI while the server runs) takes
/// effect in the running server.
async fn revocations_by_another_process_take_effect(db: TestDb) {
    let server = open(single(&db.url)).await;
    let alice = user(&server, "alice", UserKind::Human, None).await;
    let carol = user(&server, "carol", UserKind::Human, None).await;
    let (bot, key) = server
        .create_user("bot", UserKind::Agent, Some(&alice.id), None)
        .await
        .unwrap();
    let t = client_token(&server, &alice).await;
    let la = server
        .authenticate_client(&t, Some("a1"), None, None)
        .await
        .unwrap();
    let lb = server
        .authenticate_client(&key.unwrap(), None, None, None)
        .await
        .unwrap();
    server.refresh_auth().await.unwrap();
    assert!(!server.refresh_auth().await.unwrap(), "nothing changed");
    let cli = open(single(&db.url)).await;
    cli.log_out(&alice.id, None).await.unwrap();
    cli.revoke_tokens(&bot.id, None).await.unwrap();
    cli.set_deactivated(&carol.id, true).await.unwrap();
    // Still trusted until the server notices (at most a second later).
    assert!(server.login_valid(&la));
    assert!(server.refresh_auth().await.unwrap());
    assert!(!server.login_valid(&la), "logged out everywhere");
    assert!(!server.login_valid(&lb), "the agent's key revoked");
    assert!(
        server
            .authenticate_client(&t, Some("a1"), None, None)
            .await
            .is_err(),
        "her token is cut off on this server too"
    );
    assert!(server.is_deactivated(&carol.id), "deactivated");
    // Reactivation is picked up too.
    cli.set_deactivated(&carol.id, false).await.unwrap();
    server.refresh_auth().await.unwrap();
    assert!(!server.is_deactivated(&carol.id));
}

/// M12 / D95: a person's sends and syncs are limited per person, however
/// many connections they open; refusals leave nothing in the log.
async fn a_persons_limits_count_all_their_connections(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.clients.send_rate = 0.5;
    cfg.clients.send_burst = 3.0;
    let mut h = hub_cfg();
    h.sync.rate = (0.5, 2.0);
    let hub = agentim_core::hub::Hub::open(cfg, h).await.unwrap();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let conns: Vec<Conn> = (0..3).map(|_| Conn::new(&hub, &alice)).collect();
    let conv = conns[0].group(&[&bob, &bot]).await;
    for (i, c) in conns.iter().enumerate() {
        c.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("{i}"), "text": "hi"}))
            .await;
    }
    let head = last_seq(&conns[0], &conv).await;
    let r = conns[1].send(&conv, "one too many").await;
    assert_eq!(code(&r), "rate_limited", "{r}");
    assert_eq!(last_seq(&conns[0], &conv).await, head, "no trace");
    // Other people, and agents (they have their own limits), are not affected.
    Conn::new(&hub, &bob)
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "b", "text": "hi"}))
        .await;
    Conn::new(&hub, &bot)
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "g", "text": "hi"}))
        .await;
    // Sync: two rounds, then refused on any connection.
    conns[0].ok(json!({"op": "sync"})).await;
    conns[1].ok(json!({"op": "sync"})).await;
    assert_eq!(
        code(&conns[2].call(json!({"op": "sync"})).await),
        "rate_limited"
    );
}

/// D95: groups have at most `clients.max_group_members` members.
async fn groups_have_a_size_limit(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.clients.max_group_members = 3;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let r = a
        .call(json!({"op": "create_conv", "kind": "group", "title": "g", "members": [bob.id, carol.id, dave.id]}))
        .await;
    assert_eq!(code(&r), "bad_request", "{r}");
    let listed = a.ok(json!({"op": "list_convs"})).await;
    assert!(
        listed["convs"].as_array().unwrap().is_empty(),
        "nothing created"
    );
    let conv = a.group(&[&bob, &carol]).await;
    let head = last_seq(&a, &conv).await;
    let r = a
        .call(json!({"op": "add_member", "conv_id": conv, "user_id": dave.id}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    assert_eq!(last_seq(&a, &conv).await, head, "no trace");
    // Re-adding someone already in is the usual answer, not "full".
    let r = a
        .call(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    assert_ne!(
        r["error"]["message"].as_str().unwrap_or(""),
        "the group is full (3 members)"
    );
    // A place frees up when someone leaves.
    Conn::new(&hub, &carol)
        .ok(json!({"op": "leave", "conv_id": conv}))
        .await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": dave.id}))
        .await;
}

/// Runs one statement with a conversation id bound to its `?`.
async fn exec(db: &TestDb, sql: &str, conv: &str) {
    sqlx::any::install_default_drivers();
    let mut c = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
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

/// M15: retention deletes only a prefix of a log (a clock set back cannot
/// cut a hole), and with it what derives from the deleted part.
async fn retention_takes_a_prefix_and_what_derives_from_it(db: TestDb) {
    let mut h = hub_cfg();
    h.gc_every = std::time::Duration::ZERO;
    h.sync.retention = Some(std::time::Duration::from_secs(3600));
    let hub = agentim_core::hub::Hub::open(single(&db.url), h)
        .await
        .unwrap();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    // One conversation whose clock jumped back in the middle.
    let conv = a.group(&[]).await;
    for i in 0..6 {
        a.send(&conv, &format!("m{i}")).await;
    }
    exec(
        &db,
        "UPDATE events SET created_at = 1 WHERE conv_id = ? AND seq = 5",
        &conv,
    )
    .await;
    hub.tick().await;
    assert_eq!(seqs(&a.pull(&conv, 0).await).len(), 8, "no hole");
    // One old conversation, with feedback and drafts.
    let old = a.group(&[&bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": old, "agent_id": bot.id, "approval": true}))
        .await;
    let g = Conn::new(&hub, &bot);
    let d1 = g
        .ok(json!({"op": "send", "conv_id": old, "client_msg_id": "1", "text": "one"}))
        .await["draft_id"]
        .clone();
    g.ok(json!({"op": "send", "conv_id": old, "client_msg_id": "2", "text": "two"}))
        .await;
    a.ok(json!({"op": "approve", "draft_id": d1})).await;
    let m = a.pull(&old, 0).await["events"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|e| e["event"]["type"] == "msg.created")
        .unwrap()["event"]["msg"]["msg_id"]
        .clone();
    a.ok(json!({"op": "feedback", "msg_id": m, "rating": "up"}))
        .await;
    for t in ["events", "messages", "side_events", "feedback", "drafts"] {
        exec(
            &db,
            &format!("UPDATE {t} SET created_at = 1 WHERE conv_id = ?"),
            &old,
        )
        .await;
    }
    hub.tick().await;
    assert!(
        seqs(&a.pull(&old, 0).await).is_empty(),
        "the old log is gone"
    );
    let n = |t: &'static str| {
        let (db, old) = (&db, old.clone());
        async move {
            rows(
                db,
                &format!("SELECT COUNT(*) FROM {t} WHERE conv_id = ? AND conv_id = ?"),
                &old,
                &old,
            )
            .await
        }
    };
    assert_eq!(n("messages").await, 0);
    assert_eq!(n("feedback").await, 0);
    assert_eq!(n("drafts").await, 1, "the pending draft stays");
    // New messages continue the log.
    a.send(&old, "new").await;
    assert_eq!(seqs(&a.pull(&old, 0).await).len(), 1);
}

/// D87: an edit recomputes mentions: new ones notify (and wake agents),
/// removed ones stop counting as unread mentions.
async fn edits_recompute_mentions(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &carol, &bot]).await;
    let m = a.send(&conv, "hi @bob").await["msg_id"].clone();
    let b = Conn::new(&hub, &bob);
    assert_eq!(b.ok(json!({"op": "unread"})).await["mentions"], 1);
    let mut c = Conn::new(&hub, &carol);
    let mut g = Conn::new(&hub, &bot);
    c.drain();
    g.drain();
    a.ok(json!({"op": "edit", "msg_id": m, "text": "hi @carol and @bot"}))
        .await;
    assert_eq!(
        b.ok(json!({"op": "unread"})).await["mentions"],
        0,
        "no longer mentioned"
    );
    assert_eq!(c.ok(json!({"op": "unread"})).await["mentions"], 1);
    let cf = c.drain();
    assert!(
        cf.iter()
            .any(|f| f["event"]["type"] == "msg.updated" && f["notify"] == true),
        "{cf:?}"
    );
    let gf = g.drain();
    assert!(
        gf.iter()
            .any(|f| f["event"]["type"] == "msg.updated" && f["wake"] == true),
        "{gf:?}"
    );
    // Editing again without new mentions notifies nobody.
    c.drain();
    a.ok(json!({"op": "edit", "msg_id": m, "text": "hi @carol and @bot!"}))
        .await;
    assert!(c.drain().iter().all(|f| f["notify"] != true));
}

/// D88: an edit based on an older version is refused, leaving no trace.
async fn edits_can_require_a_version(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[]).await;
    let m = a.send(&conv, "v1").await["msg_id"].clone();
    a.ok(json!({"op": "edit", "msg_id": m, "text": "v2", "base_version": 1}))
        .await;
    let head = last_seq(&a, &conv).await;
    let r = a
        .call(json!({"op": "edit", "msg_id": m, "text": "stale", "base_version": 1}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    assert_eq!(last_seq(&a, &conv).await, head, "no trace");
    a.ok(json!({"op": "edit", "msg_id": m, "text": "v3", "base_version": 2}))
        .await;
    // Without base_version, the last edit wins as before.
    a.ok(json!({"op": "edit", "msg_id": m, "text": "v4"})).await;
}

/// D93: a person's own message moves their read position.
async fn writing_counts_as_reading(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &bot]).await;
    let b = Conn::new(&hub, &bob);
    for i in 0..3 {
        a.send(&conv, &format!("{i}")).await;
    }
    assert_eq!(b.ok(json!({"op": "unread"})).await["total"], 3);
    let mut other = Conn::new(&hub, &bob);
    other.drain();
    b.send(&conv, "my answer").await;
    assert_eq!(b.ok(json!({"op": "unread"})).await["total"], 0);
    assert!(
        other.drain().iter().any(|f| f["type"] == "read"),
        "other devices learn it"
    );
    // An agent's message does not move anyone's position.
    Conn::new(&hub, &bot)
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "x", "text": "agent"}))
        .await;
    assert_eq!(
        a.ok(json!({"op": "unread"})).await["total"],
        2,
        "bob's answer and the agent's"
    );
}

/// D94: people's devices unused too long are forgotten; agent keys do
/// not expire.
async fn idle_and_old_tokens_expire(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.auth.token_idle_days = 30;
    let hub = open(cfg).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let t = client_token(&hub, &alice).await;
    for d in ["idle", "fresh"] {
        hub.authenticate_client(&t, Some(d), Some(d), None)
            .await
            .unwrap();
    }
    let (bot, key) = hub
        .create_user("bot", UserKind::Agent, Some(&alice.id), None)
        .await
        .unwrap();
    let day = 86_400_000i64;
    let now = agentim_core::model::now_ms();
    let set = |sql: String| {
        let db = &db;
        async move { exec(db, &sql, "unused").await }
    };
    set(format!(
        "UPDATE tokens SET created_at = {}, last_seen = {} WHERE device_name = 'idle' AND ? <> ''",
        now - 31 * day,
        now - 31 * day
    ))
    .await;
    set(format!(
        "UPDATE tokens SET created_at = {}, last_seen = {} WHERE user_id = '{}' AND ? <> ''",
        now - 400 * day,
        now - 400 * day,
        bot.id
    ))
    .await;
    assert_eq!(hub.expire_tokens().await.unwrap(), 1);
    let names: Vec<_> = hub
        .devices(&alice.id)
        .await
        .unwrap()
        .iter()
        .filter_map(|d| d.device_name.clone())
        .collect();
    assert_eq!(names, ["fresh"]);
    assert!(
        hub.authenticate_device(&key.unwrap()).is_some(),
        "agent keys do not expire"
    );
    assert_eq!(hub.expire_tokens().await.unwrap(), 0, "idempotent");
}

/// E1 / E2: a lock held by another process makes the writer give up after
/// its lock timeout with a retryable error; the database keeps working and
/// nothing of the batch is kept.
async fn a_lock_held_elsewhere_fails_the_batch_not_the_database(db: TestDb) {
    if db.url.starts_with("sqlite") {
        return; // one writer process; covered by the busy timeout
    }
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[]).await;
    let head = last_seq(&a, &conv).await;
    sqlx::any::install_default_drivers();
    let mut other = sqlx::AnyConnection::connect(&db.url.replace("mariadb://", "mysql://"))
        .await
        .unwrap();
    let q = if db.url.starts_with("postgres") {
        "SELECT id FROM conversations WHERE id = $1 FOR UPDATE"
    } else {
        "SELECT id FROM conversations WHERE id = ? FOR UPDATE"
    };
    let mut tx = sqlx::Connection::begin(&mut other).await.unwrap();
    sqlx::query(sqlx::AssertSqlSafe(q.to_string()))
        .bind(conv.clone())
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let t = std::time::Instant::now();
    let r = a.send(&conv, "blocked").await;
    assert_eq!(code(&r), "unavailable", "{r}");
    assert!(
        t.elapsed() < std::time::Duration::from_secs(8),
        "gave up in time"
    );
    tx.rollback().await.unwrap();
    // The database is fine and the retry goes through once.
    let msg = json!({"op": "send", "conv_id": conv, "client_msg_id": "x", "text": "after"});
    let first = a.ok(msg.clone()).await;
    assert_eq!(a.ok(msg).await["msg_id"], first["msg_id"]);
    assert_eq!(
        last_seq(&a, &conv).await,
        head + 1,
        "the blocked batch left nothing"
    );
}
