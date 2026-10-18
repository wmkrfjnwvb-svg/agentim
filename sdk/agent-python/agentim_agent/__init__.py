"""AgentIM agent SDK for Python 3.9+ (docs/10-agent-ecosystem.md §5.2).

No dependencies. An agent connects with its key, receives what concerns it
in order (sync, then pushes) and acts::

    agent = Agent("https://im.example.com", key)

    @agent.on_message
    async def answer(msg, ctx):
        await ctx.reply(f"you said: {msg['content']}")

    asyncio.run(agent.run())

Delivery is at least once: a batch is acknowledged after its handlers
finish, so a handler that raises (or a crash) gets the event again; use the
event's ``seq`` per conversation to skip what was already done.
"""

import asyncio
import itertools
import json
import uuid
from urllib.parse import quote

from .ws import ConnectionClosed, HandshakeError, WebSocket

__all__ = ["Agent", "AgentIMError", "Connection"]


class AgentIMError(Exception):
    """A refused request; ``code`` is the protocol's error code."""

    def __init__(self, code, message):
        super().__init__(f"{code}: {message}")
        self.code = code
        self.detail = message


# Failures worth another try. A refusal on business grounds (forbidden,
# conflict, taken_over, ...) would only be refused again: the event is
# acknowledged and the refusal logged.
TRANSIENT = {"rate_limited", "disconnected", "storage", "conv_moved", "conv_migrating", "unavailable", "internal"}


def _retriable(e):
    return not isinstance(e, AgentIMError) or e.code in TRANSIENT


def _clean(d):
    # None means "not given", except where null is a value (a checkpoint's
    # value, clearing a rating).
    return {k: v for k, v in d.items() if v is not None or k in ("value", "rating")}


class Connection:
    """One WebSocket connection: requests by ``rid``, pushes to a callback."""

    def __init__(self, url, token, on_push=None):
        base = url.rstrip("/").replace("http", "ws", 1)
        self.url = f"{base}/ws?token={quote(token)}"
        self.on_push = on_push
        self.pending = {}
        self.rids = itertools.count(1)
        self.ws = None
        self.welcome = None
        self.failed = False
        self._reader = None

    async def open(self):
        try:
            self.ws = await WebSocket.connect(self.url)
        except HandshakeError as e:
            raise AgentIMError("unauthorized", str(e)) from e
        except OSError as e:
            raise AgentIMError("disconnected", str(e)) from e
        first = json.loads(await self.ws.recv())
        if first.get("type") != "welcome":
            raise AgentIMError("protocol", f"expected welcome, got {first}")
        self.welcome = first
        self._reader = asyncio.ensure_future(self._read())
        return first

    async def _read(self):
        try:
            while True:
                f = json.loads(await self.ws.recv())
                if f.get("type") == "res":
                    fut = self.pending.pop(f.get("rid"), None)
                    if fut and not fut.done():
                        if f.get("ok"):
                            fut.set_result(f)
                        else:
                            fut.set_exception(AgentIMError(f["error"]["code"], f["error"]["message"]))
                elif self.on_push:
                    self.on_push(f)
        except ConnectionClosed:
            pass
        finally:
            for fut in self.pending.values():
                if not fut.done():
                    fut.set_exception(AgentIMError("disconnected", "connection closed"))
            self.pending.clear()

    async def call(self, op, **args):
        """Sends a request and waits for its response (raises AgentIMError)."""
        if self.ws is None or self.ws.closed:
            raise AgentIMError("disconnected", "not connected")
        rid = next(self.rids)
        fut = asyncio.get_running_loop().create_future()
        self.pending[rid] = fut
        await self.ws.send(json.dumps({"op": op, "rid": rid, **_clean(args)}))
        return await fut

    async def wait_closed(self):
        if self._reader:
            await asyncio.shield(self._reader)

    async def close(self):
        if self.ws:
            await self.ws.close()


class Stream:
    """A streamed reply: ``await s.append(text)``, then ``await s.end()``."""

    def __init__(self, agent, msg_id):
        self.agent = agent
        self.msg_id = msg_id
        self._idx = 0

    async def append(self, text):
        i = self._idx
        self._idx += 1
        return await self.agent.call("stream_append", msg_id=self.msg_id, idx=i, text=text)

    async def end(self):
        return await self.agent.call("stream_end", msg_id=self.msg_id)

    async def cancel(self):
        return await self.agent.call("stream_cancel", msg_id=self.msg_id)


class Ctx:
    """What a handler gets to act in the conversation of an event."""

    def __init__(self, agent, event):
        self.agent = agent
        self.event = event
        self.conv_id = event["conv_id"]
        self.msg = event.get("msg")

    async def reply(self, text, **opts):
        """Replies to the message (or just sends in the conversation)."""
        opts.setdefault("reply_to", self.msg["msg_id"] if self.msg else None)
        return await self.agent.send(self.conv_id, text, **opts)

    async def stream(self):
        return await self.agent.stream(self.conv_id, reply_to=self.msg["msg_id"] if self.msg else None)

    async def progress(self, kind, text, data=None):
        return await self.agent.progress(self.conv_id, kind, text, data=data)


class Agent:
    def __init__(self, url, key, name="agentim-agent-python", log=None, position_check=30.0):
        self.url = url
        self.key = key
        # How often the agent checks it has every message (seconds).
        self.position_check = position_check
        self._last_seq = {}
        self.client_name = name
        self.log = log or (lambda m: None)
        self.handlers = {"message": [], "event": [], "card": [], "handback": []}
        self.waiting = {}  # request_id -> future
        self.me = None
        self.conn = None
        self._stopped = False
        self._queue = None
        self._worker = None

    # Handlers (usable as decorators).
    def on_message(self, fn):
        """Messages that ask this agent to act (named, replied to, or by its trigger)."""
        self.handlers["message"].append(fn)
        return fn

    def on_event(self, fn):
        """Every event delivered to the agent, woken or not."""
        self.handlers["event"].append(fn)
        return fn

    def on_card_response(self, fn):
        self.handlers["card"].append(fn)
        return fn

    def on_handback(self, fn):
        self.handlers["handback"].append(fn)
        return fn

    async def start(self, start_timeout=15.0):
        """Connects (retrying for up to ``start_timeout`` seconds: the server
        looks up a key it has not seen yet at most once a second) and keeps
        the agent connected until ``stop()``."""
        loop = asyncio.get_running_loop()
        end = loop.time() + start_timeout
        wait = 0.3
        while True:
            try:
                await self._connect()
                break
            except AgentIMError as e:
                if loop.time() + wait > end:
                    raise
                self.log(f"connect failed ({e}); retrying in {wait:.1f}s")
                await asyncio.sleep(wait)
                wait = min(wait * 2, 3.0)
        self._keeper = asyncio.ensure_future(self._keep_alive())
        self._checker = asyncio.ensure_future(self._check_positions())
        return self

    async def _check_positions(self):
        """Now and then asks where its conversations stand: a last push lost
        on the way (no later message, no resync notice) is found and synced
        (docs/11 §6.4)."""
        while not self._stopped:
            await asyncio.sleep(self.position_check)
            conn = self.conn
            if self._stopped or conn is None or conn.failed or not self._last_seq:
                continue
            try:
                ids = list(self._last_seq)[:200]
                r = await conn.call("heads", conv_ids=ids)
                heads = r.get("heads") or {}
                if any(heads.get(c, 0) > self._last_seq.get(c, 0) for c in ids) and self._queue is not None:
                    self._queue.put_nowait(("sync", None))
            except AgentIMError:
                pass

    async def run(self):
        """Starts and runs until ``stop()``."""
        await self.start()
        while not self._stopped:
            await asyncio.sleep(0.5)

    async def stop(self):
        self._stopped = True
        if self.conn:
            await self.conn.close()
        if self._worker:
            self._worker.cancel()
        if getattr(self, "_checker", None):
            self._checker.cancel()

    async def _connect(self):
        conn = Connection(self.url, self.key, self._push)
        w = await conn.open()
        self.conn = conn
        self.me = w["me"]
        # Pushes and sync rounds go through one queue: handlers see events
        # in order.
        if self._worker:
            self._worker.cancel()
        self._queue = asyncio.Queue()
        self._worker = asyncio.ensure_future(self._work(conn, self._queue))
        await conn.call("hello", client={"name": self.client_name, "version": "0.1.0"})
        done = asyncio.get_running_loop().create_future()
        self._queue.put_nowait(("sync", done))
        try:
            await done
        except Exception:
            pass

    async def _keep_alive(self):
        wait = 0.5
        while not self._stopped:
            await self.conn.wait_closed()
            if self._stopped:
                return
            self.log(f"disconnected; reconnecting in {wait:.1f}s")
            await asyncio.sleep(wait)
            try:
                await self._connect()
                wait = 0.5
            except AgentIMError as e:
                self.log(f"reconnect failed: {e}")
                wait = min(wait * 2, 30.0)

    def _push(self, f):
        # A decision a handler is waiting for must not queue behind it.
        if f.get("type") == "side":
            self._side(f["event"])
        if self._queue is not None:
            self._queue.put_nowait((f, None))

    async def _work(self, conn, queue):
        while True:
            item, done = await queue.get()
            if conn.failed:
                continue
            try:
                if item == "sync":
                    await self._sync(conn)
                elif item["type"] == "event":
                    await self._handle(item)
                    await conn.call("ack", acks=[{"conv_id": item["event"]["conv_id"], "seq": item["event"]["seq"]}])
                elif item["type"] == "side":
                    s = item["event"]
                    await conn.call("ack", acks=[{"conv_id": s["conv_id"], "side_seq": s["side_seq"]}])
                elif item["type"] == "resync":
                    await self._sync(conn)
                if done:
                    done.set_result(None)
            except Exception as e:  # noqa: BLE001 - any handler error
                # Not acknowledged: this connection acknowledges nothing
                # more (an ack is a position); reconnect, and the server
                # gives it again.
                self.log(f"handler failed: {e!r}")
                conn.failed = True
                if done and not done.done():
                    done.set_exception(e)
                if getattr(e, "code", None) == "rate_limited":
                    await asyncio.sleep(2)
                await conn.close()

    async def _sync(self, conn):
        while True:
            r = await conn.call("sync", limit=200)
            acks = []
            for c in r["convs"]:
                done = None
                try:
                    for item in c.get("events") or []:
                        await self._handle(item)
                        done = item["event"]["seq"]
                except Exception:
                    # What was handled before the failure is not given again.
                    if done is not None:
                        acks.append({"conv_id": c["conv_id"], "seq": done})
                    if acks:
                        try:
                            await conn.call("ack", acks=acks)
                        except AgentIMError:
                            pass
                    raise
                for s in c.get("side_events") or []:
                    self._side(s)
                if "upto" in c:
                    acks.append({"conv_id": c["conv_id"], "seq": c["upto"], "side_seq": c.get("side_upto", 0)})
            if acks:
                await conn.call("ack", acks=acks)
            if not r.get("more"):
                return

    async def _run(self, coro, what):
        """Runs a handler; a business refusal from the server is not retried."""
        try:
            await coro
        except Exception as e:  # noqa: BLE001
            if _retriable(e):
                raise
            self.log(f"{what}: refused by the server ({e.code}): {e.detail}; not retried")

    async def _handle(self, item):
        ev = item["event"]
        self._last_seq[ev["conv_id"]] = max(self._last_seq.get(ev["conv_id"], 0), ev["seq"])
        what = f"{ev['type']} {ev['conv_id']}#{ev['seq']}"
        for h in self.handlers["event"]:
            await self._run(h(ev, item), what)
        if not item.get("wake"):
            return
        ctx = Ctx(self, ev)
        msg = ev.get("msg")
        if msg and msg["sender_id"] != self.me["id"] and ev["type"] in ("msg.created", "msg.updated"):
            for h in self.handlers["message"]:
                await self._run(h(msg, ctx), what)
        elif ev["type"] == "card.response":
            for h in self.handlers["card"]:
                await self._run(h(ev["data"], ctx), what)
        elif ev["type"] == "agent.state" and ev["data"].get("handback"):
            for h in self.handlers["handback"]:
                await self._run(h(ev["data"], ctx), what)

    def _side(self, s):
        rid = (s.get("data") or {}).get("request_id")
        if s.get("type", "").startswith("request.") and rid in self.waiting:
            fut = self.waiting.pop(rid)
            if not fut.done():
                fut.set_result(s["data"])

    # Operations.
    async def call(self, op, **args):
        """Any protocol operation (docs/05 §7.1)."""
        return await self.conn.call(op, **args)

    async def send(self, conv_id, text, reply_to=None, to=None, topics=None, intent=None,
                   refs=None, sources=None, client_msg_id=None):
        return await self.call(
            "send", conv_id=conv_id, text=text, client_msg_id=client_msg_id or str(uuid.uuid4()),
            reply_to=reply_to, to=to, topics=topics, intent=intent, refs=refs, sources=sources,
        )

    async def send_card(self, conv_id, text, card, reply_to=None):
        """A card (docs/03 §9.6.1.1); ``text`` is what clients without cards show."""
        return await self.call("send", conv_id=conv_id, text=text, kind="agentim.card", payload=card,
                               client_msg_id=str(uuid.uuid4()), reply_to=reply_to)

    async def stream(self, conv_id, reply_to=None):
        r = await self.call("stream_start", conv_id=conv_id, client_msg_id=str(uuid.uuid4()), reply_to=reply_to)
        return Stream(self, r["msg_id"])

    async def progress(self, conv_id, kind, text, msg_id=None, data=None):
        return await self.call("progress", conv_id=conv_id, kind=kind, text=text, msg_id=msg_id, data=data)

    async def ask_human(self, conv_id, title, kind="approve", context=None, options=None,
                        assignees=None, ttl_secs=None, timeout=3600.0):
        """Asks a person to decide and waits: {state, response, decided_by}."""
        r = await self.call("request_human", conv_id=conv_id, kind=kind, title=title, context=context,
                            options=options, assignees=assignees, ttl_secs=ttl_secs,
                            client_req_id=str(uuid.uuid4()))
        rid = r["request_id"]
        fut = asyncio.get_running_loop().create_future()
        self.waiting[rid] = fut
        now = await self.call("get_request", request_id=rid)
        if now["state"] != "pending":
            self.waiting.pop(rid, None)
            return {"state": now["state"], "response": now.get("response"), "decided_by": now.get("decided_by")}
        try:
            return await asyncio.wait_for(fut, timeout)
        except asyncio.TimeoutError:
            self.waiting.pop(rid, None)
            return {"state": "timeout"}

    async def handoff(self, conv_id, note, to=None):
        return await self.call("handoff", conv_id=conv_id, note=note, to=to)

    async def history(self, conv_id, after_seq=None, before_seq=None, limit=None):
        return await self.call("history", conv_id=conv_id, after_seq=after_seq, before_seq=before_seq, limit=limit)

    async def context(self, conv_id, max_tokens=None):
        return await self.call("context", conv_id=conv_id, max_tokens=max_tokens)

    async def search(self, query, conv_id=None, limit=None):
        return await self.call("search", query=query, conv_id=conv_id, limit=limit)

    async def put_summary(self, conv_id, from_seq, to_seq, text):
        return await self.call("put_summary", conv_id=conv_id, from_seq=from_seq, to_seq=to_seq, text=text)

    async def get_state(self, key):
        """A checkpoint {key, value, version}, or None when there is none."""
        try:
            return await self.call("state_get", key=key)
        except AgentIMError as e:
            if e.code == "not_found":
                return None
            raise

    async def put_state(self, key, value, version):
        """Writes a checkpoint; ``version`` is the one read (0 creates)."""
        return await self.conn.call("state_put", key=key, value=value, version=version)

    async def claim(self, msg_id):
        return await self.call("claim", msg_id=msg_id)

    async def update_task(self, conv_id, state, note=None):
        return await self.call("update_task", conv_id=conv_id, state=state, note=note)
