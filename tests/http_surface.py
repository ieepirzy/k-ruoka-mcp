#!/usr/bin/env python3
"""Wire-level smoke test for the unified Streamable HTTP transport.

It starts the real binary, checks the Kubernetes-friendly health endpoint, then performs an
MCP initialize + tools/list over HTTP. No grocery provider tool is called, so this stays
hermetic and does not launch Chrome or touch an external site.
"""

import http.client
import json
import subprocess
import sys
import time

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/k-ruoka-mcp"
HOST = "127.0.0.1"
PORT = 18080
MCP_PATH = "/mcp"

p = subprocess.Popen(
    [BINARY, "serve-http", "--bind", f"{HOST}:{PORT}"],
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    text=True,
)


def request(method, path, body=None, session_id=None):
    connection = http.client.HTTPConnection(HOST, PORT, timeout=10)
    headers = {}
    payload = None
    if body is not None:
        payload = json.dumps(body)
        headers["Content-Type"] = "application/json"
        headers["Accept"] = "application/json, text/event-stream"
    if session_id:
        headers["Mcp-Session-Id"] = session_id
    connection.request(method, path, body=payload, headers=headers)
    response = connection.getresponse()
    raw = response.read().decode("utf-8")
    result = response.status, dict(response.getheaders()), raw
    connection.close()
    return result


def rpc(method, request_id, params=None, session_id=None):
    body = {"jsonrpc": "2.0", "id": request_id, "method": method}
    if params is not None:
        body["params"] = params
    status, headers, raw = request("POST", MCP_PATH, body, session_id)
    if status != 200:
        raise RuntimeError(f"HTTP {status} for {method}: {raw[:500]}")
    content_type = headers.get("content-type", "")
    if "application/json" not in content_type:
        raise RuntimeError(f"unexpected {method} content type {content_type!r}: {raw[:500]}")
    return headers, json.loads(raw)


try:
    for _ in range(100):
        try:
            status, _, body = request("GET", "/healthz")
            if status == 200 and body == "ok":
                break
        except OSError:
            pass
        if p.poll() is not None:
            raise RuntimeError(f"HTTP server exited early with status {p.returncode}")
        time.sleep(0.05)
    else:
        raise RuntimeError("HTTP server never became healthy")

    headers, initialized = rpc(
        "initialize",
        1,
        {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": {"name": "http-surface-test", "version": "0"},
        },
    )
    info = initialized["result"]["serverInfo"]
    if info.get("name") != "finland-grocery-mcp":
        raise RuntimeError(f"unexpected HTTP MCP identity: {info}")

    session_id = headers.get("mcp-session-id") or headers.get("Mcp-Session-Id")
    if not session_id:
        raise RuntimeError(f"initialize did not return Mcp-Session-Id: {headers}")

    notification = {
        "jsonrpc": "2.0",
        "method": "notifications/initialized",
    }
    status, _, raw = request("POST", MCP_PATH, notification, session_id)
    if status not in (200, 202, 204):
        raise RuntimeError(f"initialized notification failed: HTTP {status}: {raw[:500]}")

    _, tools_reply = rpc("tools/list", 2, {}, session_id)
    tools = tools_reply["result"]["tools"]
    names = {tool["name"] for tool in tools}
    required = {
        "search_products",
        "get_personal_offers",
        "search_s_kaupat_products",
        "search_alko_products",
    }
    missing = sorted(required - names)
    if missing:
        raise RuntimeError(f"HTTP MCP is missing representative tools: {missing}")

    print(f"ok: Streamable HTTP initialized and exposed {len(tools)} tools")
finally:
    p.terminate()
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait(timeout=5)
