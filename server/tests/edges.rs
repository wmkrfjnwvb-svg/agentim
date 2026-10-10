//! Paths that code coverage showed no test reached: input limits, blocking
//! outside direct conversations, refused user creation, a failed
//! conversation creation, and a policy hit inside a streamed draft.

mod common;

use agentim::model::*;
use common::*;
use serde_json::json;

backend_tests!(
    conversation_input_limits,
    blocking_only_closes_direct_conversations,
    refused_user_creation_and_tokens,
    a_failed_conversation_creation_leaves_no_trace,
    a_policy_hit_inside_a_streamed_draft_discards_it,
);

fn code(v: &serde_json::Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

async fn conversation_input_limits(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let create =
        |t: String| json!({"op": "create_conv", "kind": "group", "title": t, "members": []});
    for bad in [String::new(), "x".repeat(256), "a\0b".into()] {
        assert_eq!(
            code(&a.call(create(bad.clone())).await),
            "bad_request",
            "{bad:?}"
        );
    }
    let n = a.ok(json!({"op": "list_convs"})).await["convs"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(n, 0, "refused creations leave nothing");
    let conv = a.ok(create("x".repeat(255))).await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    for bad in [String::new(), "x".repeat(256), "a\0b".into()] {
        let r = a
            .call(json!({"op": "update_conv", "conv_id": conv, "title": bad}))
            .await;
        assert_eq!(code(&r), "bad_request");
    }
    // A stream chunk with NUL is refused and leaves the stream as it was.
    a.ok(json!({"op": "add_member", "conv_id": conv, "user_id": bot.id}))
        .await;
    let g = Conn::new(&hub, &bot);
    let s = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s"}))
        .await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 0, "text": "fine"}))
        .await;
    let r = g
        .call(json!({"op": "stream_append", "msg_id": s, "idx": 1, "text": "x\0y"}))
        .await;
    assert_eq!(code(&r), "bad_request");
    g.ok(json!({"op": "stream_append", "msg_id": s, "idx": 1, "text": " still"}))
        .await;
    g.ok(json!({"op": "stream_end", "msg_id": s})).await;
    let m = msg_by_id(&a.pull(&conv, 0).await, &s).unwrap();
    assert_eq!(m["content"], "fine still");
    assert_eq!(m["state"], "complete");
}

async fn blocking_only_closes_direct_conversations(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let pair = a.group(&[&bob]).await;
    a.ok(json!({"op": "block", "user_id": bob.id})).await;
    // The one who blocked is told to unblock first; the blocked one is refused.
    let r = a
        .call(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await;
    assert_eq!(code(&r), "blocked");
    assert!(r["error"]["message"].as_str().unwrap().contains("unblock"));
    assert_eq!(code(&a.send(&dm, "hi").await), "blocked");
    assert_eq!(code(&b.send(&dm, "hi").await), "blocked");
    // A group, even of the same two people, stays open.
    let before = a.pull(&pair, 0).await["upto_seq"].as_i64().unwrap();
    assert_eq!(a.send(&pair, "from alice").await["ok"], true);
    assert_eq!(b.send(&pair, "from bob").await["ok"], true);
    assert_eq!(
        a.pull(&pair, 0).await["upto_seq"].as_i64().unwrap(),
        before + 2
    );
    a.ok(json!({"op": "unblock", "user_id": bob.id})).await;
    assert_eq!(b.send(&dm, "again").await["ok"], true);
}

async fn refused_user_creation_and_tokens(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let e = hub
        .create_user("x", UserKind::Human, None, Some("nope"))
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_request");
    let e = hub
        .create_user("_x", UserKind::Human, None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_request", "names starting with _ are reserved");
    hub.create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap();
    let e = hub
        .create_user("alice", UserKind::Human, None, None)
        .await
        .unwrap_err();
    assert_eq!(e.code, "conflict");
    assert!(hub.user_by_name("x").is_none() && hub.user_by_name("_x").is_none());
    let sys = hub.system_user();
    let e = hub
        .issue_token(&sys.id, None, None, None, "test")
        .await
        .unwrap_err();
    assert_eq!(e.code, "bad_request", "the system user does not log in");
    let e = hub
        .issue_token("u_unknown", None, None, None, "test")
        .await
        .unwrap_err();
    assert_eq!(e.code, "not_found");
}

async fn a_failed_conversation_creation_leaves_no_trace(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    hub.inject_job_failures("main", 1);
    let r = a
        .call(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await;
    assert_eq!(code(&r), "storage");
    assert_eq!(a.ok(json!({"op": "list_convs"})).await["convs"], json!([]));
    assert_eq!(
        Conn::new(&hub, &bob).ok(json!({"op": "list_convs"})).await["convs"],
        json!([])
    );
    // The direct conversation is still unique once created.
    let c1 = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await;
    let c2 = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await;
    assert_eq!(c1["conv_id"], c2["conv_id"]);
    assert_eq!(
        a.ok(json!({"op": "list_convs"})).await["convs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

async fn a_policy_hit_inside_a_streamed_draft_discards_it(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let b = Conn::new(&hub, &bob);
    let g = Conn::new(&hub, &bot);
    let conv = a.group(&[&bob, &bot]).await;
    a.ok(json!({"op": "set_agent_policy", "conv_id": conv, "agent_id": bot.id, "approval": true}))
        .await;
    let q = b.send(&conv, "@bot answer").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let head = a.pull(&conv, 0).await["upto_seq"].as_i64().unwrap();
    let r = g
        .ok(json!({"op": "stream_start", "conv_id": conv, "client_msg_id": "s", "reply_to": q}))
        .await;
    assert_eq!(r["pending_approval"], true);
    let d = r["msg_id"].as_str().unwrap().to_string();
    g.ok(json!({"op": "stream_append", "msg_id": d, "idx": 0, "text": "card 4111 1111 "}))
        .await;
    let r = g
        .call(json!({"op": "stream_append", "msg_id": d, "idx": 1, "text": "1111 1111"}))
        .await;
    assert_eq!(code(&r), "policy_blocked");
    // The owner learns the draft was discarded; nobody else sees anything.
    let side = a
        .ok(json!({"op": "pull_side", "conv_id": conv}))
        .await
        .to_string();
    assert!(
        side.contains("draft.discarded") && side.contains("policy"),
        "{side}"
    );
    assert!(!side.contains("4111 1111 1111"));
    let bside = b
        .ok(json!({"op": "pull_side", "conv_id": conv}))
        .await
        .to_string();
    assert!(!bside.contains(&d) && !bside.contains("4111"));
    // The main log is unchanged and the draft cannot be approved or continued.
    assert_eq!(a.pull(&conv, 0).await["upto_seq"].as_i64().unwrap(), head);
    assert_ne!(
        a.call(json!({"op": "approve", "draft_id": d})).await["ok"],
        true
    );
    assert_ne!(
        g.call(json!({"op": "stream_append", "msg_id": d, "idx": 2, "text": "more"}))
            .await["ok"],
        true
    );
    assert_eq!(a.pull(&conv, 0).await["upto_seq"].as_i64().unwrap(), head);
}
