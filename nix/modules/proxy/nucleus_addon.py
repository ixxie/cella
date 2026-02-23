# /// script
# requires-python = ">=3.11"
# dependencies = ["mitmproxy"]
# ///
"""
Cella nucleus mitmproxy addon — per-cell strict allowlist.
Each cell registers its own nucleus allowedDomains via cells.json.
"""

import json
import os
import time
from pathlib import Path

from mitmproxy import http, ctx


STATIC_CONFIG = Path("/etc/cella/proxy-config.json")
DYNAMIC_CELLS = Path("/var/lib/cella/cells.json")
LOG_FILE = Path("/var/log/cella/proxy.log")


def matches_domain(hostname: str, pattern: str) -> bool:
    h = hostname.lower()
    p = pattern.lower()
    if p.startswith("*."):
        return h.endswith(f".{p[2:]}")
    return h == p


class NucleusAddon:
    def __init__(self):
        self.cells: dict[str, dict] = {}
        self.credentials: list[dict] = []
        self._cells_mtime = 0.0
        self._load_static()
        self._load_dynamic()

    def _load_static(self):
        if not STATIC_CONFIG.exists():
            return
        try:
            cfg = json.loads(STATIC_CONFIG.read_text())
            self.credentials = cfg.get("credentials", [])
            # load static cell nucleus config
            for cell in cfg.get("cells", []):
                self.cells[cell["cellIp"]] = cell
        except Exception as e:
            ctx.log.error(f"failed to load config: {e}")

    def _load_dynamic(self):
        if not DYNAMIC_CELLS.exists():
            return
        try:
            mtime = DYNAMIC_CELLS.stat().st_mtime
            if mtime <= self._cells_mtime:
                return
            self._cells_mtime = mtime
            for cell in json.loads(DYNAMIC_CELLS.read_text()):
                ip = cell.get("cellIp", "")
                if ip:
                    self.cells[ip] = cell
        except Exception:
            pass

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

    def request(self, flow: http.HTTPFlow):
        self._load_dynamic()

        client_ip = flow.client_conn.peername[0].removeprefix("::ffff:")
        host = flow.request.pretty_host
        method = flow.request.method
        path = flow.request.path

        # look up this cell's nucleus allowedDomains
        cell = self.cells.get(client_ip, {})
        allowed_domains = cell.get("nucleusAllowedDomains", [])

        allowed = any(matches_domain(host, d) for d in allowed_domains)
        if not allowed:
            self._log("NUCLEUS-BLOCKED", host, client_ip, f"{method} {path}")
            flow.response = http.Response.make(403, b"Blocked by nucleus filter")
            return

        self._log("NUCLEUS", host, client_ip, f"{method} {path}")

        # inject credentials
        for cred in self.credentials:
            cred_host = cred["host"]
            if host == cred_host or host.endswith(f".{cred_host}"):
                value = os.environ.get(cred["envVar"], "")
                if not value:
                    continue
                header = cred["header"]
                if header.lower() == "authorization" and not value.startswith("Bearer "):
                    value = f"Bearer {value}"
                flow.request.headers[header] = value


addons = [NucleusAddon()]
