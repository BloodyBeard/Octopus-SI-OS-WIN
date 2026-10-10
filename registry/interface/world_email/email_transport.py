import base64
import datetime
import email
import fnmatch
import hashlib
import imaplib
import ipaddress
import json
import os
import re
import select as select_module
import smtplib
import socket
import ssl
import stat
import time
import threading
import urllib.parse
from contextlib import contextmanager
from email import policy
from email.message import EmailMessage
from email.parser import BytesParser
from typing import Any, Iterator

from octopus_content_hash import octopus_content_hash

from email_contract import ContractError
from email_contract import build_outbound_message
from email_contract import decode_header_value
from email_contract import env_csv
from email_contract import env_flag
from email_contract import normalize_address
from email_contract import normalize_subject
from email_contract import parse_addresses
from email_contract import parse_inbound_message
from email_contract import persist_content_addressed
from email_contract import require_bool
from email_contract import require_flag
from email_contract import require_int
from email_contract import require_label
from email_contract import require_mailbox
from email_contract import require_object
from email_contract import require_string
from email_contract import require_uid
from email_contract import safe_message_id
from email_contract import validate_sha256


class TransportError(RuntimeError):
    def __init__(self, message: str, retryable: bool, details: dict[str, Any] | None = None):
        super().__init__(message)
        self.retryable = retryable
        self.details = details or {}


class MessageBoundaryError(ContractError):
    def __init__(self, message: str, partial: bytes, details: dict[str, Any]):
        super().__init__(message)
        self.partial = partial
        self.details = details


def read_rooted_regular(root: str, path: str, maximum: int) -> bytes:
    root = os.path.realpath(root)
    path = os.path.realpath(path)
    if os.path.commonpath((path, root)) != root or path == root:
        raise ContractError("attachment path is outside its declared root")
    components = os.path.relpath(path, root).split(os.sep)
    if not components or any(component in ("", ".", "..") for component in components):
        raise ContractError("attachment path is malformed")
    directory_flags = os.O_RDONLY | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptors: list[int] = []
    file_descriptor = None
    try:
        descriptors.append(os.open(root, directory_flags))
        for component in components[:-1]:
            descriptors.append(os.open(component, directory_flags, dir_fd=descriptors[-1]))
        file_descriptor = os.open(components[-1], os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0), dir_fd=descriptors[-1])
        if not stat.S_ISREG(os.fstat(file_descriptor).st_mode):
            raise ContractError("attachment path is not a regular file")
        result = bytearray()
        while len(result) <= maximum:
            chunk = os.read(file_descriptor, min(65536, maximum + 1 - len(result)))
            if not chunk:
                break
            result.extend(chunk)
        if len(result) > maximum:
            raise ContractError("file exceeds its declared byte ceiling")
        return bytes(result)
    except OSError as error:
        raise ContractError("attachment path could not be opened without traversing symlinks") from error
    finally:
        if file_descriptor is not None:
            os.close(file_descriptor)
        for descriptor in reversed(descriptors):
            os.close(descriptor)


def _family_value(name: str) -> int:
    if name == "ipv4":
        return socket.AF_INET
    if name == "ipv6":
        return socket.AF_INET6
    return socket.AF_UNSPEC


def _is_lexical_loopback(host: str) -> bool:
    normalized = host.rstrip(".").lower()
    if normalized == "localhost" or normalized.endswith(".localhost"):
        return True
    try:
        return ipaddress.ip_address(normalized).is_loopback
    except ValueError:
        return False


def _create_connection(host: str, port: int, timeout: float, attempt_timeout: float, family: int) -> socket.socket:
    errors: list[str] = []
    deadline = time.monotonic() + timeout
    for address_family, socktype, protocol, _, address in socket.getaddrinfo(
        host,
        port,
        family,
        socket.SOCK_STREAM,
    ):
        sock = socket.socket(address_family, socktype, protocol)
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            sock.close()
            break
        sock.settimeout(min(attempt_timeout, remaining))
        try:
            sock.connect(address)
            sock.settimeout(timeout)
            return sock
        except OSError as error:
            errors.append(f"{address!r}: {error}")
            sock.close()
    raise OSError(f"unable to connect to {host}:{port}: {'; '.join(errors) or 'no addresses'}")


class FamilyIMAP4(imaplib.IMAP4):
    def __init__(self, host: str, port: int, timeout: float, attempt_timeout: float, family: int):
        self._octopus_family = family
        self._octopus_attempt_timeout = attempt_timeout
        super().__init__(host, port, timeout=timeout)

    def _create_socket(self, timeout: float) -> socket.socket:
        return _create_connection(self.host, self.port, timeout, self._octopus_attempt_timeout, self._octopus_family)


class FamilyIMAP4SSL(imaplib.IMAP4_SSL):
    def __init__(self, host: str, port: int, timeout: float, attempt_timeout: float, family: int, context: ssl.SSLContext):
        self._octopus_family = family
        self._octopus_attempt_timeout = attempt_timeout
        super().__init__(host, port, ssl_context=context, timeout=timeout)

    def _create_socket(self, timeout: float) -> socket.socket:
        raw = _create_connection(self.host, self.port, timeout, self._octopus_attempt_timeout, self._octopus_family)
        return self.ssl_context.wrap_socket(raw, server_hostname=self.host)


class FamilySMTP(smtplib.SMTP):
    def __init__(self, host: str, port: int, timeout: float, attempt_timeout: float, family: int):
        self._octopus_family = family
        self._octopus_attempt_timeout = attempt_timeout
        super().__init__(host, port, timeout=timeout)

    def _get_socket(self, host: str, port: int, timeout: float) -> socket.socket:
        return _create_connection(host, port, timeout, self._octopus_attempt_timeout, self._octopus_family)


class FamilySMTPSSL(smtplib.SMTP_SSL):
    def __init__(self, host: str, port: int, timeout: float, attempt_timeout: float, family: int, context: ssl.SSLContext):
        self._octopus_family = family
        self._octopus_attempt_timeout = attempt_timeout
        super().__init__(host, port, timeout=timeout, context=context)

    def _get_socket(self, host: str, port: int, timeout: float) -> socket.socket:
        raw = _create_connection(host, port, timeout, self._octopus_attempt_timeout, self._octopus_family)
        return self.context.wrap_socket(raw, server_hostname=host)


def encode_modified_utf7(value: str) -> str:
    output: list[str] = []
    buffered: list[str] = []

    def flush() -> None:
        if not buffered:
            return
        raw = "".join(buffered).encode("utf-16be")
        encoded = base64.b64encode(raw).decode("ascii").rstrip("=").replace("/", ",")
        output.append(f"&{encoded}-")
        buffered.clear()

    for character in value:
        codepoint = ord(character)
        if 0x20 <= codepoint <= 0x7E:
            flush()
            output.append("&-" if character == "&" else character)
        else:
            buffered.append(character)
    flush()
    return "".join(output)


def decode_modified_utf7(value: str) -> str:
    output: list[str] = []
    index = 0
    while index < len(value):
        if value[index] != "&":
            output.append(value[index])
            index += 1
            continue
        end = value.find("-", index)
        if end < 0:
            raise ContractError("malformed modified UTF-7 mailbox name")
        payload = value[index + 1 : end]
        if not payload:
            output.append("&")
        else:
            encoded = payload.replace(",", "/")
            encoded += "=" * ((4 - len(encoded) % 4) % 4)
            try:
                output.append(base64.b64decode(encoded).decode("utf-16be"))
            except (ValueError, UnicodeDecodeError) as error:
                raise ContractError("malformed modified UTF-7 mailbox name") from error
        index = end + 1
    return "".join(output)


def quote_imap(value: str) -> str:
    if "\x00" in value or "\r" in value or "\n" in value:
        raise ContractError("IMAP value contains forbidden characters")
    return '"' + value.replace("\\", "\\\\").replace('"', '\\"') + '"'


def _response_text(data: Any) -> str:
    if isinstance(data, bytes):
        return data.decode("utf-8", errors="replace")
    if isinstance(data, str):
        return data
    if isinstance(data, (list, tuple)):
        return " ".join(_response_text(item) for item in data if item is not None)
    return repr(data)


def _parse_uidvalidity(imap: imaplib.IMAP4) -> int:
    _, values = imap.response("UIDVALIDITY")
    text = _response_text(values)
    match = re.search(r"\b(\d+)\b", text)
    if not match:
        raise TransportError("selected mailbox did not expose UIDVALIDITY", False, {"response": text})
    return int(match.group(1))


def _parse_append_uid(imap: imaplib.IMAP4, data: Any) -> dict[str, int] | None:
    candidates = [_response_text(data), _response_text(imap.response("APPENDUID")[1])]
    for text in candidates:
        match = re.search(r"APPENDUID\s+(\d+)\s+(\d+)", text, flags=re.IGNORECASE)
        if match:
            return {"uidvalidity": int(match.group(1)), "uid": int(match.group(2))}
    return None


def _parse_internal_date(value: bytes | str) -> str | None:
    text = _response_text(value)
    match = re.search(r'INTERNALDATE\s+"([^"]+)"', text, flags=re.IGNORECASE)
    if not match:
        return None
    try:
        return datetime.datetime.strptime(match.group(1), "%d-%b-%Y %H:%M:%S %z").isoformat()
    except ValueError:
        return None


def _parse_fetch_payload(data: Any) -> tuple[bytes, list[str], str | None]:
    if not isinstance(data, list):
        raise TransportError("IMAP FETCH returned a non-list response", True, {"response": _response_text(data)})
    raw: bytes | None = None
    flags: list[str] = []
    internal_date: str | None = None
    for item in data:
        if not isinstance(item, tuple) or len(item) < 2:
            continue
        header, body = item[0], item[1]
        if isinstance(body, bytes):
            raw = body
        try:
            flags.extend(flag.decode("ascii", errors="replace") for flag in imaplib.ParseFlags(header))
        except Exception:
            pass
        internal_date = _parse_internal_date(header) or internal_date
    if raw is None:
        raise TransportError("IMAP FETCH returned no message bytes", True, {"response": _response_text(data)[:4096]})
    return raw, sorted(set(flags)), internal_date


def _mailbox_list_row(value: bytes | str) -> dict[str, Any]:
    text = _response_text(value)
    match = re.match(r'^\((?P<flags>[^)]*)\)\s+(?P<delimiter>NIL|"(?:[^"\\]|\\.)*")\s+(?P<name>.+)$', text)
    if not match:
        raise TransportError("IMAP LIST returned a malformed row", False, {"row": text})
    flags = [item for item in match.group("flags").split() if item]
    delimiter_raw = match.group("delimiter")
    delimiter = None if delimiter_raw == "NIL" else delimiter_raw[1:-1].replace("\\\"", '"').replace("\\\\", "\\")
    name_raw = match.group("name")
    if name_raw.startswith('"') and name_raw.endswith('"'):
        name_raw = name_raw[1:-1].replace("\\\"", '"').replace("\\\\", "\\")
    return {"name": decode_modified_utf7(name_raw), "delimiter": delimiter, "flags": flags}


def _structured_search(query: Any) -> list[str]:
    if query is None:
        return ["ALL"]
    obj = require_object(query, "email search query")
    allowed = {
        "from",
        "to",
        "cc",
        "bcc",
        "subject",
        "body",
        "text",
        "since",
        "before",
        "on",
        "seen",
        "flagged",
        "answered",
        "draft",
        "deleted",
        "larger",
        "smaller",
        "header",
        "uid",
    }
    unknown = sorted(set(obj) - allowed)
    if unknown:
        raise ContractError(f"email search query has unknown keys: {', '.join(unknown)}")
    criteria: list[str] = []
    for key, imap_key in (("from", "FROM"), ("to", "TO"), ("cc", "CC"), ("bcc", "BCC"), ("subject", "SUBJECT"), ("body", "BODY"), ("text", "TEXT")):
        if key in obj:
            criteria.extend([imap_key, quote_imap(require_string(obj[key], f"email search {key}"))])
    for key, imap_key in (("since", "SINCE"), ("before", "BEFORE"), ("on", "ON")):
        if key in obj:
            value = require_string(obj[key], f"email search {key}")
            try:
                parsed = datetime.date.fromisoformat(value)
            except ValueError as error:
                raise ContractError(f"email search {key} must use YYYY-MM-DD") from error
            criteria.extend([imap_key, parsed.strftime("%d-%b-%Y")])
    for key, positive, negative in (
        ("seen", "SEEN", "UNSEEN"),
        ("flagged", "FLAGGED", "UNFLAGGED"),
        ("answered", "ANSWERED", "UNANSWERED"),
        ("draft", "DRAFT", "UNDRAFT"),
        ("deleted", "DELETED", "UNDELETED"),
    ):
        if key in obj:
            criteria.append(positive if require_bool(obj[key], f"email search {key}") else negative)
    for key, imap_key in (("larger", "LARGER"), ("smaller", "SMALLER")):
        if key in obj:
            criteria.extend([imap_key, str(require_int(obj[key], f"email search {key}", 0, 2147483647))])
    if "header" in obj:
        headers = require_object(obj["header"], "email search header")
        for name, value in sorted(headers.items()):
            header_name = require_string(name, "email search header name")
            if not re.fullmatch(r"[A-Za-z0-9-]{1,128}", header_name):
                raise ContractError(f"invalid email search header name {header_name!r}")
            criteria.extend(["HEADER", header_name, quote_imap(require_string(value, f"email search header {header_name}"))])
    if "uid" in obj:
        uid_set = require_string(obj["uid"], "email search uid")
        if not re.fullmatch(r"(?:\d+|\*)(?::(?:\d+|\*))?(?:,(?:\d+|\*)(?::(?:\d+|\*))?)*", uid_set):
            raise ContractError("email search uid is not a valid IMAP sequence set")
        criteria.extend(["UID", uid_set])
    return criteria or ["ALL"]


class EmailTransport:
    def __init__(self, config: dict[str, Any], var_dir: str, blobs_dir: str):
        self.config = config
        self.var_dir = var_dir
        self.blobs_dir = blobs_dir
        self.media_dir = os.path.join(var_dir, "media")
        self.raw_dir = os.path.join(var_dir, "raw")
        os.makedirs(self.media_dir, exist_ok=True)
        os.makedirs(self.raw_dir, exist_ok=True)
        account = config["account"]
        self.account_id = account["id"]
        self.address = normalize_address(self._required_env(account["address_env"]))
        self.display_name = account["display_name"]
        self.auth = account["auth"]
        self.imap_config = account["imap"]
        self.smtp_config = account["smtp"]
        self.mailboxes = account["mailboxes"]
        self.scopes = config["scopes"]
        self._last_action_at = 0.0
        self._action_lock = threading.Lock()

    def preflight_environment(self) -> dict[str, Any]:
        imap_host = self._required_env(self.imap_config["host_env"])
        smtp_host = self._required_env(self.smtp_config["host_env"])
        self._require_transport_security("imap", self.imap_config, imap_host)
        self._require_transport_security("smtp", self.smtp_config, smtp_host)
        mode, credential = self._credentials()
        if not credential:
            raise ContractError("email credential is empty")
        for alias in self.config["intake"]["mailboxes"]:
            mailbox = self.mailboxes[alias]
            self._require_mailbox_scope(mailbox, "read")
            if self.config["intake"]["mark_seen_after_admission"]:
                self._require_mailbox_scope(mailbox, "write")
        if self.smtp_config["save_sent"]:
            self._require_mailbox_scope(self.mailboxes["sent"], "write")
        return {
            "account": self.account_id,
            "address": self.address,
            "auth_mode": mode,
            "imap_host": imap_host,
            "imap_port": self._port(self.imap_config),
            "smtp_host": smtp_host,
            "smtp_port": self._port(self.smtp_config),
        }

    def _tls_context(self) -> ssl.SSLContext:
        context = ssl.create_default_context()
        context.minimum_version = ssl.TLSVersion.TLSv1_2
        return context

    def _require_transport_security(self, name: str, transport: dict[str, Any], host: str) -> None:
        if transport["tls"] != "plain":
            return
        if not transport["allow_plaintext"]:
            raise ContractError(f"email {name} plaintext transport requires explicit allow_plaintext")
        if not _is_lexical_loopback(host):
            raise ContractError(f"email {name} plaintext transport is restricted to lexical loopback hosts")

    def _credential_variants(self) -> tuple[str, ...]:
        credentials = []
        for env_name in (self.auth["password_env"], self.auth["oauth_access_token_env"]):
            value = os.environ.get(env_name, "").strip()
            if value:
                credentials.append(value)
        variants: set[str] = set()
        for credential in credentials:
            payloads: set[str] = {
                credential,
                f"Bearer {credential}",
                f"user={self.address}\x01auth=Bearer {credential}\x01\x01",
                f"\x00{self.address}\x00{credential}",
                f"{self.address}\x00{self.address}\x00{credential}",
            }
            for payload in payloads:
                encoded = payload.encode("utf-8")
                standard = base64.b64encode(encoded).decode("ascii")
                urlsafe = base64.urlsafe_b64encode(encoded).decode("ascii")
                variants.update({payload, standard, standard.rstrip("="), urlsafe, urlsafe.rstrip("=")})
            xoauth = base64.b64encode(
                f"user={self.address}\x01auth=Bearer {credential}\x01\x01".encode("utf-8")
            ).decode("ascii")
            variants.update({f"XOAUTH2 {xoauth}", f"AUTH XOAUTH2 {xoauth}"})
        for value in tuple(variants):
            quoted = urllib.parse.quote(value, safe="")
            quoted_plus = urllib.parse.quote_plus(value, safe="")
            variants.update(
                {
                    quoted,
                    quoted_plus,
                    re.sub(r"%[0-9A-F]{2}", lambda match: match.group(0).lower(), quoted),
                    re.sub(r"%[0-9A-F]{2}", lambda match: match.group(0).lower(), quoted_plus),
                }
            )
        return tuple(sorted((value for value in variants if value), key=len, reverse=True))

    def _sanitize_text(self, value: str) -> str:
        for variant in self._credential_variants():
            value = value.replace(variant, "[REDACTED_EMAIL_CREDENTIAL]")
        return value

    def _safe_remote_observation(self, value: Any) -> dict[str, Any]:
        if isinstance(value, bytes):
            raw = value
        elif isinstance(value, str):
            raw = value.encode("utf-8", errors="replace")
        else:
            raw = json.dumps(value, sort_keys=True, separators=(",", ":"), default=str).encode("utf-8")
        return {"bytes": len(raw), "sha256": hashlib.sha256(raw).hexdigest()}

    def _sanitize_details(self, value: Any) -> Any:
        if isinstance(value, dict):
            result: dict[str, Any] = {}
            for key, item in value.items():
                safe_key = self._sanitize_text(str(key))
                if str(key).lower() in {"response", "smtp_error", "row"}:
                    observation = self._safe_remote_observation(item)
                    result[f"{safe_key}_bytes"] = observation["bytes"]
                    result[f"{safe_key}_sha256"] = observation["sha256"]
                else:
                    result[safe_key] = self._sanitize_details(item)
            return result
        if isinstance(value, list):
            return [self._sanitize_details(item) for item in value]
        if isinstance(value, tuple):
            return [self._sanitize_details(item) for item in value]
        if isinstance(value, bytes):
            return self._sanitize_text(value.decode("utf-8", errors="replace"))
        if isinstance(value, str):
            return self._sanitize_text(value)
        return value

    def _sanitize_transport_error(self, error: TransportError) -> TransportError:
        error.args = (self._sanitize_text(str(error)),)
        error.details = self._sanitize_details(error.details)
        error.__cause__ = None
        error.__context__ = None
        error.__suppress_context__ = True
        return error

    def _retire_source_exception(self, error: BaseException) -> None:
        error.args = (type(error).__name__,)
        if isinstance(error, smtplib.SMTPResponseException):
            error.smtp_error = b""
        error.__cause__ = None
        error.__context__ = None
        error.__suppress_context__ = True

    def _transport_error(
        self,
        message: str,
        retryable: bool,
        details: dict[str, Any] | None = None,
    ) -> TransportError:
        return self._sanitize_transport_error(TransportError(message, retryable, details))

    def _required_env(self, name: str) -> str:
        value = os.environ.get(name, "").strip()
        if not value:
            raise ContractError(f"required email environment variable {name} is missing")
        return value

    def _port(self, transport: dict[str, Any]) -> int:
        raw = os.environ.get(transport["port_env"], "").strip()
        if not raw:
            return transport["port_default"]
        try:
            port = int(raw)
        except ValueError as error:
            raise ContractError(f"{transport['port_env']} must be an integer") from error
        if not 1 <= port <= 65535:
            raise ContractError(f"{transport['port_env']} is outside the TCP port range")
        return port

    def _credentials(self) -> tuple[str, str]:
        if self.auth["mode"] == "password":
            return "password", self._required_env(self.auth["password_env"])
        return "oauth2", self._required_env(self.auth["oauth_access_token_env"])

    def _authenticate_imap(self, imap: imaplib.IMAP4) -> None:
        mode, credential = self._credentials()
        if mode == "password":
            status, data = imap.login(self.address, credential)
        else:
            auth = f"user={self.address}\x01auth=Bearer {credential}\x01\x01".encode("utf-8")
            status, data = imap.authenticate("XOAUTH2", lambda _: auth)
        if status != "OK":
            raise TransportError("IMAP authentication failed", False, {"response": _response_text(data)})

    def _authenticate_smtp(self, smtp: smtplib.SMTP) -> None:
        mode, credential = self._credentials()
        if mode == "password":
            smtp.login(self.address, credential)
            return
        auth = base64.b64encode(f"user={self.address}\x01auth=Bearer {credential}\x01\x01".encode("utf-8")).decode("ascii")
        code, response = smtp.docmd("AUTH", "XOAUTH2 " + auth)
        if code not in {235, 250}:
            raise TransportError("SMTP OAuth authentication failed", False, {"code": code, "response": _response_text(response)})

    def _send_imap_id(self, imap: imaplib.IMAP4) -> None:
        capabilities = {
            item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item)
            for item in imap.capabilities
        }
        if not self.imap_config["send_client_id"] or "ID" not in capabilities:
            return
        try:
            imap.xatom("ID", '("name" "octopus-email-interface" "version" "1")')
        except imaplib.IMAP4.error:
            return

    @contextmanager
    def imap(self) -> Iterator[imaplib.IMAP4]:
        host = self._required_env(self.imap_config["host_env"])
        self._require_transport_security("imap", self.imap_config, host)
        port = self._port(self.imap_config)
        timeout = self.imap_config["timeout_ms"] / 1000
        attempt_timeout = self.imap_config["connect_attempt_timeout_ms"] / 1000
        family = _family_value(self.imap_config["address_family"])
        context = self._tls_context()
        client: imaplib.IMAP4 | None = None
        try:
            if self.imap_config["tls"] == "implicit":
                client = FamilyIMAP4SSL(host, port, timeout, attempt_timeout, family, context)
            else:
                client = FamilyIMAP4(host, port, timeout, attempt_timeout, family)
                if self.imap_config["tls"] == "starttls":
                    client.starttls(context)
            self._authenticate_imap(client)
            self._send_imap_id(client)
            yield client
        except ContractError:
            raise
        except TransportError as error:
            raise self._sanitize_transport_error(error)
        except (imaplib.IMAP4.error, OSError, ssl.SSLError, TimeoutError) as error:
            retryable = not isinstance(error, ssl.SSLCertVerificationError)
            self._retire_source_exception(error)
            raise self._transport_error("IMAP transport failed", retryable, {"exception": type(error).__name__}) from None
        finally:
            if client is not None:
                try:
                    client.logout()
                except Exception:
                    try:
                        client.shutdown()
                    except Exception:
                        pass

    @contextmanager
    def smtp(self) -> Iterator[smtplib.SMTP]:
        host = self._required_env(self.smtp_config["host_env"])
        self._require_transport_security("smtp", self.smtp_config, host)
        port = self._port(self.smtp_config)
        timeout = self.smtp_config["timeout_ms"] / 1000
        attempt_timeout = self.smtp_config["connect_attempt_timeout_ms"] / 1000
        family = _family_value(self.smtp_config["address_family"])
        context = self._tls_context()
        client: smtplib.SMTP | None = None
        try:
            if self.smtp_config["tls"] == "implicit":
                client = FamilySMTPSSL(host, port, timeout, attempt_timeout, family, context)
            else:
                client = FamilySMTP(host, port, timeout, attempt_timeout, family)
                client.ehlo()
                if self.smtp_config["tls"] == "starttls":
                    client.starttls(context=context)
                    client.ehlo()
            self._authenticate_smtp(client)
            yield client
        except ContractError:
            raise
        except TransportError as error:
            raise self._sanitize_transport_error(error)
        except (smtplib.SMTPException, OSError, ssl.SSLError, TimeoutError) as error:
            retryable = not isinstance(error, (smtplib.SMTPAuthenticationError, ssl.SSLCertVerificationError))
            details: dict[str, Any] = {"exception": type(error).__name__}
            if isinstance(error, smtplib.SMTPResponseException):
                details.update({"smtp_code": error.smtp_code, "smtp_error": _response_text(error.smtp_error)})
                retryable = 400 <= error.smtp_code < 500
            self._retire_source_exception(error)
            raise self._transport_error("SMTP transport failed", retryable, details) from None
        finally:
            if client is not None:
                try:
                    client.quit()
                except Exception:
                    try:
                        client.close()
                    except Exception:
                        pass

    def _throttle(self) -> None:
        with self._action_lock:
            interval = self.config["outbound"]["min_action_interval_ms"] / 1000
            elapsed = time.monotonic() - self._last_action_at
            if elapsed < interval:
                time.sleep(interval - elapsed)
            self._last_action_at = time.monotonic()

    def throttle_action(self) -> None:
        self._throttle()

    def select(self, imap: imaplib.IMAP4, mailbox: str, readonly: bool = False) -> tuple[int, int]:
        encoded = encode_modified_utf7(mailbox)
        status, data = imap.select(quote_imap(encoded), readonly=readonly)
        if status != "OK":
            raise TransportError(f"unable to select mailbox {mailbox!r}", False, {"response": _response_text(data)})
        count_text = _response_text(data)
        match = re.search(r"\d+", count_text)
        count = int(match.group(0)) if match else 0
        return count, _parse_uidvalidity(imap)

    def _mailbox_allowed(self, mailbox: str, mode: str) -> bool:
        patterns = self.scopes["read_mailboxes" if mode == "read" else "write_mailboxes"]
        lowered = mailbox.lower()
        return any(fnmatch.fnmatchcase(lowered, pattern.lower()) for pattern in patterns)

    def _require_mailbox_scope(self, mailbox: str, mode: str) -> str:
        if not self._mailbox_allowed(mailbox, mode):
            raise ContractError(f"email {mode} scope denies mailbox {mailbox!r}")
        return mailbox

    def _require_recipients(self, recipients: list[str], exact_exceptions: tuple[str, ...] = ()) -> None:
        if not self.scopes["deny_unmatched_recipients"] or env_flag(self.scopes["allow_all_recipients_env"]):
            return
        allowed = {normalize_address(value).lower() for value in env_csv(self.scopes["allowed_recipients_env"])}
        allowed.update(normalize_address(value).lower() for value in exact_exceptions)
        domains = {value.lower().lstrip("@") for value in env_csv(self.scopes["allowed_recipient_domains_env"])}
        denied = []
        for recipient in recipients:
            normalized = normalize_address(recipient).lower()
            if normalized not in allowed and normalized.rsplit("@", 1)[1] not in domains:
                denied.append(normalized)
        if denied:
            raise ContractError(f"email recipient scope denies: {', '.join(sorted(denied))}")

    def capabilities(self) -> dict[str, Any]:
        with self.imap() as imap:
            capabilities = sorted(item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item) for item in imap.capabilities)
        return {
            "account": self.account_id,
            "address": self.address,
            "imap": capabilities,
            "actions": list(self.config["outbound"]["allowed_actions"]),
            "scope": {
                "read_mailboxes": list(self.scopes["read_mailboxes"]),
                "write_mailboxes": list(self.scopes["write_mailboxes"]),
                "manage_mailboxes": self.scopes["manage_mailboxes"],
                "recipient_gate": self.scopes["deny_unmatched_recipients"],
            },
            "limits": {
                "max_recipients": self.config["outbound"]["max_recipients"],
                "max_message_bytes": self.config["outbound"]["max_message_bytes"],
                "max_attachment_bytes": self.config["outbound"]["max_attachment_bytes"],
                "max_attachments": self.config["outbound"]["max_attachments"],
            },
            "features": {
                "move": "MOVE" in capabilities,
                "sort": "SORT" in capabilities,
                "gmail_labels": "X-GM-EXT-1" in capabilities,
                "quota": "QUOTA" in capabilities,
                "idle": "IDLE" in capabilities,
                "draft_edit": True,
                "reactions": False,
            },
        }

    def health(self) -> dict[str, Any]:
        started = time.monotonic()
        self._require_mailbox_scope(self.mailboxes["inbox"], "read")
        with self.imap() as imap:
            count, uidvalidity = self.select(imap, self.mailboxes["inbox"], readonly=True)
            imap_capabilities = sorted(item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item) for item in imap.capabilities)
        imap_ms = int((time.monotonic() - started) * 1000)
        smtp_started = time.monotonic()
        with self.smtp() as smtp:
            smtp.noop()
            smtp_features = sorted(smtp.esmtp_features)
        smtp_ms = int((time.monotonic() - smtp_started) * 1000)
        return {
            "ok": True,
            "account": self.account_id,
            "address": self.address,
            "inbox_messages": count,
            "inbox_uidvalidity": uidvalidity,
            "imap_latency_ms": imap_ms,
            "smtp_latency_ms": smtp_ms,
            "imap_capabilities": imap_capabilities,
            "smtp_features": smtp_features,
        }

    def _fetch_message_bytes(self, imap: imaplib.IMAP4, uid: int, purpose: str) -> tuple[bytes, list[str], str | None]:
        maximum = self.config["content"]["max_raw_message_bytes"]
        status, data = imap.uid(
            "fetch",
            str(uid),
            f"(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[]<0.{maximum + 1}>)",
        )
        if status != "OK":
            raise TransportError(f"IMAP FETCH failed during {purpose}", True, {"uid": uid, "response": _response_text(data)})
        response_headers = " ".join(
            _response_text(item[0])
            for item in data or []
            if isinstance(item, tuple) and item
        )
        size_match = re.search(r"RFC822\.SIZE\s+(\d+)", response_headers, flags=re.IGNORECASE)
        declared_size = int(size_match.group(1)) if size_match else None
        raw, flags, internal_date = _parse_fetch_payload(data)
        if declared_size is not None and declared_size > maximum:
            raise MessageBoundaryError(
                f"email message UID {uid} exceeds max_raw_message_bytes ({declared_size} bytes)",
                raw,
                {"uid": uid, "declared_size": declared_size, "captured_size": len(raw), "maximum": maximum},
            )
        if len(raw) > maximum:
            raise MessageBoundaryError(
                f"email message UID {uid} exceeds max_raw_message_bytes",
                raw,
                {"uid": uid, "declared_size": declared_size, "captured_size": len(raw), "maximum": maximum},
            )
        return raw, flags, internal_date

    def poll_mailbox(self, mailbox: str, last_uid: int | None, startup: str, limit: int) -> dict[str, Any]:
        self._require_mailbox_scope(mailbox, "read")
        with self.imap() as imap:
            _, uidvalidity = self.select(imap, mailbox, readonly=False)
            if last_uid is None and startup == "skip_existing":
                status, data = imap.uid("search", None, "ALL")
                if status != "OK":
                    raise TransportError("IMAP startup search failed", True, {"response": _response_text(data)})
                uids = [int(value) for value in data[0].split()] if data and data[0] else []
                return {"uidvalidity": uidvalidity, "last_uid": max(uids, default=0), "messages": []}
            start_uid = (last_uid or 0) + 1
            status, data = imap.uid("search", None, "UID", f"{start_uid}:*")
            if status != "OK":
                raise TransportError("IMAP catch-up search failed", True, {"response": _response_text(data)})
            uids = [int(value) for value in data[0].split()] if data and data[0] else []
            uids = [uid for uid in sorted(set(uids)) if uid >= start_uid][:limit]
            messages: list[dict[str, Any]] = []
            for uid in uids:
                try:
                    raw, flags, internal_date = self._fetch_message_bytes(imap, uid, "catch-up")
                    messages.append({"uid": uid, "raw": raw, "flags": flags, "internal_date": internal_date})
                except MessageBoundaryError as error:
                    messages.append(
                        {
                            "uid": uid,
                            "raw": error.partial,
                            "flags": [],
                            "internal_date": None,
                            "boundary_failure": {"error": str(error), **error.details},
                        }
                    )
            return {"uidvalidity": uidvalidity, "last_uid": last_uid or 0, "messages": messages}

    def wait_for_change(self, mailbox: str, timeout_ms: int) -> dict[str, Any]:
        self._require_mailbox_scope(mailbox, "read")
        with self.imap() as imap:
            self.select(imap, mailbox, readonly=True)
            capabilities = {
                item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item)
                for item in imap.capabilities
            }
            if "IDLE" not in capabilities:
                time.sleep(min(timeout_ms, self.config["intake"]["poll_interval_ms"]) / 1000)
                return {"mode": "poll", "signaled": False, "responses": []}
            tag = imap._new_tag()
            imap.send(tag + b" IDLE\r\n")
            continuation = imap.readline()
            if not continuation.startswith(b"+"):
                raise TransportError("IMAP IDLE did not receive a continuation", True, {"response": _response_text(continuation)})
            responses: list[dict[str, Any]] = []
            signaled = False
            try:
                ready, _, _ = select_module.select([imap.sock], [], [], timeout_ms / 1000)
                if ready:
                    signaled = True
                    while True:
                        line = imap.readline()
                        responses.append(self._safe_remote_observation(line))
                        more, _, _ = select_module.select([imap.sock], [], [], 0)
                        if not more:
                            break
            finally:
                imap.send(b"DONE\r\n")
                while True:
                    completion = imap.readline()
                    if completion.startswith(tag):
                        if b"OK" not in completion.upper():
                            raise TransportError("IMAP IDLE completion failed", True, {"response": _response_text(completion)})
                        break
                    responses.append(self._safe_remote_observation(completion))
            return {"mode": "idle", "signaled": signaled, "responses": responses[-32:]}

    def mark_seen(self, mailbox: str, uid: int) -> None:
        self._require_mailbox_scope(mailbox, "write")
        with self.imap() as imap:
            self.select(imap, mailbox)
            status, data = imap.uid("store", str(uid), "+FLAGS.SILENT", "(\\Seen)")
            if status != "OK":
                raise TransportError("IMAP failed to mark admitted message seen", True, {"uid": uid, "response": _response_text(data)})

    def parse_inbound(self, mailbox: str, uidvalidity: int, message: dict[str, Any]) -> dict[str, Any]:
        return parse_inbound_message(
            message["raw"],
            self.config,
            self.media_dir,
            self.raw_dir,
            self.address,
            mailbox,
            uidvalidity,
            message["uid"],
            message["flags"],
            message["internal_date"],
        )

    def _folder(self, payload: dict[str, Any], key: str = "mailbox", default_alias: str = "inbox") -> str:
        value = payload.get(key, default_alias)
        return require_mailbox(value, self.mailboxes, f"email {key}")

    def _uid(self, payload: dict[str, Any]) -> int:
        return require_uid(payload.get("uid"), "email uid")

    def folder_list(self) -> dict[str, Any]:
        with self.imap() as imap:
            status, data = imap.list()
            if status != "OK":
                raise TransportError("IMAP LIST failed", True, {"response": _response_text(data)})
            folders = [
                item
                for item in (_mailbox_list_row(row) for row in data or [] if row is not None)
                if self._mailbox_allowed(item["name"], "read") or self._mailbox_allowed(item["name"], "write")
            ]
        return {"folders": folders, "aliases": dict(self.mailboxes)}

    def folder_status(self, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "read")
        encoded = encode_modified_utf7(mailbox)
        with self.imap() as imap:
            status, data = imap.status(quote_imap(encoded), "(MESSAGES RECENT UIDNEXT UIDVALIDITY UNSEEN)")
            if status != "OK":
                raise TransportError("IMAP STATUS failed", True, {"response": _response_text(data)})
        text = _response_text(data)
        values = {key.lower(): int(value) for key, value in re.findall(r"(MESSAGES|RECENT|UIDNEXT|UIDVALIDITY|UNSEEN)\s+(\d+)", text, flags=re.IGNORECASE)}
        return {"mailbox": mailbox, **values}

    def folder_mutate(self, action: str, payload: dict[str, Any]) -> dict[str, Any]:
        if not self.scopes["manage_mailboxes"]:
            raise ContractError("email mailbox management is disabled by scope")
        mailbox = self._require_mailbox_scope(self._folder(payload), "write")
        encoded = quote_imap(encode_modified_utf7(mailbox))
        with self.imap() as imap:
            if action == "folder.create":
                status, data = imap.create(encoded)
            elif action == "folder.delete":
                status, data = imap.delete(encoded)
            elif action == "folder.subscribe":
                status, data = imap.subscribe(encoded)
            elif action == "folder.unsubscribe":
                status, data = imap.unsubscribe(encoded)
            elif action == "folder.rename":
                destination = self._require_mailbox_scope(self._folder(payload, "destination", "archive"), "write")
                status, data = imap.rename(encoded, quote_imap(encode_modified_utf7(destination)))
            else:
                raise ContractError(f"unsupported folder action {action}")
            if status != "OK":
                raise TransportError(f"{action} failed", False, {"response": _response_text(data)})
        result = {"action": action, "mailbox": mailbox}
        if action == "folder.rename":
            result["destination"] = destination
        return result

    def _search_uids(self, imap: imaplib.IMAP4, query: Any) -> list[int]:
        criteria = _structured_search(query)
        status, data = imap.uid("search", None, *criteria)
        if status != "OK":
            raise TransportError("IMAP SEARCH failed", True, {"response": _response_text(data), "criteria": criteria})
        return [int(value) for value in data[0].split()] if data and data[0] else []

    def _fetch_headers(self, imap: imaplib.IMAP4, uid: int) -> dict[str, Any]:
        status, data = imap.uid(
            "fetch",
            str(uid),
            "(UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS (DATE FROM TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES)])",
        )
        if status != "OK":
            raise TransportError("IMAP header FETCH failed", True, {"uid": uid, "response": _response_text(data)})
        raw, flags, internal_date = _parse_fetch_payload(data)
        message = BytesParser(policy=policy.default).parsebytes(raw)
        for name in ("Date", "From", "To", "Cc", "Subject", "Message-ID", "In-Reply-To", "References"):
            if any(len(str(value)) > self.config["content"]["max_header_chars"] for value in message.get_all(name, [])):
                raise ContractError(f"email header listing contains an oversized {name} header")
        size_match = re.search(r"RFC822\.SIZE\s+(\d+)", _response_text(data), flags=re.IGNORECASE)
        return {
            "uid": uid,
            "flags": flags,
            "internal_date": internal_date,
            "size": int(size_match.group(1)) if size_match else None,
            "message_id": safe_message_id(message.get("Message-ID")),
            "in_reply_to": safe_message_id(message.get("In-Reply-To")),
            "references": re.findall(r"<[^<>\r\n]+>", str(message.get("References", ""))),
            "date": decode_header_value(message.get("Date"), 4096),
            "from": parse_addresses(message.get_all("From", []), 100),
            "to": parse_addresses(message.get_all("To", []), 1000),
            "cc": parse_addresses(message.get_all("Cc", []), 1000),
            "subject": decode_header_value(message.get("Subject"), self.config["content"]["max_header_chars"]),
        }

    def message_list(self, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "read")
        query = payload.get("query") if payload.get("action") == "message.search" else payload.get("query", {})
        limit = require_int(payload.get("limit", 50), "email list limit", 1, 500)
        before_uid = payload.get("before_uid")
        after_uid = payload.get("after_uid")
        ascending = require_bool(payload.get("ascending", False), "email list ascending")
        if before_uid is not None and after_uid is not None:
            raise ContractError("email list cannot combine before_uid and after_uid")
        if before_uid is not None:
            before_uid = require_uid(before_uid, "email before_uid")
        if after_uid is not None:
            after_uid = require_uid(after_uid, "email after_uid")
        with self.imap() as imap:
            _, uidvalidity = self.select(imap, mailbox, readonly=True)
            uids = sorted(set(self._search_uids(imap, query)), reverse=not ascending)
            if before_uid is not None:
                uids = [uid for uid in uids if uid < before_uid]
            if after_uid is not None:
                uids = [uid for uid in uids if uid > after_uid]
            page = uids[:limit]
            messages = [self._fetch_headers(imap, uid) for uid in page]
        return {
            "mailbox": mailbox,
            "uidvalidity": uidvalidity,
            "messages": messages,
            "next_before_uid": min(page) if not ascending and len(uids) > len(page) and page else None,
            "next_after_uid": max(page) if ascending and len(uids) > len(page) and page else None,
            "ascending": ascending,
            "count": len(messages),
        }

    def message_thread(self, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "read")
        uid = self._uid(payload)
        limit = require_int(payload.get("limit", 100), "email thread limit", 1, 500)
        include_bodies = require_bool(payload.get("include_bodies", False), "email thread include_bodies")
        original = self._fetch_original(mailbox, uid)
        anchors = list(dict.fromkeys(original.get("references", []) + original.get("in_reply_to", []) + [original["message_id"]]))
        direct_uids: set[int] = {uid}
        subject_uids: set[int] = set()
        with self.imap() as imap:
            _, uidvalidity = self.select(imap, mailbox, readonly=True)
            for anchor in anchors[-64:]:
                for header in ("Message-ID", "In-Reply-To", "References"):
                    status, data = imap.uid("search", None, "HEADER", header, quote_imap(anchor))
                    if status != "OK":
                        raise TransportError("IMAP thread header search failed", True, {"header": header, "response": _response_text(data)})
                    if data and data[0]:
                        direct_uids.update(int(value) for value in data[0].split())
            if original.get("subject"):
                status, data = imap.uid("search", None, "SUBJECT", quote_imap(normalize_subject(original["subject"])))
                if status == "OK" and data and data[0]:
                    for candidate in data[0].split():
                        subject_uids.add(int(candidate))
            candidate_uids = direct_uids | subject_uids
            ordered = (sorted(direct_uids) + sorted(subject_uids - direct_uids))[:limit]
            messages: list[dict[str, Any]] = []
            for candidate_uid in ordered:
                if include_bodies:
                    raw, flags, internal_date = self._fetch_message_bytes(imap, candidate_uid, "thread read")
                    item = parse_inbound_message(raw, self.config, self.media_dir, self.raw_dir, self.address, mailbox, uidvalidity, candidate_uid, flags, internal_date)
                else:
                    item = self._fetch_headers(imap, candidate_uid)
                item_in_reply_to = item.get("in_reply_to")
                reply_matches = any(value in anchors for value in item_in_reply_to) if isinstance(item_in_reply_to, list) else item_in_reply_to in anchors
                if candidate_uid == uid or normalize_subject(item.get("subject", "")) == normalize_subject(original.get("subject", "")) or any(anchor in item.get("references", []) for anchor in anchors) or reply_matches:
                    messages.append(item)
        return {
            "mailbox": mailbox,
            "uidvalidity": uidvalidity,
            "anchor_uid": uid,
            "thread_key": original["thread_key"],
            "messages": messages,
            "truncated": len(candidate_uids) > limit,
        }

    def _fetch_original(self, mailbox: str, uid: int) -> dict[str, Any]:
        self._require_mailbox_scope(mailbox, "read")
        with self.imap() as imap:
            _, uidvalidity = self.select(imap, mailbox, readonly=True)
            raw, flags, internal_date = self._fetch_message_bytes(imap, uid, "message read")
        parsed = parse_inbound_message(
            raw,
            self.config,
            self.media_dir,
            self.raw_dir,
            self.address,
            mailbox,
            uidvalidity,
            uid,
            flags,
            internal_date,
        )
        parsed["raw_bytes"] = raw
        return parsed

    def message_fetch(self, payload: dict[str, Any], raw_only: bool = False) -> dict[str, Any]:
        mailbox = self._folder(payload)
        uid = self._uid(payload)
        original = self._fetch_original(mailbox, uid)
        raw_bytes = original.pop("raw_bytes")
        if raw_only:
            raw_path = original["raw"]["path"]
            if raw_path is None:
                raw_path, _ = persist_content_addressed(self.raw_dir, raw_bytes, ".eml")
            return {
                "mailbox": mailbox,
                "uid": uid,
                "path": raw_path,
                "sha256": hashlib.sha256(raw_bytes).hexdigest(),
                "size": len(raw_bytes),
                "inline_base64": base64.b64encode(raw_bytes).decode("ascii") if len(raw_bytes) <= self.config["content"]["max_inline_data_bytes"] else None,
            }
        return original

    def attachment_fetch(self, payload: dict[str, Any]) -> dict[str, Any]:
        message = self.message_fetch(payload)
        index = require_int(payload.get("attachment_index"), "attachment_index", 1, 10000)
        matches = [item for item in message["attachments"] if item["index"] == index]
        if not matches:
            raise ContractError(f"message has no attachment with index {index}")
        item = dict(matches[0])
        data = read_rooted_regular(self.media_dir, item["stored_path"], self.config["content"]["max_attachment_bytes"])
        if len(data) != item["size"] or hashlib.sha256(data).hexdigest() != item["sha256"]:
            raise ContractError("stored attachment failed integrity verification")
        item["inline_base64"] = base64.b64encode(data).decode("ascii") if len(data) <= self.config["content"]["max_inline_data_bytes"] else None
        return {"mailbox": message["mailbox"], "uid": message["uid"], "attachment": item}

    def _resolve_attachment(self, item: dict[str, Any]) -> tuple[bytes, dict[str, Any]]:
        selectors = [key for key in ("path", "inline_base64", "blob_id", "content_hash") if key in item]
        if len(selectors) != 1:
            raise ContractError("each outbound attachment must declare exactly one of path, inline_base64, blob_id, content_hash")
        selector = selectors[0]
        if selector == "inline_base64":
            value = require_string(item[selector], "attachment inline_base64")
            try:
                data = base64.b64decode(value, validate=True)
            except ValueError as error:
                raise ContractError("attachment inline_base64 is malformed") from error
            evidence = {"carrier": "inline_base64", "sha256": hashlib.sha256(data).hexdigest()}
            return self._verify_attachment_claims(item, data, evidence)
        if selector == "path":
            requested_path = require_string(item[selector], "attachment path")
            path = os.path.realpath(requested_path)
            roots = [os.path.realpath(value) for value in os.environ.get(self.config["outbound"]["attachment_roots_env"], "").split(os.pathsep) if value]
            roots.extend([os.path.realpath(self.media_dir), os.path.realpath(self.raw_dir)])
            matching_roots = [root for root in roots if path == root or path.startswith(root + os.sep)]
            if not matching_roots:
                raise ContractError("attachment path is outside configured interface roots")
            data = read_rooted_regular(matching_roots[0], path, self.config["outbound"]["max_message_bytes"])
            evidence = {"carrier": "path", "path": path, "sha256": hashlib.sha256(data).hexdigest()}
            return self._verify_attachment_claims(item, data, evidence)
        digest = require_string(item[selector], f"attachment {selector}")
        if not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ContractError(f"attachment {selector} must be an Octopus content hash")
        path = os.path.join(self.blobs_dir, digest)
        content = read_rooted_regular(self.blobs_dir, path, self.config["outbound"]["max_message_bytes"])
        if octopus_content_hash(content) != digest:
            raise ContractError("attachment blob failed content-address verification")
        try:
            manifest = json.loads(content)
        except (UnicodeDecodeError, json.JSONDecodeError):
            return self._verify_attachment_claims(item, content, {"carrier": selector, "digest": digest})
        if isinstance(manifest, dict) and isinstance(manifest.get("payload_content_hash"), str):
            payload_digest = require_string(manifest["payload_content_hash"], "attachment payload_content_hash")
            if not re.fullmatch(r"[0-9a-f]{64}", payload_digest):
                raise ContractError("attachment payload_content_hash must be an Octopus content hash")
            data = read_rooted_regular(self.blobs_dir, os.path.join(self.blobs_dir, payload_digest), self.config["outbound"]["max_message_bytes"])
            if octopus_content_hash(data) != payload_digest:
                raise ContractError("attachment payload blob failed content-address verification")
            return self._verify_attachment_claims(item, data, {"carrier": selector, "digest": digest, "payload_digest": payload_digest})
        return self._verify_attachment_claims(item, content, {"carrier": selector, "digest": digest})

    def _verify_attachment_claims(self, item: dict[str, Any], data: bytes, evidence: dict[str, Any]) -> tuple[bytes, dict[str, Any]]:
        expected_digest = item.get("sha256")
        actual_digest = hashlib.sha256(data).hexdigest()
        if expected_digest is not None and validate_sha256(expected_digest, "attachment sha256") != actual_digest:
            raise ContractError("attachment sha256 does not match materialized bytes")
        expected_size = item.get("size")
        if expected_size is not None and require_int(expected_size, "attachment size", 0, self.config["outbound"]["max_attachment_bytes"]) != len(data):
            raise ContractError("attachment size does not match materialized bytes")
        if len(data) > self.config["outbound"]["max_attachment_bytes"]:
            raise ContractError("attachment exceeds max_attachment_bytes")
        evidence["sha256"] = actual_digest
        evidence["size"] = len(data)
        return data, evidence

    def _send_message(self, message: EmailMessage, recipients: list[str], attachments: list[dict[str, Any]], exact_recipient_exceptions: tuple[str, ...] = ()) -> dict[str, Any]:
        self._require_recipients(recipients, exact_recipient_exceptions)
        with self.smtp() as smtp:
            refused = smtp.send_message(message, from_addr=self.address, to_addrs=recipients)
        accepted = [recipient for recipient in recipients if recipient not in refused]
        refused_recipients = {}
        for address, response in refused.items():
            observation = self._safe_remote_observation(response[1])
            refused_recipients[address] = {
                "code": response[0],
                "message_bytes": observation["bytes"],
                "message_sha256": observation["sha256"],
            }
        result: dict[str, Any] = {
            "status": "sent" if accepted and not refused else "partial" if accepted else "refused",
            "message_id": str(message["Message-ID"]),
            "accepted_recipients": accepted,
            "refused_recipients": refused_recipients,
            "mime_sha256": hashlib.sha256(message.as_bytes(policy=policy.SMTP)).hexdigest(),
            "attachments": attachments,
        }
        if self.smtp_config["save_sent"] and accepted:
            raw = message.as_bytes(policy=policy.SMTP)
            try:
                append_result = self._append_raw(self.mailboxes["sent"], raw, "(\\Seen)")
                result["sent_copy"] = {"status": "saved", **(append_result or {})}
            except Exception as error:
                result["status"] = "partial"
                result["sent_copy"] = {"status": "failed", "error": str(error)}
        return result

    def _append_raw(self, mailbox: str, raw: bytes, flags: str | None = None) -> dict[str, int] | None:
        self._require_mailbox_scope(mailbox, "write")
        with self.imap() as imap:
            status, data = imap.append(quote_imap(encode_modified_utf7(mailbox)), flags, None, raw)
            if status != "OK":
                raise TransportError("IMAP APPEND failed", True, {"mailbox": mailbox, "response": _response_text(data)})
            return _parse_append_uid(imap, data)

    def message_send(self, action: str, payload: dict[str, Any], source_key: str, exact_recipient_exceptions: tuple[str, ...] = ()) -> dict[str, Any]:
        original = None
        if action in {"message.reply", "message.reply_all", "message.forward"}:
            original = self._fetch_original(self._folder(payload), self._uid(payload))
        message, recipients, attachments = build_outbound_message(
            action,
            payload,
            self.address,
            self.display_name,
            source_key,
            self.config["outbound"],
            self.config["composition"],
            self._resolve_attachment,
            original,
        )
        return self._send_message(message, recipients, attachments, exact_recipient_exceptions)

    def message_copy_move(self, action: str, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "write")
        destination = self._require_mailbox_scope(self._folder(payload, "destination", "archive"), "write")
        uid = self._uid(payload)
        with self.imap() as imap:
            self.select(imap, mailbox)
            capabilities = {item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item) for item in imap.capabilities}
            if action == "message.move" and "MOVE" in capabilities:
                status, data = imap.uid("move", str(uid), quote_imap(encode_modified_utf7(destination)))
                method = "MOVE"
            else:
                status, data = imap.uid("copy", str(uid), quote_imap(encode_modified_utf7(destination)))
                method = "COPY"
                if status == "OK" and action == "message.move":
                    delete_status, delete_data = imap.uid("store", str(uid), "+FLAGS.SILENT", "(\\Deleted)")
                    if delete_status != "OK":
                        observation = self._safe_remote_observation(delete_data)
                        return {
                            "status": "partial",
                            "mailbox": mailbox,
                            "destination": destination,
                            "uid": uid,
                            "copy": "succeeded",
                            "delete": "failed",
                            "delete_response_bytes": observation["bytes"],
                            "delete_response_sha256": observation["sha256"],
                        }
            if status != "OK":
                raise TransportError(f"{action} failed", True, {"response": _response_text(data)})
        return {"status": "completed", "action": action, "mailbox": mailbox, "destination": destination, "uid": uid, "method": method}

    def message_delete(self, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "write")
        uid = self._uid(payload)
        mode = payload.get("mode", "trash")
        if mode not in {"trash", "mark_deleted", "expunge"}:
            raise ContractError("message.delete mode must be trash, mark_deleted, or expunge")
        if mode == "trash" and mailbox != self.mailboxes["trash"]:
            return self.message_copy_move("message.move", {**payload, "destination": "trash"})
        with self.imap() as imap:
            self.select(imap, mailbox)
            capabilities = {
                item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item)
                for item in imap.capabilities
            }
            if mode == "expunge" and "UIDPLUS" not in capabilities:
                raise ContractError("per-message expunge requires IMAP UIDPLUS")
            status, data = imap.uid("store", str(uid), "+FLAGS.SILENT", "(\\Deleted)")
            if status != "OK":
                raise TransportError("message.delete failed to set Deleted", True, {"response": _response_text(data)})
            expunged = False
            if mode == "expunge":
                status, data = imap.uid("expunge", str(uid))
                if status != "OK":
                    observation = self._safe_remote_observation(data)
                    return {
                        "status": "partial",
                        "uid": uid,
                        "mailbox": mailbox,
                        "marked_deleted": True,
                        "expunge": "failed",
                        "response_bytes": observation["bytes"],
                        "response_sha256": observation["sha256"],
                    }
                expunged = True
        return {"status": "completed", "uid": uid, "mailbox": mailbox, "marked_deleted": True, "expunged": expunged}

    def message_flags(self, action: str, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "write")
        uid = self._uid(payload)
        values = payload.get("flags")
        if not isinstance(values, list) or not values:
            raise ContractError("message flags action requires a non-empty flags array")
        flags = [require_flag(value) for value in values]
        operation = "+FLAGS.SILENT" if action.endswith("add") else "-FLAGS.SILENT"
        with self.imap() as imap:
            self.select(imap, mailbox)
            status, data = imap.uid("store", str(uid), operation, "(" + " ".join(flags) + ")")
            if status != "OK":
                raise TransportError(f"{action} failed", True, {"response": _response_text(data)})
        return {"status": "completed", "action": action, "mailbox": mailbox, "uid": uid, "flags": flags}

    def message_labels(self, action: str, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._require_mailbox_scope(self._folder(payload), "write")
        uid = self._uid(payload)
        values = payload.get("labels")
        if not isinstance(values, list) or not values:
            raise ContractError("message labels action requires a non-empty labels array")
        labels = [require_label(value) for value in values]
        operation = "+X-GM-LABELS.SILENT" if action.endswith("add") else "-X-GM-LABELS.SILENT"
        encoded = "(" + " ".join(quote_imap(encode_modified_utf7(label)) for label in labels) + ")"
        with self.imap() as imap:
            self.select(imap, mailbox)
            capabilities = {item.decode("ascii", errors="replace") if isinstance(item, bytes) else str(item) for item in imap.capabilities}
            if "X-GM-EXT-1" not in capabilities:
                raise ContractError("the selected IMAP provider does not support labels")
            status, data = imap.uid("store", str(uid), operation, encoded)
            if status != "OK":
                raise TransportError(f"{action} failed", True, {"response": _response_text(data)})
        return {"status": "completed", "action": action, "mailbox": mailbox, "uid": uid, "labels": labels}

    def draft_create(self, payload: dict[str, Any], source_key: str) -> dict[str, Any]:
        message, _, attachments = build_outbound_message(
            "message.send",
            payload,
            self.address,
            self.display_name,
            source_key,
            self.config["outbound"],
            self.config["composition"],
            self._resolve_attachment,
        )
        raw = message.as_bytes(policy=policy.SMTP)
        appended = self._append_raw(self._folder(payload, "mailbox", "drafts"), raw, "(\\Draft)")
        return {"status": "created", "message_id": str(message["Message-ID"]), "append": appended, "mime_sha256": hashlib.sha256(raw).hexdigest(), "attachments": attachments}

    def draft_update(self, payload: dict[str, Any], source_key: str) -> dict[str, Any]:
        mailbox = self._folder(payload, "mailbox", "drafts")
        old_uid = self._uid(payload)
        created = self.draft_create({**payload, "mailbox": mailbox}, source_key)
        try:
            deleted = self.message_delete({"mailbox": mailbox, "uid": old_uid, "mode": "mark_deleted"})
        except Exception as error:
            return {"status": "partial", "new_draft": created, "old_uid": old_uid, "old_draft_delete": {"status": "failed", "error": str(error)}}
        return {"status": "updated", "new_draft": created, "old_uid": old_uid, "old_draft_delete": deleted}

    def draft_delete(self, payload: dict[str, Any]) -> dict[str, Any]:
        return self.message_delete({**payload, "mailbox": payload.get("mailbox", "drafts"), "mode": payload.get("mode", "mark_deleted")})

    def draft_send(self, payload: dict[str, Any]) -> dict[str, Any]:
        mailbox = self._folder(payload, "mailbox", "drafts")
        uid = self._uid(payload)
        original = self._fetch_original(mailbox, uid)
        raw = original["raw_bytes"]
        message = BytesParser(policy=policy.SMTP).parsebytes(raw)
        if not isinstance(message, EmailMessage):
            reparsed = EmailMessage(policy=policy.SMTP)
            for name, value in message.items():
                reparsed[name] = value
            reparsed.set_content(original["body"]["text"])
            message = reparsed
        recipients = [item["address"] for name in ("To", "Cc", "Bcc") for item in parse_addresses(message.get_all(name, []), self.config["outbound"]["max_recipients"])]
        if not recipients:
            raise ContractError("draft has no recipients")
        result = self._send_message(message, list(dict.fromkeys(recipients)), [])
        if result["accepted_recipients"]:
            try:
                result["draft_delete"] = self.message_delete({"mailbox": mailbox, "uid": uid, "mode": "mark_deleted"})
            except Exception as error:
                result["status"] = "partial"
                result["draft_delete"] = {"status": "failed", "error": str(error)}
        return result

    def execute(self, action: str, payload: dict[str, Any], source_key: str) -> dict[str, Any]:
        if action == "capabilities.get":
            return self.capabilities()
        if action == "health.get":
            return self.health()
        if action == "folder.list":
            return self.folder_list()
        if action == "folder.status":
            return self.folder_status(payload)
        if action.startswith("folder."):
            return self.folder_mutate(action, payload)
        if action in {"message.list", "message.search"}:
            return self.message_list(payload)
        if action == "message.fetch":
            return self.message_fetch(payload)
        if action == "message.thread":
            return self.message_thread(payload)
        if action == "message.raw":
            return self.message_fetch(payload, raw_only=True)
        if action == "attachment.fetch":
            return self.attachment_fetch(payload)
        if action in {"message.send", "message.reply", "message.reply_all", "message.forward"}:
            return self.message_send(action, payload, source_key)
        if action in {"message.copy", "message.move"}:
            return self.message_copy_move(action, payload)
        if action == "message.delete":
            return self.message_delete(payload)
        if action in {"message.flags.add", "message.flags.remove"}:
            return self.message_flags(action, payload)
        if action in {"message.labels.add", "message.labels.remove"}:
            return self.message_labels(action, payload)
        if action == "draft.create":
            return self.draft_create(payload, source_key)
        if action == "draft.update":
            return self.draft_update(payload, source_key)
        if action == "draft.delete":
            return self.draft_delete(payload)
        if action == "draft.send":
            return self.draft_send(payload)
        raise ContractError(f"unsupported email action {action!r}")
