"""Offline check of write_files and mcp_servers (docs/en/design/assistants.md), as
sdk/agent-node/files.test.mjs: MCP servers merged into .mcp.json next to the
developer's own (owner-only), API documents get their server address, and
only what the SDK wrote before is replaced or removed.

    python3 sdk/agent-python/files_test.py
"""

import asyncio
import json
import os
import shutil
import sys
import tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from agentim_agent import Agent  # noqa: E402

failures = 0


def check(cond, what):
    global failures
    print(f"{'ok  ' if cond else 'FAIL'} {what}")
    if not cond:
        failures += 1


async def main():
    d = tempfile.mkdtemp(prefix="agentim-files-")

    def read(p):
        with open(os.path.join(d, p), encoding="utf-8") as f:
            return json.load(f)

    agent = Agent("http://127.0.0.1:9", "k")
    try:
        await agent.write_files(dir=d)
        check(False, "nothing to write before the configuration arrives")
    except RuntimeError:
        check(True, "nothing to write before the configuration arrives")
    with open(os.path.join(d, ".mcp.json"), "w") as f:
        json.dump({"mcpServers": {"mine": {"type": "stdio", "command": "x"}}, "other": 1}, f)
    agent.config = {
        "version": "v1", "role": None,
        "skills": [{"name": "notes", "description": "Notes", "instructions": "Write notes.",
                    "markdown": "---\nname: notes\ndescription: Notes\n---\nWrite notes.\n", "files": []},
                   {"name": "../evil", "description": "x", "instructions": "x", "files": []}],
        "mcp": [{"name": "shop", "url": "https://mcp.example.com/mcp", "writes": "read_only", "version": 1,
                 "headers": {"Authorization": "Bearer s"}}],
        "apis": [{"name": "orders", "url": "https://api.example.com", "writes": "read_only", "version": 1,
                  "spec": {"openapi": "3.0.3", "paths": {}}}],
    }
    check(agent.mcp_servers() == [{"name": "shop", "url": "https://mcp.example.com/mcp", "headers": {"Authorization": "Bearer s"}}],
          "mcp_servers gives name, url, headers")
    r = await agent.write_files(dir=d)
    check(r == {"skills": ["notes"], "mcp": ["shop"], "apis": ["orders"]}, "it reports what it wrote; unsafe names are skipped")
    check(not os.path.exists(os.path.join(d, ".claude", "evil")), "nothing is written outside the skills folder")
    with open(os.path.join(d, ".claude/skills/notes/SKILL.md"), encoding="utf-8") as f:
        check(f.read().startswith("---\nname: notes"), "the skill is written as SKILL.md")
    m = read(".mcp.json")
    check(m["mcpServers"]["shop"]["type"] == "http" and m["mcpServers"]["shop"]["headers"]["Authorization"] == "Bearer s",
          "the MCP server is in .mcp.json with its header")
    check(m["mcpServers"]["mine"]["command"] == "x" and m["other"] == 1, "the developer's own entries stay")
    if os.name != "nt":
        check(os.stat(os.path.join(d, ".mcp.json")).st_mode & 0o777 == 0o600, ".mcp.json is readable by its owner only")
    spec = read(".agentim/apis/orders.openapi.json")
    check(spec["servers"][0]["url"] == "https://api.example.com", "the API document gets its server address")
    agent.config = dict(agent.config, version="v2", skills=[], mcp=[], apis=[])
    await agent.write_files(dir=d)
    check(not os.path.exists(os.path.join(d, ".claude/skills/notes")), "a skill no longer given is removed")
    m2 = read(".mcp.json")
    check("shop" not in m2["mcpServers"] and m2["mcpServers"]["mine"]["command"] == "x", "its MCP server is removed, the developer's stays")
    check(not os.path.exists(os.path.join(d, ".agentim/apis/orders.openapi.json")), "the API document is removed")
    os.makedirs(os.path.join(d, ".claude/skills/handmade"))
    await agent.write_files(dir=d)
    check(os.path.exists(os.path.join(d, ".claude/skills/handmade")), "the developer's own skill stays")
    shutil.rmtree(d, ignore_errors=True)


asyncio.run(main())
if failures:
    print(f"{failures} failed")
    sys.exit(1)
print("files: all passed")
