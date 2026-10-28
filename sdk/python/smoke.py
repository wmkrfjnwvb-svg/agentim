"""Smoke test against a running server (AGENTIM_URL, AGENTIM_KEY,
AGENTIM_KID, AGENTIM_SECRET). Run by CI."""
import base64
import os
import socket
import sys
import time
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agentim import AgentIM, AgentIMError, create_user_token  # noqa: E402


def ws_status(token, device=None):
    """The status of a WebSocket handshake to /ws (101: logged in)."""
    u = urllib.parse.urlparse(os.environ["AGENTIM_URL"])
    q = {"token": token, **({"device": device} if device else {})}
    key = base64.b64encode(os.urandom(16)).decode()
    with socket.create_connection((u.hostname, u.port)) as s:
        s.sendall((f"GET /ws?{urllib.parse.urlencode(q)} HTTP/1.1\r\nHost: {u.netloc}\r\n"
                   f"Upgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
                   "Sec-WebSocket-Version: 13\r\n\r\n").encode())
        return int(s.recv(64).split()[1])

im = AgentIM(os.environ["AGENTIM_URL"], os.environ["AGENTIM_KEY"])
run = f"py{int(time.time() * 1000)}"
a, b, c = f"{run}_a", f"{run}_b", f"{run}_c"

im.upsert_user(a, display_name="A")
im.batch_users([{"id": b}, {"id": c}])
assert im.get_user(a)["display_name"] == "A"
kid, secret = os.environ["AGENTIM_KID"], os.environ["AGENTIM_SECRET"]
assert ws_status(create_user_token(kid, secret, a), "smoke-1") == 101
assert ws_status(create_user_token(kid, "wrong", a), "smoke-1") == 401
assert im.revoke_tokens(a) == {"revoked": 1}
bot = f"{run}_bot"
im.upsert_user(bot, kind="agent", owner=a)
k = im.issue_agent_key(bot)
assert k["key"].startswith("atk_") and ws_status(k["key"]) == 101
assert im.revoke_agent_key(bot, k["device_id"]) == {"revoked": 1}
assert ws_status(k["key"]) == 401
assert [x["id"] for x in im.list_agents(a)["agents"]] == [bot]
assert im.stop_agent(bot, reason="smoke")["stopped"] is True
assert im.resume_agent(bot)["stopped"] is False
assert im.export_agent(bot)["sha256"]
assert im.destroy_agent(bot)["destroyed"] is True
assert isinstance(im.list_devices(a)["devices"], list)
assert any(line["type"] == "profile" for line in im.export_user(a))

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
# Files: an image and a file, read back through a short-lived link.
png = base64.b64decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")
img = im.send_file(conv["id"], a, "dot.png", png, caption="a dot")
assert img["file"]["kind"] == "image", img
doc = im.send_file(conv["id"], a, "notes.txt", b"notes", mime="text/plain")
assert doc["file"]["kind"] == "file", doc
link = im.file_url(conv["id"], doc["file"]["file_id"], b)
with urllib.request.urlopen(os.environ["AGENTIM_URL"] + link["url"]) as r:
    assert r.read() == b"notes"
# A small image is its own thumbnail.
assert img["file"]["thumb"] is False
assert im.file_url(conv["id"], img["file"]["file_id"], b, "thumb")["variant"] == "original"
try:
    im.file_url(conv["id"], doc["file"]["file_id"], c)
    raise SystemExit("expected forbidden")
except AgentIMError as e:
    assert e.code == "forbidden", e

for who, code, status in [("nobody", "not_found", 404), (c, "forbidden", 403)]:
    try:
        im.send_message(conv["id"], who, "x")
        raise SystemExit("expected an error")
    except AgentIMError as e:
        assert (e.code, e.status) == (code, status), e

# Moderation: a silenced member cannot send.
im.silence_member(conv["id"], b, 60, reason="smoke")
try:
    im.send_message(conv["id"], b, "x")
    raise SystemExit("expected forbidden")
except AgentIMError as e:
    assert e.code == "forbidden", e
assert im.unsilence_member(conv["id"], b)["changed"] is True
assert isinstance(im.reports()["reports"], list)
im.delete_user(c)
assert im.erasure(c)["state"] in ("running", "done")
im.recall_message(sent["msg_id"], "system")
im.dissolve_conversation(conv["id"], "system")
try:
    im.get_conversation(conv["id"])
    raise SystemExit("expected not_found")
except AgentIMError as e:
    assert e.code == "not_found"
print("python sdk smoke ok")
