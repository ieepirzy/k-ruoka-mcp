#!/usr/bin/env python3
"""Wire-level smoke test for the unified Streamable HTTP transport.

It starts the real binary, checks the Kubernetes-friendly health endpoint, then performs an
MCP initialize + tools/list over HTTP. No grocery provider tool is called, so this stays
hermetic and does not launch Chrome or touch an external site.

It also checks the two guards a non-loopback deployment relies on: with K_RUOKA_HTTP_TOKEN
set, /mcp refuses a request without the bearer token, and a Host header not passed with
--allowed-host is refused (rmcp's DNS-rebinding guard).
"""

import http.client
import json
import os
import subprocess
import sys
import time

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/k-ruoka-mcp"
HOST = "127.0.0.1"
PORT = 18080
MCP_PATH = "/mcp"
TOKEN = "http-surface-test-token"  # noqa: S105 - a throwaway for a local test server
AUTH = {"Authorization": f"Bearer {TOKEN}"}

p = subprocess.Popen(
    [BINARY, "serve-http", "--bind", f"{HOST}:{PORT}", "--allowed-host", "k-ruoka"],
    stdout=subprocess.DEVNULL,
    stderr=subprocess.DEVNULL,
    text=True,
    env={**os.environ, "K_RUOKA_HTTP_TOKEN": TOKEN},
)


def request(method, path, body=None, session_id=None, extra=None):
    connection = http.client.HTTPConnection(HOST, PORT, timeout=10)
    headers = dict(AUTH if extra is None else extra)
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
    if "application/json" in content_type:
        return headers, json.loads(raw)
    if "text/event-stream" in content_type:
        # The spec lets a server answer any request as an SSE stream; the reply is the
        # data event carrying our id (priming events have empty data).
        for line in raw.splitlines():
            data = line.removeprefix("data:").strip() if line.startswith("data:") else ""
            if data:
                message = json.loads(data)
                if message.get("id") == request_id:
                    return headers, message
        raise RuntimeError(f"no reply to {method} in the event stream: {raw[:500]}")
    raise RuntimeError(f"unexpected {method} content type {content_type!r}: {raw[:500]}")


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
    if status not in {200, 202, 204}:
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

    probe = {"jsonrpc": "2.0", "id": 9, "method": "tools/list", "params": {}}
    json_headers = {
        "Content-Type": "application/json",
        "Accept": "application/json, text/event-stream",
    }
    status, _, _ = request("POST", MCP_PATH, probe, session_id, extra=json_headers)
    if status != 401:
        raise RuntimeError(f"a request without the bearer token got HTTP {status}, not 401")
    wrong = {**json_headers, "Authorization": "Bearer wrong"}
    status, _, _ = request("POST", MCP_PATH, probe, session_id, extra=wrong)
    if status != 401:
        raise RuntimeError(f"a wrong bearer token got HTTP {status}, not 401")
    for host, expected_ok in (("k-ruoka", True), ("evil.example", False)):
        connection = http.client.HTTPConnection(HOST, PORT, timeout=10)
        connection.request(
            "POST",
            MCP_PATH,
            body=json.dumps(probe),
            headers={**json_headers, **AUTH, "Host": host, "Mcp-Session-Id": session_id},
        )
        status = connection.getresponse().status
        connection.close()
        if (status == 200) != expected_ok:
            raise RuntimeError(f"Host {host!r} got HTTP {status}")

    print(f"ok: Streamable HTTP initialized and exposed {len(tools)} tools")
finally:
    p.terminate()
    try:
        p.wait(timeout=10)
    except subprocess.TimeoutExpired:
        p.kill()
        p.wait(timeout=5)
