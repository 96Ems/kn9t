#!/usr/bin/env python3
"""Bi-directional contract guard: schema <-> code.

`xtask --check` proves schema -> generated agreement, nothing more. A primitive
added straight to a `match` arm therefore leaves `API.md` silently incomplete
while every gate stays green — that is how nine host-API ops stayed invisible to
plugin authors, and how `GET /policy` and `POST /plugin/{name}/ui_event` were
missing from the HTTP reference.

This checks the other direction, on the three surfaces that have a code
counterpart:

  ops     schema/plugin.json  plugin_to_host.Request.properties.op.enum
          <->  crates/kn9t-server/src/host_api.rs      `match op` arms
  routes  schema/http.json    routes[].{method,path}
          <->  crates/kn9t-server/src/router.rs        `route()` arms + inline SSE
  hooks   schema/plugin.json  hook_payloads keys
          <->  crates/kn9t-core/src/hook.rs            HookHost methods + `tool_call`

Both directions are reported, because they fail differently: an op in the code
but not the schema is undocumented; an op in the schema but not the code is a
lie. No cargo, no build — parsing only, so it is cheap enough for a push hook.

Exit 0 when every surface agrees, 1 otherwise.
"""

import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# A hook the host sends as a wire message but which is not a `HookHost` trait
# method: the host invokes a plugin's tool through it (plugin.rs `dispatch_tool`).
EXTRA_HOOKS = {"tool_call"}

failures = []


def read(path):
    return (ROOT / path).read_text(encoding="utf-8")


def load(path):
    return json.loads(read(path))


def report(surface, schema, code):
    """Compare two sets and report both directions."""
    only_schema = sorted(schema - code)
    only_code = sorted(code - schema)
    total = len(schema | code)
    if not only_schema and not only_code:
        print(f"{surface}: {total} OK")
        return
    failures.append(surface)
    print(f"{surface}: MISMATCH ({total} union)")
    for name in only_schema:
        print(f"    in schema, missing from code : {name}")
    for name in only_code:
        print(f"    in code, missing from schema : {name}")


# ── ops ───────────────────────────────────────────────────────────────────────

def ops_from_schema():
    d = load("schema/plugin.json")
    return set(d["plugin_to_host"]["Request"]["properties"]["op"]["enum"])


def ops_from_code():
    text = read("crates/kn9t-server/src/host_api.rs")
    start = text.index("match op {")
    end = text.index("unknown host API op", start)
    return set(re.findall(r'"([A-Za-z0-9_]+)"', text[start:end]))


# ── routes ────────────────────────────────────────────────────────────────────

def normalize(*segments):
    return [s.strip('"') if re.fullmatch(r'"[^"]*"', s) else "*" for s in segments]


def route_key(method, segments):
    return f"{method.upper()} /" + "/".join(segments)


def routes_from_schema():
    out = set()
    for r in load("schema/http.json")["routes"]:
        segs = [s for s in r["path"].split("/") if s]
        segs = ["*" if s.startswith("{") else s for s in segs]
        out.add(route_key(r["method"], segs))
    return out


def routes_from_code():
    text = read("crates/kn9t-server/src/router.rs")
    out = set()

    # `route()` is the whole dispatch table; slice it out so the lease-check
    # patterns in `is_lease_required` are not mistaken for routes.
    body = text[text.index("fn route(") : text.index("/// R-SRV-040/050")]

    for method, inner in re.findall(r"\(Method::(\w+),\s*\[([^\]]*)\]\)", body):
        parts = [p.strip() for p in inner.split(",") if p.strip()]
        out.add(route_key(method, normalize(*parts)))

    # Routes handled inline before `route()` because they hijack the socket.
    for inner in re.findall(r'if let \[([^\]]*)\] = segs\.as_slice\(\)', text):
        out.add(route_key("GET", normalize(*[p.strip() for p in inner.split(",")])))
    for inner in re.findall(r'segs\.as_slice\(\) == \[([^\]]*)\]', text):
        out.add(route_key("GET", normalize(*[p.strip() for p in inner.split(",")])))

    return out


# ── hooks ─────────────────────────────────────────────────────────────────────

def hooks_from_schema():
    return set(load("schema/plugin.json")["hook_payloads"])


def hooks_from_code():
    text = read("crates/kn9t-core/src/hook.rs")
    body = text[text.index("pub trait HookHost") :]
    body = body[: body.index("\n}")] if "\n}" in body else body
    return set(re.findall(r"fn ([a-z_]+)\(", body)) | EXTRA_HOOKS


# ── docs ──────────────────────────────────────────────────────────────────────

def ops_from_guide():
    """Op names from the guide's operations table (first cell of each row)."""
    text = read("docs/PLUGIN_DEVELOPMENT.md")
    start = text.index("### Available Operations")
    end = text.index("\n## ", start)
    names = set()
    for row in text[start:end].splitlines():
        if not row.startswith("|"):
            continue
        names |= set(re.findall(r"`([a-z_]+)`", row.split("|")[1]))
    return names


def main():
    print("== contract check (schema <-> code) ==")

    schema_ops, code_ops = ops_from_schema(), ops_from_code()
    report("ops", schema_ops, code_ops)

    # The op prose in the description must name every op, or API.md can list an
    # op in the table and omit it from the sentence a reader actually reads.
    description = load("schema/plugin.json")["plugin_to_host"]["Request"]["description"]
    unseen = sorted(op for op in schema_ops if op not in description)
    if unseen:
        failures.append("ops-in-description")
        print(f"ops-in-description: MISMATCH — described nowhere in the Request description: {unseen}")

    schema_routes, code_routes = routes_from_schema(), routes_from_code()
    report("routes", schema_routes, code_routes)

    schema_hooks, code_hooks = hooks_from_schema(), hooks_from_code()
    report("hooks", schema_hooks, code_hooks)

    # A hand-written guide is the one place a primitive can be real, generated into
    # API.md, and still invisible to the person who has to use it. Compared as a
    # set, not a substring search: `session_createX` must not satisfy `session_create`.
    report("docs", schema_ops, ops_from_guide())

    if failures:
        print("")
        print("CONTRACT DRIFT: " + ", ".join(failures))
        print("Fix the schema (it is the source of truth), then: cargo run -p xtask -- generate")
        return 1

    print("contract: OK (schema and code agree in both directions)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
