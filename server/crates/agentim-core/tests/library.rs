//! The built-in library of roles and Skills (docs/en/design/assistants.md,
//! AST-15): listing it, installing a role with the Skills it names, a Skill
//! with its files, `replace`, whether the library has the shipped version,
//! what an agent with an installed role receives, refusals that leave no
//! trace, who may install, a restart, and concurrent installs with exactly
//! one winner.

mod common;

use std::net::SocketAddr;

use agentim_core::hub::Hub;
use agentim_core::library::{ROLES, SKILLS};
use agentim_core::model::*;
use agentim_core::server::{AppState, router};
use common::*;
use serde_json::{Value, json};

backend_tests!(
    the_library_lists_what_ships,
    installing_a_role_brings_its_skills,
    a_skill_installs_with_its_files,
    installing_again_needs_replace,
    agents_get_installed_roles_and_skills,
    refusals_leave_no_trace,
    installs_survive_a_restart,
    race_installs_have_one_winner,
);

struct Srv {
    base: String,
    hub: Hub,
    http: reqwest::Client,
    dev: String,
}

impl Srv {
    async fn start(db: &TestDb) -> Srv {
        Srv::on(Hub::open(single(&db.url), hub_cfg()).await.unwrap()).await
    }

    async fn on(hub: Hub) -> Srv {
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
        let (dev, _) = hub.create_admin_key("dev", "developer").await.unwrap();
        Srv {
            base: format!("http://{addr}"),
            hub,
            http: reqwest::Client::new(),
            dev,
        }
    }

    async fn call_as(
        &self,
        key: &str,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, Value) {
        let mut r = self
            .http
            .request(
                method.parse().unwrap(),
                format!("{}/admin/v1/app{path}", self.base),
            )
            .bearer_auth(key);
        if let Some(b) = body {
            r = r
                .header("content-type", "application/json")
                .body(b.to_string());
        }
        let res = r.send().await.unwrap();
        let s = res.status().as_u16();
        let t = res.text().await.unwrap();
        (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
    }

    async fn call(&self, method: &str, path: &str, body: Option<&str>) -> (u16, Value) {
        self.call_as(&self.dev, method, path, body).await
    }

    async fn ok(&self, method: &str, path: &str, body: Option<&str>) -> Value {
        let (s, v) = self.call(method, path, body).await;
        assert!((200..300).contains(&s), "{method} {path} -> {s} {v}");
        v
    }

    fn find<'a>(list: &'a Value, key: &str, name: &str) -> &'a Value {
        list[key]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["name"] == name)
            .unwrap_or_else(|| panic!("{name} not in {list}"))
    }

    async fn library_names(&self, what: &str) -> Vec<String> {
        let v = self.ok("GET", &format!("/{what}"), None).await;
        let mut n: Vec<String> = v[what]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["name"].as_str().unwrap().to_string())
            .collect();
        n.sort();
        n
    }

    async fn audits(&self, prefix: &str) -> usize {
        self.hub
            .audit_log(10_000)
            .await
            .unwrap()
            .into_iter()
            .filter(|a| a["action"].as_str().unwrap_or("").starts_with(prefix))
            .count()
    }
}

/// Every built-in role and Skill is listed, in order, none installed.
async fn the_library_lists_what_ships(db: TestDb) {
    let s = Srv::start(&db).await;
    let roles = s.ok("GET", "/library/roles", None).await;
    let names: Vec<_> = roles["roles"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ROLES.iter().map(|(n, _)| *n).collect::<Vec<_>>());
    for r in roles["roles"].as_array().unwrap() {
        assert_eq!(r["installed"], false, "{r}");
        assert_eq!(r["current"], false);
        assert_eq!(r["assistant"], false);
        assert!(!r["description"].as_str().unwrap().is_empty());
        assert!(!r["instructions"].as_str().unwrap().is_empty());
    }
    let order = Srv::find(&roles, "roles", "order-support");
    assert!(
        order["skills"]
            .as_array()
            .unwrap()
            .iter()
            .any(|k| k == "actions-with-approval")
    );
    let skills = s.ok("GET", "/library/skills", None).await;
    assert_eq!(skills["skills"].as_array().unwrap().len(), SKILLS.len());
    let cards = Srv::find(&skills, "skills", "show-data-as-cards");
    assert_eq!(cards["files"], json!(["references/blocks.md"]));
    assert!(
        cards["used_by"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r == "order-support")
    );
    assert_eq!(cards["installed"], false);
    // Moderators may look.
    let (modk, _) = s.hub.create_admin_key("m", "moderator").await.unwrap();
    assert_eq!(s.call_as(&modk, "GET", "/library/roles", None).await.0, 200);
}

/// A role comes with the Skills it names that the library lacks; Skills the
/// app already has (and may have changed) are left as they are.
async fn installing_a_role_brings_its_skills(db: TestDb) {
    let s = Srv::start(&db).await;
    // The app already has its own handoff skill.
    let own =
        "---\nname: handoff-to-a-person\ndescription: Our own hand-off rules\n---\nCall Sam.\n";
    let (st, v) = s
        .call_as(
            &s.dev,
            "PUT",
            "/skills/handoff-to-a-person",
            Some(&json!({"markdown": own}).to_string()),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let v = s.ok("POST", "/library/roles/order-support", None).await;
    assert_eq!(v["role"], "order-support");
    assert_eq!(
        v["skills_added"],
        json!([
            "support-replies",
            "show-data-as-cards",
            "collect-details-with-forms",
            "actions-with-approval"
        ])
    );
    assert_eq!(v["skills_updated"], json!([]));
    let role = s.ok("GET", "/roles/order-support", None).await;
    assert_eq!(role["name"], "order-support");
    let kept = s.ok("GET", "/skills/handoff-to-a-person", None).await;
    assert_eq!(kept["markdown"], own, "the app's own skill is untouched");
    let ex = s.ok("GET", "/skills/actions-with-approval", None).await;
    assert_eq!(ex["files"], json!(["references/examples.md"]));
    let roles = s.ok("GET", "/library/roles", None).await;
    let r = Srv::find(&roles, "roles", "order-support");
    assert_eq!(
        (r["installed"].clone(), r["current"].clone()),
        (json!(true), json!(true))
    );
    let skills = s.ok("GET", "/library/skills", None).await;
    let h = Srv::find(&skills, "skills", "handoff-to-a-person");
    assert_eq!(
        (h["installed"].clone(), h["current"].clone()),
        (json!(true), json!(false))
    );
    // A second role shares skills: only the missing ones are added.
    let v = s.ok("POST", "/library/roles/booking", Some("{}")).await;
    assert_eq!(v["skills_added"], json!(["booking-appointments"]));
    assert_eq!(s.audits("admin.library.role_install").await, 2);
}

async fn a_skill_installs_with_its_files(db: TestDb) {
    let s = Srv::start(&db).await;
    let v = s
        .ok("POST", "/library/skills/collect-details-with-forms", None)
        .await;
    assert_eq!(v["role"], Value::Null);
    assert_eq!(v["skills_added"], json!(["collect-details-with-forms"]));
    let k = s
        .ok("GET", "/skills/collect-details-with-forms", None)
        .await;
    assert_eq!(k["files"], json!(["references/fields.md"]));
    let b = SKILLS
        .iter()
        .find(|x| x.name == "collect-details-with-forms")
        .unwrap();
    assert_eq!(k["markdown"], b.markdown);
    // A skill without files.
    let v = s.ok("POST", "/library/skills/translation", None).await;
    assert_eq!(v["skills_added"], json!(["translation"]));
    assert_eq!(
        s.ok("GET", "/skills/translation", None).await["files"],
        json!([])
    );
    assert_eq!(s.audits("admin.library.skill_install").await, 2);
}

/// Installing what is there is refused; `replace` brings back the shipped
/// text (and only the shipped files) and the item is current again.
async fn installing_again_needs_replace(db: TestDb) {
    let s = Srv::start(&db).await;
    s.ok("POST", "/library/roles/support", None).await;
    let (st, v) = s.call("POST", "/library/roles/support", None).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["rule"], "role.exists");
    s.ok("POST", "/library/skills/actions-with-approval", None)
        .await;
    let (st, v) = s
        .call("POST", "/library/skills/actions-with-approval", None)
        .await;
    assert_eq!((st, v["rule"].clone()), (409, json!("skill.exists")));
    // The app changes them.
    let mine = "---\nname: support\ndescription: Ours\n---\nBe terse.\n";
    let (st, _) = s
        .call(
            "PUT",
            "/roles/support",
            Some(&json!({"markdown": mine}).to_string()),
        )
        .await;
    assert_eq!(st, 200);
    let (st, _) = s
        .call(
            "PUT",
            "/skills/actions-with-approval/files/notes/extra.md",
            Some("\"x\""),
        )
        .await;
    assert_eq!(st, 200);
    let roles = s.ok("GET", "/library/roles", None).await;
    assert_eq!(Srv::find(&roles, "roles", "support")["current"], false);
    // Replace.
    let v = s
        .ok(
            "POST",
            "/library/roles/support",
            Some(r#"{"replace": true}"#),
        )
        .await;
    assert_eq!(
        v["skills_added"],
        json!([]),
        "existing skills are not touched by a role"
    );
    let role = s.ok("GET", "/roles/support", None).await;
    assert_eq!(
        role["markdown"],
        agentim_core::library::role("support").unwrap()
    );
    let v = s
        .ok(
            "POST",
            "/library/skills/actions-with-approval",
            Some(r#"{"replace": true}"#),
        )
        .await;
    assert_eq!(v["skills_updated"], json!(["actions-with-approval"]));
    let k = s.ok("GET", "/skills/actions-with-approval", None).await;
    assert_eq!(
        k["files"],
        json!(["references/examples.md"]),
        "files not shipped are gone"
    );
    let roles = s.ok("GET", "/library/roles", None).await;
    assert_eq!(Srv::find(&roles, "roles", "support")["current"], true);
    let skills = s.ok("GET", "/library/skills", None).await;
    assert_eq!(
        Srv::find(&skills, "skills", "actions-with-approval")["current"],
        true
    );
}

/// An agent given an installed role receives its instructions and Skills;
/// replacing the role tells its agents their configuration changed.
async fn agents_get_installed_roles_and_skills(db: TestDb) {
    let s = Srv::start(&db).await;
    let alice = user(&s.hub, "alice", UserKind::Human, None).await;
    let (bot, _) = s
        .hub
        .create_user_with("aide", UserKind::Agent, Some(&alice.id), None, true)
        .await
        .unwrap();
    s.ok("POST", "/library/roles/advisor-aide", None).await;
    s.ok(
        "PUT",
        "/agents/aide/role",
        Some(r#"{"role": "advisor-aide"}"#),
    )
    .await;
    let cfg = s.hub.agent_config(&bot).await.unwrap();
    assert_eq!(cfg["role"]["name"], "advisor-aide");
    let names: Vec<_> = cfg["skills"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["draft-for-review", "summarizing-conversations"]);
    let before = cfg["version"].clone();
    // The app edits the role, then brings the shipped one back.
    let mine = "---\nname: advisor-aide\ndescription: Ours\nskills: [draft-for-review]\n---\nShort drafts.\n";
    s.ok(
        "PUT",
        "/roles/advisor-aide",
        Some(&json!({"markdown": mine}).to_string()),
    )
    .await;
    let edited = s.hub.agent_config(&bot).await.unwrap();
    assert_ne!(edited["version"], before);
    s.ok(
        "POST",
        "/library/roles/advisor-aide",
        Some(r#"{"replace": true}"#),
    )
    .await;
    let back = s.hub.agent_config(&bot).await.unwrap();
    assert_ne!(
        back["version"], edited["version"],
        "agents are told it changed"
    );
    for k in ["name", "description", "instructions", "skills", "tags"] {
        assert_eq!(
            back["role"][k], cfg["role"][k],
            "the shipped role again: {k}"
        );
    }
    assert_eq!(back["skills"], cfg["skills"]);
}

async fn refusals_leave_no_trace(db: TestDb) {
    let s = Srv::start(&db).await;
    let (modk, _) = s.hub.create_admin_key("m", "moderator").await.unwrap();
    type Case<'a> = (&'a str, &'a str, Option<&'a str>, u16, Option<&'a str>);
    let cases: Vec<Case> = vec![
        (
            s.dev.as_str(),
            "/library/roles/wizard",
            None,
            404,
            Some("role.catalog"),
        ),
        (
            s.dev.as_str(),
            "/library/skills/wizardry",
            None,
            404,
            Some("skill.catalog"),
        ),
        (
            s.dev.as_str(),
            "/library/roles/support",
            Some("{\"replace\": 1}"),
            400,
            None,
        ),
        (
            s.dev.as_str(),
            "/library/roles/support",
            Some("{\"other\": true}"),
            400,
            None,
        ),
        (
            s.dev.as_str(),
            "/library/skills/translation",
            Some("not json"),
            400,
            None,
        ),
        (modk.as_str(), "/library/roles/support", None, 403, None),
        (
            modk.as_str(),
            "/library/skills/translation",
            None,
            403,
            None,
        ),
    ];
    for (key, path, body, want, rule) in cases {
        let (st, v) = s.call_as(key, "POST", path, body).await;
        assert_eq!(st, want, "{path} {body:?}: {v}");
        if let Some(r) = rule {
            assert_eq!(v["rule"], r, "{v}");
        }
    }
    // A conflict on the role leaves its skills uninstalled too: the library
    // has a role named support (the app's own), so nothing is written.
    let mine = "---\nname: support\ndescription: Ours\n---\nHi.\n";
    s.ok(
        "PUT",
        "/roles/support",
        Some(&json!({"markdown": mine}).to_string()),
    )
    .await;
    let (st, _) = s.call("POST", "/library/roles/support", None).await;
    assert_eq!(st, 409);
    assert!(
        s.library_names("skills").await.is_empty(),
        "no skill was added"
    );
    assert_eq!(s.ok("GET", "/roles/support", None).await["markdown"], mine);
    assert_eq!(s.audits("admin.library").await, 0);
}

async fn installs_survive_a_restart(db: TestDb) {
    let s = Srv::start(&db).await;
    s.ok("POST", "/library/roles/it-helpdesk", None).await;
    let skills = s.library_names("skills").await;
    drop(s);
    let s = Srv::on(Hub::open(single(&db.url), hub_cfg()).await.unwrap()).await;
    assert_eq!(s.library_names("skills").await, skills);
    let roles = s.ok("GET", "/library/roles", None).await;
    let r = Srv::find(&roles, "roles", "it-helpdesk");
    assert_eq!(
        (r["installed"].clone(), r["current"].clone()),
        (json!(true), json!(true))
    );
}

/// Concurrent installs of the same role (or of roles sharing Skills): one
/// install of a role wins, every Skill is there once with its files, and
/// a refused install wrote nothing.
async fn race_installs_have_one_winner(db: TestDb) {
    let s = Srv::start(&db).await;
    for round in 0..6 {
        let audits = s.audits("admin.library.role_install").await;
        let mut tasks = vec![];
        for i in 0..6 {
            let path = if i % 2 == 0 {
                "/library/roles/order-support"
            } else {
                "/library/roles/booking"
            };
            let http = s.http.clone();
            let url = format!("{}/admin/v1/app{path}", s.base);
            let key = s.dev.clone();
            tasks.push(tokio::spawn(async move {
                let r = http.post(url).bearer_auth(key).send().await.unwrap();
                (path, r.status().as_u16())
            }));
        }
        let mut ok = std::collections::HashMap::new();
        for t in tasks {
            let (path, st) = t.await.unwrap();
            assert!(st == 200 || st == 409, "round {round}: {path} {st}");
            if st == 200 {
                *ok.entry(path).or_insert(0) += 1;
            }
        }
        assert_eq!(
            ok.get("/library/roles/order-support"),
            Some(&1),
            "round {round}"
        );
        assert_eq!(ok.get("/library/roles/booking"), Some(&1), "round {round}");
        assert_eq!(s.audits("admin.library.role_install").await, audits + 2);
        let mut want: Vec<String> = ["order-support", "booking"]
            .iter()
            .flat_map(|r| {
                agentim_core::role::parse(r, agentim_core::library::role(r).unwrap())
                    .unwrap()
                    .skills
            })
            .collect();
        want.sort();
        want.dedup();
        assert_eq!(s.library_names("skills").await, want, "round {round}");
        for k in &want {
            let b = SKILLS.iter().find(|x| x.name == k.as_str()).unwrap();
            let got = s.ok("GET", &format!("/skills/{k}"), None).await;
            assert_eq!(got["files"].as_array().unwrap().len(), b.files.len());
        }
        // Empty the library for the next round.
        for r in ["order-support", "booking"] {
            s.ok("DELETE", &format!("/roles/{r}"), None).await;
        }
        for k in &want {
            s.ok("DELETE", &format!("/skills/{k}"), None).await;
        }
    }
}
