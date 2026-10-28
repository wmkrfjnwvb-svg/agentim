"""AgentIM server SDK for Python (3.9+): a thin client for the open API /v1
(docs/07-open-api.md). Standard library only."""

from __future__ import annotations

import base64
import hashlib
import hmac
import json
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from typing import Any, Optional

__all__ = ["AgentIM", "AgentIMError", "create_user_token"]


def _b64u(b: bytes) -> str:
    return base64.urlsafe_b64encode(b).rstrip(b"=").decode()


def create_user_token(kid: str, secret: str, user_id: str, expires_in: int = 3600) -> str:
    """Signs a client token for one of your users (docs/13 §4.2): an HS256
    JWT with the signing key from `agentim signing-key create`. Do this in
    your backend after your own login and give the token to your app; no
    call to AgentIM is made. `expires_in`: seconds (at most
    `auth.max_token_days`)."""
    if not kid or not secret:
        raise ValueError("kid and secret are required")
    iat = int(time.time())
    head = _b64u(json.dumps({"alg": "HS256", "typ": "JWT", "kid": kid}, separators=(",", ":")).encode())
    body = _b64u(json.dumps({"sub": user_id, "iat": iat, "exp": iat + expires_in}, separators=(",", ":")).encode())
    sig = hmac.new(secret.encode(), f"{head}.{body}".encode(), hashlib.sha256).digest()
    return f"{head}.{body}.{_b64u(sig)}"


class AgentIMError(Exception):
    """An error answered by the server (RFC 9457 problem details)."""

    def __init__(self, status: int, code: str, detail: str):
        super().__init__(f"{status} {code}: {detail}")
        self.status = status
        self.code = code
        self.detail = detail


def _q(s: str) -> str:
    return urllib.parse.quote(s, safe="")


def _clean(d: dict) -> dict:
    return {k: v for k, v in d.items() if v is not None}


class AgentIM:
    def __init__(self, base_url: str, api_key: str, retries: int = 3, timeout: float = 30.0):
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        self.retries = retries
        self.timeout = timeout

    def request(self, method: str, path: str, body: Optional[dict] = None,
                idempotency_key: Optional[str] = None, raw: Optional[bytes] = None) -> Any:
        """Low-level call. POSTs get an Idempotency-Key reused across retries."""
        headers = {"authorization": f"Bearer {self.api_key}"}
        data = None
        if raw is not None:
            headers["content-type"] = "application/octet-stream"
            data = raw
        elif body is not None:
            headers["content-type"] = "application/json"
            data = json.dumps(_clean(body)).encode()
        if method == "POST":
            headers["idempotency-key"] = idempotency_key or str(uuid.uuid4())
        retryable = method in ("GET", "PUT", "POST")
        attempt = 0
        while True:
            req = urllib.request.Request(self.base_url + path, data=data, method=method, headers=headers)
            try:
                with urllib.request.urlopen(req, timeout=self.timeout) as r:
                    text = r.read().decode()
                    return json.loads(text) if text else {}
            except urllib.error.HTTPError as e:
                text = e.read().decode()
                if retryable and attempt < self.retries and e.code in (429, 503):
                    after = e.headers.get("retry-after")
                    time.sleep(float(after) if after and after.isdigit() else _backoff(attempt))
                    attempt += 1
                    continue
                try:
                    p = json.loads(text)
                except ValueError:
                    p = {}
                raise AgentIMError(e.code, p.get("code", "error"), p.get("detail", text)) from None
            except urllib.error.URLError:
                if retryable and attempt < self.retries:
                    time.sleep(_backoff(attempt))
                    attempt += 1
                    continue
                raise

    # ---- login (client tokens are signed with create_user_token)
    def revoke_tokens(self, user_id):
        """Logs a person out everywhere: client tokens issued until now are refused."""
        return self.request("DELETE", f"/v1/users/{_q(user_id)}/tokens")

    # ---- agent keys
    def issue_agent_key(self, agent_id, revoke_others=False):
        return self.request("POST", f"/v1/agents/{_q(agent_id)}/keys", {"revoke_others": revoke_others})

    def revoke_agent_keys(self, agent_id):
        return self.request("DELETE", f"/v1/agents/{_q(agent_id)}/keys")

    def revoke_agent_key(self, agent_id, device_id):
        return self.request("DELETE", f"/v1/agents/{_q(agent_id)}/keys/{_q(device_id)}")

    # ---- agents (docs/13 §4.1)
    def list_agents(self, user_id):
        return self.request("GET", f"/v1/users/{_q(user_id)}/agents")

    def stop_agent(self, agent_id, reason=None):
        return self.request("POST", f"/v1/agents/{_q(agent_id)}:stop",
                            {"reason": reason} if reason is not None else None)

    def resume_agent(self, agent_id):
        return self.request("POST", f"/v1/agents/{_q(agent_id)}:resume")

    def transfer_agent(self, agent_id, owner, rotate_keys=False):
        return self.request("POST", f"/v1/agents/{_q(agent_id)}:transfer",
                            {"owner": owner, "rotate_keys": rotate_keys})

    def destroy_agent(self, agent_id):
        return self.request("DELETE", f"/v1/agents/{_q(agent_id)}")

    def export_agent(self, agent_id, with_conversations=False):
        flag = "true" if with_conversations else "false"
        return self.request("GET", f"/v1/agents/{_q(agent_id)}/export?with_conversations={flag}")

    def import_agent(self, package, owner, name=None, allow_unverified=False):
        """``allow_unverified``: accept a package this deployment cannot verify
        (unsigned, or from another deployment)."""
        return self.request(
            "POST",
            "/v1/agents:import",
            {"package": package, "owner": owner, "name": name, "allow_unverified": allow_unverified},
        )

    # ---- devices
    def list_devices(self, user_id):
        return self.request("GET", f"/v1/users/{_q(user_id)}/devices")

    def logout_device(self, user_id, device_id):
        return self.request("DELETE", f"/v1/users/{_q(user_id)}/devices/{_q(device_id)}")

    # ---- compliance (docs/04 §4.2)
    def delete_user(self, user_id):
        """Deletes a person's account; returns the erasure's progress."""
        return self.request("DELETE", f"/v1/users/{_q(user_id)}")

    def erasure(self, user_id):
        return self.request("GET", f"/v1/users/{_q(user_id)}/erasure")

    def export_user(self, user_id, after=None):
        """The person's own data, one object per line (NDJSON); ``after``
        continues from a line's ``cursor``."""
        q = f"?after={_q(after)}" if after else ""
        req = urllib.request.Request(f"{self.base_url}/v1/users/{_q(user_id)}/export{q}",
                                     headers={"authorization": f"Bearer {self.api_key}"})
        try:
            with urllib.request.urlopen(req, timeout=self.timeout) as r:
                return [json.loads(line) for line in r.read().decode().splitlines() if line]
        except urllib.error.HTTPError as e:
            text = e.read().decode()
            try:
                p = json.loads(text)
            except ValueError:
                p = {}
            raise AgentIMError(e.code, p.get("code", "error"), p.get("detail", text)) from None

    # ---- moderation
    def reports(self, state="open", limit=50, offset=0):
        return self.request("GET", f"/v1/reports?state={_q(state)}&limit={limit}&offset={offset}")

    def get_report(self, report_id):
        return self.request("GET", f"/v1/reports/{_q(report_id)}")

    def resolve_report(self, report_id, action, reason, ground=None, automated=None, days=None):
        return self.request("POST", f"/v1/reports/{_q(report_id)}:resolve",
                            {"action": action, "reason": reason, "ground": ground,
                             "automated": automated, "days": days})

    def silence_member(self, conv_id, user_id, seconds, reason=None):
        """The member cannot send, edit or stream in the conversation for ``seconds``."""
        return self.request("POST", f"/v1/conversations/{_q(conv_id)}/members/{_q(user_id)}:silence",
                            {"seconds": seconds, "reason": reason})

    def unsilence_member(self, conv_id, user_id):
        return self.request("POST", f"/v1/conversations/{_q(conv_id)}/members/{_q(user_id)}:unsilence")

    # ---- users
    def upsert_user(self, id, **attrs):
        return self.request("PUT", f"/v1/users/{_q(id)}", attrs)

    def get_user(self, id):
        return self.request("GET", f"/v1/users/{_q(id)}")

    def update_user(self, id, **attrs):
        return self.request("PATCH", f"/v1/users/{_q(id)}", attrs)

    def deactivate_user(self, id):
        return self.request("POST", f"/v1/users/{_q(id)}:deactivate")

    def reactivate_user(self, id):
        return self.request("POST", f"/v1/users/{_q(id)}:reactivate")

    def batch_users(self, users):
        return self.request("POST", "/v1/users:batch", {"users": users})

    def user_conversations(self, id):
        return self.request("GET", f"/v1/users/{_q(id)}/conversations")

    # ---- conversations (`as_` is a user id or "system")
    def create_conversation(self, as_, kind, title, members=None, owner=None, fields=None):
        return self.request("POST", "/v1/conversations",
                            {"as": as_, "kind": kind, "title": title, "members": members or [],
                             "owner": owner, "fields": fields})

    def get_conversation(self, id):
        return self.request("GET", f"/v1/conversations/{_q(id)}")

    def update_conversation(self, id, as_, title=None, fields=None):
        return self.request("PATCH", f"/v1/conversations/{_q(id)}", {"as": as_, "title": title, "fields": fields})

    def dissolve_conversation(self, id, as_):
        return self.request("DELETE", f"/v1/conversations/{_q(id)}?as={_q(as_)}")

    def add_members(self, id, as_, users):
        return self.request("POST", f"/v1/conversations/{_q(id)}/members", {"as": as_, "users": users})

    def remove_member(self, id, user, as_):
        return self.request("DELETE", f"/v1/conversations/{_q(id)}/members/{_q(user)}?as={_q(as_)}")

    def set_role(self, id, user, role, as_):
        return self.request("PATCH", f"/v1/conversations/{_q(id)}/members/{_q(user)}", {"as": as_, "role": role})

    # ---- messages
    @staticmethod
    def _msg(as_, text, kind=None, payload=None, client_msg_id=None, reply_to=None, to_user=None,
             to=None, topics=None, intent=None, refs=None, sources=None):
        return {"as": as_, "text": text, "kind": kind, "payload": payload,
                "client_msg_id": client_msg_id, "reply_to": reply_to, "to_user": to_user,
                "to": to, "topics": topics, "intent": intent, "refs": refs, "sources": sources}

    def upload_file(self, conv_id, as_, name, data, mime=None):
        """Uploads a whole file (at most 64 MiB); the server removes EXIF and
        GPS and makes a thumbnail. Returns the file (file_id, kind, ...)."""
        q = {"as": as_, "name": name}
        if mime:
            q["mime"] = mime
        return self.request("POST", f"/v1/conversations/{_q(conv_id)}/files?{urllib.parse.urlencode(q)}", raw=bytes(data))

    def send_file(self, conv_id, as_, name, data, mime=None, caption=None, reply_to=None):
        """Uploads a file and sends it as an image or file message."""
        f = self.upload_file(conv_id, as_, name, data, mime)
        kind = "image" if f["kind"] == "image" else "file"
        payload = {"file_id": f["file_id"]}
        if caption:
            payload["caption"] = caption
        text = "[Image]" if kind == "image" else f"[File] {f['name']}"
        sent = self.send_message(conv_id, as_, text, kind=kind, payload=payload, reply_to=reply_to)
        sent["file"] = f
        return sent

    def file_url(self, conv_id, file_id, as_=None, variant=None):
        """A link (valid 5 minutes) to read a file; variant is original or thumb."""
        q = {k: v for k, v in {"as": as_, "variant": variant}.items() if v}
        return self.request("GET", f"/v1/conversations/{_q(conv_id)}/files/{_q(file_id)}?{urllib.parse.urlencode(q)}")

    def send_message(self, conv_id, as_, text, **kw):
        return self.request("POST", f"/v1/conversations/{_q(conv_id)}/messages", self._msg(as_, text, **kw))

    def send_to_user(self, user_id, as_, text, **kw):
        return self.request("POST", f"/v1/users/{_q(user_id)}/messages", self._msg(as_, text, **kw))

    def batch_send(self, as_, to, text, kind=None, payload=None, client_msg_id=None):
        return self.request("POST", "/v1/messages:batch-send",
                            {"as": as_, "to": to, "text": text, "kind": kind, "payload": payload,
                             "client_msg_id": client_msg_id})

    def edit_message(self, id, as_, text, payload=None):
        return self.request("PATCH", f"/v1/messages/{_q(id)}", {"as": as_, "text": text, "payload": payload})

    def recall_message(self, id, as_):
        return self.request("POST", f"/v1/messages/{_q(id)}:recall", {"as": as_})

    def history(self, conv_id, after_seq=0, limit=50):
        return self.request("GET", f"/v1/conversations/{_q(conv_id)}/messages?after_seq={after_seq}&limit={limit}")

    def mark_read(self, conv_id, as_, seq):
        return self.request("POST", f"/v1/conversations/{_q(conv_id)}/read", {"as": as_, "seq": seq})


def _backoff(attempt: int) -> float:
    return min(0.2 * 2 ** attempt, 5.0)
