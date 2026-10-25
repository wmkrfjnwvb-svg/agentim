"""The first five minutes, on a real install (docs/12 §1): start `agentim`
with no arguments in an empty folder, wait for /readyz, make an open API
key with the same program, and send and read back a first message.

    python3 deploy/smoke/first_message.py path/to/agentim

Runs on Linux, macOS and Windows (CI runs it after each install)."""
import json
import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "..", "sdk", "python"))
from agentim import AgentIM  # noqa: E402


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def main(binary):
    binary = os.path.abspath(binary)
    work = tempfile.mkdtemp(prefix="agentim-first-")
    port = free_port()
    url = f"http://127.0.0.1:{port}"
    env = {k: v for k, v in os.environ.items() if k != "AGENTIM_MASTER_KEY"}
    server = subprocess.Popen([binary, "--addr", f"127.0.0.1:{port}"], cwd=work, env=env,
                              stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    try:
        lines = []
        deadline = time.time() + 60
        while time.time() < deadline:
            line = server.stdout.readline()
            if not line:
                break
            lines.append(line.rstrip())
            if "/console/setup?token=" in line:
                break
        out = "\n".join(lines)
        assert f"listening on {url}" in out, out
        assert "Created the master key" in out, out
        assert os.path.exists(os.path.join(work, "agentim.db.key")), os.listdir(work)
        for _ in range(100):
            try:
                with urllib.request.urlopen(f"{url}/readyz", timeout=2) as r:
                    if r.status == 200:
                        break
            except OSError:
                time.sleep(0.1)
        else:
            raise SystemExit("not ready")
        made = subprocess.run([binary, "api-key", "create", "--name", "first",
                               "--scopes", "users:write,conversations:write,messages:write,messages:read"],
                              cwd=work, env=env, capture_output=True, text=True, check=True)
        key = json.loads(made.stdout)["key"]
        im = AgentIM(url, key)
        im.batch_users([{"id": "ana"}, {"id": "ben"}])
        conv = im.create_conversation("ana", "group", "first", members=["ben"])
        sent = im.send_message(conv["id"], "ana", "hello from a fresh install")
        page = im.history(conv["id"], limit=50)
        texts = [e["msg"]["content"] for e in page["events"] if e.get("msg")]
        assert "hello from a fresh install" in texts, page
        print(f"ok: first message {sent['msg_id']} in {conv['id']}")
    finally:
        server.terminate()
        try:
            server.wait(timeout=10)
        except subprocess.TimeoutExpired:
            server.kill()


if __name__ == "__main__":
    main(sys.argv[1])
