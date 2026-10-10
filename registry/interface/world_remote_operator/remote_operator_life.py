#!/usr/bin/env python3
import base64
import datetime
import hashlib
import json
import os
import platform
import re
import socket
import ssl
import struct
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

from remote_operator_contract import ClientSegmentReassembler
from remote_operator_contract import ContractError
from remote_operator_contract import OPERATION_OUTPUT_LABELS
from remote_operator_contract import default_private_state
from remote_operator_contract import inbound_artifact
from remote_operator_contract import load_config
from remote_operator_contract import load_operation_catalog
from remote_operator_contract import normalize_clients
from remote_operator_contract import normalize_target
from remote_operator_contract import parse_rfc3339
from remote_operator_contract import public_status
from remote_operator_contract import read_json
from remote_operator_contract import redact
from remote_operator_contract import require_int
from remote_operator_contract import require_object
from remote_operator_contract import require_string
from remote_operator_contract import split_server_message
from remote_operator_contract import validate_activation
from remote_operator_contract import validate_private_state
from remote_operator_contract import validate_request
from remote_operator_io import RuntimeIO
from remote_operator_io import atomic_bytes
from remote_operator_io import atomic_json
from remote_operator_io import read_bounded_regular
from remote_operator_io import read_private_json


SAFE_OPERATIONS = frozenset(
    {
        "remote_operator.capabilities.status",
        "remote_operator.connection.disable",
        "remote_operator.connection.status",
        "remote_operator.pairing.status",
        "remote_operator.clients.list",
    }
)


class RemoteHttpError(RuntimeError):
    def __init__(self, message, status=None, retryable=False):
        super().__init__(message)
        self.status = status
        self.retryable = retryable


class RemoteHttp:
    def __init__(self, config):
        self.config = config
        transport = config["transport"]
        self.base_url = require_string(os.environ.get(transport["base_url_env"]), "remote operator base URL", 4096)
        self.target = normalize_target(self.base_url)
        self.account_id = require_string(os.environ.get(transport["account_id_env"]), "remote operator account id", 1024)
        self.server_name = require_string(os.environ.get(transport["server_name_env"]), "remote operator server name", 1024)
        self.installation_id = require_string(os.environ.get(transport["installation_id_env"]), "remote operator installation id", 1024)
        auth_kind = require_string(os.environ.get(transport["auth_kind_env"]), "remote operator auth kind", 64)
        if auth_kind != "chatgpt_account":
            raise ContractError("remote operator requires ChatGPT account auth; API keys are unsupported")
        self.auth_token_env = transport["auth_token_env"]
        self.last_evidence = None

    def auth_token(self):
        return require_string(os.environ.get(self.auth_token_env), "remote operator account token", 32768)

    def secret_values(self, extra=()):
        values = [os.environ.get(self.auth_token_env), self.account_id, self.installation_id]
        values.extend(value for value in extra if value)
        return values

    def sanitize(self, value, extra=()):
        return redact(value, self.secret_values(extra), self.config["protocol"]["body_preview_bytes"])

    def request(self, method, url, body=None, server_token=None, idempotency_key=None):
        raw = None if body is None else json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        token = server_token or self.auth_token()
        headers = {
            "Authorization": f"Bearer {token}",
            "Accept": "application/json",
            "User-Agent": self.config["transport"]["user_agent"],
            "ChatGPT-Account-Id": self.account_id,
            "x-codex-installation-id": self.installation_id,
        }
        if raw is not None:
            headers["Content-Type"] = "application/json"
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        request = urllib.request.Request(url, method=method, data=raw, headers=headers)
        timeout = self.config["transport"]["request_timeout_ms"] / 1000
        try:
            response = urllib.request.urlopen(request, timeout=timeout)
            with response:
                data = response.read(self.config["transport"]["max_response_bytes"] + 1)
                status = response.status
                request_id = response.headers.get("x-request-id")
        except urllib.error.HTTPError as error:
            data = error.read(min(self.config["transport"]["max_response_bytes"], self.config["protocol"]["body_preview_bytes"]))
            preview = data.decode("utf-8", errors="replace")
            raise RemoteHttpError(
                self.sanitize(f"remote operator {method} {urllib.parse.urlsplit(url).path} returned HTTP {error.code}: {preview}", (server_token,)),
                error.code,
                error.code == 429 or error.code >= 500,
            ) from error
        except (urllib.error.URLError, TimeoutError, socket.timeout, OSError) as error:
            raise RemoteHttpError(self.sanitize(f"remote operator {method} {urllib.parse.urlsplit(url).path} failed: {error}", (server_token,)), None, True) from error
        if len(data) > self.config["transport"]["max_response_bytes"]:
            raise RemoteHttpError("remote operator response exceeded its byte bound")
        if not data:
            value = {}
        else:
            try:
                value = require_object(json.loads(data), "remote operator HTTP response")
            except Exception as error:
                raise RemoteHttpError(self.sanitize(f"remote operator response is invalid JSON: {error}", (server_token,))) from error
        self.last_evidence = {"http_status": status, "request_id": request_id, "path": urllib.parse.urlsplit(url).path}
        return value

    def enroll(self, request_id):
        response = self.request(
            "POST",
            self.target["enroll_url"],
            {
                "name": self.server_name,
                "os": platform.system().lower(),
                "arch": platform.machine().lower(),
                "app_server_version": "octopus-remote-operator-interface/1",
                "installation_id": self.installation_id,
            },
            idempotency_key=request_id,
        )
        enrollment = {
            "base_url": self.target["base_url"],
            "account_id": self.account_id,
            "server_id": require_string(response.get("server_id"), "remote operator enrollment server_id", 4096),
            "environment_id": require_string(response.get("environment_id"), "remote operator enrollment environment_id", 4096),
            "server_name": self.server_name,
            "installation_id": self.installation_id,
            "remote_control_token": require_string(response.get("remote_control_token"), "remote operator enrollment token", 32768),
            "expires_at": require_string(response.get("expires_at"), "remote operator enrollment expiry", 128),
        }
        parse_rfc3339(enrollment["expires_at"], "remote operator enrollment expiry")
        return enrollment

    def refresh(self, enrollment):
        response = self.request(
            "POST",
            self.target["refresh_url"],
            {"server_id": enrollment["server_id"], "installation_id": self.installation_id},
        )
        server_id = require_string(response.get("server_id"), "remote operator refreshed server_id", 4096)
        environment_id = require_string(response.get("environment_id"), "remote operator refreshed environment_id", 4096)
        if server_id != enrollment["server_id"] or environment_id != enrollment["environment_id"]:
            raise ContractError("remote operator refresh returned mismatched server/environment identity")
        refreshed = dict(enrollment)
        refreshed["remote_control_token"] = require_string(response.get("remote_control_token"), "remote operator refreshed token", 32768)
        refreshed["expires_at"] = require_string(response.get("expires_at"), "remote operator refreshed expiry", 128)
        parse_rfc3339(refreshed["expires_at"], "remote operator refreshed expiry")
        return refreshed

    def pairing_start(self, enrollment, manual_code, request_id):
        response = self.request(
            "POST",
            self.target["pair_url"],
            {"manual_code": manual_code},
            server_token=enrollment["remote_control_token"],
            idempotency_key=request_id,
        )
        server_id = require_string(response.get("server_id"), "remote operator pairing server_id", 4096)
        environment_id = require_string(response.get("environment_id"), "remote operator pairing environment_id", 4096)
        if server_id != enrollment["server_id"] or environment_id != enrollment["environment_id"]:
            raise ContractError("remote operator pairing returned mismatched server/environment identity")
        pairing_code = require_string(response.get("pairing_code"), "remote operator pairing code", 1024)
        manual = response.get("manual_pairing_code")
        if manual is not None:
            manual = require_string(manual, "remote operator manual pairing code", 1024)
        expires_at = require_string(response.get("expires_at"), "remote operator pairing expiry", 128)
        parse_rfc3339(expires_at, "remote operator pairing expiry")
        return {
            "schema_version": 1,
            "pairing_code": pairing_code,
            "manual_pairing_code": manual,
            "server_id": server_id,
            "environment_id": environment_id,
            "expires_at": expires_at,
            "claimed": False,
        }

    def pairing_status(self, enrollment, arguments):
        body = {key: arguments[key] for key in ("pairing_code", "manual_pairing_code") if key in arguments}
        response = self.request("POST", self.target["pair_status_url"], body, server_token=enrollment["remote_control_token"])
        claimed = response.get("claimed")
        if not isinstance(claimed, bool):
            raise ContractError("remote operator pairing status has no claimed boolean")
        return {"schema_version": 1, "claimed": claimed, "environment_id": enrollment["environment_id"], "server_id": enrollment["server_id"]}

    def list_clients(self, arguments):
        environment_id = urllib.parse.quote(arguments["environment_id"], safe="")
        url = f"{self.target['environment_clients_url'].rstrip('/')}/{environment_id}/clients"
        query = urllib.parse.urlencode(
            {
                "limit": arguments.get("limit", self.config["contract"]["max_clients_limit"]),
                "order": arguments.get("order", "desc"),
                **({"cursor": arguments["cursor"]} if "cursor" in arguments else {}),
            }
        )
        return normalize_clients(self.request("GET", f"{url}?{query}"))

    def revoke_client(self, arguments, request_id):
        environment_id = urllib.parse.quote(arguments["environment_id"], safe="")
        client_id = urllib.parse.quote(arguments["client_id"], safe="")
        url = f"{self.target['environment_clients_url'].rstrip('/')}/{environment_id}/clients/{client_id}"
        self.request("DELETE", url, idempotency_key=request_id)
        return {"schema_version": 1, "environment_id": arguments["environment_id"], "client_id": arguments["client_id"], "revoked": True}


class WebSocketConnection:
    def __init__(self, url, headers, timeout, max_message_bytes):
        self.url = url
        self.headers = headers
        self.timeout = timeout
        self.max_message_bytes = max_message_bytes
        self.socket = None
        self.buffer = bytearray()
        self.send_lock = threading.Lock()
        self.closed = False
        self.fragment_opcode = None
        self.fragments = bytearray()

    def connect(self):
        parsed = urllib.parse.urlsplit(self.url)
        if parsed.scheme not in {"ws", "wss"} or not parsed.hostname:
            raise ContractError("remote operator websocket URL is invalid")
        port = parsed.port or (443 if parsed.scheme == "wss" else 80)
        raw_socket = socket.create_connection((parsed.hostname, port), timeout=self.timeout)
        try:
            if parsed.scheme == "wss":
                raw_socket = ssl.create_default_context().wrap_socket(raw_socket, server_hostname=parsed.hostname)
            raw_socket.settimeout(self.timeout)
            self.socket = raw_socket
            key = base64.b64encode(os.urandom(16)).decode("ascii")
            path = parsed.path or "/"
            if parsed.query:
                path += "?" + parsed.query
            default_port = 443 if parsed.scheme == "wss" else 80
            host_name = f"[{parsed.hostname}]" if ":" in parsed.hostname else parsed.hostname
            host = host_name if port == default_port else f"{host_name}:{port}"
            lines = [
                f"GET {path} HTTP/1.1",
                f"Host: {host}",
                "Upgrade: websocket",
                "Connection: Upgrade",
                f"Sec-WebSocket-Key: {key}",
                "Sec-WebSocket-Version: 13",
            ]
            lines.extend(f"{name}: {value}" for name, value in self.headers.items())
            raw_socket.sendall(("\r\n".join(lines) + "\r\n\r\n").encode("ascii"))
            header_bytes = self._read_headers(65536)
            header_lines = header_bytes.decode("iso-8859-1").split("\r\n")
            if not header_lines or " 101 " not in f" {header_lines[0]} ":
                raise RuntimeError(f"remote operator websocket handshake failed: {header_lines[0] if header_lines else '<empty>'}")
            response_headers = {}
            for line in header_lines[1:]:
                if ":" in line:
                    name, value = line.split(":", 1)
                    response_headers[name.strip().lower()] = value.strip()
            expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest()).decode("ascii")
            if response_headers.get("sec-websocket-accept") != expected:
                raise RuntimeError("remote operator websocket accept key mismatched")
            raw_socket.settimeout(1.0)
            self.closed = False
        except Exception:
            raw_socket.close()
            self.socket = None
            raise

    def _read_headers(self, maximum):
        while b"\r\n\r\n" not in self.buffer:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise RuntimeError("remote operator websocket closed during handshake")
            self.buffer.extend(chunk)
            if len(self.buffer) > maximum:
                raise RuntimeError("remote operator websocket handshake exceeded its bound")
        index = self.buffer.index(b"\r\n\r\n")
        value = bytes(self.buffer[:index])
        del self.buffer[: index + 4]
        return value

    def _recv_exact(self, size):
        while len(self.buffer) < size:
            chunk = self.socket.recv(max(4096, size - len(self.buffer)))
            if not chunk:
                raise EOFError("remote operator websocket closed")
            self.buffer.extend(chunk)
            if len(self.buffer) > self.max_message_bytes + 65536:
                raise RuntimeError("remote operator websocket receive buffer exceeded its bound")
        value = bytes(self.buffer[:size])
        del self.buffer[:size]
        return value

    def send_frame(self, opcode, payload=b""):
        if self.socket is None or self.closed:
            raise RuntimeError("remote operator websocket is not connected")
        payload = bytes(payload)
        if opcode >= 8 and len(payload) > 125:
            raise ContractError("remote operator websocket control frame is oversized")
        first = 0x80 | opcode
        length = len(payload)
        if length < 126:
            header = bytes((first, 0x80 | length))
        elif length <= 65535:
            header = bytes((first, 0x80 | 126)) + struct.pack("!H", length)
        else:
            header = bytes((first, 0x80 | 127)) + struct.pack("!Q", length)
        mask = os.urandom(4)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        with self.send_lock:
            self.socket.sendall(header + mask + masked)

    def send_json(self, value):
        raw = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        if len(raw) > self.max_message_bytes:
            raise ContractError("remote operator websocket JSON exceeds its bound")
        self.send_frame(1, raw)

    def recv_event(self):
        while True:
            header = self._recv_exact(2)
            if header[0] & 0x70:
                raise RuntimeError("remote operator websocket extensions are unsupported")
            final = bool(header[0] & 0x80)
            opcode = header[0] & 0x0F
            masked = bool(header[1] & 0x80)
            length = header[1] & 0x7F
            if masked:
                raise RuntimeError("remote operator server sent a masked websocket frame")
            if length == 126:
                length = struct.unpack("!H", self._recv_exact(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._recv_exact(8))[0]
            if opcode >= 8 and (not final or length > 125):
                raise RuntimeError("remote operator websocket control frame is invalid")
            if length > self.max_message_bytes:
                raise RuntimeError("remote operator websocket frame exceeds its bound")
            payload = self._recv_exact(length)
            if opcode == 8:
                self.closed = True
                self.fragment_opcode = None
                self.fragments.clear()
                return "close", payload
            if opcode == 9:
                self.send_frame(10, payload)
                return "ping", payload
            if opcode == 10:
                return "pong", payload
            if opcode in {1, 2}:
                if self.fragment_opcode is not None:
                    raise RuntimeError("remote operator websocket started nested fragmented data")
                self.fragment_opcode = opcode
                self.fragments.extend(payload)
            elif opcode == 0:
                if self.fragment_opcode is None:
                    raise RuntimeError("remote operator websocket continuation has no initial frame")
                self.fragments.extend(payload)
            else:
                raise RuntimeError(f"remote operator websocket opcode {opcode} is unsupported")
            if len(self.fragments) > self.max_message_bytes:
                raise RuntimeError("remote operator websocket message exceeds its bound")
            if final:
                fragment_opcode = self.fragment_opcode
                message = bytes(self.fragments)
                self.fragment_opcode = None
                self.fragments.clear()
                if fragment_opcode != 1:
                    raise RuntimeError("remote operator websocket binary messages are unsupported")
                try:
                    return "text", message.decode("utf-8")
                except UnicodeDecodeError as error:
                    raise RuntimeError("remote operator websocket text is not UTF-8") from error

    def close(self):
        if self.socket is None:
            self.closed = True
            return
        if not self.closed:
            try:
                self.send_frame(8, struct.pack("!H", 1000))
            except Exception:
                pass
        self.closed = True
        try:
            if self.socket is not None:
                self.socket.shutdown(socket.SHUT_RDWR)
        except Exception:
            pass
        if self.socket is not None:
            self.socket.close()
        self.socket = None


class RemoteOperator:
    def __init__(self, config, runtime, http):
        self.config = config
        self.runtime = runtime
        self.http = http
        self.state_path = os.path.join(runtime.private_dir, "remote-operator-state.json")
        self.outbound_dir = os.path.join(runtime.private_dir, "outbound")
        os.makedirs(self.outbound_dir, exist_ok=True)
        os.chmod(self.outbound_dir, 0o700)
        self.lock = threading.RLock()
        self.state_condition = threading.Condition(self.lock)
        self.websocket = None
        self.connection_thread = None
        self.stop_event = threading.Event()
        self.reassembler = ClientSegmentReassembler(config["protocol"])
        self.state = self._load_state()
        self.last_transport_evidence = None

    def _load_state(self):
        try:
            state = validate_private_state(read_private_json(self.state_path, 33554432, "remote operator private state"))
        except FileNotFoundError:
            state = default_private_state()
            atomic_json(self.state_path, state)
        if state["desired_mode"] == "ephemeral":
            state["desired_mode"] = "disabled"
            state["connection_state"] = "disabled"
            state["last_error"] = None
            atomic_json(self.state_path, state)
        return state

    def _commit(self):
        validate_private_state(self.state)
        atomic_json(self.state_path, self.state)
        self.state_condition.notify_all()

    def _status_payload(self, reason=None):
        payload = public_status(self.state, True, self.last_transport_evidence)
        if reason:
            payload["reason"] = reason
        return payload

    def emit_status(self, reason):
        with self.lock:
            payload = self._status_payload(reason)
        raw = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
        return self.runtime.hand_json("remote_operator.status", payload, f"remote-operator-status-{hashlib.sha256(raw).hexdigest()}")

    def start_if_durable(self):
        with self.lock:
            if self.state["desired_mode"] == "durable":
                self._ensure_connection_thread()

    def _enrollment_matches(self, enrollment):
        return enrollment is not None and enrollment["base_url"] == self.http.target["base_url"] and enrollment["account_id"] == self.http.account_id and enrollment["server_name"] == self.http.server_name and enrollment["installation_id"] == self.http.installation_id

    def _token_needs_refresh(self, enrollment):
        expiry = parse_rfc3339(enrollment["expires_at"], "remote operator token expiry")
        now = datetime.datetime.now(datetime.timezone.utc)
        return expiry <= now + datetime.timedelta(milliseconds=self.config["protocol"]["token_refresh_skew_ms"])

    def _ensure_enrollment(self, request_id):
        with self.lock:
            enrollment = self.state["enrollment"]
        if not self._enrollment_matches(enrollment):
            enrollment = self.http.enroll(request_id)
            with self.lock:
                self.state["enrollment"] = enrollment
                self.last_transport_evidence = {**(self.http.last_evidence or {}), "enrollment": "created"}
                self._commit()
            return enrollment
        if self._token_needs_refresh(enrollment):
            try:
                refreshed = self.http.refresh(enrollment)
            except RemoteHttpError as error:
                if error.status != 404:
                    raise
                refreshed = self.http.enroll(f"{request_id}-replacement")
                replacement = "replaced_after_not_found"
            else:
                replacement = "refreshed"
            with self.lock:
                self.state["enrollment"] = refreshed
                self.last_transport_evidence = {**(self.http.last_evidence or {}), "enrollment": replacement}
                self._commit()
            return refreshed
        return enrollment

    def _ensure_connection_thread(self):
        if self.connection_thread is not None and self.connection_thread.is_alive():
            return
        self.stop_event.clear()
        self.connection_thread = threading.Thread(target=self._connection_loop, daemon=True)
        self.connection_thread.start()

    def enable(self, mode, request_id):
        if mode not in {"ephemeral", "durable"}:
            raise ContractError("remote operator enable mode is invalid")
        with self.lock:
            self.state["desired_mode"] = mode
            if self.state["connection_state"] == "connected" and self.websocket is not None:
                self._commit()
                return self._status_payload("already_connected")
            self.state["connection_state"] = "connecting"
            self.state["last_error"] = None
            self._commit()
            self._ensure_connection_thread()
            deadline = time.monotonic() + self.config["protocol"]["enable_ready_timeout_ms"] / 1000
            while self.state["connection_state"] == "connecting":
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                self.state_condition.wait(remaining)
            return self._status_payload("enable_requested")

    def disable(self):
        with self.lock:
            self.state["desired_mode"] = "disabled"
            self.state["connection_state"] = "disabled"
            self.state["last_error"] = None
            self._commit()
            websocket = self.websocket
            connection_thread = self.connection_thread
            self.websocket = None
            self.stop_event.set()
        if websocket is not None:
            websocket.close()
        if connection_thread is not None and connection_thread is not threading.current_thread():
            connection_thread.join(self.config["protocol"]["shutdown_timeout_ms"] / 1000)
            with self.lock:
                if not connection_thread.is_alive() and self.connection_thread is connection_thread:
                    self.connection_thread = None
        return self.status("disable_completed")

    def status(self, reason=None):
        with self.lock:
            return self._status_payload(reason)

    def _websocket_headers(self, enrollment):
        headers = {
            "x-codex-server-id": enrollment["server_id"],
            "x-codex-name": base64.b64encode(enrollment["server_name"].encode("utf-8")).decode("ascii"),
            "x-codex-protocol-version": self.config["protocol"]["version"],
            "Authorization": f"Bearer {enrollment['remote_control_token']}",
            "x-codex-installation-id": enrollment["installation_id"],
        }
        with self.lock:
            if self.state["subscribe_cursor"]:
                headers["x-codex-subscribe-cursor"] = self.state["subscribe_cursor"]
        return headers

    def _connection_loop(self):
        backoff = self.config["protocol"]["reconnect_backoff_ms"]
        while not self.stop_event.is_set():
            websocket = None
            with self.lock:
                if self.state["desired_mode"] == "disabled":
                    return
                self.state["connection_state"] = "connecting"
                self.state["last_error"] = None
                self._commit()
            try:
                enrollment = self._ensure_enrollment("remote-operator-connect")
                websocket = WebSocketConnection(
                    self.http.target["websocket_url"],
                    self._websocket_headers(enrollment),
                    self.config["protocol"]["connect_timeout_ms"] / 1000,
                    self.config["protocol"]["segment_max_bytes"],
                )
                websocket.connect()
                with self.lock:
                    if self.state["desired_mode"] == "disabled":
                        websocket.close()
                        return
                    self.websocket = websocket
                    self.state["connection_state"] = "connected"
                    self.state["last_error"] = None
                    self.last_transport_evidence = {"websocket": "connected", "server_id": enrollment["server_id"], "environment_id": enrollment["environment_id"]}
                    self._commit()
                self.emit_status("connected")
                self._replay_buffer(websocket)
                backoff = self.config["protocol"]["reconnect_backoff_ms"]
                self._read_connection(websocket)
            except Exception as error:
                with self.lock:
                    if self.state["desired_mode"] == "disabled":
                        return
                    self.state["connection_state"] = "errored"
                    self.state["last_error"] = self.http.sanitize(error, ((self.state.get("enrollment") or {}).get("remote_control_token"),))
                    self.websocket = None
                    self._commit()
                try:
                    self.emit_status("connection_error")
                except Exception as evidence_error:
                    sys.stderr.write(f"remote operator status evidence failed: {self.http.sanitize(evidence_error)}\n")
                    sys.stderr.flush()
                if self.stop_event.wait(backoff / 1000):
                    return
                backoff = min(backoff * 2, self.config["protocol"]["reconnect_backoff_cap_ms"])
            finally:
                with self.lock:
                    if self.websocket is websocket:
                        self.websocket = None
                if websocket is not None:
                    websocket.close()

    def _read_connection(self, websocket):
        last_pong = time.monotonic()
        last_ping = 0.0
        while not self.stop_event.is_set():
            with self.lock:
                if self.state["desired_mode"] == "disabled" or self.websocket is not websocket:
                    return
            now = time.monotonic()
            if now - last_ping >= self.config["protocol"]["ping_interval_ms"] / 1000:
                websocket.send_frame(9, b"octopus")
                last_ping = now
            if now - last_pong >= self.config["protocol"]["pong_timeout_ms"] / 1000:
                raise TimeoutError("remote operator websocket pong timed out")
            try:
                event_type, payload = websocket.recv_event()
            except socket.timeout:
                continue
            if event_type in {"pong", "ping"}:
                last_pong = time.monotonic()
                continue
            if event_type == "close":
                raise EOFError("remote operator websocket closed")
            envelope = require_object(json.loads(payload), "remote operator websocket envelope")
            observation, event = self.reassembler.observe(envelope)
            if event.get("cursor"):
                with self.lock:
                    self.state["subscribe_cursor"] = event["cursor"]
                    self._commit()
            if observation == "pending":
                continue
            if observation == "duplicate":
                continue
            if observation == "message":
                artifact = inbound_artifact(event)
                key_body = json.dumps(artifact, sort_keys=True, separators=(",", ":")).encode("utf-8")
                self.runtime.hand_json("remote_operator.input", artifact, f"remote-operator-input-{hashlib.sha256(key_body).hexdigest()}")
            elif observation == "ack":
                self._ack(event)
            elif observation == "ping":
                self._send_pong(event)
            elif observation == "closed":
                self.runtime.hand_json(
                    "remote_operator.status",
                    {"schema_version": 1, "client_id": event["client_id"], "stream_id": event.get("stream_id"), "client_connection": "closed", "interface_connection": "connected"},
                    f"remote-operator-client-closed-{hashlib.sha256(json.dumps(event, sort_keys=True).encode()).hexdigest()}",
                )

    def _stream_key(self, client_id, stream_id):
        return f"{client_id}\0{stream_id}"

    def _next_seq(self, client_id, stream_id):
        key = self._stream_key(client_id, stream_id)
        with self.lock:
            seq = self.state["next_seq_by_stream"].get(key, 0)
            self.state["next_seq_by_stream"][key] = seq + 1
            self._commit()
            return seq

    def _send_pong(self, event):
        if event.get("stream_id") is None:
            return
        seq = self._next_seq(event["client_id"], event["stream_id"])
        envelope = {"type": "pong", "status": "active", "client_id": event["client_id"], "stream_id": event["stream_id"], "seq_id": seq}
        with self.lock:
            websocket = self.websocket
        if websocket is not None:
            websocket.send_json(envelope)

    def _buffer_file(self, envelope):
        raw = json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        digest = hashlib.sha256(raw).hexdigest()
        file_name = f"{digest}.json"
        path = os.path.join(self.outbound_dir, file_name)
        try:
            existing = read_bounded_regular(path, self.config["protocol"]["segment_max_bytes"], "remote operator buffered envelope")
        except FileNotFoundError:
            atomic_bytes(path, raw)
        else:
            if existing != raw:
                raise ContractError("remote operator buffered envelope digest collided")
        return file_name, digest

    def _buffer(self, envelopes):
        entries = []
        for envelope in envelopes:
            file_name, digest = self._buffer_file(envelope)
            entries.append(
                {
                    "file": file_name,
                    "sha256": digest,
                    "client_id": envelope["client_id"],
                    "stream_id": envelope["stream_id"],
                    "seq_id": envelope["seq_id"],
                    "segment_id": envelope.get("segment_id"),
                }
            )
        with self.lock:
            if len(self.state["outbound_buffer"]) + len(entries) > self.config["protocol"]["outbound_buffer_max"]:
                raise ContractError("remote operator outbound buffer is full")
            self.state["outbound_buffer"].extend(entries)
            self._commit()

    def _read_buffered(self, entry):
        raw = read_bounded_regular(os.path.join(self.outbound_dir, entry["file"]), self.config["protocol"]["segment_max_bytes"], "remote operator buffered envelope")
        if hashlib.sha256(raw).hexdigest() != entry["sha256"]:
            raise ContractError("remote operator buffered envelope failed SHA-256 verification")
        return require_object(json.loads(raw), "remote operator buffered envelope")

    def _replay_buffer(self, websocket):
        with self.lock:
            entries = list(self.state["outbound_buffer"])
        for entry in entries:
            websocket.send_json(self._read_buffered(entry))

    def _ack(self, event):
        key = self._stream_key(event["client_id"], event["stream_id"])
        ack_seq = event["seq_id"]
        ack_segment = event.get("segment_id")
        removed = []
        with self.lock:
            kept = []
            for item in self.state["outbound_buffer"]:
                same = item["client_id"] == event["client_id"] and item["stream_id"] == event["stream_id"]
                before = item["seq_id"] < ack_seq
                equal = item["seq_id"] == ack_seq and (ack_segment is None or item["segment_id"] is None or item["segment_id"] <= ack_segment)
                if same and (before or equal):
                    removed.append(item)
                else:
                    kept.append(item)
            self.state["outbound_buffer"] = kept
            self.state["acked_by_stream"][key] = max(self.state["acked_by_stream"].get(key, 0), ack_seq)
            self._commit()
        for item in removed:
            try:
                os.unlink(os.path.join(self.outbound_dir, item["file"]))
            except FileNotFoundError:
                pass
        payload = {
            "schema_version": 1,
            "client_id": event["client_id"],
            "stream_id": event["stream_id"],
            "seq_id": ack_seq,
            "segment_id": ack_segment,
            "acknowledged": True,
            "buffered_envelopes_released": len(removed),
        }
        self.runtime.hand_json("remote_operator.delivery", payload, f"remote-operator-ack-{hashlib.sha256(json.dumps(payload, sort_keys=True).encode()).hexdigest()}")

    def deliver(self, arguments):
        with self.lock:
            websocket = self.websocket
            connected = self.state["connection_state"] == "connected" and websocket is not None
        if not connected:
            raise ContractError("remote operator delivery requires current connected evidence")
        seq = self._next_seq(arguments["client_id"], arguments["stream_id"])
        envelopes = split_server_message(arguments["client_id"], arguments["stream_id"], seq, arguments["message"], self.config["protocol"])
        self._buffer(envelopes)
        sent = 0
        for envelope in envelopes:
            websocket.send_json(envelope)
            sent += 1
        return {
            "schema_version": 1,
            "client_id": arguments["client_id"],
            "stream_id": arguments["stream_id"],
            "seq_id": seq,
            "wire_envelopes_sent": sent,
            "segmented": len(envelopes) > 1,
            "acknowledged": False,
            "delivery_state": "awaiting_ack",
        }

    def pairing_start(self, arguments, request_id):
        with self.lock:
            if self.state["desired_mode"] == "disabled":
                raise ContractError("remote operator pairing is unavailable while disabled")
        enrollment = self._ensure_enrollment(request_id)
        return self.http.pairing_start(enrollment, arguments.get("manual_code", True), request_id)

    def pairing_status(self, arguments):
        with self.lock:
            if self.state["desired_mode"] == "disabled":
                raise ContractError("remote operator pairing status is unavailable while disabled")
        enrollment = self._ensure_enrollment("remote-operator-pairing-status")
        return self.http.pairing_status(enrollment, arguments)


def operation_label(operation):
    return OPERATION_OUTPUT_LABELS[operation]


def execute_operation(operator, request):
    operation = request["operation"]
    arguments = request["arguments"]
    request_id = request["request_id"]
    if operation == "remote_operator.capabilities.status":
        status = operator.status("capabilities_status")
        status.update(
            {
                "request_id": request_id,
                "wire_protocol_version": "3",
                "segment_target_bytes": operator.config["protocol"]["segment_target_bytes"],
                "segment_max_bytes": operator.config["protocol"]["segment_max_bytes"],
                "reassembled_max_bytes": operator.config["protocol"]["reassembled_max_bytes"],
                "apply_or_provider_authority": False,
                "runtime_timer": False,
                "private_interface_timers": True,
            }
        )
        return status
    if operation == "remote_operator.connection.enable":
        return {"request_id": request_id, **operator.enable(arguments.get("mode", "durable"), request_id)}
    if operation == "remote_operator.connection.disable":
        return {"request_id": request_id, **operator.disable()}
    if operation == "remote_operator.connection.status":
        return {"request_id": request_id, **operator.status("status_requested")}
    if operation == "remote_operator.pairing.start":
        return {"request_id": request_id, **operator.pairing_start(arguments, request_id)}
    if operation == "remote_operator.pairing.status":
        return {"request_id": request_id, **operator.pairing_status(arguments)}
    if operation == "remote_operator.clients.list":
        return {"request_id": request_id, **operator.http.list_clients(arguments), "service_evidence": operator.http.last_evidence}
    if operation == "remote_operator.clients.revoke":
        return {"request_id": request_id, **operator.http.revoke_client(arguments, request_id), "service_evidence": operator.http.last_evidence}
    if operation == "remote_operator.deliver":
        return {"request_id": request_id, **operator.deliver(arguments)}
    raise ContractError(f"remote operator operation {operation!r} is unsupported")


def process_outbound(runtime, operator, source_seq, content_hash, run, checkpoint):
    try:
        label, payload_bytes = runtime.materialize(content_hash)
        if label not in runtime.config["contract"]["accepted_labels"]:
            raise ContractError(f"remote operator rejects artifact label {label!r}")
        request = validate_request(require_object(json.loads(payload_bytes), "remote operator request payload"), runtime.config)
    except Exception as error:
        runtime.hand_json(
            "remote_operator.failure",
            {"schema_version": 1, "source_movement_seq": source_seq, "failure_class": "boundary_rejected", "error": operator.http.sanitize(error)},
            f"remote-operator-failure-{source_seq}",
        )
        return
    effect = runtime.load_effect(source_seq)
    if effect is not None and effect["source_hash"] != content_hash:
        raise ContractError("remote operator effect journal source hash mismatched")
    if effect is not None and effect["status"] == "evidenced":
        return
    if effect is not None and effect["status"] == "succeeded":
        evidence_seq = runtime.hand_json(effect["label"], effect["payload"], f"remote-operator-result-{source_seq}")
        effect.update({"status": "evidenced", "evidence_seq": evidence_seq})
        runtime.commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] in {"failed", "indeterminate"}:
        evidence_seq = runtime.hand_json(
            "remote_operator.failure",
            {
                "schema_version": 1,
                "source_movement_seq": source_seq,
                "request_id": effect["request_id"],
                "operation": effect["operation"],
                "failure_class": effect.get("failure_class", "indeterminate_external_effect"),
                "error": effect.get("error"),
            },
            f"remote-operator-result-{source_seq}",
        )
        effect.update({"status": "evidenced", "evidence_seq": evidence_seq})
        runtime.commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] == "started" and request["operation"] not in SAFE_OPERATIONS:
        effect.update({"status": "indeterminate", "failure_class": "indeterminate_external_effect", "error": "remote operator effect may have reached its private wire before interruption; it was not retried"})
        runtime.commit_effect(source_seq, effect)
        process_outbound(runtime, operator, source_seq, content_hash, run, checkpoint)
        return
    effect = {
        "schema_version": 1,
        "source_seq": source_seq,
        "source_hash": content_hash,
        "request_id": request["request_id"],
        "operation": request["operation"],
        "status": "started",
    }
    runtime.commit_effect(source_seq, effect)
    try:
        payload = execute_operation(operator, request)
    except ContractError as error:
        effect.update({"status": "failed", "failure_class": "boundary_rejected", "error": operator.http.sanitize(error)})
        runtime.commit_effect(source_seq, effect)
        process_outbound(runtime, operator, source_seq, content_hash, run, checkpoint)
        return
    except Exception as error:
        status = "failed" if request["operation"] in SAFE_OPERATIONS else "indeterminate"
        effect.update({"status": status, "failure_class": "transport_failure" if status == "failed" else "indeterminate_external_effect", "error": operator.http.sanitize(error)})
        runtime.commit_effect(source_seq, effect)
        process_outbound(runtime, operator, source_seq, content_hash, run, checkpoint)
        return
    effect.update({"status": "succeeded", "label": operation_label(request["operation"]), "payload": payload})
    runtime.commit_effect(source_seq, effect)
    process_outbound(runtime, operator, source_seq, content_hash, run, checkpoint)


def main():
    interface_dir = os.environ["OCTOPUS_INTERFACE_DIR"]
    if os.environ["OCTOPUS_INTERFACE_ID"] != "world_remote_operator":
        raise ContractError("remote operator life was started for the wrong exact interface")
    config = load_config(os.path.join(interface_dir, "config.json"))
    validate_activation(read_json(os.path.join(interface_dir, "activation.json"), "remote operator activation"))
    load_operation_catalog(interface_dir)
    runtime = RuntimeIO(config)
    http = RemoteHttp(config)
    operator = RemoteOperator(config, runtime, http)
    runtime.start()
    runtime.ready()
    operator.start_if_durable()
    sys.stderr.write("remote operator interface live: exact_interface=world_remote_operator protocol=3 app_server_rpc=false\n")
    sys.stderr.flush()
    next_seq = runtime.load_cursor()
    while True:
        rows = runtime.read_outbound(next_seq)
        for source_seq, content_hash, run, checkpoint in rows:
            try:
                process_outbound(runtime, operator, source_seq, content_hash, run, checkpoint)
            except Exception as error:
                sys.stderr.write(f"remote operator movement {source_seq} retained after error: {http.sanitize(error)}\n")
                sys.stderr.flush()
                time.sleep(0.25)
                break
            next_seq = source_seq + 1
            runtime.commit_cursor(next_seq)
        time.sleep(config["driver_ipc"]["idle_sleep_ms"] / 1000)


if __name__ == "__main__":
    main()
