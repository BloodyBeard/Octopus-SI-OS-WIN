#!/usr/bin/env python3
import base64
import hashlib
import hmac
import json
import os
import re
import select
import secrets
import socket
import sqlite3
import stat
import struct
import sys
import threading
import time
import urllib.parse

from octopus_content_hash import octopus_content_hash

from slack_contract import CHANNEL_RE
from slack_contract import TEAM_RE
from slack_contract import TS_RE
from slack_contract import USER_RE
from slack_contract import authored_interface_binding
from slack_contract import envelope_key
from slack_contract import env_flag
from slack_contract import env_ids
from slack_contract import event_coordinates
from slack_contract import normalize_operation
from slack_contract import require_int
from slack_contract import require_interface_binding
from slack_contract import require_keys
from slack_contract import require_object
from slack_contract import require_string
from slack_contract import require_string_list
from slack_contract import rich_text
from slack_contract import safe_blocks
from slack_contract import slack_mrkdwn
from slack_contract import validate_config
from slack_wire import SlackApi
from slack_wire import SlackWireError
from slack_wire import WebSocket
from slack_wire import is_loopback_host
from slack_wire import validate_proxy_url


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
RAW_DIR = os.path.join(VAR_DIR, "raw-events")
PENDING_DIR = os.path.join(VAR_DIR, "pending-events")
PAYLOAD_DIR = os.path.join(VAR_DIR, "payloads")
MEDIA_DIR = os.path.join(VAR_DIR, "media")
EFFECT_DIR = os.path.join(VAR_DIR, "outbound-effects")
CALLBACK_DIR = os.path.join(VAR_DIR, "callback-handles")
STATE_PATH = os.path.join(VAR_DIR, "slack-state.json")
STREAM_STATE_PATH = os.path.join(VAR_DIR, "stream-state.json")
BINDINGS_PATH = os.path.join(VAR_DIR, "private-bindings.json")
TOPOLOGY_OWNERSHIP_PATH = os.path.join(VAR_DIR, "topology-ownership.json")
BINDING_EVENTS_PATH = os.path.join(VAR_DIR, "binding-events.json")
SECRETS_DIR = os.path.join(VAR_DIR, "private-secrets")
IDENTITY_SALT_PATH = os.path.join(SECRETS_DIR, "identity-salt.key")
PRIVATE_REGISTRATION_LIMIT = 32
PRIVATE_REGISTRATION_MAX_BYTES = 16 * 1024
TOPOLOGY_MAX_BYTES = 16 * 1024 * 1024
TOPOLOGY_OWNERSHIP_MAX_PATHS = 100_000

os.umask(0o077)


class ArtifactDeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict
        super().__init__(f"runtime refused Slack artifact: {verdict!r}")


def ensure_private_directory(path):
    path = os.path.abspath(path)
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
        raise PermissionError(f"Slack private directory is not a real directory: {path}")
    if current.st_uid != os.getuid():
        raise PermissionError(f"Slack private directory has a different owner: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


for directory in (VAR_DIR, RAW_DIR, PENDING_DIR, PAYLOAD_DIR, MEDIA_DIR, EFFECT_DIR, CALLBACK_DIR, SECRETS_DIR):
    ensure_private_directory(directory)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_LEDGER_LOCK = threading.RLock()
_LEDGER_FALLBACK_OFFSET = 0
_LEDGER_FALLBACK_SIZE = 0
_LEDGER_FALLBACK_IDENTITY = None
_LEDGER_INDEX_WARNING = None
_AUTHORING_LOCK = threading.RLock()
_BINDING_EVENTS_LOCK = threading.RLock()
_CONFIG = None


def open_private_read(path):
    if not isinstance(path, str) or not os.path.isabs(path):
        raise PermissionError("Slack private file path must be absolute")
    path = os.path.abspath(path)
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError(f"Slack private path is not a regular file: {path}")
    if before.st_uid != os.getuid() or stat.S_IMODE(before.st_mode) != 0o600:
        raise PermissionError(f"Slack private path must be owner-only 0600: {path}")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        current = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(current.st_mode) or current.st_uid != os.getuid() or stat.S_IMODE(current.st_mode) != 0o600:
            raise PermissionError(f"Slack private path changed permissions: {path}")
        if stat.S_ISLNK(linked.st_mode) or (linked.st_dev, linked.st_ino) != (current.st_dev, current.st_ino) or (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"Slack private path changed while opening: {path}")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def sensitive_values(config=None):
    values = set()
    if isinstance(config, dict):
        transport = config.get("transport") if isinstance(config.get("transport"), dict) else {}
        for key in ("bot_token_env", "app_token_env", "proxy_env", "socket_url_env"):
            env_name = transport.get(key)
            raw = os.environ.get(env_name, "") if isinstance(env_name, str) else ""
            if raw:
                values.add(raw)
                values.update(part.strip() for part in raw.split(",") if part.strip())
    return {value for value in values if len(value) >= 4}


def sanitize_external_error(value, config=None):
    if isinstance(value, dict):
        return {sanitize_external_error(key, config): sanitize_external_error(item, config) for key, item in value.items()}
    if isinstance(value, list):
        return [sanitize_external_error(item, config) for item in value]
    if isinstance(value, tuple):
        return tuple(sanitize_external_error(item, config) for item in value)
    if isinstance(value, BaseException):
        value = str(value)
    if not isinstance(value, str):
        return value
    tokens = set(sensitive_values(config))
    for token in tuple(tokens):
        tokens.add(urllib.parse.quote(token, safe=""))
        tokens.add(urllib.parse.quote_plus(token, safe=""))
        tokens.add(base64.b64encode(token.encode("utf-8")).decode("ascii"))
    sanitized = value
    for token in sorted(tokens, key=len, reverse=True):
        sanitized = sanitized.replace(token, "[secret-redacted]")
    sanitized = re.sub(r"xox[baprs]-[0-9A-Za-z-]+", "[slack-token-redacted]", sanitized)
    sanitized = re.sub(r"xapp-[0-9A-Za-z-]+", "[slack-token-redacted]", sanitized)
    sanitized = re.sub(r"https://hooks\.slack(?:-gov)?\.com/services/[^\s\"']+", "[slack-callback-redacted]", sanitized)
    sanitized = re.sub(r"wss://[^\s\"']+", "[slack-socket-url-redacted]", sanitized)
    sanitized = re.sub(r"(?i)(authorization\s*[:=]\s*(?:bearer|basic)\s+)[^\s,;]+", r"\1[secret-redacted]", sanitized)
    sanitized = re.sub(r"(https?://)([^/@\s]+)@", r"\1[credentials-redacted]@", sanitized)
    return sanitized


def stderr(config, message):
    sys.stderr.write(sanitize_external_error(message, config) + "\n")
    sys.stderr.flush()


def load_config():
    config = json.loads(
        read_bounded_regular(CONFIG_PATH, TOPOLOGY_MAX_BYTES, "interface config")
    )
    validate_config(config, INTERFACE_ID)
    return config


def atomic_bytes(path, payload):
    parent = os.path.dirname(path)
    ensure_private_directory(parent)
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    descriptor = None
    try:
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = None
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        directory = os.open(parent, os.O_RDONLY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if descriptor is not None:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_json(path, value, indent=None):
    atomic_bytes(path, json.dumps(value, ensure_ascii=False, sort_keys=True, indent=indent).encode("utf-8"))


def _runtime_verdict_reader():
    global _VERDICT_INPUT_ERROR
    try:
        for line in sys.stdin:
            message = json.loads(line)
            if not isinstance(message, dict) or message.get("kind") != "delivery_verdict":
                raise RuntimeError(f"unexpected runtime input {message!r}")
            request_id = message.get("request_id")
            if isinstance(request_id, bool) or not isinstance(request_id, int):
                raise RuntimeError("runtime verdict has no integer request_id")
            with _VERDICT_CONDITION:
                _VERDICTS[request_id] = message.get("verdict") or {}
                _VERDICT_CONDITION.notify_all()
        raise RuntimeError("runtime verdict input closed")
    except Exception as error:
        with _VERDICT_CONDITION:
            _VERDICT_INPUT_ERROR = error
            _VERDICT_CONDITION.notify_all()


def start_runtime_verdict_reader():
    threading.Thread(target=_runtime_verdict_reader, daemon=True).start()


def signal_ready():
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "ready"}, separators=(",", ":")) + "\n")
        sys.stdout.flush()


def hand_artifact(manifest_path, evidence=None):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    with _STDOUT_LOCK:
        sys.stdout.write(json.dumps({"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": evidence or []}, separators=(",", ":")) + "\n")
        sys.stdout.flush()
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            _VERDICT_CONDITION.wait()
        if _VERDICT_INPUT_ERROR is not None:
            raise RuntimeError(f"runtime verdict input failed before Slack request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int) and not isinstance(admitted.get("seq"), bool):
        return admitted["seq"]
    raise ArtifactDeliveryRefused(verdict)


def materialize_artifact(label, payload, key):
    key = require_string(key, "Slack artifact key", maximum=512)
    if not re.fullmatch(r"[0-9A-Za-z._-]+", key):
        raise ValueError("Slack artifact key contains unsafe characters")
    payload_path = os.path.join(PAYLOAD_DIR, f"{key}.payload")
    manifest_path = os.path.join(PAYLOAD_DIR, f"{key}.artifact.json")
    atomic_bytes(payload_path, payload)
    atomic_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def hand_json(label, value, key):
    value = sanitize_external_error(value, _CONFIG)
    return hand_artifact(materialize_artifact(label, json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8"), key))


def materialized_artifact_id(manifest_path, config):
    manifest = require_keys(json.loads(read_bounded_regular(manifest_path, 1048576, "materialized artifact manifest")), {"label", "payload_path"}, {"payload_content_hash"}, "Slack materialized artifact manifest")
    label = require_string(manifest["label"], "Slack materialized artifact label")
    payload_path = require_string(manifest["payload_path"], "Slack materialized artifact payload_path")
    if not os.path.isabs(payload_path):
        raise ValueError("Slack materialized artifact payload_path must be absolute")
    payload = read_bounded_regular(payload_path, config["routing"]["ipc_max_frame_bytes"], "materialized artifact payload")
    payload_hash = octopus_content_hash(payload)
    declared = manifest.get("payload_content_hash")
    if declared is not None and declared != payload_hash:
        raise ValueError("Slack materialized artifact payload hash mismatched")
    pinned = {"label": label, "payload_path": payload_path, "payload_content_hash": payload_hash}
    pinned_bytes = json.dumps(pinned, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"{label}:{octopus_content_hash(pinned_bytes)}"


def find_admitted_arrival_seq(artifact_id, config):
    artifact_id = require_string(artifact_id, "Slack arrival artifact id")
    try:
        uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("3",):
                raise RuntimeError("Slack ledger index has unsupported schema")
            row = connection.execute("SELECT seq FROM movements WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' AND from_id = ? AND artifact_id = ? ORDER BY seq ASC LIMIT 1", (INTERFACE_ID, artifact_id)).fetchone()
        finally:
            connection.close()
        if row is not None:
            return require_int(row[0], "Slack admitted arrival seq", 0)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError):
        pass
    try:
        handle = open(MOVEMENTS_JSONL, "rb")
    except FileNotFoundError:
        return None
    maximum = config["ledger"]["max_row_bytes"]
    with handle:
        while True:
            line = handle.readline(maximum + 2)
            if not line:
                break
            if len(line) > maximum + 1:
                raise RuntimeError("Slack movement lookup row exceeds configured ceiling")
            if not line.endswith(b"\n"):
                break
            movement = require_object(json.loads(line), "Slack movement lookup row")
            if movement.get("kind") != "InterfaceArrival" or movement.get("from") != {"Interface": INTERFACE_ID}:
                continue
            artifact = movement.get("artifact")
            if isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                return require_int(movement.get("seq"), "Slack admitted arrival seq", 0)
    return None


def hand_json_idempotent(label, value, key, config):
    value = sanitize_external_error(value, config)
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    manifest_path = materialize_artifact(label, payload, key)
    artifact_id = materialized_artifact_id(manifest_path, config)
    evidence = [{"interface_id": INTERFACE_ID, "artifact_id": artifact_id, "label": "slack.arrival.idempotency", "bytes": list(hashlib.sha256(artifact_id.encode("utf-8")).digest())}]
    try:
        return hand_artifact(manifest_path, evidence)
    except ArtifactDeliveryRefused as error:
        refused = error.verdict.get("refused") if isinstance(error.verdict, dict) else None
        details = refused.get("detail") if isinstance(refused, dict) else None
        if isinstance(details, dict) and details.get("artifact_id") == artifact_id and details.get("message") == "arrival evidence already exists in the movement ledger":
            admitted = find_admitted_arrival_seq(artifact_id, config)
            if admitted is not None:
                return admitted
        raise


def hand_failure(source_seq, category, details, key):
    return hand_json(f"{INTERFACE_ID}.failure", {"source_movement_seq": source_seq, "failure_class": category, "details": sanitize_external_error(details, _CONFIG)}, key)


def load_state():
    try:
        with open(STATE_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return {"schema_version": 1, "bot_user_ids": {}, "mentioned_threads": [], "bot_message_threads": [], "processed": []}
    value = require_keys(value, {"schema_version", "bot_user_ids", "mentioned_threads", "bot_message_threads", "processed"}, set(), "Slack state")
    if value["schema_version"] != 1:
        raise ValueError("Slack state schema_version must be 1")
    if not isinstance(value["bot_user_ids"], dict):
        raise ValueError("Slack state bot_user_ids must be an object")
    for team_id, user_id in value["bot_user_ids"].items():
        if not TEAM_RE.fullmatch(team_id) or not isinstance(user_id, str) or not USER_RE.fullmatch(user_id):
            raise ValueError("Slack state bot_user_ids contains malformed identity")
    for key in ("mentioned_threads", "bot_message_threads", "processed"):
        if not isinstance(value[key], list) or any(not isinstance(item, str) for item in value[key]):
            raise ValueError(f"Slack state {key} must be a string array")
    return value


def update_state(config, mutator):
    with _STATE_LOCK:
        state = load_state()
        mutator(state)
        retention = config["intake"]["dedup_retention"]
        state["mentioned_threads"] = state["mentioned_threads"][-retention:]
        state["bot_message_threads"] = state["bot_message_threads"][-retention:]
        state["processed"] = state["processed"][-retention:]
        atomic_json(STATE_PATH, state, indent=2)
        return state


def event_user(envelope):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    if envelope.get("type") == "events_api":
        event = payload.get("event") if isinstance(payload.get("event"), dict) else {}
        assistant_thread = event.get("assistant_thread") if isinstance(event.get("assistant_thread"), dict) else {}
        if event.get("subtype") in {"message_changed", "message_deleted"}:
            message = event.get("message") if isinstance(event.get("message"), dict) else {}
            previous = event.get("previous_message") if isinstance(event.get("previous_message"), dict) else {}
            return message.get("user") or previous.get("user") or event.get("user")
        user = event.get("user") or event.get("user_id") or assistant_thread.get("user_id")
        if isinstance(user, dict):
            return user.get("id")
        return user
    if envelope.get("type") == "slash_commands":
        return payload.get("user_id")
    if envelope.get("type") == "interactive":
        user = payload.get("user") if isinstance(payload.get("user"), dict) else {}
        return user.get("id")
    return None


def configured_ids(config, list_key, env_key, pattern):
    return set(config["authz"][list_key]) | env_ids(config["authz"][env_key], pattern)


def authorized(config, envelope):
    team, channel, _, _ = event_coordinates(envelope)
    user = event_user(envelope)
    teams = configured_ids(config, "allowed_team_ids", "allowed_team_ids_env", TEAM_RE)
    channels = configured_ids(config, "allowed_channel_ids", "allowed_channel_ids_env", CHANNEL_RE)
    users = configured_ids(config, "allowed_user_ids", "allowed_user_ids_env", USER_RE)
    allow_all_override = env_flag(config["authz"]["allow_all_users_env"])
    allow_all = config["authz"]["allow_all_users"] if allow_all_override is None else allow_all_override
    if teams and team not in teams:
        return False, "team_not_allowed"
    if channels and channel is not None and channel not in channels:
        return False, "channel_not_allowed"
    if user is None:
        if config["authz"]["deny_unmatched"]:
            return False, "user_missing"
        return True, "allowed_without_user"
    if allow_all or user in users:
        return True, "allowed_user"
    return False, "user_not_allowed"


def event_body(envelope):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    if envelope.get("type") == "events_api":
        return payload.get("event") if isinstance(payload.get("event"), dict) else {}
    return payload


def message_shape(envelope):
    if envelope.get("type") != "events_api":
        return None
    event = event_body(envelope)
    if event.get("type") not in {"message", "app_mention"}:
        return None
    subtype = event.get("subtype")
    if subtype == "message_changed":
        return event.get("message") if isinstance(event.get("message"), dict) else event
    if subtype == "message_deleted":
        return event.get("previous_message") if isinstance(event.get("previous_message"), dict) else event
    return event


def should_admit(config, envelope, state):
    allowed, reason = authorized(config, envelope)
    if not allowed:
        return False, reason
    if envelope.get("type") == "events_api":
        event = event_body(envelope)
        if event.get("type") not in config["intake"]["event_types"]:
            return False, "event_type_not_declared"
    elif envelope.get("type") == "interactive":
        payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
        if payload.get("type") not in config["intake"]["interactive_types"]:
            return False, "interactive_type_not_declared"
    elif envelope.get("type") != "slash_commands":
        return False, "envelope_type_not_declared"
    message = message_shape(envelope)
    if message is None:
        return True, "authorized_event"
    subtype = event_body(envelope).get("subtype")
    if subtype == "message_changed" and not config["gating"]["accept_message_edits"]:
        return False, "message_edits_disabled"
    if subtype == "message_deleted" and not config["gating"]["accept_message_deletes"]:
        return False, "message_deletes_disabled"
    if (message.get("bot_id") or subtype == "bot_message") and not config["gating"]["accept_bot_events"]:
        return False, "bot_event"
    team, channel, thread, ts = event_coordinates(envelope)
    if isinstance(channel, str) and channel.startswith("D"):
        return (config["gating"]["respond_in_dms"], "direct_message")
    free = set(config["gating"]["free_response_channel_ids"]) | env_ids(config["gating"]["free_response_channel_ids_env"], CHANNEL_RE)
    if channel in free:
        return True, "free_response_channel"
    if not config["gating"]["respond_in_channels_on_mention_only"]:
        return True, "channel_response_enabled"
    bot = state.get("bot_user_ids", {}).get(team)
    text = str(message.get("text") or "")
    blocks_text = rich_text(message.get("blocks"))
    explicitly_mentioned = bool(bot and (f"<@{bot}>" in text or f"<@{bot}>" in blocks_text))
    if envelope.get("type") == "events_api" and event_body(envelope).get("type") == "app_mention":
        explicitly_mentioned = True
    if any(re.search(pattern, text, re.IGNORECASE) for pattern in config["gating"]["mention_patterns"]):
        explicitly_mentioned = True
    thread_key = f"{channel}:{thread or ts}" if channel and (thread or ts) else None
    if explicitly_mentioned:
        if config["gating"]["thread_sticky_after_mention"] and thread_key:
            def add_mentioned(current):
                if thread_key not in current["mentioned_threads"]:
                    current["mentioned_threads"].append(thread_key)
            update_state(config, add_mentioned)
        return True, "mentioned"
    if thread_key and thread_key in state["mentioned_threads"]:
        return True, "mentioned_thread"
    if thread_key and thread_key in state["bot_message_threads"]:
        return True, "bot_reply_thread"
    return False, "mention_required"


def callback_handle(payload, event_key):
    response_url = payload.get("response_url")
    trigger_id = payload.get("trigger_id")
    if not isinstance(response_url, str) and not isinstance(trigger_id, str):
        return None
    seed = require_string(event_key, "Slack callback event key") + "\0" + (response_url or "") + "\0" + (trigger_id or "")
    handle = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    atomic_json(os.path.join(CALLBACK_DIR, f"{handle}.json"), {"schema_version": 1, "response_url": response_url, "trigger_id": trigger_id, "created_at_unix_ms": int(time.time() * 1000)})
    return handle


def raw_event_path(key):
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", key)[:180]
    return os.path.join(RAW_DIR, f"{safe}.json")


def pending_event_path(key):
    safe = re.sub(r"[^A-Za-z0-9_.-]", "_", key)[:180]
    return os.path.join(PENDING_DIR, f"{safe}.json")


def persist_envelope(config, envelope):
    key = envelope_key(envelope)
    pending = pending_event_path(key)
    try:
        with open(pending, "r", encoding="utf-8") as handle:
            current = json.load(handle)
        require_object(current, "Slack pending event")
        return key, current
    except FileNotFoundError:
        pass
    if len(os.listdir(PENDING_DIR)) >= config["intake"]["max_pending_events"]:
        raise RuntimeError("Slack pending event ceiling reached")
    raw = json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    raw_path = raw_event_path(key)
    atomic_bytes(raw_path, raw)
    record = {
        "schema_version": 1,
        "key": key,
        "raw_path": raw_path,
        "raw_sha256": hashlib.sha256(raw).hexdigest(),
        "status": "pending",
        "attempts": 0,
        "created_at_unix_ms": int(time.time() * 1000),
    }
    atomic_json(pending, record, indent=2)
    return key, record


def media_rows(api, config, event, key, team_id=None, download=True):
    files = event.get("files") if isinstance(event.get("files"), list) else []
    rows = []
    for index, file_row in enumerate(files[: config["media"]["max_files_per_event"]]):
        if not isinstance(file_row, dict):
            continue
        row = {
            "id": file_row.get("id"),
            "name": file_row.get("name"),
            "title": file_row.get("title"),
            "mimetype": file_row.get("mimetype"),
            "filetype": file_row.get("filetype"),
            "size": file_row.get("size"),
            "mode": file_row.get("mode"),
            "subtype": file_row.get("subtype"),
            "permalink": file_row.get("permalink"),
        }
        url = file_row.get("url_private_download") or file_row.get("url_private")
        if not download and isinstance(url, str) and url:
            row["_source_url"] = url
        if download and config["media"]["download_enabled"] and isinstance(url, str) and url:
            try:
                content, content_type = api.download(url, config["media"]["allowed_download_hosts"], config["media"]["max_download_bytes"], team_id=team_id)
                digest = hashlib.sha256(content).hexdigest()
                filename = str(file_row.get("name") or file_row.get("id") or f"file-{index}")
                suffix = os.path.splitext(filename)[1][:20]
                path = os.path.join(MEDIA_DIR, f"{key}-{index}-{digest}{suffix}")
                atomic_bytes(path, content)
                row.update({"payload_path": path, "sha256": digest, "downloaded_bytes": len(content), "content_type": content_type})
                if len(content) <= config["media"]["text_preview_bytes"] and (str(content_type or "").startswith("text/") or suffix.lower() in {".txt", ".md", ".json", ".csv", ".log", ".py", ".rs", ".js", ".ts"}):
                    row["text_preview"] = content.decode("utf-8", "replace")
            except Exception as error:
                row["download_error"] = sanitize_external_error(error, config)
        rows.append(row)
    return rows


def normalized_event(api, config, envelope, record, download_media=True):
    payload = envelope.get("payload") if isinstance(envelope.get("payload"), dict) else {}
    event = event_body(envelope)
    team, channel, thread, ts = event_coordinates(envelope)
    message = message_shape(envelope) or {}
    if event.get("type") in {"file_shared", "file_created", "file_change"} and not message.get("files"):
        file_id = event.get("file_id") or event.get("file")
        if isinstance(file_id, str) and file_id:
            try:
                file_info = api.call("files.info", {"file": file_id}, safe_retry=True, team_id=team)
                if isinstance(file_info.get("file"), dict):
                    message = {**message, "files": [file_info["file"]]}
            except Exception as error:
                message = {**message, "file_lookup_error": sanitize_external_error(error, config)}
    handle = callback_handle(payload, record["key"]) if envelope.get("type") in {"interactive", "slash_commands"} else None
    normalized_payload = json.loads(json.dumps(payload))
    if isinstance(normalized_payload, dict):
        normalized_payload.pop("response_url", None)
        normalized_payload.pop("trigger_id", None)
        if isinstance(normalized_payload.get("actions"), list):
            for action in normalized_payload["actions"]:
                if isinstance(action, dict):
                    action.pop("url", None)
    text = message.get("text") or payload.get("text") or ""
    blocks = message.get("blocks") if isinstance(message.get("blocks"), list) else []
    thread_context = []
    if channel and thread and config["intake"]["thread_context_max_messages"]:
        try:
            context_response = api.call("conversations.replies", {"channel": channel, "ts": thread, "limit": min(100, config["intake"]["thread_context_max_messages"])}, safe_retry=True, team_id=team)
            messages = context_response.get("messages") if isinstance(context_response.get("messages"), list) else []
            for context_message in messages[-config["intake"]["thread_context_max_messages"] :]:
                if not isinstance(context_message, dict):
                    continue
                context_blocks = context_message.get("blocks") if isinstance(context_message.get("blocks"), list) else []
                thread_context.append({"ts": context_message.get("ts"), "user_id": context_message.get("user"), "bot_id": context_message.get("bot_id"), "text": context_message.get("text") or "", "rich_text": rich_text(context_blocks), "edited": context_message.get("edited")})
        except Exception as error:
            thread_context = [{"context_error": sanitize_external_error(error, config)}]
    return {
        "schema_version": 1,
        "event_key": record["key"],
        "envelope_type": envelope.get("type"),
        "event_type": event.get("type") if isinstance(event, dict) else None,
        "event_subtype": event.get("subtype") if isinstance(event, dict) else None,
        "team_id": team,
        "channel_id": channel,
        "thread_ts": thread,
        "message_ts": ts,
        "user_id": event_user(envelope),
        "text": text,
        "rich_text": rich_text(blocks),
        "blocks": blocks,
        "files": media_rows(api, config, message or event, record["key"], team_id=team, download=download_media),
        "thread_context": thread_context,
        "callback_handle": handle,
        "trigger_handle": handle if isinstance(payload.get("trigger_id"), str) else None,
        "payload": normalized_payload,
        "evidence": {"raw_event_path": record["raw_path"], "raw_event_sha256": record["raw_sha256"]},
    }


def read_frame(sock, maximum):
    header = bytearray()
    while len(header) < 4:
        chunk = sock.recv(4 - len(header))
        if not chunk:
            raise EOFError("Slack IPC closed before frame header")
        header.extend(chunk)
    size = struct.unpack("!I", bytes(header))[0]
    if size <= 0 or size > maximum:
        raise ValueError("Slack IPC frame size is invalid")
    body = bytearray()
    while len(body) < size:
        chunk = sock.recv(size - len(body))
        if not chunk:
            raise EOFError("Slack IPC closed before frame body")
        body.extend(chunk)
    value = json.loads(body)
    if not isinstance(value, dict):
        raise ValueError("Slack IPC frame must contain an object")
    return value


def write_frame(sock, value, maximum):
    body = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if not body or len(body) > maximum:
        raise ValueError("Slack IPC response exceeds frame ceiling")
    sock.sendall(struct.pack("!I", len(body)) + body)


def load_binding_registry(config):
    result = {}
    rows = []
    try:
        rows.extend({key: binding[key] for key in ("interface", "team_id", "channel_id", "thread_ts", "secret_sha256")} for binding in load_bindings()["bindings"].values())
    except FileNotFoundError:
        pass
    path = os.environ.get(config["routing"]["binding_registry_path_env"])
    if path:
        value = json.loads(
            read_bounded_regular(
                path,
                config["transport"]["max_response_bytes"],
                "external binding registry",
            )
        )
        value = require_keys(value, {"schema_version", "bindings"}, set(), "Slack binding registry")
        if value["schema_version"] != 1 or not isinstance(value["bindings"], list):
            raise ValueError("Slack binding registry is malformed")
        rows.extend(value["bindings"])
    for index, binding in enumerate(rows):
        binding = require_keys(binding, {"interface", "team_id", "channel_id", "thread_ts", "secret_sha256"}, set(), f"Slack binding registry.bindings[{index}]")
        interface = require_string(binding["interface"], f"Slack binding registry.bindings[{index}].interface")
        team = require_string(binding["team_id"], f"Slack binding registry.bindings[{index}].team_id")
        channel = require_string(binding["channel_id"], f"Slack binding registry.bindings[{index}].channel_id")
        thread = binding["thread_ts"]
        if not TEAM_RE.fullmatch(team) or not CHANNEL_RE.fullmatch(channel):
            raise ValueError("Slack binding registry contains malformed coordinates")
        if thread is not None and (not isinstance(thread, str) or not TS_RE.fullmatch(thread)):
            raise ValueError("Slack binding registry contains malformed thread_ts")
        digest = binding["secret_sha256"]
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("Slack binding registry contains malformed secret digest")
        coordinate = (team, channel, thread)
        row = {"interface": interface, "secret_sha256": digest}
        if coordinate in result and result[coordinate] != row:
            raise ValueError("Slack binding registry repeats coordinates with different declarations")
        result[coordinate] = row
    return result


def coordinate_key(team_id, channel_id, thread_ts):
    if not TEAM_RE.fullmatch(require_string(team_id, "Slack binding team_id")):
        raise ValueError("Slack binding team_id is malformed")
    if not CHANNEL_RE.fullmatch(require_string(channel_id, "Slack binding channel_id")):
        raise ValueError("Slack binding channel_id is malformed")
    if thread_ts is not None and (not isinstance(thread_ts, str) or not TS_RE.fullmatch(thread_ts)):
        raise ValueError("Slack binding thread_ts is malformed")
    return f"{team_id}\n{channel_id}\n{thread_ts or ''}"


def identity_salt():
    try:
        with open_private_read(IDENTITY_SALT_PATH) as handle:
            value = handle.read()
    except FileNotFoundError:
        value = secrets.token_bytes(32)
        atomic_bytes(IDENTITY_SALT_PATH, value)
    if len(value) != 32:
        raise RuntimeError("Slack identity salt is malformed")
    return value


def secret_path(opaque_id):
    if not isinstance(opaque_id, str) or not re.fullmatch(r"[0-9a-f]{32}", opaque_id):
        raise ValueError("Slack binding opaque id is malformed")
    root = os.path.realpath(SECRETS_DIR)
    path = os.path.abspath(os.path.join(root, f"{opaque_id}.key"))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("Slack binding secret escaped its private directory")
    return path


def ensure_binding_secret(opaque_id):
    path = secret_path(opaque_id)
    try:
        with open_private_read(path) as handle:
            value = handle.read().decode("ascii").strip()
    except FileNotFoundError:
        value = secrets.token_hex(32)
        atomic_bytes(path, (value + "\n").encode("ascii"))
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        raise RuntimeError("Slack binding secret is malformed")
    return path, value


def binding_bot_token_path(opaque_id):
    if not isinstance(opaque_id, str) or not re.fullmatch(r"[0-9a-f]{32}", opaque_id):
        raise ValueError("Slack binding opaque id is malformed")
    root = os.path.realpath(SECRETS_DIR)
    path = os.path.abspath(os.path.join(root, f"{opaque_id}.bot-token"))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("Slack binding bot token escaped its private directory")
    return path


def store_binding_bot_token(opaque_id, token):
    if not isinstance(token, str) or re.fullmatch(r"xoxb-[0-9A-Za-z-]{15,4096}", token) is None:
        raise ValueError("Slack binding bot token is malformed")
    path = binding_bot_token_path(opaque_id)
    encoded = (token + "\n").encode("ascii")
    try:
        with open_private_read(path) as handle:
            current = handle.read()
    except FileNotFoundError:
        current = None
    if current != encoded:
        atomic_bytes(path, encoded)
    return path


def load_binding_bot_token(path):
    with open_private_read(path) as handle:
        raw = handle.read().decode("ascii")
    token = raw[:-1] if raw.endswith("\n") else raw
    if raw != token + "\n" or re.fullmatch(r"xoxb-[0-9A-Za-z-]{15,4096}", token) is None:
        raise ValueError("Slack binding bot token file is malformed")
    return token


def load_bindings():
    try:
        value = json.loads(
            read_bounded_regular(BINDINGS_PATH, TOPOLOGY_MAX_BYTES, "private bindings")
        )
    except FileNotFoundError:
        return {"schema_version": 1, "bindings": {}}
    value = require_keys(value, {"schema_version", "bindings"}, set(), "Slack private bindings")
    if value["schema_version"] != 1 or not isinstance(value["bindings"], dict):
        raise ValueError("Slack private bindings are malformed")
    result = {"schema_version": 1, "bindings": {}}
    for key, binding in value["bindings"].items():
        binding = require_keys(binding, {"schema_version", "opaque_id", "team_id", "channel_id", "thread_ts", "interface", "provider_interface", "interface_authoring_interface", "worker_environment_interface", "file_write_tool", "worker_interface", "run", "ee", "gateway_host", "gateway_port", "secret_sha256", "status"}, set(), f"Slack private binding {key!r}")
        if binding["schema_version"] != 1 or coordinate_key(binding["team_id"], binding["channel_id"], binding["thread_ts"]) != key:
            raise ValueError(f"Slack private binding {key!r} identity drifted")
        if not re.fullmatch(r"[0-9a-f]{32}", binding["opaque_id"]):
            raise ValueError(f"Slack private binding {key!r} opaque id is malformed")
        registry_name(binding["worker_environment_interface"], f"private binding {key!r} worker environment interface")
        registry_name(binding["file_write_tool"], f"private binding {key!r} file write tool")
        if not is_loopback_host(binding["gateway_host"]):
            raise ValueError(f"Slack private binding {key!r} gateway must be loopback")
        require_int(binding["gateway_port"], f"Slack private binding {key!r}.gateway_port", 1, 65535)
        if not re.fullmatch(r"[0-9a-f]{64}", binding["secret_sha256"]):
            raise ValueError(f"Slack private binding {key!r} secret digest is malformed")
        if binding["status"] not in {"declared", "ready"}:
            raise ValueError(f"Slack private binding {key!r} status is invalid")
        result["bindings"][key] = binding
    return result


def ensure_binding_record(config, team_id, channel_id, thread_ts, gateway_host, gateway_port):
    if not is_loopback_host(gateway_host):
        raise ValueError("Slack private binding gateway must be loopback")
    gateway_port = require_int(gateway_port, "Slack private binding gateway_port", 1, 65535)
    key = coordinate_key(team_id, channel_id, thread_ts)
    with _AUTHORING_LOCK:
        state = load_bindings()
        existing = state["bindings"].get(key)
        if existing is not None:
            path, secret = ensure_binding_secret(existing["opaque_id"])
            if not hmac.compare_digest(existing["secret_sha256"], hashlib.sha256(secret.encode("ascii")).hexdigest()):
                raise RuntimeError("Slack private binding secret digest drifted")
            existing["gateway_host"] = gateway_host
            existing["gateway_port"] = gateway_port
            atomic_json(BINDINGS_PATH, state, indent=2)
            return existing
        opaque_id = hmac.new(identity_salt(), key.encode("utf-8"), hashlib.sha256).hexdigest()[:32]
        prefixes = config["private_run"]["id_prefixes"]
        path, secret = ensure_binding_secret(opaque_id)
        binding = {
            "schema_version": 1,
            "opaque_id": opaque_id,
            "team_id": team_id,
            "channel_id": channel_id,
            "thread_ts": thread_ts,
            "interface": prefixes["slack"] + opaque_id,
            "provider_interface": prefixes["provider"] + opaque_id,
            "interface_authoring_interface": prefixes["interface_authoring"] + opaque_id,
            "worker_environment_interface": prefixes["worker_environment"] + opaque_id,
            "file_write_tool": prefixes["tool"] + opaque_id,
            "worker_interface": prefixes["worker"] + opaque_id,
            "run": prefixes["run"] + opaque_id,
            "ee": prefixes["ee"] + opaque_id,
            "gateway_host": gateway_host,
            "gateway_port": gateway_port,
            "secret_sha256": hashlib.sha256(secret.encode("ascii")).hexdigest(),
            "status": "declared",
        }
        state["bindings"][key] = binding
        atomic_json(BINDINGS_PATH, state, indent=2)
        return binding


def mark_binding_ready(binding):
    key = coordinate_key(binding["team_id"], binding["channel_id"], binding["thread_ts"])
    with _AUTHORING_LOCK:
        state = load_bindings()
        current = state["bindings"].get(key)
        if current is None or current["interface"] != binding["interface"]:
            raise RuntimeError("Slack private binding disappeared before activation")
        current["status"] = "ready"
        atomic_json(BINDINGS_PATH, state, indent=2)
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


def rooted_path(root, *parts):
    root = os.path.realpath(root)
    path = os.path.realpath(os.path.join(root, *parts))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("Slack topology path escaped its granted root")
    return path


def json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def registry_name(value, label):
    value = require_string(value, label)
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,191}", value):
        raise ValueError(f"Slack {label} must be a registry-safe name")
    return value


def registry_json(root, owner, filename, label):
    owner = registry_name(owner, f"{label} owner")
    path = rooted_path(root, owner, filename)
    try:
        value = json.loads(read_bounded_regular(path, TOPOLOGY_MAX_BYTES, label))
    except json.JSONDecodeError as error:
        raise ValueError(f"Slack {label} is invalid JSON") from error
    return require_object(value, f"Slack {label}")


def worker_role_artifact_bindings(worker, policy=None):
    worker = registry_name(worker, "exact worker interface")
    if policy is None:
        policy = registry_json(
            root_slots()["registry_interface_root"],
            worker,
            "policy.json",
            f"exact worker {worker!r} policy",
        )
    policy = require_object(policy, "Slack exact worker policy")
    if policy.get("id") != worker:
        raise ValueError("Slack exact worker policy identity does not match its registry folder")
    return [
        {"id": artifact}
        for artifact in require_string_list(
            policy.get("role_artifacts"),
            "Slack exact worker policy.role_artifacts",
            True,
        )
    ]


def gateway_worker_environment_interface(config):
    return registry_name(config["private_run"]["interface_owners"]["worker_environment"], "gateway worker environment interface")


def authoring_target(config):
    if config["routing"]["binding_authoring_mode"] == "automatic":
        return gateway_worker_environment_interface(config)
    return registry_name(config["private_run"]["interface_owners"]["operator"], "operator interface")


def replace_exact_value(value, before, after):
    if isinstance(value, dict):
        return {key: replace_exact_value(item, before, after) for key, item in value.items()}
    if isinstance(value, list):
        return [replace_exact_value(item, before, after) for item in value]
    return after if value == before else value


def concrete_interface_descriptor(identifier, owner, interface_type, role, require_driver=True):
    identifier = registry_name(identifier, "concrete interface")
    owner = registry_name(owner, "interface owner")
    descriptor = registry_json(root_slots()["registry_interface_root"], owner, "interface.json", f"interface owner {owner!r} descriptor")
    if descriptor.get("id") != owner:
        raise ValueError("Slack interface owner descriptor identity does not match its registry folder")
    if "reference" in descriptor:
        raise ValueError("Slack interface owner descriptor cannot carry reference identity")
    if descriptor.get("interface_type") != interface_type or descriptor.get("role") != role:
        raise ValueError("Slack interface owner descriptor classification does not match the requested concrete interface")
    if descriptor.get("direction") != "bidirectional":
        raise ValueError("Slack interface owner descriptor must be bidirectional")
    sources = descriptor.get("source_names")
    if not isinstance(sources, list) or not sources:
        raise ValueError("Slack interface owner descriptor source_names must be a non-empty list")
    normalized_sources = []
    for index, source in enumerate(sources):
        source = require_string(source, f"Slack interface owner source_names[{index}]")
        if not re.fullmatch(r"[a-z0-9_-]+(?:\.[a-z0-9_-]+)*", source):
            raise ValueError("Slack interface owner source_names entry is malformed")
        normalized_sources.append(source)
    if len(normalized_sources) != len(set(normalized_sources)):
        raise ValueError("Slack interface owner source_names contains duplicates")
    if require_driver:
        require_string(descriptor.get("interface_driver_ref"), "Slack interface owner interface_driver_ref")
    for field in ("interface_driver_config", "interface_policy"):
        if field in descriptor and not isinstance(descriptor[field], dict):
            raise ValueError(f"Slack interface owner {field} must be a JSON object")
    descriptor = replace_exact_value(descriptor, owner, identifier)
    descriptor["id"] = identifier
    descriptor["source_names"] = normalized_sources
    return descriptor


def interface_owner_life(owner):
    owner = registry_name(owner, "interface owner")
    life = registry_json(root_slots()["registry_interface_root"], owner, "life.json", f"interface owner {owner!r} life")
    if not isinstance(life.get("processes"), list) or not life["processes"]:
        raise ValueError("Slack interface owner life must declare processes")
    return life


def optional_interface_owner_config(owner):
    owner = registry_name(owner, "interface owner")
    try:
        return registry_json(root_slots()["registry_interface_root"], owner, "config.json", f"interface owner {owner!r} config")
    except FileNotFoundError:
        return None


def concrete_tool_descriptor(identifier, owner, interface, interface_owner, worker_environment_kind):
    identifier = registry_name(identifier, "concrete tool")
    owner = registry_name(owner, "tool owner")
    interface = registry_name(interface, "concrete tool interface")
    interface_owner = registry_name(interface_owner, "tool owner interface")
    worker_environment_kind = registry_name(worker_environment_kind, "tool worker environment kind")
    descriptor = registry_json(root_slots()["registry_tool_root"], owner, "tool.json", f"tool owner {owner!r} descriptor")
    if descriptor.get("id") != owner or descriptor.get("interface") != interface_owner:
        raise ValueError("Slack tool owner descriptor identity drifted")
    supported = descriptor.get("supported_worker_environment_kinds")
    if not isinstance(supported, list) or worker_environment_kind not in supported:
        raise ValueError("Slack tool owner does not support the configured worker environment kind")
    if not isinstance(descriptor.get("process"), dict) or not isinstance(descriptor.get("args_schema"), dict):
        raise ValueError("Slack tool owner descriptor is incomplete")
    if descriptor.get("is_world_effect") is not True:
        raise ValueError("Slack file-write tool owner must be a world-effect tool")
    descriptor = replace_exact_value(descriptor, interface_owner, interface)
    descriptor = replace_exact_value(descriptor, owner, identifier)
    descriptor["id"] = identifier
    descriptor["interface"] = interface
    return descriptor


def provider_descriptor(identifier, owner, private_config):
    descriptor = concrete_interface_descriptor(identifier, owner, "world", "provider")
    driver_config = descriptor.get("interface_driver_config", {})
    if not isinstance(driver_config, dict):
        raise ValueError("Slack provider owner interface_driver_config must be an object")
    metadata = driver_config.get("private_config", {})
    if not isinstance(metadata, dict):
        raise ValueError("Slack provider owner private_config must be an object")
    metadata.pop("run_authoring", None)
    metadata.update(private_config)
    driver_config["private_config"] = metadata
    descriptor["interface_driver_config"] = driver_config
    return descriptor


def worker_policy(identifier, owner, interface_replacements, tool_replacements, required_interfaces, required_tools, required_wek, role_artifacts):
    identifier = registry_name(identifier, "concrete worker interface")
    owner = registry_name(owner, "worker interface owner")
    policy = registry_json(root_slots()["registry_interface_root"], owner, "policy.json", f"worker interface owner {owner!r} policy")
    if policy.get("id") != owner:
        raise ValueError("Slack worker policy identity does not match its registry folder")
    ceiling = require_object(policy.get("ceiling"), "Slack worker policy ceiling")
    interfaces = ceiling.get("interfaces")
    if not isinstance(interfaces, list) or any(not isinstance(value, str) or not value for value in interfaces):
        raise ValueError("Slack worker policy interfaces must be a string list")
    mapped = []
    for value in interfaces:
        value = interface_replacements.get(value, value)
        if value not in mapped:
            mapped.append(value)
    for value in required_interfaces:
        if value not in mapped:
            mapped.append(value)
    ceiling["interfaces"] = mapped
    tools = ceiling.get("tools")
    if not isinstance(tools, list) or any(not isinstance(value, str) or not value for value in tools):
        raise ValueError("Slack worker policy tools must be a string list")
    mapped_tools = []
    for value in tools:
        value = tool_replacements.get(value, value)
        if value not in mapped_tools:
            mapped_tools.append(value)
    for value in required_tools:
        if value not in mapped_tools:
            mapped_tools.append(value)
    ceiling["tools"] = mapped_tools
    wek = ceiling.get("wek")
    if not isinstance(wek, list) or any(not isinstance(value, str) or not value for value in wek):
        raise ValueError("Slack worker policy wek must be a string list")
    for value in required_wek:
        if value not in wek:
            wek.append(value)
    ceiling["wek"] = list(dict.fromkeys(wek))
    policy["id"] = identifier
    policy["ceiling"] = ceiling
    policy["role_artifacts"] = list(role_artifacts)
    return policy


def concrete_ee_template(identifier, owner, interfaces):
    identifier = registry_name(identifier, "concrete EE template")
    owner = registry_name(owner, "EE template owner")
    template = registry_json(root_slots()["registry_ee_template_root"], owner, "ee_template.json", f"EE template owner {owner!r}")
    if template.get("id") != owner:
        raise ValueError("Slack EE template owner identity does not match its registry folder")
    if "reference" in template:
        raise ValueError("Slack EE template owner cannot carry reference identity")
    template["id"] = identifier
    template["allowed_interfaces"] = list(dict.fromkeys(interfaces))
    return template


def child_life_json(config, secret_file, bot_token_file):
    transport = config["transport"]
    inherited = ["PATH", transport["proxy_env"], transport["api_base_env"]]
    runtime_source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME")
    if not isinstance(runtime_source, str) or not os.path.isabs(runtime_source):
        raise RuntimeError("Slack child topology requires the package-owned content-hash runtime")
    runtime_source = os.path.realpath(runtime_source)
    return {
        "processes": [{
            "bin": "python3",
            "max_in_flight_tool_processes": 16,
            "io_limits": {"max_control_frame_bytes": 1048576, "max_stderr_bytes": 262144, "max_manifest_bytes": 1048576, "max_payload_bytes": 268435456},
            "args": [
                {"repository_file": "registry/interface/world_slack/octopus_python_boot.py"},
                {"repository_file": "registry/interface/world_slack/slack_life.py"},
            ],
            "required_files": ["registry/interface/world_slack/octopus_python_boot.py", "registry/interface/world_slack/slack_life.py", "registry/interface/world_slack/slack_contract.py", "registry/interface/world_slack/slack_wire.py", "registry/interface/world_slack/octopus_content_hash.py", "registry/interface/world_slack/requirements-content-hash.lock"],
            "env": {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source},
            "inherit_env": list(dict.fromkeys(inherited)),
            "required_env": [],
            "authority": {
                "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
                "read_paths": [secret_file, bot_token_file, runtime_source], "write_paths": [], "executable_paths": [runtime_source],
                "executable_commands": ["python3"], "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
                "allow_network": True, "allow_local_ipc": True,
            },
            "coordinate_root_slots": [],
            "read_handles": ["ledger", "blobs", "ledger_index"],
        }]
    }


def binding_topology(config, binding):
    private = config["private_run"]
    owners = private["interface_owners"]
    interface = binding["interface"]
    provider = binding["provider_interface"]
    authoring_source = binding["interface_authoring_interface"]
    authoring = registry_name(binding["worker_environment_interface"], "private worker environment interface")
    file_write_tool = registry_name(binding["file_write_tool"], "private file write tool")
    worker = binding["worker_interface"]
    run = binding["run"]
    ee = binding["ee"]
    ee_template = private["id_prefixes"]["ee_template"] + binding["opaque_id"]
    prefix = f"cp_{binding['opaque_id'][:12]}"
    exact_interfaces = [interface, provider, authoring_source, authoring, worker]
    child_config = json.loads(json.dumps(config))
    child_config["mode"] = "binding"
    child_config["gateway_run_template"] = None
    child_config["private_run"]["file_write_tool"] = file_write_tool
    secret_file, _ = ensure_binding_secret(binding["opaque_id"])
    bot_token_file = binding_bot_token_path(binding["opaque_id"])
    load_binding_bot_token(bot_token_file)
    child_config["binding"] = {"interface": interface, "team_id": binding["team_id"], "channel_id": binding["channel_id"], "thread_ts": binding["thread_ts"], "run": run, "gateway_host": binding["gateway_host"], "gateway_port": binding["gateway_port"], "secret_file": secret_file, "bot_token_file": bot_token_file}
    provider_private_config = {"model": private["model"], "reasoning_effort": private["reasoning_effort"], "context_output_labels": private["provider_context_output_labels"], "run_authoring_profile": {"file_write_tool": file_write_tool, "file_write_label": private["file_write_label"], "file_write_result_label": private["file_write_result_label"], "result_timeout_ms": private["result_timeout_ms"]}}
    provider_definition = provider_descriptor(provider, owners["provider"], provider_private_config)
    interface_replacements = {owners["slack"]: interface, owners["provider"]: provider, owners["interface_authoring"]: authoring_source, owners["worker_environment"]: authoring, owners["worker"]: worker}
    tool_replacements = {private["file_write_tool"]: file_write_tool}
    worker_definition = concrete_interface_descriptor(worker, owners["worker"], "system", "worker", require_driver=False)
    worker_ceiling = worker_policy(worker, owners["worker"], interface_replacements, tool_replacements, exact_interfaces, [file_write_tool], [private["worker_environment_kind"]], private["role_artifacts"])
    ee_template_definition = concrete_ee_template(ee_template, private["ee_template_owner"], exact_interfaces)
    mandatory_roles = worker_role_artifact_bindings(worker, worker_ceiling)
    context = list(mandatory_roles)
    run_record = {
        "id": run, "ee": ee, "worker": worker, "trace": f"trace:{run}", "status": "running",
        "interface_state": {provider: {"context_generation": 0, "authoring_generation": 0, "frontier_checkpoint_ids": [f"{prefix}_slack_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"]}},
        "checkpoints": [
            {"id": f"{prefix}_slack_input", "run": run, "trace": f"trace:{run}:{prefix}_slack_input", "epoch": 0, "interface_binding": authored_interface_binding(interface), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": context, "required_verified_artifacts": []},
            {"id": f"{prefix}_slack_output", "run": run, "trace": f"trace:{run}:{prefix}_slack_output", "epoch": 0, "interface_binding": authored_interface_binding(provider), "outbound_interface_bindings": [authored_interface_binding(interface)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_call", "run": run, "trace": f"trace:{run}:{prefix}_authoring_call", "epoch": 0, "interface_binding": authored_interface_binding(authoring_source), "outbound_interface_bindings": [authored_interface_binding(authoring)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_result", "run": run, "trace": f"trace:{run}:{prefix}_authoring_result", "epoch": 0, "interface_binding": authored_interface_binding(authoring), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        ],
        "active_checkpoints": [f"{prefix}_slack_input", f"{prefix}_slack_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"],
    }
    activation = {"schema_version": 1, "interface": authoring_source, "activations": [{"activation_driver_ref": "app_server_transport", "config": {"bind_interface_socket_when_env_unset": True}}]}
    ee_instance = {"ee_id": ee, "template_name": ee_template, "parent_ee_id": private["parent_ee"], "goal": f"Serve the Slack boundary bound to interface {interface}"}
    roots = root_slots()
    interface_root = roots["registry_interface_root"]
    topology = {
        rooted_path(interface_root, interface, "config.json"): json_text(child_config),
        rooted_path(interface_root, interface, "life.json"): json_text(child_life_json(config, secret_file, bot_token_file)),
        rooted_path(interface_root, interface, "interface.json"): json_text(concrete_interface_descriptor(interface, owners["slack"], "world", "service")),
        rooted_path(interface_root, provider, "life.json"): json_text(interface_owner_life(owners["provider"])),
    }
    provider_config = optional_interface_owner_config(owners["provider"])
    if provider_config is not None:
        topology[rooted_path(interface_root, provider, "config.json")] = json_text(replace_exact_value(provider_config, owners["provider"], provider))
    topology[rooted_path(interface_root, provider, "interface.json")] = json_text(provider_definition)
    topology.update({
        rooted_path(interface_root, authoring_source, "life.json"): json_text(interface_owner_life(owners["interface_authoring"])),
        rooted_path(interface_root, authoring_source, "activation.json"): json_text(activation),
    })
    authoring_config = optional_interface_owner_config(owners["interface_authoring"])
    if authoring_config is not None:
        topology[rooted_path(interface_root, authoring_source, "config.json")] = json_text(replace_exact_value(authoring_config, owners["interface_authoring"], authoring_source))
    topology.update({
        rooted_path(interface_root, authoring_source, "interface.json"): json_text(concrete_interface_descriptor(authoring_source, owners["interface_authoring"], "system", "system")),
        rooted_path(interface_root, authoring, "interface.json"): json_text(concrete_interface_descriptor(authoring, owners["worker_environment"], "system", "system")),
        rooted_path(interface_root, authoring, "life.json"): json_text(interface_owner_life(owners["worker_environment"])),
        rooted_path(roots["registry_tool_root"], file_write_tool, "tool.json"): json_text(concrete_tool_descriptor(file_write_tool, private["file_write_tool"], authoring, owners["worker_environment"], private["worker_environment_kind"])),
        rooted_path(interface_root, worker, "policy.json"): json_text(worker_ceiling),
        rooted_path(interface_root, worker, "interface.json"): json_text(worker_definition),
        rooted_path(roots["registry_ee_template_root"], ee_template, "ee_template.json"): json_text(ee_template_definition),
        rooted_path(roots["ee_instances_root"], f"{ee}.json"): json_text(ee_instance),
        rooted_path(roots["run_records_root"], f"{run}.json"): json_text(run_record),
    })
    return topology


def read_topology_json(path, label):
    raw = read_bounded_regular(path, TOPOLOGY_MAX_BYTES, label)
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"Slack {label} is invalid JSON") from error
    return require_object(value, f"Slack {label}"), raw


def union_string_field(value, field, additions, label):
    current = value.get(field)
    if not isinstance(current, list) or any(not isinstance(item, str) or not item for item in current):
        raise ValueError(f"Slack {label}.{field} must be a string list")
    if len(current) != len(set(current)):
        raise ValueError(f"Slack {label}.{field} contains duplicates")
    merged = list(current)
    for item in additions:
        if item not in merged:
            merged.append(item)
    changed = merged != current
    value[field] = merged
    return changed


def private_state_writes(config, binding, topology):
    private = config["private_run"]
    roots = root_slots()
    ee_template = private["id_prefixes"]["ee_template"] + binding["opaque_id"]
    writes = []
    ee_path = rooted_path(roots["ee_instances_root"], f"{binding['ee']}.json")
    desired_ee = topology[ee_path]
    try:
        current_ee, current_ee_raw = read_topology_json(ee_path, "private EE instance")
    except FileNotFoundError:
        writes.append({"path": ee_path, "content": desired_ee, "expected_before_hash": None})
    else:
        if current_ee.get("ee_id") != binding["ee"] or current_ee.get("parent_ee_id") != private["parent_ee"]:
            raise RuntimeError("Slack private EE instance identity drifted")
        current_template = current_ee.get("template_name")
        if current_template not in {private["ee_template_owner"], ee_template}:
            raise RuntimeError("Slack private EE instance template drifted")
        if current_template != ee_template:
            current_ee["template_name"] = ee_template
            writes.append({"path": ee_path, "content": json_text(current_ee), "expected_before_hash": octopus_content_hash(current_ee_raw)})
    run_path = rooted_path(roots["run_records_root"], f"{binding['run']}.json")
    desired_run = topology[run_path]
    try:
        current_run, _ = read_topology_json(run_path, "private run record")
    except FileNotFoundError:
        writes.append({"path": run_path, "content": desired_run, "expected_before_hash": None})
    else:
        if current_run.get("id") != binding["run"] or current_run.get("ee") != binding["ee"] or current_run.get("worker") != binding["worker_interface"]:
            raise RuntimeError("Slack private run identity drifted")
        exact_interfaces = {binding["interface"], binding["provider_interface"], binding["interface_authoring_interface"], binding["worker_environment_interface"], binding["worker_interface"]}
        checkpoints = current_run.get("checkpoints")
        if not isinstance(checkpoints, list):
            raise RuntimeError("Slack private run checkpoint topology is malformed")
        for index, checkpoint in enumerate(checkpoints):
            if not isinstance(checkpoint, dict) or require_interface_binding(checkpoint.get("interface_binding"), f"Slack private run checkpoints[{index}].interface_binding") not in exact_interfaces:
                raise RuntimeError("Slack private run contains a stale or foreign interface binding")
            outbound = checkpoint.get("outbound_interface_bindings")
            if not isinstance(outbound, list) or any(
                require_interface_binding(binding, f"Slack private run checkpoints[{index}].outbound_interface_bindings[{outbound_index}]") not in exact_interfaces
                for outbound_index, binding in enumerate(outbound)
            ):
                raise RuntimeError("Slack private run contains a stale or foreign outbound interface binding")
    return writes


def shared_policy_writes(config, binding):
    private = config["private_run"]
    roots = root_slots()
    ee_template = private["id_prefixes"]["ee_template"] + binding["opaque_id"]
    exact_interfaces = [binding["interface"], binding["provider_interface"], binding["interface_authoring_interface"], binding["worker_environment_interface"], binding["worker_interface"]]
    writes = []
    parent_instance_path = rooted_path(roots["ee_instances_root"], f"{private['parent_ee']}.json")
    parent_instance, _ = read_topology_json(parent_instance_path, "parent EE instance")
    if parent_instance.get("ee_id") != private["parent_ee"]:
        raise ValueError("Slack parent EE instance identity drifted")
    parent_template_id = registry_name(parent_instance.get("template_name"), "parent EE template")
    parent_template_path = rooted_path(roots["registry_ee_template_root"], parent_template_id, "ee_template.json")
    parent_template, parent_template_raw = read_topology_json(parent_template_path, "parent EE template")
    if parent_template.get("id") != parent_template_id:
        raise ValueError("Slack parent EE template identity drifted")
    parent_changed = union_string_field(parent_template, "allowed_interfaces", exact_interfaces, "parent EE template")
    parent_changed = union_string_field(parent_template, "allowed_child_ee_templates", [ee_template], "parent EE template") or parent_changed
    if parent_changed:
        writes.append({"path": parent_template_path, "content": json_text(parent_template), "expected_before_hash": octopus_content_hash(parent_template_raw)})
    return writes


def wait_for_shared_policy_admissions(config, binding, attempted_hashes):
    deadline = time.monotonic() + config["private_run"]["result_timeout_ms"] / 1000.0
    while True:
        pending = shared_policy_writes(config, binding)
        if not pending:
            return True
        if any(attempted_hashes.get(write["path"]) != write["expected_before_hash"] for write in pending):
            return False
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return False
        time.sleep(min(0.05, remaining))


def ensure_gateway_run(config):
    template = require_object(config["gateway_run_template"], "Slack gateway_run_template")
    run_id = require_string(template.get("id"), "Slack gateway_run_template.id")
    path = rooted_path(root_slots()["run_records_root"], f"{run_id}.json")
    try:
        record = require_object(
            json.loads(
                read_bounded_regular(
                    path,
                    config["transport"]["max_response_bytes"],
                    "gateway run",
                )
            ),
            "Slack gateway run",
        )
    except FileNotFoundError as error:
        raise RuntimeError(f"Slack gateway run {path!r} is not authored; provision gateway_run_template before enabling the interface") from error
    if record.get("id") != run_id or record.get("ee") != template.get("ee") or record.get("worker") != template.get("worker"):
        raise RuntimeError("Slack gateway run identity conflicts with gateway_run_template")
    active = record.get("active_checkpoints") if isinstance(record.get("active_checkpoints"), list) else []
    checkpoints = record.get("checkpoints") if isinstance(record.get("checkpoints"), list) else []
    target = authoring_target(config)
    inbound = any(
        isinstance(row, dict)
        and row.get("id") in active
        and require_interface_binding(row.get("interface_binding"), f"Slack gateway checkpoint {row.get('id')}.interface_binding") == INTERFACE_ID
        and target in [
            require_interface_binding(binding, f"Slack gateway checkpoint {row.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(row.get("outbound_interface_bindings", []))
        ]
        for row in checkpoints
    )
    outbound = any(
        isinstance(row, dict)
        and row.get("id") in active
        and require_interface_binding(row.get("interface_binding"), f"Slack gateway checkpoint {row.get('id')}.interface_binding") == target
        and INTERFACE_ID in [
            require_interface_binding(binding, f"Slack gateway checkpoint {row.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(row.get("outbound_interface_bindings", []))
        ]
        for row in checkpoints
    )
    if not inbound or not outbound:
        raise RuntimeError("Slack gateway run is not armed for the configured binding authoring mode")
    return path


def gateway_rearm_write(config, opaque_id):
    path = ensure_gateway_run(config)
    current = read_bounded_regular(
        path,
        config["transport"]["max_response_bytes"],
        "gateway run",
    )
    record = require_object(json.loads(current), "Slack gateway run")
    run_id = config["gateway_run_template"]["id"]
    if record.get("id") != run_id or record.get("worker") != config["gateway_run_template"]["worker"]:
        raise RuntimeError("Slack gateway run identity drifted")
    mandatory_roles = worker_role_artifact_bindings(record["worker"])
    interface_state = require_object(record.setdefault("interface_state", {}), "Slack gateway interface_state")
    state = require_object(interface_state.setdefault(INTERFACE_ID, {}), "Slack gateway interface state")
    generation = require_int(state.get("authoring_generation", 0), "Slack gateway authoring_generation", 0) + 1
    state["authoring_generation"] = generation
    suffix = f"{opaque_id[:12]}_{generation}"
    call = f"cp_slack_gateway_authoring_call_{suffix}"
    result = f"cp_slack_gateway_authoring_result_{suffix}"
    target = authoring_target(config)
    record["checkpoints"] = [
        {"id": call, "run": run_id, "trace": f"trace:{run_id}:{call}", "epoch": 0, "interface_binding": authored_interface_binding(INTERFACE_ID), "outbound_interface_bindings": [authored_interface_binding(target)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        {"id": result, "run": run_id, "trace": f"trace:{run_id}:{result}", "epoch": 0, "interface_binding": authored_interface_binding(target), "outbound_interface_bindings": [authored_interface_binding(INTERFACE_ID)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
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
                config["transport"]["max_response_bytes"],
                "topology document",
            )
        except FileNotFoundError:
            if path in ownership["paths"]:
                raise RuntimeError(f"Slack package-authored topology target {path!r} disappeared")
            writes.append({"path": path, "content": content, "expected_before_hash": None})
            continue
        if existing == desired:
            continue
        observed = octopus_content_hash(existing)
        if ownership["paths"].get(path) != observed:
            raise RuntimeError(f"Slack private topology target {path!r} drifted from its last package-authored bytes")
        writes.append({"path": path, "content": content, "expected_before_hash": observed})
    return writes


def load_topology_ownership():
    try:
        state = json.loads(read_bounded_regular(TOPOLOGY_OWNERSHIP_PATH, TOPOLOGY_MAX_BYTES, "Slack topology ownership"))
    except FileNotFoundError:
        return {"schema_version": 1, "paths": {}}
    state = require_keys(state, {"schema_version", "paths"}, set(), "Slack topology ownership")
    if state["schema_version"] != 1:
        raise ValueError("Slack topology ownership schema_version must be 1")
    paths = require_object(state["paths"], "Slack topology ownership.paths")
    if len(paths) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("Slack topology ownership has too many paths")
    for path, digest in paths.items():
        require_string(path, "Slack topology ownership path")
        if not os.path.isabs(path):
            raise ValueError("Slack topology ownership path must be absolute")
        if not isinstance(digest, str) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("Slack topology ownership digest is malformed")
    return state


def commit_topology_ownership(config, topology):
    ownership = load_topology_ownership()
    for path, content in topology.items():
        desired = content.encode("utf-8")
        existing = read_bounded_regular(
            path,
            config["transport"]["max_response_bytes"],
            "topology document",
        )
        if existing != desired:
            raise RuntimeError(f"Slack package-authored topology target {path!r} did not materialize exactly")
        ownership["paths"][path] = octopus_content_hash(existing)
    if len(ownership["paths"]) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("Slack topology ownership has too many paths")
    atomic_json(TOPOLOGY_OWNERSHIP_PATH, ownership, indent=2)


def package_owned_topology(topology):
    roots = root_slots()
    owned_roots = [roots["registry_interface_root"], roots["registry_ee_template_root"], roots["registry_tool_root"]]
    return {
        path: content
        for path, content in topology.items()
        if any(os.path.commonpath((os.path.realpath(path), root)) == root and os.path.realpath(path) != root for root in owned_roots)
    }


def ensure_private_binding(api, config, hub, team_id, channel_id, thread_ts):
    if config["routing"]["binding_authoring_mode"] != "automatic":
        return None
    with _AUTHORING_LOCK:
        binding = ensure_binding_record(config, team_id, channel_id, thread_ts, hub.host, hub.port)
        store_binding_bot_token(binding["opaque_id"], api.token_for_team(team_id))
        for attempt in range(config["private_run"]["authoring_retry_attempts"]):
            topology = binding_topology(config, binding)
            managed_topology = package_owned_topology(topology)
            private_writes = missing_topology_writes(config, managed_topology)
            private_writes.extend(private_state_writes(config, binding, topology))
            shared_writes = shared_policy_writes(config, binding)
            if private_writes or shared_writes:
                writes = [gateway_rearm_write(config, binding["opaque_id"])] + private_writes + shared_writes
                hand_json(
                    config["private_run"]["file_write_label"],
                    {"tool": config["private_run"]["file_write_tool"], "arguments": {"writes": writes}, "correlation_id": f"slack-binding-authoring:{binding['opaque_id']}:{time.time_ns()}"},
                    f"slack-binding-authoring-{binding['opaque_id']}-{time.time_ns()}",
                )
            if not wait_for_shared_policy_admissions(
                config,
                binding,
                {write["path"]: write["expected_before_hash"] for write in shared_writes},
            ):
                if attempt + 1 < config["private_run"]["authoring_retry_attempts"]:
                    continue
                raise RuntimeError("Slack exact parent and worker admissions could not be authored without overwriting concurrent state")
            try:
                hub.wait_for(team_id, channel_id, thread_ts, config["private_run"]["result_timeout_ms"])
            except TimeoutError:
                if attempt + 1 == config["private_run"]["authoring_retry_attempts"]:
                    raise
                continue
            break
        commit_topology_ownership(config, managed_topology)
        if binding["status"] != "ready":
            mark_binding_ready(binding)
        return binding


def reconcile_known_bindings(api, config, hub):
    with _AUTHORING_LOCK:
        coordinates = [
            (binding["team_id"], binding["channel_id"], binding["thread_ts"])
            for _, binding in sorted(load_bindings()["bindings"].items())
        ]
    for team_id, channel_id, thread_ts in coordinates:
        ensure_private_binding(api, config, hub, team_id, channel_id, thread_ts)


class BindingConnection:
    def __init__(self, sock, maximum):
        self.sock = sock
        self.maximum = maximum
        self.lock = threading.Lock()

    def dispatch(self, candidate):
        with self.lock:
            write_frame(self.sock, {"kind": "event", "candidate": candidate}, self.maximum)
            response = read_frame(self.sock, self.maximum)
        if response.get("kind") != "event_verdict" or response.get("ok") is not True:
            raise RuntimeError(f"Slack private binding refused event: {sanitize_external_error(response)}")
        return response

    def probe_idle(self):
        if not self.lock.acquire(blocking=False):
            return "busy"
        try:
            try:
                payload = self.sock.recv(1, socket.MSG_PEEK | getattr(socket, "MSG_DONTWAIT", 0))
            except BlockingIOError:
                return "open"
            return "closed" if not payload else "data"
        finally:
            self.lock.release()

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


class BindingHub:
    def __init__(self, config):
        self.config = config
        self.maximum = config["routing"]["ipc_max_frame_bytes"]
        self.lock = threading.RLock()
        self.condition = threading.Condition(self.lock)
        self.connections = {}
        configured_host = config["routing"]["ipc_listen"]
        if not is_loopback_host(configured_host):
            raise ValueError("Slack private IPC listener must be loopback")
        self.listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        self.listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        self.listener.bind((configured_host, config["routing"]["ipc_port"]))
        self.listener.listen(64)
        self.host, self.port = self.listener.getsockname()
        self.registration_slots = threading.BoundedSemaphore(PRIVATE_REGISTRATION_LIMIT)
        threading.Thread(target=self._accept, daemon=True).start()

    def _accept(self):
        while True:
            try:
                sock, _ = self.listener.accept()
            except OSError:
                if self.listener.fileno() < 0:
                    return
                time.sleep(0.05)
                continue
            if not self.registration_slots.acquire(blocking=False):
                try:
                    sock.close()
                except OSError:
                    pass
                continue
            try:
                threading.Thread(target=self._register, args=(sock,), daemon=True).start()
            except Exception:
                self.registration_slots.release()
                try:
                    sock.close()
                except OSError:
                    pass
                raise

    def _register(self, sock):
        connection = None
        coordinate = None
        registration_released = False
        try:
            sock.settimeout(self.config["transport"]["connect_timeout_ms"] / 1000.0)
            registration_maximum = min(self.maximum, PRIVATE_REGISTRATION_MAX_BYTES)
            request = read_frame(sock, registration_maximum)
            request = require_keys(request, {"kind", "interface", "team_id", "channel_id", "thread_ts", "secret"}, set(), "Slack IPC registration")
            if request["kind"] != "register":
                raise ValueError("Slack IPC first frame must register")
            registry = load_binding_registry(self.config)
            coordinate = (request["team_id"], request["channel_id"], request["thread_ts"])
            expected = registry.get(coordinate)
            if expected is None or expected["interface"] != request["interface"]:
                raise ValueError("Slack IPC binding is not declared")
            secret = require_string(request["secret"], "Slack IPC registration.secret")
            if not hmac.compare_digest(hashlib.sha256(secret.encode("utf-8")).hexdigest(), expected["secret_sha256"]):
                raise ValueError("Slack IPC binding secret mismatched")
            connection = BindingConnection(sock, self.maximum)
            write_frame(sock, {"kind": "registered", "interface": request["interface"]}, registration_maximum)
            sock.settimeout(None)
            with self.condition:
                previous = self.connections.get(coordinate)
                self.connections[coordinate] = connection
                self.condition.notify_all()
            if previous is not None:
                previous.close()
            self.registration_slots.release()
            registration_released = True
            while True:
                readable, _, _ = select.select([sock], [], [], 60)
                if not readable:
                    continue
                state = connection.probe_idle()
                if state == "busy":
                    time.sleep(0.01)
                    continue
                if state == "closed":
                    return
                if state == "data":
                    raise ValueError("Slack private binding sent an unsolicited frame")
        except Exception as error:
            try:
                write_frame(sock, {"kind": "registration_refused", "error": sanitize_external_error(error, self.config)}, self.maximum)
            except Exception:
                pass
        finally:
            if not registration_released:
                self.registration_slots.release()
            if coordinate is not None and connection is not None:
                with self.lock:
                    if self.connections.get(coordinate) is connection:
                        self.connections.pop(coordinate, None)
            try:
                sock.close()
            except OSError:
                pass

    def private_connection(self, team, channel, thread):
        with self.lock:
            return self.connections.get((team, channel, thread))

    def wait_for(self, team, channel, thread, timeout_ms):
        coordinate = (team, channel, thread)
        deadline = time.monotonic() + timeout_ms / 1000.0
        with self.condition:
            while coordinate not in self.connections:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"Slack private binding {coordinate!r} did not activate")
                self.condition.wait(remaining)
            return self.connections[coordinate]

    def dispatch(self, team, channel, thread, candidate):
        coordinate = (team, channel, thread)
        with self.lock:
            connection = self.connections.get(coordinate)
        if connection is None:
            return None
        try:
            return connection.dispatch(candidate)
        except Exception:
            connection.close()
            with self.lock:
                if self.connections.get(coordinate) is connection:
                    self.connections.pop(coordinate, None)
            raise


def mark_pending(record, status, **updates):
    record = dict(record)
    record["status"] = status
    record.update(updates)
    atomic_json(pending_event_path(record["key"]), record, indent=2)
    return record


def load_envelope(record):
    with open(record["raw_path"], "rb") as handle:
        raw = handle.read()
    if hashlib.sha256(raw).hexdigest() != record["raw_sha256"]:
        raise RuntimeError("Slack raw event evidence digest mismatched")
    value = json.loads(raw)
    if not isinstance(value, dict):
        raise RuntimeError("Slack raw event evidence is not an object")
    return value


def validate_private_candidate(config, candidate):
    candidate = require_keys(candidate, {"schema_version", "event_key", "raw_sha256", "envelope"}, set(), "Slack private candidate")
    if candidate["schema_version"] != 1:
        raise ValueError("Slack private candidate schema_version must be 1")
    event_key = require_string(candidate["event_key"], "Slack private candidate event_key", maximum=512)
    digest = require_string(candidate["raw_sha256"], "Slack private candidate raw_sha256")
    if not re.fullmatch(r"[0-9a-f]{64}", digest):
        raise ValueError("Slack private candidate raw_sha256 is malformed")
    envelope = require_object(candidate["envelope"], "Slack private candidate envelope")
    raw = json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    if hashlib.sha256(raw).hexdigest() != digest:
        raise ValueError("Slack private candidate raw digest mismatched")
    if envelope_key(envelope) != event_key:
        raise ValueError("Slack private candidate event key mismatched its envelope")
    admitted, reason = should_admit(config, envelope, load_state())
    if not admitted:
        raise ValueError(f"Slack private candidate failed child admission: {reason}")
    binding = config["binding"]
    team, channel, thread, message_ts = event_coordinates(envelope)
    if team != binding["team_id"] or channel != binding["channel_id"]:
        raise ValueError("Slack private candidate crossed its bound team or channel")
    observed_thread = thread or (message_ts if binding["channel_id"].startswith("D") else None)
    if binding["thread_ts"] is not None and observed_thread != binding["thread_ts"]:
        raise ValueError("Slack private candidate crossed its bound thread")
    return envelope, event_key, digest, raw


def load_binding_events(config):
    try:
        value = json.loads(read_bounded_regular(BINDING_EVENTS_PATH, TOPOLOGY_MAX_BYTES, "binding event journal"))
    except FileNotFoundError:
        return {"schema_version": 1, "events": {}}
    value = require_keys(value, {"schema_version", "events"}, set(), "Slack binding event journal")
    if value["schema_version"] != 1 or not isinstance(value["events"], dict):
        raise ValueError("Slack binding event journal is malformed")
    if len(value["events"]) > config["intake"]["dedup_retention"]:
        raise ValueError("Slack binding event journal exceeds its configured bound")
    for event_key, record in value["events"].items():
        require_string(event_key, "Slack binding event journal key", maximum=512)
        record = require_keys(record, {"raw_sha256", "payload_sha256", "payload_path", "raw_path", "admitted_seq"}, set(), f"Slack binding event {event_key}")
        for field in ("raw_sha256", "payload_sha256"):
            if not re.fullmatch(r"[0-9a-f]{64}", require_string(record[field], f"Slack binding event {event_key}.{field}")):
                raise ValueError(f"Slack binding event {event_key}.{field} is malformed")
        for field, root in (("payload_path", PAYLOAD_DIR), ("raw_path", RAW_DIR)):
            path = require_string(record[field], f"Slack binding event {event_key}.{field}")
            if not os.path.isabs(path) or os.path.commonpath((os.path.realpath(path), os.path.realpath(root))) != os.path.realpath(root):
                raise ValueError(f"Slack binding event {event_key}.{field} escaped private state")
        if record["admitted_seq"] is not None:
            require_int(record["admitted_seq"], f"Slack binding event {event_key}.admitted_seq", 0)
    return value


def trim_binding_events(config, state):
    limit = config["intake"]["dedup_retention"]
    while len(state["events"]) > limit:
        oldest = next(iter(state["events"]))
        record = state["events"][oldest]
        if record["admitted_seq"] is None:
            raise RuntimeError("Slack binding event journal cannot evict an uncommitted event")
        state["events"].pop(oldest)
        for field in ("payload_path", "raw_path"):
            try:
                os.unlink(record[field])
            except FileNotFoundError:
                pass


def process_private_candidate(api, config, candidate):
    with _BINDING_EVENTS_LOCK:
        return process_private_candidate_locked(api, config, candidate)


def process_private_candidate_locked(api, config, candidate):
    envelope, event_key, raw_sha256, raw = validate_private_candidate(config, candidate)
    state = load_binding_events(config)
    existing = state["events"].get(event_key)
    if existing is not None:
        if existing["raw_sha256"] != raw_sha256:
            raise RuntimeError(f"Slack private event {event_key!r} was reused with different raw bytes")
        payload = read_bounded_regular(existing["payload_path"], config["routing"]["ipc_max_frame_bytes"], "binding event payload")
        if hashlib.sha256(payload).hexdigest() != existing["payload_sha256"]:
            raise RuntimeError(f"Slack private event {event_key!r} payload snapshot drifted")
        if existing["admitted_seq"] is not None:
            def mark(current):
                if event_key not in current["processed"]:
                    current["processed"].append(event_key)
            update_state(config, mark)
            return existing["admitted_seq"]
    else:
        raw_path = raw_event_path(event_key)
        try:
            current_raw = read_bounded_regular(raw_path, config["routing"]["ipc_max_frame_bytes"], "binding raw event")
            if current_raw != raw:
                raise RuntimeError(f"Slack private event {event_key!r} raw snapshot drifted")
        except FileNotFoundError:
            atomic_bytes(raw_path, raw)
        record = {"key": event_key, "raw_path": raw_path, "raw_sha256": raw_sha256}
        normalized = normalized_event(api, config, envelope, record, download_media=True)
        payload = json.dumps(normalized, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
        payload_sha256 = hashlib.sha256(payload).hexdigest()
        payload_path = os.path.join(PAYLOAD_DIR, f"binding-event-{hashlib.sha256(event_key.encode('utf-8')).hexdigest()}.json")
        atomic_bytes(payload_path, payload)
        existing = {"raw_sha256": raw_sha256, "payload_sha256": payload_sha256, "payload_path": payload_path, "raw_path": raw_path, "admitted_seq": None}
        state["events"][event_key] = existing
        trim_binding_events(config, state)
        atomic_json(BINDING_EVENTS_PATH, state, indent=2)
    normalized = require_object(json.loads(payload), "Slack binding event payload")
    admitted_seq = hand_json_idempotent("slack.event", normalized, f"slack-event-{hashlib.sha256(event_key.encode('utf-8')).hexdigest()[:32]}", config)
    existing["admitted_seq"] = admitted_seq
    atomic_json(BINDING_EVENTS_PATH, state, indent=2)
    def mark(current):
        if event_key not in current["processed"]:
            current["processed"].append(event_key)
    update_state(config, mark)
    return admitted_seq


def process_pending_event(api, config, hub, record):
    envelope = load_envelope(record)
    state = load_state()
    if record["key"] in state["processed"]:
        try:
            os.unlink(pending_event_path(record["key"]))
        except FileNotFoundError:
            pass
        return True
    admitted, reason = should_admit(config, envelope, state)
    team, channel, thread, ts = event_coordinates(envelope)
    if not admitted:
        hand_json("slack.intake.refused", {"schema_version": 1, "event_key": record["key"], "reason": reason, "team_id": team, "channel_id": channel, "thread_ts": thread, "message_ts": ts, "evidence": {"raw_event_path": record["raw_path"], "raw_event_sha256": record["raw_sha256"]}}, f"slack-refused-{record['key']}")
        def mark(current):
            current["processed"].append(record["key"])
        update_state(config, mark)
        os.unlink(pending_event_path(record["key"]))
        return True
    private_thread = thread or (ts if message_shape(envelope) is not None else None)
    connection = hub.private_connection(team, channel, private_thread) if hub is not None else None
    privacy_required = config["routing"]["private_scope"] == "all" and team is not None and channel is not None and (message_shape(envelope) is not None or thread is not None)
    if connection is None and privacy_required and config["routing"]["unbound_policy"] == "retain" and config["routing"]["binding_authoring_mode"] == "automatic":
        if hub is None:
            raise RuntimeError("Slack automatic private binding has no gateway hub")
        ensure_private_binding(api, config, hub, team, channel, private_thread)
        connection = hub.private_connection(team, channel, private_thread)
        if connection is None:
            raise RuntimeError("Slack private binding activated without a live endpoint")
    awaiting_private = connection is None and privacy_required and config["routing"]["unbound_policy"] == "retain"
    if connection is not None:
        candidate = {"schema_version": 1, "event_key": record["key"], "raw_sha256": record["raw_sha256"], "envelope": envelope}
        result = hub.dispatch(team, channel, private_thread, candidate)
        runtime_seq = result.get("runtime_seq")
        if isinstance(runtime_seq, bool) or not isinstance(runtime_seq, int):
            raise RuntimeError("Slack private binding returned no admitted movement sequence")
    else:
        normalized = normalized_event(api, config, envelope, record, download_media=not awaiting_private)
    if connection is None and privacy_required and config["routing"]["unbound_policy"] == "retain":
        binding_request_seq = record.get("binding_request_seq")
        if binding_request_seq is None:
            binding_request_seq = hand_json("slack.binding.requested", {"schema_version": 1, "event_key": record["key"], "team_id": team, "channel_id": channel, "thread_ts": private_thread, "user_id": event_user(envelope), "evidence": normalized["evidence"]}, f"slack-binding-request-{record['key']}")
        mark_pending(record, "awaiting_binding", attempts=record.get("attempts", 0) + 1, last_attempt_unix_ms=int(time.time() * 1000), binding_request_seq=binding_request_seq)
        return False
    elif connection is None and privacy_required and config["routing"]["unbound_policy"] == "refuse":
        hand_json("slack.binding.refused", {"schema_version": 1, "event_key": record["key"], "team_id": team, "channel_id": channel, "thread_ts": thread, "message_ts": ts, "reason": "private_binding_missing", "evidence": normalized["evidence"]}, f"slack-binding-refused-{record['key']}")
        runtime_seq = None
    elif connection is None:
        runtime_seq = hand_json("slack.event", normalized, f"slack-event-{record['key']}")
    def mark(current):
        current["processed"].append(record["key"])
    update_state(config, mark)
    os.unlink(pending_event_path(record["key"]))
    return runtime_seq is not None or config["routing"]["unbound_policy"] == "refuse"


def retry_pending(api, config, hub):
    for name in sorted(os.listdir(PENDING_DIR)):
        if not name.endswith(".json"):
            continue
        path = os.path.join(PENDING_DIR, name)
        try:
            with open(path, "r", encoding="utf-8") as handle:
                record = json.load(handle)
            if not isinstance(record, dict) or record.get("schema_version") != 1:
                raise ValueError("Slack pending record is malformed")
            process_pending_event(api, config, hub, record)
        except Exception as error:
            stderr(config, f"Slack pending event {name} failed: {error}")


def connect_private_binding(config, api):
    binding = config["binding"]
    if not is_loopback_host(binding["gateway_host"]):
        raise ValueError("Slack private binding gateway must be loopback")
    with open_private_read(binding["secret_file"]) as handle:
        secret = handle.read().decode("ascii").strip()
    if len(secret) < 32 or len(secret) > 512:
        raise ValueError("Slack binding secret file must contain 32..512 characters")
    maximum = config["routing"]["ipc_max_frame_bytes"]
    backoff = config["transport"]["reconnect_backoff_ms"]
    while True:
        sock = None
        try:
            sock = socket.create_connection((binding["gateway_host"], binding["gateway_port"]), timeout=config["transport"]["connect_timeout_ms"] / 1000.0)
            write_frame(sock, {"kind": "register", "interface": INTERFACE_ID, "team_id": binding["team_id"], "channel_id": binding["channel_id"], "thread_ts": binding["thread_ts"], "secret": secret}, maximum)
            reply = read_frame(sock, maximum)
            if reply.get("kind") != "registered":
                raise RuntimeError(f"Slack gateway refused private binding: {reply!r}")
            sock.settimeout(None)
            backoff = config["transport"]["reconnect_backoff_ms"]
            while True:
                request = read_frame(sock, maximum)
                request = require_keys(request, {"kind", "candidate"}, set(), "Slack private event frame")
                if request["kind"] != "event" or not isinstance(request["candidate"], dict):
                    raise ValueError("Slack gateway sent an invalid event frame")
                runtime_seq = process_private_candidate(api, config, request["candidate"])
                write_frame(sock, {"kind": "event_verdict", "ok": True, "runtime_seq": runtime_seq}, maximum)
        except Exception as error:
            stderr(config, f"Slack private binding reconnecting after error: {error}")
            if sock is not None:
                try:
                    sock.close()
                except OSError:
                    pass
            time.sleep(backoff / 1000.0)
            backoff = min(backoff * 2, config["transport"]["reconnect_backoff_cap_ms"])


def load_cursor():
    try:
        with open(CURSOR_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return 0
    value = require_keys(value, {"schema_version", "next_seq"}, set(), "Slack ledger cursor")
    if value["schema_version"] != 1:
        raise ValueError("Slack ledger cursor schema_version must be 1")
    return require_int(value["next_seq"], "Slack ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 or item > 255 for item in content):
            raise ValueError("Slack artifact content list is not a 32-byte hash")
        content = bytes(content)
    if not isinstance(content, bytes) or len(content) != 32:
        raise ValueError("Slack artifact content must be a 32-byte hash")
    return content.hex()


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"Slack {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"Slack {name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"Slack {name} exceeds {maximum} bytes")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"Slack {name} grew above {maximum} bytes while reading")
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
            raise RuntimeError(f"Slack {name} changed while opening")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def materialize_movement_payload(content, config):
    digest = content_hexdigest(content)
    manifest_bytes = read_bounded_regular(
        os.path.join(BLOBS_DIR, digest),
        config["outbound"]["max_manifest_bytes"],
        "cached artifact manifest",
    )
    if octopus_content_hash(manifest_bytes) != digest:
        raise ValueError("Slack cached artifact manifest failed content-address verification")
    manifest = json.loads(manifest_bytes)
    manifest = require_object(manifest, "Slack cached artifact manifest")
    label = require_string(manifest.get("label"), "Slack cached artifact label")
    payload_digest = manifest.get("payload_content_hash")
    if not isinstance(payload_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_digest):
        raise ValueError("Slack cached artifact manifest has no payload hash")
    payload = read_bounded_regular(
        os.path.join(BLOBS_DIR, payload_digest),
        config["outbound"]["max_payload_bytes"],
        "cached artifact payload",
    )
    if octopus_content_hash(payload) != payload_digest:
        raise ValueError("Slack cached artifact payload failed content-address verification")
    return label, payload, digest


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
        "Slack outbound movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "Slack outbound movement runtime metadata",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"Slack outbound movement runtime.{field}",
        )
    require_int(runtime.get("epoch"), "Slack outbound movement runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("Slack outbound runtime interface does not match its exact target")
    return runtime


def read_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        schema = connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone()
        if schema != ("3",):
            raise RuntimeError("Slack ledger index schema is unsupported")
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
        seq = require_int(seq, "Slack ledger index seq", 0)
        require_string(source_interface, f"Slack ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content)):
            raise RuntimeError(f"Slack ledger index movement {seq} has malformed artifact hash")
        require_string(run, f"Slack ledger index movement {seq}.run")
        require_string(checkpoint, f"Slack ledger index movement {seq}.checkpoint")
        result.append((seq, bytes.fromhex(content) if content else None, run))
    return result


def read_ledger(next_seq, limit, max_row_bytes):
    global _LEDGER_FALLBACK_IDENTITY, _LEDGER_FALLBACK_OFFSET, _LEDGER_FALLBACK_SIZE
    max_row_bytes = require_int(max_row_bytes, "Slack ledger.max_row_bytes", 256, 16777216)
    with _LEDGER_LOCK:
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
                    raise RuntimeError(f"Slack ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
                if not line.endswith(b"\n"):
                    raise RuntimeError(f"Slack ledger row at byte {row_offset} is partial")
                movement = require_object(json.loads(line), "Slack ledger movement")
                seq = require_int(movement.get("seq"), "Slack ledger movement.seq", 0)
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
                artifact = movement.get("artifact") if isinstance(movement.get("artifact"), dict) else None
                if not has_uncommitted:
                    safe_offset = row_offset
                    has_uncommitted = True
                result.append((seq, artifact.get("content") if artifact else None, runtime.get("run")))
                if len(result) >= limit:
                    break
            _LEDGER_FALLBACK_OFFSET = safe_offset
            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
            _LEDGER_FALLBACK_IDENTITY = identity
        return result


def read_outbound(next_seq, limit=128, config=None):
    global _LEDGER_INDEX_WARNING
    config = _CONFIG if config is None else config
    if config is None:
        config = load_config()
    try:
        return read_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = sanitize_external_error(error, config)
        if message != _LEDGER_INDEX_WARNING:
            stderr(config, f"Slack ledger index unavailable; reading ledger truth: {message}")
            _LEDGER_INDEX_WARNING = message
        return read_ledger(next_seq, limit, config["ledger"]["max_row_bytes"])


def effect_path(source_seq):
    return os.path.join(EFFECT_DIR, f"{source_seq}.json")


def load_effect(source_seq):
    try:
        with open(effect_path(source_seq), "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return None
    original = value
    value = sanitize_external_error(value, _CONFIG)
    validate_effect(source_seq, value)
    if value != original:
        atomic_json(effect_path(source_seq), value, indent=2)
    return value


def validate_effect(source_seq, value):
    value = require_keys(value, {"schema_version", "source_movement_seq", "source_artifact_hash", "run", "artifact_label", "status"}, {"operation", "result", "error", "evidence_movement_seq"}, f"Slack effect {source_seq}")
    if value["schema_version"] != 1 or value["source_movement_seq"] != source_seq:
        raise ValueError(f"Slack effect {source_seq} identity drifted")
    if not isinstance(value["source_artifact_hash"], str) or not re.fullmatch(r"[0-9a-f]{64}", value["source_artifact_hash"]):
        raise ValueError(f"Slack effect {source_seq} artifact hash is malformed")
    require_string(value["run"], f"Slack effect {source_seq}.run")
    require_string(value["artifact_label"], f"Slack effect {source_seq}.artifact_label")
    if value["status"] not in {"started", "succeeded", "indeterminate", "evidenced", "failure_evidenced"}:
        raise ValueError(f"Slack effect {source_seq} status is invalid")
    if "result" in value and not isinstance(value["result"], dict):
        raise ValueError(f"Slack effect {source_seq}.result must be an object")
    if "error" in value:
        require_string(value["error"], f"Slack effect {source_seq}.error")
    return value


def commit_effect(source_seq, value):
    value = sanitize_external_error(value, _CONFIG)
    validate_effect(source_seq, value)
    atomic_json(effect_path(source_seq), value, indent=2)


def delivery_evidence(source_seq, content_hash, run, result):
    return hand_json("slack.delivery.result", {"source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "ok": True, "result": result}, f"slack-delivery-{source_seq}")


def require_channel(value, name):
    value = require_string(value, name)
    if not CHANNEL_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Slack channel id")
    return value


def require_ts(value, name, optional=False):
    if value is None and optional:
        return None
    value = require_string(value, name)
    if not TS_RE.fullmatch(value):
        raise ValueError(f"{name} is not a Slack timestamp")
    return value


def target_channel(args, binding, config=None):
    home = os.environ.get(config["outbound"]["home_channel_env"]) if config else None
    channel = args.get("channel") or args.get("channel_id") or (binding.get("channel_id") if binding else None) or home
    channel = require_channel(channel, "Slack operation channel")
    if binding and channel != binding["channel_id"]:
        raise ValueError("Slack operation cannot leave its private channel binding")
    return channel


def target_thread(args, binding):
    thread = args.get("thread_ts")
    if thread is None and binding:
        thread = binding.get("thread_ts")
    thread = require_ts(thread, "Slack operation thread_ts", optional=True)
    if binding and binding.get("thread_ts") is not None and thread != binding["thread_ts"]:
        raise ValueError("Slack operation cannot leave its private thread binding")
    return thread


def target_team(args, binding):
    team = args.get("team_id") or (binding.get("team_id") if binding else None)
    if team is not None and (not isinstance(team, str) or not TEAM_RE.fullmatch(team)):
        raise ValueError("Slack operation team_id is malformed")
    if binding and team != binding["team_id"]:
        raise ValueError("Slack operation cannot leave its private workspace binding")
    return team


def split_text(text, maximum):
    text = require_string(text, "Slack message text")
    if len(text) <= maximum:
        return [text]
    chunks = []
    remaining = text
    while remaining:
        if len(remaining) <= maximum:
            chunks.append(remaining)
            break
        cut = remaining.rfind("\n", 0, maximum + 1)
        if cut < maximum // 2:
            cut = remaining.rfind(" ", 0, maximum + 1)
        if cut < maximum // 2:
            cut = maximum
        chunks.append(remaining[:cut].rstrip())
        remaining = remaining[cut:].lstrip()
    return chunks


def idempotency_uuid(seed):
    digest = hashlib.sha256(seed.encode("utf-8")).hexdigest()
    return f"{digest[:8]}-{digest[8:12]}-4{digest[13:16]}-a{digest[17:20]}-{digest[20:32]}"


def read_callback_record(handle):
    handle = require_string(handle, "Slack callback handle")
    if not re.fullmatch(r"[0-9a-f]{64}", handle):
        raise ValueError("Slack callback handle is malformed")
    path = os.path.join(CALLBACK_DIR, f"{handle}.json")
    with open_private_read(path) as source:
        value = require_keys(json.loads(source.read()), {"schema_version", "response_url", "trigger_id", "created_at_unix_ms"}, set(), "Slack callback record")
    if value["schema_version"] != 1:
        raise ValueError("Slack callback record schema is unsupported")
    return value


def callback_url(handle):
    return require_string(read_callback_record(handle)["response_url"], "Slack callback response_url")


def callback_trigger(handle):
    handle = require_string(handle, "Slack trigger handle")
    if not re.fullmatch(r"[0-9a-f]{64}", handle):
        raise ValueError("Slack trigger handle is malformed")
    return require_string(read_callback_record(handle)["trigger_id"], "Slack callback trigger_id")


def artifact_blob(blob_id, maximum):
    if not isinstance(blob_id, str) or not re.fullmatch(r"[0-9a-f]{64}", blob_id):
        raise ValueError("Slack file upload blob_id must be a content hash")
    content = read_bounded_regular(os.path.join(BLOBS_DIR, blob_id), maximum, "file upload blob")
    if octopus_content_hash(content) != blob_id:
        raise ValueError("Slack file upload blob failed content-address verification")
    return content


def record_bot_thread(config, response, channel, thread):
    if not config["gating"]["thread_sticky_after_bot_reply"]:
        return
    ts = response.get("ts") if isinstance(response, dict) else None
    effective = thread or ts
    team = response.get("team") if isinstance(response, dict) else None
    if not effective or not channel:
        return
    key = f"{channel}:{effective}"
    def add(current):
        if key not in current["bot_message_threads"]:
            current["bot_message_threads"].append(key)
    update_state(config, add)


def load_stream_state():
    try:
        with open(STREAM_STATE_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return {"schema_version": 1, "streams": {}}
    value = require_keys(value, {"schema_version", "streams"}, set(), "Slack stream state")
    if value["schema_version"] != 1 or not isinstance(value["streams"], dict):
        raise ValueError("Slack stream state is malformed")
    return value


def stream_key(channel, thread):
    return f"{channel}:{thread or '-'}"


def execute_stream(api, config, operation, args, binding):
    channel = target_channel(args, binding, config)
    thread = target_thread(args, binding)
    team = target_team(args, binding)
    key = stream_key(channel, thread)
    state = load_stream_state()
    streams = state["streams"]
    current = streams.get(key)
    if current is not None and not isinstance(current, dict):
        raise ValueError("Slack stream entry is malformed")
    if operation == "stream.delta":
        sequence = require_int(args.get("seq"), "Slack stream sequence", 0)
        delta = require_string(args.get("text"), "Slack stream delta", allow_empty=True)
        if current is None:
            if sequence != 0:
                raise ValueError("Slack stream must start at sequence zero")
            response = api.call("chat.postMessage", {"channel": channel, "thread_ts": thread, "text": slack_mrkdwn(delta) or "…", "mrkdwn": True, "client_msg_id": args["client_msg_id"]}, team_id=team)
            ts = require_ts(response.get("ts"), "Slack stream message ts")
            current = {"channel": channel, "thread_ts": thread, "message_ts": ts, "last_seq": sequence, "text": delta, "last_edit_unix_ms": int(time.time() * 1000)}
            streams[key] = current
            atomic_json(STREAM_STATE_PATH, state, indent=2)
            return {"operation": operation, "response": response, "sequence": sequence}
        previous = require_int(current.get("last_seq"), "Slack stream last sequence", 0)
        if sequence <= previous:
            return {"operation": operation, "disposition": "duplicate", "sequence": sequence}
        if sequence != previous + 1:
            raise ValueError(f"Slack stream sequence jumped from {previous} to {sequence}")
        current["text"] = str(current.get("text") or "") + delta
        current["last_seq"] = sequence
        now = int(time.time() * 1000)
        if now - require_int(current.get("last_edit_unix_ms"), "Slack stream last edit time", 0) >= config["outbound"]["stream_edit_interval_ms"]:
            response = api.call("chat.update", {"channel": channel, "ts": current["message_ts"], "text": slack_mrkdwn(current["text"]) or "…"}, team_id=team)
            current["last_edit_unix_ms"] = now
            disposition = "edited"
        else:
            response = {"ok": True}
            disposition = "buffered"
        streams[key] = current
        atomic_json(STREAM_STATE_PATH, state, indent=2)
        return {"operation": operation, "response": response, "disposition": disposition, "sequence": sequence}
    text = require_string(args.get("text"), "Slack stream final text")
    if current is None:
        return execute_operation(api, config, "message.post", {"team_id": team, "channel": channel, "thread_ts": thread, "text": text}, args["source_hash"], binding)
    response = api.call("chat.update", {"channel": channel, "ts": current["message_ts"], "text": slack_mrkdwn(text)}, team_id=team)
    status_clear = None
    if thread:
        try:
            status_clear = {"ok": True, "response": api.call("assistant.threads.setStatus", {"channel_id": channel, "thread_ts": thread, "status": ""}, team_id=team)}
        except Exception as error:
            status_clear = {"ok": False, "error": sanitize_external_error(error, config)}
    streams.pop(key, None)
    atomic_json(STREAM_STATE_PATH, state, indent=2)
    return {"operation": operation, "response": response, "status_clear": status_clear}


def execute_operation(api, config, operation, args, source_hash, binding):
    outbound = config["outbound"]
    if operation in {"stream.delta", "stream.final"}:
        args = dict(args)
        args["client_msg_id"] = idempotency_uuid(source_hash)
        args["source_hash"] = source_hash
        return execute_stream(api, config, operation, args, binding)
    team = target_team(args, binding)
    if operation == "message.post":
        channel = target_channel(args, binding, config)
        thread = target_thread(args, binding)
        text = args.get("text")
        blocks = safe_blocks(args.get("blocks"), outbound["max_block_json_bytes"])
        if len(blocks) > outbound["max_blocks"]:
            raise ValueError("Slack message blocks exceed configured count")
        if not isinstance(text, str) or not text:
            if not blocks:
                raise ValueError("Slack message.post requires text or blocks")
            text = args.get("fallback_text") or "Message"
        chunks = split_text(text, outbound["max_message_chars"])
        if blocks and len(chunks) != 1:
            raise ValueError("Slack block message text cannot require splitting")
        results = []
        for index, chunk in enumerate(chunks):
            params = {"channel": channel, "text": slack_mrkdwn(chunk), "mrkdwn": args.get("mrkdwn", True)}
            if thread:
                params["thread_ts"] = thread
            if args.get("reply_broadcast") is not None:
                params["reply_broadcast"] = bool(args["reply_broadcast"])
            if blocks and index == 0:
                params["blocks"] = blocks
            if isinstance(args.get("metadata"), dict):
                params["metadata"] = args["metadata"]
            params["client_msg_id"] = idempotency_uuid(f"{source_hash}:{index}")
            response = api.call("chat.postMessage", params, team_id=team)
            record_bot_thread(config, response, channel, thread)
            results.append(response)
        status_clear = None
        if thread:
            try:
                status_clear = {"ok": True, "response": api.call("assistant.threads.setStatus", {"channel_id": channel, "thread_ts": thread, "status": ""}, team_id=team)}
            except Exception as error:
                status_clear = {"ok": False, "error": sanitize_external_error(error, config)}
        return {"operation": operation, "messages": results, "status_clear": status_clear}
    if operation == "message.ephemeral":
        channel = target_channel(args, binding, config)
        user = require_string(args.get("user"), "Slack ephemeral user")
        if not USER_RE.fullmatch(user):
            raise ValueError("Slack ephemeral user is malformed")
        text = slack_mrkdwn(require_string(args.get("text"), "Slack ephemeral text", maximum=outbound["max_message_chars"]))
        params = {"channel": channel, "user": user, "text": text}
        blocks = safe_blocks(args.get("blocks"), outbound["max_block_json_bytes"])
        if blocks:
            params["blocks"] = blocks
        thread = target_thread(args, binding)
        if thread:
            params["thread_ts"] = thread
        return {"operation": operation, "response": api.call("chat.postEphemeral", params, team_id=team)}
    if operation == "message.update":
        channel = target_channel(args, binding, config)
        ts = require_ts(args.get("ts"), "Slack message.update ts")
        params = {"channel": channel, "ts": ts}
        if "text" in args:
            params["text"] = slack_mrkdwn(require_string(args["text"], "Slack message.update text", maximum=outbound["max_message_chars"]))
        blocks = safe_blocks(args.get("blocks"), outbound["max_block_json_bytes"])
        if blocks:
            params["blocks"] = blocks
        if "text" not in params and "blocks" not in params:
            raise ValueError("Slack message.update requires text or blocks")
        return {"operation": operation, "response": api.call("chat.update", params, team_id=team)}
    if operation == "message.delete":
        return {"operation": operation, "response": api.call("chat.delete", {"channel": target_channel(args, binding, config), "ts": require_ts(args.get("ts"), "Slack message.delete ts")}, team_id=team)}
    if operation in {"reaction.add", "reaction.remove"}:
        method = "reactions.add" if operation.endswith("add") else "reactions.remove"
        params = {"channel": target_channel(args, binding, config), "timestamp": require_ts(args.get("timestamp") or args.get("ts"), "Slack reaction timestamp"), "name": require_string(args.get("name"), "Slack reaction name")}
        return {"operation": operation, "response": api.call(method, params, team_id=team)}
    if operation == "file.upload":
        channel = target_channel(args, binding, config)
        requested = args.get("files")
        if requested is None:
            requested = [{"filename": args.get("filename"), "blob_id": args.get("blob_id"), "title": args.get("title")}]
        if not isinstance(requested, list) or not requested or len(requested) > 10:
            raise ValueError("Slack file.upload files must contain 1..10 items")
        files = []
        for index, item in enumerate(requested):
            item = require_object(item, f"Slack file.upload files[{index}]")
            title = item.get("title")
            if title is not None:
                title = require_string(title, f"Slack file.upload files[{index}].title", maximum=255)
            files.append({"filename": require_string(item.get("filename"), f"Slack file.upload files[{index}].filename", maximum=255), "content": artifact_blob(item.get("blob_id"), config["media"]["max_upload_bytes"]), "title": title})
        initial_comment = args.get("initial_comment") or args.get("caption")
        if initial_comment is not None:
            initial_comment = slack_mrkdwn(require_string(initial_comment, "Slack file.upload initial_comment", maximum=outbound["max_message_chars"]))
        return {"operation": operation, "response": api.upload_multiple_external(channel, files, initial_comment=initial_comment, thread_ts=target_thread(args, binding), team_id=team)}
    if operation in {"view.open", "view.push"}:
        trigger_id = callback_trigger(args.get("trigger_handle")) if args.get("trigger_handle") else require_string(args.get("trigger_id"), f"Slack {operation} trigger_id")
        view = require_object(args.get("view"), f"Slack {operation} view")
        method = "views.open" if operation == "view.open" else "views.push"
        return {"operation": operation, "response": api.call(method, {"trigger_id": trigger_id, "view": view}, team_id=team)}
    if operation == "view.update":
        view = require_object(args.get("view"), "Slack view.update view")
        params = {"view": view}
        if args.get("view_id"):
            params["view_id"] = require_string(args["view_id"], "Slack view.update view_id")
        elif args.get("external_id"):
            params["external_id"] = require_string(args["external_id"], "Slack view.update external_id")
        else:
            raise ValueError("Slack view.update requires view_id or external_id")
        if args.get("hash"):
            params["hash"] = require_string(args["hash"], "Slack view.update hash")
        return {"operation": operation, "response": api.call("views.update", params, team_id=team)}
    if operation == "view.publish":
        return {"operation": operation, "response": api.call("views.publish", {"user_id": require_string(args.get("user_id"), "Slack view.publish user_id"), "view": require_object(args.get("view"), "Slack view.publish view")}, team_id=team)}
    if operation == "callback.respond":
        body = require_object(args.get("body"), "Slack callback response body")
        return {"operation": operation, "response": api.post_response_url(callback_url(args.get("handle")), body)}
    if operation in {"assistant.status.set", "assistant.status.clear", "typing.set", "typing.clear"}:
        channel = target_channel(args, binding, config)
        thread = target_thread(args, binding)
        if thread is None:
            raise ValueError("Slack Assistant status requires thread_ts")
        status = "" if operation.endswith("clear") else require_string(args.get("status", "Working…"), "Slack Assistant status", maximum=100)
        return {"operation": operation, "response": api.call("assistant.threads.setStatus", {"channel_id": channel, "thread_ts": thread, "status": status}, team_id=team)}
    if operation == "profile.status.set":
        profile = {
            "status_text": require_string(args.get("status_text", ""), "Slack profile status_text", allow_empty=True, maximum=100),
            "status_emoji": require_string(args.get("status_emoji", ""), "Slack profile status_emoji", allow_empty=True, maximum=100),
            "status_expiration": require_int(args.get("status_expiration", 0), "Slack profile status_expiration", 0),
        }
        return {"operation": operation, "response": api.call("users.profile.set", {"profile": profile}, team_id=team)}
    if operation == "web.call":
        method = require_string(args.get("method"), "Slack web.call method")
        if method not in outbound["allowed_web_methods"]:
            raise ValueError(f"Slack Web API method {method!r} is not allowed")
        params = require_object(args.get("params", {}), "Slack web.call params")
        if binding:
            for key in ("channel", "channel_id"):
                if key in params and params[key] != binding["channel_id"]:
                    raise ValueError("Slack web.call cannot leave its private channel binding")
        return {"operation": operation, "method": method, "response": api.call(method, params, safe_retry=method in outbound["retry_safe_web_methods"], team_id=team)}
    raise ValueError(f"Slack operation {operation!r} is unsupported")


def prepare_output(config, label, payload, binding):
    try:
        body = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"Slack outbound payload is invalid JSON: {error}") from error
    body = require_object(body, "Slack outbound payload")
    outbound = config["outbound"]
    if label == config["private_run"]["file_write_result_label"]:
        return None, {"disposition": "topology_authoring_result", "label": label}
    if label in outbound["telemetry_labels"]:
        return None, {"disposition": "observed", "label": label}
    if label in outbound["stream_labels"]:
        require_keys(body, {"text", "seq"}, {"team_id", "channel", "thread_ts"}, "Slack stream payload")
        text = require_string(body.get("text"), "Slack stream text", allow_empty=True)
        args = {"text": text, "seq": body.get("seq")}
        if binding:
            args.update({"team_id": binding["team_id"], "channel": binding["channel_id"], "thread_ts": binding["thread_ts"]})
        elif body.get("channel"):
            args.update({"team_id": body.get("team_id"), "channel": body.get("channel"), "thread_ts": body.get("thread_ts")})
        return ("stream.delta", args), {"disposition": "stream_delta", "sequence": body.get("seq")}
    if label in outbound["error_labels"]:
        detail = body.get("message") or body.get("error") or body.get("detail") or json.dumps(body, ensure_ascii=False, sort_keys=True)
        target = {"team_id": binding.get("team_id"), "channel_id": binding.get("channel_id"), "thread_ts": binding.get("thread_ts")} if binding else None
        operation, args = normalize_operation({"text": f"Request failed: {detail}"}, config, target)
        return (operation, args), {"disposition": "error_delivered"}
    if label not in outbound["reply_labels"]:
        raise ValueError(f"Slack interface does not declare outbound label {label!r}")
    target = {"team_id": binding.get("team_id"), "channel_id": binding.get("channel_id"), "thread_ts": binding.get("thread_ts")} if binding else None
    if label == "interface.artifact.output" and isinstance(body.get("text"), str):
        stream = load_stream_state()["streams"].get(stream_key(target["channel_id"], target["thread_ts"])) if target else None
        if stream is not None:
            return ("stream.final", {"channel": target["channel_id"], "thread_ts": target["thread_ts"], "text": body["text"]}), {"disposition": "stream_final"}
    operation, args = normalize_operation(body, config, target)
    return (operation, args), {"disposition": "delivered", "operation": operation}


def process_outbound(api, config, source_seq, content, run):
    if content is None or run is None:
        hand_failure(source_seq, "poisoned_outbound", {"error": "movement carries no artifact or runtime run"}, f"slack-poison-{source_seq}")
        return
    binding = config["binding"] if config["mode"] == "binding" else None
    if binding and run != binding["run"]:
        hand_failure(source_seq, "boundary_rejected", {"error": "outbound movement run mismatched private binding", "expected_run": binding["run"], "observed_run": run}, f"slack-run-refused-{source_seq}")
        return
    label, payload, content_hash = materialize_movement_payload(content, config)
    effect = load_effect(source_seq)
    if effect is not None and effect["source_artifact_hash"] != content_hash:
        hand_failure(source_seq, "effect_journal_mismatch", {"expected": effect["source_artifact_hash"], "observed": content_hash}, f"slack-effect-mismatch-{source_seq}")
        return
    if effect is not None and effect["status"] in {"evidenced", "failure_evidenced"}:
        return
    if effect is not None and effect["status"] == "succeeded":
        evidence = delivery_evidence(source_seq, content_hash, run, effect["result"])
        effect.update({"status": "evidenced", "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] in {"started", "indeterminate"}:
        evidence = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "error": effect.get("error")}, f"slack-indeterminate-{source_seq}")
        effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        return
    try:
        prepared, disposition = prepare_output(config, label, payload, binding)
    except Exception as error:
        hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "error": sanitize_external_error(error, config)}, f"slack-boundary-{source_seq}")
        return
    effect = {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "artifact_label": label, "status": "succeeded" if prepared is None else "started"}
    if prepared is None:
        effect["result"] = disposition
        commit_effect(source_seq, effect)
    else:
        operation, args = prepared
        effect["operation"] = operation
        commit_effect(source_seq, effect)
        try:
            result = execute_operation(api, config, operation, args, content_hash, binding)
            result.update(disposition)
        except ValueError as error:
            safe_error = sanitize_external_error(error, config)
            evidence = hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "operation": operation, "error": safe_error}, f"slack-boundary-{source_seq}")
            effect.update({"status": "failure_evidenced", "error": safe_error, "evidence_movement_seq": evidence})
            commit_effect(source_seq, effect)
            return
        except Exception as error:
            safe_error = sanitize_external_error(error, config)
            effect.update({"status": "indeterminate", "error": safe_error})
            commit_effect(source_seq, effect)
            evidence = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "operation": operation, "error": safe_error}, f"slack-indeterminate-{source_seq}")
            effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence})
            commit_effect(source_seq, effect)
            return
        effect.update({"status": "succeeded", "result": result})
        commit_effect(source_seq, effect)
    evidence = delivery_evidence(source_seq, content_hash, run, effect["result"])
    effect.update({"status": "evidenced", "evidence_movement_seq": evidence})
    commit_effect(source_seq, effect)


def outbound_loop(api, config):
    next_seq = load_cursor()
    while True:
        rows = read_outbound(next_seq, config=config)
        for source_seq, content, run in rows:
            try:
                process_outbound(api, config, source_seq, content, run)
            except Exception as error:
                stderr(config, f"Slack outbound movement {source_seq} failed: {error}")
                time.sleep(0.25)
                break
            next_seq = source_seq + 1
            commit_cursor(next_seq)
        time.sleep(0.05)


def authenticate(api, config):
    responses = []
    bot_ids = {}
    for token in api.bot_tokens:
        response = api.call("auth.test", {}, safe_retry=True, token_override=token)
        user_id = response.get("user_id")
        team_id = response.get("team_id")
        if not isinstance(user_id, str) or not USER_RE.fullmatch(user_id):
            raise RuntimeError("Slack auth.test returned no valid bot user id")
        if not isinstance(team_id, str) or not TEAM_RE.fullmatch(team_id):
            raise RuntimeError("Slack auth.test returned no valid team id")
        if team_id in api.team_tokens:
            raise RuntimeError(f"Slack bot token list repeats workspace {team_id}")
        api.team_tokens[team_id] = token
        bot_ids[team_id] = user_id
        responses.append(response)
    def set_bot(state):
        state["bot_user_ids"] = bot_ids
    update_state(config, set_bot)
    return responses


def gateway_loop(api, config):
    hub = BindingHub(config)
    reconcile_known_bindings(api, config, hub)
    retry_pending(api, config, hub)
    initial = config["transport"]["reconnect_backoff_ms"]
    cap = config["transport"]["reconnect_backoff_cap_ms"]
    backoff = initial
    while True:
        wire = None
        try:
            wire = WebSocket(api.open_socket_url(), config["transport"]["connect_timeout_ms"], config["transport"]["max_frame_bytes"], api.proxy_url).connect()
            backoff = initial
            while True:
                envelope = wire.receive_json()
                if envelope.get("type") in {"hello", "warning"} and "envelope_id" not in envelope:
                    continue
                if envelope.get("type") == "disconnect" and "envelope_id" not in envelope:
                    raise SlackWireError("transport_error", f"Slack requested reconnect: {envelope.get('reason')}")
                envelope_id = envelope.get("envelope_id")
                if not isinstance(envelope_id, str) or not envelope_id:
                    raise SlackWireError("malformed_response", "Slack Socket Mode envelope has no envelope_id")
                _, record = persist_envelope(config, envelope)
                wire.send_json({"envelope_id": envelope_id})
                try:
                    process_pending_event(api, config, hub, record)
                except Exception as error:
                    record["attempts"] = require_int(record.get("attempts", 0), "Slack pending attempts", 0) + 1
                    safe_error = sanitize_external_error(error, config)
                    mark_pending(record, "retryable_failure", attempts=record["attempts"], last_error=safe_error, last_attempt_unix_ms=int(time.time() * 1000))
                    stderr(config, f"Slack intake envelope {envelope_id} retained after error: {safe_error}")
                retry_pending(api, config, hub)
        except Exception as error:
            stderr(config, f"Slack Socket Mode reconnecting after error: {error}")
            if wire is not None:
                wire.close()
            retry_pending(api, config, hub)
            time.sleep(backoff / 1000.0)
            backoff = min(backoff * 2, cap)


def resolve_tokens(config):
    if config["mode"] == "binding":
        return [load_binding_bot_token(config["binding"]["bot_token_file"])], None
    raw_bot = os.environ.get(config["transport"]["bot_token_env"])
    app = os.environ.get(config["transport"]["app_token_env"])
    if not isinstance(raw_bot, str):
        raise ValueError("Slack bot token is missing or malformed")
    bots = [token.strip() for token in raw_bot.split(",") if token.strip()]
    if not bots or any(re.fullmatch(r"xoxb-[0-9A-Za-z-]{15,4096}", token) is None for token in bots) or len(set(bots)) != len(bots):
        raise ValueError("Slack bot token list is missing, malformed, or duplicated")
    if not isinstance(app, str) or re.fullmatch(r"xapp-[0-9A-Za-z-]{15,4096}", app) is None:
        raise ValueError("Slack app token is missing or malformed")
    return bots, app


def main():
    global _CONFIG
    config = load_config()
    _CONFIG = config
    bot, app = resolve_tokens(config)
    api = SlackApi(config, bot, app)
    start_runtime_verdict_reader()
    authenticate(api, config)
    threading.Thread(target=outbound_loop, args=(api, config), daemon=True).start()
    signal_ready()
    if config["mode"] == "binding":
        connect_private_binding(config, api)
    else:
        gateway_loop(api, config)


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        stderr(_CONFIG, f"Slack interface exited: {error}")
        raise SystemExit(1)
