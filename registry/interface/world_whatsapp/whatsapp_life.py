import base64
import heapq
import hashlib
import hmac
import ipaddress
import json
import os
import queue
import re
import secrets
import shutil
import signal
import socket
import sqlite3
import stat
import subprocess
import sys
import threading
import time
import urllib.parse
from contextlib import suppress

from octopus_content_hash import octopus_content_hash

INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
MEDIA_DIR = os.path.join(VAR_DIR, "media")
EFFECTS_DIR = os.path.join(VAR_DIR, "outbound-effects")
SECRETS_DIR = os.path.join(VAR_DIR, "binding-secrets")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
BINDINGS_PATH = os.path.join(VAR_DIR, "bindings.json")
TOPOLOGY_OWNERSHIP_PATH = os.path.join(VAR_DIR, "topology-ownership.json")
IDENTITY_SALT_PATH = os.path.join(VAR_DIR, "identity-salt")
BRIDGE_STATUS_PATH = os.path.join(VAR_DIR, "bridge-status.json")
PAIRING_PATH = os.path.join(VAR_DIR, "pairing.json")
DENIED_PATH = os.path.join(VAR_DIR, "denied-events.json")
BINDING_EVENTS_PATH = os.path.join(VAR_DIR, "binding-events.json")
STREAM_STATE_PATH = os.path.join(VAR_DIR, "stream-state.json")
PENDING_TURNS_PATH = os.path.join(VAR_DIR, "pending-turns.json")
os.umask(0o077)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_AUTHORING_LOCK = threading.RLock()
_LEDGER_FALLBACK_OFFSET = 0
_LEDGER_FALLBACK_SIZE = 0
_LEDGER_FALLBACK_IDENTITY = None
_LEDGER_INDEX_WARNING = None
_CONFIGURED_PROXY_URL = ""
DEPENDENCY_ENV_ALLOWLIST = (
    "PATH",
    "TMPDIR",
    "TMP",
    "TEMP",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "NODE_EXTRA_CA_CERTS",
)
PRIVATE_REQUEST_WORKERS = 8
PRIVATE_REQUEST_QUEUE_LIMIT = 64
PRIVATE_REGISTRATION_LIMIT = 32
PRIVATE_REGISTRATION_MAX_BYTES = 16 * 1024
TOPOLOGY_MAX_BYTES = 16 * 1024 * 1024
TOPOLOGY_OWNERSHIP_MAX_PATHS = 100_000
SUPPORTED_OUTBOUND_OPERATIONS = frozenset(
    {
        "message.send",
        "message.edit",
        "message.delete",
        "reaction.set",
        "reaction.clear",
        "receipt.read",
        "typing.start",
        "typing.stop",
        "presence.set",
        "media.send",
        "location.send",
        "contact.send",
        "sticker.send",
        "poll.send",
        "interactive.buttons.send",
        "interactive.list.send",
        "interactive.template.send",
        "chat.info",
        "group.participants.update",
        "group.subject.set",
        "group.description.set",
        "group.setting.set",
        "group.invite.get",
        "group.invite.revoke",
        "group.join_requests.list",
        "group.join_requests.update",
        "group.ephemeral.set",
        "group.leave",
        "community.info",
        "community.participants.update",
        "community.subject.set",
        "community.description.set",
        "community.setting.set",
        "community.invite.get",
        "community.invite.revoke",
        "community.linked_groups.list",
        "community.join_requests.list",
        "community.join_requests.update",
        "community.member_add_mode.set",
        "community.join_approval_mode.set",
        "community.ephemeral.set",
        "community.leave",
    }
)


def _require_private_mode(status, expected, name):
    if os.name == "posix" and stat.S_IMODE(status.st_mode) != expected:
        raise PermissionError(f"{name} must have mode {expected:04o}")


def ensure_private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    status = os.lstat(path)
    if stat.S_ISLNK(status.st_mode) or not stat.S_ISDIR(status.st_mode):
        raise PermissionError(f"private directory is not a real directory: {path}")
    _require_private_mode(status, 0o700, path)
    return path


def _open_private_file(path, flags, create=False):
    before = None
    try:
        before = os.lstat(path)
        if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
            raise PermissionError(f"private path is not a regular file: {path}")
        _require_private_mode(before, 0o600, path)
    except FileNotFoundError:
        if not create:
            raise
    nofollow = getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags | nofollow, 0o600)
    try:
        current = os.fstat(descriptor)
        if not stat.S_ISREG(current.st_mode):
            raise PermissionError(f"private path is not a regular file: {path}")
        _require_private_mode(current, 0o600, path)
        linked = os.lstat(path)
        if stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise PermissionError(f"private path is not a regular file: {path}")
        if (linked.st_dev, linked.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"private path changed while opening: {path}")
        if before is not None and (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"private path changed while opening: {path}")
        return descriptor
    except Exception:
        os.close(descriptor)
        raise


def open_private_read(path):
    return os.fdopen(_open_private_file(path, os.O_RDONLY), "rb")


def open_private_append(path):
    ensure_private_directory(os.path.dirname(path))
    return os.fdopen(_open_private_file(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, create=True), "ab", buffering=0)


def open_private_lock(path):
    ensure_private_directory(os.path.dirname(path))
    return os.fdopen(_open_private_file(path, os.O_RDWR | os.O_CREAT, create=True), "a+b")


def validate_private_tree(root):
    root = ensure_private_directory(root)
    for current, directories, files in os.walk(root, topdown=True, followlinks=False):
        ensure_private_directory(current)
        for name in directories:
            path = os.path.join(current, name)
            status = os.lstat(path)
            if stat.S_ISLNK(status.st_mode):
                raise PermissionError(f"private directory tree contains a symlink: {path}")
            ensure_private_directory(path)
        for name in files:
            path = os.path.join(current, name)
            with open_private_read(path):
                pass
    return root


for _directory in (VAR_DIR, PAYLOADS_DIR, MEDIA_DIR, EFFECTS_DIR, SECRETS_DIR):
    ensure_private_directory(_directory)


def require_object(value, name):
    if not isinstance(value, dict):
        raise ValueError(f"{name} must be a JSON object")
    return value


def require_keys(value, required, optional, name):
    value = require_object(value, name)
    missing = sorted(set(required) - set(value))
    unknown = sorted(set(value) - set(required) - set(optional))
    if missing:
        raise ValueError(f"{name} is missing keys: {', '.join(missing)}")
    if unknown:
        raise ValueError(f"{name} has unknown keys: {', '.join(unknown)}")
    return value


def require_string(value, name, allow_empty=False):
    if not isinstance(value, str) or value.strip() != value or (not allow_empty and not value):
        raise ValueError(f"{name} must be a trimmed string")
    return value


def require_text(value, name, allow_empty=False):
    if not isinstance(value, str) or (not allow_empty and not value.strip()):
        raise ValueError(f"{name} must be text")
    return value


def require_bool(value, name):
    if not isinstance(value, bool):
        raise ValueError(f"{name} must be a boolean")
    return value


def require_int(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int):
        raise ValueError(f"{name} must be an integer")
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return value


def require_number(value, name, minimum=None, maximum=None):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not float("-inf") < float(value) < float("inf"):
        raise ValueError(f"{name} must be finite")
    if minimum is not None and value < minimum:
        raise ValueError(f"{name} must be at least {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be at most {maximum}")
    return value


def require_string_list(value, name, allow_empty=False):
    if not isinstance(value, list) or (not allow_empty and not value):
        raise ValueError(f"{name} must be a list")
    result = []
    for index, item in enumerate(value):
        result.append(require_string(item, f"{name}[{index}]"))
    if len(result) != len(set(result)):
        raise ValueError(f"{name} contains duplicates")
    return result


def authored_interface_binding(interface):
    return {
        "interface": require_string(interface, "interface binding identity"),
        "config": {"artifact_policy": {"mode": "all"}},
    }


def require_interface_binding(value, name):
    binding = require_keys(value, {"interface", "config"}, set(), name)
    interface = require_string(binding["interface"], f"{name}.interface")
    config = require_keys(binding["config"], {"artifact_policy"}, set(), f"{name}.config")
    policy = require_object(config["artifact_policy"], f"{name}.config.artifact_policy")
    mode = require_string(policy.get("mode"), f"{name}.config.artifact_policy.mode")
    if mode == "all":
        require_keys(policy, {"mode"}, set(), f"{name}.config.artifact_policy")
    elif mode == "allow":
        require_keys(policy, {"mode", "names", "namespaces"}, set(), f"{name}.config.artifact_policy")
        names = require_string_list(policy["names"], f"{name}.config.artifact_policy.names", True)
        namespaces = require_string_list(policy["namespaces"], f"{name}.config.artifact_policy.namespaces", True)
        if not names and not namespaces:
            raise ValueError(f"{name}.config.artifact_policy allow mode grants nothing")
        if any(namespace.endswith(".") for namespace in namespaces):
            raise ValueError(f"{name}.config.artifact_policy namespace must not end with '.'")
    else:
        raise ValueError(f"{name}.config.artifact_policy.mode must be all or allow")
    return interface


def env_flag(name):
    value = os.environ.get(name)
    if value is None or not value.strip():
        return False
    normalized = value.strip().lower()
    if normalized not in ("1", "true", "yes", "on", "0", "false", "no", "off"):
        raise ValueError(f"{name} must be a boolean string")
    return normalized in ("1", "true", "yes", "on")


def env_csv(name):
    raw = os.environ.get(name, "")
    return {part.strip() for part in raw.split(",") if part.strip()}


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
    value = require_string(value, "whatsapp proxy URL")
    if any(ord(character) < 33 or ord(character) == 127 for character in value) or "\\" in value:
        raise ValueError("whatsapp proxy URL contains invalid characters")
    if "?" in value or "#" in value:
        raise ValueError("whatsapp proxy URL must not contain query or fragment data")
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
    except ValueError as error:
        raise ValueError("whatsapp proxy URL is malformed") from error
    if parsed.scheme not in ("http", "https") or not parsed.netloc or parsed.hostname is None:
        raise ValueError("whatsapp proxy URL must be an HTTP or HTTPS URL")
    if parsed.path not in ("", "/"):
        raise ValueError("whatsapp proxy URL must not contain a path")
    if port is not None and not 1 <= port <= 65535:
        raise ValueError("whatsapp proxy URL port is invalid")
    if re.search(r"%(?![0-9A-Fa-f]{2})", parsed.netloc):
        raise ValueError("whatsapp proxy URL contains malformed escaping")
    try:
        username = urllib.parse.unquote(parsed.username, errors="strict") if parsed.username is not None else None
        password = urllib.parse.unquote(parsed.password, errors="strict") if parsed.password is not None else None
    except UnicodeDecodeError as error:
        raise ValueError("whatsapp proxy credentials are malformed") from error
    if username is not None and not username:
        raise ValueError("whatsapp proxy username must not be empty")
    if any(value is not None and any(ord(character) < 32 or ord(character) == 127 for character in value) for value in (username, password)):
        raise ValueError("whatsapp proxy credentials contain invalid characters")
    if parsed.scheme == "http" and not is_loopback_host(parsed.hostname):
        raise ValueError("remote WhatsApp proxies must use HTTPS")
    return value


def sanitize_external_error(value, proxy_url=None):
    if isinstance(value, dict):
        return {key: sanitize_external_error(item, proxy_url) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize_external_error(item, proxy_url) for item in value]
    if isinstance(value, tuple):
        return tuple(sanitize_external_error(item, proxy_url) for item in value)
    if isinstance(value, BaseException):
        value = str(value)
    if not isinstance(value, str):
        return value
    configured = _CONFIGURED_PROXY_URL if proxy_url is None else proxy_url
    if not configured:
        return value
    try:
        parsed = urllib.parse.urlsplit(configured)
    except ValueError:
        return value.replace(configured, "[proxy-redacted]")
    if "@" not in parsed.netloc:
        return value
    encoded_userinfo, authority = parsed.netloc.rsplit("@", 1)
    try:
        decoded_username = urllib.parse.unquote(parsed.username or "", errors="strict")
        decoded_password = urllib.parse.unquote(parsed.password or "", errors="strict")
    except UnicodeDecodeError:
        decoded_username = ""
        decoded_password = ""
    decoded_userinfo = decoded_username
    if parsed.password is not None:
        decoded_userinfo += f":{decoded_password}"
    redacted_url = urllib.parse.urlunsplit(
        (parsed.scheme, f"[credentials-redacted]@{authority}", parsed.path, parsed.query, parsed.fragment)
    )
    sentinel = "\x00octopus-whatsapp-proxy-url\x00"
    while sentinel in value:
        sentinel += "_"
    sanitized = value.replace(configured, sentinel)
    tokens = {encoded_userinfo, decoded_userinfo}
    if decoded_userinfo:
        tokens.add(base64.b64encode(decoded_userinfo.encode("utf-8")).decode("ascii"))
    for token in (
        parsed.username or "",
        parsed.password or "",
        decoded_username,
        decoded_password,
    ):
        if len(token) >= 3:
            tokens.add(token)
    for token in tuple(tokens):
        if token:
            tokens.add(urllib.parse.quote(token, safe=""))
            tokens.add(urllib.parse.quote_plus(token, safe=""))
    for token in sorted((token for token in tokens if token), key=len, reverse=True):
        sanitized = sanitized.replace(token, "[credentials-redacted]")
    return sanitized.replace(sentinel, redacted_url)


def atomic_write_bytes(path, payload):
    ensure_private_directory(os.path.dirname(path))
    staged = os.path.join(os.path.dirname(path), f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    try:
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
        with os.fdopen(_open_private_file(staged, flags, create=True), "wb") as handle:
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        try:
            existing = os.lstat(path)
            if stat.S_ISLNK(existing.st_mode) or not stat.S_ISREG(existing.st_mode):
                raise PermissionError(f"private path is not a regular file: {path}")
            _require_private_mode(existing, 0o600, path)
        except FileNotFoundError:
            pass
        os.replace(staged, path)
        directory = os.open(os.path.dirname(path), os.O_RDONLY | getattr(os, "O_DIRECTORY", 0))
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    except Exception:
        with suppress(FileNotFoundError):
            os.unlink(staged)
        raise


def atomic_write_json(path, value, indent=None):
    atomic_write_bytes(path, json.dumps(value, ensure_ascii=False, indent=indent).encode("utf-8"))


def read_json(path, default=None):
    try:
        with open_private_read(path) as handle:
            return json.loads(handle.read().decode("utf-8"))
    except FileNotFoundError:
        return default


def validate_config(config):
    config = require_keys(config, {"schema_version", "mode", "ledger", "bridge", "authz", "gating", "media", "outbound", "driver_ipc", "proactive", "context_routes", "private_run"}, {"gateway_run_template", "binding"}, "whatsapp config")
    if config["schema_version"] != 1:
        raise ValueError("whatsapp config schema_version must be 1")
    if config["mode"] not in ("gateway", "binding"):
        raise ValueError("whatsapp config mode must be gateway or binding")
    ledger = require_keys(config["ledger"], {"max_row_bytes"}, set(), "whatsapp ledger")
    require_int(ledger["max_row_bytes"], "whatsapp ledger.max_row_bytes", 256, 16777216)
    bridge = require_keys(config["bridge"], {"script", "node_bin_env", "session_dir_env", "account_mode", "account_mode_env", "pair_on_boot", "connect_timeout_ms", "command_timeout_ms", "npm_install_timeout_ms", "shutdown_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "event_replay_limit", "effect_journal_limit", "max_frame_bytes", "max_log_line_bytes", "max_log_bytes", "proxy_env"}, set(), "whatsapp bridge")
    require_string(bridge["script"], "whatsapp bridge.script")
    for key in ("node_bin_env", "session_dir_env", "account_mode_env", "proxy_env"):
        require_string(bridge[key], f"whatsapp bridge.{key}")
    if bridge["account_mode"] not in ("bot", "self-chat"):
        raise ValueError("whatsapp bridge.account_mode is invalid")
    require_bool(bridge["pair_on_boot"], "whatsapp bridge.pair_on_boot")
    for key in ("connect_timeout_ms", "command_timeout_ms", "npm_install_timeout_ms", "shutdown_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "event_replay_limit", "effect_journal_limit", "max_frame_bytes", "max_log_line_bytes", "max_log_bytes"):
        require_int(bridge[key], f"whatsapp bridge.{key}", 1)
    authz = require_keys(config["authz"], {"deny_unmatched", "allow_all_users", "allow_all_users_env", "allowed_users", "allowed_users_env", "allowed_groups", "allowed_groups_env", "allow_self_chat", "allow_broadcasts", "allow_newsletters"}, set(), "whatsapp authz")
    for key in ("deny_unmatched", "allow_all_users", "allow_self_chat", "allow_broadcasts", "allow_newsletters"):
        require_bool(authz[key], f"whatsapp authz.{key}")
    require_string(authz["allow_all_users_env"], "whatsapp authz.allow_all_users_env")
    require_string(authz["allowed_users_env"], "whatsapp authz.allowed_users_env")
    require_string(authz["allowed_groups_env"], "whatsapp authz.allowed_groups_env")
    require_string_list(authz["allowed_users"], "whatsapp authz.allowed_users", True)
    require_string_list(authz["allowed_groups"], "whatsapp authz.allowed_groups", True)
    gating = require_keys(config["gating"], {"respond_in_dms", "respond_in_groups", "require_mention_in_groups", "reply_to_bot_counts_as_mention", "commands_count_as_mention", "free_response_groups", "free_response_groups_env", "mention_patterns", "mention_patterns_env", "text_batch_delay_ms", "text_batch_split_delay_ms", "text_batch_split_threshold", "max_batch_events", "max_batch_bytes"}, set(), "whatsapp gating")
    for key in ("respond_in_dms", "respond_in_groups", "require_mention_in_groups", "reply_to_bot_counts_as_mention", "commands_count_as_mention"):
        require_bool(gating[key], f"whatsapp gating.{key}")
    require_string_list(gating["free_response_groups"], "whatsapp gating.free_response_groups", True)
    require_string_list(gating["mention_patterns"], "whatsapp gating.mention_patterns", True)
    require_string(gating["free_response_groups_env"], "whatsapp gating.free_response_groups_env")
    require_string(gating["mention_patterns_env"], "whatsapp gating.mention_patterns_env")
    for pattern in gating["mention_patterns"]:
        re.compile(pattern, re.IGNORECASE)
    for key in ("text_batch_delay_ms", "text_batch_split_delay_ms", "text_batch_split_threshold", "max_batch_events", "max_batch_bytes"):
        require_int(gating[key], f"whatsapp gating.{key}", 0)
    media = require_keys(config["media"], {"max_download_bytes", "max_upload_bytes", "max_media_per_message", "inline_text_max_bytes", "inline_text_extensions", "allowed_outbound_mime_prefixes"}, set(), "whatsapp media")
    for key in ("max_download_bytes", "max_upload_bytes", "max_media_per_message", "inline_text_max_bytes"):
        require_int(media[key], f"whatsapp media.{key}", 1)
    require_string_list(media["inline_text_extensions"], "whatsapp media.inline_text_extensions", True)
    require_string_list(media["allowed_outbound_mime_prefixes"], "whatsapp media.allowed_outbound_mime_prefixes")
    outbound = require_keys(config["outbound"], {"max_message_chars", "chunk_delay_ms", "min_edit_interval_ms", "send_timeout_ms", "max_retries", "retry_backoff_ms", "retry_backoff_cap_ms", "reply_prefix", "reply_labels", "stream_labels", "error_labels", "telemetry_labels", "allowed_operations", "typing", "read_receipts", "reactions", "presence"}, set(), "whatsapp outbound")
    for key in ("max_message_chars", "send_timeout_ms", "retry_backoff_ms", "retry_backoff_cap_ms"):
        require_int(outbound[key], f"whatsapp outbound.{key}", 1)
    require_int(outbound["chunk_delay_ms"], "whatsapp outbound.chunk_delay_ms", 0)
    require_int(outbound["min_edit_interval_ms"], "whatsapp outbound.min_edit_interval_ms", 0)
    require_int(outbound["max_retries"], "whatsapp outbound.max_retries", 0, 10)
    require_string(outbound["reply_prefix"], "whatsapp outbound.reply_prefix", True)
    for key in ("reply_labels", "stream_labels", "error_labels", "telemetry_labels", "allowed_operations"):
        require_string_list(outbound[key], f"whatsapp outbound.{key}", True)
    unsupported_operations = sorted(set(outbound["allowed_operations"]) - SUPPORTED_OUTBOUND_OPERATIONS)
    if unsupported_operations:
        raise ValueError(
            f"whatsapp outbound.allowed_operations declares unsupported operations: {unsupported_operations}"
        )
    typing = require_keys(outbound["typing"], {"enabled", "refresh_seconds"}, set(), "whatsapp typing")
    require_bool(typing["enabled"], "whatsapp typing.enabled")
    require_int(typing["refresh_seconds"], "whatsapp typing.refresh_seconds", 1)
    receipts = require_keys(outbound["read_receipts"], {"enabled", "on_admission"}, set(), "whatsapp read_receipts")
    require_bool(receipts["enabled"], "whatsapp read_receipts.enabled")
    require_bool(receipts["on_admission"], "whatsapp read_receipts.on_admission")
    reactions = require_keys(outbound["reactions"], {"enabled", "in_progress", "success", "failure"}, set(), "whatsapp reactions")
    require_bool(reactions["enabled"], "whatsapp reactions.enabled")
    for key in ("in_progress", "success", "failure"):
        require_string(reactions[key], f"whatsapp reactions.{key}", True)
    presence = require_keys(outbound["presence"], {"on_connect", "on_disconnect"}, set(), "whatsapp presence")
    require_string(presence["on_connect"], "whatsapp presence.on_connect", True)
    require_string(presence["on_disconnect"], "whatsapp presence.on_disconnect", True)
    ipc = require_keys(config["driver_ipc"], {"listen", "port", "state_file", "connect_timeout_ms", "request_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "max_manifest_bytes", "max_frame_bytes"}, set(), "whatsapp driver_ipc")
    ipc_host = require_string(ipc["listen"], "whatsapp driver_ipc.listen")
    if not is_loopback_host(ipc_host):
        raise ValueError("whatsapp driver_ipc.listen must be loopback")
    require_int(ipc["port"], "whatsapp driver_ipc.port", 0, 65535)
    require_string(ipc["state_file"], "whatsapp driver_ipc.state_file")
    for key in ("connect_timeout_ms", "request_timeout_ms", "reconnect_backoff_ms", "reconnect_backoff_cap_ms", "max_manifest_bytes", "max_frame_bytes"):
        require_int(ipc[key], f"whatsapp driver_ipc.{key}", 1)
    proactive = require_keys(config["proactive"], {"chat_ids", "chat_ids_env"}, set(), "whatsapp proactive")
    require_string_list(proactive["chat_ids"], "whatsapp proactive.chat_ids", True)
    require_string(proactive["chat_ids_env"], "whatsapp proactive.chat_ids_env")
    routes = require_object(config["context_routes"], "whatsapp context_routes")
    for chat_id, artifacts in routes.items():
        if coordinate_key(chat_id) != chat_id:
            raise ValueError(f"whatsapp context_routes key {chat_id!r} must be a normalized JID")
        require_string_list(artifacts, f"whatsapp context_routes[{chat_id!r}]", True)
    private = require_keys(config["private_run"], {"ee_template", "parent_ee", "model", "reasoning_effort", "role_artifacts", "owners", "id_prefixes", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "worker_environment_door_owner", "gateway_worker_environment_interface", "gateway_file_write_tool", "file_write_mark_outcome", "provider_context_output_labels", "authoring_max_attempts", "result_timeout_ms"}, set(), "whatsapp private_run")
    for key in ("ee_template", "parent_ee", "model", "reasoning_effort", "file_write_tool", "file_write_label", "file_write_result_label", "worker_environment_kind", "worker_environment_door_owner", "gateway_worker_environment_interface", "gateway_file_write_tool", "file_write_mark_outcome"):
        require_string(private[key], f"whatsapp private_run.{key}")
    require_string_list(private["role_artifacts"], "whatsapp private_run.role_artifacts", True)
    require_string_list(private["provider_context_output_labels"], "whatsapp private_run.provider_context_output_labels", True)
    require_int(private["authoring_max_attempts"], "whatsapp private_run.authoring_max_attempts", 1, 16)
    require_int(private["result_timeout_ms"], "whatsapp private_run.result_timeout_ms", 1)
    owners = require_keys(private["owners"], {"whatsapp", "provider", "interface_authoring", "worker"}, set(), "whatsapp owners")
    prefixes = require_keys(private["id_prefixes"], {"run", "ee", "ee_template", "whatsapp", "provider", "interface_authoring", "worker", "worker_environment", "file_write_tool"}, set(), "whatsapp id_prefixes")
    for key, value in owners.items():
        require_string(value, f"whatsapp owners.{key}")
        if not re.fullmatch(r"[A-Za-z0-9._-]+", value):
            raise ValueError(f"whatsapp owners.{key} is not a registry identifier")
    for key, value in prefixes.items():
        require_string(value, f"whatsapp id_prefixes.{key}")
        if not re.fullmatch(r"[A-Za-z0-9._-]+", value):
            raise ValueError(f"whatsapp id_prefixes.{key} is not a registry identifier prefix")
    if config["mode"] == "gateway":
        require_object(config.get("gateway_run_template"), "whatsapp gateway_run_template")
    else:
        binding = require_keys(config.get("binding"), {"interface", "chat_id", "gateway_interface", "secret_handle", "host", "port", "run"}, set(), "whatsapp binding")
        for key in ("interface", "chat_id", "gateway_interface", "secret_handle", "host", "run"):
            require_string(binding[key], f"whatsapp binding.{key}")
        if not os.path.isabs(binding["secret_handle"]):
            raise ValueError("whatsapp binding.secret_handle must be absolute")
        if not is_loopback_host(binding["host"]):
            raise ValueError("whatsapp binding.host must be loopback")
        require_int(binding["port"], "whatsapp binding.port", 1, 65535)
    return config


def load_config():
    global _CONFIGURED_PROXY_URL
    config = validate_config(
        json.loads(
            read_bounded_regular(CONFIG_PATH, TOPOLOGY_MAX_BYTES, "interface config")
        )
    )
    _CONFIGURED_PROXY_URL = validate_proxy_url(
        os.environ.get(config["bridge"]["proxy_env"], "")
    )
    return config


def _read_runtime_verdicts():
    global _VERDICT_INPUT_ERROR
    try:
        for line in sys.stdin:
            message = require_object(json.loads(line), "runtime verdict")
            if message.get("kind") != "delivery_verdict":
                raise RuntimeError(f"unexpected runtime input: {message!r}")
            request_id = require_int(message.get("request_id"), "runtime verdict request_id", 1)
            with _VERDICT_CONDITION:
                _VERDICTS[request_id] = message.get("verdict") or {}
                _VERDICT_CONDITION.notify_all()
        raise RuntimeError("runtime verdict input closed")
    except Exception as error:
        with _VERDICT_CONDITION:
            _VERDICT_INPUT_ERROR = error
            _VERDICT_CONDITION.notify_all()


def start_runtime_verdict_reader():
    threading.Thread(target=_read_runtime_verdicts, daemon=True).start()


def signal_ready():
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "ready"}, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def hand_artifact(manifest_path):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": []}) + "\n")
        sys.stdout.flush()
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            _VERDICT_CONDITION.wait()
        if _VERDICT_INPUT_ERROR is not None:
            raise RuntimeError(f"runtime verdict input failed before request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int):
        return admitted["seq"]
    raise RuntimeError(f"runtime refused whatsapp artifact: {verdict!r}")


def materialize_artifact(label, payload, key):
    safe_key = re.sub(r"[^A-Za-z0-9._-]", "_", key)[:180]
    payload_path = os.path.join(PAYLOADS_DIR, f"{safe_key}.payload")
    manifest_path = os.path.join(PAYLOADS_DIR, f"{safe_key}.artifact.json")
    atomic_write_bytes(payload_path, payload)
    atomic_write_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def hand_json(label, payload, key):
    return hand_artifact(materialize_artifact(label, json.dumps(payload, ensure_ascii=False, sort_keys=True).encode("utf-8"), key))


def hand_failure(source_seq, failure_class, details, key):
    return hand_json(f"{INTERFACE_ID}.failure", {"source_movement_seq": source_seq, "failure_class": failure_class, "details": sanitize_external_error(details)}, key)


def file_hexdigest(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        while True:
            chunk = handle.read(1024 * 1024)
            if not chunk:
                return digest.hexdigest()
            digest.update(chunk)


def normalize_identifier(value):
    value = str(value or "").strip().lstrip("+")
    value = re.sub(r":.*@", "@", value)
    return value.split("@", 1)[0]


def normalize_jid(value):
    value = str(value or "").strip()
    if not value:
        return ""
    if "@" in value:
        prefix, domain = value.split("@", 1)
        return f"{prefix.split(':', 1)[0]}@{domain}"
    digits = re.sub(r"\D", "", value)
    return f"{digits}@s.whatsapp.net" if digits else value


def _identity_salt():
    try:
        with open_private_read(IDENTITY_SALT_PATH) as handle:
            salt = handle.read()
    except FileNotFoundError:
        salt = secrets.token_bytes(32)
        atomic_write_bytes(IDENTITY_SALT_PATH, salt)
    if len(salt) != 32:
        raise RuntimeError("whatsapp identity salt is malformed")
    return salt


def coordinate_key(chat_id):
    chat_id = normalize_jid(chat_id)
    if not chat_id or len(chat_id) > 255 or not re.fullmatch(r"[A-Za-z0-9_.:+@-]+", chat_id):
        raise ValueError("whatsapp chat_id is malformed")
    return chat_id


def load_bindings():
    try:
        state = json.loads(
            read_bounded_regular(BINDINGS_PATH, TOPOLOGY_MAX_BYTES, "binding state")
        )
    except FileNotFoundError:
        state = {"schema_version": 1, "bindings": {}}
    state = require_keys(state, {"schema_version", "bindings"}, set(), "whatsapp bindings")
    if state["schema_version"] != 1 or not isinstance(state["bindings"], dict):
        raise ValueError("whatsapp bindings state is malformed")
    for coordinate, binding in state["bindings"].items():
        binding = require_keys(
            binding,
            {
                "schema_version", "opaque_id", "chat_id", "interface",
                "provider_interface", "interface_authoring_interface",
                "worker_environment_interface", "file_write_tool",
                "worker_interface", "run",
                "ee", "ee_template", "host", "port", "status",
            },
            set(),
            f"whatsapp binding {coordinate!r}",
        )
        if binding["schema_version"] != 1 or coordinate_key(binding["chat_id"]) != coordinate:
            raise ValueError(f"whatsapp binding {coordinate!r} has invalid identity")
        if not re.fullmatch(r"[0-9a-f]{32}", require_string(binding["opaque_id"], f"whatsapp binding {coordinate!r}.opaque_id")):
            raise ValueError(f"whatsapp binding {coordinate!r} has malformed opaque_id")
        for field in ("interface", "provider_interface", "interface_authoring_interface", "worker_environment_interface", "file_write_tool", "worker_interface", "run", "ee", "ee_template"):
            require_string(binding[field], f"whatsapp binding {coordinate!r}.{field}")
        if not is_loopback_host(require_string(binding["host"], f"whatsapp binding {coordinate!r}.host")):
            raise ValueError(f"whatsapp binding {coordinate!r}.host must be loopback")
        require_int(binding["port"], f"whatsapp binding {coordinate!r}.port", 1, 65535)
        if binding["status"] not in ("declared", "ready"):
            raise ValueError(f"whatsapp binding {coordinate!r}.status is invalid")
    return state


def binding_by_interface(interface):
    matches = [binding for binding in load_bindings()["bindings"].values() if binding.get("interface") == interface]
    if len(matches) > 1:
        raise RuntimeError("whatsapp interface identity resolves to multiple bindings")
    return matches[0] if matches else None


def binding_secret_path(binding):
    opaque_id = require_string(binding.get("opaque_id"), "whatsapp binding opaque_id")
    if not re.fullmatch(r"[0-9a-f]{32}", opaque_id):
        raise ValueError("whatsapp binding opaque_id is malformed")
    root = os.path.realpath(SECRETS_DIR)
    path = os.path.abspath(os.path.join(root, f"{opaque_id}.key"))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("whatsapp binding secret handle escaped its private directory")
    return path


def ensure_binding_secret(binding):
    path = binding_secret_path(binding)
    try:
        with open_private_read(path) as handle:
            secret = handle.read()
    except FileNotFoundError:
        secret = secrets.token_bytes(32)
        atomic_write_bytes(path, secret)
    if len(secret) != 32:
        raise RuntimeError("whatsapp binding secret is malformed")
    return path


def binding_secret(binding):
    path = binding_secret_path(binding)
    with open_private_read(path) as handle:
        secret = handle.read()
    if len(secret) != 32:
        raise RuntimeError("whatsapp binding secret is malformed")
    return secret


def load_secret_handle(path):
    path = require_string(path, "whatsapp binding secret_handle")
    if not os.path.isabs(path):
        raise RuntimeError("whatsapp binding secret_handle is unavailable")
    path = os.path.abspath(path)
    with open_private_read(path) as handle:
        secret = handle.read()
    if len(secret) != 32:
        raise RuntimeError("whatsapp binding secret_handle is malformed")
    return secret


def ensure_binding_record(config, chat_id, host, port):
    chat_id = coordinate_key(chat_id)
    host = require_string(host, "whatsapp binding host")
    if not is_loopback_host(host):
        raise ValueError("whatsapp binding host must be loopback")
    port = require_int(port, "whatsapp binding port", 1, 65535)
    key = chat_id
    with _STATE_LOCK:
        state = load_bindings()
        existing = state["bindings"].get(key)
        if existing is not None:
            if existing.get("chat_id") != chat_id:
                raise RuntimeError("whatsapp binding coordinate collision")
            existing["host"] = host
            existing["port"] = port
            ensure_binding_secret(existing)
            atomic_write_json(BINDINGS_PATH, state, indent=2)
            return existing
        opaque_id = hmac.new(_identity_salt(), key.encode("utf-8"), hashlib.sha256).hexdigest()[:32]
        private = config["private_run"]
        prefixes = private["id_prefixes"]
        binding = {
            "schema_version": 1,
            "opaque_id": opaque_id,
            "chat_id": chat_id,
            "interface": prefixes["whatsapp"] + opaque_id,
            "provider_interface": prefixes["provider"] + opaque_id,
            "interface_authoring_interface": prefixes["interface_authoring"] + opaque_id,
            "worker_environment_interface": prefixes["worker_environment"] + opaque_id,
            "file_write_tool": prefixes["file_write_tool"] + opaque_id,
            "worker_interface": prefixes["worker"] + opaque_id,
            "run": prefixes["run"] + opaque_id,
            "ee": prefixes["ee"] + opaque_id,
            "ee_template": prefixes["ee_template"] + opaque_id,
            "host": host,
            "port": port,
            "status": "declared",
        }
        ensure_binding_secret(binding)
        state["bindings"][key] = binding
        atomic_write_json(BINDINGS_PATH, state, indent=2)
        return binding


def mark_binding_ready(binding):
    with _STATE_LOCK:
        state = load_bindings()
        current = state["bindings"].get(binding["chat_id"])
        if current is None or current.get("interface") != binding["interface"]:
            raise RuntimeError("whatsapp binding disappeared before activation")
        current["status"] = "ready"
        atomic_write_json(BINDINGS_PATH, state, indent=2)
        binding["status"] = "ready"


def root_slots():
    try:
        slots = require_object(json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"]), "OCTOPUS_ROOT_SLOTS_JSON")
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is absent or malformed") from error
    result = {}
    for name in ("registry_interface_root", "registry_ee_template_root", "registry_tool_root", "registry_worker_environment_kind_root", "run_records_root", "ee_instances_root"):
        path = slots.get(name)
        if not isinstance(path, str) or not os.path.isabs(path):
            raise RuntimeError(f"OCTOPUS_ROOT_SLOTS_JSON has no absolute {name}")
        result[name] = os.path.realpath(path)
    return result


def json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def rooted_path(root, *parts):
    root = os.path.realpath(root)
    path = os.path.realpath(os.path.join(root, *parts))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("whatsapp topology path escaped its granted root")
    return path


def interface_owner_json(config, owner, filename):
    owner = require_string(owner, "whatsapp interface owner")
    if os.path.basename(owner) != owner or owner in (".", ".."):
        raise ValueError("whatsapp interface owner must be one registry folder name")
    if filename not in ("interface.json", "life.json"):
        raise ValueError("whatsapp interface owner file is unsupported")
    path = rooted_path(root_slots()["registry_interface_root"], owner, filename)
    try:
        value = json.loads(
            read_bounded_regular(
                path,
                config["driver_ipc"]["max_manifest_bytes"],
                f"interface owner {owner} {filename}",
            )
        )
    except FileNotFoundError as error:
        raise RuntimeError(
            f"whatsapp topology requires installed interface owner {owner!r} with {filename}"
        ) from error
    except json.JSONDecodeError as error:
        raise ValueError(
            f"whatsapp interface owner {owner!r} {filename} is malformed"
        ) from error
    return require_object(value, f"whatsapp interface owner {owner} {filename}")


def worker_role_artifact_bindings(config, worker, policy=None):
    worker = require_string(worker, "whatsapp exact worker interface")
    if os.path.basename(worker) != worker or worker in (".", ".."):
        raise ValueError("whatsapp exact worker interface must be one registry folder name")
    if policy is None:
        path = rooted_path(root_slots()["registry_interface_root"], worker, "policy.json")
        try:
            policy = json.loads(
                read_bounded_regular(
                    path,
                    config["driver_ipc"]["max_manifest_bytes"],
                    f"exact worker {worker} policy",
                )
            )
        except FileNotFoundError as error:
            raise RuntimeError(
                f"whatsapp topology requires exact worker policy {worker!r}"
            ) from error
        except json.JSONDecodeError as error:
            raise ValueError(f"whatsapp exact worker policy {worker!r} is malformed") from error
    policy = require_object(policy, "whatsapp exact worker policy")
    if policy.get("id") != worker:
        raise ValueError("whatsapp exact worker policy identity does not match its registry folder")
    roles = require_string_list(
        policy.get("role_artifacts"),
        "whatsapp exact worker policy.role_artifacts",
        True,
    )
    return [{"id": artifact} for artifact in roles]


def exact_interface_descriptor(config, identifier, owner):
    descriptor = require_keys(
        interface_owner_json(config, owner, "interface.json"),
        {"id", "interface_type", "role", "direction", "source_names"},
        {"interface_driver_ref", "interface_driver_config", "interface_policy"},
        f"whatsapp interface owner {owner} descriptor",
    )
    if descriptor["id"] != owner:
        raise ValueError(
            f"whatsapp interface owner {owner!r} descriptor identity does not match its folder"
        )
    identifier = require_string(identifier, "whatsapp exact interface id")
    if os.path.basename(identifier) != identifier or identifier in (".", ".."):
        raise ValueError("whatsapp exact interface id must be one registry folder name")
    if descriptor["interface_type"] not in ("system", "world"):
        raise ValueError(f"whatsapp interface owner {owner!r} has invalid interface_type")
    require_string(descriptor["role"], f"whatsapp interface owner {owner} role")
    if descriptor["direction"] not in ("in", "out", "bidirectional"):
        raise ValueError(f"whatsapp interface owner {owner!r} has invalid direction")
    require_string_list(
        descriptor["source_names"], f"whatsapp interface owner {owner} source_names"
    )
    if "interface_driver_ref" in descriptor:
        require_string(
            descriptor["interface_driver_ref"],
            f"whatsapp interface owner {owner} interface_driver_ref",
        )
    for field in ("interface_driver_config", "interface_policy"):
        if field in descriptor and not isinstance(descriptor[field], dict):
            raise ValueError(f"whatsapp interface owner {owner!r} {field} must be an object")
    descriptor["id"] = identifier
    return descriptor


def interface_owner_life(config, owner):
    life = interface_owner_json(config, owner, "life.json")
    processes = life.get("processes")
    if not isinstance(processes, list) or not processes:
        raise ValueError(f"whatsapp interface owner {owner!r} life declares no processes")
    return life


def exact_tool_descriptor(config, identifier, interface):
    private = config["private_run"]
    owner = private["file_write_tool"]
    path = rooted_path(root_slots()["registry_tool_root"], owner, "tool.json")
    try:
        descriptor = json.loads(
            read_bounded_regular(
                path,
                config["driver_ipc"]["max_manifest_bytes"],
                f"tool owner {owner}",
            )
        )
    except FileNotFoundError as error:
        raise RuntimeError(f"whatsapp topology requires installed tool owner {owner!r}") from error
    except json.JSONDecodeError as error:
        raise ValueError(f"whatsapp tool owner {owner!r} is malformed") from error
    descriptor = require_object(descriptor, f"whatsapp tool owner {owner}")
    if descriptor.get("id") != owner:
        raise ValueError("whatsapp tool owner identity does not match its registry folder")
    if "reference" in descriptor:
        raise ValueError("whatsapp tool owner must be a self-contained exact descriptor")
    if descriptor.get("interface") != private["worker_environment_door_owner"]:
        raise ValueError("whatsapp file-write tool owner is not owned by the configured door owner")
    kinds = require_string_list(
        descriptor.get("supported_worker_environment_kinds"),
        "whatsapp file-write tool owner supported_worker_environment_kinds",
    )
    if private["worker_environment_kind"] not in kinds:
        raise ValueError("whatsapp file-write tool owner does not support the configured worker environment kind")
    require_object(descriptor.get("process"), "whatsapp file-write tool owner process")
    require_object(descriptor.get("args_schema"), "whatsapp file-write tool owner args_schema")
    descriptor["id"] = require_string(identifier, "whatsapp exact file-write tool")
    descriptor["interface"] = require_string(interface, "whatsapp exact worker-environment interface")
    return descriptor


def ee_template_owner_json(config, owner):
    owner = require_string(owner, "whatsapp EE template owner")
    if os.path.basename(owner) != owner or owner in (".", ".."):
        raise ValueError("whatsapp EE template owner must be one registry folder name")
    path = rooted_path(
        root_slots()["registry_ee_template_root"], owner, "ee_template.json"
    )
    try:
        raw = read_bounded_regular(
            path,
            config["driver_ipc"]["max_manifest_bytes"],
            f"EE template owner {owner}",
        )
        template = json.loads(raw)
    except FileNotFoundError as error:
        raise RuntimeError(
            f"whatsapp topology requires installed EE template owner {owner!r}"
        ) from error
    except json.JSONDecodeError as error:
        raise ValueError(f"whatsapp EE template owner {owner!r} is malformed") from error
    template = require_keys(
        template,
        {"id", "goal_class"},
        {
            "allowed_tools",
            "allowed_interfaces",
            "allowed_worker_environment_kinds",
            "allowed_root_slots",
            "allowed_artifact_verbs",
            "hard_blocked_tools",
            "allowed_child_ee_templates",
            "interface_policy_overrides",
            "bootstrap_root",
        },
        f"whatsapp EE template owner {owner}",
    )
    if template["id"] != owner:
        raise ValueError(
            f"whatsapp EE template owner {owner!r} identity does not match its folder"
        )
    require_string(template["goal_class"], f"whatsapp EE template owner {owner} goal_class")
    for field in (
        "allowed_tools",
        "allowed_interfaces",
        "allowed_worker_environment_kinds",
        "allowed_root_slots",
        "hard_blocked_tools",
        "allowed_child_ee_templates",
    ):
        if field in template:
            require_string_list(template[field], f"whatsapp EE template owner {owner} {field}", True)
    return path, raw, template


def stable_append(values, additions):
    result = list(values)
    seen = set(result)
    for value in additions:
        if value not in seen:
            result.append(value)
            seen.add(value)
    return result


def binding_exact_interfaces(config, binding):
    return [
        binding["interface"],
        binding["provider_interface"],
        binding["interface_authoring_interface"],
        binding["worker_environment_interface"],
        binding["worker_interface"],
    ]


def exact_ee_template(config, identifier, interfaces, file_write_tool):
    private = config["private_run"]
    owner = private["ee_template"]
    _, _, template = ee_template_owner_json(config, owner)
    if private["worker_environment_kind"] not in template.get(
        "allowed_worker_environment_kinds", []
    ):
        raise ValueError(
            f"whatsapp EE template owner {owner!r} does not admit the configured worker environment kind"
        )
    identifier = require_string(identifier, "whatsapp exact EE template id")
    if os.path.basename(identifier) != identifier or identifier in (".", ".."):
        raise ValueError("whatsapp exact EE template id must be one registry folder name")
    template["id"] = identifier
    template["allowed_interfaces"] = stable_append([], interfaces)
    if template.get("allowed_tools") is not None:
        allowed_tools = require_string_list(
            template["allowed_tools"],
            f"whatsapp EE template owner {owner} allowed_tools",
            True,
        )
        if private["file_write_tool"] not in allowed_tools:
            raise ValueError("whatsapp EE template owner does not admit the file-write tool owner")
        template["allowed_tools"] = list(
            dict.fromkeys(
                file_write_tool if tool == private["file_write_tool"] else tool
                for tool in allowed_tools
            )
        )
    return template


def parent_template_admission_writes(config, binding):
    current = config["private_run"]["parent_ee"]
    seen_ees = set()
    seen_templates = set()
    writes = []
    depth = 0
    while current is not None:
        current = require_string(current, "whatsapp parent EE")
        if current in seen_ees:
            raise ValueError("whatsapp parent EE chain contains a cycle")
        seen_ees.add(current)
        ee_path = rooted_path(root_slots()["ee_instances_root"], f"{current}.json")
        try:
            parent = json.loads(
                read_bounded_regular(
                    ee_path,
                    config["driver_ipc"]["max_manifest_bytes"],
                    f"parent EE {current}",
                )
            )
        except FileNotFoundError as error:
            raise RuntimeError(f"whatsapp parent EE {current!r} is not installed") from error
        except json.JSONDecodeError as error:
            raise ValueError(f"whatsapp parent EE {current!r} is malformed") from error
        parent = require_keys(
            parent,
            {"ee_id", "template_name", "parent_ee_id", "goal"},
            {"config", "policy_narrowing"},
            f"whatsapp parent EE {current}",
        )
        if parent["ee_id"] != current:
            raise ValueError(f"whatsapp parent EE {current!r} is not the exact instance")
        parent_template = require_string(
            parent["template_name"], f"whatsapp parent EE {current} template"
        )
        if parent_template not in seen_templates:
            seen_templates.add(parent_template)
            path, raw, descriptor = ee_template_owner_json(config, parent_template)
            allowed_interfaces = descriptor.get("allowed_interfaces")
            if not isinstance(allowed_interfaces, list):
                raise ValueError(
                    f"whatsapp parent EE template {parent_template!r} declares no allowed_interfaces array"
                )
            descriptor["allowed_interfaces"] = stable_append(
                allowed_interfaces, binding_exact_interfaces(config, binding)
            )
            if descriptor.get("allowed_tools") is not None:
                descriptor["allowed_tools"] = stable_append(
                    require_string_list(
                        descriptor["allowed_tools"],
                        f"whatsapp parent EE template {parent_template} allowed_tools",
                        True,
                    ),
                    [binding["file_write_tool"]],
                )
            if depth == 0:
                allowed_children = descriptor.get("allowed_child_ee_templates")
                if not isinstance(allowed_children, list):
                    raise ValueError(
                        f"whatsapp parent EE template {parent_template!r} declares no allowed_child_ee_templates array"
                    )
                descriptor["allowed_child_ee_templates"] = stable_append(
                    allowed_children, [binding["ee_template"]]
                )
            content = json_text(descriptor)
            if content.encode("utf-8") != raw:
                writes.append(
                    {
                        "path": path,
                        "content": content,
                        "expected_before_hash": octopus_content_hash(raw),
                    }
                )
        current = parent["parent_ee_id"]
        depth += 1
    if not seen_ees:
        raise ValueError("whatsapp private topology has no parent EE chain")
    return writes


def child_life_json(secret_handle):
    runtime_source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME")
    if not isinstance(runtime_source, str) or not os.path.isabs(runtime_source):
        raise RuntimeError("whatsapp child topology requires the package-owned content-hash runtime")
    runtime_source = os.path.realpath(runtime_source)
    return {
        "processes": [{
            "bin": "python3",
            "max_in_flight_tool_processes": 16,
            "io_limits": {"max_control_frame_bytes": 1048576, "max_stderr_bytes": 262144, "max_manifest_bytes": 1048576, "max_payload_bytes": 268435456},
            "args": [
                {"repository_file": "registry/interface/world_whatsapp/octopus_python_boot.py"},
                {"repository_file": "registry/interface/world_whatsapp/whatsapp_life.py"},
            ],
            "required_files": [
                "registry/interface/world_whatsapp/octopus_python_boot.py",
                "registry/interface/world_whatsapp/whatsapp_life.py",
                "registry/interface/world_whatsapp/octopus_content_hash.py",
                "registry/interface/world_whatsapp/requirements-content-hash.lock",
                "registry/interface/world_whatsapp/bridge/bridge.mjs",
                "registry/interface/world_whatsapp/bridge/allowlist.mjs",
                "registry/interface/world_whatsapp/bridge/redaction.mjs",
                "registry/interface/world_whatsapp/bridge/package.json",
                "registry/interface/world_whatsapp/bridge/package-lock.json",
            ],
            "env": {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source},
            "inherit_env": ["PATH"],
            "authority": {
                "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
                "read_paths": [secret_handle, runtime_source], "write_paths": [], "executable_paths": [runtime_source],
                "executable_commands": ["python3"], "read_env_paths": [], "write_env_paths": [],
                "executable_env_paths": [], "allow_network": False, "allow_local_ipc": True,
            },
            "read_handles": ["ledger", "blobs", "ledger_index"],
        }]
    }


def binding_topology(config, binding):
    private = config["private_run"]
    owners = private["owners"]
    interface = binding["interface"]
    provider = binding["provider_interface"]
    authoring_source = binding["interface_authoring_interface"]
    authoring = binding["worker_environment_interface"]
    file_write_tool = binding["file_write_tool"]
    worker = binding["worker_interface"]
    run = binding["run"]
    ee = binding["ee"]
    ee_template = binding["ee_template"]
    prefix = f"cp_{binding['opaque_id'][:12]}"
    child_config = json.loads(json.dumps(config))
    child_config["mode"] = "binding"
    child_config.pop("gateway_run_template", None)
    secret_handle = ensure_binding_secret(binding)
    child_config["binding"] = {"interface": interface, "chat_id": binding["chat_id"], "gateway_interface": INTERFACE_ID, "secret_handle": secret_handle, "host": binding["host"], "port": binding["port"], "run": run}
    whatsapp_descriptor = exact_interface_descriptor(
        config, interface, owners["whatsapp"]
    )
    provider_descriptor = exact_interface_descriptor(
        config, provider, owners["provider"]
    )
    provider_driver_config = provider_descriptor.get("interface_driver_config")
    if not isinstance(provider_driver_config, dict):
        raise ValueError("whatsapp provider owner declares no interface_driver_config object")
    provider_metadata = provider_driver_config.get("private_config")
    if not isinstance(provider_metadata, dict):
        raise ValueError("whatsapp provider owner declares no private_config object")
    provider_metadata.pop("run_authoring", None)
    provider_metadata.update(
        {
            "model": private["model"],
            "reasoning_effort": private["reasoning_effort"],
            "context_output_labels": private["provider_context_output_labels"],
            "run_authoring_profile": {
                "file_write_tool": file_write_tool,
                "file_write_label": private["file_write_label"],
                "file_write_result_label": private["file_write_result_label"],
                "result_timeout_ms": private["result_timeout_ms"],
            },
        }
    )
    authoring_source_descriptor = exact_interface_descriptor(
        config, authoring_source, owners["interface_authoring"]
    )
    worker_descriptor = exact_interface_descriptor(
        config, worker, owners["worker"]
    )
    door_descriptor = exact_interface_descriptor(
        config, authoring, private["worker_environment_door_owner"]
    )
    door_life = interface_owner_life(
        config, private["worker_environment_door_owner"]
    )
    tool_descriptor = exact_tool_descriptor(config, file_write_tool, authoring)
    provider_life = interface_owner_life(config, owners["provider"])
    authoring_source_life = interface_owner_life(
        config, owners["interface_authoring"]
    )
    ee_template_descriptor = exact_ee_template(
        config,
        ee_template,
        binding_exact_interfaces(config, binding),
        file_write_tool,
    )
    worker_policy = {
        "id": worker,
        "ceiling": {
            "interfaces": binding_exact_interfaces(config, binding),
            "tools": [file_write_tool],
            "wek": [private["worker_environment_kind"]],
            "mark_outcomes": [private["file_write_mark_outcome"]],
            "hard_blocked_tools": [],
        },
        "role_artifacts": private["role_artifacts"],
    }
    mandatory_roles = worker_role_artifact_bindings(config, worker, worker_policy)
    route_artifacts = config["context_routes"].get(binding["chat_id"], [])
    context = [
        {"id": artifact}
        for artifact in dict.fromkeys(
            [role["id"] for role in mandatory_roles] + route_artifacts
        )
    ]
    run_record = {
        "id": run,
        "ee": ee,
        "worker": worker,
        "trace": f"trace:{run}",
        "status": "running",
        "interface_state": {provider: {"context_generation": 0, "authoring_generation": 0, "frontier_checkpoint_ids": [f"{prefix}_whatsapp_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"]}},
        "checkpoints": [
            {"id": f"{prefix}_whatsapp_input", "run": run, "trace": f"trace:{run}:{prefix}_whatsapp_input", "epoch": 0, "interface_binding": authored_interface_binding(interface), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": context, "required_verified_artifacts": []},
            {"id": f"{prefix}_whatsapp_output", "run": run, "trace": f"trace:{run}:{prefix}_whatsapp_output", "epoch": 0, "interface_binding": authored_interface_binding(provider), "outbound_interface_bindings": [authored_interface_binding(interface)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_call", "run": run, "trace": f"trace:{run}:{prefix}_authoring_call", "epoch": 0, "interface_binding": authored_interface_binding(authoring_source), "outbound_interface_bindings": [authored_interface_binding(authoring)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_result", "run": run, "trace": f"trace:{run}:{prefix}_authoring_result", "epoch": 0, "interface_binding": authored_interface_binding(authoring), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        ],
        "active_checkpoints": [f"{prefix}_whatsapp_input", f"{prefix}_whatsapp_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"],
    }
    activation = {"schema_version": 1, "interface": authoring_source, "activations": [{"activation_driver_ref": "app_server_transport", "config": {"bind_interface_socket_when_env_unset": True}}]}
    ee_instance = {"ee_id": ee, "template_name": ee_template, "parent_ee_id": private["parent_ee"], "goal": f"Serve the WhatsApp boundary bound to interface {interface}"}
    roots = root_slots()
    interface_root = roots["registry_interface_root"]
    return {
        rooted_path(interface_root, interface, "config.json"): json_text(child_config),
        rooted_path(interface_root, interface, "life.json"): json_text(child_life_json(secret_handle)),
        rooted_path(interface_root, interface, "interface.json"): json_text(whatsapp_descriptor),
        rooted_path(interface_root, provider, "life.json"): json_text(provider_life),
        rooted_path(interface_root, provider, "interface.json"): json_text(provider_descriptor),
        rooted_path(interface_root, authoring_source, "activation.json"): json_text(activation),
        rooted_path(interface_root, authoring_source, "life.json"): json_text(authoring_source_life),
        rooted_path(interface_root, authoring_source, "interface.json"): json_text(authoring_source_descriptor),
        rooted_path(interface_root, authoring, "life.json"): json_text(door_life),
        rooted_path(interface_root, authoring, "interface.json"): json_text(door_descriptor),
        rooted_path(roots["registry_tool_root"], file_write_tool, "tool.json"): json_text(tool_descriptor),
        rooted_path(interface_root, worker, "policy.json"): json_text(worker_policy),
        rooted_path(interface_root, worker, "interface.json"): json_text(worker_descriptor),
        rooted_path(roots["registry_ee_template_root"], ee_template, "ee_template.json"): json_text(ee_template_descriptor),
        rooted_path(roots["ee_instances_root"], f"{ee}.json"): json_text(ee_instance),
        rooted_path(roots["run_records_root"], f"{run}.json"): json_text(run_record),
    }


def ensure_gateway_run(config):
    template = require_object(config.get("gateway_run_template"), "whatsapp gateway_run_template")
    run_id = require_string(template.get("id"), "whatsapp gateway_run_template.id")
    path = rooted_path(root_slots()["run_records_root"], f"{run_id}.json")
    try:
        record = require_object(
            json.loads(
                read_bounded_regular(
                    path,
                    config["driver_ipc"]["max_manifest_bytes"],
                    "gateway run",
                )
            ),
            "whatsapp gateway run",
        )
    except FileNotFoundError as error:
        raise RuntimeError(f"whatsapp gateway run {path!r} is not authored; provision gateway_run_template before enabling the interface") from error
    if not isinstance(record, dict) or record.get("id") != run_id or record.get("ee") != template.get("ee") or record.get("worker") != template.get("worker"):
        raise RuntimeError("whatsapp gateway run identity conflicts with gateway_run_template")
    configured_door = config["private_run"]["gateway_worker_environment_interface"]
    if not any(
        require_interface_binding(checkpoint.get("interface_binding"), f"whatsapp gateway checkpoint {checkpoint.get('id')}.interface_binding") == configured_door
        or configured_door in [
            require_interface_binding(binding, f"whatsapp gateway checkpoint {checkpoint.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(checkpoint.get("outbound_interface_bindings", []))
        ]
        for checkpoint in record.get("checkpoints", [])
        if isinstance(checkpoint, dict)
    ):
        raise RuntimeError("whatsapp gateway run is not bound to its exact worker-environment interface")
    return path


def gateway_rearm_write(config, opaque_id):
    path = ensure_gateway_run(config)
    current = read_bounded_regular(
        path,
        config["driver_ipc"]["max_manifest_bytes"],
        "gateway run",
    )
    record = require_object(json.loads(current), "whatsapp gateway run")
    template = config["gateway_run_template"]
    run_id = template["id"]
    if record.get("id") != run_id or record.get("worker") != template["worker"]:
        raise RuntimeError("whatsapp gateway run identity drifted")
    mandatory_roles = worker_role_artifact_bindings(config, record["worker"])
    interface_state = require_object(record.setdefault("interface_state", {}), "whatsapp gateway interface_state")
    state = require_object(interface_state.setdefault(INTERFACE_ID, {}), "whatsapp gateway interface state")
    generation = require_int(state.get("authoring_generation", 0), "whatsapp authoring_generation", 0) + 1
    state["authoring_generation"] = generation
    suffix = f"{opaque_id[:12]}_{generation}"
    call = f"cp_whatsapp_gateway_authoring_call_{suffix}"
    result = f"cp_whatsapp_gateway_authoring_result_{suffix}"
    authoring = config["private_run"]["gateway_worker_environment_interface"]
    record["checkpoints"] = [
        {"id": call, "run": run_id, "trace": f"trace:{run_id}:{call}", "epoch": 0, "interface_binding": authored_interface_binding(INTERFACE_ID), "outbound_interface_bindings": [authored_interface_binding(authoring)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        {"id": result, "run": run_id, "trace": f"trace:{run_id}:{result}", "epoch": 0, "interface_binding": authored_interface_binding(authoring), "outbound_interface_bindings": [authored_interface_binding(INTERFACE_ID)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
    ]
    record["active_checkpoints"] = [call, result]
    return {"path": path, "content": json_text(record), "expected_before_hash": octopus_content_hash(current)}


def missing_topology_writes(config, topology):
    ownership = load_topology_ownership()
    writes = []
    for path, content in topology.items():
        desired = content.encode("utf-8")
        try:
            existing = read_bounded_regular(
                path,
                config["driver_ipc"]["max_manifest_bytes"],
                "topology document",
            )
        except FileNotFoundError:
            if path in ownership["paths"]:
                raise RuntimeError(f"whatsapp package-authored topology target {path!r} disappeared")
            writes.append({"path": path, "content": content, "expected_before_hash": None})
            continue
        if existing == desired:
            continue
        observed = octopus_content_hash(existing)
        if ownership["paths"].get(path) != observed:
            raise RuntimeError(f"whatsapp private topology target {path!r} drifted from its last package-authored bytes")
        writes.append({"path": path, "content": content, "expected_before_hash": observed})
    return writes


def load_topology_ownership():
    try:
        state = json.loads(read_bounded_regular(TOPOLOGY_OWNERSHIP_PATH, TOPOLOGY_MAX_BYTES, "whatsapp topology ownership"))
    except FileNotFoundError:
        return {"schema_version": 1, "paths": {}}
    state = require_keys(state, {"schema_version", "paths"}, set(), "whatsapp topology ownership")
    if state["schema_version"] != 1:
        raise ValueError("whatsapp topology ownership schema_version must be 1")
    paths = require_object(state["paths"], "whatsapp topology ownership.paths")
    if len(paths) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("whatsapp topology ownership has too many paths")
    for path, digest in paths.items():
        require_string(path, "whatsapp topology ownership path")
        if not os.path.isabs(path):
            raise ValueError("whatsapp topology ownership path must be absolute")
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("whatsapp topology ownership digest is malformed")
    return state


def commit_topology_ownership(config, topology):
    ownership = load_topology_ownership()
    for path, content in sorted(topology.items()):
        desired = content.encode("utf-8")
        existing = read_bounded_regular(
            path,
            config["driver_ipc"]["max_manifest_bytes"],
            "topology document",
        )
        if existing != desired:
            raise RuntimeError(f"whatsapp package-authored topology target {path!r} did not materialize exactly")
        ownership["paths"][path] = octopus_content_hash(existing)
    if len(ownership["paths"]) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("whatsapp topology ownership has too many paths")
    atomic_write_json(TOPOLOGY_OWNERSHIP_PATH, ownership, indent=2)


def package_owned_topology(topology):
    roots = root_slots()
    owned_roots = (
        roots["registry_interface_root"],
        roots["registry_ee_template_root"],
        roots["registry_tool_root"],
    )
    return {
        path: content
        for path, content in topology.items()
        if any(
            os.path.commonpath((os.path.realpath(path), root)) == root
            and os.path.realpath(path) != root
            for root in owned_roots
        )
    }


def shared_admission_writes(config, binding):
    writes = parent_template_admission_writes(config, binding)
    return sorted(writes, key=lambda write: write["path"])


def wait_for_shared_admissions(config, binding, expected_before_hashes):
    deadline = time.monotonic() + config["private_run"]["result_timeout_ms"] / 1000.0
    while True:
        pending = shared_admission_writes(config, binding)
        if not pending:
            return True
        for write in pending:
            if expected_before_hashes.get(write["path"]) != write["expected_before_hash"]:
                return False
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        time.sleep(min(0.05, remaining))


def ensure_private_binding(config, hub, chat_id):
    with _AUTHORING_LOCK:
        binding = ensure_binding_record(config, chat_id, hub.host, hub.port)
        topology = binding_topology(config, binding)
        managed_topology = package_owned_topology(topology)
        for attempt in range(config["private_run"]["authoring_max_attempts"]):
            topology_writes = missing_topology_writes(
                config,
                managed_topology if binding["status"] == "ready" else topology,
            )
            shared_writes = shared_admission_writes(config, binding)
            writes = topology_writes + shared_writes
            if writes:
                writes.insert(0, gateway_rearm_write(config, binding["opaque_id"]))
                now = time.time_ns()
                hand_json(
                    config["private_run"]["file_write_label"],
                    {
                        "tool": config["private_run"]["gateway_file_write_tool"],
                        "arguments": {"writes": writes},
                        "correlation_id": f"whatsapp-binding-authoring:{binding['opaque_id']}:{now}",
                    },
                    f"whatsapp-binding-authoring-{binding['opaque_id']}-{now}",
                )
            if not wait_for_shared_admissions(
                config,
                binding,
                {
                    write["path"]: write["expected_before_hash"]
                    for write in shared_writes
                },
            ):
                if attempt + 1 < config["private_run"]["authoring_max_attempts"]:
                    continue
                raise RuntimeError(
                    "whatsapp exact parent and worker admissions could not be authored without overwriting concurrent ids"
                )
            try:
                hub.wait_for(
                    binding["interface"], config["driver_ipc"]["connect_timeout_ms"]
                )
            except TimeoutError:
                if attempt + 1 < config["private_run"]["authoring_max_attempts"]:
                    continue
                raise
            commit_topology_ownership(config, managed_topology)
            if binding["status"] != "ready":
                mark_binding_ready(binding)
            return binding
        raise RuntimeError("whatsapp private topology authoring exhausted its attempts")


def reconcile_known_bindings(config, hub):
    with _STATE_LOCK:
        chat_ids = [
            binding["chat_id"]
            for _, binding in sorted(load_bindings()["bindings"].items())
        ]
    for chat_id in chat_ids:
        ensure_private_binding(config, hub, chat_id)


def read_frame(sock, buffer, maximum):
    while True:
        newline = buffer.find(b"\n")
        if newline >= 0:
            payload = bytes(buffer[:newline])
            del buffer[: newline + 1]
            if not payload:
                raise RuntimeError("whatsapp IPC received an empty frame")
            value = json.loads(payload)
            if not isinstance(value, dict):
                raise RuntimeError("whatsapp IPC frame must be an object")
            return value
        if len(buffer) >= maximum:
            raise RuntimeError("whatsapp IPC frame exceeds max_frame_bytes")
        chunk = sock.recv(min(65536, maximum - len(buffer)))
        if not chunk:
            raise EOFError("whatsapp IPC closed")
        buffer.extend(chunk)


def write_frame(sock, value, maximum, lock=None):
    payload = json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
    if len(payload) > maximum:
        raise RuntimeError("whatsapp IPC response exceeds max_frame_bytes")
    if lock is None:
        sock.sendall(payload)
    else:
        with lock:
            sock.sendall(payload)


class JsonPeer:
    def __init__(self, sock, maximum, timeout_seconds, handler):
        self.sock = sock
        self.maximum = maximum
        self.timeout_seconds = timeout_seconds
        self.handler = handler
        self.buffer = bytearray()
        self.write_lock = threading.Lock()
        self.condition = threading.Condition()
        self.responses = {}
        self.pending_requests = set()
        self.next_id = 1
        self.last_inbound_request_id = 0
        self.request_queue = queue.Queue(maxsize=PRIVATE_REQUEST_QUEUE_LIMIT)
        self.closed = threading.Event()
        self.error = None
        self.request_workers = [
            threading.Thread(target=self._request_worker, daemon=True)
            for _ in range(PRIVATE_REQUEST_WORKERS)
        ]
        for worker in self.request_workers:
            worker.start()
        threading.Thread(target=self._read_loop, daemon=True).start()

    def _record_response(self, request_id, frame):
        with self.condition:
            if request_id in self.pending_requests:
                if request_id in self.responses:
                    raise RuntimeError("whatsapp IPC received a duplicate response")
                self.responses[request_id] = frame
                self.condition.notify_all()
            elif request_id >= self.next_id:
                raise RuntimeError("whatsapp IPC received an unsolicited response")

    def _read_loop(self):
        try:
            while True:
                frame = read_frame(self.sock, self.buffer, self.maximum)
                frame_type = frame.get("type")
                if frame_type == "response":
                    request_id = require_int(frame.get("id"), "whatsapp IPC response id", 1)
                    self._record_response(request_id, frame)
                elif frame_type == "request":
                    request_id = require_int(frame.get("id"), "whatsapp IPC request id", 1)
                    if request_id <= self.last_inbound_request_id:
                        raise RuntimeError("whatsapp IPC repeated or reordered a request id")
                    self.last_inbound_request_id = request_id
                    try:
                        self.request_queue.put_nowait(frame)
                    except queue.Full:
                        self._write_response(
                            {
                                "type": "response",
                                "id": request_id,
                                "ok": False,
                                "error": {
                                    "class": "CapacityError",
                                    "message": "whatsapp IPC request capacity is exhausted",
                                },
                            }
                        )
                else:
                    raise RuntimeError(f"whatsapp IPC received unknown frame type {frame_type!r}")
        except Exception as error:
            with self.condition:
                self.error = error
                self.responses[None] = error
                self.condition.notify_all()
        finally:
            self.closed.set()
            with suppress(OSError):
                self.sock.close()

    def _request_worker(self):
        while not self.closed.is_set():
            try:
                frame = self.request_queue.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                self._handle_request(frame)
            finally:
                self.request_queue.task_done()

    def _write_response(self, response):
        try:
            write_frame(self.sock, response, self.maximum, self.write_lock)
        except OSError:
            self.close()

    def _handle_request(self, frame):
        request_id = require_int(frame.get("id"), "whatsapp IPC request id", 1)
        try:
            operation = require_string(frame.get("operation"), "whatsapp IPC operation")
            payload = require_object(frame.get("payload"), "whatsapp IPC payload")
            result = self.handler(operation, payload)
            response = {"type": "response", "id": request_id, "ok": True, "result": result}
        except Exception as error:
            response = {"type": "response", "id": request_id, "ok": False, "error": {"class": type(error).__name__, "message": sanitize_external_error(error)}}
        self._write_response(response)

    def request(self, operation, payload):
        with self.write_lock:
            with self.condition:
                if len(self.pending_requests) >= PRIVATE_REQUEST_QUEUE_LIMIT:
                    raise RuntimeError("whatsapp IPC outgoing request capacity is exhausted")
                request_id = self.next_id
                self.next_id += 1
                self.pending_requests.add(request_id)
            try:
                write_frame(
                    self.sock,
                    {"type": "request", "id": request_id, "operation": operation, "payload": payload},
                    self.maximum,
                )
            except Exception:
                with self.condition:
                    self.pending_requests.discard(request_id)
                self.close()
                raise
        deadline = time.monotonic() + self.timeout_seconds
        with self.condition:
            while request_id not in self.responses and None not in self.responses:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.pending_requests.discard(request_id)
                    self.responses.pop(request_id, None)
                    raise TimeoutError(f"whatsapp IPC operation {operation!r} timed out")
                self.condition.wait(remaining)
            if None in self.responses:
                self.pending_requests.discard(request_id)
                self.responses.pop(request_id, None)
                raise RuntimeError(f"whatsapp IPC reader failed: {self.responses[None]}")
            response = self.responses.pop(request_id)
            self.pending_requests.discard(request_id)
        if not response.get("ok"):
            error = require_object(response.get("error"), "whatsapp IPC error")
            raise RuntimeError(sanitize_external_error(require_string(error.get("message"), "whatsapp IPC error.message")))
        return response.get("result")

    def close(self):
        if self.closed.is_set():
            return
        self.closed.set()
        with suppress(OSError):
            self.sock.shutdown(socket.SHUT_RDWR)
        with suppress(OSError):
            self.sock.close()
        with self.condition:
            if self.error is None:
                self.error = ConnectionError("whatsapp IPC peer closed")
            self.responses.setdefault(None, self.error)
            self.condition.notify_all()


class GatewayHub:
    def __init__(self, config, operation_handler):
        self.config = config
        self.operation_handler = operation_handler
        self.condition = threading.Condition()
        self.peers = {}
        self.server = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.server.bind((config["driver_ipc"]["listen"], config["driver_ipc"]["port"]))
        self.server.listen(128)
        self.host, self.port = self.server.getsockname()
        self.registration_slots = threading.BoundedSemaphore(PRIVATE_REGISTRATION_LIMIT)
        atomic_write_json(os.path.join(VAR_DIR, config["driver_ipc"]["state_file"]), {"schema_version": 1, "host": self.host, "port": self.port})
        threading.Thread(target=self._accept_loop, daemon=True).start()

    def _accept_loop(self):
        while True:
            sock, _address = self.server.accept()
            if not self.registration_slots.acquire(blocking=False):
                with suppress(OSError):
                    sock.close()
                continue
            try:
                threading.Thread(target=self._register, args=(sock,), daemon=True).start()
            except Exception:
                self.registration_slots.release()
                with suppress(OSError):
                    sock.close()
                raise

    def _register(self, sock):
        peer = None
        registration_released = False
        try:
            sock.settimeout(self.config["driver_ipc"]["connect_timeout_ms"] / 1000.0)
            buffer = bytearray()
            registration_maximum = min(
                self.config["driver_ipc"]["max_frame_bytes"],
                PRIVATE_REGISTRATION_MAX_BYTES,
            )
            frame = read_frame(sock, buffer, registration_maximum)
            if frame.get("type") != "register":
                raise RuntimeError("whatsapp IPC peer did not register")
            interface = require_string(frame.get("interface"), "whatsapp IPC registration interface")
            secret = require_string(frame.get("secret"), "whatsapp IPC registration secret")
            binding = binding_by_interface(interface)
            expected = None if binding is None else binding_secret(binding).hex()
            if expected is None or not hmac.compare_digest(expected, secret):
                raise RuntimeError("whatsapp IPC registration is not authorized")
            write_frame(sock, {"type": "registered", "interface": interface}, registration_maximum)
            sock.settimeout(None)
            peer = JsonPeer(sock, self.config["driver_ipc"]["max_frame_bytes"], self.config["driver_ipc"]["request_timeout_ms"] / 1000.0, lambda operation, payload: self.operation_handler(interface, operation, payload))
            peer.buffer.extend(buffer)
            with self.condition:
                old = self.peers.get(interface)
                if old is not None:
                    old.close()
                self.peers[interface] = peer
                self.condition.notify_all()
            self.registration_slots.release()
            registration_released = True
            peer.closed.wait()
        except Exception as error:
            sys.stderr.write(f"whatsapp binding registration failed: {sanitize_external_error(error)}\n")
            sys.stderr.flush()
        finally:
            if not registration_released:
                self.registration_slots.release()
            with self.condition:
                for interface, active in list(self.peers.items()):
                    if active is peer:
                        self.peers.pop(interface, None)
            if peer is not None:
                peer.close()
            else:
                with suppress(OSError):
                    sock.close()

    def wait_for(self, interface, timeout_ms):
        deadline = time.monotonic() + timeout_ms / 1000.0
        with self.condition:
            while True:
                peer = self.peers.get(interface)
                if peer is not None and not peer.closed.is_set():
                    return peer
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"whatsapp binding {interface} did not activate")
                self.condition.wait(remaining)

    def dispatch(self, binding, payload):
        peer = self.wait_for(binding["interface"], self.config["driver_ipc"]["connect_timeout_ms"])
        result = peer.request("arrival", payload)
        if not isinstance(result, dict) or not isinstance(result.get("admitted_seq"), int):
            raise RuntimeError("whatsapp binding returned invalid arrival evidence")
        return result["admitted_seq"]


class BindingLink:
    def __init__(self, config, arrival_handler):
        self.config = config
        self.arrival_handler = arrival_handler
        self.condition = threading.Condition()
        self.peer = None
        self.closed = False
        threading.Thread(target=self._connect_loop, daemon=True).start()

    def _connect_loop(self):
        binding = self.config["binding"]
        ipc = self.config["driver_ipc"]
        backoff = ipc["reconnect_backoff_ms"]
        while not self.closed:
            peer = None
            try:
                sock = socket.create_connection((binding["host"], binding["port"]), ipc["connect_timeout_ms"] / 1000.0)
                write_frame(sock, {"type": "register", "interface": INTERFACE_ID, "secret": load_secret_handle(binding["secret_handle"]).hex()}, ipc["max_frame_bytes"])
                buffer = bytearray()
                registered = read_frame(sock, buffer, ipc["max_frame_bytes"])
                if registered != {"type": "registered", "interface": INTERFACE_ID}:
                    raise RuntimeError("whatsapp gateway returned invalid registration")
                sock.settimeout(None)
                peer = JsonPeer(sock, ipc["max_frame_bytes"], ipc["request_timeout_ms"] / 1000.0, self._handle)
                peer.buffer.extend(buffer)
                with self.condition:
                    self.peer = peer
                    self.condition.notify_all()
                backoff = ipc["reconnect_backoff_ms"]
                peer.closed.wait()
            except Exception as error:
                sys.stderr.write(f"whatsapp binding IPC disconnected: {sanitize_external_error(error)}\n")
                sys.stderr.flush()
            finally:
                with self.condition:
                    if self.peer is peer:
                        self.peer = None
                if peer is not None:
                    peer.close()
            time.sleep(backoff / 1000.0)
            backoff = min(backoff * 2, ipc["reconnect_backoff_cap_ms"])

    def _handle(self, operation, payload):
        if operation != "arrival":
            raise RuntimeError(f"whatsapp binding does not accept operation {operation!r}")
        admitted_seq = self.arrival_handler(payload, self)
        return {"admitted_seq": admitted_seq}

    def request(self, operation, payload):
        deadline = time.monotonic() + self.config["driver_ipc"]["connect_timeout_ms"] / 1000.0
        with self.condition:
            while self.peer is None or self.peer.closed.is_set():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("whatsapp gateway IPC is unavailable")
                self.condition.wait(remaining)
            peer = self.peer
        return peer.request(operation, payload)


def grant_media(binding, path):
    path = os.path.realpath(path)
    session_dir = os.path.realpath(resolve_session_dir(load_config()))
    media_root = os.path.realpath(os.path.join(session_dir, "media"))
    if os.path.commonpath((path, media_root)) != media_root or not os.path.isfile(path):
        raise ValueError("whatsapp media grant escaped the bridge media directory")
    handle = os.path.basename(path)
    size = os.path.getsize(path)
    digest = file_hexdigest(path)
    message = f"{binding['interface']}\0{handle}\0{digest}\0{size}".encode("utf-8")
    token = hmac.new(binding_secret(binding), message, hashlib.sha256).hexdigest()
    return {"media_handle": handle, "media_sha256": digest, "media_size": size, "media_token": token}


def read_granted_media(interface, handle, digest, size, token, offset, limit):
    interface = require_string(interface, "whatsapp media interface")
    handle = require_string(handle, "whatsapp media handle")
    digest = require_string(digest, "whatsapp media digest")
    token = require_string(token, "whatsapp media token")
    size = require_int(size, "whatsapp media size", 0)
    offset = require_int(offset, "whatsapp media offset", 0, size)
    limit = require_int(limit, "whatsapp media limit", 1, 1048576)
    if os.path.basename(handle) != handle or not re.fullmatch(r"[A-Za-z0-9._-]{1,255}", handle):
        raise ValueError("whatsapp media handle is malformed")
    if not re.fullmatch(r"[0-9a-f]{64}", digest) or not re.fullmatch(r"[0-9a-f]{64}", token):
        raise ValueError("whatsapp media digest or token is malformed")
    binding = binding_by_interface(interface)
    if binding is None:
        raise ValueError("whatsapp media interface has no binding")
    message = f"{interface}\0{handle}\0{digest}\0{size}".encode("utf-8")
    expected = hmac.new(binding_secret(binding), message, hashlib.sha256).hexdigest()
    if not hmac.compare_digest(expected, token):
        raise ValueError("whatsapp media token is invalid")
    media_root = os.path.realpath(os.path.join(resolve_session_dir(load_config()), "media"))
    path = os.path.abspath(os.path.join(media_root, handle))
    if os.path.commonpath((path, media_root)) != media_root:
        raise FileNotFoundError("whatsapp granted media is unavailable")
    with open_private_read(path) as source:
        status = os.fstat(source.fileno())
        if status.st_size != size:
            raise RuntimeError("whatsapp granted media changed")
        content_hash = hashlib.sha256()
        while True:
            block = source.read(1024 * 1024)
            if not block:
                break
            content_hash.update(block)
        if content_hash.hexdigest() != digest:
            raise RuntimeError("whatsapp granted media changed")
        source.seek(offset)
        chunk = source.read(min(limit, size - offset))
    return {"offset": offset, "data_base64": base64.b64encode(chunk).decode("ascii"), "eof": offset + len(chunk) == size}


def repository_root():
    return os.path.realpath(os.path.join(os.path.dirname(__file__), "../../.."))


def resolve_session_dir(config):
    env_name = config["bridge"]["session_dir_env"]
    configured = os.environ.get(env_name)
    target = configured if configured else os.path.join(VAR_DIR, "session")
    target = os.path.realpath(os.path.expanduser(target))
    if not os.path.isabs(target):
        raise ValueError("whatsapp session directory must be absolute")
    return validate_private_tree(target)


def resolve_bridge_script(config):
    script = config["bridge"]["script"]
    target = script if os.path.isabs(script) else os.path.join(repository_root(), script)
    target = os.path.realpath(target)
    package_root = os.path.realpath(os.path.join(repository_root(), "registry/interface/world_whatsapp/bridge"))
    if os.path.commonpath((target, package_root)) != package_root or not os.path.isfile(target):
        raise FileNotFoundError("whatsapp bridge script is missing or escaped its package")
    return target


def effective_mode(config):
    value = os.environ.get(config["bridge"]["account_mode_env"], config["bridge"]["account_mode"]).strip()
    if value not in ("bot", "self-chat"):
        raise ValueError("whatsapp account mode must be bot or self-chat")
    return value


def effective_allowed_users(config):
    values = set(config["authz"]["allowed_users"]) | env_csv(config["authz"]["allowed_users_env"])
    return {normalize_identifier(value) for value in values if normalize_identifier(value)}


def effective_allowed_groups(config):
    values = set(config["authz"]["allowed_groups"]) | env_csv(config["authz"]["allowed_groups_env"])
    return {normalize_jid(value) for value in values if normalize_jid(value)}


def effective_free_response_groups(config):
    values = set(config["gating"]["free_response_groups"]) | env_csv(config["gating"]["free_response_groups_env"])
    return {normalize_jid(value) for value in values if normalize_jid(value)}


def effective_mention_patterns(config):
    patterns = list(config["gating"]["mention_patterns"])
    raw = os.environ.get(config["gating"]["mention_patterns_env"], "").strip()
    if raw:
        try:
            parsed = json.loads(raw)
            if not isinstance(parsed, list):
                raise ValueError("mention pattern environment JSON must be a list")
            patterns.extend(require_string(item, "whatsapp mention pattern") for item in parsed)
        except json.JSONDecodeError:
            patterns.extend(part.strip() for part in raw.splitlines() if part.strip())
    return [re.compile(pattern, re.IGNORECASE) for pattern in dict.fromkeys(patterns)]


def dependency_process_environment(node_bin, proxy_url="", npm_cache=None):
    environment = {
        name: os.environ[name]
        for name in DEPENDENCY_ENV_ALLOWLIST
        if os.environ.get(name)
    }
    inherited_path = environment.get("PATH", "")
    node_path = os.path.dirname(node_bin)
    environment["PATH"] = node_path if not inherited_path else node_path + os.pathsep + inherited_path
    environment["npm_config_userconfig"] = os.devnull
    environment["npm_config_update_notifier"] = "false"
    if npm_cache is not None:
        environment["npm_config_cache"] = npm_cache
    if proxy_url:
        environment["HTTP_PROXY"] = proxy_url
        environment["HTTPS_PROXY"] = proxy_url
    return environment


def install_bridge_dependencies(config, bridge_script, node_bin):
    source_dir = os.path.dirname(bridge_script)
    source_files = ("bridge.mjs", "allowlist.mjs", "redaction.mjs", "package.json", "package-lock.json")
    digest = hashlib.sha256()
    for filename in source_files:
        source = rooted_path(source_dir, filename)
        if not os.path.isfile(source):
            raise RuntimeError(f"whatsapp bridge source {filename!r} is missing")
        digest.update(filename.encode("utf-8") + b"\0")
        with open(source, "rb") as handle:
            digest.update(handle.read())
    package_hash = digest.hexdigest()
    runtime_root = os.path.join(VAR_DIR, "bridge-runtime")
    target = os.path.join(runtime_root, package_hash)
    stamp = os.path.join(target, ".octopus-source-hash")
    proxy_url = validate_proxy_url(os.environ.get(config["bridge"]["proxy_env"], ""))
    npm_cache = os.path.join(VAR_DIR, "npm-cache")
    ensure_private_directory(npm_cache)
    install_env = dependency_process_environment(node_bin, proxy_url, npm_cache)
    version = subprocess.run(
        [node_bin, "-e", "const major=Number(process.versions.node.split('.')[0]);process.exit(Number.isInteger(major)&&major>=20?0:2)"],
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        timeout=10,
        check=False,
        env=dependency_process_environment(node_bin),
    )
    if version.returncode != 0:
        raise RuntimeError("WhatsApp bridge requires Node.js 20 or newer")
    smoke_script = "import('@whiskeysockets/baileys').then(m=>{if(typeof m.makeWASocket!=='function'||typeof m.generateWAMessageFromContent!=='function'||!m.proto?.Message?.InteractiveMessage)process.exit(2)})"

    def installed():
        try:
            with open_private_read(stamp) as handle:
                if handle.read().decode("utf-8").strip() != package_hash or not os.path.isdir(os.path.join(target, "node_modules")):
                    return False
        except FileNotFoundError:
            return False
        check = subprocess.run([node_bin, "--input-type=module", "-e", smoke_script], cwd=target, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=install_env, timeout=30)
        return check.returncode == 0

    if installed():
        return os.path.join(target, "bridge.mjs")
    npm_bin = shutil.which("npm", path=install_env["PATH"])
    if npm_bin is None:
        raise RuntimeError("npm is required to install the WhatsApp bridge package")
    lock_path = os.path.join(VAR_DIR, "npm-install.lock")
    with open_private_lock(lock_path) as lock:
        try:
            import fcntl
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
        except ImportError:
            pass
        if installed():
            return os.path.join(target, "bridge.mjs")
        os.makedirs(runtime_root, exist_ok=True)
        staged = os.path.join(runtime_root, f".{package_hash}.{os.getpid()}.{time.time_ns()}.tmp")
        shutil.rmtree(staged, ignore_errors=True)
        os.makedirs(staged, mode=0o700)
        for filename in source_files:
            shutil.copyfile(rooted_path(source_dir, filename), os.path.join(staged, filename))
        try:
            result = subprocess.run(
                [npm_bin, "ci", "--omit=dev", "--no-audit", "--no-fund"],
                cwd=staged,
                stdin=subprocess.DEVNULL,
                stdout=subprocess.DEVNULL,
                stderr=subprocess.DEVNULL,
                env=install_env,
                timeout=config["bridge"]["npm_install_timeout_ms"] / 1000.0,
            )
        except Exception:
            shutil.rmtree(staged, ignore_errors=True)
            raise
        if result.returncode != 0:
            shutil.rmtree(staged, ignore_errors=True)
            raise RuntimeError(f"whatsapp bridge npm install failed with exit {result.returncode}")
        smoke = subprocess.run(
            [node_bin, "--input-type=module", "-e", smoke_script],
            cwd=staged,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            env=install_env,
            timeout=30,
        )
        if smoke.returncode != 0:
            shutil.rmtree(staged, ignore_errors=True)
            raise RuntimeError(f"whatsapp bridge dependency preflight failed with exit {smoke.returncode}")
        atomic_write_bytes(os.path.join(staged, ".octopus-source-hash"), (package_hash + "\n").encode("utf-8"))
        if os.path.exists(target):
            shutil.rmtree(target)
        os.replace(staged, target)
        directory = os.open(runtime_root, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
        for entry in os.listdir(runtime_root):
            candidate = os.path.join(runtime_root, entry)
            if candidate != target and re.fullmatch(r"[0-9a-f]{64}", entry) and os.path.isdir(candidate):
                shutil.rmtree(candidate)
        return os.path.join(target, "bridge.mjs")


class BridgeProcess:
    def __init__(self, config, event_queue):
        self.config = config
        self.events = event_queue
        self.write_lock = threading.Lock()
        self.condition = threading.Condition()
        self.responses = {}
        self.pending_requests = set()
        self.next_id = 1
        self.process = None
        self.stderr_handle = None
        self.stderr_thread = None
        self.proxy_url = ""
        self.reader_error = None
        self.status = "starting"

    def _record_response(self, request_id, frame):
        with self.condition:
            if request_id in self.pending_requests:
                if request_id in self.responses:
                    raise RuntimeError("whatsapp bridge returned a duplicate response")
                self.responses[request_id] = frame
                self.condition.notify_all()
            elif request_id >= self.next_id:
                raise RuntimeError("whatsapp bridge returned an unsolicited response")

    def start(self):
        source_bridge_script = resolve_bridge_script(self.config)
        node_env = self.config["bridge"]["node_bin_env"]
        configured_node = os.environ.get(node_env)
        node_bin = (configured_node if configured_node and os.path.isabs(configured_node) else shutil.which(configured_node or "node"))
        if not node_bin or not os.path.isfile(node_bin):
            raise RuntimeError("node executable is unavailable for WhatsApp")
        bridge_script = install_bridge_dependencies(self.config, source_bridge_script, node_bin)
        session_dir = resolve_session_dir(self.config)
        proxy_url = validate_proxy_url(os.environ.get(self.config["bridge"]["proxy_env"], ""))
        self.proxy_url = proxy_url
        bridge_env = os.environ.copy()
        bridge_env.update({
            "OCTOPUS_WHATSAPP_BRIDGE_SESSION_DIR": session_dir,
            "OCTOPUS_WHATSAPP_BRIDGE_MODE": effective_mode(self.config),
            "OCTOPUS_WHATSAPP_BRIDGE_ALLOWED_USERS": ",".join(sorted(effective_allowed_users(self.config))),
            "OCTOPUS_WHATSAPP_BRIDGE_ALLOWED_GROUPS": ",".join(sorted(effective_allowed_groups(self.config))),
            "OCTOPUS_WHATSAPP_BRIDGE_ALLOW_ALL_USERS": "true" if self.config["authz"]["allow_all_users"] or env_flag(self.config["authz"]["allow_all_users_env"]) else "false",
            "OCTOPUS_WHATSAPP_BRIDGE_DENY_UNMATCHED": "true" if self.config["authz"]["deny_unmatched"] else "false",
            "OCTOPUS_WHATSAPP_BRIDGE_ALLOW_SELF_CHAT": "true" if self.config["authz"]["allow_self_chat"] else "false",
            "OCTOPUS_WHATSAPP_BRIDGE_PAIR_ON_BOOT": "true" if self.config["bridge"]["pair_on_boot"] else "false",
            "OCTOPUS_WHATSAPP_BRIDGE_ALLOW_BROADCASTS": "true" if self.config["authz"]["allow_broadcasts"] else "false",
            "OCTOPUS_WHATSAPP_BRIDGE_ALLOW_NEWSLETTERS": "true" if self.config["authz"]["allow_newsletters"] else "false",
            "OCTOPUS_WHATSAPP_MAX_DOWNLOAD_BYTES": str(self.config["media"]["max_download_bytes"]),
            "OCTOPUS_WHATSAPP_MAX_UPLOAD_BYTES": str(self.config["media"]["max_upload_bytes"]),
            "OCTOPUS_WHATSAPP_SEND_TIMEOUT_MS": str(self.config["outbound"]["send_timeout_ms"]),
            "OCTOPUS_WHATSAPP_MAX_RETRIES": str(self.config["outbound"]["max_retries"]),
            "OCTOPUS_WHATSAPP_RETRY_BACKOFF_MS": str(self.config["outbound"]["retry_backoff_ms"]),
            "OCTOPUS_WHATSAPP_RETRY_BACKOFF_CAP_MS": str(self.config["outbound"]["retry_backoff_cap_ms"]),
            "OCTOPUS_WHATSAPP_EVENT_REPLAY_LIMIT": str(self.config["bridge"]["event_replay_limit"]),
            "OCTOPUS_WHATSAPP_EFFECT_JOURNAL_LIMIT": str(self.config["bridge"]["effect_journal_limit"]),
            "OCTOPUS_WHATSAPP_RECONNECT_BACKOFF_MS": str(self.config["bridge"]["reconnect_backoff_ms"]),
            "OCTOPUS_WHATSAPP_RECONNECT_BACKOFF_CAP_MS": str(self.config["bridge"]["reconnect_backoff_cap_ms"]),
            "OCTOPUS_WHATSAPP_BRIDGE_PROXY": proxy_url,
            "OCTOPUS_WHATSAPP_ON_CONNECT_PRESENCE": self.config["outbound"]["presence"]["on_connect"],
            "OCTOPUS_WHATSAPP_ON_DISCONNECT_PRESENCE": self.config["outbound"]["presence"]["on_disconnect"],
        })
        log_path = os.path.join(VAR_DIR, "bridge.log")
        if os.path.exists(log_path) and os.path.getsize(log_path) > self.config["bridge"]["max_log_bytes"]:
            atomic_write_bytes(log_path, b"")
        self.stderr_handle = open_private_append(log_path)
        self.process = subprocess.Popen(
            [node_bin, bridge_script],
            cwd=os.path.dirname(bridge_script),
            env=bridge_env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            start_new_session=True,
            bufsize=0,
        )
        self.stderr_thread = threading.Thread(target=self._stderr_loop, daemon=True)
        self.stderr_thread.start()
        threading.Thread(target=self._read_loop, daemon=True).start()
        deadline = time.monotonic() + self.config["bridge"]["connect_timeout_ms"] / 1000.0
        with self.condition:
            while self.status == "starting" and self.reader_error is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("whatsapp bridge did not report ready")
                self.condition.wait(remaining)
            if self.reader_error is not None:
                raise RuntimeError(
                    f"whatsapp bridge failed during start: {sanitize_external_error(self.reader_error, self.proxy_url)}"
                )

    def _stderr_loop(self):
        try:
            while True:
                raw = self.process.stderr.readline(self.config["bridge"]["max_log_line_bytes"] + 1)
                if not raw:
                    return
                if len(raw) > self.config["bridge"]["max_log_line_bytes"] or not raw.endswith(b"\n"):
                    message = sanitize_external_error(raw[:self.config["bridge"]["max_log_line_bytes"]].decode("utf-8", errors="replace"), self.proxy_url)
                    self._write_stderr(f"{message}\n<whatsapp bridge stderr line refused: byte ceiling exceeded>\n".encode("utf-8"))
                    raise RuntimeError("whatsapp bridge stderr line exceeds max_log_line_bytes")
                message = raw.decode("utf-8", errors="replace")
                self._write_stderr(sanitize_external_error(message, self.proxy_url).encode("utf-8"))
        except Exception as error:
            self._abort_protocol_process()
            with self.condition:
                if self.reader_error is None:
                    self.reader_error = RuntimeError(
                        f"whatsapp bridge stderr reader failed: {sanitize_external_error(error, self.proxy_url)}"
                    )
                self.condition.notify_all()

    def _write_stderr(self, payload):
        remaining = self.config["bridge"]["max_log_bytes"] - self.stderr_handle.tell()
        if remaining <= 0:
            return
        self.stderr_handle.write(payload[:remaining])
        self.stderr_handle.flush()

    def _abort_protocol_process(self):
        if self.process is not None and self.process.poll() is None:
            with suppress(ProcessLookupError):
                os.killpg(self.process.pid, signal.SIGTERM)

    def _read_loop(self):
        try:
            while True:
                raw = self.process.stdout.readline(self.config["bridge"]["max_frame_bytes"] + 1)
                if not raw:
                    code = self.process.poll()
                    raise RuntimeError(f"whatsapp bridge stdout closed with code {code}")
                if len(raw) > self.config["bridge"]["max_frame_bytes"] or not raw.endswith(b"\n"):
                    raise RuntimeError("whatsapp bridge frame exceeds max_frame_bytes")
                frame = require_object(json.loads(raw), "whatsapp bridge frame")
                frame_type = frame.get("type")
                if frame_type == "response":
                    request_id = require_int(frame.get("id"), "whatsapp bridge response id", 1)
                    self._record_response(request_id, frame)
                elif frame_type == "event":
                    self.events.put(frame)
                elif frame_type == "ready":
                    with self.condition:
                        self.status = "ready"
                        self.condition.notify_all()
                elif frame_type == "status":
                    frame = sanitize_external_error(frame, self.proxy_url)
                    self.status = require_string(frame.get("status"), "whatsapp bridge status")
                    atomic_write_json(BRIDGE_STATUS_PATH, frame, indent=2)
                elif frame_type == "pairing":
                    atomic_write_json(PAIRING_PATH, frame, indent=2)
                elif frame_type == "protocol_error":
                    sys.stderr.write(
                        f"whatsapp bridge protocol error: {sanitize_external_error(frame.get('error'), self.proxy_url)}\n"
                    )
                    sys.stderr.flush()
                else:
                    raise RuntimeError(f"unknown whatsapp bridge frame type {frame_type!r}")
        except Exception as error:
            self._abort_protocol_process()
            with self.condition:
                self.reader_error = error
                self.condition.notify_all()

    def _write_command_frame(self, frame):
        with self.write_lock:
            stream = None if self.process is None else self.process.stdin
            if stream is None or stream.closed:
                raise RuntimeError("whatsapp bridge command stream is unavailable")
            remaining = memoryview(frame)
            try:
                while remaining:
                    written = stream.write(remaining)
                    if isinstance(written, bool) or not isinstance(written, int) or written <= 0 or written > len(remaining):
                        raise RuntimeError("whatsapp bridge command stream refused a complete frame")
                    remaining = remaining[written:]
                stream.flush()
            except Exception as error:
                self._abort_protocol_process()
                with self.condition:
                    if self.reader_error is None:
                        self.reader_error = RuntimeError(
                            f"whatsapp bridge command writer failed: {sanitize_external_error(error, self.proxy_url)}"
                        )
                    self.condition.notify_all()
                raise

    def command(self, operation, payload, timeout_ms=None):
        if self.process is None or self.process.poll() is not None or self.reader_error is not None:
            reason = self.reader_error or self.process.poll() if self.process else "not started"
            raise RuntimeError(
                f"whatsapp bridge is unavailable: {sanitize_external_error(reason, self.proxy_url)}"
            )
        with self.condition:
            request_id = self.next_id
            self.next_id += 1
            self.pending_requests.add(request_id)
        frame = json.dumps({"id": request_id, "operation": operation, "payload": payload}, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
        if len(frame) > self.config["bridge"]["max_frame_bytes"]:
            with self.condition:
                self.pending_requests.discard(request_id)
            raise ValueError("whatsapp bridge command exceeds max_frame_bytes")
        try:
            self._write_command_frame(frame)
        except Exception:
            with self.condition:
                self.pending_requests.discard(request_id)
            raise
        deadline = time.monotonic() + (timeout_ms or self.config["bridge"]["command_timeout_ms"]) / 1000.0
        with self.condition:
            while request_id not in self.responses and self.reader_error is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.pending_requests.discard(request_id)
                    self.responses.pop(request_id, None)
                    raise TimeoutError(f"whatsapp bridge operation {operation!r} timed out")
                self.condition.wait(remaining)
            if self.reader_error is not None:
                self.pending_requests.discard(request_id)
                self.responses.pop(request_id, None)
                raise RuntimeError(
                    f"whatsapp bridge reader failed: {sanitize_external_error(self.reader_error, self.proxy_url)}"
                )
            response = self.responses.pop(request_id)
            self.pending_requests.discard(request_id)
        if not response.get("ok"):
            error = require_object(response.get("error"), "whatsapp bridge error")
            code = require_string(error.get("code"), "whatsapp bridge error.code")
            message = sanitize_external_error(
                require_string(error.get("message"), "whatsapp bridge error.message"),
                self.proxy_url,
            )
            raise RuntimeError(f"{code}: {message}")
        result = response.get("result")
        if not isinstance(result, dict):
            raise RuntimeError("whatsapp bridge returned a non-object result")
        return result

    def stop(self):
        if self.process is None:
            return
        if self.process.poll() is None:
            with suppress(Exception):
                self.command("bridge.shutdown", {}, self.config["bridge"]["shutdown_timeout_ms"])
            try:
                self.process.wait(timeout=self.config["bridge"]["shutdown_timeout_ms"] / 1000.0)
            except subprocess.TimeoutExpired:
                with suppress(ProcessLookupError):
                    os.killpg(self.process.pid, signal.SIGTERM)
                try:
                    self.process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    with suppress(ProcessLookupError):
                        os.killpg(self.process.pid, signal.SIGKILL)
        if self.stderr_thread is not None:
            self.stderr_thread.join(timeout=2)
        if self.stderr_handle is not None:
            self.stderr_handle.close()


def expand_aliases(value, session_dir):
    first = normalize_identifier(value)
    if not first:
        return set()
    resolved = set()
    pending = [first]
    while pending:
        current = pending.pop(0)
        if current in resolved or not re.fullmatch(r"[A-Za-z0-9@.+-]+", current):
            continue
        resolved.add(current)
        for suffix in ("", "_reverse"):
            path = os.path.join(session_dir, f"lid-mapping-{current}{suffix}.json")
            try:
                mapped = normalize_identifier(read_json(path))
            except (OSError, ValueError, TypeError):
                mapped = ""
            if mapped and mapped not in resolved:
                pending.append(mapped)
    return resolved


def user_matches(config, sender_id):
    if config["authz"]["allow_all_users"] or env_flag(config["authz"]["allow_all_users_env"]):
        return True
    allowed = effective_allowed_users(config)
    if "*" in allowed:
        return True
    return bool(expand_aliases(sender_id, resolve_session_dir(config)) & allowed)


def event_authorized(config, event):
    chat_id = normalize_jid(event.get("chat_id"))
    if not chat_id:
        return False, "missing_chat_id"
    if chat_id.endswith("@broadcast") and not config["authz"]["allow_broadcasts"]:
        return False, "broadcast_refused"
    if chat_id.endswith("@newsletter") and not config["authz"]["allow_newsletters"]:
        return False, "newsletter_refused"
    is_group = bool(event.get("is_group")) or chat_id.endswith("@g.us")
    mode = effective_mode(config)
    if event.get("from_me") and mode == "bot":
        return False, "outbound_echo"
    if event.get("from_me") and mode == "self-chat":
        if not config["authz"]["allow_self_chat"]:
            return False, "self_chat_disabled"
        bot_ids = {normalize_jid(value) for value in event.get("bot_ids") or []}
        if bot_ids and chat_id not in bot_ids:
            return False, "self_chat_only"
        return True, None
    if is_group:
        if not config["gating"]["respond_in_groups"]:
            return False, "groups_disabled"
        groups = effective_allowed_groups(config)
        if config["authz"]["deny_unmatched"] and chat_id not in groups and "*" not in groups:
            return False, "group_allowlist"
        if event.get("system_event"):
            return True, None
        sender = event.get("sender_id") or event.get("participant_id")
        if config["authz"]["deny_unmatched"] and not user_matches(config, sender):
            return False, "user_allowlist"
    else:
        if not config["gating"]["respond_in_dms"]:
            return False, "dms_disabled"
        sender = event.get("sender_id") or event.get("participant_id") or chat_id
        if config["authz"]["deny_unmatched"] and not user_matches(config, sender):
            return False, "user_allowlist"
    return True, None


def event_should_trigger(config, event):
    if event.get("event_type") != "message.create" or not event.get("is_group"):
        return True
    chat_id = normalize_jid(event.get("chat_id"))
    if chat_id in effective_free_response_groups(config):
        return True
    if not config["gating"]["require_mention_in_groups"]:
        return True
    text = str(event.get("text") or "")
    if config["gating"]["commands_count_as_mention"] and text.lstrip().startswith("/"):
        return True
    bot_ids = {normalize_jid(value) for value in event.get("bot_ids") or []}
    mentioned = {normalize_jid(value) for value in event.get("mentioned_ids") or []}
    if bot_ids & mentioned:
        return True
    quoted = event.get("quoted")
    if config["gating"]["reply_to_bot_counts_as_mention"] and isinstance(quoted, dict) and normalize_jid(quoted.get("participant")) in bot_ids:
        return True
    for pattern in effective_mention_patterns(config):
        if pattern.search(text):
            return True
    return False


def record_denied(envelope, reason):
    with _STATE_LOCK:
        state = read_json(DENIED_PATH, {"schema_version": 1, "events": []})
        state = require_keys(state, {"schema_version", "events"}, set(), "whatsapp denied events")
        if state["schema_version"] != 1 or not isinstance(state["events"], list):
            raise ValueError("whatsapp denied events state is malformed")
        state["events"].append({"seq": envelope.get("seq"), "event_id": envelope.get("event_id"), "reason": reason, "at_unix_ms": int(time.time() * 1000)})
        state["events"] = state["events"][-2000:]
        atomic_write_json(DENIED_PATH, state, indent=2)


def grant_payload_media(config, binding, payload):
    payload = json.loads(json.dumps(payload))
    for event in payload.get("events") or [payload]:
        media = event.get("media") or []
        maximum = config["media"]["max_media_per_message"]
        if not isinstance(media, list) or len(media) > maximum:
            raise ValueError("whatsapp inbound media count exceeds max_media_per_message")
        for item in media:
            path = item.pop("payload_path", None)
            if path is not None:
                item.update(grant_media(binding, path))
    return payload


def combine_events(envelopes):
    events = [require_object(envelope["event"], "whatsapp bridge event") for envelope in envelopes]
    first = events[0]
    return {
        "event_type": "message.batch" if len(events) > 1 else first.get("event_type"),
        "event_id": "+".join(str(envelope["event_id"]) for envelope in envelopes),
        "bridge_event_seqs": [envelope["seq"] for envelope in envelopes],
        "chat_id": first.get("chat_id"),
        "sender_id": first.get("sender_id"),
        "sender_name": first.get("sender_name"),
        "is_group": first.get("is_group", False),
        "text": "\n".join(str(event.get("text") or "") for event in events if event.get("text")),
        "events": events,
    }


class GatewayDriver:
    def __init__(self, config):
        self.config = config
        self.event_queue = queue.Queue()
        self.deferred = []
        self.bridge = BridgeProcess(config, self.event_queue)
        self.hub = GatewayHub(config, self._binding_operation)
        self.stopping = threading.Event()
        self.retry_heap = []
        self.retry_counter = 0
        self.retry_chats = set()
        self.blocked_by_chat = {}
        self.typing_lock = threading.Lock()
        self.typing_generations = {}

    def _binding_operation(self, interface, operation, payload):
        if operation == "media.read":
            return read_granted_media(interface, payload.get("handle"), payload.get("sha256"), payload.get("size"), payload.get("token"), payload.get("offset"), payload.get("limit"))
        if operation == "outbound":
            payload = require_keys(payload, {"interface", "request"}, set(), "whatsapp outbound envelope")
            request = require_keys(payload["request"], {"operation", "payload"}, set(), "whatsapp outbound request")
            if payload.get("interface") != interface:
                raise ValueError("whatsapp outbound interface identity mismatch")
            binding = binding_by_interface(interface)
            if binding is None:
                raise ValueError("whatsapp outbound interface has no private binding")
            outbound_operation = require_string(request.get("operation"), "whatsapp outbound operation")
            outbound_payload = require_object(request.get("payload"), "whatsapp outbound payload")
            key = require_string(outbound_payload.get("idempotency_key"), "whatsapp outbound idempotency_key")
            if not key.startswith(interface + ":"):
                raise ValueError("whatsapp outbound idempotency key escaped its private interface")
            if outbound_operation != "effect.ack" and normalize_jid(outbound_payload.get("chat_id")) != binding["chat_id"]:
                raise ValueError("whatsapp outbound operation escaped its bound chat")
            if outbound_operation == "typing.stop":
                self._stop_typing(outbound_payload.get("chat_id"))
            return self.bridge.command(outbound_operation, outbound_payload)
        if operation == "bridge.health":
            return self.bridge.command("bridge.health", {})
        raise ValueError(f"unsupported whatsapp binding operation {operation!r}")

    def _next_event(self, timeout=None):
        if self.deferred:
            return self.deferred.pop(0)
        return self.event_queue.get(timeout=timeout)

    def _schedule_retry(self, envelopes, attempt):
        chat_id = normalize_jid(require_object(envelopes[0].get("event"), "whatsapp retry event").get("chat_id"))
        if not chat_id:
            raise RuntimeError("whatsapp retry event has no chat coordinate")
        base = self.config["bridge"]["reconnect_backoff_ms"]
        cap = self.config["bridge"]["reconnect_backoff_cap_ms"]
        delay = min(base * (2 ** min(attempt, 20)), cap)
        self.retry_counter += 1
        self.retry_chats.add(chat_id)
        heapq.heappush(self.retry_heap, (time.monotonic() + delay / 1000.0, self.retry_counter, attempt + 1, envelopes))

    def _release_chat(self, chat_id):
        self.retry_chats.discard(chat_id)
        blocked = self.blocked_by_chat.pop(chat_id, [])
        if blocked:
            self.deferred[0:0] = blocked

    def _batch(self, first):
        event = require_object(first.get("event"), "whatsapp bridge event")
        if event.get("event_type") != "message.create" or event.get("media"):
            return [first]
        config = self.config["gating"]
        delay = config["text_batch_split_delay_ms"] if len(str(event.get("text") or "")) >= config["text_batch_split_threshold"] else config["text_batch_delay_ms"]
        deadline = time.monotonic() + delay / 1000.0
        batch = [first]
        size = len(json.dumps(first, ensure_ascii=False).encode("utf-8"))
        while len(batch) < config["max_batch_events"] and size < config["max_batch_bytes"]:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            try:
                candidate = self._next_event(remaining)
            except queue.Empty:
                break
            other = require_object(candidate.get("event"), "whatsapp bridge event")
            candidate_size = len(json.dumps(candidate, ensure_ascii=False).encode("utf-8"))
            if other.get("event_type") == "message.create" and not other.get("media") and other.get("chat_id") == event.get("chat_id") and other.get("sender_id") == event.get("sender_id") and size + candidate_size <= config["max_batch_bytes"]:
                batch.append(candidate)
                size += candidate_size
                next_delay = config["text_batch_split_delay_ms"] if len(str(other.get("text") or "")) >= config["text_batch_split_threshold"] else config["text_batch_delay_ms"]
                deadline = time.monotonic() + next_delay / 1000.0
            else:
                self.deferred.append(candidate)
        return batch

    def _automatic_effect(self, operation, payload, key):
        try:
            return self.bridge.command(operation, {**payload, "idempotency_key": key})
        except Exception as error:
            path = os.path.join(VAR_DIR, "automatic-effect-failures.json")
            with _STATE_LOCK:
                state = read_json(path, {"schema_version": 1, "failures": []})
                if not isinstance(state, dict) or state.get("schema_version") != 1 or not isinstance(state.get("failures"), list):
                    raise
                state["failures"].append({"operation": operation, "key": key, "error": sanitize_external_error(error), "at_unix_ms": int(time.time() * 1000)})
                state["failures"] = state["failures"][-1000:]
                atomic_write_json(path, state, indent=2)
            return None

    def _stop_typing(self, chat_id):
        chat_id = normalize_jid(chat_id)
        if not chat_id:
            return
        with self.typing_lock:
            self.typing_generations.pop(chat_id, None)

    def _start_typing(self, chat_id, admitted_seq):
        chat_id = coordinate_key(chat_id)
        with self.typing_lock:
            generation = self.typing_generations.get(chat_id, 0) + 1
            self.typing_generations[chat_id] = generation
        self._automatic_effect("typing.start", {"chat_id": chat_id}, f"auto-typing:{admitted_seq}:0")

        def refresh():
            cycle = 1
            interval = self.config["outbound"]["typing"]["refresh_seconds"]
            while not self.stopping.wait(interval):
                with self.typing_lock:
                    if self.typing_generations.get(chat_id) != generation:
                        return
                self._automatic_effect("typing.start", {"chat_id": chat_id}, f"auto-typing:{admitted_seq}:{cycle}")
                cycle += 1

        threading.Thread(target=refresh, daemon=True).start()

    def _process(self, envelopes):
        admitted_envelopes = []
        for envelope in envelopes:
            event = require_object(envelope.get("event"), "whatsapp bridge event")
            allowed, reason = event_authorized(self.config, event)
            if allowed:
                admitted_envelopes.append(envelope)
            else:
                record_denied(envelope, reason)
                self.bridge.command("event.ack", {"seq": envelope["seq"]})
        if not admitted_envelopes:
            return
        admitted_events = [require_object(envelope["event"], "whatsapp bridge event") for envelope in admitted_envelopes]
        if all(event.get("event_type") == "message.create" and event.get("is_group") for event in admitted_events) and not any(event_should_trigger(self.config, event) for event in admitted_events):
            for envelope in admitted_envelopes:
                record_denied(envelope, "mention_required")
                self.bridge.command("event.ack", {"seq": envelope["seq"]})
            return
        payload = combine_events(admitted_envelopes)
        binding = ensure_private_binding(self.config, self.hub, payload["chat_id"])
        payload = grant_payload_media(self.config, binding, payload)
        admitted_seq = self.hub.dispatch(binding, payload)
        for envelope in admitted_envelopes:
            self.bridge.command("event.ack", {"seq": envelope["seq"]})
        message_keys = [{"id": event["message_id"], "remoteJid": payload["chat_id"], "fromMe": False, "participant": event.get("sender_id")} for event in payload.get("events") or [] if event.get("message_id")]
        if self.config["outbound"]["read_receipts"]["enabled"] and self.config["outbound"]["read_receipts"]["on_admission"] and message_keys:
            keys = message_keys
            self._automatic_effect("receipt.read", {"chat_id": payload["chat_id"], "message_keys": keys}, f"auto-read:{admitted_seq}")
        if self.config["outbound"]["typing"]["enabled"]:
            self._start_typing(payload["chat_id"], admitted_seq)
        if self.config["outbound"]["reactions"]["enabled"] and message_keys:
            latest = message_keys[-1]
            self._automatic_effect("reaction.set", {"chat_id": payload["chat_id"], "message_id": latest["id"], "emoji": self.config["outbound"]["reactions"]["in_progress"], "from_me": False, "participant_id": latest.get("participant")}, f"auto-progress:{admitted_seq}")

    def run(self):
        ensure_gateway_run(self.config)
        self.bridge.start()
        atomic_write_json(os.path.join(VAR_DIR, "gateway-reconnect.json"), {"schema_version": 1, "backoff_ms": self.config["bridge"]["reconnect_backoff_ms"]}, indent=2)
        reconcile_known_bindings(self.config, self.hub)
        proactive = set(self.config["proactive"]["chat_ids"]) | env_csv(self.config["proactive"]["chat_ids_env"])
        for chat_id in sorted(proactive):
            ensure_private_binding(self.config, self.hub, chat_id)
        try:
            while not self.stopping.is_set():
                now = time.monotonic()
                is_retry = bool(self.retry_heap and self.retry_heap[0][0] <= now)
                attempt = 0
                if is_retry:
                    _due, _counter, attempt, envelopes = heapq.heappop(self.retry_heap)
                else:
                    timeout = 0.5
                    if self.retry_heap:
                        timeout = max(0.0, min(timeout, self.retry_heap[0][0] - now))
                    try:
                        first = self._next_event(timeout)
                    except queue.Empty:
                        if self.bridge.reader_error is not None:
                            raise RuntimeError(
                                f"whatsapp bridge failed: {sanitize_external_error(self.bridge.reader_error)}"
                            )
                        continue
                    envelopes = self._batch(first)
                    chat_id = normalize_jid(require_object(envelopes[0].get("event"), "whatsapp bridge event").get("chat_id"))
                    if chat_id in self.retry_chats:
                        self.blocked_by_chat.setdefault(chat_id, []).extend(envelopes)
                        continue
                chat_id = normalize_jid(require_object(envelopes[0].get("event"), "whatsapp bridge event").get("chat_id"))
                try:
                    self._process(envelopes)
                    if is_retry:
                        self._release_chat(chat_id)
                except Exception as error:
                    sys.stderr.write(f"whatsapp event dispatch failed: {sanitize_external_error(error)}\n")
                    sys.stderr.flush()
                    self._schedule_retry(envelopes, attempt)
        finally:
            with self.typing_lock:
                self.typing_generations.clear()
            self.bridge.stop()


def materialize_granted_media(config, link, payload):
    payload = json.loads(json.dumps(payload))
    maximum = config["media"]["max_download_bytes"]
    frame_limit = config["driver_ipc"]["max_frame_bytes"]
    chunk_limit = min(1048576, max(1, ((frame_limit - 4096) * 3) // 4))
    for event_index, event in enumerate(payload.get("events") or [payload]):
        for media_index, item in enumerate(event.get("media") or []):
            handle = item.pop("media_handle", None)
            if handle is None:
                continue
            digest = require_string(item.pop("media_sha256", None), "whatsapp media digest")
            size = require_int(item.pop("media_size", None), "whatsapp media size", 0, maximum)
            token = require_string(item.pop("media_token", None), "whatsapp media token")
            filename = re.sub(r"[^A-Za-z0-9._-]", "_", str(item.get("filename") or handle))[:180]
            event_id = str(event.get("event_id") or payload.get("event_id") or "event")
            event_key = hashlib.sha256(event_id.encode("utf-8")).hexdigest()[:20]
            target = os.path.join(MEDIA_DIR, f"inbound-{event_key}-{event_index}-{media_index}-{digest[:16]}-{filename}")
            try:
                with open_private_read(target) as existing:
                    existing_bytes = existing.read(maximum + 1)
                if len(existing_bytes) != size or hashlib.sha256(existing_bytes).hexdigest() != digest:
                    raise RuntimeError("whatsapp cached inbound media evidence is inconsistent")
                item["payload_path"] = target
                item["sha256"] = digest
                continue
            except FileNotFoundError:
                pass
            staged = os.path.join(MEDIA_DIR, f".{os.path.basename(target)}.{os.getpid()}.{time.time_ns()}.tmp")
            offset = 0
            content_hash = hashlib.sha256()
            try:
                flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL
                with os.fdopen(_open_private_file(staged, flags, create=True), "wb") as sink:
                    while offset < size:
                        response = link.request("media.read", {"interface": INTERFACE_ID, "handle": handle, "sha256": digest, "size": size, "token": token, "offset": offset, "limit": chunk_limit})
                        response = require_keys(response, {"offset", "data_base64", "eof"}, set(), "whatsapp media.read response")
                        if require_int(response["offset"], "whatsapp media.read offset", 0) != offset:
                            raise RuntimeError("whatsapp media.read returned a discontinuous offset")
                        require_bool(response["eof"], "whatsapp media.read eof")
                        try:
                            chunk = base64.b64decode(require_string(response["data_base64"], "whatsapp media.read data", True), validate=True)
                        except ValueError as error:
                            raise RuntimeError("whatsapp media.read returned malformed base64") from error
                        if not chunk or offset + len(chunk) > size:
                            raise RuntimeError("whatsapp media.read returned an invalid chunk")
                        sink.write(chunk)
                        content_hash.update(chunk)
                        offset += len(chunk)
                        if response["eof"] != (offset == size):
                            raise RuntimeError("whatsapp media.read returned an invalid EOF boundary")
                    sink.flush()
                    os.fsync(sink.fileno())
                if content_hash.hexdigest() != digest:
                    raise RuntimeError("whatsapp media.read hash mismatch")
                try:
                    os.lstat(target)
                    raise RuntimeError("whatsapp cached inbound media appeared during materialization")
                except FileNotFoundError:
                    pass
                os.replace(staged, target)
            finally:
                with suppress(FileNotFoundError):
                    os.unlink(staged)
            item["payload_path"] = target
            item["sha256"] = digest
    return payload


def inject_text_documents(config, payload):
    extensions = set(config["media"]["inline_text_extensions"])
    maximum = config["media"]["inline_text_max_bytes"]
    payload = json.loads(json.dumps(payload))
    for event in payload.get("events") or [payload]:
        snippets = []
        for item in event.get("media") or []:
            path = item.get("payload_path")
            if not isinstance(path, str) or os.path.splitext(path)[1].lower() not in extensions:
                continue
            with open_private_read(path) as handle:
                size = os.fstat(handle.fileno()).st_size
                if size <= maximum:
                    body = handle.read(maximum + 1)
            if size > maximum:
                item["inline_text_failure"] = {"class": "size_limit", "size": size, "maximum": maximum}
                continue
            if len(body) > maximum:
                item["inline_text_failure"] = {"class": "size_limit", "size": len(body), "maximum": maximum}
                continue
            text = body.decode("utf-8", errors="replace")
            item["inline_text"] = text
            snippets.append(f"[Content of {item.get('filename') or os.path.basename(path)}]:\n{text}")
        if snippets:
            event["text"] = "\n\n".join(snippets + ([event["text"]] if event.get("text") else []))
    if payload.get("events"):
        payload["text"] = "\n".join(str(event.get("text") or "") for event in payload["events"] if event.get("text"))
    return payload


def load_binding_events():
    state = read_json(BINDING_EVENTS_PATH, {"schema_version": 1, "events": {}})
    state = require_keys(state, {"schema_version", "events"}, set(), "whatsapp binding events")
    if state["schema_version"] != 1 or not isinstance(state["events"], dict):
        raise ValueError("whatsapp binding events state is malformed")
    return state


def load_pending_turns():
    state = read_json(PENDING_TURNS_PATH, {"schema_version": 1, "pending": [], "completions": {}})
    state = require_keys(state, {"schema_version", "pending", "completions"}, set(), "whatsapp pending turns")
    if state["schema_version"] != 1 or not isinstance(state["pending"], list) or not isinstance(state["completions"], dict):
        raise ValueError("whatsapp pending turns state is malformed")
    return state


def record_pending_turn(admitted_seq, payload):
    events = payload.get("events") or [payload]
    message = None
    for event in events:
        if event.get("message_id"):
            message = {"message_id": event["message_id"], "sender_id": event.get("sender_id"), "from_me": event.get("from_me", False)}
    state = load_pending_turns()
    if any(entry.get("admitted_seq") == admitted_seq for entry in state["pending"]) or any(entry.get("admitted_seq") == admitted_seq for entry in state["completions"].values() if isinstance(entry, dict)):
        return
    state["pending"].append({"admitted_seq": admitted_seq, "message": message})
    if len(state["pending"]) > 2000:
        raise RuntimeError("whatsapp pending turn backlog exceeds its bound")
    atomic_write_json(PENDING_TURNS_PATH, state, indent=2)


def completion_turn(source_seq):
    with _STATE_LOCK:
        state = load_pending_turns()
        key = str(source_seq)
        existing = state["completions"].get(key)
        if existing is not None:
            return existing.get("message")
        entry = state["pending"].pop(0) if state["pending"] else {"admitted_seq": None, "message": None}
        state["completions"][key] = entry
        if len(state["completions"]) > 2000:
            state["completions"] = dict(list(state["completions"].items())[-2000:])
        atomic_write_json(PENDING_TURNS_PATH, state, indent=2)
        return entry.get("message")


def inbound_label(payload):
    event_type = payload.get("event_type")
    return {
        "message.batch": "whatsapp.inbound_batch",
        "message.create": "whatsapp.inbound",
        "message.edit": "whatsapp.message.edited",
        "message.delete": "whatsapp.message.deleted",
        "reaction.update": "whatsapp.reaction.updated",
        "receipt.update": "whatsapp.receipt.updated",
        "presence.update": "whatsapp.presence.updated",
        "poll.update": "whatsapp.poll.updated",
        "group.update": "whatsapp.group.updated",
        "group.participants.update": "whatsapp.group.participants.updated",
        "message.failure": "whatsapp.inbound.failure",
    }.get(event_type, "whatsapp.event")


def clean_bot_mentions(payload):
    payload = json.loads(json.dumps(payload))
    for event in payload.get("events") or [payload]:
        text = event.get("text")
        if not isinstance(text, str) or not text:
            continue
        for bot_id in event.get("bot_ids") or []:
            bare = normalize_identifier(bot_id)
            if bare:
                text = re.sub(rf"@{re.escape(bare)}\b[,;:\-]*\s*", "", text)
        event["text"] = text.strip() or event.get("text")
    if payload.get("events"):
        payload["text"] = "\n".join(str(event.get("text") or "") for event in payload["events"] if event.get("text"))
    return payload


def process_arrival(config, link, payload):
    payload = require_object(payload, "whatsapp binding arrival")
    binding = config["binding"]
    if normalize_jid(payload.get("chat_id")) != binding["chat_id"]:
        raise ValueError("whatsapp gateway delivered coordinates outside the private binding")
    event_id = require_string(payload.get("event_id"), "whatsapp binding event_id")
    payload = clean_bot_mentions(inject_text_documents(config, materialize_granted_media(config, link, payload)))
    with _STATE_LOCK:
        state = load_binding_events()
        existing = state["events"].get(event_id)
        if existing is not None:
            existing = require_int(existing, "whatsapp duplicate event admitted seq", 0)
            record_pending_turn(existing, payload)
            return existing
        admitted = hand_json(inbound_label(payload), payload, f"whatsapp-in-{hashlib.sha256(event_id.encode('utf-8')).hexdigest()[:40]}")
        state["events"][event_id] = admitted
        if len(state["events"]) > 10000:
            state["events"] = dict(list(state["events"].items())[-10000:])
        atomic_write_json(BINDING_EVENTS_PATH, state, indent=2)
        record_pending_turn(admitted, payload)
        return admitted


def load_cursor():
    state = read_json(CURSOR_PATH, {"schema_version": 1, "next_seq": 0})
    state = require_keys(state, {"schema_version", "next_seq"}, set(), "whatsapp ledger cursor")
    if state["schema_version"] != 1:
        raise ValueError("whatsapp ledger cursor schema_version must be 1")
    return require_int(state["next_seq"], "whatsapp ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_write_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def content_hexdigest(content):
    if isinstance(content, list):
        if any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content):
            raise ValueError("whatsapp movement artifact content contains a non-byte")
        content = bytes(content)
    if not isinstance(content, (bytes, bytearray)) or len(content) != 32:
        raise ValueError("whatsapp movement artifact content must be a 32-byte hash")
    return bytes(content).hex()


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"whatsapp {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"whatsapp {name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"whatsapp {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"whatsapp {name} grew above {maximum} bytes while reading")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def open_regular_file_stable(path, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise RuntimeError(f"whatsapp {name} changed while opening")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def materialize_payload(content, manifest_maximum, payload_maximum):
    manifest_hash = content_hexdigest(content)
    manifest_path = rooted_path(BLOBS_DIR, manifest_hash)
    manifest_bytes = read_bounded_regular(manifest_path, manifest_maximum, "cached artifact manifest")
    if octopus_content_hash(manifest_bytes) != manifest_hash:
        raise RuntimeError("whatsapp cached artifact manifest hash mismatch")
    manifest = require_object(json.loads(manifest_bytes), "whatsapp cached artifact manifest")
    payload_hash = manifest.get("payload_content_hash")
    if not isinstance(payload_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_hash):
        raise ValueError("whatsapp cached artifact manifest has invalid payload_content_hash")
    label = require_string(manifest.get("label"), "whatsapp cached artifact manifest.label")
    payload_path = rooted_path(BLOBS_DIR, payload_hash)
    payload = read_bounded_regular(payload_path, payload_maximum, "cached artifact payload")
    if octopus_content_hash(payload) != payload_hash:
        raise RuntimeError("whatsapp cached artifact payload hash mismatch")
    return label, payload


def movement_interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def require_outbound_runtime(movement):
    metadata = require_object(
        movement.get("invocation_metadata"),
        "whatsapp outbound movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "whatsapp outbound movement runtime metadata",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"whatsapp outbound movement runtime.{field}",
        )
    require_int(runtime.get("epoch"), "whatsapp outbound movement runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("whatsapp outbound runtime interface does not match its exact target")
    return runtime


def _read_outbound_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("3",):
            raise RuntimeError("whatsapp ledger index has unsupported schema")
        rows = connection.execute(
            "SELECT seq, artifact_content, runtime_run, runtime_checkpoint, from_id FROM movements "
            "WHERE runtime_interface = ? AND kind = 'InterfaceOutbound' "
            "AND from_kind = 'interface' AND to_kind = 'interface' AND to_id = ? AND seq >= ? "
            "ORDER BY seq ASC LIMIT ?",
            (INTERFACE_ID, INTERFACE_ID, next_seq, limit),
        ).fetchall()
    finally:
        connection.close()
    result = []
    for seq, content, run, checkpoint, source_interface in rows:
        seq = require_int(seq, "whatsapp ledger index seq", 0)
        require_string(source_interface, f"whatsapp ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content)):
            raise RuntimeError(f"whatsapp ledger index movement {seq} has malformed content")
        require_string(run, f"whatsapp ledger index movement {seq}.run")
        require_string(checkpoint, f"whatsapp ledger index movement {seq}.checkpoint")
        result.append((seq, None if content is None else bytes.fromhex(content), run))
    return result


def _read_outbound_ledger(next_seq, limit, max_row_bytes):
    global _LEDGER_FALLBACK_IDENTITY, _LEDGER_FALLBACK_OFFSET, _LEDGER_FALLBACK_SIZE
    max_row_bytes = require_int(max_row_bytes, "whatsapp ledger.max_row_bytes", 256, 16777216)
    try:
        handle = open_regular_file_stable(MOVEMENTS_JSONL, "movement ledger")
    except FileNotFoundError:
        return []
    result = []
    with handle:
        status = os.fstat(handle.fileno())
        size = status.st_size
        identity = (status.st_dev, status.st_ino)
        if identity != _LEDGER_FALLBACK_IDENTITY or size < _LEDGER_FALLBACK_SIZE or _LEDGER_FALLBACK_OFFSET > size:
            _LEDGER_FALLBACK_OFFSET = 0
        handle.seek(_LEDGER_FALLBACK_OFFSET)
        safe_offset = _LEDGER_FALLBACK_OFFSET
        has_uncommitted = False
        while True:
            row_offset = handle.tell()
            line = handle.readline(max_row_bytes + 1)
            if not line:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                break
            if len(line) > max_row_bytes:
                raise RuntimeError(f"whatsapp ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
            if not line.endswith(b"\n"):
                raise RuntimeError(f"whatsapp ledger row at byte {row_offset} is partial")
            movement = require_object(json.loads(line), "whatsapp ledger movement")
            seq = require_int(movement.get("seq"), "whatsapp ledger movement.seq", 0)
            if seq < next_seq:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            if movement.get("kind") != "InterfaceOutbound":
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            if movement_interface_endpoint(movement.get("to")) != INTERFACE_ID:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            if movement_interface_endpoint(movement.get("from")) is None:
                if not has_uncommitted:
                    safe_offset = handle.tell()
                continue
            runtime = require_outbound_runtime(movement)
            artifact = movement.get("artifact")
            if not has_uncommitted:
                safe_offset = row_offset
                has_uncommitted = True
            result.append((seq, None if not artifact else artifact.get("content"), runtime.get("run")))
            if len(result) >= limit:
                break
        _LEDGER_FALLBACK_OFFSET = safe_offset
        _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
        _LEDGER_FALLBACK_IDENTITY = identity
    return result


def read_own_outbound(next_seq, limit=128, *, max_row_bytes):
    global _LEDGER_INDEX_WARNING
    try:
        return _read_outbound_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = sanitize_external_error(error)
        if message != _LEDGER_INDEX_WARNING:
            sys.stderr.write(f"whatsapp ledger index unavailable; reading ledger truth: {message}\n")
            sys.stderr.flush()
            _LEDGER_INDEX_WARNING = message
        return _read_outbound_ledger(next_seq, limit, max_row_bytes)


def effect_path(source_seq):
    return os.path.join(EFFECTS_DIR, f"{source_seq}.json")


def validate_effect(source_seq, state):
    state = require_keys(state, {"schema_version", "source_movement_seq", "source_artifact_hash", "run", "artifact_label", "status"}, {"request", "result_base", "state_updates", "delivery", "result", "error", "evidence_movement_seq"}, f"whatsapp effect {source_seq}")
    if state["schema_version"] != 1 or state["source_movement_seq"] != source_seq:
        raise ValueError(f"whatsapp effect {source_seq} identity is invalid")
    if not isinstance(state["source_artifact_hash"], str) or not re.fullmatch(r"[0-9a-f]{64}", state["source_artifact_hash"]):
        raise ValueError(f"whatsapp effect {source_seq} artifact hash is invalid")
    require_string(state["run"], f"whatsapp effect {source_seq}.run")
    require_string(state["artifact_label"], f"whatsapp effect {source_seq}.artifact_label")
    if state["status"] not in ("started", "delivered", "succeeded", "indeterminate", "evidenced", "failure_evidenced"):
        raise ValueError(f"whatsapp effect {source_seq}.status is invalid")
    if state["status"] in ("started", "delivered"):
        require_object(state.get("request"), f"whatsapp effect {source_seq}.request")
        require_object(state.get("result_base"), f"whatsapp effect {source_seq}.result_base")
        require_object(state.get("state_updates"), f"whatsapp effect {source_seq}.state_updates")
    if state["status"] == "delivered":
        require_object(state.get("delivery"), f"whatsapp effect {source_seq}.delivery")
    if state["status"] in ("succeeded", "evidenced") and not isinstance(state.get("result"), dict):
        raise ValueError(f"whatsapp effect {source_seq} success has no result")
    if state["status"] == "indeterminate":
        require_string(state.get("error"), f"whatsapp effect {source_seq}.error")
    if state["status"] in ("evidenced", "failure_evidenced"):
        require_int(state.get("evidence_movement_seq"), f"whatsapp effect {source_seq}.evidence_movement_seq", 0)
    return state


def load_effect(source_seq):
    state = read_json(effect_path(source_seq))
    if state is None:
        return None
    sanitized = sanitize_external_error(state)
    if sanitized != state:
        atomic_write_json(effect_path(source_seq), sanitized, indent=2)
    return validate_effect(source_seq, sanitized)


def commit_effect(source_seq, state):
    atomic_write_json(effect_path(source_seq), validate_effect(source_seq, sanitize_external_error(state)), indent=2)


def hand_delivery_result(source_seq, content_hash, run, result):
    return hand_json("whatsapp.delivery.result", {"source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "ok": True, "result": result}, f"whatsapp-delivery-result-{source_seq}")


def format_message(content):
    if not content:
        return content
    fences = []
    codes = []

    def save_fence(match):
        fences.append(match.group(0))
        return f"\x00FENCE{len(fences) - 1}\x00"

    def save_code(match):
        codes.append(match.group(0))
        return f"\x00CODE{len(codes) - 1}\x00"

    result = re.sub(r"```[\s\S]*?```", save_fence, content)
    result = re.sub(r"`[^`\n]+`", save_code, result)
    result = re.sub(r"\*\*(.+?)\*\*", r"*\1*", result)
    result = re.sub(r"__(.+?)__", r"*\1*", result)
    result = re.sub(r"~~(.+?)~~", r"~\1~", result)

    def header(match):
        inner = match.group(1).strip()
        while len(inner) > 1 and inner.startswith("*") and inner.endswith("*"):
            inner = inner[1:-1].strip()
        return f"*{inner}*"

    result = re.sub(r"^#{1,6}\s+(.+)$", header, result, flags=re.MULTILINE)
    result = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r"\1 (\2)", result)
    for index, value in enumerate(fences):
        result = result.replace(f"\x00FENCE{index}\x00", value)
    for index, value in enumerate(codes):
        result = result.replace(f"\x00CODE{index}\x00", value)
    return result


def safe_blob_path(blob_id, maximum):
    blob_id = require_string(blob_id, "whatsapp blob_id")
    if not re.fullmatch(r"[0-9a-f]{64}", blob_id):
        raise ValueError("whatsapp blob_id must be an Octopus content hash")
    payload = read_bounded_regular(os.path.join(BLOBS_DIR, blob_id), maximum, "outbound media blob")
    if octopus_content_hash(payload) != blob_id:
        raise RuntimeError("whatsapp outbound blob hash mismatch")
    path = os.path.join(MEDIA_DIR, f"outbound-{blob_id}")
    try:
        with open_private_read(path) as existing:
            if existing.read(maximum + 1) == payload:
                return path
    except FileNotFoundError:
        pass
    atomic_write_bytes(path, payload)
    return path


def parse_payload_json(payload):
    try:
        return json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError):
        try:
            return payload.decode("utf-8")
        except UnicodeDecodeError as error:
            raise ValueError("whatsapp outbound payload is neither JSON nor UTF-8 text") from error


def require_bound_chat(binding, value):
    if value is not None and normalize_jid(value) != binding["chat_id"]:
        raise ValueError("whatsapp outbound artifact tried to escape its bound chat")
    return binding["chat_id"]


def message_key(value, chat_id, name):
    value = require_object(value, name)
    value = require_keys(value, {"id"}, {"remoteJid", "fromMe", "participant"}, name)
    return {
        "id": require_string(value["id"], f"{name}.id"),
        "remoteJid": require_bound_chat({"chat_id": chat_id}, value.get("remoteJid")),
        "fromMe": require_bool(value.get("fromMe", False), f"{name}.fromMe"),
        **({"participant": normalize_jid(require_string(value["participant"], f"{name}.participant"))} if value.get("participant") is not None else {}),
    }


def require_group_chat(chat_id, operation):
    if not chat_id.endswith("@g.us"):
        raise ValueError(f"whatsapp {operation} requires a group/community binding")
    return chat_id


def require_participants(value, name):
    if not isinstance(value, list) or not value or len(value) > 100:
        raise ValueError(f"{name} must contain 1..100 participants")
    participants = [normalize_jid(require_string(item, f"{name}[{index}]")) for index, item in enumerate(value)]
    if any(not item.endswith(("@s.whatsapp.net", "@lid")) for item in participants) or len(participants) != len(set(participants)):
        raise ValueError(f"{name} contains malformed or duplicate participant identities")
    return participants


def add_reply_target(body, payload, name):
    if body.get("reply_to_message_id") is not None:
        payload["reply_to_message_id"] = require_string(body["reply_to_message_id"], f"{name}.reply_to_message_id")


def operation_request(config, body, binding, source_seq):
    body = require_object(body, "whatsapp outbound operation")
    operation = require_string(body.get("operation"), "whatsapp outbound operation.operation")
    if operation not in config["outbound"]["allowed_operations"]:
        raise ValueError(f"whatsapp outbound operation {operation!r} is not declared")
    chat_id = require_bound_chat(binding, body.get("chat_id"))
    common = {"operation", "chat_id"}
    payload = {"chat_id": chat_id, "idempotency_key": f"{INTERFACE_ID}:{source_seq}:{operation}"}
    if operation == "message.send":
        body = require_keys(body, {"operation", "text"}, common | {"reply_to_message_id"}, "whatsapp message.send")
        payload.update({"text": format_message(require_text(body["text"], "whatsapp message.send.text")), "max_chars": config["outbound"]["max_message_chars"], "chunk_delay_ms": config["outbound"]["chunk_delay_ms"]})
        if body.get("reply_to_message_id") is not None:
            payload["reply_to_message_id"] = require_string(body["reply_to_message_id"], "whatsapp message.send.reply_to_message_id")
    elif operation == "message.edit":
        body = require_keys(body, {"operation", "message_id", "text"}, common, "whatsapp message.edit")
        payload.update({"message_id": require_string(body["message_id"], "whatsapp message.edit.message_id"), "text": format_message(require_text(body["text"], "whatsapp message.edit.text")), "max_chars": config["outbound"]["max_message_chars"], "chunk_delay_ms": config["outbound"]["chunk_delay_ms"]})
    elif operation == "message.delete":
        body = require_keys(body, {"operation", "message_id"}, common | {"from_me", "participant_id"}, "whatsapp message.delete")
        payload.update({"message_id": require_string(body["message_id"], "whatsapp message.delete.message_id"), "from_me": require_bool(body.get("from_me", True), "whatsapp message.delete.from_me")})
        if body.get("participant_id") is not None:
            payload["participant_id"] = normalize_jid(require_string(body["participant_id"], "whatsapp message.delete.participant_id"))
    elif operation in ("reaction.set", "reaction.clear"):
        required = {"operation", "message_id"} | ({"emoji"} if operation == "reaction.set" else set())
        body = require_keys(body, required, common | {"emoji", "from_me", "participant_id"}, f"whatsapp {operation}")
        payload.update({"message_id": require_string(body["message_id"], f"whatsapp {operation}.message_id"), "from_me": require_bool(body.get("from_me", False), f"whatsapp {operation}.from_me")})
        if operation == "reaction.set":
            payload["emoji"] = require_string(body["emoji"], "whatsapp reaction.set.emoji")
        if body.get("participant_id") is not None:
            payload["participant_id"] = normalize_jid(require_string(body["participant_id"], f"whatsapp {operation}.participant_id"))
    elif operation == "receipt.read":
        body = require_keys(body, {"operation", "message_keys"}, common, "whatsapp receipt.read")
        if not isinstance(body["message_keys"], list) or not body["message_keys"] or len(body["message_keys"]) > 100:
            raise ValueError("whatsapp receipt.read.message_keys must contain 1..100 keys")
        payload["message_keys"] = [message_key(value, chat_id, f"whatsapp receipt.read.message_keys[{index}]") for index, value in enumerate(body["message_keys"])]
    elif operation in ("typing.start", "typing.stop"):
        require_keys(body, {"operation"}, common, f"whatsapp {operation}")
    elif operation == "presence.set":
        body = require_keys(body, {"operation", "presence"}, common, "whatsapp presence.set")
        presence = require_string(body["presence"], "whatsapp presence.set.presence")
        if presence not in ("available", "unavailable", "composing", "recording", "paused"):
            raise ValueError("whatsapp presence is unsupported")
        payload["presence"] = presence
    elif operation in ("media.send", "sticker.send"):
        required = {"operation", "mime_type"} | ({"media_type"} if operation == "media.send" else set())
        body = require_keys(body, required, common | {"blob_id", "artifact_ref", "artifact_id", "media_type", "filename", "caption", "reply_to_message_id", "ptt", "gif_playback"}, f"whatsapp {operation}")
        artifact_ref = body.get("artifact_ref")
        ref_blob_id = None
        ref_artifact_id = None
        ref_content_hash = None
        if artifact_ref is not None:
            artifact_ref = require_keys(artifact_ref, set(), {"blob_id", "artifact_id", "content_hash"}, f"whatsapp {operation}.artifact_ref")
            if artifact_ref.get("blob_id") is not None:
                ref_blob_id = require_string(artifact_ref["blob_id"], f"whatsapp {operation}.artifact_ref.blob_id")
            if artifact_ref.get("artifact_id") is not None:
                ref_artifact_id = require_string(artifact_ref["artifact_id"], f"whatsapp {operation}.artifact_ref.artifact_id")
            if artifact_ref.get("content_hash") is not None:
                ref_content_hash = require_string(artifact_ref["content_hash"], f"whatsapp {operation}.artifact_ref.content_hash")
                if not re.fullmatch(r"[0-9a-f]{64}", ref_content_hash):
                    raise ValueError(f"whatsapp {operation}.artifact_ref.content_hash must be a sha256 digest")
                if ref_blob_id is not None and ref_content_hash != ref_blob_id:
                    raise ValueError(f"whatsapp {operation} artifact content references disagree")
        blob_id = body.get("blob_id") or ref_blob_id
        if not isinstance(blob_id, str):
            raise ValueError(f"whatsapp {operation} requires blob_id directly or in artifact_ref")
        if body.get("blob_id") is not None and ref_blob_id is not None and body["blob_id"] != ref_blob_id:
            raise ValueError(f"whatsapp {operation} blob references disagree")
        if ref_content_hash is not None and ref_content_hash != blob_id:
            raise ValueError(f"whatsapp {operation} artifact content hash disagrees with blob_id")
        path = safe_blob_path(blob_id, config["media"]["max_upload_bytes"])
        size = os.path.getsize(path)
        if size > config["media"]["max_upload_bytes"]:
            raise ValueError("whatsapp outbound blob exceeds max_upload_bytes")
        mime_type = require_string(body["mime_type"], f"whatsapp {operation}.mime_type")
        if not any(mime_type.startswith(prefix) for prefix in config["media"]["allowed_outbound_mime_prefixes"]):
            raise ValueError("whatsapp outbound MIME type is not declared")
        payload.update({"payload_path": path, "sha256": file_hexdigest(path), "size": size, "mime_type": mime_type, "blob_id": blob_id})
        artifact_id = body.get("artifact_id") or ref_artifact_id
        if body.get("artifact_id") is not None and ref_artifact_id is not None and body["artifact_id"] != ref_artifact_id:
            raise ValueError(f"whatsapp {operation} artifact identifiers disagree")
        if artifact_id is not None:
            payload["artifact_id"] = require_string(artifact_id, f"whatsapp {operation}.artifact_id")
        if operation == "media.send":
            media_type = require_string(body["media_type"], "whatsapp media.send.media_type")
            if media_type not in ("image", "video", "audio", "document"):
                raise ValueError("whatsapp media.send.media_type is unsupported")
            if media_type != "document" and not mime_type.startswith(f"{media_type}/"):
                raise ValueError("whatsapp media.send MIME type does not match media_type")
            payload["media_type"] = media_type
        elif mime_type != "image/webp":
            raise ValueError("whatsapp sticker.send requires image/webp")
        if body.get("filename") is not None:
            payload["filename"] = require_string(body["filename"], f"whatsapp {operation}.filename")
        if body.get("caption") is not None:
            if operation == "media.send" and payload.get("media_type") == "audio":
                raise ValueError("whatsapp audio media does not support captions")
            payload["caption"] = format_message(require_text(body["caption"], f"whatsapp {operation}.caption", True))
        if body.get("reply_to_message_id") is not None:
            payload["reply_to_message_id"] = require_string(body["reply_to_message_id"], f"whatsapp {operation}.reply_to_message_id")
        payload["ptt"] = require_bool(body.get("ptt", False), f"whatsapp {operation}.ptt")
        payload["gif_playback"] = require_bool(body.get("gif_playback", False), f"whatsapp {operation}.gif_playback")
        if payload["ptt"] and (operation != "media.send" or payload.get("media_type") != "audio"):
            raise ValueError("whatsapp ptt is valid only for audio media")
        if payload["gif_playback"] and (operation != "media.send" or payload.get("media_type") != "video"):
            raise ValueError("whatsapp gif_playback is valid only for video media")
    elif operation == "location.send":
        body = require_keys(body, {"operation", "latitude", "longitude"}, common | {"name", "address"}, "whatsapp location.send")
        payload.update({"latitude": require_number(body["latitude"], "whatsapp location.send.latitude", -90, 90), "longitude": require_number(body["longitude"], "whatsapp location.send.longitude", -180, 180)})
        for key in ("name", "address"):
            if body.get(key) is not None:
                payload[key] = require_string(body[key], f"whatsapp location.send.{key}", True)
    elif operation == "contact.send":
        body = require_keys(body, {"operation", "contacts"}, common, "whatsapp contact.send")
        if not isinstance(body["contacts"], list) or not body["contacts"] or len(body["contacts"]) > 100:
            raise ValueError("whatsapp contact.send.contacts must contain 1..100 contacts")
        contacts = []
        for index, contact in enumerate(body["contacts"]):
            contact = require_keys(contact, {"display_name", "vcard"}, set(), f"whatsapp contact[{index}]")
            contacts.append({"display_name": require_string(contact["display_name"], f"whatsapp contact[{index}].display_name"), "vcard": require_string(contact["vcard"], f"whatsapp contact[{index}].vcard")})
        payload["contacts"] = contacts
    elif operation == "poll.send":
        body = require_keys(body, {"operation", "question", "options"}, common | {"selectable_count", "reply_to_message_id"}, "whatsapp poll.send")
        options = require_string_list(body["options"], "whatsapp poll.send.options")
        if not 2 <= len(options) <= 12 or len(options) != len(set(options)):
            raise ValueError("whatsapp poll.send.options must contain 2..12 unique values")
        selectable = require_int(body.get("selectable_count", 1), "whatsapp poll.send.selectable_count", 1, len(options))
        payload.update({"question": require_text(body["question"], "whatsapp poll.send.question"), "options": options, "selectable_count": selectable})
        add_reply_target(body, payload, "whatsapp poll.send")
    elif operation == "interactive.buttons.send":
        body = require_keys(body, {"operation", "text", "buttons"}, common | {"title", "footer", "reply_to_message_id"}, "whatsapp interactive.buttons.send")
        if not isinstance(body["buttons"], list) or not 1 <= len(body["buttons"]) <= 3:
            raise ValueError("whatsapp interactive.buttons.send.buttons must contain 1..3 buttons")
        buttons = []
        button_ids = set()
        for index, button in enumerate(body["buttons"]):
            button = require_keys(button, {"id", "text"}, set(), f"whatsapp button[{index}]")
            button_id = require_string(button["id"], f"whatsapp button[{index}].id")
            if button_id in button_ids:
                raise ValueError("whatsapp interactive button ids must be unique")
            button_ids.add(button_id)
            buttons.append({"id": button_id, "text": require_text(button["text"], f"whatsapp button[{index}].text")})
        payload.update({"text": require_text(body["text"], "whatsapp interactive.buttons.send.text"), "buttons": buttons})
        for key in ("title", "footer"):
            if body.get(key) is not None:
                payload[key] = require_text(body[key], f"whatsapp interactive.buttons.send.{key}", True)
        add_reply_target(body, payload, "whatsapp interactive.buttons.send")
    elif operation == "interactive.list.send":
        body = require_keys(body, {"operation", "text", "button_text", "sections"}, common | {"title", "footer", "reply_to_message_id"}, "whatsapp interactive.list.send")
        if not isinstance(body["sections"], list) or not 1 <= len(body["sections"]) <= 10:
            raise ValueError("whatsapp interactive.list.send.sections must contain 1..10 sections")
        sections = []
        row_ids = set()
        for section_index, section in enumerate(body["sections"]):
            section = require_keys(section, {"rows"}, {"title"}, f"whatsapp list section[{section_index}]")
            rows = section["rows"]
            if not isinstance(rows, list) or not 1 <= len(rows) <= 10:
                raise ValueError(f"whatsapp list section[{section_index}].rows must contain 1..10 rows")
            normalized_rows = []
            for row_index, row in enumerate(rows):
                row = require_keys(row, {"id", "title"}, {"description"}, f"whatsapp list row[{section_index}][{row_index}]")
                row_id = require_string(row["id"], f"whatsapp list row[{section_index}][{row_index}].id")
                if row_id in row_ids:
                    raise ValueError("whatsapp interactive list row ids must be unique")
                row_ids.add(row_id)
                normalized = {"id": row_id, "title": require_text(row["title"], f"whatsapp list row[{section_index}][{row_index}].title")}
                if row.get("description") is not None:
                    normalized["description"] = require_text(row["description"], f"whatsapp list row[{section_index}][{row_index}].description", True)
                normalized_rows.append(normalized)
            normalized_section = {"rows": normalized_rows}
            if section.get("title") is not None:
                normalized_section["title"] = require_text(section["title"], f"whatsapp list section[{section_index}].title", True)
            sections.append(normalized_section)
        payload.update({"text": require_text(body["text"], "whatsapp interactive.list.send.text"), "button_text": require_text(body["button_text"], "whatsapp interactive.list.send.button_text"), "sections": sections})
        for key in ("title", "footer"):
            if body.get(key) is not None:
                payload[key] = require_text(body[key], f"whatsapp interactive.list.send.{key}", True)
        add_reply_target(body, payload, "whatsapp interactive.list.send")
    elif operation == "interactive.template.send":
        body = require_keys(body, {"operation", "text", "buttons"}, common | {"footer", "reply_to_message_id"}, "whatsapp interactive.template.send")
        if not isinstance(body["buttons"], list) or not 1 <= len(body["buttons"]) <= 3:
            raise ValueError("whatsapp interactive.template.send.buttons must contain 1..3 buttons")
        buttons = []
        quick_reply_ids = set()
        for index, button in enumerate(body["buttons"]):
            button = require_keys(button, {"type", "text"}, {"id", "url", "phone_number"}, f"whatsapp template button[{index}]")
            button_type = require_string(button["type"], f"whatsapp template button[{index}].type")
            normalized = {"type": button_type, "text": require_text(button["text"], f"whatsapp template button[{index}].text")}
            if button_type == "quick_reply":
                button_id = require_string(button.get("id"), f"whatsapp template button[{index}].id")
                if button_id in quick_reply_ids:
                    raise ValueError("whatsapp template quick-reply ids must be unique")
                quick_reply_ids.add(button_id)
                normalized["id"] = button_id
            elif button_type == "url":
                url = require_string(button.get("url"), f"whatsapp template button[{index}].url")
                if urllib.parse.urlparse(url).scheme not in ("http", "https"):
                    raise ValueError("whatsapp template URL button requires http or https")
                normalized["url"] = url
            elif button_type == "call":
                normalized["phone_number"] = require_string(button.get("phone_number"), f"whatsapp template button[{index}].phone_number")
            else:
                raise ValueError("whatsapp template button type is unsupported")
            buttons.append(normalized)
        payload.update({"text": require_text(body["text"], "whatsapp interactive.template.send.text"), "buttons": buttons})
        if body.get("footer") is not None:
            payload["footer"] = require_text(body["footer"], "whatsapp interactive.template.send.footer", True)
        add_reply_target(body, payload, "whatsapp interactive.template.send")
    elif operation == "chat.info":
        require_keys(body, {"operation"}, common, "whatsapp chat.info")
    elif operation == "group.participants.update":
        body = require_keys(body, {"operation", "participants", "action"}, common, "whatsapp group.participants.update")
        require_group_chat(chat_id, operation)
        action = require_string(body["action"], "whatsapp group.participants.update.action")
        if action not in ("add", "remove", "promote", "demote"):
            raise ValueError("whatsapp group participant action is unsupported")
        payload.update({"participants": require_participants(body["participants"], "whatsapp group.participants.update.participants"), "action": action})
    elif operation == "group.subject.set":
        body = require_keys(body, {"operation", "subject"}, common, "whatsapp group.subject.set")
        require_group_chat(chat_id, operation)
        subject = require_text(body["subject"], "whatsapp group.subject.set.subject")
        if len(subject) > 100:
            raise ValueError("whatsapp group subject exceeds 100 characters")
        payload["subject"] = subject
    elif operation == "group.description.set":
        body = require_keys(body, {"operation", "description"}, common, "whatsapp group.description.set")
        require_group_chat(chat_id, operation)
        description = require_text(body["description"], "whatsapp group.description.set.description", True)
        if len(description) > 4096:
            raise ValueError("whatsapp group description exceeds 4096 characters")
        payload["description"] = description
    elif operation == "group.setting.set":
        body = require_keys(body, {"operation", "setting"}, common, "whatsapp group.setting.set")
        require_group_chat(chat_id, operation)
        setting = require_string(body["setting"], "whatsapp group.setting.set.setting")
        if setting not in ("announcement", "not_announcement", "locked", "unlocked"):
            raise ValueError("whatsapp group setting is unsupported")
        payload["setting"] = setting
    elif operation in ("group.invite.get", "group.invite.revoke", "group.join_requests.list", "group.leave"):
        require_keys(body, {"operation"}, common, f"whatsapp {operation}")
        require_group_chat(chat_id, operation)
    elif operation == "group.join_requests.update":
        body = require_keys(body, {"operation", "participants", "action"}, common, "whatsapp group.join_requests.update")
        require_group_chat(chat_id, operation)
        action = require_string(body["action"], "whatsapp group.join_requests.update.action")
        if action not in ("approve", "reject"):
            raise ValueError("whatsapp group join request action is unsupported")
        payload.update({"participants": require_participants(body["participants"], "whatsapp group.join_requests.update.participants"), "action": action})
    elif operation == "group.ephemeral.set":
        body = require_keys(body, {"operation", "expiration_seconds"}, common, "whatsapp group.ephemeral.set")
        require_group_chat(chat_id, operation)
        expiration = require_int(body["expiration_seconds"], "whatsapp group.ephemeral.set.expiration_seconds", 0)
        if expiration not in (0, 86400, 604800, 7776000):
            raise ValueError("whatsapp group ephemeral expiration is unsupported")
        payload["expiration_seconds"] = expiration
    elif operation in ("community.info", "community.invite.get", "community.invite.revoke", "community.linked_groups.list", "community.join_requests.list", "community.leave"):
        require_keys(body, {"operation"}, common, f"whatsapp {operation}")
        require_group_chat(chat_id, operation)
    elif operation == "community.participants.update":
        body = require_keys(body, {"operation", "participants", "action"}, common, "whatsapp community.participants.update")
        require_group_chat(chat_id, operation)
        action = require_string(body["action"], "whatsapp community.participants.update.action")
        if action not in ("add", "remove", "promote", "demote"):
            raise ValueError("whatsapp community participant action is unsupported")
        payload.update({"participants": require_participants(body["participants"], "whatsapp community.participants.update.participants"), "action": action})
    elif operation in ("community.subject.set", "community.description.set"):
        field = "subject" if operation == "community.subject.set" else "description"
        body = require_keys(body, {"operation", field}, common, f"whatsapp {operation}")
        require_group_chat(chat_id, operation)
        value = require_text(body[field], f"whatsapp {operation}.{field}", operation.endswith("description.set"))
        if len(value) > (100 if field == "subject" else 4096):
            raise ValueError(f"whatsapp {operation}.{field} exceeds its bound")
        payload[field] = value
    elif operation == "community.setting.set":
        body = require_keys(body, {"operation", "setting"}, common, "whatsapp community.setting.set")
        require_group_chat(chat_id, operation)
        setting = require_string(body["setting"], "whatsapp community.setting.set.setting")
        if setting not in ("announcement", "not_announcement", "locked", "unlocked"):
            raise ValueError("whatsapp community setting is unsupported")
        payload["setting"] = setting
    elif operation == "community.join_requests.update":
        body = require_keys(body, {"operation", "participants", "action"}, common, "whatsapp community.join_requests.update")
        require_group_chat(chat_id, operation)
        action = require_string(body["action"], "whatsapp community.join_requests.update.action")
        if action not in ("approve", "reject"):
            raise ValueError("whatsapp community join request action is unsupported")
        payload.update({"participants": require_participants(body["participants"], "whatsapp community.join_requests.update.participants"), "action": action})
    elif operation == "community.member_add_mode.set":
        body = require_keys(body, {"operation", "mode"}, common, "whatsapp community.member_add_mode.set")
        require_group_chat(chat_id, operation)
        mode = require_string(body["mode"], "whatsapp community.member_add_mode.set.mode")
        if mode not in ("admin_add", "all_member_add"):
            raise ValueError("whatsapp community member add mode is unsupported")
        payload["mode"] = mode
    elif operation == "community.join_approval_mode.set":
        body = require_keys(body, {"operation", "mode"}, common, "whatsapp community.join_approval_mode.set")
        require_group_chat(chat_id, operation)
        mode = require_string(body["mode"], "whatsapp community.join_approval_mode.set.mode")
        if mode not in ("on", "off"):
            raise ValueError("whatsapp community join approval mode is unsupported")
        payload["mode"] = mode
    elif operation == "community.ephemeral.set":
        body = require_keys(body, {"operation", "expiration_seconds"}, common, "whatsapp community.ephemeral.set")
        require_group_chat(chat_id, operation)
        expiration = require_int(body["expiration_seconds"], "whatsapp community.ephemeral.set.expiration_seconds", 0)
        if expiration not in (0, 86400, 604800, 7776000):
            raise ValueError("whatsapp community ephemeral expiration is unsupported")
        payload["expiration_seconds"] = expiration
    else:
        raise ValueError(f"whatsapp operation {operation!r} is unimplemented")
    return {"operation": operation, "payload": payload}


def stream_state():
    state = read_json(STREAM_STATE_PATH, {"schema_version": 1, "message_id": None, "text": "", "last_seq": None, "last_edit_ms": 0})
    state = require_keys(state, {"schema_version", "message_id", "text", "last_seq", "last_edit_ms"}, set(), "whatsapp stream state")
    if state["schema_version"] != 1:
        raise ValueError("whatsapp stream state schema_version must be 1")
    if state["message_id"] is not None:
        require_string(state["message_id"], "whatsapp stream state.message_id")
    require_string(state["text"], "whatsapp stream state.text", True)
    if state["last_seq"] is not None:
        require_int(state["last_seq"], "whatsapp stream state.last_seq", 0)
    require_int(state["last_edit_ms"], "whatsapp stream state.last_edit_ms", 0)
    return state


def update_stream_state(**fields):
    with _STATE_LOCK:
        state = stream_state()
        state.update(fields)
        atomic_write_json(STREAM_STATE_PATH, state, indent=2)
        return state


def text_from_body(body):
    if isinstance(body, str):
        return body
    if not isinstance(body, dict):
        raise ValueError("whatsapp text artifact body must be a string or object")
    for key in ("text", "content", "message", "delta", "error"):
        if isinstance(body.get(key), str):
            return body[key]
    raise ValueError("whatsapp text artifact body has no text field")


def cleared_stream(source_seq):
    return {"text": "", "message_id": None, "last_seq": source_seq, "last_edit_ms": 0}


def decode_output(config, label, payload, binding, source_seq):
    body = parse_payload_json(payload)
    outbound = config["outbound"]
    if label in outbound["telemetry_labels"]:
        return None, {"ignored": True, "label": label}, {}
    if label in outbound["stream_labels"]:
        delta = text_from_body(body)
        state = stream_state()
        if state["last_seq"] is not None and source_seq <= state["last_seq"]:
            return None, {"duplicate_stream_delta": True, "label": label}, {}
        text = state["text"] + delta
        now = int(time.time() * 1000)
        if state["message_id"] is None:
            preview = text if len(text) <= outbound["max_message_chars"] else text[: outbound["max_message_chars"] - 1] + "…"
            request = operation_request(config, {"operation": "message.send", "text": preview}, binding, source_seq)
            return request, {"label": label, "stream": True}, {"text": text, "last_seq": source_seq}
        if now - state["last_edit_ms"] < outbound["min_edit_interval_ms"]:
            return None, {"label": label, "stream_buffered": True}, {"text": text, "last_seq": source_seq}
        preview = text if len(text) <= outbound["max_message_chars"] else text[: outbound["max_message_chars"] - 1] + "…"
        request = operation_request(config, {"operation": "message.edit", "message_id": state["message_id"], "text": preview}, binding, source_seq)
        return request, {"label": label, "stream": True}, {"text": text, "last_seq": source_seq, "last_edit_ms": now}
    if label in outbound["error_labels"]:
        text = text_from_body(body)
        state = stream_state()
        operation = {"operation": "message.edit", "message_id": state["message_id"], "text": f"Error: {text}"} if state["message_id"] is not None else {"operation": "message.send", "text": f"Error: {text}"}
        request = operation_request(config, operation, binding, source_seq)
        return request, {"label": label, "error_delivery": True, "final": True}, cleared_stream(source_seq)
    if label in outbound["reply_labels"]:
        if isinstance(body, dict) and isinstance(body.get("operation"), str):
            request = operation_request(config, body, binding, source_seq)
            return request, {"label": label, "operation": request["operation"], "final": True}, cleared_stream(source_seq)
        text = outbound["reply_prefix"] + text_from_body(body)
        state = stream_state()
        if state["message_id"] is not None:
            request = operation_request(config, {"operation": "message.edit", "message_id": state["message_id"], "text": text}, binding, source_seq)
            return request, {"label": label, "operation": "message.edit", "final": True}, cleared_stream(source_seq)
        request = operation_request(config, {"operation": "message.send", "text": text}, binding, source_seq)
        return request, {"label": label, "operation": "message.send", "final": True}, cleared_stream(source_seq)
    if isinstance(body, dict) and isinstance(body.get("operation"), str):
        request = operation_request(config, body, binding, source_seq)
        return request, {"label": label, "operation": request["operation"], "final": True}, cleared_stream(source_seq)
    raise ValueError(f"whatsapp output label {label!r} is not declared")


def post_effect(link, operation, payload, key):
    try:
        return link.request("outbound", {"interface": INTERFACE_ID, "request": {"operation": operation, "payload": {**payload, "idempotency_key": key}}})
    except Exception as error:
        return {"ok": False, "error": sanitize_external_error(error)}


def finish_processing(config, link, source_seq, failed):
    last = completion_turn(source_seq)
    if not isinstance(last, dict) or not isinstance(last.get("message_id"), str):
        return []
    binding = config["binding"]
    effects = []
    if config["outbound"]["typing"]["enabled"]:
        effects.append({"operation": "typing.stop", "result": post_effect(link, "typing.stop", {"chat_id": binding["chat_id"]}, f"{INTERFACE_ID}:finish-typing:{source_seq}")})
    reactions = config["outbound"]["reactions"]
    if reactions["enabled"]:
        emoji = reactions["failure"] if failed else reactions["success"]
        effects.append({"operation": "reaction.set", "result": post_effect(link, "reaction.set", {"chat_id": binding["chat_id"], "message_id": last["message_id"], "emoji": emoji, "from_me": False, "participant_id": last.get("sender_id")}, f"{INTERFACE_ID}:finish-reaction:{source_seq}")})
    return effects


def complete_delivered_effect(config, link, source_seq, effect):
    state_updates = require_object(effect.get("state_updates"), f"whatsapp effect {source_seq}.state_updates")
    if state_updates:
        update_stream_state(**state_updates)
    result_base = require_object(effect.get("result_base"), f"whatsapp effect {source_seq}.result_base")
    delivery = require_object(effect.get("delivery"), f"whatsapp effect {source_seq}.delivery")
    post_effects = finish_processing(config, link, source_seq, False) if result_base.get("final") else []
    effect["result"] = {**result_base, "delivery": delivery, "post_effects": post_effects}
    effect["status"] = "succeeded"
    commit_effect(source_seq, effect)
    return effect


def evidence_succeeded_effect(link, source_seq, content_hash, run, effect):
    admitted = hand_delivery_result(source_seq, content_hash, run, require_object(effect.get("result"), f"whatsapp effect {source_seq}.result"))
    effect["status"] = "evidenced"
    effect["evidence_movement_seq"] = admitted
    commit_effect(source_seq, effect)
    request = effect.get("request")
    key = request.get("payload", {}).get("idempotency_key") if isinstance(request, dict) else None
    if isinstance(key, str):
        with suppress(Exception):
            link.request("outbound", {"interface": INTERFACE_ID, "request": {"operation": "effect.ack", "payload": {"idempotency_key": key}}})


def evidence_indeterminate_effect(config, link, source_seq, content_hash, effect, error):
    effect["status"] = "indeterminate"
    safe_error = sanitize_external_error(error)
    effect["error"] = safe_error
    commit_effect(source_seq, effect)
    state_updates = effect.get("state_updates")
    if isinstance(state_updates, dict) and state_updates.get("message_id") is None:
        update_stream_state(**state_updates)
    result_base = effect.get("result_base")
    finish = finish_processing(config, link, source_seq, True) if isinstance(result_base, dict) and result_base.get("final") else []
    admitted = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "error": safe_error, "post_effects": finish}, f"whatsapp-effect-indeterminate-{source_seq}")
    effect["status"] = "failure_evidenced"
    effect["evidence_movement_seq"] = admitted
    commit_effect(source_seq, effect)


def process_outbound(config, link, source_seq, content, run):
    binding = config["binding"]
    if content is None or run is None:
        hand_failure(source_seq, "poisoned_outbound", {"error": "movement has no artifact content or runtime run"}, f"whatsapp-outbound-poison-{source_seq}")
        return
    if run != binding["run"]:
        hand_failure(source_seq, "boundary_rejected", {"error": "movement run does not match private interface", "expected_run": binding["run"], "observed_run": run}, f"whatsapp-outbound-run-{source_seq}")
        return
    content_hash = content_hexdigest(content)
    effect = load_effect(source_seq)
    if effect is not None and (effect["run"] != run or effect["source_artifact_hash"] != content_hash):
        hand_failure(source_seq, "effect_journal_mismatch", {"source_artifact_hash": content_hash}, f"whatsapp-effect-mismatch-{source_seq}")
        return
    if effect is not None and effect["status"] in ("evidenced", "failure_evidenced"):
        return
    if effect is not None and effect["status"] == "succeeded":
        state_updates = effect.get("state_updates")
        if isinstance(state_updates, dict) and state_updates:
            update_stream_state(**state_updates)
        evidence_succeeded_effect(link, source_seq, content_hash, run, effect)
        return
    if effect is not None and effect["status"] == "delivered":
        effect = complete_delivered_effect(config, link, source_seq, effect)
        evidence_succeeded_effect(link, source_seq, content_hash, run, effect)
        return
    if effect is not None and effect["status"] == "started":
        try:
            delivery = link.request("outbound", {"interface": INTERFACE_ID, "request": require_object(effect.get("request"), f"whatsapp effect {source_seq}.request")})
            if not isinstance(delivery, dict):
                raise RuntimeError("whatsapp gateway returned a non-object recovery result")
            result_base = require_object(effect.get("result_base"), f"whatsapp effect {source_seq}.result_base")
            state_updates = require_object(effect.get("state_updates"), f"whatsapp effect {source_seq}.state_updates")
            if result_base.get("stream") and effect["request"]["operation"] == "message.send":
                message_id = delivery.get("message_id") or (delivery.get("message_ids") or [None])[-1]
                if message_id:
                    state_updates["message_id"] = message_id
            if result_base.get("stream"):
                state_updates["last_edit_ms"] = int(time.time() * 1000)
            effect["delivery"] = delivery
            effect["status"] = "delivered"
            effect["result_base"] = {**result_base, "recovered_from_bridge_journal": True}
            effect["state_updates"] = state_updates
            commit_effect(source_seq, effect)
        except Exception as error:
            evidence_indeterminate_effect(config, link, source_seq, content_hash, effect, error)
            return
        effect = complete_delivered_effect(config, link, source_seq, effect)
        evidence_succeeded_effect(link, source_seq, content_hash, run, effect)
        return
    if effect is not None and effect["status"] == "indeterminate":
        state_updates = effect.get("state_updates")
        if isinstance(state_updates, dict) and state_updates.get("message_id") is None:
            update_stream_state(**state_updates)
        result_base = effect.get("result_base")
        finish = finish_processing(config, link, source_seq, True) if isinstance(result_base, dict) and result_base.get("final") else []
        admitted = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "error": effect.get("error"), "post_effects": finish}, f"whatsapp-effect-indeterminate-{source_seq}")
        effect["status"] = "failure_evidenced"
        effect["evidence_movement_seq"] = admitted
        commit_effect(source_seq, effect)
        return
    try:
        label, payload = materialize_payload(
            content,
            config["driver_ipc"]["max_manifest_bytes"],
            config["driver_ipc"]["max_frame_bytes"],
        )
        request, result, state_updates = decode_output(config, label, payload, binding, source_seq)
    except Exception as error:
        update_stream_state(**cleared_stream(source_seq))
        finish = finish_processing(config, link, source_seq, True)
        hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "error": sanitize_external_error(error), "post_effects": finish}, f"whatsapp-outbound-boundary-{source_seq}")
        return
    effect = {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "artifact_label": label, "status": "started" if request is not None else "succeeded", "state_updates": state_updates}
    if request is None:
        if state_updates:
            update_stream_state(**state_updates)
        effect["result"] = result
        commit_effect(source_seq, effect)
        evidence_succeeded_effect(link, source_seq, content_hash, run, effect)
        return
    effect["request"] = request
    effect["result_base"] = result
    commit_effect(source_seq, effect)
    try:
        delivery = link.request("outbound", {"interface": INTERFACE_ID, "request": request})
        if not isinstance(delivery, dict):
            raise RuntimeError("whatsapp gateway returned a non-object delivery result")
        if result.get("stream") and request["operation"] == "message.send":
            message_id = delivery.get("message_id") or (delivery.get("message_ids") or [None])[-1]
            if message_id:
                state_updates["message_id"] = message_id
        if result.get("stream"):
            state_updates["last_edit_ms"] = int(time.time() * 1000)
        effect["delivery"] = delivery
        effect["state_updates"] = state_updates
        effect["status"] = "delivered"
        commit_effect(source_seq, effect)
    except Exception as error:
        evidence_indeterminate_effect(config, link, source_seq, content_hash, effect, error)
        return
    effect = complete_delivered_effect(config, link, source_seq, effect)
    evidence_succeeded_effect(link, source_seq, content_hash, run, effect)


def run_binding(config):
    link = BindingLink(config, lambda payload, current_link: process_arrival(config, current_link, payload))
    next_seq = load_cursor()
    binding = config["binding"]
    sys.stderr.write(f"whatsapp binding up: interface={INTERFACE_ID} run={binding['run']} ledger_next_seq={next_seq}\n")
    sys.stderr.flush()
    signal_ready()
    while True:
        for source_seq, content, run in read_own_outbound(next_seq, max_row_bytes=config["ledger"]["max_row_bytes"]):
            process_outbound(config, link, source_seq, content, run)
            next_seq = source_seq + 1
            commit_cursor(next_seq)
        time.sleep(0.05)


def run_gateway(config):
    driver = GatewayDriver(config)
    signal_ready()

    def stop(_signum, _frame):
        driver.stopping.set()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    try:
        driver.run()
    except KeyboardInterrupt:
        driver.stopping.set()
        raise
    except Exception as error:
        path = os.path.join(VAR_DIR, "gateway-reconnect.json")
        state = read_json(path, {"schema_version": 1, "backoff_ms": config["bridge"]["reconnect_backoff_ms"]})
        state = require_keys(state, {"schema_version", "backoff_ms"}, set(), "whatsapp gateway reconnect state")
        if state["schema_version"] != 1:
            raise RuntimeError("whatsapp gateway reconnect state schema_version is invalid") from error
        backoff_ms = require_int(state["backoff_ms"], "whatsapp gateway reconnect backoff_ms", 1)
        next_backoff = min(backoff_ms * 2, config["bridge"]["reconnect_backoff_cap_ms"])
        atomic_write_json(path, {"schema_version": 1, "backoff_ms": next_backoff}, indent=2)
        sys.stderr.write(f"whatsapp gateway exited: {sanitize_external_error(error)}; restarting in {backoff_ms / 1000.0:.3f}s\n")
        sys.stderr.flush()
        time.sleep(backoff_ms / 1000.0)
        os.execv(sys.executable, [sys.executable] + sys.argv)


def main():
    config = load_config()
    if "--validate-config" in sys.argv:
        print(json.dumps({"ok": True, "mode": config["mode"], "interface": INTERFACE_ID}))
        return
    start_runtime_verdict_reader()
    if config["mode"] == "binding":
        run_binding(config)
    else:
        run_gateway(config)


if __name__ == "__main__":
    main()
