//! Compliance basics (docs/04-security-compliance.md §4.2): account deletion
//! (what is erased, ownership passing on, agents destroyed, ids not reused,
//! restart finishing the work, racing sends), exporting one's own data
//! (both ways in, cursor, limits), reports (rules, error codes, refusals
//! leave no trace, duplicates, limits) and the moderation queue (order,
//! decisions, effects, suspensions ending, racing moderators).

mod common;

use std::net::SocketAddr;

use agentim_core::config::Config;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    deleting_an_account_erases_what_the_person_wrote,
    deletion_refusals_and_repeats,
    an_interrupted_deletion_finishes_after_a_restart,
    race_sends_while_the_account_is_deleted,
    export_has_the_persons_own_data,
    reports_follow_the_rules,
    the_queue_and_decisions,
    race_moderators_decide_once,
);

struct Api {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    key: String,
}

impl Api {
    async fn start(db: &TestDb) -> Api {
        Api::with(single(&db.url)).await
    }

    async fn with(cfg: Config) -> Api {
        let hub = Hub::open(cfg, hub_cfg()).await.unwrap();
        let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = tcp.local_addr().unwrap().to_string();
        let app = router(AppState {
            hub: hub.clone(),
            demo_login: false,
            metrics_token: None,
        });
        tokio::spawn(async move {
            axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
                .await
                .unwrap()
        });
        let scopes: Vec<String> = agentim_core::open_api::SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (key, _) = hub
            .create_api_key("test", &scopes, None, None)
            .await
            .unwrap();
        Api {
            base: format!("http://{addr}"),
            hub,
            http: reqwest::Client::new(),
            key,
        }
    }

    async fn raw(
        &self,
        method: &str,
        path: &str,
        bearer: Option<&str>,
        body: Option<Value>,
    ) -> (u16, String) {
        let mut r = self
            .http
            .request(method.parse().unwrap(), format!("{}{path}", self.base));
        if let Some(k) = bearer {
            r = r.header("authorization", format!("Bearer {k}"));
        }
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let resp = r.send().await.unwrap();
        let s = resp.status().as_u16();
        (s, resp.text().await.unwrap())
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let (s, t) = self.raw(method, path, Some(&self.key), body).await;
        (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }

    async fn ok(&self, method: &str, path: &str, body: Option<Value>) -> Value {
        let (s, v) = self.call(method, path, body.clone()).await;
        assert!(
            (200..300).contains(&s),
            "{method} {path} {body:?} -> {s} {v}"
        );
        v
    }

    async fn user(&self, id: &str) -> User {
        self.ok("PUT", &format!("/v1/users/{id}"), Some(json!({})))
            .await;
        self.hub.user_by_name(id).unwrap()
    }

    async fn token(&self, id: &str) -> String {
        self.ok("POST", &format!("/v1/users/{id}/tokens"), Some(json!({})))
            .await["token"]
            .as_str()
            .unwrap()
            .to_string()
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

/// Every event of a conversation (server-side read).
async fn log(hub: &Hub, conv: &str) -> Vec<Value> {
    hub.read_log(&hub.system_user(), conv, 0, 10_000)
        .await
        .map(|v| v["events"].as_array().cloned().unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .map(|e| e["event"].clone())
        .collect()
}

/// Content of a sender's messages as stored now.
async fn contents_of(hub: &Hub, conv: &str, sender: &str) -> Vec<String> {
    let mut out = vec![];
    for e in log(hub, conv).await {
        if e["type"] == "msg.created" && e["msg"]["sender_id"] == sender {
            out.push(e["msg"]["content"].as_str().unwrap_or("").to_string());
        }
    }
    out
}

async fn audits(hub: &Hub, action: &str) -> usize {
    hub.audit_log(10_000)
        .await
        .unwrap()
        .iter()
        .filter(|a| a["action"] == action)
        .count()
}

async fn erased(hub: &Hub, user: &User) -> Value {
    hub.erasures_idle().await;
    hub.erasure_status(&user.id).await.unwrap().unwrap()
}

async fn deleting_an_account_erases_what_the_person_wrote(db: TestDb) {
    let api = Api::start(&db).await;
    let hub = &api.hub;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    let carol = api.user("carol").await;
    let dave = api.user("dave").await;
    let a = Conn::new(hub, &alice);
    let b = Conn::new(hub, &bob);
    // Alice owns a group (carol is an admin, bob joined first), a direct
    // conversation with bob, a group only she is in, a group she left, and
    // an agent.
    let group = a.group(&[&bob, &carol]).await;
    a.ok(json!({"op": "set_role", "conv_id": group, "user_id": carol.id, "role": "admin"}))
        .await;
    let dm = a
        .ok(json!({"op": "create_conv", "kind": "dm", "title": "dm", "members": [bob.id]}))
        .await["conv_id"]
        .as_str()
        .unwrap()
        .to_string();
    let alone = a.group(&[]).await;
    let left = Conn::new(hub, &dave).group(&[&alice]).await;
    let bot = user(hub, "alices-bot", UserKind::Agent, Some(&alice)).await;
    a.ok(json!({"op": "add_member", "conv_id": group, "user_id": bot.id}))
        .await;
    for c in [&group, &dm, &alone, &left] {
        a.send(c, "alice secret").await;
    }
    a.send(&group, "alice second").await;
    b.send(&group, "bob stays").await;
    b.send(&dm, "bob stays too").await;
    a.ok(json!({"op": "leave", "conv_id": left})).await;

    // Only the developer's backend deletes an account (docs/04 §4.2).
    let (s, r) = api.call("DELETE", "/v1/users/alice", None).await;
    assert_eq!((s, r["state"].as_str()), (202, Some("running")), "{r}");
    // At once: no acting, no adding, no signing in.
    assert_eq!(code(&a.send(&group, "after").await), "forbidden");
    assert_eq!(
        code(
            &Conn::new(hub, &carol)
                .call(json!({"op": "add_member", "conv_id": group, "user_id": alice.id}))
                .await
        ),
        "not_found"
    );
    let (s, _) = api
        .call("POST", "/v1/users/alice/tokens", Some(json!({})))
        .await;
    assert_eq!(s, 403);
    let e = erased(hub, &alice).await;
    assert_eq!(e["state"], "done", "{e}");
    assert_eq!(
        e["conversations_done"], 4,
        "group, dm, alone and the one she left"
    );

    // Her messages are emptied everywhere, others' stay.
    for c in [&group, &dm, &left] {
        assert!(
            contents_of(hub, c, &alice.id)
                .await
                .iter()
                .all(|t| t.is_empty()),
            "{c}: {:?}",
            contents_of(hub, c, &alice.id).await
        );
    }
    assert_eq!(contents_of(hub, &group, &bob.id).await, vec!["bob stays"]);
    assert_eq!(contents_of(hub, &dm, &bob.id).await, vec!["bob stays too"]);
    // Devices are told, and see her leave; ownership passed to the admin.
    let ev = log(hub, &group).await;
    let kinds: Vec<&str> = ev.iter().map(|e| e["type"].as_str().unwrap()).collect();
    assert!(kinds.contains(&"user.erased"), "{kinds:?}");
    let removed = ev
        .iter()
        .find(|e| e["type"] == "member.removed" && e["data"]["user_id"] == alice.id)
        .expect("removed");
    assert_eq!(removed["data"]["reason"], "account_deleted");
    let t = ev
        .iter()
        .find(|e| e["type"] == "owner.transferred")
        .unwrap();
    assert_eq!(t["data"]["to"], carol.id, "an admin takes over first");
    let members = hub.conv_detail(&group).await.unwrap()["members"].clone();
    assert!(!members.to_string().contains("\"alice\""), "{members}");
    // The group she was alone in is gone; so is her agent.
    assert_eq!(
        code(
            &b.call(json!({"op": "send", "conv_id": alone, "client_msg_id": "x", "text": "?"}))
                .await
        ),
        "not_found"
    );
    assert!(hub.shared().destroyed.read().unwrap().contains(&bot.id));
    // The direct conversation stays for bob, without her.
    assert!(
        log(hub, &dm)
            .await
            .iter()
            .any(|e| e["type"] == "member.removed")
    );
    // Profile cleared; the id is never reused.
    let p = hub.user_profile(&alice.id).await.unwrap().unwrap();
    assert!(p["display_name"].is_null());
    let (s, v) = api.call("PUT", "/v1/users/alice", Some(json!({}))).await;
    assert_eq!((s, v["code"].as_str()), (409, Some("conflict")));
    // Audited once, without content.
    assert_eq!(audits(hub, "account.deleted").await, 1);
    assert_eq!(audits(hub, "account.erased").await, 1);
    let all = hub.audit_log(10_000).await.unwrap();
    assert!(
        !serde_json::to_string(&all)
            .unwrap()
            .contains("alice secret")
    );
}

async fn deletion_refusals_and_repeats(db: TestDb) {
    let api = Api::start(&db).await;
    let hub = &api.hub;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    let bot = user(hub, "bot", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(hub, &alice);
    let conv = a.group(&[&bob]).await;
    a.send(&conv, "kept").await;
    // Not from a device: the client protocol has no such operation.
    for c in [&a, &Conn::new(hub, &bot)] {
        assert_eq!(
            code(
                &c.call(json!({"op": "delete_account", "confirm": "alice"}))
                    .await
            ),
            "unknown_op"
        );
    }
    // An agent's account is not deleted (it is destroyed); the system
    // user is not a user of the open API.
    let (s, v) = api.call("DELETE", "/v1/users/bot", None).await;
    assert_eq!((s, v["code"].as_str()), (400, Some("bad_request")), "{v}");
    let (s, _) = api.call("DELETE", "/v1/users/system", None).await;
    assert_eq!(s, 404);
    assert!(hub.erasure_status(&bot.id).await.unwrap().is_none());
    assert!(hub.erasure_status(&alice.id).await.unwrap().is_none());
    assert_eq!(audits(hub, "account.deleted").await, 0);
    assert_eq!(contents_of(hub, &conv, &alice.id).await, vec!["kept"]);
    // Open API: scope, unknown user, unknown erasure.
    let k = hub
        .create_api_key("k", &["users:write".to_string()], None, None)
        .await
        .unwrap()
        .0;
    let (s, _) = api.raw("DELETE", "/v1/users/alice", Some(&k), None).await;
    assert_eq!(s, 403, "needs the compliance scope");
    let (s, _) = api.call("DELETE", "/v1/users/nobody", None).await;
    assert_eq!(s, 404);
    let (s, _) = api.call("GET", "/v1/users/alice/erasure", None).await;
    assert_eq!(s, 404);
    // Deleting again is the same deletion.
    let (s, v) = api.call("DELETE", "/v1/users/alice", None).await;
    assert_eq!((s, v["state"].as_str()), (202, Some("running")));
    let (s, _) = api.call("DELETE", "/v1/users/alice", None).await;
    assert_eq!(s, 202);
    assert_eq!(erased(hub, &alice).await["state"], "done");
    let (s, v) = api.call("DELETE", "/v1/users/alice", None).await;
    assert_eq!((s, v["state"].as_str()), (202, Some("done")));
    assert_eq!(
        api.ok("GET", "/v1/users/alice/erasure", None).await["state"],
        "done"
    );
    let erasures = log(hub, &conv)
        .await
        .iter()
        .filter(|e| e["type"] == "user.erased")
        .count();
    assert_eq!(erasures, 1);
    assert_eq!(audits(hub, "account.deleted").await, 1);
}

async fn an_interrupted_deletion_finishes_after_a_restart(db: TestDb) {
    let cfg = single(&db.url);
    let hub = open(cfg.clone()).await;
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let a = Conn::new(&hub, &alice);
    let convs = [
        a.group(&[&bob]).await,
        a.group(&[&bob]).await,
        a.group(&[&bob]).await,
    ];
    for c in &convs {
        // More than one erase batch in the first conversation.
        let n = if c == &convs[0] { 230 } else { 2 };
        for i in 0..n {
            a.send(c, &format!("m{i}")).await;
        }
    }
    let fp = &hub.shared().failpoints;
    fp.set(
        agentim_core::hub::ERASURE_STEP,
        agentim_core::failpoint::Action::Pause,
        None,
    );
    hub.delete_account(&alice, Some("k")).await.unwrap();
    fp.wait_hits(agentim_core::hub::ERASURE_STEP, 1).await;
    // "Crash": the process goes away mid-way.
    hub.shutdown();
    drop(hub);
    let hub = open(cfg).await;
    let alice = hub.shared().user(&alice.id).unwrap();
    assert_eq!(erased(&hub, &alice).await["state"], "done");
    for c in &convs {
        let left = contents_of(&hub, c, &alice.id).await;
        assert!(
            left.iter().all(|t| t.is_empty()),
            "{c}: {} left",
            left.len()
        );
        let removed = log(&hub, c)
            .await
            .iter()
            .filter(|e| e["type"] == "member.removed")
            .count();
        assert_eq!(removed, 1, "she leaves each conversation once");
    }
}

async fn race_sends_while_the_account_is_deleted(db: TestDb) {
    let hub = open(single(&db.url)).await;
    let bob = user(&hub, "bob", UserKind::Human, None).await;
    let rounds = 6;
    let check = |mutated: bool| {
        let hub = hub.clone();
        let bob = bob.clone();
        async move {
            let mut leaks = 0;
            for r in 0..rounds {
                let name = format!("u{r}{}", if mutated { "m" } else { "" });
                let u = user(&hub, &name, UserKind::Human, None).await;
                let conv = Conn::new(&hub, &u).group(&[&bob]).await;
                let sender = {
                    let hub = hub.clone();
                    let u = u.clone();
                    let conv = conv.clone();
                    tokio::spawn(async move {
                        let c = Conn::new(&hub, &u);
                        for i in 0..60 {
                            let _ = c.send(&conv, &format!("x{i}")).await;
                        }
                    })
                };
                tokio::time::sleep(std::time::Duration::from_millis(2 + r as u64)).await;
                hub.delete_account(&u, None).await.unwrap();
                // Sends through the open API as the user keep trying.
                for i in 0..20 {
                    let _ = hub
                        .act_as(
                            &u,
                            serde_json::from_value::<ClientMsg>(json!({"op": "send", "conv_id": conv, "client_msg_id": format!("late{i}"), "text": "late"})).unwrap(),
                        )
                        .await;
                }
                sender.await.unwrap();
                hub.erasures_idle().await;
                leaks += contents_of(&hub, &conv, &u.id)
                    .await
                    .iter()
                    .filter(|t| !t.is_empty())
                    .count();
                // Invariant: the log replays to the stored state.
                let ev = log(&hub, &conv).await;
                let seqs: Vec<i64> = ev.iter().map(|e| e["seq"].as_i64().unwrap()).collect();
                assert_eq!(seqs, (1..=seqs.len() as i64).collect::<Vec<_>>());
            }
            leaks
        }
    };
    assert_eq!(check(false).await, 0, "nothing of a deleted person stays");
    // Mutation check: without the checks of a deleted account (session,
    // writer, membership) the late sends get through and the test sees it.
    let fp = &hub.shared().failpoints;
    fp.set(
        agentim_core::hub::DELETED_CAN_SEND,
        agentim_core::failpoint::Action::Error,
        None,
    );
    assert!(check(true).await > 0, "the mutation must be caught");
    fp.release(agentim_core::hub::DELETED_CAN_SEND);
}

async fn export_has_the_persons_own_data(db: TestDb) {
    let api = Api::start(&db).await;
    let hub = &api.hub;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    let a = Conn::new(hub, &alice);
    let b = Conn::new(hub, &bob);
    let c1 = a.group(&[&bob]).await;
    let c2 = b.group(&[&alice]).await;
    a.send(&c1, "mine one").await;
    let gone = a.send(&c1, "mine recalled").await["msg_id"].clone();
    a.ok(json!({"op": "recall", "msg_id": gone})).await;
    b.send(&c1, "bobs words").await;
    a.send(&c2, "mine two").await;
    a.ok(json!({"op": "block", "user_id": bob.id})).await;
    // Only the developer's backend exports (compliance scope); it hands
    // the file to the person.
    let (s, body) = api
        .raw("GET", "/v1/users/alice/export", Some(&api.key), None)
        .await;
    assert_eq!(s, 200, "{body}");
    let lines: Vec<Value> = body
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let types: Vec<&str> = lines.iter().map(|l| l["type"].as_str().unwrap()).collect();
    assert_eq!(types[0], "export");
    assert_eq!(*types.last().unwrap(), "end");
    for t in [
        "profile",
        "devices",
        "blocks",
        "agents",
        "reports",
        "conversation",
        "message",
        "answers",
    ] {
        assert!(types.contains(&t), "{t} missing: {types:?}");
    }
    let texts: Vec<Value> = lines
        .iter()
        .filter(|l| l["type"] == "message")
        .map(|l| l["content"].clone())
        .collect();
    assert_eq!(
        texts,
        vec![json!("mine one"), Value::Null, json!("mine two")],
        "only her own; recalled has no content"
    );
    assert!(!body.contains("bobs words"));
    assert!(
        lines
            .iter()
            .any(|l| l["type"] == "blocks" && l["blocks"].to_string().contains(&bob.id))
    );
    // Continue after the first conversation.
    let first = lines.iter().find(|l| l["type"] == "conversation").unwrap()["cursor"]
        .as_str()
        .unwrap()
        .to_string();
    let (s, rest) = api
        .raw(
            "GET",
            &format!("/v1/users/alice/export?after={first}"),
            Some(&api.key),
            None,
        )
        .await;
    assert_eq!(s, 200);
    assert!(!rest.contains("\"profile\""));
    assert_eq!(rest.matches("\"type\":\"conversation\"").count(), 1);
    // Exporting again (page by page) is not limited per person.
    for _ in 0..5 {
        let (s, _) = api
            .raw("GET", "/v1/users/alice/export", Some(&api.key), None)
            .await;
        assert_eq!(s, 200);
    }
    // Refusals: no key, a wrong key, a client token, an unknown user.
    let token = api.token("alice").await;
    for t in [None, Some("nonsense"), Some(token.as_str())] {
        let (s, _) = api.raw("GET", "/v1/users/alice/export", t, None).await;
        assert_eq!(s, 401);
    }
    let (s, _) = api
        .raw("GET", "/v1/users/nobody/export", Some(&api.key), None)
        .await;
    assert_eq!(s, 404);
    let (s, _) = api.raw("GET", "/v1/me/export", Some(&token), None).await;
    assert_eq!(s, 401, "no export with a client token");
    let k = hub
        .create_api_key("k", &["users:read".to_string()], None, None)
        .await
        .unwrap()
        .0;
    let (s, _) = api
        .raw("GET", "/v1/users/alice/export", Some(&k), None)
        .await;
    assert_eq!(s, 403);
    assert_eq!(
        audits(hub, "api.users.export").await,
        7,
        "each export, not the refused"
    );
}

fn report(kind: &str, id: &str, cat: &str) -> Value {
    json!({"op": "report", "target_kind": kind, "target_id": id, "category": cat})
}

async fn reports_follow_the_rules(db: TestDb) {
    let mut cfg = single(&db.url);
    cfg.moderation.report_per_hour = 4;
    let api = Api::with(cfg).await;
    let hub = &api.hub;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    let eve = api.user("eve").await;
    let bot = user(hub, "bot", UserKind::Agent, Some(&bob)).await;
    let a = Conn::new(hub, &alice);
    let b = Conn::new(hub, &bob);
    let conv = b.group(&[&alice]).await;
    let early = b.send(&conv, "before eve").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let msg = b.send(&conv, "nasty words").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mine = a.send(&conv, "my own").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    b.ok(json!({"op": "add_member", "conv_id": conv, "user_id": eve.id, "history": 0}))
        .await;
    let e = Conn::new(hub, &eve);
    let other = b.group(&[]).await;
    // Refusals with business codes, and no trace.
    let cases = [
        (report("message", &msg, "nope"), "bad_request"),
        (report("thing", &msg, "spam"), "bad_request"),
        (report("message", "m_unknown", "spam"), "not_found"),
        (report("message", &mine, "spam"), "bad_request"),
        (report("user", &alice.id, "spam"), "bad_request"),
        (report("user", "u_unknown", "spam"), "not_found"),
        (report("conversation", &other, "spam"), "forbidden"),
        (report("conversation", "c_unknown", "spam"), "not_found"),
    ];
    for (req, want) in cases {
        assert_eq!(code(&a.call(req.clone()).await), want, "{req}");
    }
    let long = "x".repeat(1001);
    let mut r = report("message", &msg, "spam");
    r["text"] = json!(long);
    assert_eq!(code(&a.call(r).await), "bad_request");
    // Not visible to eve (before she could see the log).
    assert_eq!(
        code(&e.call(report("message", &early, "spam")).await),
        "not_found"
    );
    assert_eq!(
        code(
            &Conn::new(hub, &bot)
                .call(report("message", &msg, "spam"))
                .await
        ),
        "forbidden"
    );
    assert!(hub.reports(true, 200, 0).await.unwrap().is_empty());
    // A report, its duplicate, and the evidence kept after a recall.
    let mut r = report("message", &msg, "harassment");
    r["text"] = json!("Größe 你好");
    let first = a.ok(r.clone()).await;
    assert_eq!(first["duplicate"], false);
    let again = a.ok(r).await;
    assert_eq!(
        (again["report_id"].clone(), again["duplicate"].clone()),
        (first["report_id"].clone(), json!(true))
    );
    b.ok(json!({"op": "recall", "msg_id": msg})).await;
    let d = hub
        .report_detail(first["report_id"].as_str().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(d["evidence"]["content"], "nasty words");
    assert_eq!(d["text"], "Größe 你好");
    assert_eq!(d["evidence"]["user_id"], bob.id);
    // Users and conversations; then the hourly limit.
    a.ok(report("user", &bob.id, "spam")).await;
    a.ok(report("conversation", &conv, "spam")).await;
    a.ok(report("user", &eve.id, "other")).await;
    assert_eq!(
        code(&a.call(report("user", &bot.id, "spam")).await),
        "rate_limited"
    );
    assert_eq!(hub.reports(true, 200, 0).await.unwrap().len(), 4);
    // Encrypted at rest: the evidence is not readable in the database.
    let raw = db_text(&db, "SELECT body FROM reports").await;
    assert!(!raw.contains("nasty words"), "{raw}");
}

async fn db_text(db: &TestDb, sql: &str) -> String {
    use sqlx::Row;
    sqlx::any::install_default_drivers();
    let mut c = <sqlx::AnyConnection as sqlx::Connection>::connect(&db.url)
        .await
        .unwrap();
    let rows = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .fetch_all(&mut c)
        .await
        .unwrap();
    rows.iter()
        .map(|r| {
            r.try_get::<Option<String>, _>(0)
                .unwrap()
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn db_exec(db: &TestDb, sql: &str) {
    sqlx::any::install_default_drivers();
    let mut c = <sqlx::AnyConnection as sqlx::Connection>::connect(&db.url)
        .await
        .unwrap();
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .execute(&mut c)
        .await
        .unwrap();
}

async fn the_queue_and_decisions(db: TestDb) {
    let api = Api::start(&db).await;
    let hub = &api.hub;
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    let carol = api.user("carol").await;
    let a = Conn::new(hub, &alice);
    let b = Conn::new(hub, &bob);
    let c = Conn::new(hub, &carol);
    let conv = b.group(&[&alice, &carol]).await;
    let spam = b.send(&conv, "buy now").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let worse = b.send(&conv, "threat").await["msg_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r1 = a.ok(report("message", &spam, "spam")).await["report_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r2 = c.ok(report("message", &spam, "spam")).await["report_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r3 = a.ok(report("message", &worse, "violence")).await["report_id"]
        .as_str()
        .unwrap()
        .to_string();
    let r4 = a.ok(report("user", &carol.id, "other")).await["report_id"]
        .as_str()
        .unwrap()
        .to_string();
    // Most severe first, then earliest due; paging.
    let q = api.ok("GET", "/v1/reports", None).await;
    let ids: Vec<&str> = q["reports"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        ids,
        vec![r3.as_str(), r1.as_str(), r2.as_str(), r4.as_str()]
    );
    let p = api.ok("GET", "/v1/reports?limit=2&offset=2", None).await;
    assert_eq!(p["reports"].as_array().unwrap().len(), 2);
    for bad in [
        "/v1/reports?state=x",
        "/v1/reports?limit=0",
        "/v1/reports?limit=201",
        "/v1/reports?offset=-1",
    ] {
        assert_eq!(api.call("GET", bad, None).await.0, 400, "{bad}");
    }
    let d = api.ok("GET", &format!("/v1/reports/{r1}"), None).await;
    assert_eq!(d["same_target"].as_array().unwrap().len(), 1);
    assert_eq!(api.call("GET", "/v1/reports/nope", None).await.0, 404);
    // Bad decisions change nothing.
    let res = |id: &str, b: Value| {
        let path = format!("/v1/reports/{id}:resolve");
        let api = &api;
        async move { api.call("POST", &path, Some(b)).await }
    };
    for (id, body) in [
        (&r1, json!({"action": "explode", "reason": "x"})),
        (&r1, json!({"action": "none", "reason": ""})),
        (&r1, json!({"action": "suspend_user", "reason": "x"})),
        (
            &r1,
            json!({"action": "suspend_user", "reason": "x", "days": 0}),
        ),
        (&r4, json!({"action": "remove_content", "reason": "x"})),
    ] {
        assert_eq!(res(id, body.clone()).await.0, 400, "{body}");
    }
    assert_eq!(
        res("nope", json!({"action": "none", "reason": "x"}))
            .await
            .0,
        404
    );
    assert_eq!(
        api.call(
            "POST",
            &format!("/v1/reports/{r1}:explode"),
            Some(json!({}))
        )
        .await
        .0,
        404
    );
    // Remove the content: both reports on it are decided, the message is recalled.
    let v = api
        .ok("POST", &format!("/v1/reports/{r1}:resolve"), Some(json!({"action": "remove_content", "reason": "commercial spam", "ground": "terms 4.2"})))
        .await;
    assert_eq!(v["resolved"].as_array().unwrap().len(), 2);
    assert!(
        contents_of(hub, &conv, &bob.id)
            .await
            .contains(&String::new())
    );
    let (s, v) = res(&r2, json!({"action": "none", "reason": "x"})).await;
    assert_eq!((s, v["code"].as_str()), (409, Some("conflict")));
    let d = api.ok("GET", &format!("/v1/reports/{r2}"), None).await;
    assert_eq!(d["decision"]["facts"], "commercial spam");
    assert_eq!(d["decision"]["ground"], "terms 4.2");
    assert_eq!(d["decision"]["automated"], false);
    // Suspend the sender for a day: cannot act, connections end; when the
    // time is over the account works again.
    api.ok(
        "POST",
        &format!("/v1/reports/{r3}:resolve"),
        Some(json!({"action": "suspend_user", "reason": "threats", "days": 1})),
    )
    .await;
    assert_eq!(code(&b.send(&conv, "still here").await), "forbidden");
    hub.moderation_upkeep().await.unwrap();
    assert_eq!(
        code(&b.send(&conv, "still here").await),
        "forbidden",
        "not over yet"
    );
    db_exec(
        &db,
        &format!(
            "UPDATE users SET suspended_until = 1 WHERE id = '{}'",
            bob.id
        ),
    )
    .await;
    hub.moderation_upkeep().await.unwrap();
    assert_eq!(Conn::new(hub, &bob).send(&conv, "back").await["ok"], true);
    // A ban does not end.
    api.ok(
        "POST",
        &format!("/v1/reports/{r4}:resolve"),
        Some(json!({"action": "ban_user", "reason": "abuse"})),
    )
    .await;
    hub.moderation_upkeep().await.unwrap();
    assert_eq!(code(&c.send(&conv, "x").await), "forbidden");
    assert_eq!(
        api.ok("GET", "/v1/reports?state=resolved", None).await["reports"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
    assert!(
        api.ok("GET", "/v1/reports", None).await["reports"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(audits(hub, "report.resolved").await, 3);
    // Scope.
    let k = hub
        .create_api_key("k", &["users:read".to_string()], None, None)
        .await
        .unwrap()
        .0;
    assert_eq!(api.raw("GET", "/v1/reports", Some(&k), None).await.0, 403);
}

async fn race_moderators_decide_once(db: TestDb) {
    let api = Api::start(&db).await;
    let hub = api.hub.clone();
    let alice = api.user("alice").await;
    let bob = api.user("bob").await;
    let a = Conn::new(&hub, &alice);
    let conv = Conn::new(&hub, &bob).group(&[&alice]).await;
    for round in 0..15 {
        let m = Conn::new(&hub, &bob)
            .send(&conv, &format!("m{round}"))
            .await["msg_id"]
            .as_str()
            .unwrap()
            .to_string();
        let id = a.ok(report("message", &m, "spam")).await["report_id"]
            .as_str()
            .unwrap()
            .to_string();
        let tasks: Vec<_> = ["none", "remove_content", "none", "remove_content"]
            .into_iter()
            .map(|action| {
                let hub = hub.clone();
                let id = id.clone();
                tokio::spawn(async move {
                    hub.resolve_report(&id, action, "why", None, false, None, "mod")
                        .await
                })
            })
            .collect();
        let mut wins = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => wins += 1,
                Err(e) => assert_eq!(e.code, "conflict"),
            }
        }
        assert_eq!(wins, 1, "round {round}: exactly one decision");
        let d = hub.report_detail(&id).await.unwrap().unwrap();
        // The recorded decision is the one that took effect.
        let recalled = contents_of(&hub, &conv, &bob.id)
            .await
            .iter()
            .filter(|t| t.is_empty())
            .count();
        let removes = hub
            .reports(false, 200, 0)
            .await
            .unwrap()
            .iter()
            .filter(|r| r["decision"]["action"] == "remove_content")
            .count();
        assert_eq!(recalled, removes, "round {round}: {d}");
    }
}
