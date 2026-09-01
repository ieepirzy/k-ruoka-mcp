#!/usr/bin/env python3
"""Wire-level smoke test for the unified grocery MCP tool catalogue.

This deliberately does not call any tools. It proves the composed rmcp router is the one
actually exposed by `serve-grocery` without depending on K-Ruoka, S-Kaupat or Alko being
reachable from CI.
"""

import json
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/k-ruoka-mcp"

EXPECTED = sorted(
    [
        # K-Ruoka catalogue + account/cart surface.
        "search_products",
        "search_stores",
        "set_default_store",
        "get_personal_offers",
        "get_cart",
        "add_to_cart",
        "update_cart_item",
        "remove_from_cart",
        "clear_cart",
        "auth_status",
        "start_login",
        "login_status",
        "cancel_login",
        # S-Kaupat read-only catalogue.
        "search_s_kaupat_products",
        "search_s_kaupat_stores",
        # Alko read-only catalogue.
        "search_alko_products",
        "search_alko_stores",
    ]
)

p = subprocess.Popen(
    [BINARY, "serve-grocery"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL,
    text=True,
    bufsize=1,
)
request_id = 0


def send(message):
    p.stdin.write(json.dumps(message) + "\n")
    p.stdin.flush()


def call(method, params=None):
    global request_id
    request_id += 1
    message = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        message["params"] = params
    send(message)
    while True:
        line = p.stdout.readline()
        if not line:
            raise RuntimeError("unified grocery MCP closed unexpectedly")
        reply = json.loads(line)
        if reply.get("id") == request_id:
            return reply


try:
    initialized = call(
        "initialize",
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "grocery-surface-test", "version": "0"},
        },
    )
    info = initialized["result"]["serverInfo"]
    if info.get("name") != "finland-grocery-mcp":
        raise SystemExit(f"unexpected server identity: {info}")

    send({"jsonrpc": "2.0", "method": "notifications/initialized"})
    tools = sorted(tool["name"] for tool in call("tools/list", {})["result"]["tools"])
    if tools != EXPECTED:
        missing = sorted(set(EXPECTED) - set(tools))
        extra = sorted(set(tools) - set(EXPECTED))
        raise SystemExit(f"unified tool surface mismatch; missing={missing}, extra={extra}")

    print(f"ok: finland-grocery-mcp exposes all {len(EXPECTED)} expected tools")
finally:
    if p.stdin:
        p.stdin.close()
    p.wait(timeout=30)
