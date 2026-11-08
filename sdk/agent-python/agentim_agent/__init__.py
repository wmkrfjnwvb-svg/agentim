"""AgentIM agent SDK for Python 3.9+ (docs/en/design/agents.md).

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
import os
import re
import uuid
import urllib.error
import urllib.request
from urllib.parse import quote, urlencode, urljoin

from .ws import ConnectionClosed, HandshakeError, WebSocket

__all__ = ["Agent", "AgentIMError", "Connection", "child_trace"]


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


def _http_base(url):
    """The server's HTTP address, whether the agent was given http(s) or ws(s)."""
    return url.rstrip("/").replace("ws", "http", 1) if url.startswith("ws") else url.rstrip("/")


def _clean(d):
    # None means "not given", except where null is a value (a checkpoint's
    # value, clearing a rating).
    return {k: v for k, v in d.items() if v is not None or k in ("value", "rating")}


class Connection:
    """One WebSocket connection: requests by ``rid``, pushes to a callback."""

    def __init__(self, url, token, on_push=None, device=None):
        """``device``: a person's device id (agents connect with a key and none)."""
        base = url.rstrip("/").replace("http", "ws", 1)
        # The token goes in a header, never the address (proxies log it).
        self.url = f"{base}/ws" + (f"?device={quote(device)}" if device else "")
        self.headers = {"Authorization": f"Bearer {token}"}
        self.on_push = on_push
        self.pending = {}
        self.rids = itertools.count(1)
        self.ws = None
        self.welcome = None
        self.failed = False
        self._reader = None

    async def open(self):
        """Opens the connection and waits for the welcome frame (who the agent is, its
        limits).
        """
        try:
            self.ws = await WebSocket.connect(self.url, headers=self.headers)
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
        """Waits until the connection is closed."""
        if self._reader:
            await asyncio.shield(self._reader)

    async def close(self):
        """Closes the connection."""
        if self.ws:
            await self.ws.close()


class Stream:
    """A streamed reply: ``await s.append(text)``, then ``await s.end()``."""

    def __init__(self, agent, msg_id):
        self.agent = agent
        self.msg_id = msg_id
        self._idx = 0

    async def append(self, text):
        """Appends text to the stream."""
        i = self._idx
        self._idx += 1
        return await self.agent.call("stream_append", msg_id=self.msg_id, idx=i, text=text)

    async def end(self):
        """Ends the stream: the message becomes final."""
        return await self.agent.call("stream_end", msg_id=self.msg_id)

    async def cancel(self):
        """Cancels the stream (shown as stopped)."""
        return await self.agent.call("stream_cancel", msg_id=self.msg_id)


_TRACEPARENT = re.compile(r"^00-([0-9a-f]{32})-[0-9a-f]{16}-([0-9a-f]{2})$")


def child_trace(traceparent):
    """A W3C traceparent in the same trace with a new span id (docs/en/design/tracing.md);
    None for a missing or invalid one."""
    m = _TRACEPARENT.match(traceparent or "")
    if not m:
        return None
    return f"00-{m.group(1)}-{os.urandom(8).hex()}-{m.group(2)}"


class Ctx:
    """What a handler gets to act in the conversation of an event.

    ``trace`` is the W3C traceparent of the request that woke the agent (None
    when the event came by sync), ``trace_id`` its trace id: pass them on to
    your model calls and services. Replies, streams and progress from this
    context are sent in the same trace.
    """

    def __init__(self, agent, event, trace=None):
        self.agent = agent
        self.event = event
        self.conv_id = event["conv_id"]
        self.msg = event.get("msg")
        self.trace = trace
        self.trace_id = trace.split("-")[1] if trace else None

    @property
    def config(self):
        """The agent's configuration now (role, skills, MCP, APIs; docs/en/design/agents.md)."""
        return self.agent.config

    @property
    def role(self):
        """Its role (name, description, instructions, ...), or None."""
        return (self.agent.config or {}).get("role")

    async def reply(self, text, **opts):
        """Replies to the message (or just sends in the conversation), in its trace."""
        opts.setdefault("reply_to", self.msg["msg_id"] if self.msg else None)
        opts.setdefault("trace", child_trace(self.trace))
        return await self.agent.send(self.conv_id, text, **opts)

    async def stream(self):
        """Starts a streamed reply to the message being handled."""
        return await self.agent.stream(
            self.conv_id, reply_to=self.msg["msg_id"] if self.msg else None, trace=child_trace(self.trace)
        )

    async def progress(self, kind, text, data=None):
        """Reports progress on the conversation being handled."""
        return await self.agent.progress(self.conv_id, kind, text, data=data, trace=child_trace(self.trace))

    async def show_card(self, title, blocks, text=None):
        """Shows data as a card in reply (docs/en/design/chat-components.md): text, facts, table, list or your own ``x-…`` blocks."""
        return await self.agent.send_card(self.conv_id, text or title, {"title": title, "blocks": blocks},
                                          reply_to=self.msg["msg_id"] if self.msg else None)

    async def ask_form(self, title, fields, intro=None, submit="Send", to=None, timeout=1800.0):
        """Asks the person who wrote with a form card only they can fill in and waits
        for their answers (docs/en/design/chat-components.md): returns the values, keyed by field id."""
        if to is None and self.msg:
            to = [self.msg["sender_id"]]
        return await self.agent.ask_form(self.conv_id, title, fields, intro=intro, submit=submit, to=to,
                                         reply_to=self.msg["msg_id"] if self.msg else None, timeout=timeout)

    async def steps(self, titles, msg_id=None):
        """Steps people see next to the reply (docs/en/design/chat-components.md): ``s = await ctx.steps([...])``,
        then ``await s.start(0)``, ``s.done(0)``, ``s.wait(1, "Waiting for approval")``,
        ``s.fail(1, "...")``, ``s.skip(1)``."""
        return await Steps.begin(self, titles, msg_id)


class Steps:
    """The steps of one reply (docs/en/design/chat-components.md); each change is a ``progress`` event."""

    def __init__(self, ctx, titles, msg_id):
        self._ctx, self._titles, self._msg_id = ctx, list(titles), msg_id
        self._ids = [f"s{i + 1}" for i in range(len(self._titles))]

    @classmethod
    async def begin(cls, ctx, titles, msg_id=None):
        """Lists the steps (a `plan` event) and returns them."""
        s = cls(ctx, titles, msg_id)
        await s._send("plan", " → ".join(s._titles)[:4000],
                      {"steps": [{"id": i, "title": t} for i, t in zip(s._ids, s._titles)]})
        return s

    async def _send(self, kind, text, data):
        return await self._ctx.agent.progress(self._ctx.conv_id, kind, text, msg_id=self._msg_id, data=data,
                                              trace=child_trace(self._ctx.trace))

    async def _set(self, i, state, detail):
        data = {"id": self._ids[i], "state": state, "title": self._titles[i]}
        if detail:
            data["detail"] = detail
        return await self._send("step", self._titles[i], data)

    async def start(self, i, detail=None):
        """Step ``i`` is in progress."""
        return await self._set(i, "running", detail)

    async def done(self, i, detail=None):
        """Step ``i`` is done."""
        return await self._set(i, "done", detail)

    async def wait(self, i, detail=None):
        """Step ``i`` waits (for a person: say who in ``detail``)."""
        return await self._set(i, "waiting", detail)

    async def fail(self, i, detail=None):
        """Step ``i`` failed (``detail``: why, shown to people)."""
        return await self._set(i, "failed", detail)

    async def skip(self, i, detail=None):
        """Step ``i`` is not needed after all."""
        return await self._set(i, "skipped", detail)


class Agent:
    """An agent: connects with its key, receives what concerns it in order (sync, then
    pushes), runs your handlers and acts. Delivery is at least once: a batch is
    acknowledged after its handlers finish.
    """
    def __init__(self, url, key, name="agentim-agent-python", log=None, position_check=30.0, files=None):
        self.url = url
        # Where ``write_files`` keeps skills and MCP servers current (a dict
        # of its arguments, or True for the defaults), or None.
        self.files = files
        self.key = key
        # How often the agent checks it has every message (seconds).
        self.position_check = position_check
        self._last_seq = {}
        self.client_name = name
        self.log = log or (lambda m: None)
        self.handlers = {"message": [], "event": [], "card": [], "handback": [], "config": []}
        # The configuration from the server, kept current (docs/en/design/agents.md).
        self.config = None
        self.waiting = {}  # request_id -> future
        self.forms = {}  # card msg_id -> future
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
        """Called when a person answers one of the agent's cards (a button or a filled
        form).
        """
        self.handlers["card"].append(fn)
        return fn

    def on_handback(self, fn):
        """Called when the person a conversation was handed off to hands it back, with
        their note.
        """
        self.handlers["handback"].append(fn)
        return fn

    def on_config(self, fn):
        """Called with the configuration when it arrives at login and whenever it
        changes.
        """
        self.handlers["config"].append(fn)
        return fn

    async def _set_config(self, c):
        if not c or (self.config and c.get("version") == self.config.get("version")):
            return
        self.config = c
        if self.files:
            try:
                await self.write_files(**({} if self.files is True else self.files))
            except Exception as e:  # noqa: BLE001 - logged, the agent keeps running
                self.log(f"files: {e!r}")
        for h in self.handlers["config"]:
            try:
                r = h(c)
                if asyncio.iscoroutine(r):
                    await r
            except Exception as e:  # noqa: BLE001 - a handler's error
                self.log(f"config handler failed: {e!r}")

    def mcp_servers(self):
        """The configuration's MCP servers as agent frameworks take them (e.g.
        OpenAI Agents SDK's ``MCPServerStreamableHttp(params=...)``): dicts of
        name, url, headers."""
        return [
            {"name": m["name"], "url": m["url"], "headers": m.get("headers") or {}}
            for m in (self.config or {}).get("mcp") or []
        ]

    async def write_files(self, dir=".", skills_dir=".claude/skills", mcp_file=".mcp.json", apis_dir=".agentim/apis"):
        """Writes the configuration where agent frameworks look for it
        (docs/en/design/assistants.md): each skill as ``<skills_dir>/<name>/SKILL.md`` with its
        files (Claude Code, Claude Agent SDK), the MCP servers into
        ``mcp_file`` (``mcpServers`` with type http, url, headers) and each
        API's OpenAPI document into ``<apis_dir>/<name>.openapi.json``. Only
        what it wrote before is replaced or removed (a manifest in
        ``.agentim/files.json``). The MCP file holds credentials: it is
        written readable by its owner only. Returns the names written."""
        c = self.config
        if not c:
            raise RuntimeError("no configuration yet: call start() first")
        base = _http_base(self.url)
        key = self.key

        def safe(p):
            return isinstance(p, str) and p != "" and not p.startswith("/") and all(
                seg not in ("", "..") for seg in re.split(r"[\\/]", p)
            )

        def fetch(url):
            req = urllib.request.Request(url, headers={"authorization": f"Bearer {key}"})
            try:
                with urllib.request.urlopen(req) as r:
                    return r.read().decode("utf-8")
            except urllib.error.HTTPError as e:
                raise AgentIMError("unavailable", f"{url}: HTTP {e.code}") from None

        def write(path, text, mode=None):
            os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
            with open(path, "w", encoding="utf-8") as f:
                f.write(text)
            if mode is not None:
                os.chmod(path, mode)

        def run():
            import shutil

            manifest = os.path.join(dir, ".agentim", "files.json")
            before = {"skills": [], "mcp": [], "apis": []}
            try:
                with open(manifest, encoding="utf-8") as f:
                    before.update(json.load(f))
            except (OSError, ValueError):
                pass
            skills = [s for s in c.get("skills") or [] if safe(s.get("name"))]
            for s in skills:
                root = os.path.join(dir, skills_dir, s["name"])
                shutil.rmtree(root, ignore_errors=True)
                md = s.get("markdown") or f"---\nname: {s['name']}\ndescription: {s.get('description', '')}\n---\n{s.get('instructions', '')}\n"
                write(os.path.join(root, "SKILL.md"), md)
                for f in [f for f in s.get("files") or [] if safe(f)]:
                    path = "/".join(quote(x, safe="") for x in f.split("/"))
                    write(os.path.join(root, *f.split("/")), fetch(f"{base}/api/agent/skills/{quote(s['name'], safe='')}/files/{path}"))
            for name in before["skills"]:
                if safe(name) and not any(s["name"] == name for s in skills):
                    shutil.rmtree(os.path.join(dir, skills_dir, name), ignore_errors=True)
            mcp_path = os.path.join(dir, mcp_file)
            try:
                with open(mcp_path, encoding="utf-8") as f:
                    doc = json.load(f)
            except (OSError, ValueError):
                doc = {}
            servers = dict(doc.get("mcpServers") or {})
            for name in before["mcp"]:
                servers.pop(name, None)
            mcp = c.get("mcp") or []
            for m in mcp:
                e = {"type": "http", "url": m["url"]}
                if m.get("headers"):
                    e["headers"] = m["headers"]
                servers[m["name"]] = e
            if mcp or before["mcp"]:
                doc["mcpServers"] = servers
                write(mcp_path, json.dumps(doc, indent=2) + "\n", 0o600)
            apis = [a for a in c.get("apis") or [] if safe(a.get("name"))]
            for name in before["apis"]:
                if safe(name) and not any(a["name"] == name for a in apis):
                    try:
                        os.remove(os.path.join(dir, apis_dir, f"{name}.openapi.json"))
                    except OSError:
                        pass
            for a in apis:
                spec = dict(a.get("spec") or {})
                spec.setdefault("servers", [{"url": a["url"]}])
                write(os.path.join(dir, apis_dir, f"{a['name']}.openapi.json"), json.dumps(spec, indent=2) + "\n")
            now = {"skills": [s["name"] for s in skills], "mcp": [m["name"] for m in mcp], "apis": [a["name"] for a in apis]}
            write(manifest, json.dumps(now, indent=2) + "\n")
            return now

        return await asyncio.to_thread(run)

    async def refresh_config(self):
        """Fetches the configuration again (the SDK does it on ``config.updated``)."""
        c = await self.conn.call("get_config")
        c = {k: v for k, v in c.items() if k not in ("type", "rid", "ok")}
        await self._set_config(c)
        return self.config

    async def _refresh_config_quietly(self):
        try:
            await self.refresh_config()
        except AgentIMError as e:
            self.log(f"config: {e}")

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
        (docs/en/design/cluster.md)."""
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
        """Stops: closes the connection; no handler runs after this."""
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
        await self._set_config(w.get("config"))
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
        if f.get("type") == "event":
            self._form_answered(f["event"])
        if f.get("type") == "config.updated":
            asyncio.ensure_future(self._refresh_config_quietly())
            return
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
                        self._form_answered(item["event"])
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
        ctx = Ctx(self, ev, item.get("trace"))
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

    def _form_answered(self, ev):
        data = ev.get("data") or {}
        if ev.get("type") == "card.response" and data.get("msg_id") in self.forms:
            fut = self.forms.pop(data["msg_id"])
            if not fut.done():
                fut.set_result(data.get("values") or {})

    def _side(self, s):
        rid = (s.get("data") or {}).get("request_id")
        if s.get("type", "").startswith("request.") and rid in self.waiting:
            fut = self.waiting.pop(rid)
            if not fut.done():
                fut.set_result(s["data"])

    # Operations.
    async def call(self, op, **args):
        """Any protocol operation (docs/en/testing.md)."""
        return await self.conn.call(op, **args)

    async def send(self, conv_id, text, reply_to=None, to=None, topics=None, intent=None,
                   refs=None, sources=None, client_msg_id=None, trace=None):
        """Sends a message, with an optional envelope (`to`, `topics`, `intent`, `refs`)
        and `sources`, in the trace `trace` (a W3C traceparent) when given. Under
        approval it becomes a draft (`draft_id`).
        """
        return await self.call(
            "send", conv_id=conv_id, text=text, client_msg_id=client_msg_id or str(uuid.uuid4()),
            reply_to=reply_to, to=to, topics=topics, intent=intent, refs=refs, sources=sources, trace=trace,
        )

    async def ask_form(self, conv_id, title, fields, intro=None, submit="Send", to=None, reply_to=None,
                       timeout=1800.0):
        """Sends a form card (docs/en/design/chat-components.md) and waits for its answers; ``to`` limits who may
        answer. Returns the values; raises ``asyncio.TimeoutError`` after ``timeout`` seconds."""
        blocks = ([{"type": "text", "text": intro}] if intro else []) + [
            {"type": "form", "id": "form", "submit": submit, "fields": fields}]
        card = {"title": title, "once": True, "blocks": blocks}
        if to:
            card["to"] = to
        sent = await self.send_card(conv_id, f"{title} (please fill in the form)", card, reply_to=reply_to)
        fut = asyncio.get_running_loop().create_future()
        self.forms[sent["msg_id"]] = fut
        try:
            return await asyncio.wait_for(fut, timeout)
        finally:
            self.forms.pop(sent["msg_id"], None)

    async def send_card(self, conv_id, text, card, reply_to=None):
        """A card (docs/en/design/agents.md); ``text`` is what clients without cards show."""
        return await self.call("send", conv_id=conv_id, text=text, kind="agentim.card", payload=card,
                               client_msg_id=str(uuid.uuid4()), reply_to=reply_to)

    async def upload_file(self, conv_id, name, data, mime=None):
        """Uploads a whole file (at most 64 MiB) to a conversation the agent is
        in; the server removes EXIF and GPS and makes an image's thumbnail.
        Returns the file (file_id, kind, name, size, ...)."""
        q = {"conv_id": conv_id, "name": name}
        if mime:
            q["mime"] = mime
        url = f"{_http_base(self.url)}/api/files?{urlencode(q)}"
        headers = {"authorization": f"Bearer {self.key}", "content-type": "application/octet-stream"}

        def post():
            req = urllib.request.Request(url, data=bytes(data), headers=headers, method="POST")
            try:
                with urllib.request.urlopen(req) as r:
                    return json.loads(r.read())
            except urllib.error.HTTPError as e:
                try:
                    err = json.loads(e.read())["error"]
                except Exception:
                    err = {"code": "unavailable", "message": f"HTTP {e.code}"}
                raise AgentIMError(err.get("code", "unavailable"), err.get("message", "")) from None

        return await asyncio.to_thread(post)

    async def send_file(self, conv_id, name, data, mime=None, caption=None, reply_to=None, client_msg_id=None):
        """Uploads a file and sends it as an image or file message."""
        f = await self.upload_file(conv_id, name, data, mime)
        image = f["kind"] == "image"
        payload = {"file_id": f["file_id"]}
        if caption:
            payload["caption"] = caption
        sent = await self.call(
            "send", conv_id=conv_id, text="[Image]" if image else f"[File] {f['name']}",
            kind="image" if image else "file", payload=payload,
            client_msg_id=client_msg_id or str(uuid.uuid4()), reply_to=reply_to,
        )
        return {**sent, "file": f}

    async def file_url(self, conv_id, file_id, variant=None):
        """A link (valid 5 minutes, absolute) to read a file of a message;
        ``variant``: "original" (default) or "thumb"."""
        r = await self.call("file_url", conv_id=conv_id, file_id=file_id, variant=variant)
        return {**r, "url": urljoin(_http_base(self.url) + "/", r["url"])}

    async def stream(self, conv_id, reply_to=None, trace=None):
        """Starts a streamed reply; append text, then end it. People see it as it is
        written and can stop it.
        """
        r = await self.call(
            "stream_start", conv_id=conv_id, client_msg_id=str(uuid.uuid4()), reply_to=reply_to, trace=trace
        )
        return Stream(self, r["msg_id"])

    async def progress(self, conv_id, kind, text, msg_id=None, data=None, trace=None):
        """Reports progress in the side channel (`thinking`, `plan`, `step`, `tool_call`, `tool_result`, `error`), optionally
        tied to a message.
        """
        return await self.call("progress", conv_id=conv_id, kind=kind, text=text, msg_id=msg_id, data=data, trace=trace)

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
        """Hands the conversation to a person (its owner, or `to`) with a note; the agent
        stays quiet until it is handed back.
        """
        return await self.call("handoff", conv_id=conv_id, note=note, to=to)

    async def history(self, conv_id, after_seq=None, before_seq=None, limit=None):
        """Reads the conversation's events the agent may see, by seq (`after_seq` or
        `before_seq`, up to `limit`).
        """
        return await self.call("history", conv_id=conv_id, after_seq=after_seq, before_seq=before_seq, limit=limit)

    async def context(self, conv_id, max_tokens=None):
        """The conversation for the model: stored summaries and recent messages within
        `max_tokens`.
        """
        return await self.call("context", conv_id=conv_id, max_tokens=max_tokens)

    async def search(self, query, conv_id=None, limit=None):
        """Searches the messages the agent may see, in one conversation or all."""
        return await self.call("search", query=query, conv_id=conv_id, limit=limit)

    async def put_summary(self, conv_id, from_seq, to_seq, text):
        """Stores a summary of a range of seqs, used by `context` in place of those
        messages.
        """
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
        """Claims a request (`intent: request`) for this agent; the first claim wins, the
        others get `conflict`.
        """
        return await self.call("claim", msg_id=msg_id)

    async def update_task(self, conv_id, state, note=None):
        """Moves a task channel to a state (`submitted`, `working`, `input_required`, `completed`, `failed`, `canceled`) with a note."""
        return await self.call("update_task", conv_id=conv_id, state=state, note=note)
