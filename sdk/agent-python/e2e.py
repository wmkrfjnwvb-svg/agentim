"""End-to-end check of the Python agent SDK against a real server it starts
itself (docs/03-agents.md §2.4). A person (alice) talks to two
agents over WebSocket; the agents use the SDK.

    AGENTIM_BIN=server/target/debug/agentim python3 sdk/agent-python/e2e.py
"""

import asyncio
import json
import os
import random
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import urllib.request
import uuid
import struct
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.join(HERE, "..", "python"))

from agentim import AgentIM, create_user_token  # noqa: E402  (the server SDK)
from agentim_agent import Agent, AgentIMError, Connection  # noqa: E402

BIN = os.environ.get("AGENTIM_BIN", "server/target/debug/agentim")
PORT = int(os.environ.get("PORT", 20000 + random.randrange(20000)))
URL = f"http://127.0.0.1:{PORT}"
DIR = tempfile.mkdtemp(prefix="agentim-e2e-")
DB = os.path.join(DIR, "e2e.db")
failures = 0
server = None


def png(w, h):
    """A w x h RGB PNG, made here (no image library needed)."""
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    rows = b"".join(b"\0" + bytes(((x * 7) % 256, 0, 0)[i] for x in range(w) for i in range(3)) for _ in range(h))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


def check(cond, what):
    global failures
    print(("ok   " if cond else "FAIL ") + what, flush=True)
    if not cond:
        failures += 1


def cli(*args):
    out = subprocess.run([BIN, "--db", DB, *args], capture_output=True, check=True).stdout
    return json.loads(out)


def start_server():
    global server
    server = subprocess.Popen([BIN, "--db", DB, "--addr", f"127.0.0.1:{PORT}"],
                              stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            urllib.request.urlopen(f"{URL}/readyz", timeout=1)
            return
        except OSError:
            time.sleep(0.1)
    raise RuntimeError("server did not start")


def stop_server():
    server.terminate()
    server.wait()


def on_signal(*_):
    if server:
        server.kill()
    sys.exit(130)


signal.signal(signal.SIGTERM, on_signal)


class Person:
    """A person's client: syncs, then keeps every event and side event."""

    def __init__(self, token):
        self.token = token
        self.events = []
        self.sides = []

    async def connect(self):
        def push(f):
            if f["type"] == "event":
                self.events.append(f["event"])
            elif f["type"] == "side":
                self.sides.append(f["event"])
            elif f["type"] == "resync":
                asyncio.ensure_future(self.sync())

        self.conn = Connection(URL, self.token, push, device="e2e-browser")
        await self.conn.open()
        await self.sync()

    async def sync(self):
        while True:
            r = await self.conn.call("sync", limit=500)
            for c in r["convs"]:
                self.events += [e["event"] for e in c.get("events") or []]
                self.sides += c.get("side_events") or []
            if not r.get("more"):
                return

    async def say(self, conv, text):
        return await self.conn.call("send", conv_id=conv, client_msg_id=str(uuid.uuid4()), text=text)

    async def wait(self, pred, what, timeout=20):
        end = time.time() + timeout
        while time.time() < end:
            for e in self.events + self.sides:
                if pred(e):
                    return e
            await asyncio.sleep(0.05)
        raise RuntimeError(f"timed out waiting for {what}")

    async def reply(self, sender, text):
        return await self.wait(
            lambda e: e.get("msg") and e["msg"]["sender_id"] == sender
            and text in e["msg"]["content"] and e["msg"]["state"] != "streaming",
            repr(text),
        )


async def main():
    start_server()
    key = cli("api-key", "create", "--name", "e2e", "--scopes", "users:write,agents:write")["key"]
    im = AgentIM(URL, key)
    im.upsert_user("alice")
    # The developer's backend signs alice's token itself (docs/13 §4.2).
    sk = cli("signing-key", "create", "--name", "e2e")
    token = create_user_token(sk["kid"], sk["secret"], "alice")
    alice = Person(token)
    await alice.connect()

    # The backend gives alice two agents; her app finds their ids.
    async def agent(name):
        r = im.upsert_user(name, kind="agent", owner="alice")
        u = (await alice.conn.call("find_user", name=name))["user"]
        return {"key": r["key"], "agent": {"id": u["id"]}}

    one = await agent("pybot")
    two = await agent("pybot2")
    conv = (await alice.conn.call("create_conv", kind="group", title="e2e",
                                  members=[one["agent"]["id"], two["agent"]["id"]]))["conv_id"]

    # A conversation pybot is not in.
    elsewhere = (await alice.conn.call("create_conv", kind="group", title="without pybot",
                                       members=[two["agent"]["id"]]))["conv_id"]
    failed_once = {"done": False}
    refused_runs = {"n": 0}
    bot = Agent(URL, one["key"], log=lambda m: print("[pybot]", m, flush=True))

    @bot.on_message
    async def answer(msg, ctx):
        t = msg["content"]
        if msg["sender_id"] == two["agent"]["id"]:
            await ctx.reply("peer answered")
        elif "hello" in t:
            s = await ctx.stream()
            await s.append("Hi ")
            await ctx.progress("step", "streaming")
            await s.append("there")
            await s.end()
        elif "card" in t:
            card = {"blocks": [{"type": "buttons", "id": "go", "options": [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}]}]}
            await bot.send_card(ctx.conv_id, "Go on? (yes/no)", card, reply_to=msg["msg_id"])
        elif "approve" in t:
            d = await bot.ask_human(ctx.conv_id, "Ship it?")
            await ctx.reply(f"decision: {d['state']}")
        elif "state" in t:
            w = await bot.put_state("plan", {"step": 1}, 0)
            conflict = ""
            try:
                await bot.put_state("plan", {"step": 2}, 0)
            except AgentIMError as e:
                conflict = e.code
            await bot.put_state("empty", None, 0)
            r = await bot.get_state("plan")
            e = await bot.get_state("empty")
            await ctx.reply(f"state {w['version']} {conflict} {r['value']['step']} {await bot.get_state('nope')} {e['value']}")
        elif "context" in t:
            c = await bot.context(ctx.conv_id)
            await ctx.reply(f"context has {len(c['events'])} events")
        elif "refused" in t:
            refused_runs["n"] += 1
            await bot.claim(msg["msg_id"])  # not a request: refused, and not retried
        elif "fail once" in t:
            if not failed_once["done"]:
                failed_once["done"] = True
                raise RuntimeError("first try fails")
            await ctx.reply("retried")
        elif "ask peer" in t:
            await bot.send(ctx.conv_id, "pybot2, are you there?", to=["pybot2"], intent="request")
        elif "handoff" in t:
            await bot.handoff(ctx.conv_id, "a person should take this")
        elif "picture" in t:
            sent = await bot.send_file(ctx.conv_id, "chart.png", png(640, 480), caption="the chart",
                                       reply_to=msg["msg_id"])
            doc = await bot.send_file(ctx.conv_id, "report.txt", b"the report", mime="text/plain")
            link = await bot.file_url(ctx.conv_id, doc["file"]["file_id"])
            body = await asyncio.to_thread(lambda: urllib.request.urlopen(link["url"]).read().decode())
            outside = ""
            try:
                await bot.upload_file(elsewhere, "x.txt", b"x")
            except AgentIMError as e:
                outside = e.code
            await ctx.reply(f"files {sent['file']['kind']} {sent['file']['thumb']} {doc['file']['kind']} {body} {outside}")
        elif "sources" in t:
            await ctx.reply("see the docs", sources=[{"title": "Docs", "url": "https://example.com/docs"}])

    @bot.on_card_response
    async def card(r, ctx):
        await ctx.reply(f"card: {r['action']}")

    @bot.on_handback
    async def back(d, ctx):
        await ctx.reply(f"back: {d['note']}")

    await bot.start()
    check(bot.me["id"] == one["agent"]["id"] and bot.me["kind"] == "agent", "the agent connects with its key")
    peer = Agent(URL, two["key"])

    @peer.on_message
    async def here(msg, ctx):
        if msg["sender_id"] == one["agent"]["id"]:
            await ctx.reply("here")

    await peer.start()
    a = one["agent"]["id"]

    await alice.say(conv, "@pybot hello")
    hi = await alice.reply(a, "Hi there")
    check(hi["msg"]["sender_type"] == "agent", "a streamed reply arrives complete, marked as AI")
    check(any(s["type"] == "progress" and s["data"]["text"] == "streaming" for s in alice.sides), "progress reaches the person")

    await alice.say(conv, "@pybot card")
    c = await alice.wait(lambda e: e.get("msg") and e["msg"]["kind"] == "agentim.card", "the card")
    await alice.conn.call("respond", msg_id=c["msg"]["msg_id"], block_id="go", action="yes")
    await alice.reply(a, "card: yes")
    check(True, "a card is answered and the agent gets the answer")

    await alice.say(conv, "@pybot approve")
    asked = await alice.wait(lambda e: e.get("type") == "request.created", "the request")
    await alice.conn.call("decide", request_id=asked["data"]["request"]["request_id"], decision="approve")
    await alice.reply(a, "decision: approved")
    check(True, "the agent asks a person and gets the decision")

    await alice.say(conv, "@pybot state")
    await alice.reply(a, "state 1 conflict 1 None None")
    check(True, "checkpoints: created, conflicting write refused, read back, missing is None, null stored")

    await alice.say(conv, "@pybot context")
    r = await alice.reply(a, "context has")
    check(int(r["msg"]["content"].split()[2]) > 3, "the agent reads its context")

    await alice.say(conv, "@pybot sources")
    r = await alice.reply(a, "see the docs")
    check(r["msg"]["envelope"]["sources"][0]["title"] == "Docs", "sources travel with the reply")

    await asyncio.sleep(3)  # the agent's per-conversation message budget refills (three messages follow)
    await alice.say(conv, "@pybot picture")
    await alice.reply(a, "files image True file the report forbidden")
    pic = await alice.wait(lambda e: e.get("msg") and e["msg"]["kind"] == "image", "the image")
    p = pic["msg"]["payload"]
    check(p["width"] == 640 and p["caption"] == "the chart", "the image message carries the file and its caption")
    thumb = await alice.conn.call("file_url", conv_id=conv, file_id=p["file_id"], variant="thumb")
    tb = urllib.request.urlopen(URL + thumb["url"]).read()
    sof = tb.find(b"\xff\xc0")  # a baseline JPEG's frame header: height, then width
    check(thumb["variant"] == "thumb" and tb[:2] == b"\xff\xd8"
          and struct.unpack(">HH", tb[sof + 5:sof + 9]) == (240, 320), "the person reads a 320 x 240 JPEG thumbnail")
    check(True, "an agent sends an image and a file, reads one back, and cannot upload elsewhere")

    await alice.say(conv, "@pybot fail once")
    await alice.reply(a, "retried")
    check(failed_once["done"], "a failed handler gets the message again (at least once)")

    await alice.say(conv, "@pybot refused")
    await asyncio.sleep(1.5)
    await alice.say(conv, "@pybot sources again")
    await alice.wait(lambda e: e.get("msg") and e["msg"]["sender_id"] == a and e["msg"]["content"] == "see the docs"
                     and e["seq"] > 0 and e is not r, "the second docs reply")
    check(refused_runs["n"] == 1, f"a handler refused by the server is not run again ({refused_runs['n']})")

    await alice.say(conv, "@pybot ask peer")
    await alice.reply(a, "peer answered")
    check(True, "two agents address each other with `to` and answer")

    await alice.say(conv, "@pybot handoff")
    await alice.wait(lambda e: e.get("type") == "agent.state" and e["data"].get("handoff"), "the handoff")
    await alice.conn.call("handback", conv_id=conv, agent_id=a, note="done")
    await alice.reply(a, "back: done")
    check(True, "handed off to a person and back")

    stop_server()
    start_server()
    await alice.conn.close()
    alice.events.clear()
    await alice.connect()
    await asyncio.sleep(1.5)
    await alice.say(conv, "@pybot hello again")
    await alice.reply(a, "Hi there")
    check(True, "after a server restart the agent reconnects and answers")
    await bot.stop()
    await peer.stop()


try:
    asyncio.run(main())
except Exception as e:  # noqa: BLE001
    import traceback

    traceback.print_exc()
    failures += 1
finally:
    if server:
        server.kill()
    shutil.rmtree(DIR, ignore_errors=True)
if failures:
    print(f"{failures} check(s) failed", file=sys.stderr)
    sys.exit(1)
print("agent SDK end-to-end (Python) passed")
