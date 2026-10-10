"""Smoke test against a running server (AGENTIM_URL, AGENTIM_KEY). Run by CI."""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agentim import AgentIM, AgentIMError  # noqa: E402

im = AgentIM(os.environ["AGENTIM_URL"], os.environ["AGENTIM_KEY"])
run = f"py{int(time.time() * 1000)}"
a, b, c = f"{run}_a", f"{run}_b", f"{run}_c"

im.upsert_user(a, display_name="A")
im.batch_users([{"id": b}, {"id": c}])
assert im.get_user(a)["display_name"] == "A"
assert im.issue_token(a, device_name="smoke")["token"].startswith("atk_")

conv = im.create_conversation("system", "group", "smoke", members=[b], owner=a)
sent = im.send_message(conv["id"], a, "hello", client_msg_id="m1")
again = im.send_message(conv["id"], a, "hello", client_msg_id="m1")
assert again["duplicate"] is True and again["msg_id"] == sent["msg_id"]
im.send_message(conv["id"], "system", "notice")
im.edit_message(sent["msg_id"], a, "hello!")
dm = im.send_to_user(b, a, "hi")
assert [r["ok"] for r in im.batch_send(b, [a, "nobody"], "promo")["results"]] == [True, False]
h = im.history(conv["id"], limit=200)
assert any(e.get("msg") and e["msg"]["content"] == "hello!" for e in h["events"])
im.mark_read(dm["conv_id"], b, 1)

for who, code, status in [("nobody", "not_found", 404), (c, "forbidden", 403)]:
    try:
        im.send_message(conv["id"], who, "x")
        raise SystemExit("expected an error")
    except AgentIMError as e:
        assert (e.code, e.status) == (code, status), e

im.recall_message(sent["msg_id"], "system")
im.dissolve_conversation(conv["id"], "system")
try:
    im.get_conversation(conv["id"])
    raise SystemExit("expected not_found")
except AgentIMError as e:
    assert e.code == "not_found"
print("python sdk smoke ok")
