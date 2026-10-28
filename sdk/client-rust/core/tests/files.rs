//! Files and images from the app (docs/02-architecture.md §2.7, core side):
//! upload_start, PUTs in parts with at most four at once, the thumbnail,
//! upload_complete, then the message; resumed after failures and restarts,
//! cancelled when discarded, and never holding back other messages.

mod common;

use agentim_client_core::*;
use common::*;
use serde_json::{Value, json};

fn image(conv: &str, cm: &str, size: u64) -> Value {
    json!({"cmd": "send_file", "conv_id": conv, "client_msg_id": cm,
        "file": {"source": format!("/pics/{cm}.jpg"), "name": format!("{cm}.jpg"), "size": size, "mime": "image/jpeg", "width": 4000, "height": 3000},
        "thumb": {"source": format!("/tmp/{cm}.thumb.jpg"), "size": 7, "width": 320, "height": 240},
        "caption": "the view"})
}

fn file(conv: &str, cm: &str, size: u64) -> Value {
    json!({"cmd": "send_file", "conv_id": conv, "client_msg_id": cm,
        "file": {"source": format!("/docs/{cm}"), "name": format!("{cm}.pdf"), "size": size, "mime": "application/pdf"}})
}

fn ops(w: &World) -> Vec<String> {
    w.server
        .requests
        .iter()
        .map(|r| r["op"].as_str().unwrap().to_string())
        .collect()
}

fn count(w: &World, op: &str) -> usize {
    ops(w).iter().filter(|o| *o == op).count()
}

/// The server's messages of a conversation, in order: (kind, text).
fn stored(w: &World, conv: &str) -> Vec<(String, String)> {
    w.server.logs[conv]
        .iter()
        .filter(|e| e["type"] == "msg.created")
        .map(|e| {
            (
                e["msg"]["kind"].as_str().unwrap().to_string(),
                e["msg"]["content"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn local(w: &World, conv: &str, cm: &str) -> Value {
    w.views[conv][&format!("local:{cm}")].clone()
}

#[test]
fn an_image_goes_up_in_parts_with_its_thumbnail_then_is_sent() {
    let mut w = World::new(1);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    let r = w.ask(image("c1", "i1", 95)).unwrap();
    assert_eq!(r["key"], "local:i1");
    // Shown at once: uploading, with what the app picked.
    let m = local(&w, "c1", "i1");
    assert_eq!(
        (m["status"].as_str(), m["kind"].as_str()),
        (Some("uploading"), Some("image"))
    );
    assert_eq!(m["payload"]["local_source"], "/pics/i1.jpg");
    assert_eq!(m["payload"]["local_thumb"], "/tmp/i1.thumb.jpg");
    assert_eq!(m["upload"], json!({"sent": 0, "size": 95}));
    w.run(5_000);
    // 10 parts of 10 bytes (the last 5) and the thumbnail; three
    // addresses per answer, so upload_parts three times more.
    let mut keys: Vec<String> = w.puts.iter().map(|p| p.1.clone()).collect();
    keys.sort();
    let mut want: Vec<String> = (1..=10).map(|n| n.to_string()).collect();
    want.push("thumb".into());
    want.sort();
    assert_eq!(keys, want, "every part once");
    assert_eq!(count(&w, "upload_start"), 1);
    assert_eq!(count(&w, "upload_parts"), 3);
    assert_eq!(count(&w, "upload_complete"), 1);
    assert!(w.most_running <= upload::PUTS_AT_ONCE, "{}", w.most_running);
    // upload_complete names every part with its ETag.
    let done = w
        .server
        .requests
        .iter()
        .find(|r| r["op"] == "upload_complete")
        .unwrap();
    assert_eq!(done["parts"].as_array().unwrap().len(), 10);
    assert_eq!(done["parts"][0], json!({"n": 1, "etag": "e-1"}));
    // Sent with the file and the caption; the local copy is gone.
    let send = w
        .server
        .requests
        .iter()
        .find(|r| r["op"] == "send")
        .unwrap();
    assert_eq!(send["kind"], "image");
    assert_eq!(send["text"], "[Image]");
    assert_eq!(send["payload"]["caption"], "the view");
    let fid = send["payload"]["file_id"].as_str().unwrap().to_string();
    assert!(w.server.uploads[&fid].taken);
    assert!(!w.views["c1"].contains_key("local:i1"));
    assert!(w.store.ops.is_empty(), "nothing left in the queue");
    // Its sources were released once the server had the file.
    assert_eq!(w.releases.len(), 1);
    w.assert_converged();
}

#[test]
fn progress_shows_while_parts_go_up() {
    let mut w = World::new(2);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.ask(file("c1", "f1", 40)).unwrap();
    let mut seen = vec![];
    for _ in 0..40 {
        w.run(10);
        if let Some(m) = w.views["c1"].get("local:f1") {
            seen.push(m["upload"]["sent"].as_u64().unwrap());
        }
    }
    assert!(
        seen.windows(2).all(|p| p[0] <= p[1]),
        "never goes back: {seen:?}"
    );
    assert!(
        seen.iter().any(|s| *s > 0 && *s < 40),
        "in between: {seen:?}"
    );
    w.run(5_000);
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
}

#[test]
fn an_upload_does_not_hold_back_messages_written_after_it() {
    let mut w = World::new(3);
    w.server.conv("c1");
    w.connected();
    w.put_ms = 2_000;
    w.ask(file("c1", "f1", 25)).unwrap();
    w.ask(
        json!({"cmd": "send", "conv_id": "c1", "client_msg_id": "t1", "text": "while it uploads"}),
    )
    .unwrap();
    w.run(500);
    assert_eq!(
        stored(&w, "c1"),
        [("text".into(), "while it uploads".into())]
    );
    w.run(20_000);
    assert_eq!(
        stored(&w, "c1"),
        [
            ("text".into(), "while it uploads".into()),
            ("file".into(), "[File] f1.pdf".into())
        ]
    );
    // Texts after a completed file keep their order behind it.
    w.put_ms = 50;
    w.ask(file("c1", "f2", 5)).unwrap();
    w.run(2_000);
    w.ask(json!({"cmd": "send", "conv_id": "c1", "client_msg_id": "t2", "text": "after"}))
        .unwrap();
    w.run(2_000);
    let s = stored(&w, "c1");
    assert_eq!(
        s[2..],
        [
            ("file".into(), "[File] f2.pdf".into()),
            ("text".into(), "after".into())
        ]
    );
}

#[test]
fn after_a_restart_only_the_missing_parts_go_up() {
    let mut w = World::new(4);
    w.server.conv("c1");
    w.connected();
    w.put_ms = 100;
    w.ask(file("c1", "f1", 100)).unwrap();
    w.run(250);
    let before: Vec<(String, String)> = w.puts.clone();
    let done_before: Vec<String> = w.server.objects.keys().cloned().collect();
    assert!(
        !done_before.is_empty() && done_before.len() < 10,
        "{done_before:?}"
    );
    w.restart(5);
    w.cmd(json!({"cmd": "connect"}));
    w.run(10_000);
    assert_eq!(count(&w, "upload_start"), 1, "the same upload");
    let after = &w.puts[before.len()..];
    for d in &done_before {
        let key = d.split('/').nth(1).unwrap();
        assert!(
            !after.iter().any(|p| p.1 == key),
            "part {key} was stored before the restart and went up again"
        );
    }
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
    w.assert_converged();
}

#[test]
fn failed_puts_are_retried_and_expired_addresses_asked_again() {
    let mut w = World::new(6);
    w.server.conv("c1");
    w.connected();
    // A lost connection, a busy store, an expired signature.
    w.put_plan.extend([
        PutOutcome::Lost,
        PutOutcome::Status(503),
        PutOutcome::Status(403),
    ]);
    w.ask(file("c1", "f1", 30)).unwrap();
    w.run(60_000);
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
    assert!(
        count(&w, "upload_parts") >= 1,
        "the 403 asked for new addresses"
    );
    // Addresses that expire while waiting are asked for again too.
    w.server.upload_ttl_ms = 90_000;
    w.ask(file("c1", "f2", 30)).unwrap();
    w.cut(1006, "network");
    w.run(1_000);
    w.server.now = w.now;
    w.run(120_000);
    assert_eq!(stored(&w, "c1").len(), 2);
    w.assert_converged();
}

#[test]
fn puts_go_on_without_the_connection() {
    let mut w = World::new(7);
    w.server.conv("c1");
    w.connected();
    w.put_ms = 300;
    w.ask(file("c1", "f1", 30)).unwrap();
    w.run(150);
    let puts = w.puts.len();
    assert!(puts > 0);
    w.plan.extend([Outcome::Unreachable, Outcome::Unreachable]);
    w.cut(1006, "network");
    w.run(1_000);
    assert_eq!(
        w.server.objects.len(),
        3,
        "the running PUTs finished offline"
    );
    w.run(30_000);
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
}

#[test]
fn an_upload_the_server_lost_starts_over_a_few_times() {
    let mut w = World::new(8);
    w.server.conv("c1");
    w.connected();
    w.put_ms = 200;
    w.ask(file("c1", "f1", 40)).unwrap();
    w.run(100);
    // Swept (a day passed): the next upload_parts says not_found.
    w.server.uploads.clear();
    w.run(30_000);
    assert_eq!(count(&w, "upload_start"), 2, "started over once");
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
    // Lost every time: after three restarts it fails, retry starts again.
    w.server
        .reject
        .entry("upload_parts".into())
        .or_default()
        .extend((0..4).map(|_| json!({"code": "not_found", "message": "gone"})));
    w.server.parts_per_answer = 1;
    w.ask(file("c1", "f2", 30)).unwrap();
    w.run(60_000);
    let m = w.core.handle(Input::Tick, w.now);
    drop(m);
    let failed = w
        .emitted("operation")
        .into_iter()
        .any(|e| e["result"] == "failed" && e["error"]["code"] == "not_found");
    assert!(failed, "fails after three restarts");
    w.ask(json!({"cmd": "retry", "conv_id": "c1", "client_msg_id": "f2"}))
        .unwrap();
    w.run(60_000);
    assert_eq!(stored(&w, "c1").len(), 2);
}

#[test]
fn a_refused_completion_fails_and_a_retry_uploads_again() {
    let mut w = World::new(9);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.server
        .reject
        .entry("upload_complete".into())
        .or_default()
        .push_back(json!({"code": "bad_request", "message": "the size does not match"}));
    w.ask(file("c1", "f1", 15)).unwrap();
    w.run(10_000);
    let m = local(&w, "c1", "f1");
    assert_eq!(m["status"], "failed");
    assert_eq!(m["error"]["code"], "bad_request");
    assert_eq!(count(&w, "send"), 0, "nothing sent");
    w.ask(json!({"cmd": "retry", "conv_id": "c1", "client_msg_id": "f1"}))
        .unwrap();
    w.run(10_000);
    assert_eq!(
        count(&w, "upload_start"),
        2,
        "uploaded again from the start"
    );
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
}

#[test]
fn a_proxy_refusing_the_part_size_fails_at_once() {
    let mut w = World::new(10);
    w.server.conv("c1");
    w.connected();
    w.put_plan.push_back(PutOutcome::Status(413));
    w.ask(file("c1", "f1", 5)).unwrap();
    w.run(10_000);
    let e = w.emitted("operation");
    assert_eq!(e.last().unwrap()["result"], "failed");
    assert!(
        e.last().unwrap()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("body limit")
    );
    assert_eq!(w.puts.len(), 1, "not retried");
}

#[test]
fn a_source_deleted_meanwhile_fails_the_send() {
    let mut w = World::new(17);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.put_plan.push_back(PutOutcome::Unreadable);
    w.ask(file("c1", "f1", 5)).unwrap();
    w.run(10_000);
    let m = local(&w, "c1", "f1");
    assert_eq!(m["status"], "failed");
    assert!(
        m["error"]["message"]
            .as_str()
            .unwrap()
            .contains("no longer be read")
    );
    assert_eq!(w.puts.len(), 1, "not retried");
}

#[test]
fn a_completed_upload_whose_answer_was_lost_completes_again() {
    let mut w = World::new(11);
    w.server.conv("c1");
    w.connected();
    w.ask(file("c1", "f1", 5)).unwrap();
    while w.puts.is_empty() {
        w.run(1);
    }
    // While the PUT runs nothing else waits for an answer: the next one
    // lost is upload_complete's.
    w.lose_answers = 1;
    w.run(20_000);
    assert!(count(&w, "upload_complete") >= 2);
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
    w.assert_converged();
}

#[test]
fn discarding_an_upload_cancels_its_puts_and_ignores_their_results() {
    let mut w = World::new(12);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.put_ms = 1_000;
    w.ask(file("c1", "f1", 50)).unwrap();
    w.run(300);
    assert!(!w.puts.is_empty());
    // Not refused while uploading (only while the message itself is sent).
    w.ask(json!({"cmd": "discard", "conv_id": "c1", "client_msg_id": "f1"}))
        .unwrap();
    assert_eq!(w.releases.len(), 1);
    assert!(!w.views["c1"].contains_key("local:f1"));
    w.run(10_000);
    assert_eq!(count(&w, "upload_complete"), 0);
    assert_eq!(count(&w, "send"), 0);
    assert!(w.store.ops.is_empty());
    // Recalled before it was sent: the same.
    w.ask(file("c1", "f2", 50)).unwrap();
    w.run(300);
    w.ask(json!({"cmd": "recall", "conv_id": "c1", "client_msg_id": "f2"}))
        .unwrap();
    assert_eq!(w.releases.len(), 2);
    w.run(10_000);
    assert_eq!(count(&w, "send"), 0);
}

#[test]
fn editing_a_file_before_it_is_sent_changes_its_caption() {
    let mut w = World::new(13);
    w.server.conv("c1");
    w.connected();
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    w.put_ms = 500;
    w.ask(image("c1", "i1", 20)).unwrap();
    w.run(100);
    w.ask(json!({"cmd": "edit", "conv_id": "c1", "client_msg_id": "i1", "text": "better caption"}))
        .unwrap();
    assert_eq!(
        local(&w, "c1", "i1")["payload"]["caption"],
        "better caption"
    );
    w.run(10_000);
    let send = w
        .server
        .requests
        .iter()
        .find(|r| r["op"] == "send")
        .unwrap();
    assert_eq!(send["payload"]["caption"], "better caption");
    assert_eq!(send["text"], "[Image]");
}

#[test]
fn send_file_checks_what_it_is_given() {
    let mut w = World::new(14);
    w.server.conv("c1");
    w.connected();
    let bad = [
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"name": "a", "size": 1}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "size": 1}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "name": "a", "size": 0}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "name": "a"}}),
        json!({"cmd": "send_file", "file": {"source": "s", "name": "a", "size": 1}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "name": "a", "size": 1},
            "thumb": {"source": "t", "size": (512 << 10) + 1}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "name": "a", "size": 1},
            "thumb": {"source": "t", "size": 9, "width": 321, "height": 10}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "name": "a", "size": 1},
            "thumb": {"size": 9}}),
        json!({"cmd": "send_file", "conv_id": "c1", "file": {"source": "s", "name": "a", "size": 1},
            "caption": "x".repeat(4097)}),
    ];
    for b in bad {
        assert_eq!(w.ask(b.clone()).unwrap_err().code, "bad_request", "{b}");
    }
    assert!(w.store.ops.is_empty() && w.core.debug_state()["queue"] == json!([]));
    // A picture without a thumbnail but with a size is an image; without
    // either, a file. The same client_msg_id twice is one message.
    let pic = json!({"cmd": "send_file", "conv_id": "c1", "client_msg_id": "p",
        "file": {"source": "s", "name": "p.png", "size": 5, "mime": "image/png", "width": 100, "height": 80}});
    w.ask(pic.clone()).unwrap();
    w.ask(pic).unwrap();
    w.run(10_000);
    assert_eq!(count(&w, "upload_start"), 1);
    assert_eq!(stored(&w, "c1"), [("image".into(), "[Image]".into())]);
}

#[test]
fn a_conversation_left_or_dissolved_takes_no_files() {
    let mut w = World::new(15);
    w.server.conv("c1");
    w.connected();
    w.server.forbidden.insert("c1".into());
    w.cmd(json!({"cmd": "messages", "conv_id": "c1"}));
    // Refused by the server: shown as failed, nothing uploaded.
    w.ask(file("c1", "f1", 5)).unwrap();
    w.run(5_000);
    assert_eq!(local(&w, "c1", "f1")["error"]["code"], "forbidden");
    assert!(w.puts.is_empty());
}

#[test]
fn file_urls_are_absolute_and_reused_while_valid() {
    let mut w = World::new(16);
    w.server.conv("c1");
    w.connected();
    w.ask(file("c1", "f1", 5)).unwrap();
    w.run(5_000);
    let fid = w.server.uploads.keys().next().unwrap().clone();
    let id = w.cmd(json!({"cmd": "file_url", "conv_id": "c1", "file_id": fid}));
    w.run(1_000);
    let u = w.reply(id).unwrap().unwrap();
    assert!(
        u["url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("https://im.example/files/d/{fid}/original")),
        "{u}"
    );
    // Asked again: answered from memory, also offline.
    w.cut(1006, "network");
    w.plan.extend((0..5).map(|_| Outcome::Unreachable));
    w.run(100);
    let again = w
        .ask(json!({"cmd": "file_url", "conv_id": "c1", "file_id": fid}))
        .unwrap();
    assert_eq!(again["url"], u["url"]);
    assert_eq!(count(&w, "file_url"), 1);
    // Another variant, offline: not known.
    let e = w
        .ask(json!({"cmd": "file_url", "conv_id": "c1", "file_id": fid, "variant": "thumb"}))
        .unwrap_err();
    assert_eq!(e.code, "disconnected");
    // Expired: asked again.
    w.plan.clear();
    w.run(300_000);
    let id = w.cmd(json!({"cmd": "file_url", "conv_id": "c1", "file_id": fid}));
    w.run(1_000);
    assert!(w.reply(id).unwrap().is_ok());
    assert_eq!(count(&w, "file_url"), 2);
    // Unknown file: the server's answer.
    let id = w.cmd(json!({"cmd": "file_url", "conv_id": "c1", "file_id": "nope"}));
    w.run(1_000);
    assert_eq!(w.reply(id).unwrap().unwrap_err().code, "not_found");
    assert_eq!(
        w.ask(json!({"cmd": "file_url", "conv_id": "c1"}))
            .unwrap_err()
            .code,
        "bad_request"
    );
}

/// Many files and texts over a bad network with restarts: every message
/// arrives exactly once, texts keep their order, files complete.
#[test]
fn files_and_texts_survive_faults_and_restarts() {
    for seed in 0..30u64 {
        let mut w = World::new(seed);
        w.server.conv("c1");
        w.server.conv("c2");
        w.connected();
        let mut rng = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut next = |m: u64| {
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (rng >> 33) % m
        };
        let mut texts = 0;
        for i in 0..12 {
            let conv = if next(2) == 0 { "c1" } else { "c2" };
            match next(3) {
                0 => {
                    w.ask(file(conv, &format!("f{i}"), 1 + next(60))).unwrap();
                }
                1 => {
                    w.ask(image(conv, &format!("i{i}"), 1 + next(60))).unwrap();
                }
                _ => {
                    texts += 1;
                    w.ask(json!({"cmd": "send", "conv_id": conv, "client_msg_id": format!("t{i}"), "text": format!("t{i}")}))
                        .unwrap();
                }
            }
            for _ in 0..next(4) {
                w.put_plan.push_back(match next(3) {
                    0 => PutOutcome::Lost,
                    1 => PutOutcome::Status(500),
                    _ => PutOutcome::Status(403),
                });
            }
            match next(6) {
                0 => w.cut(1006, "network"),
                1 => {
                    w.restart(seed + 100 + i);
                    w.cmd(json!({"cmd": "connect"}));
                }
                2 => w.lose_answers = 1,
                _ => {}
            }
            w.run(next(400) as i64);
        }
        w.run(600_000);
        let all: Vec<(String, String)> = ["c1", "c2"].iter().flat_map(|c| stored(&w, c)).collect();
        assert_eq!(all.len(), 12, "seed {seed}: {all:?}");
        let mut cms = w.server.applied_cms.clone();
        cms.sort();
        cms.dedup();
        assert_eq!(cms.len(), 12, "seed {seed}: each once");
        assert_eq!(all.iter().filter(|m| m.0 == "text").count(), texts);
        for c in ["c1", "c2"] {
            let t: Vec<u32> = stored(&w, c)
                .iter()
                .filter(|m| m.0 == "text")
                .map(|m| m.1[1..].parse().unwrap())
                .collect();
            assert!(
                t.windows(2).all(|p| p[0] < p[1]),
                "seed {seed}: texts in order {t:?}"
            );
        }
        assert!(w.store.ops.is_empty(), "seed {seed}: queue empty");
        assert!(w.most_running <= upload::PUTS_AT_ONCE);
        w.assert_converged();
    }
}

#[test]
fn an_s3_store_gets_its_own_addresses_and_trouble_is_retried() {
    let mut w = World::new(18);
    w.server.conv("c1");
    w.server.s3 = true;
    w.connected();
    // The server busy once: upload_start is asked again later.
    w.server
        .reject
        .entry("upload_start".into())
        .or_default()
        .push_back(json!({"code": "unavailable", "message": "busy"}));
    w.ask(image("c1", "i1", 25)).unwrap();
    w.run(30_000);
    assert_eq!(count(&w, "upload_start"), 2);
    // The signed headers are passed on as they are.
    let put = w
        .effects
        .iter()
        .find_map(|e| match e {
            Effect::Put { headers, key, .. } if key == "1" => Some(headers.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(put.get("content-length").map(String::as_str), Some("x"));
    assert_eq!(stored(&w, "c1"), [("image".into(), "[Image]".into())]);
}

#[test]
fn an_upload_found_complete_when_asking_for_addresses_is_completed() {
    let mut w = World::new(19);
    w.server.conv("c1");
    w.server.parts_per_answer = 1;
    w.connected();
    w.put_ms = 200;
    w.ask(file("c1", "f1", 25)).unwrap();
    while w.server.objects.is_empty() {
        w.run(10);
    }
    // Meanwhile it was completed (an answer lost): the next upload_parts
    // says conflict, and upload_complete gives the file.
    let fid = w.server.uploads.keys().next().unwrap().clone();
    for n in 1..=3 {
        w.server.objects.insert(format!("{fid}/{n}"), 10);
    }
    w.server.uploads.get_mut(&fid).unwrap().ready = true;
    w.run(30_000);
    assert!(w.server.requests.iter().any(|r| r["op"] == "upload_parts"));
    assert_eq!(count(&w, "upload_start"), 1);
    assert_eq!(stored(&w, "c1"), [("file".into(), "[File] f1.pdf".into())]);
}
