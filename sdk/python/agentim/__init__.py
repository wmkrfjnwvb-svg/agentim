"""AgentIM server SDK for Python (3.9+): a thin client for the open API /v1
(docs/07-open-api.md). Standard library only."""

from __future__ import annotations

import json
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from typing import Any, Optional

__all__ = ["AgentIM", "AgentIMError"]


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
                idempotency_key: Optional[str] = None) -> Any:
        """Low-level call. POSTs get an Idempotency-Key reused across retries."""
        headers = {"authorization": f"Bearer {self.api_key}"}
        data = None
        if body is not None:
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

    # ---- tokens
    def issue_token(self, user_id, device_name=None, platform=None, ttl_seconds=None):
        return self.request("POST", f"/v1/users/{_q(user_id)}/tokens",
                            {"device_name": device_name, "platform": platform, "ttl_seconds": ttl_seconds})

    def revoke_tokens(self, user_id):
        return self.request("DELETE", f"/v1/users/{_q(user_id)}/tokens")

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
