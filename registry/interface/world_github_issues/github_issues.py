"""Authenticated GitHub input and durable interface-owned delivery queue."""
import hashlib
import hmac
import json
import os
import re
import secrets
import sqlite3
import threading
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path

LABEL = "github.issue.opened"


def validate_config(c):
    required = {"schema_version", "enabled", "repository", "port", "path"}
    optional = {"max_body_bytes", "max_pending", "retry_seconds", "tunnel", "relay"}
    if not isinstance(c, dict) or required - c.keys() or c.keys() - required - optional:
        raise ValueError("invalid private_config keys")
    if c["schema_version"] != 1 or not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", c["repository"]):
        raise ValueError("invalid schema_version or repository")
    if not isinstance(c["path"], str) or not re.fullmatch(r"/[A-Za-z0-9/_-]{16,200}", c["path"]):
        raise ValueError("path must be an exact private route, at least 16 characters")
    c = dict(c)
    if type(c["enabled"]) is not bool:
        raise ValueError("enabled must be boolean")
    for key, default, minimum, maximum in [
        ("port", None, 1, 65535), ("max_body_bytes", 1048576, 1024, 2097152),
        ("max_pending", 1000, 1, 100000), ("retry_seconds", 5, 1, 300),
    ]:
        value = c.setdefault(key, default)
        if type(value) is not int or not minimum <= value <= maximum:
            raise ValueError("invalid " + key)
    if "tunnel" in c and "relay" in c:
        raise ValueError("tunnel and relay are mutually exclusive")
    if "relay" in c:
        r = c["relay"]
        if not isinstance(r, dict) or set(r) != {"url", "gh", "register_hook"}:
            raise ValueError("invalid relay config")
        if not isinstance(r["url"], str) or not re.fullmatch(r"https://smee\.io/[A-Za-z0-9_-]{1,200}", r["url"]):
            raise ValueError("invalid Smee channel URL")
        if not isinstance(r["gh"], str) or not os.path.isabs(r["gh"]) or type(r["register_hook"]) is not bool:
            raise ValueError("invalid relay gh or register_hook")
    if "tunnel" in c:
        t = c["tunnel"]
        if not isinstance(t, dict) or set(t) != {"cloudflared", "metrics_port", "register_hook", "gh"}:
            raise ValueError("invalid tunnel config")
        if not os.path.isabs(t["cloudflared"]) or not os.path.isabs(t["gh"]):
            raise ValueError("tunnel executables must be absolute paths")
        if type(t["register_hook"]) is not bool:
            raise ValueError("register_hook must be boolean")
        if type(t["metrics_port"]) is not int or not 1 <= t["metrics_port"] <= 65535 or t["metrics_port"] == c["port"]:
            raise ValueError("invalid metrics_port")
    return c


def load_secret(boundary):
    path = Path(boundary.VAR_DIR) / "webhook.secret"
    try:
        secret = boundary.stable_read(path, 256, "webhook secret", private=True)
    except FileNotFoundError:
        secret = secrets.token_hex(32).encode("ascii")
        boundary.atomic_bytes(str(path), secret)
    if not 32 <= len(secret) <= 256 or any(c < 33 or c > 126 for c in secret):
        raise ValueError("webhook.secret must contain 32..256 printable ASCII bytes, no newline")
    return secret


class Receiver:
    def __init__(self, config, boundary, secret):
        self.config, self.b, self.secret = config, boundary, secret
        self.lock = threading.RLock()
        self.wake = threading.Event()
        self.db = sqlite3.connect(str(Path(boundary.VAR_DIR) / "deliveries.sqlite3"),
                                  check_same_thread=False)
        self.db.execute("PRAGMA journal_mode=DELETE")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute("""CREATE TABLE IF NOT EXISTS deliveries (
            delivery TEXT PRIMARY KEY, digest TEXT NOT NULL, manifest TEXT NOT NULL,
            artifact_hash TEXT NOT NULL, state TEXT NOT NULL, seq INTEGER,
            attempts INTEGER NOT NULL DEFAULT 0, next_attempt REAL NOT NULL DEFAULT 0,
            error TEXT)""")
        self.db.commit()
        os.chmod(str(Path(boundary.VAR_DIR) / "deliveries.sqlite3"), 0o600)

    def receive(self, headers, raw):
        signature = headers.get("X-Hub-Signature-256", "")
        expected = "sha256=" + hmac.new(self.secret, raw, hashlib.sha256).hexdigest()
        if not hmac.compare_digest(signature.encode("utf-8"), expected.encode("ascii")):
            return 401, "invalid_signature"
        event = headers.get("X-GitHub-Event", "")
        delivery = headers.get("X-GitHub-Delivery", "")
        if not re.fullmatch(r"[A-Za-z0-9-]{1,128}", delivery):
            return 400, "invalid_delivery_id"
        try:
            payload = json.loads(raw)
            if not isinstance(payload, dict):
                raise ValueError()
        except (ValueError, UnicodeDecodeError):
            return 400, "invalid_json"
        repo = payload.get("repository")
        if not isinstance(repo, dict) or repo.get("full_name") != self.config["repository"]:
            return 403, "repository_mismatch"
        if event == "ping":
            self.b.atomic_json(str(Path(self.b.VAR_DIR) / "ping.json"),
                               {"delivery_id": delivery, "repository": repo["full_name"],
                                "received_at": time.time(), "status": "authenticated"})
            return 200, "ping"
        if event != "issues" or payload.get("action") != "opened":
            return 204, "ignored"
        issue = payload.get("issue")
        if not isinstance(issue, dict):
            return 400, "invalid_issue"
        if "pull_request" in issue:
            return 204, "ignored"
        if type(issue.get("number")) is not int or type(issue.get("id")) is not int:
            return 400, "invalid_issue"
        digest = hashlib.sha256(raw).hexdigest()
        with self.lock:
            existing = self.db.execute("SELECT digest,state FROM deliveries WHERE delivery=?", (delivery,)).fetchone()
            if existing:
                if existing[0] != digest:
                    return 409, "delivery_conflict"
                self.wake.set()
                return (200, "delivered") if existing[1] == "delivered" else (202, "durably_queued")
            pending = self.db.execute("SELECT count(*) FROM deliveries WHERE state='pending'").fetchone()[0]
            if pending >= self.config["max_pending"]:
                return 503, "queue_full"
            body = self.b.canonical_json({
                "schema_version": 1, "delivery_id": delivery, "source": {
                    "interface": self.b.INTERFACE_ID, "delivery_id": delivery,
                    "event": event, "action": "opened", "repository": repo["full_name"],
                    "received_at": time.time(), "body_sha256": digest,
                }, "issue": issue, "webhook": payload,
            })
            if len(body) > self.b.IO_LIMITS["max_payload_bytes"]:
                return 413, "artifact_too_large"
            manifest, artifact_hash = self.b.materialize_artifact(
                LABEL, body, "github-" + delivery)
            with self.db:
                self.db.execute("INSERT INTO deliveries(delivery,digest,manifest,artifact_hash,state) VALUES(?,?,?,?, 'pending')",
                                (delivery, digest, str(manifest), artifact_hash))
        self.wake.set()
        return 202, "durably_queued"

    def deliver_one(self):
        with self.lock:
            row = self.db.execute("""SELECT delivery,manifest,artifact_hash FROM deliveries
                WHERE state='pending' AND next_attempt<=? ORDER BY rowid LIMIT 1""", (time.time(),)).fetchone()
        if row is None:
            return False
        delivery, manifest, artifact_hash = row
        try:
            seq = self.b.hand_artifact_idempotent(manifest, LABEL, artifact_hash)
            if type(seq) is not int or seq < 0:
                raise RuntimeError("invalid admission sequence")
        except Exception as error:
            # Boundary exceptions can contain external data; persist only their class.
            with self.lock, self.db:
                self.db.execute("""UPDATE deliveries SET attempts=attempts+1,next_attempt=?,error=?
                    WHERE delivery=?""", (time.time() + self.config["retry_seconds"],
                                         type(error).__name__, delivery))
            if not isinstance(error, self.b.DeliveryRefused):
                raise
            return True
        with self.lock, self.db:
            self.db.execute("""UPDATE deliveries SET state='delivered',seq=?,attempts=attempts+1,
                error=NULL WHERE delivery=?""", (seq, delivery))
        self.b.atomic_json(str(Path(self.b.VAR_DIR) / "last-admission.json"),
                           {"delivery_id": delivery, "admitted_seq": seq,
                            "artifact_hash": artifact_hash})
        return True

    def run(self, stop, fatal):
        try:
            while not stop.is_set():
                self.wake.clear()
                while not stop.is_set() and self.deliver_one():
                    pass
                self.wake.wait(self.config["retry_seconds"])
        except Exception:
            fatal.set()
            stop.set()

    def close(self):
        self.db.close()


def make_server(receiver):
    class Handler(BaseHTTPRequestHandler):
        def setup(self):
            super().setup()
            self.connection.settimeout(5)

        def log_message(self, *_):
            pass

        def do_POST(self):
            if self.path != receiver.config["path"]:
                return self.reply(404, "not_found")
            if self.headers.get("Transfer-Encoding"):
                return self.reply(400, "transfer_encoding_not_supported")
            for name in ("Content-Length", "X-Hub-Signature-256", "X-GitHub-Event", "X-GitHub-Delivery"):
                if len(self.headers.get_all(name, [])) != 1:
                    return self.reply(400, "ambiguous_headers")
            if self.headers.get_content_type() != "application/json":
                return self.reply(415, "json_required")
            length = self.headers["Content-Length"]
            if not re.fullmatch(r"[0-9]{1,10}", length):
                return self.reply(400, "invalid_length")
            length = int(length)
            if length > receiver.config["max_body_bytes"]:
                return self.reply(413, "body_too_large")
            try:
                raw = self.rfile.read(length)
                if len(raw) != length:
                    return self.reply(400, "incomplete_body")
                code, message = receiver.receive(self.headers, raw)
            except TimeoutError:
                return self.reply(408, "read_timeout")
            except Exception:
                return self.reply(503, "acceptance_failed")
            self.reply(code, message)

        def reply(self, code, message):
            self.close_connection = True
            body = b"" if code == 204 else json.dumps({"status": message}).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                self.wfile.write(body)
            except OSError:
                pass

    server = HTTPServer(("127.0.0.1", receiver.config["port"]), Handler)
    server.timeout = 0.5
    return server
