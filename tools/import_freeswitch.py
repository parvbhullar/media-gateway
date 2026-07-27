#!/usr/bin/env python3
"""Import FreeSWITCH export (freeswitch_export.json) into rustpbx via console API.

Usage:
    python3 tools/import_freeswitch.py --dry-run
    python3 tools/import_freeswitch.py --apply
    python3 tools/import_freeswitch.py --dump-refined tools/refined_freeswitch.json

See docs/superpowers/specs/2026-04-16-freeswitch-import-design.md for design.
"""
from __future__ import annotations

import argparse
import http.cookiejar
import json
import os
import re
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field, asdict
from typing import Any


# ---------- Data classes ----------

@dataclass
class RefinedTrunk:
    name: str
    display_name: str
    direction: str            # "bidirectional"
    sip_server: str           # "sip:host:5060"
    sip_transport: str        # "udp" | "tcp"
    auth_username: str
    auth_password: str
    register_enabled: bool
    is_active: bool
    description: str


@dataclass
class RefinedDidBulk:
    trunk_name: str           # always "voda_intphony" in this export
    numbers: list[str]        # E.164, e.g. "+918071539132"
    label: str
    enabled: bool = True


@dataclass
class RefinedRoute:
    name: str
    direction: str            # "inbound" | "outbound"
    priority: int
    disabled: bool
    match: dict[str, str]     # {"to.user": "..."} or {"from.user": "..."}
    rewrite: dict[str, str]   # {"to.user": "+91{1}"} — may be empty
    action: dict[str, Any]
    source_trunk: str
    description: str


@dataclass
class RefinedPlan:
    trunks: list[RefinedTrunk] = field(default_factory=list)
    dids: list[RefinedDidBulk] = field(default_factory=list)
    routes: list[RefinedRoute] = field(default_factory=list)
    dropped_trunks: list[tuple[str, str]] = field(default_factory=list)  # (name, reason)
    dropped_routes: list[tuple[str, str]] = field(default_factory=list)
    warnings: list[str] = field(default_factory=list)


# ---------- Pattern expander ----------

def expand_fs_pattern(expression: str) -> list[str]:
    """Expand the outer capture group of a FreeSWITCH regex into literal numbers.

    Supports: alternation `|`, character classes `[0-9]` / `[0-24-6]`,
    `{N}` repetition, and plain digits. Anything else raises ValueError.
    """
    group = _extract_outer_group(expression)
    out: list[str] = []
    for alt in _split_alternatives(group):
        out.extend(_expand_literal(alt))
    # De-dup while preserving order.
    seen: set[str] = set()
    result: list[str] = []
    for n in out:
        if n not in seen:
            seen.add(n)
            result.append(n)
    return result


def _extract_outer_group(expression: str) -> str:
    """Return the body of the last top-level (capturing) group in expression."""
    depth = 0
    start = -1
    end = -1
    i = 0
    while i < len(expression):
        c = expression[i]
        if c == "\\":
            i += 2
            continue
        if c == "(":
            # Skip non-capturing groups "(?:" and lookarounds.
            if expression[i:i+3] == "(?:" or expression[i:i+2] == "(?":
                depth += 1
                i += 1
                continue
            if depth == 0:
                start = i + 1
                depth += 1
                i += 1
                continue
            depth += 1
        elif c == ")":
            depth -= 1
            if depth == 0 and start != -1 and end == -1:
                end = i
        i += 1
    if start == -1 or end == -1:
        raise ValueError(f"no capture group in pattern: {expression!r}")
    return expression[start:end]


def _split_alternatives(group: str) -> list[str]:
    """Split on top-level `|` (ignoring `|` inside brackets or groups)."""
    out: list[str] = []
    depth = 0
    buf: list[str] = []
    i = 0
    while i < len(group):
        c = group[i]
        if c == "\\" and i + 1 < len(group):
            buf.append(group[i:i+2])
            i += 2
            continue
        if c == "[":
            depth += 1
        elif c == "]":
            depth -= 1
        elif c == "(":
            depth += 1
        elif c == ")":
            depth -= 1
        elif c == "|" and depth == 0:
            out.append("".join(buf))
            buf = []
            i += 1
            continue
        buf.append(c)
        i += 1
    out.append("".join(buf))
    return out


def _expand_class(body: str) -> list[str]:
    """Expand the body of a character class like '0-5' or '0-24-6' into digits."""
    digits: list[str] = []
    i = 0
    while i < len(body):
        if i + 2 < len(body) and body[i+1] == "-":
            lo, hi = body[i], body[i+2]
            if not (lo.isdigit() and hi.isdigit()):
                raise ValueError(f"non-digit range in class: {body!r}")
            digits.extend(str(d) for d in range(int(lo), int(hi) + 1))
            i += 3
            continue
        if body[i].isdigit():
            digits.append(body[i])
            i += 1
            continue
        raise ValueError(f"unsupported char in class: {body[i]!r}")
    # Dedup preserving order.
    seen: set[str] = set()
    out: list[str] = []
    for d in digits:
        if d not in seen:
            seen.add(d)
            out.append(d)
    return out


def _expand_literal(literal: str) -> list[str]:
    """Expand a single alternative (no top-level `|`) into concrete strings.

    Walks left to right. Each token contributes a list of possible chars;
    the cartesian product yields the final strings.
    """
    tokens: list[list[str]] = []
    i = 0
    while i < len(literal):
        c = literal[i]
        if c.isdigit():
            tokens.append([c])
            i += 1
            continue
        if c == "[":
            end = literal.find("]", i)
            if end == -1:
                raise ValueError(f"unterminated class in {literal!r}")
            body = literal[i+1:end]
            choices = _expand_class(body)
            i = end + 1
            # Optional {N} repetition.
            if i < len(literal) and literal[i] == "{":
                close = literal.find("}", i)
                if close == -1:
                    raise ValueError(f"unterminated repetition in {literal!r}")
                n = int(literal[i+1:close])
                i = close + 1
                for _ in range(n):
                    tokens.append(list(choices))
            else:
                tokens.append(choices)
            continue
        raise ValueError(f"unsupported token {c!r} in {literal!r}")

    # Cartesian product.
    out = [""]
    for choices in tokens:
        out = [prefix + ch for prefix in out for ch in choices]
    return out


# ---------- Refiner (pure, no I/O) ----------

class Refiner:
    """Turns a raw freeswitch_export.json dict into a RefinedPlan."""

    DROP_TRUNK_ORPHANS = {"cloud", "cloud_2", "cloud_rustpbx", "cloud_libresbc"}
    DROP_TRUNK_PLACEHOLDER_HOSTS = {"SHEEMISH_SIP_HOST"}
    DROP_ROUTE_NAMES = {"public_did"}
    DROP_ROUTE_MATCH_FIELDS = {"network_addr"}
    SOURCE_INBOUND_TRUNK = "voda_intphony"

    def refine(self, export: dict) -> RefinedPlan:
        plan = RefinedPlan()
        self._refine_trunks(export, plan)
        self._refine_dids(export, plan)
        self._refine_routes(export, plan)
        return plan

    def _refine_dids(self, export: dict, plan: RefinedPlan) -> None:
        for row in export.get("dids", []):
            pattern = row["pattern"]
            route = row["route"]
            template = row.get("target_user_template") or "+91$1"
            if template != "+91$1":
                plan.warnings.append(
                    f"DID {route}: unexpected user_template {template!r}, skipping"
                )
                continue
            try:
                local_numbers = expand_fs_pattern(pattern)
            except ValueError as e:
                plan.warnings.append(f"DID {route}: cannot expand pattern ({e}), skipping")
                continue
            e164 = [f"+91{n}" for n in local_numbers]
            plan.dids.append(RefinedDidBulk(
                trunk_name=self.SOURCE_INBOUND_TRUNK,
                numbers=e164,
                label=f"imported: {route}",
                enabled=True,
            ))

    def _refine_routes(self, export: dict, plan: RefinedPlan) -> None:
        kept_trunk_names = {t.name for t in plan.trunks}
        raw_by_name = {
            ext["name"]: ext
            for ext in export.get("raw", {}).get("extensions", [])
        }
        for r in export.get("routes", []):
            name = r["name"]
            if name in self.DROP_ROUTE_NAMES:
                plan.dropped_routes.append((name, "test/extension route, no bridge"))
                continue
            if r.get("match_field") in self.DROP_ROUTE_MATCH_FIELDS:
                plan.dropped_routes.append((name, f"network_addr match unsupported in rustpbx"))
                continue
            bridges = r.get("bridge_targets") or []
            if not bridges:
                plan.dropped_routes.append((name, "no bridge_targets (FS no-op)"))
                continue
            target = bridges[0]
            target_gw = target.get("gateway")
            if target_gw not in kept_trunk_names:
                plan.dropped_routes.append(
                    (name, f"target gateway {target_gw!r} not in kept trunks"))
                continue

            match_field = r["match_field"]
            # The FS JSON export double-encodes backslashes (e.g. \\+91 → \\\\+91).
            # Un-escape so rustpbx regex sees \+91 (literal +) not \\+91 (literal \).
            raw_expr = r["match_expression"].replace("\\\\", "\\")
            if match_field == "destination_number":
                match = {"to.user": raw_expr}
                direction = "inbound"
                source_trunk = self.SOURCE_INBOUND_TRUNK
            elif match_field == "caller_id_number":
                match = {"from.user": raw_expr}
                direction = "outbound"
                source_trunk = name.rsplit("_to_voda", 1)[0] if name.endswith("_to_voda") else ""
                if source_trunk not in kept_trunk_names:
                    source_trunk = ""
            else:
                plan.dropped_routes.append((name, f"unsupported match_field {match_field!r}"))
                continue

            user_template = target.get("user_template") or ""
            rewrite = self._translate_rewrite(user_template, "to.user")

            from_template = self._extract_from_user_rewrite(raw_by_name.get(name))
            if from_template:
                rewrite.update(self._translate_rewrite(from_template, "from.user"))

            plan.routes.append(RefinedRoute(
                name=name,
                direction=direction,
                priority=100,
                disabled=False,
                match=match,
                rewrite=rewrite,
                action={
                    "select": "rr",
                    "target_type": "sip_trunk",
                    "trunks": [{"name": target_gw, "weight": 1}],
                },
                source_trunk=source_trunk,
                description=f"Imported from FreeSWITCH {r.get('source_file', '?')}",
            ))

    _FS_VAR_RE = re.compile(r"^\$\{.+\}$")
    _FS_BACKREF_RE = re.compile(r"\$(\d+)")

    @classmethod
    def _translate_rewrite(cls, fs_template: str, field: str = "to.user") -> dict[str, str]:
        """Convert a FreeSWITCH template to a rustpbx rewrite dict entry.

        rustpbx uses {1}, {2} for capture-group backreferences (matcher.rs:858).
        FreeSWITCH uses $1, $2. FS channel variables like ${destination_number}
        have no equivalent — they mean "pass through the original value", so we
        return an empty dict (no-op).
        """
        if not fs_template:
            return {}
        if cls._FS_VAR_RE.match(fs_template):
            return {}
        translated = cls._FS_BACKREF_RE.sub(lambda m: "{" + m.group(1) + "}", fs_template)
        return {field: translated}

    @staticmethod
    def _extract_from_user_rewrite(raw_ext: dict | None) -> str:
        """Extract sip_from_user value from raw FS extension actions.

        Returns the template string (e.g. "$1" or "${caller_id_number}"),
        or empty string if not found.
        """
        if not raw_ext:
            return ""
        for cond in raw_ext.get("conditions", []):
            for action in cond.get("actions", []):
                data = action.get("data") or ""
                if data.startswith("sip_from_user="):
                    return data.split("=", 1)[1]
        return ""

    def _refine_trunks(self, export: dict, plan: RefinedPlan) -> None:
        for t in export.get("trunks", []):
            name = t["name"]
            if name in self.DROP_TRUNK_ORPHANS:
                plan.dropped_trunks.append((name, "orphaned: no routes reference it"))
                continue
            proxy = (t.get("proxy") or "").strip()
            # Strip inline ";transport=..." from the host.
            host = proxy.split(";", 1)[0].strip()
            if not host or host in self.DROP_TRUNK_PLACEHOLDER_HOSTS:
                plan.dropped_trunks.append((name, f"placeholder host: {host!r}"))
                continue
            transport = (t.get("transport") or "udp").lower()
            if transport not in ("udp", "tcp"):
                transport = "udp"
            plan.trunks.append(RefinedTrunk(
                name=name,
                display_name=name,
                direction="bidirectional",
                sip_server=f"sip:{host}:5060",
                sip_transport=transport,
                auth_username=t.get("username") or "",
                auth_password=t.get("password") or "",
                register_enabled=bool(t.get("register")) is True,
                is_active=True,
                description=f"Imported from FreeSWITCH {t.get('source_file', '?')} on 2026-04-16",
            ))


# ---------- Entrypoint ----------

def load_env(path: str) -> dict[str, str]:
    """Minimal .env parser: KEY=VALUE per line, `#` comments, no quoting."""
    env: dict[str, str] = {}
    with open(path) as f:
        for raw in f:
            line = raw.strip()
            if not line or line.startswith("#"):
                continue
            if "=" not in line:
                continue
            key, _, value = line.partition("=")
            env[key.strip()] = value.strip()
    return env


def refined_trunk_to_record(trunk: RefinedTrunk) -> dict[str, Any]:
    """Convert a RefinedTrunk into the canonical `kind` + `kind_config` record
    shape consumed by the `/api/v1/gateways` REST endpoint (Phase 8a wire
    shape).

    Top-level fields are shared across kinds; everything SIP-specific lives
    inside `kind_config`. `register_extra_headers` uses the list-of-pairs
    shape `[[key, value], ...]` so duplicates and ordering survive a round
    trip (the old dict shape coalesced repeats).
    """
    return {
        "name": trunk.name,
        "kind": "sip",
        "display_name": trunk.display_name,
        "description": trunk.description,
        "direction": trunk.direction,
        "is_active": trunk.is_active,
        "kind_config": {
            "sip_server": trunk.sip_server,
            "sip_transport": trunk.sip_transport,
            "auth_username": trunk.auth_username,
            "auth_password": trunk.auth_password,
            "register_enabled": trunk.register_enabled,
            # Defaults for fields the FS importer doesn't populate. Kept
            # explicit so dumped artifacts document the full kind_config
            # surface.
            "outbound_proxy": None,
            "register_expires": None,
            "register_extra_headers": [],
            "rewrite_hostport": False,
            "did_numbers": [],
            "incoming_from_user_prefix": None,
            "incoming_to_user_prefix": None,
            "default_route_label": None,
            "billing_snapshot": None,
            "analytics": None,
            "carrier": None,
        },
    }


def dump_refined(plan: RefinedPlan, path: str) -> None:
    """Write refined plan as pretty JSON.

    Trunks are serialised in the canonical `kind` + `kind_config` shape so
    the dumped artifact matches what `/api/v1/gateways` would round-trip.
    DIDs and routes keep their dataclass shape (they have no kind split).
    """
    payload = {
        "trunks": [refined_trunk_to_record(t) for t in plan.trunks],
        "dids": [asdict(d) for d in plan.dids],
        "routes": [asdict(r) for r in plan.routes],
        "dropped_trunks": plan.dropped_trunks,
        "dropped_routes": plan.dropped_routes,
        "warnings": plan.warnings,
    }
    with open(path, "w") as f:
        json.dump(payload, f, indent=2, sort_keys=True)


class ConsoleError(RuntimeError):
    pass


class ConsoleClient:
    """Thin wrapper over rustpbx's /console/* endpoints using a cookie jar.

    All paths below are relative to base_url, which should include the console
    base path (e.g. "http://localhost:3000/console").  The router nests the
    handler sub-routes under that base path, so the full URLs become e.g.
    http://localhost:3000/console/login, /console/sip-trunk, etc.
    """

    def __init__(self, base_url: str):
        self.base_url = base_url.rstrip("/")
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(
            urllib.request.HTTPCookieProcessor(self.jar),
            urllib.request.HTTPRedirectHandler(),
        )

    # ---- transport ----

    def _request(
        self,
        method: str,
        path: str,
        *,
        form: dict | None = None,
        body: dict | None = None,
    ) -> tuple[int, bytes]:
        url = f"{self.base_url}{path}"
        data: bytes | None = None
        headers = {"Accept": "application/json"}
        if form is not None:
            data = urllib.parse.urlencode(form).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        elif body is not None:
            data = json.dumps(body).encode()
            headers["Content-Type"] = "application/json"
        req = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with self.opener.open(req, timeout=30) as resp:
                return resp.status, resp.read()
        except urllib.error.HTTPError as e:
            return e.code, e.read()

    # ---- auth ----

    def login(self, username: str, password: str) -> None:
        """Authenticate with rustpbx console.

        Verified against src/console/handlers/user.rs (login_post, line 80) and
        src/console/handlers/forms.rs (LoginForm, line 13):
          - Route: POST /login  (registered at src/console/handlers/user.rs line 32)
          - Extractor: Form<LoginForm>
          - Field names: "identifier" (not "username"), "password"
          - On success: 302 redirect; cookie name: "rustpbx_session"
            (src/console/auth.rs SESSION_COOKIE_NAME, line 23)
        """
        status, body = self._request(
            "POST", "/login",
            form={"identifier": username, "password": password},
        )
        # Login redirects (302) on success; urllib follows it so we may see 200
        # (the dashboard page) or the raw 302 if redirect-following is limited.
        if status not in (200, 204, 302):
            raise ConsoleError(f"login failed: {status} {body[:200]!r}")
        if not any(c.name == "rustpbx_session" for c in self.jar):
            print(
                "  warning: login returned no rustpbx_session cookie",
                file=sys.stderr,
            )

    # ---- reads ----

    def snapshot(self) -> tuple[set[str], set[str], set[str]]:
        """Return (trunk_names, did_numbers, route_names) currently in rustpbx.

        List endpoints:
          - Trunks: POST /sip-trunk  JSON body ListQuery  → {items:[...], ...}
            (src/console/handlers/sip_trunk.rs lines 52-57, query_sip_trunks)
          - DIDs:   GET  /dids        query-string params  → {items:[...]}
            (src/console/handlers/did.rs lines 21-22, list_dids)
          - Routes: POST /routing     JSON body ListQuery  → {items:[...], ...}
            (src/console/handlers/routing.rs lines 41-44, query_routing)
        """
        trunks = self._list_trunks()
        dids = self._list_dids()
        routes = self._list_routes()
        return trunks, dids, routes

    def _list_trunks(self) -> set[str]:
        """POST /sip-trunk with a large-page ListQuery; returns {items:[...]}."""
        payload = {"page": 1, "per_page": 1000}
        status, body = self._request("POST", "/sip-trunk", body=payload)
        if status != 200:
            raise ConsoleError(f"POST /sip-trunk → {status} {body[:200]!r}")
        try:
            data = json.loads(body)
        except json.JSONDecodeError as e:
            raise ConsoleError(f"POST /sip-trunk: non-JSON response ({e})")
        items = data.get("items") or []
        return {item["name"] for item in items if isinstance(item, dict) and "name" in item}

    def _list_dids(self) -> set[str]:
        """GET /dids → {items:[...]}."""
        status, body = self._request("GET", "/dids")
        if status != 200:
            raise ConsoleError(f"GET /dids → {status} {body[:200]!r}")
        try:
            data = json.loads(body)
        except json.JSONDecodeError as e:
            raise ConsoleError(f"GET /dids: non-JSON response ({e})")
        items = data.get("items") or []
        return {
            item["number"]
            for item in items
            if isinstance(item, dict) and "number" in item
        }

    def _list_routes(self) -> set[str]:
        """POST /routing with a large-page ListQuery; returns {items:[...]}."""
        payload = {"page": 1, "per_page": 1000}
        status, body = self._request("POST", "/routing", body=payload)
        if status != 200:
            raise ConsoleError(f"POST /routing → {status} {body[:200]!r}")
        try:
            data = json.loads(body)
        except json.JSONDecodeError as e:
            raise ConsoleError(f"POST /routing: non-JSON response ({e})")
        items = data.get("items") or []
        return {item["name"] for item in items if isinstance(item, dict) and "name" in item}

    # ---- writes ----

    def create_trunk(self, trunk: RefinedTrunk) -> None:
        """PUT /sip-trunk to create a SIP trunk via the console form.

        The canonical wire shape after Phase 8a is `kind` + `kind_config`
        (see `refined_trunk_to_record`). The console form handler still
        accepts the legacy flat SIP fields and folds them into a SIP-typed
        `kind_config` server-side, so this form-encoded path stays correct
        without resending the kind discriminator. Switching to
        `/api/v1/gateways` (which speaks the new shape natively) would
        require a Bearer token instead of a console session cookie and is
        out of scope for the importer.

        Verified against src/console/handlers/sip_trunk.rs (create_sip_trunk,
        line 203) and src/console/handlers/forms.rs (SipTrunkForm, line 60):
          - Route: PUT /sip-trunk  (registered at sip_trunk.rs line 55)
          - Extractor: Form<SipTrunkForm>  (form-encoded, not JSON)
          - All SipTrunkForm fields are Option<T>; absent fields are ignored.
          - On success: 200 {"status": "ok", "id": <i64>}
        """
        form = {
            "name": trunk.name,
            "display_name": trunk.display_name,
            "direction": trunk.direction,
            "sip_server": trunk.sip_server,
            "sip_transport": trunk.sip_transport,
            "auth_username": trunk.auth_username,
            "auth_password": trunk.auth_password,
            "register_enabled": "true" if trunk.register_enabled else "false",
            "is_active": "true" if trunk.is_active else "false",
            "description": trunk.description,
        }
        status, body = self._request("PUT", "/sip-trunk", form=form)
        if status not in (200, 201):
            raise ConsoleError(
                f"create trunk {trunk.name!r}: {status} {body[:300]!r}"
            )

    def bulk_create_dids(self, bulk: RefinedDidBulk, existing: set[str]) -> list[str]:
        """POST /dids/bulk to create DIDs in bulk.

        Verified against src/console/handlers/did.rs (bulk_create_dids, line
        296) and BulkCreatePayload (line 83):
          - Route: POST /dids/bulk  (did.rs line 23)
          - Extractor: Json<BulkCreatePayload>
          - Fields: trunk_name (optional), numbers (list), extension_number
            (optional), failover_trunk (optional), label (optional), enabled.
          - On success: 200 {"accepted": [...], "rejected": [...]}
        """
        numbers = [n for n in bulk.numbers if n not in existing]
        if not numbers:
            return []
        payload: dict = {
            "numbers": numbers,
            "enabled": bulk.enabled,
        }
        if bulk.trunk_name:
            payload["trunk_name"] = bulk.trunk_name
        if bulk.label:
            payload["label"] = bulk.label
        status, body = self._request("POST", "/dids/bulk", body=payload)
        if status not in (200, 201):
            raise ConsoleError(
                f"bulk DID {bulk.label!r}: {status} {body[:300]!r}"
            )
        return numbers

    def create_route(self, route: RefinedRoute) -> None:
        """PUT /routing to create a routing rule.

        Verified against src/console/handlers/routing.rs (create_routing, line
        1329) and RouteDocument (line 57):
          - Route: PUT /routing  (routing.rs line 43)
          - Extractor: Json<RouteDocument>
          - RouteDocument uses #[serde(rename_all = "camelCase")] so:
              * source_trunk  → "sourceTrunk"
              * matchers field → "match"  (explicit #[serde(rename = "match")])
              * All other top-level fields: camelCase (but most are already
                single-word so no change: name, direction, priority, disabled,
                rewrite, action, notes)
          - RouteActionDocument has NO rename_all, so its fields are snake_case:
              select, hash_key, trunks, target_type
          - On success: 200 {"status": "ok", "id": <i64>}
        """
        payload: dict = {
            "name": route.name,
            "direction": route.direction,
            "priority": route.priority,
            "disabled": route.disabled,
            "match": route.match,
            "rewrite": route.rewrite,
            "action": {
                "select": route.action.get("select", "rr"),
                "target_type": route.action.get("target_type", "sip_trunk"),
                "trunks": route.action.get("trunks", []),
            },
        }
        if route.source_trunk:
            payload["sourceTrunk"] = route.source_trunk
        if route.description:
            payload["description"] = route.description
        status, body = self._request("PUT", "/routing", body=payload)
        if status not in (200, 201):
            raise ConsoleError(
                f"create route {route.name!r}: {status} {body[:300]!r}"
            )

    def reload(self) -> None:
        """POST /ami/v1/reload/trunks and /ami/v1/reload/routes to apply changes.

        The AMI endpoints accept authenticated console session cookies from superusers,
        so no separate AMI key is required after login().
        """
        ami_base = self.base_url.replace("/console", "")
        for path, label in [("/ami/v1/reload/trunks", "trunks"), ("/ami/v1/reload/routes", "routes")]:
            url = f"{ami_base}{path}"
            req = urllib.request.Request(url, data=b"", method="POST",
                                         headers={"Accept": "application/json"})
            try:
                with self.opener.open(req, timeout=30) as resp:
                    status, body = resp.status, resp.read()
            except urllib.error.HTTPError as e:
                status, body = e.code, e.read()
            if status == 200:
                print(f"  reloaded {label}", file=sys.stderr)
            else:
                print(f"  ! reload {label} failed: {status} {body[:200]!r}", file=sys.stderr)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--env", default=".env.prod")
    parser.add_argument("--export", default="freeswitch_export.json")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--apply", action="store_true")
    parser.add_argument("--dump-refined", metavar="PATH")
    parser.add_argument("--only", choices=["trunks", "dids", "routes"])
    args = parser.parse_args(argv)

    with open(args.export) as f:
        export = json.load(f)
    plan = Refiner().refine(export)

    print(f"Refined: {len(plan.trunks)} trunks, "
          f"{sum(len(b.numbers) for b in plan.dids)} DID numbers "
          f"({len(plan.dids)} bulk groups), {len(plan.routes)} routes",
          file=sys.stderr)
    for name, reason in plan.dropped_trunks:
        print(f"  dropped trunk {name}: {reason}", file=sys.stderr)
    for name, reason in plan.dropped_routes:
        print(f"  dropped route {name}: {reason}", file=sys.stderr)
    for w in plan.warnings:
        print(f"  warning: {w}", file=sys.stderr)

    if args.dump_refined:
        dump_refined(plan, args.dump_refined)
        print(f"wrote {args.dump_refined}", file=sys.stderr)
        return 0

    if not args.apply:
        print("\nDry run. Use --apply to POST to rustpbx.", file=sys.stderr)
        return 0

    env = load_env(args.env)
    for k in ("SUPER_USERNAME", "SUPER_PASSWORD", "RUSTPBX_URL"):
        if not env.get(k):
            print(f"error: {k} missing from {args.env}", file=sys.stderr)
            return 2

    base_url = env["RUSTPBX_URL"].rstrip("/") + "/console"
    client = ConsoleClient(base_url)
    print(f"logging in to {base_url} as {env['SUPER_USERNAME']}",
          file=sys.stderr)
    client.login(env["SUPER_USERNAME"], env["SUPER_PASSWORD"])

    existing_trunks, existing_dids, existing_routes = client.snapshot()
    print(f"existing: {len(existing_trunks)} trunks, "
          f"{len(existing_dids)} dids, {len(existing_routes)} routes",
          file=sys.stderr)

    created = {"trunks": 0, "dids": 0, "routes": 0}
    skipped = {"trunks": 0, "dids": 0, "routes": 0}
    failed: list[str] = []

    if args.only in (None, "trunks"):
        for trunk in plan.trunks:
            if trunk.name in existing_trunks:
                skipped["trunks"] += 1
                print(f"  skip trunk {trunk.name} (exists)", file=sys.stderr)
                continue
            try:
                client.create_trunk(trunk)
                created["trunks"] += 1
                print(f"  + trunk {trunk.name}", file=sys.stderr)
            except ConsoleError as e:
                failed.append(f"trunk {trunk.name}: {e}")
                print(f"  ! trunk {trunk.name}: {e}", file=sys.stderr)

    if args.only in (None, "dids"):
        for bulk in plan.dids:
            try:
                posted = client.bulk_create_dids(bulk, existing_dids)
                created["dids"] += len(posted)
                skipped["dids"] += len(bulk.numbers) - len(posted)
                existing_dids.update(posted)
                print(f"  + dids {bulk.label}: {len(posted)} new, "
                      f"{len(bulk.numbers) - len(posted)} skipped",
                      file=sys.stderr)
            except ConsoleError as e:
                failed.append(f"dids {bulk.label}: {e}")
                print(f"  ! dids {bulk.label}: {e}", file=sys.stderr)

    if args.only in (None, "routes"):
        for route in plan.routes:
            if route.name in existing_routes:
                skipped["routes"] += 1
                print(f"  skip route {route.name} (exists)", file=sys.stderr)
                continue
            try:
                client.create_route(route)
                created["routes"] += 1
                print(f"  + route {route.name}", file=sys.stderr)
            except ConsoleError as e:
                failed.append(f"route {route.name}: {e}")
                print(f"  ! route {route.name}: {e}", file=sys.stderr)

    client.reload()
    print(f"\nSummary: created {created}, skipped {skipped}", file=sys.stderr)
    if failed:
        print(f"Failures ({len(failed)}):", file=sys.stderr)
        for f in failed:
            print(f"  - {f}", file=sys.stderr)
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
