//! A person's assistant and team agents (docs/03 §9.3.1): which agents
//! leave or stop with their owner, the owner's powers only where they are
//! a member, conversation managers configuring team agents, and transfers.

mod common;

use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::{Value, json};

backend_tests!(
    team_agents_are_created_by_administrators_only,
    team_agents_stay_when_their_owner_leaves,
    team_agents_keep_working_when_their_owner_is_deactivated,
    owners_act_on_an_agent_only_where_they_are_members,
    managers_configure_team_agents_in_their_conversation,
    an_agent_needing_approval_cannot_speak_without_its_owner,
    side_entries_skip_an_absent_owner,
    transferring_a_team_agent,
    transfer_errors,
    race_transfers_of_one_agent,
);

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn team(hub: &Hub, owner: &User, name: &str) -> (User, String) {
    let v = hub.create_agent_with(owner, name, true).await.unwrap();
    let agent: User = serde_json::from_value(v["agent"].clone()).unwrap();
    (agent, v["key"].as_str().unwrap().to_string())
}

async fn head(c: &Conn, conv: &str) -> i64 {
    seqs(&c.pull(conv, 0).await).last().copied().unwrap_or(0)
}

fn member_ids(listed: &Value, conv: &str) -> Vec<(String, Value)> {
    listed["convs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["conv_id"] == conv)
        .unwrap()["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["user_id"].as_str().unwrap().to_string(),
                m["owner_id"].clone(),
            )
        })
        .collect()
}

async fn team_agents_are_created_by_administrators_only(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let (bot, _) = team(&hub, &alice, "deploy-bot").await;
    assert!(bot.team);
    // The client operation creates a person's assistant.
    let a = Conn::new(&hub, &alice);
    let v = a.ok(json!({"op": "create_agent", "name": "helper"})).await;
    assert!(v["agent"]["team"].is_null(), "{v}");
    let listed = a.ok(json!({"op": "list_agents"})).await.to_string();
    assert!(listed.contains("\"team\":true"), "{listed}");
    // Only agents with an owner are team agents.
    let e = hub
        .create_user_with("x", UserKind::Human, None, None, true)
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_request");
    let e = hub
        .create_user_with("y", UserKind::Agent, None, None, true)
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_request");
    // It survives a restart.
    drop(a);
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(hub.user(&bot.id).unwrap().team);
    assert!(!hub.user_by_name("helper").unwrap().team);
}

async fn team_agents_stay_when_their_owner_leaves(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let pb = user(&hub, "bob-assistant", UserKind::Agent, Some(&bob)).await;
    let (tc, _) = team(&hub, &carol, "faq-bot").await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bob, &carol, &tb, &pb, &tc]).await;
    // bob takes over his team agent, then is removed: the takeover ends,
    // his assistant leaves, the team agent stays.
    let b = Conn::new(&hub, &bob);
    b.ok(json!({"op": "takeover", "conv_id": conv, "agent_id": tb.id}))
        .await;
    let r = a
        .ok(json!({"op": "remove_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    assert_eq!(r["agents"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!(r["agents"][0]["user_id"], json!(pb.id));
    let g = Conn::new(&hub, &tb);
    g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "still on duty"}))
        .await;
    assert_eq!(
        code(&Conn::new(&hub, &pb).send(&conv, "x").await),
        "forbidden"
    );
    // carol leaves herself: same.
    Conn::new(&hub, &carol)
        .ok(json!({"op": "leave", "conv_id": conv}))
        .await;
    let ids: Vec<String> = member_ids(&a.ok(json!({"op": "list_convs"})).await, &conv)
        .into_iter()
        .map(|(i, _)| i)
        .collect();
    assert!(ids.contains(&tb.id) && ids.contains(&tc.id), "{ids:?}");
    assert!(!ids.contains(&pb.id) && !ids.contains(&bob.id) && !ids.contains(&carol.id));
    // The takeover ended before bob left.
    let p = a.pull(&conv, 0).await;
    let kinds: Vec<String> = p["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(
            |e| match (e["event"]["type"].as_str().unwrap(), &e["event"]["data"]) {
                ("agent.state", d) => format!("state:{}", d["state"].as_str().unwrap()),
                ("member.removed", d) => format!("removed:{}", d["name"].as_str().unwrap()),
                (t, _) => t.to_string(),
            },
        )
        .filter(|k| k.starts_with("state:") || k.starts_with("removed:"))
        .collect();
    assert_eq!(
        kinds,
        [
            "state:taken_over",
            "state:active",
            "removed:bob",
            "removed:bob-assistant"
        ]
    );
}

async fn team_agents_keep_working_when_their_owner_is_deactivated(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let pb = user(&hub, "bob-assistant", UserKind::Agent, Some(&bob)).await;
    let conv = Conn::new(&hub, &alice).group(&[&bob, &tb, &pb]).await;
    hub.set_deactivated(&bob.id, true).await.unwrap();
    assert!(!hub.is_stopped(&tb.id));
    assert!(hub.is_stopped(&pb.id));
    Conn::new(&hub, &tb)
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "on duty"}))
        .await;
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert!(!hub.is_stopped(&tb.id) && hub.is_stopped(&pb.id));
}

/// The owner's powers in a conversation (observe, focus, whisper, takeover,
/// policy, guard, approval) need the owner to be a member; outside, the
/// agent's own management still works.
async fn owners_act_on_an_agent_only_where_they_are_members(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let aria = user(&hub, "aria", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    // A team agent bob no longer sits with, and his assistant in a chat he
    // was never in.
    let conv = a.group(&[&bob, &tb]).await;
    b.ok(json!({"op": "leave", "conv_id": conv})).await;
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "a", "members": [aria.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    for (c, agent) in [(&conv, &tb.id), (&dm, &aria.id)] {
        let before = head(&a, c).await;
        for req in [
            json!({"op": "pull", "conv_id": c, "as_agent": agent}),
            json!({"op": "focus", "conv_id": c, "as_agent": agent}),
            json!({"op": "whisper", "conv_id": c, "agent_id": agent, "text": "psst"}),
            json!({"op": "takeover", "conv_id": c, "agent_id": agent}),
            json!({"op": "release", "conv_id": c, "agent_id": agent}),
            json!({"op": "set_agent_policy", "conv_id": c, "agent_id": agent, "trigger": "all"}),
            json!({"op": "reset_guard", "conv_id": c, "agent_id": agent}),
        ] {
            assert_eq!(code(&b.call(req.clone()).await), "forbidden", "{req}");
        }
        assert_eq!(head(&a, c).await, before, "refusals leave no trace");
        let listed = b.ok(json!({"op": "list_convs"})).await.to_string();
        assert!(!listed.contains(c.as_str()), "{listed}");
    }
    // Outside any conversation he still manages them.
    b.ok(json!({"op": "stop_agent", "agent_id": tb.id})).await;
    b.ok(json!({"op": "resume_agent", "agent_id": tb.id})).await;
    // Back in the conversation, the powers return.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id}))
        .await;
    b.ok(json!({"op": "pull", "conv_id": conv, "as_agent": tb.id}))
        .await;
    b.ok(json!({"op": "whisper", "conv_id": conv, "agent_id": tb.id, "text": "psst"}))
        .await;
    b.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": tb.id, "trigger": "all"}))
        .await;
}

async fn managers_configure_team_agents_in_their_conversation(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let carol = user(&hub, "carol", UserKind::Human, None).await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let aria = user(&hub, "aria", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(&hub, &alice);
    let c = Conn::new(&hub, &carol);
    let conv = a.group(&[&carol, &tb, &aria]).await;
    let policy = |agent: &str| json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": agent, "trigger": "all"});
    let reset = |agent: &str| json!({"op": "reset_guard", "conv_id": conv, "agent_id": agent});
    // alice owns the conversation: she configures the team agent...
    a.ok(policy(&tb.id)).await;
    assert_eq!(
        code(&a.call(reset(&tb.id)).await),
        "conflict",
        "allowed; the guard is not tripped"
    );
    // ...but not someone's assistant, and plain members configure neither.
    assert_eq!(code(&a.call(policy(&aria.id)).await), "forbidden");
    assert_eq!(code(&c.call(policy(&tb.id)).await), "forbidden");
    assert_eq!(code(&c.call(reset(&tb.id)).await), "forbidden");
    // An admin of the conversation is a manager too.
    a.ok(json!({"op": "set_role", "conv_id": conv, "user_id": carol.id, "role": "admin"}))
        .await;
    c.ok(policy(&tb.id)).await;
    // Not an agent of this conversation.
    let other = a.group(&[&carol]).await;
    let r = a
        .call(json!({"op": "set_agent_policy", "conv_id": other, "agent_id": tb.id, "trigger": "all"}))
        .await;
    assert_eq!(code(&r), "forbidden", "{r}");
}

async fn an_agent_needing_approval_cannot_speak_without_its_owner(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob, &tb]).await;
    b.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": tb.id, "approval": true}))
        .await;
    let g = Conn::new(&hub, &tb);
    let pending = g
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "draft"}))
        .await["draft_id"]
        .clone();
    b.ok(json!({"op": "leave", "conv_id": conv})).await;
    // Nobody here can approve: refused, with nothing left behind.
    let before = head(&a, &conv).await;
    let r = g
        .call(json!({"op": "send", "conv_id": conv, "client_msg_id": "2", "text": "another"}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    let r = g
        .call(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "3"}))
        .await;
    assert_eq!(code(&r), "conflict", "{r}");
    assert_eq!(head(&a, &conv).await, before);
    // The earlier draft waits; its owner cannot decide it from outside.
    assert_eq!(
        code(&b.call(json!({"op": "approve", "draft_id": pending})).await),
        "forbidden"
    );
    assert_eq!(
        code(&b.call(json!({"op": "reject", "draft_id": pending})).await),
        "forbidden"
    );
    // The conversation's owner turns approval off; the agent speaks.
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": tb.id, "approval": false}))
        .await;
    g.ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "4", "text": "live"}))
        .await;
}

async fn side_entries_skip_an_absent_owner(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let conv = a.group(&[&bob, &tb]).await;
    let g = Conn::new(&hub, &tb);
    let m = g
        .ok(json!({"op": "send", "conv_id": conv, "client_msg_id": "1", "text": "answer"}))
        .await["msg_id"]
        .clone();
    a.ok(json!({"op": "feedback", "msg_id": m, "rating": "up", "reason": "while bob is here"}))
        .await;
    b.ok(json!({"op": "leave", "conv_id": conv})).await;
    a.ok(json!({"op": "feedback", "msg_id": m, "rating": "down", "reason": "while bob is away"}))
        .await;
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bob.id, "history": 200}))
        .await;
    let side = b
        .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
        .await
        .to_string();
    assert!(side.contains("while bob is here"), "{side}");
    assert!(!side.contains("while bob is away"), "{side}");
    // The agent got both.
    let side = g
        .ok(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
        .await
        .to_string();
    assert!(side.contains("while bob is away"));
    // Away, he cannot read the side channel at all.
    b.ok(json!({"op": "leave", "conv_id": conv})).await;
    let r = b
        .call(json!({"op": "pull_side", "conv_id": conv, "after_side_seq": 0}))
        .await;
    assert_eq!(code(&r), "forbidden");
}

async fn transferring_a_team_agent(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let (tb, key) = team(&hub, &bob, "release-bot").await;
    let conv = Conn::new(&hub, &alice).group(&[&bob, &dave, &tb]).await;
    hub.set_deactivated(&bob.id, true).await.unwrap();
    // Without rotating, the running agent keeps its key.
    let r = hub.transfer_agent(&tb.id, &dave.id, false).await.unwrap();
    assert_eq!(
        (r["from"].clone(), r["to"].clone()),
        (json!(bob.id), json!(dave.id))
    );
    assert!(hub.authenticate_device(&key).is_some());
    assert_eq!(
        hub.user(&tb.id).unwrap().owner_id.as_deref(),
        Some(dave.id.as_str())
    );
    let d = Conn::new(&hub, &dave);
    d.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": tb.id, "trigger": "all"}))
        .await;
    let listed = d.ok(json!({"op": "list_agents"})).await.to_string();
    assert!(listed.contains(&tb.id));
    let owners = member_ids(&d.ok(json!({"op": "list_convs"})).await, &conv);
    assert!(
        owners.contains(&(tb.id.clone(), json!(dave.id))),
        "{owners:?}"
    );
    // Back to bob (reactivated), rotating keys: the old key stops working.
    hub.set_deactivated(&bob.id, false).await.unwrap();
    let r = hub.transfer_agent(&tb.id, &bob.id, true).await.unwrap();
    assert_eq!(r["keys_revoked"], true);
    assert!(hub.authenticate_device(&key).is_none());
    assert_eq!(
        code(&d.call(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": tb.id, "trigger": "all"})).await),
        "forbidden"
    );
    // Restart: owner and member rows agree; the transfer is audited.
    drop(d);
    drop(hub);
    let hub = open(single(&db.url)).await;
    assert_eq!(
        hub.user(&tb.id).unwrap().owner_id.as_deref(),
        Some(bob.id.as_str())
    );
    let owners = member_ids(
        &Conn::new(&hub, &dave).ok(json!({"op": "list_convs"})).await,
        &conv,
    );
    assert!(
        owners.contains(&(tb.id.clone(), json!(bob.id))),
        "{owners:?}"
    );
    assert!(
        hub.stats().await.unwrap()["audit"]
            .to_string()
            .contains("agent.transferred")
    );
}

async fn transfer_errors(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.agents.max_per_owner = 1;
    cfg.units = vec![agentim_core::config::UnitConfig {
        name: "b".into(),
        database: "main".into(),
    }];
    let hub = open(cfg).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let dave = user(&hub, "dave", UserKind::Human, None).await;
    let erin = user(&hub, "erin", UserKind::Human, None).await;
    let far = user_in(&hub, "far", "b").await;
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let aria = user(&hub, "aria", UserKind::Agent, Some(&dave)).await;
    let e = |r: agentim_core::model::ApiResult<Value>| r.unwrap_err().code;
    assert_eq!(
        e(hub.transfer_agent(&aria.id, &bob.id, false).await),
        "bad_request",
        "an assistant"
    );
    assert_eq!(
        e(hub.transfer_agent("u_nobody", &dave.id, false).await),
        "not_found"
    );
    assert_eq!(
        e(hub.transfer_agent(&bob.id, &dave.id, false).await),
        "not_found",
        "a person"
    );
    assert_eq!(
        e(hub.transfer_agent(&tb.id, "u_nobody", false).await),
        "not_found"
    );
    assert_eq!(
        e(hub.transfer_agent(&tb.id, &aria.id, false).await),
        "not_found",
        "to an agent"
    );
    assert_eq!(
        e(hub.transfer_agent(&tb.id, &far.id, false).await),
        "bad_request",
        "another unit"
    );
    assert_eq!(
        e(hub.transfer_agent(&tb.id, &bob.id, false).await),
        "conflict",
        "already theirs"
    );
    assert_eq!(
        e(hub.transfer_agent(&tb.id, &dave.id, false).await),
        "conflict",
        "dave is at the cap"
    );
    hub.set_deactivated(&erin.id, true).await.unwrap();
    assert_eq!(
        e(hub.transfer_agent(&tb.id, &erin.id, false).await),
        "not_found",
        "deactivated"
    );
    assert_eq!(
        hub.user(&tb.id).unwrap().owner_id.as_deref(),
        Some(bob.id.as_str())
    );
    // Destroyed: gone.
    hub.destroy_agent(&bob, &tb.id, &tb.name).await.unwrap();
    hub.set_deactivated(&erin.id, false).await.unwrap();
    assert_eq!(
        e(hub.transfer_agent(&tb.id, &erin.id, false).await),
        "not_found"
    );
}

/// Concurrent transfers of one agent: whichever wins, the owner in memory,
/// the owner row and every member row (in every database) agree; two
/// agents racing for the last place under the cap: exactly one gets it.
async fn race_transfers_of_one_agent(db: TestDb) {
    let Some(secret) = new_db(Backend::Sqlite).await else {
        return;
    };
    let mut cfg = two(&db.url, &secret.url, "secret");
    cfg.agents.max_per_owner = 1;
    let hub = open(cfg.clone()).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let mut people = vec![];
    for i in 0..4 {
        people.push(user(&hub, &format!("p{i}"), UserKind::Human, None).await);
    }
    let (tb, _) = team(&hub, &bob, "release-bot").await;
    let carl = user(&hub, "carl", UserKind::Human, None).await;
    let (tc, _) = team(&hub, &carl, "faq-bot").await;
    let refs: Vec<&User> = people.iter().chain([&tb, &tc]).collect();
    let a = Conn::new(&hub, &bob);
    // One conversation in each database.
    let main_conv = a.group(&refs).await;
    let b_owner = user_in(&hub, "b-owner", "b").await;
    let other = Conn::new(&hub, &b_owner).group(&refs).await;
    let rows_owner = |listed: Value, conv: &str| {
        member_ids(&listed, conv)
            .into_iter()
            .find(|(id, _)| *id == tb.id)
            .unwrap()
            .1
    };
    for round in 0..40 {
        let tasks: Vec<_> = (0..4)
            .map(|i| {
                let (hub, agent, to) = (hub.clone(), tb.id.clone(), people[i].id.clone());
                tokio::spawn(async move { hub.transfer_agent(&agent, &to, false).await })
            })
            .collect();
        let mut won = 0;
        for t in tasks {
            won += t.await.unwrap().is_ok() as u32;
        }
        assert!(won >= 1, "round {round}");
        let owner = json!(hub.user(&tb.id).unwrap().owner_id.unwrap());
        assert_eq!(
            rows_owner(a.ok(json!({"op": "list_convs"})).await, &main_conv),
            owner,
            "round {round}"
        );
        let listed = Conn::new(&hub, &b_owner)
            .ok(json!({"op": "list_convs"}))
            .await;
        assert_eq!(rows_owner(listed, &other), owner, "round {round}");
        // Back to bob for the next round.
        hub.transfer_agent(&tb.id, &bob.id, false).await.unwrap();
    }
    // Memory and database agree after a restart.
    let before = hub.user(&tb.id).unwrap().owner_id;
    drop(a);
    drop(hub);
    let hub = open(cfg).await;
    assert_eq!(hub.user(&tb.id).unwrap().owner_id, before);
    // Both agents to the same free person: the cap lets one in.
    let free = &people[0];
    let (r1, r2) = tokio::join!(
        hub.transfer_agent(&tb.id, &free.id, false),
        hub.transfer_agent(&tc.id, &free.id, false)
    );
    assert_eq!(r1.is_ok() as u8 + r2.is_ok() as u8, 1);
    assert_eq!(hub.shared().agents_of(&free.id).len(), 1);
}
