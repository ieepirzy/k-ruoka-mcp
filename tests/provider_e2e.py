#!/usr/bin/env python3
"""Live read-only smoke test for the additional Finnish catalogue providers.

Not part of the hermetic cargo test suite: it intentionally hits Alko and S-Kaupat over
real network connections, and S-Kaupat launches Chrome to discover the frontend's current
persisted GraphQL hashes.

    cargo build
    python3 tests/provider_e2e.py ./target/debug/k-ruoka-mcp

Nothing here logs in, mutates a cart, places an order, or spends money.
"""

import json
import os
import shutil
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/debug/k-ruoka-mcp"
S_PROFILE = "/tmp/k-ruoka-s-kaupat-provider-smoke"
failures = []


class Server:
    def __init__(self, subcommand, env=None):
        child_env = dict(os.environ)
        if env:
            child_env.update(env)
        self.p = subprocess.Popen(
            [BINARY, subcommand],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=None,
            text=True,
            bufsize=1,
            env=child_env,
        )
        self._id = 0
        init = self.call(
            "initialize",
            {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "provider-e2e", "version": "0"},
            },
        )
        self.server_info = init["result"]["serverInfo"]
        self.notify("notifications/initialized")

    def _send(self, msg):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def notify(self, method):
        self._send({"jsonrpc": "2.0", "method": method})

    def call(self, method, params=None):
        self._id += 1
        msg = {"jsonrpc": "2.0", "id": self._id, "method": method}
        if params is not None:
            msg["params"] = params
        self._send(msg)
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("server closed the connection unexpectedly")
            reply = json.loads(line)
            if reply.get("id") == self._id:
                return reply

    def tools(self):
        return sorted(tool["name"] for tool in self.call("tools/list", {})["result"]["tools"])

    def tool(self, name, **args):
        reply = self.call("tools/call", {"name": name, "arguments": args})
        if "error" in reply:
            return {"__error__": reply["error"]["message"]}
        result = reply["result"]
        if result.get("isError"):
            return {"__error__": result["content"][0]["text"]}
        return result.get("structuredContent") or json.loads(result["content"][0]["text"])

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=60)


def check(label, condition, detail=""):
    status = "ok  " if condition else "FAIL"
    print(f"  [{status}] {label}" + (f" -- {detail}" if detail and not condition else ""))
    if not condition:
        failures.append(label)


print("1. Alko guest catalogue")
alko = Server("serve-alko")
check(
    "Alko tool surface",
    alko.tools() == ["search_alko_products", "search_alko_stores"],
    str(alko.tools()),
)
stores = alko.tool("search_alko_stores", city="Jyväskylä")
check("Alko store lookup succeeds", "__error__" not in stores, str(stores)[:300])
check("Alko finds a Jyväskylä store", bool(stores), str(stores)[:300])
products = alko.tool("search_alko_products", query="riesling", limit=3)
check("Alko product lookup succeeds", "__error__" not in products, str(products)[:300])
if "__error__" not in products:
    hits = products.get("results", [])
    check("Alko returns products", bool(hits), str(products)[:300])
    check("Alko respects limit", len(hits) <= 3, str(len(hits)))
    check("Alko products carry SKU/name", all(p.get("sku") and p.get("name") for p in hits))
alko.close()

print("\n2. S-Kaupat persisted-query discovery + catalogue")
shutil.rmtree(S_PROFILE, ignore_errors=True)
s = Server("serve-s-kaupat", {"K_RUOKA_PROFILE": S_PROFILE})
check(
    "S-Kaupat tool surface",
    s.tools() == ["search_s_kaupat_products", "search_s_kaupat_stores"],
    str(s.tools()),
)
stores = s.tool("search_s_kaupat_stores", query="Jyväskylä")
check("S-Kaupat store lookup succeeds", "__error__" not in stores, str(stores)[:500])
if "__error__" not in stores:
    found = stores.get("results", [])
    check("S-Kaupat finds a Jyväskylä store", bool(found), str(stores)[:500])
    if found:
        store_id = found[0]["storeId"]
        products = s.tool("search_s_kaupat_products", store_id=store_id, query="maito", limit=3)
        check("S-Kaupat product lookup succeeds", "__error__" not in products, str(products)[:500])
        if "__error__" not in products:
            hits = products.get("results", [])
            check("S-Kaupat returns products", bool(hits), str(products)[:500])
            check("S-Kaupat respects limit", len(hits) <= 3, str(len(hits)))
            check(
                "S-Kaupat products carry EAN/name",
                all(p.get("ean") and p.get("name") for p in hits),
                str(hits)[:500],
            )
s.close()
shutil.rmtree(S_PROFILE, ignore_errors=True)

if failures:
    print(f"\nFAILED: {len(failures)} check(s): {', '.join(failures)}")
    raise SystemExit(1)
print("\nPASSED")
