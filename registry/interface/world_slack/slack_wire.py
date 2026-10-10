import base64
import hashlib
import http.client
import ipaddress
import json
import os
import re
import secrets
import socket
import ssl
import struct
import time
import urllib.error
import urllib.parse
import urllib.request


class SlackWireError(RuntimeError):
    def __init__(self, category, message, status=None, retry_after_ms=None, details=None):
        super().__init__(message)
        self.category = category
        self.status = status
        self.retry_after_ms = retry_after_ms
        self.details = details or {}


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def is_loopback_host(value):
    if not isinstance(value, str) or not value:
        return False
    host = value.rstrip(".").lower()
    if host == "localhost" or host.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def validate_proxy_url(value):
    if value is None or value == "":
        return ""
    if not isinstance(value, str) or value.strip() != value or any(ord(character) < 33 or ord(character) == 127 for character in value) or "\\" in value:
        raise ValueError("Slack proxy URL contains invalid characters")
    if "?" in value or "#" in value:
        raise ValueError("Slack proxy URL must not contain query or fragment data")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise ValueError("Slack proxy URL is malformed") from error
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.hostname is None:
        raise ValueError("Slack proxy URL must be an HTTP or HTTPS URL")
    if parsed.path not in {"", "/"}:
        raise ValueError("Slack proxy URL must not contain a path")
    if port is not None and not 1 <= port <= 65535:
        raise ValueError("Slack proxy URL port is invalid")
    if re.search(r"%(?![0-9A-Fa-f]{2})", parsed.netloc):
        raise ValueError("Slack proxy URL contains malformed escaping")
    try:
        username = urllib.parse.unquote(parsed.username, errors="strict") if parsed.username is not None else None
        password = urllib.parse.unquote(parsed.password, errors="strict") if parsed.password is not None else None
    except UnicodeDecodeError as error:
        raise ValueError("Slack proxy credentials are malformed") from error
    if (username is None) != (password is None) or username == "" or password == "":
        raise ValueError("Slack proxy credentials require a non-empty username and password")
    if username is not None and ":" in username:
        raise ValueError("Slack proxy username must not contain a colon")
    if any(item is not None and any(ord(character) < 32 or ord(character) == 127 for character in item) for item in (username, password)):
        raise ValueError("Slack proxy credentials contain invalid characters")
    if parsed.scheme != "https" and not is_loopback_host(parsed.hostname):
        raise ValueError("remote Slack proxies must use HTTPS")
    return value


class TlsTunnelSocket:
    def __init__(self, transport, server_hostname, timeout):
        self.transport = transport
        self.incoming = ssl.MemoryBIO()
        self.outgoing = ssl.MemoryBIO()
        self.tls = ssl.create_default_context().wrap_bio(self.incoming, self.outgoing, server_side=False, server_hostname=server_hostname)
        self.settimeout(timeout)
        self._drive(self.tls.do_handshake)

    def _flush(self):
        while self.outgoing.pending:
            self.transport.sendall(self.outgoing.read())

    def _feed(self):
        payload = self.transport.recv(65536)
        if not payload:
            self.incoming.write_eof()
            raise SlackWireError("transport_error", "proxy tunnel closed during TLS")
        self.incoming.write(payload)

    def _drive(self, operation):
        while True:
            try:
                result = operation()
                self._flush()
                return result
            except ssl.SSLWantReadError:
                self._flush()
                self._feed()
            except ssl.SSLWantWriteError:
                self._flush()

    def sendall(self, payload):
        view = memoryview(payload)
        offset = 0
        while offset < len(view):
            written = self._drive(lambda: self.tls.write(view[offset : offset + 65536]))
            if not written:
                raise SlackWireError("transport_error", "proxy tunnel TLS wrote zero bytes")
            offset += written

    def recv(self, maximum):
        return self._drive(lambda: self.tls.read(maximum))

    def settimeout(self, timeout):
        self.transport.settimeout(timeout)

    def close(self):
        self.transport.close()


def proxy_tunnel(proxy_url, target_host, target_port, timeout):
    parsed = urllib.parse.urlsplit(validate_proxy_url(proxy_url))
    proxy_port = parsed.port or (443 if parsed.scheme == "https" else 80)
    transport = socket.create_connection((parsed.hostname, proxy_port), timeout=timeout)
    try:
        if parsed.scheme == "https":
            transport = ssl.create_default_context().wrap_socket(transport, server_hostname=parsed.hostname)
        authority = f"{target_host}:{target_port}"
        headers = [f"CONNECT {authority} HTTP/1.1", f"Host: {authority}", "Proxy-Connection: keep-alive"]
        if parsed.username is not None:
            username = urllib.parse.unquote(parsed.username)
            password = urllib.parse.unquote(parsed.password or "")
            encoded = base64.b64encode(f"{username}:{password}".encode("utf-8")).decode("ascii")
            headers.append(f"Proxy-Authorization: Basic {encoded}")
        transport.sendall(("\r\n".join(headers) + "\r\n\r\n").encode("ascii"))
        response = bytearray()
        while b"\r\n\r\n" not in response:
            chunk = transport.recv(4096)
            if not chunk:
                raise SlackWireError("transport_error", "Slack proxy closed during CONNECT")
            response.extend(chunk)
            if len(response) > 65536:
                raise SlackWireError("malformed_response", "Slack proxy CONNECT response is too large")
        head, extra = bytes(response).split(b"\r\n\r\n", 1)
        if extra:
            raise SlackWireError("malformed_response", "Slack proxy sent bytes beyond CONNECT headers")
        first = head.split(b"\r\n", 1)[0]
        match = re.fullmatch(br"HTTP/1\.[01] ([0-9]{3})(?: .*)?", first)
        if match is None or int(match.group(1)) != 200:
            status = int(match.group(1)) if match is not None else None
            raise SlackWireError("proxy_refused", "Slack proxy refused CONNECT", status=status)
        return TlsTunnelSocket(transport, target_host, timeout)
    except Exception:
        transport.close()
        raise


def _read_bounded(response, maximum):
    length = response.headers.get("Content-Length")
    if length is not None:
        try:
            declared = int(length)
        except ValueError as error:
            raise SlackWireError("malformed_response", "response Content-Length is invalid") from error
        if declared < 0 or declared > maximum:
            raise SlackWireError("response_too_large", f"response declares {declared} bytes")
    body = response.read(maximum + 1)
    if len(body) > maximum:
        raise SlackWireError("response_too_large", f"response exceeds {maximum} bytes")
    return body


def _json_object(body, surface):
    try:
        value = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise SlackWireError("malformed_response", f"{surface} returned invalid JSON") from error
    if not isinstance(value, dict):
        raise SlackWireError("malformed_response", f"{surface} returned a non-object")
    return value


def _body_observation(body):
    return {
        "body_bytes": len(body),
        "body_sha256": hashlib.sha256(body).hexdigest(),
    }


def _slack_error_code(value):
    if isinstance(value, str) and re.fullmatch(r"[a-z0-9_]{1,128}", value):
        return value
    return "unknown_slack_error"


class SlackApi:
    def __init__(self, config, bot_tokens, app_token):
        self.config = config
        transport = config["transport"]
        base = os.environ.get(transport["api_base_env"], transport["api_base_default"])
        parsed = urllib.parse.urlparse(base)
        if parsed.scheme != "https" or not parsed.netloc or parsed.username or parsed.password:
            raise ValueError("Slack API base must be an absolute HTTPS URL without userinfo")
        self.base = base.rstrip("/")
        if not isinstance(bot_tokens, list) or not bot_tokens:
            raise ValueError("Slack bot_tokens must be a non-empty array")
        self.bot_tokens = bot_tokens
        self.bot_token = bot_tokens[0]
        self.team_tokens = {}
        self.app_token = app_token
        self.timeout = transport["request_timeout_ms"] / 1000.0
        self.maximum = transport["max_response_bytes"]
        self.rate_limit_max_wait_ms = transport["rate_limit_max_wait_ms"]
        self.read_retry_limit = transport["read_retry_limit"]
        self.proxy_url = validate_proxy_url(os.environ.get(transport["proxy_env"], ""))
        proxies = {"http": self.proxy_url, "https": self.proxy_url} if self.proxy_url else {}
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler(proxies), NoRedirect())

    def _request(self, url, token, body=None, headers=None, method=None, safe_retry=False, maximum=None):
        request_headers = {"User-Agent": "octopus-world-slack/1", "Accept": "application/json"}
        if token:
            request_headers["Authorization"] = f"Bearer {token}"
        if headers:
            request_headers.update(headers)
        maximum = self.maximum if maximum is None else maximum
        network_attempt = 0
        rate_retried = False
        while True:
            request = urllib.request.Request(url, data=body, headers=request_headers, method=method)
            try:
                with self.opener.open(request, timeout=self.timeout) as response:
                    return response.status, response.headers, _read_bounded(response, maximum)
            except urllib.error.HTTPError as error:
                response_body = _read_bounded(error, maximum)
                if error.code == 429:
                    retry_text = error.headers.get("Retry-After", "")
                    try:
                        retry_ms = max(0, int(float(retry_text) * 1000))
                    except ValueError:
                        retry_ms = None
                    if retry_ms is None or retry_ms > self.rate_limit_max_wait_ms or rate_retried:
                        raise SlackWireError(
                            "rate_limited",
                            "Slack rate limit exceeds configured wait ceiling",
                            error.code,
                            retry_ms,
                        )
                    time.sleep(retry_ms / 1000.0)
                    rate_retried = True
                    continue
                if safe_retry and error.code >= 500 and network_attempt < self.read_retry_limit:
                    time.sleep(min(0.25 * (2**network_attempt), 2.0))
                    network_attempt += 1
                    continue
                raise SlackWireError(
                    "http_error",
                    f"Slack HTTP request failed with status {error.code}",
                    error.code,
                    details=_body_observation(response_body),
                ) from error
            except (urllib.error.URLError, TimeoutError, OSError) as error:
                if safe_retry and network_attempt < self.read_retry_limit:
                    time.sleep(min(0.25 * (2**network_attempt), 2.0))
                    network_attempt += 1
                    continue
                raise SlackWireError(
                    "transport_error",
                    "Slack transport failed",
                    details={"exception": type(error).__name__},
                ) from error

    def call(self, method, params=None, app_auth=False, safe_retry=False, team_id=None, token_override=None):
        if not isinstance(method, str) or not method or method.strip() != method:
            raise ValueError("Slack method must be a non-empty trimmed string")
        if params is None:
            params = {}
        if not isinstance(params, dict):
            raise ValueError("Slack method params must be an object")
        if app_auth and self.app_token is None:
            raise SlackWireError("boundary_rejected", "Slack app authentication is unavailable to this interface instance")
        body = json.dumps(params, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        status, _, response_body = self._request(
            f"{self.base}/{method}",
            self.app_token if app_auth else (token_override or self.token_for_team(team_id)),
            body=body,
            headers={"Content-Type": "application/json; charset=utf-8"},
            method="POST",
            safe_retry=safe_retry,
        )
        response = _json_object(response_body, method)
        if status != 200 or response.get("ok") is not True:
            error_name = _slack_error_code(response.get("error"))
            raise SlackWireError(
                "slack_error",
                f"Slack {method} refused the request: {error_name}",
                status,
                details={"error_code": error_name, **_body_observation(response_body)},
            )
        return response

    def token_for_team(self, team_id):
        if team_id is not None:
            token = self.team_tokens.get(team_id)
            if token is None:
                raise SlackWireError("boundary_rejected", f"Slack workspace {team_id!r} has no authenticated bot token")
            return token
        if len(self.bot_tokens) != 1:
            raise SlackWireError("boundary_rejected", "Slack operation must identify a workspace when multiple bot tokens are configured")
        return self.bot_token

    def open_socket_url(self):
        override = os.environ.get(self.config["transport"]["socket_url_env"])
        if override:
            parsed = urllib.parse.urlparse(override)
            if parsed.scheme != "wss" or not parsed.hostname or parsed.username or parsed.password:
                raise ValueError("Slack socket URL override must be an absolute wss URL")
            return override
        response = self.call("apps.connections.open", {}, app_auth=True)
        url = response.get("url")
        if not isinstance(url, str):
            raise SlackWireError("malformed_response", "apps.connections.open returned no URL")
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "wss" or not parsed.hostname or parsed.username or parsed.password:
            raise SlackWireError("malformed_response", "apps.connections.open returned an invalid URL")
        return url

    def download(self, url, allowed_hosts, maximum, team_id=None):
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
            raise SlackWireError("boundary_rejected", "Slack download URL must be absolute HTTPS")
        hostname = parsed.hostname.lower().rstrip(".")
        if not any(hostname == host or hostname.endswith("." + host) for host in allowed_hosts):
            raise SlackWireError("boundary_rejected", f"Slack download host {hostname!r} is not allowed")
        status, headers, body = self._request(
            url,
            self.token_for_team(team_id),
            method="GET",
            safe_retry=True,
            maximum=maximum,
        )
        if status != 200:
            raise SlackWireError("http_error", f"Slack download returned status {status}", status)
        if len(body) > maximum:
            raise SlackWireError("response_too_large", f"Slack file exceeds {maximum} bytes")
        return body, headers.get("Content-Type")

    def upload_external(self, channel_id, filename, content, title=None, initial_comment=None, thread_ts=None, team_id=None):
        return self.upload_multiple_external(channel_id, [{"filename": filename, "content": content, "title": title}], initial_comment, thread_ts, team_id)

    def upload_multiple_external(self, channel_id, files, initial_comment=None, thread_ts=None, team_id=None):
        if not isinstance(files, list) or not files:
            raise ValueError("Slack upload files must be a non-empty array")
        completed = []
        for item in files:
            filename = item["filename"]
            content = item["content"]
            if len(content) > self.config["media"]["max_upload_bytes"]:
                raise SlackWireError("boundary_rejected", "Slack upload exceeds configured maximum")
            prepared = self.call("files.getUploadURLExternal", {"filename": filename, "length": len(content)}, team_id=team_id)
            upload_url = prepared.get("upload_url")
            file_id = prepared.get("file_id")
            if not isinstance(upload_url, str) or not isinstance(file_id, str):
                raise SlackWireError("malformed_response", "Slack upload preparation is incomplete")
            parsed = urllib.parse.urlparse(upload_url)
            if parsed.scheme != "https" or not parsed.hostname or parsed.username or parsed.password:
                raise SlackWireError("malformed_response", "Slack upload URL is invalid")
            hostname = parsed.hostname.lower().rstrip(".")
            allowed_hosts = self.config["media"]["allowed_upload_hosts"]
            if not any(hostname == host or hostname.endswith("." + host) for host in allowed_hosts):
                raise SlackWireError("boundary_rejected", f"Slack upload host {hostname!r} is not allowed")
            status, _, body = self._request(upload_url, "", body=content, headers={"Content-Type": "application/octet-stream"}, method="POST")
            if status < 200 or status >= 300:
                raise SlackWireError(
                    "http_error",
                    f"Slack upload body request failed with status {status}",
                    status,
                    details=_body_observation(body),
                )
            file_row = {"id": file_id}
            if item.get("title"):
                file_row["title"] = item["title"]
            completed.append(file_row)
        complete = {"files": completed, "channel_id": channel_id}
        if initial_comment:
            complete["initial_comment"] = initial_comment
        if thread_ts:
            complete["thread_ts"] = thread_ts
        return self.call("files.completeUploadExternal", complete, team_id=team_id)

    def post_response_url(self, response_url, body):
        parsed = urllib.parse.urlparse(response_url)
        if parsed.scheme != "https" or parsed.hostname not in {"hooks.slack.com", "hooks.slack-gov.com"}:
            raise SlackWireError("boundary_rejected", "Slack response URL host is not allowed")
        payload = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
        status, _, response = self._request(
            response_url,
            "",
            body=payload,
            headers={"Content-Type": "application/json; charset=utf-8"},
            method="POST",
        )
        if status < 200 or status >= 300:
            raise SlackWireError("http_error", f"Slack response URL returned status {status}", status)
        return {"ok": True, "status": status, **_body_observation(response)}


class WebSocket:
    def __init__(self, url, connect_timeout_ms, max_frame_bytes, proxy_url=""):
        self.url = url
        self.connect_timeout = connect_timeout_ms / 1000.0
        self.maximum = max_frame_bytes
        self.proxy_url = validate_proxy_url(proxy_url)
        self.sock = None
        self.buffer = bytearray()

    def connect(self):
        parsed = urllib.parse.urlparse(self.url)
        if parsed.scheme != "wss" or not parsed.hostname or parsed.username or parsed.password:
            raise SlackWireError("boundary_rejected", "Socket Mode URL must use wss")
        port = parsed.port or 443
        if self.proxy_url:
            sock = proxy_tunnel(self.proxy_url, parsed.hostname, port, self.connect_timeout)
        else:
            raw = socket.create_connection((parsed.hostname, port), timeout=self.connect_timeout)
            context = ssl.create_default_context()
            sock = context.wrap_socket(raw, server_hostname=parsed.hostname)
        path = parsed.path or "/"
        if parsed.query:
            path += "?" + parsed.query
        key = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
        request = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {parsed.hostname}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n"
            "User-Agent: octopus-world-slack/1\r\n\r\n"
        ).encode("ascii")
        sock.sendall(request)
        response = bytearray()
        while b"\r\n\r\n" not in response:
            chunk = sock.recv(4096)
            if not chunk:
                raise SlackWireError("transport_error", "Socket Mode handshake closed")
            response.extend(chunk)
            if len(response) > 65536:
                raise SlackWireError("malformed_response", "Socket Mode handshake is too large")
        head, extra = bytes(response).split(b"\r\n\r\n", 1)
        lines = head.split(b"\r\n")
        status_line = lines[0] if lines else b""
        if re.fullmatch(br"HTTP/1\.[01] 101(?: .*)?", status_line) is None:
            status_match = re.fullmatch(br"HTTP/1\.[01] ([0-9]{3})(?: .*)?", status_line)
            status = int(status_match.group(1)) if status_match is not None else None
            raise SlackWireError(
                "http_error",
                "Socket Mode handshake failed",
                status=status,
                details=_body_observation(head),
            )
        headers = {}
        for line in lines[1:]:
            if b":" not in line:
                raise SlackWireError("malformed_response", "Socket Mode handshake header is malformed")
            name, value = line.split(b":", 1)
            headers[name.decode("ascii").lower()] = value.decode("ascii").strip()
        expected = base64.b64encode(hashlib.sha1((key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11").encode("ascii")).digest()).decode("ascii")
        if headers.get("sec-websocket-accept") != expected:
            raise SlackWireError("malformed_response", "Socket Mode handshake accept digest mismatched")
        if headers.get("upgrade", "").lower() != "websocket" or "upgrade" not in {item.strip().lower() for item in headers.get("connection", "").split(",")}:
            raise SlackWireError("malformed_response", "Socket Mode handshake upgrade headers are invalid")
        sock.settimeout(None)
        self.sock = sock
        self.buffer = bytearray(extra)
        return self

    def close(self):
        sock = self.sock
        if sock is not None:
            try:
                self._send_frame(8, b"")
            except Exception:
                pass
            self.sock = None
            try:
                sock.close()
            except OSError:
                pass

    def _read_exact(self, count):
        while len(self.buffer) < count:
            if self.sock is None:
                raise SlackWireError("transport_error", "Socket Mode connection is closed")
            chunk = self.sock.recv(max(4096, count - len(self.buffer)))
            if not chunk:
                raise SlackWireError("transport_error", "Socket Mode connection ended")
            self.buffer.extend(chunk)
        value = bytes(self.buffer[:count])
        del self.buffer[:count]
        return value

    def _send_frame(self, opcode, payload):
        if self.sock is None:
            raise SlackWireError("transport_error", "Socket Mode connection is closed")
        if len(payload) > self.maximum:
            raise SlackWireError("boundary_rejected", "Socket Mode frame exceeds configured maximum")
        first = 0x80 | opcode
        mask = secrets.token_bytes(4)
        length = len(payload)
        if length < 126:
            header = bytes([first, 0x80 | length])
        elif length <= 65535:
            header = bytes([first, 0x80 | 126]) + struct.pack("!H", length)
        else:
            header = bytes([first, 0x80 | 127]) + struct.pack("!Q", length)
        masked = bytes(value ^ mask[index % 4] for index, value in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def send_json(self, value):
        self._send_frame(1, json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8"))

    def receive_json(self):
        fragments = bytearray()
        active_opcode = None
        while True:
            first, second = self._read_exact(2)
            final = bool(first & 0x80)
            opcode = first & 0x0F
            if first & 0x70:
                raise SlackWireError("malformed_response", "Socket Mode frame uses unsupported extension bits")
            masked = bool(second & 0x80)
            length = second & 0x7F
            if masked:
                raise SlackWireError("malformed_response", "Slack server sent a masked frame")
            if length == 126:
                length = struct.unpack("!H", self._read_exact(2))[0]
            elif length == 127:
                length = struct.unpack("!Q", self._read_exact(8))[0]
            if opcode in {8, 9, 10} and (not final or length > 125):
                raise SlackWireError("malformed_response", "Socket Mode control frame is invalid")
            if length > self.maximum:
                raise SlackWireError("response_too_large", "Socket Mode frame exceeds configured maximum")
            payload = self._read_exact(length)
            if opcode == 8:
                raise SlackWireError("transport_error", "Slack closed the Socket Mode connection")
            if opcode == 9:
                self._send_frame(10, payload)
                continue
            if opcode == 10:
                continue
            if opcode not in (0, 1):
                raise SlackWireError("malformed_response", f"unsupported Socket Mode opcode {opcode}")
            if opcode == 1:
                if active_opcode is not None:
                    raise SlackWireError("malformed_response", "Socket Mode started a nested fragmented message")
                active_opcode = opcode
            elif active_opcode is None:
                raise SlackWireError("malformed_response", "Socket Mode sent an orphan continuation frame")
            fragments.extend(payload)
            if len(fragments) > self.maximum:
                raise SlackWireError("response_too_large", "Socket Mode message exceeds configured maximum")
            if final:
                try:
                    value = json.loads(bytes(fragments))
                except (UnicodeDecodeError, json.JSONDecodeError) as error:
                    raise SlackWireError("malformed_response", "Socket Mode sent invalid JSON") from error
                if not isinstance(value, dict):
                    raise SlackWireError("malformed_response", "Socket Mode sent a non-object")
                return value
