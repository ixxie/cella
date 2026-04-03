# /// script
# requires-python = ">=3.11"
# dependencies = ["mitmproxy"]
# ///
"""
Cella mitmproxy addon — domain filtering + credential injection.

Reads config from:
  /etc/cella/proxy-config.json  (static, from NixOS)
  /var/lib/cella/cells.json     (dynamic, from control API)
"""

import json
import os
import time
from pathlib import Path
from fnmatch import fnmatch

from mitmproxy import http, tls, ctx, connection


STATIC_CONFIG = Path("/etc/cella/proxy-config.json")
DYNAMIC_CELLS = Path("/var/lib/cella/cells.json")
LOG_FILE = Path("/var/log/cella/proxy.log")


def matches_domain(hostname: str, pattern: str) -> bool:
    h = hostname.lower()
    p = pattern.lower()
    if p.startswith("*."):
        return h.endswith(f".{p[2:]}")
    return h == p


def matches_any(hostname: str, domains) -> bool:
    if domains == "*":
        return True
    if isinstance(domains, list):
        return any(matches_domain(hostname, d) for d in domains)
    return False


class CellaAddon:
    def __init__(self):
        self.egress = {"reads": {}, "writes": {}}
        self.credentials: list[dict] = []
        self.passthrough: list[str] = []
        self.cells: dict[str, dict] = {}
        self._cells_mtime = 0.0
        self._load_static()
        self._load_dynamic()

    def _load_static(self):
        if not STATIC_CONFIG.exists():
            return
        try:
            cfg = json.loads(STATIC_CONFIG.read_text())
        except Exception as e:
            ctx.log.error(f"failed to load static config: {e}")
            return

        self.passthrough = cfg.get("egress", {}).get("passthrough", [])
        self.egress = cfg.get("egress", {"reads": {}, "writes": {}})

        for cell in cfg.get("cells", []):
            self.cells[cell["cellIp"]] = cell

        self.credentials = self.egress.get("credentials", [])

    def _load_dynamic(self):
        if not DYNAMIC_CELLS.exists():
            return
        try:
            mtime = DYNAMIC_CELLS.stat().st_mtime
            if mtime <= self._cells_mtime:
                return
            self._cells_mtime = mtime
            cells = json.loads(DYNAMIC_CELLS.read_text())
            for cell in cells:
                ip = cell.get("cellIp", "")
                if ip:
                    self.cells[ip] = cell
        except Exception:
            pass

    def _classify_method(self, method: str) -> str:
        read_methods = self.egress.get("reads", {}).get("methods", ["GET", "HEAD", "OPTIONS"])
        if method in read_methods:
            return "reads"
        return "writes"

    def _is_allowed(self, client_ip: str, host: str, method: str) -> bool:
        ip = client_ip.removeprefix("::ffff:")

        if ip not in self.cells:
            return False

        cell = self.cells[ip]
        direction = self._classify_method(method)

        # global rules
        rules = self.egress.get(direction, {})
        global_allowed = rules.get("allowed", [])
        global_denied = rules.get("denied", [])

        # per-cell egress overrides (set by flow engine on op transitions)
        cell_egress = cell.get("egress") or {}
        additive = cell_egress.get("additive", True)
        cell_rules = cell_egress.get(direction) or {}
        cell_allowed = cell_rules.get("allowed")
        cell_denied = cell_rules.get("denied")

        # merge: additive means cell rules layer on top of global
        # non-additive means cell rules replace global
        if cell_allowed is not None or cell_denied is not None:
            if additive:
                allowed = global_allowed
                denied = global_denied
                # cell denied adds to global denied
                if cell_denied:
                    if isinstance(denied, list):
                        denied = denied + cell_denied
                    else:
                        denied = cell_denied
                # cell allowed overrides global allowed
                if cell_allowed is not None:
                    allowed = cell_allowed
            else:
                # standalone: cell rules replace global entirely
                allowed = cell_allowed or []
                denied = cell_denied or []
        else:
            allowed = global_allowed
            denied = global_denied

        if matches_any(host, denied):
            if matches_any(host, allowed):
                return True
            return False

        if matches_any(host, allowed):
            return True

        return False

    def _load_secrets_env(self) -> dict[str, str]:
        secrets_path = Path("/var/lib/cella/secrets.env")
        if not secrets_path.exists():
            return {}
        env = {}
        for line in secrets_path.read_text().splitlines():
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            if "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
        return env

    def _inject_credentials(self, host: str, client_ip: str, flow: http.HTTPFlow):
        # global credentials
        creds = list(self.credentials)

        # per-cell credentials
        ip = client_ip.removeprefix("::ffff:")
        if ip in self.cells:
            cell = self.cells[ip]
            cell_egress = cell.get("egress") or {}
            cell_creds = cell_egress.get("credentials", [])
            creds.extend(cell_creds)

        if not creds:
            return

        # merge process env with secrets.env (secrets.env takes precedence)
        secrets = self._load_secrets_env()

        for cred in creds:
            cred_host = cred["host"]
            if host == cred_host or host.endswith(f".{cred_host}"):
                env_var = cred["envVar"]
                value = secrets.get(env_var, "") or os.environ.get(env_var, "")
                if not value:
                    continue
                header = cred["header"]
                if header.lower() == "authorization" and not value.startswith("Bearer "):
                    value = f"Bearer {value}"
                flow.request.headers[header] = value

    def _log(self, action: str, host: str, client: str, details: str = ""):
        ts = int(time.time())
        line = f"{ts} {action} {host} {client}"
        if details:
            line += f" {details}"
        ctx.log.info(line)
        try:
            with open(LOG_FILE, "a") as f:
                f.write(line + "\n")
        except Exception:
            pass

    def tls_clienthello(self, data: tls.ClientHelloData):
        host = data.context.server.address[0] if data.context.server.address else ""
        if not host and data.client_hello:
            host = data.client_hello.sni or ""
        if matches_any(host, self.passthrough):
            data.ignore_connection = True
            self._log("PASSTHROUGH", host, str(data.context.client.peername[0]))

    def request(self, flow: http.HTTPFlow):
        # reload dynamic config periodically
        self._load_dynamic()

        client_ip = flow.client_conn.peername[0]
        host = flow.request.pretty_host
        method = flow.request.method
        path = flow.request.path

        if not self._is_allowed(client_ip, host, method):
            self._log("BLOCKED", host, client_ip, f"{method} {path}")
            flow.response = http.Response.make(403, b"Blocked by cella proxy")
            return

        direction = self._classify_method(method)
        tag = "READ" if direction == "reads" else "WRITE"
        self._log(tag, host, client_ip, f"{method} {path}")
        self._inject_credentials(host, client_ip, flow)


addons = [CellaAddon()]
