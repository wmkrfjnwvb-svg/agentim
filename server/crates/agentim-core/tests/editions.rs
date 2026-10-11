//! AgentIM Lite and AgentIM (docs/01-product.md §6): same protocol and
//! schema, so one database moves between the two by replacing the binary;
//! Lite refuses configurations that need AgentIM modules and says which
//! product it is.

mod common;

use agentim_core::edition::Edition;
use agentim_core::hub::Hub;
use agentim_core::model::*;
use common::*;
use serde_json::json;

fn as_(e: Edition) -> agentim_core::logic::HubConfig {
    let mut h = hub_cfg();
    h.edition = e;
    h
}

#[tokio::test]
async fn lite_runs_on_one_database_only() {
    let main = new_db(Backend::Sqlite).await.unwrap();
    let secret = new_db(Backend::Sqlite).await.unwrap();
    let e = Hub::open(two(&main.url, &secret.url, "secret"), as_(Edition::Lite))
        .await
        .err()
        .unwrap();
    assert_eq!(e.code, "config");
    assert!(
        e.message.contains("AgentIM Lite runs on one database"),
        "{}",
        e.message
    );
    assert!(
        Hub::open(two(&main.url, &secret.url, "secret"), as_(Edition::Full))
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn hello_names_the_product() {
    for (e, name, product) in [
        (Edition::Lite, "agentim-lite", "AgentIM Lite"),
        (Edition::Full, "agentim", "AgentIM"),
    ] {
        let db = new_db(Backend::Sqlite).await.unwrap();
        let hub = Hub::open(single(&db.url), as_(e)).await.unwrap();
        let alice = user(&hub, "alice", UserKind::Human, None).await;
        let r = Conn::new(&hub, &alice).ok(json!({"op": "hello"})).await;
        assert_eq!(r["server"]["name"], name, "{r}");
        assert_eq!(r["server"]["product"], product);
        assert_eq!(r["server"]["edition"], e.as_str());
        assert_eq!(hub.edition(), e);
    }
}

#[tokio::test]
async fn one_database_moves_between_the_products() {
    let db = new_db(Backend::Sqlite).await.unwrap();
    // Start on Lite...
    let hub = Hub::open(single(&db.url), as_(Edition::Lite))
        .await
        .unwrap();
    let alice = user(&hub, "alice", UserKind::Human, None).await;
    let bot = user(&hub, "bot", UserKind::Agent, Some(&alice)).await;
    let a = Conn::new(&hub, &alice);
    let conv = a.group(&[&bot]).await;
    a.send(&conv, "written by lite").await;
    let (key, _) = hub
        .create_api_key("k", &["users:read".into()], None, None)
        .await
        .unwrap();
    drop(a);
    drop(hub);
    // ...replace the binary: everything is there.
    let hub = Hub::open(single(&db.url), as_(Edition::Full))
        .await
        .unwrap();
    let a = Conn::new(&hub, &alice);
    assert!(
        a.pull(&conv, 0)
            .await
            .to_string()
            .contains("written by lite")
    );
    assert!(hub.api_key(&key).await.is_some(), "keys survive");
    a.send(&conv, "written by agentim").await;
    drop(a);
    drop(hub);
    // ...and back.
    let hub = Hub::open(single(&db.url), as_(Edition::Lite))
        .await
        .unwrap();
    let p = Conn::new(&hub, &alice).pull(&conv, 0).await.to_string();
    assert!(p.contains("written by lite") && p.contains("written by agentim"));
}
