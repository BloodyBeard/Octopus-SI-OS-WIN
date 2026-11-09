"""Bounded Smee SSE push transport; all payloads cross Receiver authentication."""
import http.client
import json
import socket
import threading
import time
from email.message import Message
from pathlib import Path
from urllib.parse import urlsplit

from tunnel import HookManager


class RelayError(RuntimeError):
    pass


def sse_events(stream, maximum):
    data, size, event, identifier = [], 0, "message", None
    while True:
        line = stream.readline(maximum + 1)
        if not line:
            return
        if len(line) > maximum or not line.endswith(b"\n"):
            raise RelayError("sse_line_limit_or_partial")
        size += len(line)
        if size > maximum:
            raise RelayError("sse_event_limit")
        line = line.rstrip(b"\r\n")
        if not line:
            if data:
                yield identifier, event, b"\n".join(data)
            data, size, event, identifier = [], 0, "message", None
            continue
        if line.startswith(b":"):
            continue
        field, _, value = line.partition(b":")
        if value.startswith(b" "):
            value = value[1:]
        if field == b"data":
            data.append(value)
        elif field == b"event":
            event = value.decode("utf-8")
        elif field == b"id":
            identifier = value.decode("utf-8")
            if len(value) > 256 or any(ord(c) < 32 or ord(c) > 126 for c in identifier):
                raise RelayError("invalid_sse_id")


def relay_request(data, maximum):
    envelope = json.loads(data)
    if not isinstance(envelope, dict) or not isinstance(envelope.get("body"), dict):
        raise RelayError("invalid_relay_envelope")
    body = json.dumps(envelope["body"], separators=(",", ":"), ensure_ascii=False,
                      allow_nan=False).encode("utf-8")
    if len(body) > maximum:
        raise RelayError("relay_body_limit")
    headers = Message()
    for field in ("x-hub-signature-256", "x-github-event", "x-github-delivery"):
        values = [v for k, v in envelope.items() if k.lower() == field]
        if len(values) != 1 or not isinstance(values[0], str) or len(values[0]) > 256:
            raise RelayError("invalid_relay_headers")
        if any(ord(c) < 32 or ord(c) > 126 for c in values[0]):
            raise RelayError("invalid_relay_headers")
        headers[field] = values[0]
    headers["Content-Type"] = "application/json"
    headers["Content-Length"] = str(len(body))
    return headers, body


class RelaySupervisor:
    def __init__(self, config, boundary, receiver, connection_factory=http.client.HTTPSConnection,
                 hooks=None):
        self.c, self.b, self.receiver = config, boundary, receiver
        self.connection_factory = connection_factory
        self.hooks = hooks
        if hooks is None and config["relay"]["register_hook"]:
            self.hooks = HookManager(config, boundary, receiver.secret)
        self.status_path = str(Path(boundary.VAR_DIR) / "relay-status.json")
        self.status = {"connected": False, "ready": False, "error": None,
                       "last_delivery": None, "hook_id": None, "state": "stopped"}
        self.lock = threading.Lock()
        self.connection = None
        self.socket = None
        self.last_event_id = None

    def publish(self, **changes):
        self.status.update(changes, updated_at=time.time())
        self.b.atomic_json(self.status_path, self.status)

    def close(self):
        with self.lock:
            if self.socket is not None:
                try:
                    self.socket.shutdown(socket.SHUT_RDWR)
                except OSError:
                    pass

    def process(self, identifier, data, stop):
        try:
            headers, body = relay_request(data, self.c["max_body_bytes"])
        except (ValueError, UnicodeError, RelayError, RecursionError):
            self.publish(error="invalid_relay_message", last_delivery={"status": "rejected"})
            return
        while not stop.is_set():
            try:
                code, status = self.receiver.receive(headers, body)
            except Exception:
                code, status = 503, "acceptance_failed"
            self.publish(last_delivery={"delivery_id": headers["X-GitHub-Delivery"],
                                        "http_status": code, "status": status},
                         error=None if code < 300 else status)
            if 200 <= code < 300:
                # The receiver persists the artifact before reporting durable acceptance.
                self.last_event_id = identifier or self.last_event_id
                return
            if code != 503:
                return
            if stop.wait(self.c["retry_seconds"]):
                return

    def run(self, stop):
        backoff, registered = 1, False
        try:
            while not stop.is_set():
                connection = None
                started = time.monotonic()
                try:
                    self.publish(state="connecting", connected=False, ready=False, error=None)
                    target = urlsplit(self.c["relay"]["url"])
                    connection = self.connection_factory(target.hostname, timeout=10)
                    with self.lock:
                        self.connection = connection
                    connection.connect()
                    with self.lock:
                        self.socket = connection.sock
                    if stop.is_set():
                        break
                    connection.sock.settimeout(45)
                    headers = {"Accept": "text/event-stream", "Cache-Control": "no-cache",
                               "User-Agent": "Octopus-GitHub-Issues"}
                    if self.last_event_id is not None:
                        headers["Last-Event-ID"] = self.last_event_id
                    connection.request("GET", target.path, headers=headers)
                    response = connection.getresponse()
                    if response.status != 200:
                        raise RelayError("relay_http_" + str(response.status))
                    content_type = response.getheader("Content-Type", "").split(";", 1)[0].strip().lower()
                    if content_type != "text/event-stream":
                        raise RelayError("relay_content_type_mismatch")
                    self.publish(state="connected", connected=True, ready=False)
                    if self.hooks is not None and not registered:
                        hook_id = self.hooks.reconcile_url(self.c["relay"]["url"])
                        self.publish(hook_id=hook_id)
                        registered = True
                    self.publish(state="ready", ready=True, error=None)
                    for identifier, event, data in sse_events(response, self.c["max_body_bytes"] * 2 + 65536):
                        if stop.is_set():
                            break
                        if event == "message":
                            self.process(identifier, data, stop)
                    if not stop.is_set():
                        raise RelayError("relay_stream_closed")
                except Exception as error:
                    if not stop.is_set():
                        self.publish(state="backoff", connected=False, ready=False,
                                     error=str(error) if isinstance(error, RelayError) else type(error).__name__,
                                     retry_seconds=backoff)
                finally:
                    if connection is not None:
                        connection.close()
                    with self.lock:
                        self.connection = None
                        self.socket = None
                if stop.wait(backoff):
                    break
                backoff = 1 if time.monotonic() - started > 120 else min(60, backoff * 2)
        finally:
            self.publish(state="stopped", connected=False, ready=False)
