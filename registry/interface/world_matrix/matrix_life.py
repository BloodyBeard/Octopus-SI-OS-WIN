import asyncio
import base64
import hashlib
import hmac
import json
import mimetypes
import os
import queue
import re
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

from matrix_contract import DIGEST_RE
from matrix_contract import EVENT_ID_RE
from matrix_contract import ROOM_ID_RE
from matrix_contract import authored_interface_binding
from matrix_contract import binding_registry
from matrix_contract import canonical_json
from matrix_contract import event_content
from matrix_contract import event_key
from matrix_contract import event_reply
from matrix_contract import event_thread
from matrix_contract import mentioned
from matrix_contract import normalize_operation
from matrix_contract import normalized_event
from matrix_contract import require_event
from matrix_contract import require_int
from matrix_contract import require_interface_binding
from matrix_contract import require_keys
from matrix_contract import require_mxid
from matrix_contract import require_object
from matrix_contract import require_room
from matrix_contract import require_string
from matrix_contract import require_string_list
from matrix_contract import should_admit
from matrix_contract import strip_mention
from matrix_contract import strip_reply_fallback
from matrix_contract import validate_config
from matrix_contract import is_loopback_hostname
from matrix_contract import validate_proxy_url
from matrix_crypto import CryptoFailure
from matrix_wire import MatrixWire
from matrix_wire import MatrixWireFailure


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
PAYLOAD_DIR = os.path.join(VAR_DIR, "payloads")
RAW_EVENT_DIR = os.path.join(VAR_DIR, "raw-events")
RAW_SYNC_DIR = os.path.join(VAR_DIR, "raw-sync")
PENDING_DIR = os.path.join(VAR_DIR, "pending-events")
MEDIA_DIR = os.path.join(VAR_DIR, "media")
EFFECT_DIR = os.path.join(VAR_DIR, "outbound-effects")
STATE_PATH = os.path.join(VAR_DIR, "matrix-state.json")
SYNC_CURSOR_PATH = os.path.join(VAR_DIR, "sync-cursor.json")
LEDGER_CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
LEDGER_FALLBACK_CURSOR_PATH = os.path.join(VAR_DIR, "ledger-fallback-cursor.json")
STREAM_STATE_PATH = os.path.join(VAR_DIR, "stream-state.json")
IPC_STATE_PATH = os.path.join(VAR_DIR, "ipc-address.json")
BINDING_CONTEXT_PATH = os.path.join(VAR_DIR, "binding-context.json")
BINDING_EVENTS_PATH = os.path.join(VAR_DIR, "binding-events.json")
PRIVATE_DISPATCH_PATH = os.path.join(VAR_DIR, "private-dispatch.json")
BINDINGS_PATH = os.path.join(VAR_DIR, "private-bindings.json")
TOPOLOGY_OWNERSHIP_PATH = os.path.join(VAR_DIR, "topology-ownership.json")
SECRETS_DIR = os.path.join(VAR_DIR, "private-secrets")
IDENTITY_SALT_PATH = os.path.join(SECRETS_DIR, "identity-salt.key")
PRIVATE_REQUEST_WORKERS = 8
PRIVATE_REQUEST_QUEUE_LIMIT = 64
PRIVATE_REGISTRATION_LIMIT = 32
PRIVATE_REGISTRATION_MAX_BYTES = 16 * 1024
TOPOLOGY_MAX_BYTES = 16 * 1024 * 1024
TOPOLOGY_OWNERSHIP_MAX_PATHS = 100_000

os.umask(0o077)


class ArtifactDeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict
        super().__init__(f"runtime refused Matrix artifact: {verdict!r}")


def ensure_private_directory(path):
    path = os.path.abspath(path)
    os.makedirs(path, mode=0o700, exist_ok=True)
    current = os.lstat(path)
    if stat.S_ISLNK(current.st_mode) or not stat.S_ISDIR(current.st_mode):
        raise PermissionError(f"Matrix private directory is not a real directory: {path}")
    if current.st_uid != os.getuid():
        raise PermissionError(f"Matrix private directory has a different owner: {path}")
    if stat.S_IMODE(current.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


for directory in (VAR_DIR, PAYLOAD_DIR, RAW_EVENT_DIR, RAW_SYNC_DIR, PENDING_DIR, MEDIA_DIR, EFFECT_DIR, SECRETS_DIR):
    ensure_private_directory(directory)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_LEDGER_LOCK = threading.RLock()
_LEDGER_INDEX_WARNING = None
_AUTHORING_LOCK = threading.RLock()
_BINDING_EVENTS_LOCK = threading.RLock()
_CONFIG = None


def open_private_read(path):
    if not isinstance(path, str) or not os.path.isabs(path):
        raise PermissionError("Matrix private file path must be absolute")
    path = os.path.abspath(path)
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError(f"Matrix private path is not a regular file: {path}")
    if before.st_uid != os.getuid() or stat.S_IMODE(before.st_mode) != 0o600:
        raise PermissionError(f"Matrix private path must be owner-only 0600: {path}")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        current = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(current.st_mode) or current.st_uid != os.getuid() or stat.S_IMODE(current.st_mode) != 0o600:
            raise PermissionError(f"Matrix private path changed permissions: {path}")
        if stat.S_ISLNK(linked.st_mode) or (linked.st_dev, linked.st_ino) != (current.st_dev, current.st_ino) or (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise PermissionError(f"Matrix private path changed while opening: {path}")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def sensitive_values(config=None):
    values = set()
    if isinstance(config, dict):
        transport = config.get("transport") if isinstance(config.get("transport"), dict) else {}
        crypto = config.get("crypto") if isinstance(config.get("crypto"), dict) else {}
        for env_name in (transport.get("access_token_env"), transport.get("password_env"), transport.get("proxy_env"), crypto.get("recovery_key_env")):
            raw = os.environ.get(env_name, "") if isinstance(env_name, str) else ""
            if raw:
                values.add(raw)
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
    sanitized = re.sub(r"(?i)(authorization\s*[:=]\s*(?:bearer|basic)\s+)[^\s,;]+", r"\1[secret-redacted]", sanitized)
    sanitized = re.sub(r"((?:https?|socks(?:4|5h?)?)://)([^/@\s]+)@", r"\1[credentials-redacted]@", sanitized)
    return sanitized


def stderr(config, message):
    sys.stderr.write(sanitize_external_error(message, config) + "\n")
    sys.stderr.flush()


def load_config():
    value = json.loads(
        read_bounded(CONFIG_PATH, TOPOLOGY_MAX_BYTES, "Matrix interface config")
    )
    return validate_config(value, INTERFACE_ID)


def atomic_bytes(path, payload, mode=0o600):
    parent = os.path.dirname(path)
    ensure_private_directory(parent)
    temporary = os.path.join(parent, f".{os.path.basename(path)}.{os.getpid()}.{time.time_ns()}.tmp")
    descriptor = None
    try:
        descriptor = os.open(temporary, os.O_CREAT | os.O_EXCL | os.O_WRONLY, mode)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = None
            handle.write(payload)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, mode)
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


def read_json(path, name, missing=None):
    try:
        with open(path, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return missing
    return require_object(value, name)


def runtime_verdict_reader():
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
    threading.Thread(target=runtime_verdict_reader, daemon=True).start()


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
            raise RuntimeError(f"runtime verdict input failed before Matrix request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict) and isinstance(admitted.get("seq"), int) and not isinstance(admitted.get("seq"), bool):
        return admitted["seq"]
    raise ArtifactDeliveryRefused(verdict)


def materialize_artifact(label, payload, key):
    key = require_string(key, "Matrix artifact key", maximum=512)
    if not re.fullmatch(r"[0-9A-Za-z._-]+", key):
        raise ValueError("Matrix artifact key contains unsafe characters")
    payload_path = os.path.join(PAYLOAD_DIR, f"{key}.payload")
    manifest_path = os.path.join(PAYLOAD_DIR, f"{key}.artifact.json")
    atomic_bytes(payload_path, payload)
    atomic_json(manifest_path, {"label": label, "payload_path": payload_path})
    return manifest_path


def hand_json(label, value, key):
    value = sanitize_external_error(value, _CONFIG)
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hand_artifact(materialize_artifact(label, payload, key))


def materialized_artifact_id(manifest_path, config):
    manifest = require_keys(json.loads(read_bounded(manifest_path, 1048576, "Matrix materialized artifact manifest")), {"label", "payload_path"}, {"payload_content_hash"}, "Matrix materialized artifact manifest")
    label = require_string(manifest["label"], "Matrix materialized artifact label")
    payload_path = require_string(manifest["payload_path"], "Matrix materialized artifact payload_path")
    if not os.path.isabs(payload_path):
        raise ValueError("Matrix materialized artifact payload_path must be absolute")
    payload = read_bounded(payload_path, config["routing"]["ipc_max_frame_bytes"], "Matrix materialized artifact payload")
    payload_hash = octopus_content_hash(payload)
    declared = manifest.get("payload_content_hash")
    if declared is not None and declared != payload_hash:
        raise ValueError("Matrix materialized artifact payload hash mismatched")
    pinned = {"label": label, "payload_path": payload_path, "payload_content_hash": payload_hash}
    pinned_bytes = json.dumps(pinned, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return f"{label}:{octopus_content_hash(pinned_bytes)}"


def find_admitted_arrival_seq(artifact_id, config):
    artifact_id = require_string(artifact_id, "Matrix arrival artifact id")
    try:
        uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            if connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone() != ("3",):
                raise RuntimeError("Matrix ledger index has unsupported schema")
            row = connection.execute("SELECT seq FROM movements WHERE kind = 'InterfaceArrival' AND from_kind = 'interface' AND from_id = ? AND artifact_id = ? ORDER BY seq ASC LIMIT 1", (INTERFACE_ID, artifact_id)).fetchone()
        finally:
            connection.close()
        if row is not None:
            return require_int(row[0], "Matrix admitted arrival seq", 0)
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
                raise RuntimeError("Matrix movement lookup row exceeds configured ceiling")
            if not line.endswith(b"\n"):
                break
            movement = require_object(json.loads(line), "Matrix movement lookup row")
            if movement.get("kind") != "InterfaceArrival" or movement.get("from") != {"Interface": INTERFACE_ID}:
                continue
            artifact = movement.get("artifact")
            if isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                return require_int(movement.get("seq"), "Matrix admitted arrival seq", 0)
    return None


def hand_json_idempotent(label, value, key, config):
    value = sanitize_external_error(value, config)
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    manifest_path = materialize_artifact(label, payload, key)
    artifact_id = materialized_artifact_id(manifest_path, config)
    evidence = [{"interface_id": INTERFACE_ID, "artifact_id": artifact_id, "label": "matrix.arrival.idempotency", "bytes": list(hashlib.sha256(artifact_id.encode("utf-8")).digest())}]
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
    return hand_json(
        "matrix.failure",
        {"schema_version": 1, "source_movement_seq": source_seq, "failure_class": category, "details": sanitize_external_error(details, _CONFIG)},
        key,
    )


async def async_hand_json(label, value, key):
    return await asyncio.to_thread(hand_json, label, value, key)


def root_slots():
    try:
        value = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
    except (KeyError, json.JSONDecodeError) as error:
        raise RuntimeError("OCTOPUS_ROOT_SLOTS_JSON is absent or malformed") from error
    value = require_object(value, "OCTOPUS_ROOT_SLOTS_JSON")
    result = {}
    for name in ("registry_interface_root", "registry_ee_template_root", "registry_tool_root", "registry_worker_environment_kind_root", "run_records_root", "ee_instances_root"):
        path = value.get(name)
        if not isinstance(path, str) or not os.path.isabs(path):
            raise RuntimeError(f"OCTOPUS_ROOT_SLOTS_JSON has no absolute {name}")
        result[name] = os.path.realpath(path)
    return result


def ensure_gateway_run(config):
    template = require_object(config["gateway_run_template"], "Matrix gateway_run_template")
    run_id = require_string(template.get("id"), "Matrix gateway_run_template.id")
    path = os.path.join(root_slots()["run_records_root"], f"{run_id}.json")
    try:
        record = require_object(
            json.loads(
                read_bounded(
                    path,
                    config["transport"]["max_response_bytes"],
                    "Matrix gateway run",
                )
            ),
            "Matrix gateway run",
        )
    except FileNotFoundError as error:
        raise RuntimeError(f"Matrix gateway run {path!r} is not authored; provision gateway_run_template before enabling the interface") from error
    record = require_object(record, "Matrix gateway run")
    if record.get("id") != run_id or record.get("ee") != template.get("ee") or record.get("worker") != template.get("worker"):
        raise RuntimeError("Matrix gateway run identity conflicts with gateway_run_template")
    checkpoints = record.get("checkpoints")
    active = record.get("active_checkpoints")
    if not isinstance(checkpoints, list) or not isinstance(active, list):
        raise RuntimeError("Matrix gateway run has no checkpoint topology")
    target = authoring_target(config)
    inbound = any(
        isinstance(row, dict)
        and row.get("id") in active
        and require_interface_binding(row.get("interface_binding"), f"Matrix gateway checkpoint {row.get('id')}.interface_binding") == INTERFACE_ID
        and target in [
            require_interface_binding(binding, f"Matrix gateway checkpoint {row.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(row.get("outbound_interface_bindings", []))
        ]
        for row in checkpoints
    )
    outbound = any(
        isinstance(row, dict)
        and row.get("id") in active
        and require_interface_binding(row.get("interface_binding"), f"Matrix gateway checkpoint {row.get('id')}.interface_binding") == target
        and INTERFACE_ID in [
            require_interface_binding(binding, f"Matrix gateway checkpoint {row.get('id')}.outbound_interface_bindings[{index}]")
            for index, binding in enumerate(row.get("outbound_interface_bindings", []))
        ]
        for row in checkpoints
    )
    if not inbound or not outbound:
        raise RuntimeError("Matrix gateway run is not armed for the configured binding authoring mode")
    return path


def default_state():
    return {
        "schema_version": 1,
        "processed": [],
        "participated_threads": [],
        "event_threads": {},
        "status_reactions": {},
        "undecrypted": [],
    }


def load_state():
    value = read_json(STATE_PATH, "Matrix state", default_state())
    value = require_keys(value, {"schema_version", "processed", "participated_threads", "event_threads", "status_reactions", "undecrypted"}, set(), "Matrix state")
    if value["schema_version"] != 1:
        raise ValueError("Matrix state schema_version must be 1")
    for name in ("processed", "participated_threads", "undecrypted"):
        if not isinstance(value[name], list) or any(not isinstance(item, str) for item in value[name]):
            raise ValueError(f"Matrix state {name} must be a string array")
    for name in ("event_threads", "status_reactions"):
        if not isinstance(value[name], dict) or any(not isinstance(key, str) or not isinstance(item, str) for key, item in value[name].items()):
            raise ValueError(f"Matrix state {name} must be a string map")
    return value


def update_state(config, mutator):
    with _STATE_LOCK:
        value = load_state()
        mutator(value)
        retention = config["sync"]["dedup_retention"]
        value["processed"] = value["processed"][-retention:]
        value["participated_threads"] = value["participated_threads"][-retention:]
        value["undecrypted"] = value["undecrypted"][-config["crypto"]["max_undecrypted_events"]:]
        if len(value["event_threads"]) > retention:
            value["event_threads"] = dict(list(value["event_threads"].items())[-retention:])
        if len(value["status_reactions"]) > retention:
            value["status_reactions"] = dict(list(value["status_reactions"].items())[-retention:])
        atomic_json(STATE_PATH, value, indent=2)
        return value


def load_sync_cursor():
    value = read_json(SYNC_CURSOR_PATH, "Matrix sync cursor", None)
    if value is None:
        return None
    value = require_keys(value, {"schema_version", "next_batch", "committed_at_unix_ms"}, set(), "Matrix sync cursor")
    if value["schema_version"] != 1:
        raise ValueError("Matrix sync cursor schema_version must be 1")
    return require_string(value["next_batch"], "Matrix sync cursor.next_batch", maximum=8192)


def commit_sync_cursor(next_batch):
    atomic_json(SYNC_CURSOR_PATH, {"schema_version": 1, "next_batch": require_string(next_batch, "Matrix next_batch", maximum=8192), "committed_at_unix_ms": int(time.time() * 1000)}, indent=2)


def persist_sync(config, value):
    payload = canonical_json(value)
    digest = hashlib.sha256(payload).hexdigest()
    token = str(value.get("next_batch") or "")
    token_digest = hashlib.sha256(token.encode("utf-8")).hexdigest()[:20]
    path = os.path.join(RAW_SYNC_DIR, f"{int(time.time() * 1000)}-{token_digest}-{digest}.json")
    atomic_bytes(path, payload)
    rows = []
    for name in os.listdir(RAW_SYNC_DIR):
        candidate = os.path.join(RAW_SYNC_DIR, name)
        if os.path.isfile(candidate):
            rows.append((os.stat(candidate).st_mtime_ns, candidate))
    for _, candidate in sorted(rows)[:-config["sync"]["raw_sync_retention"]]:
        try:
            os.unlink(candidate)
        except FileNotFoundError:
            pass
    return {"raw_sync_path": path, "raw_sync_sha256": digest}


def expand_ephemeral(event):
    event = require_object(event, "Matrix event")
    kind = event.get("type")
    content = event_content(event)
    if kind == "m.typing":
        users = content.get("user_ids")
        if isinstance(users, list):
            result = []
            for user_id in users:
                if isinstance(user_id, str):
                    item = dict(event)
                    item["sender"] = user_id
                    item["content"] = {"user_ids": [user_id]}
                    result.append(item)
            return result
    if kind == "m.receipt":
        result = []
        for receipt_event_id, receipt_types in content.items():
            if not isinstance(receipt_types, dict):
                continue
            for receipt_type, users in receipt_types.items():
                if not isinstance(users, dict):
                    continue
                for user_id, receipt in users.items():
                    if isinstance(user_id, str) and isinstance(receipt, dict):
                        item = dict(event)
                        item["sender"] = user_id
                        item["content"] = {receipt_event_id: {receipt_type: {user_id: receipt}}}
                        item["receipt_event_id"] = receipt_event_id
                        item["receipt_type"] = receipt_type
                        result.append(item)
        return result
    return [event]


def pending_path(key):
    return os.path.join(PENDING_DIR, f"{key}.json")


def raw_event_path(key):
    return os.path.join(RAW_EVENT_DIR, f"{key}.json")


def pending_count():
    return sum(1 for name in os.listdir(PENDING_DIR) if name.endswith(".json"))


def mark_processed(config, keys):
    def mutate(state):
        for key in keys:
            if key not in state["processed"]:
                state["processed"].append(key)
    update_state(config, mutate)


def persist_event(config, event, phase, sync_source, sync_index):
    event = dict(require_object(event, "Matrix event"))
    event["_octopus_sync_source"] = sync_source
    event["_octopus_sync_index"] = sync_index
    if not event.get("type") and event_content(event).get("membership"):
        event["type"] = "m.room.member"
    event["membership"] = event.get("membership") or event_content(event).get("membership")
    key = event_key(event)
    state = load_state()
    if key in state["processed"]:
        return None
    raw = canonical_json(event)
    digest = hashlib.sha256(raw).hexdigest()
    path = raw_event_path(key)
    existing = None
    try:
        with open(path, "rb") as handle:
            existing = handle.read()
    except FileNotFoundError:
        atomic_bytes(path, raw)
    if existing is not None and existing != raw:
        raise RuntimeError(f"Matrix raw event {key} changed bytes")
    if phase == "initial" and config["sync"]["initial_backfill"] != "admit":
        mark_processed(config, [key])
        if config["sync"]["initial_backfill"] == "drop":
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
        return None
    record_path = pending_path(key)
    existing_record = read_json(record_path, "Matrix pending event", None)
    if existing_record is not None:
        if existing_record.get("raw_sha256") != digest or existing_record.get("raw_path") != path:
            raise RuntimeError(f"Matrix pending event {key} identity drifted")
        return existing_record
    if pending_count() >= config["sync"]["max_pending_events"]:
        raise RuntimeError("Matrix pending event ceiling reached")
    now = int(time.time() * 1000)
    delay = config["sync"]["text_batch_delay_ms"] if event.get("type") == "m.room.message" else 0
    record = {
        "schema_version": 1,
        "key": key,
        "raw_path": path,
        "raw_sha256": digest,
        "status": "pending",
        "phase": phase,
        "created_at_unix_ms": now,
        "available_after_unix_ms": now + delay,
        "attempts": 0,
    }
    atomic_json(record_path, record, indent=2)
    return record


def load_pending_record(path):
    value = read_json(path, "Matrix pending event")
    value = require_keys(
        value,
        {"schema_version", "key", "raw_path", "raw_sha256", "status", "phase", "created_at_unix_ms", "available_after_unix_ms", "attempts"},
        {"binding_request_seq", "last_error", "last_attempt_unix_ms", "invite_result"},
        "Matrix pending event",
    )
    if value["schema_version"] != 1:
        raise ValueError("Matrix pending event schema_version must be 1")
    require_string(value["key"], "Matrix pending event.key")
    require_string(value["raw_path"], "Matrix pending event.raw_path")
    if not DIGEST_RE.fullmatch(value["raw_sha256"]):
        raise ValueError("Matrix pending event raw digest is malformed")
    require_string(value["status"], "Matrix pending event.status")
    for key in ("created_at_unix_ms", "available_after_unix_ms", "attempts"):
        require_int(value[key], f"Matrix pending event.{key}", 0)
    return value


def update_pending(record, status, **updates):
    value = dict(record)
    value["status"] = status
    value.update(updates)
    atomic_json(pending_path(value["key"]), value, indent=2)
    return value


def pending_retry_at(config, attempts, now):
    exponent = min(max(attempts - 1, 0), 20)
    delay = min(config["sync"]["pending_retry_ms"] * (2 ** exponent), config["transport"]["reconnect_backoff_cap_ms"])
    return now + delay


def load_private_dispatch(config):
    try:
        value = json.loads(read_bounded(PRIVATE_DISPATCH_PATH, config["routing"]["ipc_max_frame_bytes"], "Matrix private dispatch journal"))
    except FileNotFoundError:
        return None
    value = require_keys(value, {"schema_version", "room_id", "thread_event_id", "events"}, set(), "Matrix private dispatch journal")
    if value["schema_version"] != 1:
        raise ValueError("Matrix private dispatch journal schema_version must be 1")
    require_room(value["room_id"], "Matrix private dispatch room_id")
    require_event(value["thread_event_id"], "Matrix private dispatch thread_event_id", nullable=True)
    events = value["events"]
    if not isinstance(events, list) or not events or len(events) > config["sync"]["max_pending_events"]:
        raise ValueError("Matrix private dispatch events must be a finite non-empty array")
    seen = set()
    normalized = []
    for index, event in enumerate(events):
        event = require_keys(event, {"key", "raw_sha256"}, set(), f"Matrix private dispatch.events[{index}]")
        key = require_string(event["key"], f"Matrix private dispatch.events[{index}].key", maximum=512)
        if key in seen:
            raise ValueError("Matrix private dispatch repeats an event key")
        seen.add(key)
        digest = require_string(event["raw_sha256"], f"Matrix private dispatch.events[{index}].raw_sha256")
        if not DIGEST_RE.fullmatch(digest):
            raise ValueError("Matrix private dispatch raw digest is malformed")
        normalized.append({"key": key, "raw_sha256": digest})
    return {"schema_version": 1, "room_id": value["room_id"], "thread_event_id": value["thread_event_id"], "events": normalized}


def private_dispatch_value(coordinate, records):
    return {
        "schema_version": 1,
        "room_id": coordinate[0],
        "thread_event_id": coordinate[1],
        "events": [{"key": record["key"], "raw_sha256": record["raw_sha256"]} for record in records],
    }


def pin_private_dispatch(config, coordinate, records):
    value = private_dispatch_value(coordinate, records)
    existing = load_private_dispatch(config)
    if existing is not None and existing != value:
        raise RuntimeError("Matrix private dispatch journal already pins a different batch")
    if existing is None:
        atomic_json(PRIVATE_DISPATCH_PATH, value, indent=2)


def clear_private_dispatch(config, coordinate, records):
    expected = private_dispatch_value(coordinate, records)
    existing = load_private_dispatch(config)
    if existing != expected:
        raise RuntimeError("Matrix private dispatch journal drifted before completion")
    os.unlink(PRIVATE_DISPATCH_PATH)


def load_raw_event(record):
    with open(record["raw_path"], "rb") as handle:
        payload = handle.read()
    if hashlib.sha256(payload).hexdigest() != record["raw_sha256"]:
        raise RuntimeError("Matrix raw event evidence digest mismatched")
    value = json.loads(payload)
    return require_object(value, "Matrix raw event evidence")


def resolve_coordinate(config, event, state):
    room_id = event.get("room_id")
    if not isinstance(room_id, str) or not ROOM_ID_RE.fullmatch(room_id):
        return None, None
    thread = event_thread(event)
    if thread:
        return room_id, thread
    related = event_reply(event)
    content = event_content(event)
    relation = content.get("m.relates_to") if isinstance(content.get("m.relates_to"), dict) else {}
    if relation.get("rel_type") in {"m.annotation", "m.replace"}:
        related = relation.get("event_id")
    if event.get("type") == "m.room.redaction":
        related = event.get("redacts") or content.get("redacts")
    if isinstance(related, str):
        mapped = state["event_threads"].get(related)
        if mapped:
            return room_id, mapped
    room = event.get("room") if isinstance(event.get("room"), dict) else {}
    direct = room.get("is_direct") is True
    if event.get("type") == "m.room.message" and ((direct and config["gating"]["auto_thread_dms"]) or (not direct and config["gating"]["auto_thread_rooms"])):
        event_id = event.get("event_id")
        if isinstance(event_id, str) and EVENT_ID_RE.fullmatch(event_id):
            return room_id, event_id
    return room_id, None


def event_text(event, own_user_id):
    content = event_content(event)
    if isinstance(content.get("m.new_content"), dict):
        content = content["m.new_content"]
    text = content.get("body")
    if not isinstance(text, str):
        return ""
    text = strip_reply_fallback(text)
    if mentioned(event, own_user_id):
        text = strip_mention(text, own_user_id)
    return text


def safe_filename(value, fallback):
    value = value if isinstance(value, str) else fallback
    value = os.path.basename(value).strip().replace("\x00", "")
    value = re.sub(r"[^0-9A-Za-z._ -]+", "_", value)[:255]
    return value or fallback


def media_content(event):
    content = event_content(event)
    msgtype = content.get("msgtype")
    if msgtype not in {"m.image", "m.audio", "m.video", "m.file"}:
        return None
    return content


async def localize_media(wire, config, event, key, private):
    content = media_content(event)
    if content is None:
        return []
    try:
        uri, payload = await wire.download_media(content)
    except Exception as error:
        return [{"ok": False, "error": sanitize_external_error(error, config), "event_key": key}]
    digest = hashlib.sha256(payload).hexdigest()
    info = content.get("info") if isinstance(content.get("info"), dict) else {}
    mimetype = info.get("mimetype") if isinstance(info.get("mimetype"), str) else None
    filename = safe_filename(content.get("filename") or content.get("body"), f"matrix-{digest[:16]}")
    row = {
        "ok": True,
        "mxc_uri": uri,
        "filename": filename,
        "mimetype": mimetype,
        "msgtype": content.get("msgtype"),
        "size": len(payload),
        "sha256": digest,
        "encrypted": isinstance(content.get("file"), dict),
        "voice": isinstance(content.get("org.matrix.msc3245.voice"), dict),
    }
    if private:
        row["data_base64"] = base64.b64encode(payload).decode("ascii")
    else:
        suffix = os.path.splitext(filename)[1][:20]
        path = os.path.join(MEDIA_DIR, f"{key}-{digest}{suffix}")
        atomic_bytes(path, payload)
        row["payload_path"] = path
    if len(payload) <= config["media"]["text_preview_bytes"]:
        extension = os.path.splitext(filename)[1].lower()
        if (mimetype or "").startswith("text/") or extension in config["media"]["inline_text_extensions"]:
            row["text_preview"] = payload.decode("utf-8", "replace")
    return [row]


async def reply_context(wire, event):
    reply_id = event_reply(event)
    room_id = event.get("room_id")
    if not reply_id or not isinstance(room_id, str):
        return None
    try:
        replied = await wire.fetch_event(room_id, reply_id)
        return {
            "event_id": reply_id,
            "sender": replied.get("sender"),
            "event_type": replied.get("type"),
            "text": event_text(replied, wire.user_id),
            "content": event_content(replied),
        }
    except Exception as error:
        return {"event_id": reply_id, "error": sanitize_external_error(error, _CONFIG)}


def combine_events(events):
    if len(events) == 1:
        return events[0]
    first = dict(events[0])
    first["event_keys"] = [row["event_key"] for row in events]
    first["event_ids"] = [row.get("event_id") for row in events]
    first["messages"] = events
    first["text"] = "\n\n".join(row.get("text", "") for row in events if row.get("text"))
    first["media"] = [item for row in events for item in row.get("media", [])]
    first["evidence"] = {"raw_events": [row["evidence"] for row in events]}
    first["batched"] = True
    return first


def read_frame(sock, maximum):
    header = bytearray()
    while len(header) < 4:
        chunk = sock.recv(4 - len(header))
        if not chunk:
            raise EOFError("Matrix IPC closed before frame header")
        header.extend(chunk)
    size = struct.unpack("!I", bytes(header))[0]
    if size <= 0 or size > maximum:
        raise ValueError("Matrix IPC frame size is invalid")
    body = bytearray()
    while len(body) < size:
        chunk = sock.recv(min(65536, size - len(body)))
        if not chunk:
            raise EOFError("Matrix IPC closed before frame body")
        body.extend(chunk)
    value = json.loads(body)
    return require_object(value, "Matrix IPC frame")


def write_frame(sock, value, maximum, lock=None):
    body = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    if not body or len(body) > maximum:
        raise ValueError("Matrix IPC frame exceeds configured ceiling")
    payload = struct.pack("!I", len(body)) + body
    if lock is None:
        sock.sendall(payload)
    else:
        with lock:
            sock.sendall(payload)


def load_binding_registry(config):
    rows = []
    try:
        rows.extend({key: binding[key] for key in ("interface", "room_id", "thread_event_id", "secret_sha256")} for binding in load_bindings()["bindings"].values())
    except FileNotFoundError:
        pass
    path = os.environ.get(config["routing"]["binding_registry_path_env"], "").strip()
    if path:
        external = binding_registry(
            json.loads(
                read_bounded(
                    path,
                    config["transport"]["max_response_bytes"],
                    "Matrix external binding registry",
                )
            )
        )
        for coordinate, row in external.items():
            rows.append({"interface": row["interface"], "room_id": coordinate[0], "thread_event_id": coordinate[1], "secret_sha256": row["secret_sha256"]})
    result = {}
    for row in rows:
        parsed = binding_registry({"schema_version": 1, "bindings": [row]})
        coordinate, declaration = next(iter(parsed.items()))
        if coordinate in result and result[coordinate] != declaration:
            raise ValueError("Matrix binding registry repeats coordinates with different declarations")
        result[coordinate] = declaration
    return result


def coordinate_key(room_id, thread_event_id):
    room_id = require_room(room_id, "Matrix binding room_id")
    if not ROOM_ID_RE.fullmatch(room_id):
        raise ValueError("Matrix binding room_id must be concrete")
    thread_event_id = require_event(thread_event_id, "Matrix binding thread_event_id", nullable=True)
    return f"{room_id}\n{thread_event_id or ''}"


def identity_salt():
    try:
        with open_private_read(IDENTITY_SALT_PATH) as handle:
            value = handle.read()
    except FileNotFoundError:
        value = secrets.token_bytes(32)
        atomic_bytes(IDENTITY_SALT_PATH, value)
    if len(value) != 32:
        raise RuntimeError("Matrix identity salt is malformed")
    return value


def secret_path(opaque_id):
    if not isinstance(opaque_id, str) or not re.fullmatch(r"[0-9a-f]{32}", opaque_id):
        raise ValueError("Matrix binding opaque id is malformed")
    root = os.path.realpath(SECRETS_DIR)
    path = os.path.abspath(os.path.join(root, f"{opaque_id}.key"))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("Matrix binding secret escaped its private directory")
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
        raise RuntimeError("Matrix binding secret is malformed")
    return path, value


def load_bindings():
    try:
        value = json.loads(
            read_bounded(BINDINGS_PATH, TOPOLOGY_MAX_BYTES, "Matrix private bindings")
        )
    except FileNotFoundError:
        return {"schema_version": 1, "bindings": {}}
    value = require_keys(value, {"schema_version", "bindings"}, set(), "Matrix private bindings")
    if value["schema_version"] != 1 or not isinstance(value["bindings"], dict):
        raise ValueError("Matrix private bindings are malformed")
    result = {"schema_version": 1, "bindings": {}}
    for key, binding in value["bindings"].items():
        binding = require_keys(binding, {"schema_version", "opaque_id", "room_id", "thread_event_id", "interface", "provider_interface", "interface_authoring_interface", "worker_environment_interface", "file_write_tool", "worker_interface", "run", "ee", "gateway_host", "gateway_port", "secret_sha256", "status"}, set(), f"Matrix private binding {key!r}")
        if binding["schema_version"] != 1 or coordinate_key(binding["room_id"], binding["thread_event_id"]) != key:
            raise ValueError(f"Matrix private binding {key!r} identity drifted")
        if not re.fullmatch(r"[0-9a-f]{32}", binding["opaque_id"]):
            raise ValueError(f"Matrix private binding {key!r} opaque id is malformed")
        registry_name(binding["worker_environment_interface"], f"private binding {key!r} worker environment interface")
        registry_name(binding["file_write_tool"], f"private binding {key!r} file write tool")
        if not is_loopback_hostname(binding["gateway_host"]):
            raise ValueError(f"Matrix private binding {key!r} gateway must be loopback")
        require_int(binding["gateway_port"], f"Matrix private binding {key!r}.gateway_port", 1, 65535)
        if not DIGEST_RE.fullmatch(binding["secret_sha256"]):
            raise ValueError(f"Matrix private binding {key!r} secret digest is malformed")
        if binding["status"] not in {"declared", "ready"}:
            raise ValueError(f"Matrix private binding {key!r} status is invalid")
        result["bindings"][key] = binding
    return result


def ensure_binding_record(config, room_id, thread_event_id, gateway_host, gateway_port):
    if not is_loopback_hostname(gateway_host):
        raise ValueError("Matrix private binding gateway must be loopback")
    gateway_port = require_int(gateway_port, "Matrix private binding gateway_port", 1, 65535)
    key = coordinate_key(room_id, thread_event_id)
    with _AUTHORING_LOCK:
        state = load_bindings()
        existing = state["bindings"].get(key)
        if existing is not None:
            _, secret = ensure_binding_secret(existing["opaque_id"])
            if not hmac.compare_digest(existing["secret_sha256"], hashlib.sha256(secret.encode("ascii")).hexdigest()):
                raise RuntimeError("Matrix private binding secret digest drifted")
            existing["gateway_host"] = gateway_host
            existing["gateway_port"] = gateway_port
            atomic_json(BINDINGS_PATH, state, indent=2)
            return existing
        opaque_id = hmac.new(identity_salt(), key.encode("utf-8"), hashlib.sha256).hexdigest()[:32]
        prefixes = config["private_run"]["id_prefixes"]
        _, secret = ensure_binding_secret(opaque_id)
        binding = {
            "schema_version": 1,
            "opaque_id": opaque_id,
            "room_id": room_id,
            "thread_event_id": thread_event_id,
            "interface": prefixes["matrix"] + opaque_id,
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
    key = coordinate_key(binding["room_id"], binding["thread_event_id"])
    with _AUTHORING_LOCK:
        state = load_bindings()
        current = state["bindings"].get(key)
        if current is None or current["interface"] != binding["interface"]:
            raise RuntimeError("Matrix private binding disappeared before activation")
        current["status"] = "ready"
        atomic_json(BINDINGS_PATH, state, indent=2)
        binding["status"] = "ready"


def rooted_path(root, *parts):
    root = os.path.realpath(root)
    path = os.path.realpath(os.path.join(root, *parts))
    if os.path.commonpath((path, root)) != root:
        raise ValueError("Matrix topology path escaped its granted root")
    return path


def json_text(value):
    return json.dumps(value, indent=2, ensure_ascii=False) + "\n"


def registry_name(value, label):
    value = require_string(value, label)
    if not re.fullmatch(r"[a-z0-9][a-z0-9._-]{0,191}", value):
        raise ValueError(f"Matrix {label} must be a registry-safe name")
    return value


def registry_json(root, owner, filename, label):
    owner = registry_name(owner, f"{label} owner")
    path = rooted_path(root, owner, filename)
    try:
        value = json.loads(read_bounded(path, TOPOLOGY_MAX_BYTES, label))
    except json.JSONDecodeError as error:
        raise ValueError(f"Matrix {label} is invalid JSON") from error
    return require_object(value, f"Matrix {label}")


def worker_role_artifact_bindings(worker, policy=None):
    worker = registry_name(worker, "exact worker interface")
    if policy is None:
        policy = registry_json(
            root_slots()["registry_interface_root"],
            worker,
            "policy.json",
            f"exact worker {worker!r} policy",
        )
    policy = require_object(policy, "Matrix exact worker policy")
    if policy.get("id") != worker:
        raise ValueError("Matrix exact worker policy identity does not match its registry folder")
    return [
        {"id": artifact}
        for artifact in require_string_list(
            policy.get("role_artifacts"),
            "Matrix exact worker policy.role_artifacts",
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
        raise ValueError("Matrix interface owner descriptor identity does not match its registry folder")
    if "reference" in descriptor:
        raise ValueError("Matrix interface owner descriptor cannot carry reference identity")
    if descriptor.get("interface_type") != interface_type or descriptor.get("role") != role:
        raise ValueError("Matrix interface owner descriptor classification does not match the requested concrete interface")
    if descriptor.get("direction") != "bidirectional":
        raise ValueError("Matrix interface owner descriptor must be bidirectional")
    sources = descriptor.get("source_names")
    if not isinstance(sources, list) or not sources:
        raise ValueError("Matrix interface owner descriptor source_names must be a non-empty list")
    normalized_sources = []
    for index, source in enumerate(sources):
        source = require_string(source, f"Matrix interface owner source_names[{index}]")
        if not re.fullmatch(r"[a-z0-9_-]+(?:\.[a-z0-9_-]+)*", source):
            raise ValueError("Matrix interface owner source_names entry is malformed")
        normalized_sources.append(source)
    if len(normalized_sources) != len(set(normalized_sources)):
        raise ValueError("Matrix interface owner source_names contains duplicates")
    if require_driver:
        require_string(descriptor.get("interface_driver_ref"), "Matrix interface owner interface_driver_ref")
    for field in ("interface_driver_config", "interface_policy"):
        if field in descriptor and not isinstance(descriptor[field], dict):
            raise ValueError(f"Matrix interface owner {field} must be a JSON object")
    descriptor = replace_exact_value(descriptor, owner, identifier)
    descriptor["id"] = identifier
    descriptor["source_names"] = normalized_sources
    return descriptor


def interface_owner_life(owner):
    owner = registry_name(owner, "interface owner")
    life = registry_json(root_slots()["registry_interface_root"], owner, "life.json", f"interface owner {owner!r} life")
    if not isinstance(life.get("processes"), list) or not life["processes"]:
        raise ValueError("Matrix interface owner life must declare processes")
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
        raise ValueError("Matrix tool owner descriptor identity drifted")
    supported = descriptor.get("supported_worker_environment_kinds")
    if not isinstance(supported, list) or worker_environment_kind not in supported:
        raise ValueError("Matrix tool owner does not support the configured worker environment kind")
    if not isinstance(descriptor.get("process"), dict) or not isinstance(descriptor.get("args_schema"), dict):
        raise ValueError("Matrix tool owner descriptor is incomplete")
    if descriptor.get("is_world_effect") is not True:
        raise ValueError("Matrix file-write tool owner must be a world-effect tool")
    descriptor = replace_exact_value(descriptor, interface_owner, interface)
    descriptor = replace_exact_value(descriptor, owner, identifier)
    descriptor["id"] = identifier
    descriptor["interface"] = interface
    return descriptor


def provider_descriptor(identifier, owner, private_config):
    descriptor = concrete_interface_descriptor(identifier, owner, "world", "provider")
    driver_config = descriptor.get("interface_driver_config", {})
    if not isinstance(driver_config, dict):
        raise ValueError("Matrix provider owner interface_driver_config must be an object")
    metadata = driver_config.get("private_config", {})
    if not isinstance(metadata, dict):
        raise ValueError("Matrix provider owner private_config must be an object")
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
        raise ValueError("Matrix worker policy identity does not match its registry folder")
    ceiling = require_object(policy.get("ceiling"), "Matrix worker policy ceiling")
    interfaces = ceiling.get("interfaces")
    if not isinstance(interfaces, list) or any(not isinstance(value, str) or not value for value in interfaces):
        raise ValueError("Matrix worker policy interfaces must be a string list")
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
        raise ValueError("Matrix worker policy tools must be a string list")
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
        raise ValueError("Matrix worker policy wek must be a string list")
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
        raise ValueError("Matrix EE template owner identity does not match its registry folder")
    if "reference" in template:
        raise ValueError("Matrix EE template owner cannot carry reference identity")
    template["id"] = identifier
    template["allowed_interfaces"] = list(dict.fromkeys(interfaces))
    return template


def child_life_json(secret_file):
    runtime_source = os.environ.get("OCTOPUS_CONTENT_HASH_RUNTIME")
    if not isinstance(runtime_source, str) or not os.path.isabs(runtime_source):
        raise RuntimeError("Matrix child topology requires the package-owned dependency runtime")
    runtime_source = os.path.realpath(runtime_source)
    evidence_source = os.path.realpath(os.path.join(os.path.dirname(runtime_source), "dependency-evidence.json"))
    return {
        "processes": [{
            "bin": "python3",
            "max_in_flight_tool_processes": 16,
            "io_limits": {"max_control_frame_bytes": 1048576, "max_stderr_bytes": 262144, "max_manifest_bytes": 1048576, "max_payload_bytes": 268435456},
            "args": [{"repository_file": "registry/interface/world_matrix/matrix_bootstrap.py"}],
            "required_files": ["registry/interface/world_matrix/matrix_bootstrap.py", "registry/interface/world_matrix/dependency_lock.json", "registry/interface/world_matrix/requirements-lock-darwin-arm64-py313.txt", "registry/interface/world_matrix/matrix_life.py", "registry/interface/world_matrix/matrix_contract.py", "registry/interface/world_matrix/matrix_wire.py", "registry/interface/world_matrix/matrix_crypto.py", "registry/interface/world_matrix/matrix_rendering.py", "registry/interface/world_matrix/octopus_content_hash.py", "registry/interface/world_matrix/requirements.txt", "registry/interface/world_matrix/requirements-e2ee.txt"],
            "env": {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source, "OCTOPUS_MATRIX_DEPENDENCY_EVIDENCE_SOURCE": evidence_source},
            "inherit_env": ["PATH"],
            "authority": {
                "read_root_slots": [], "write_root_slots": [], "executable_root_slots": [],
                "read_paths": [secret_file, runtime_source, evidence_source], "write_paths": [], "executable_paths": [runtime_source],
                "executable_commands": ["python3"], "read_env_paths": [], "write_env_paths": [], "executable_env_paths": [],
                "allow_network": False, "allow_local_ipc": True,
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
    child_config["binding"] = {"interface": interface, "room_id": binding["room_id"], "thread_event_id": binding["thread_event_id"], "run": run, "gateway_host": binding["gateway_host"], "gateway_port": binding["gateway_port"], "secret_file": secret_file}
    provider_private_config = {"model": private["model"], "reasoning_effort": private["reasoning_effort"], "context_output_labels": private["provider_context_output_labels"], "run_authoring_profile": {"file_write_tool": file_write_tool, "file_write_label": private["file_write_label"], "file_write_result_label": private["file_write_result_label"], "result_timeout_ms": private["result_timeout_ms"]}}
    provider_definition = provider_descriptor(provider, owners["provider"], provider_private_config)
    interface_replacements = {owners["matrix"]: interface, owners["provider"]: provider, owners["interface_authoring"]: authoring_source, owners["worker_environment"]: authoring, owners["worker"]: worker}
    tool_replacements = {private["file_write_tool"]: file_write_tool}
    worker_definition = concrete_interface_descriptor(worker, owners["worker"], "system", "worker", require_driver=False)
    worker_ceiling = worker_policy(worker, owners["worker"], interface_replacements, tool_replacements, exact_interfaces, [file_write_tool], [private["worker_environment_kind"]], private["role_artifacts"])
    ee_template_definition = concrete_ee_template(ee_template, private["ee_template_owner"], exact_interfaces)
    mandatory_roles = worker_role_artifact_bindings(worker, worker_ceiling)
    context = list(mandatory_roles)
    run_record = {
        "id": run, "ee": ee, "worker": worker, "trace": f"trace:{run}", "status": "running",
        "interface_state": {provider: {"context_generation": 0, "authoring_generation": 0, "frontier_checkpoint_ids": [f"{prefix}_matrix_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"]}},
        "checkpoints": [
            {"id": f"{prefix}_matrix_input", "run": run, "trace": f"trace:{run}:{prefix}_matrix_input", "epoch": 0, "interface_binding": authored_interface_binding(interface), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": context, "required_verified_artifacts": []},
            {"id": f"{prefix}_matrix_output", "run": run, "trace": f"trace:{run}:{prefix}_matrix_output", "epoch": 0, "interface_binding": authored_interface_binding(provider), "outbound_interface_bindings": [authored_interface_binding(interface)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_call", "run": run, "trace": f"trace:{run}:{prefix}_authoring_call", "epoch": 0, "interface_binding": authored_interface_binding(authoring_source), "outbound_interface_bindings": [authored_interface_binding(authoring)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
            {"id": f"{prefix}_authoring_result", "run": run, "trace": f"trace:{run}:{prefix}_authoring_result", "epoch": 0, "interface_binding": authored_interface_binding(authoring), "outbound_interface_bindings": [authored_interface_binding(provider)], "artifact_bindings": mandatory_roles, "required_verified_artifacts": []},
        ],
        "active_checkpoints": [f"{prefix}_matrix_input", f"{prefix}_matrix_output", f"{prefix}_authoring_call", f"{prefix}_authoring_result"],
    }
    activation = {"schema_version": 1, "interface": authoring_source, "activations": [{"activation_driver_ref": "app_server_transport", "config": {"bind_interface_socket_when_env_unset": True}}]}
    ee_instance = {"ee_id": ee, "template_name": ee_template, "parent_ee_id": private["parent_ee"], "goal": f"Serve the Matrix boundary bound to interface {interface}"}
    roots = root_slots()
    interface_root = roots["registry_interface_root"]
    topology = {
        rooted_path(interface_root, interface, "config.json"): json_text(child_config),
        rooted_path(interface_root, interface, "life.json"): json_text(child_life_json(secret_file)),
        rooted_path(interface_root, interface, "interface.json"): json_text(concrete_interface_descriptor(interface, owners["matrix"], "world", "service")),
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
    raw = read_bounded(path, TOPOLOGY_MAX_BYTES, label)
    try:
        value = json.loads(raw)
    except json.JSONDecodeError as error:
        raise ValueError(f"Matrix {label} is invalid JSON") from error
    return require_object(value, f"Matrix {label}"), raw


def union_string_field(value, field, additions, label):
    current = value.get(field)
    if not isinstance(current, list) or any(not isinstance(item, str) or not item for item in current):
        raise ValueError(f"Matrix {label}.{field} must be a string list")
    if len(current) != len(set(current)):
        raise ValueError(f"Matrix {label}.{field} contains duplicates")
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
            raise RuntimeError("Matrix private EE instance identity drifted")
        current_template = current_ee.get("template_name")
        if current_template not in {private["ee_template_owner"], ee_template}:
            raise RuntimeError("Matrix private EE instance template drifted")
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
            raise RuntimeError("Matrix private run identity drifted")
        exact_interfaces = {binding["interface"], binding["provider_interface"], binding["interface_authoring_interface"], binding["worker_environment_interface"], binding["worker_interface"]}
        checkpoints = current_run.get("checkpoints")
        if not isinstance(checkpoints, list):
            raise RuntimeError("Matrix private run checkpoint topology is malformed")
        for index, checkpoint in enumerate(checkpoints):
            if not isinstance(checkpoint, dict) or require_interface_binding(checkpoint.get("interface_binding"), f"Matrix private run checkpoints[{index}].interface_binding") not in exact_interfaces:
                raise RuntimeError("Matrix private run contains a stale or foreign interface binding")
            outbound = checkpoint.get("outbound_interface_bindings")
            if not isinstance(outbound, list) or any(
                require_interface_binding(binding, f"Matrix private run checkpoints[{index}].outbound_interface_bindings[{outbound_index}]") not in exact_interfaces
                for outbound_index, binding in enumerate(outbound)
            ):
                raise RuntimeError("Matrix private run contains a stale or foreign outbound interface binding")
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
        raise ValueError("Matrix parent EE instance identity drifted")
    parent_template_id = registry_name(parent_instance.get("template_name"), "parent EE template")
    parent_template_path = rooted_path(roots["registry_ee_template_root"], parent_template_id, "ee_template.json")
    parent_template, parent_template_raw = read_topology_json(parent_template_path, "parent EE template")
    if parent_template.get("id") != parent_template_id:
        raise ValueError("Matrix parent EE template identity drifted")
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


def gateway_rearm_write(config, opaque_id):
    path = ensure_gateway_run(config)
    current = read_bounded(
        path,
        config["transport"]["max_response_bytes"],
        "Matrix gateway run",
    )
    record = require_object(json.loads(current), "Matrix gateway run")
    run_id = config["gateway_run_template"]["id"]
    if record.get("id") != run_id or record.get("worker") != config["gateway_run_template"]["worker"]:
        raise RuntimeError("Matrix gateway run identity drifted")
    mandatory_roles = worker_role_artifact_bindings(record["worker"])
    interface_state = require_object(record.setdefault("interface_state", {}), "Matrix gateway interface_state")
    state = require_object(interface_state.setdefault(INTERFACE_ID, {}), "Matrix gateway interface state")
    generation = require_int(state.get("authoring_generation", 0), "Matrix gateway authoring_generation", 0) + 1
    state["authoring_generation"] = generation
    suffix = f"{opaque_id[:12]}_{generation}"
    call = f"cp_matrix_gateway_authoring_call_{suffix}"
    result = f"cp_matrix_gateway_authoring_result_{suffix}"
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
            existing = read_bounded(
                path,
                config["transport"]["max_response_bytes"],
                "Matrix topology document",
            )
        except FileNotFoundError:
            if path in ownership["paths"]:
                raise RuntimeError(f"Matrix package-authored topology target {path!r} disappeared")
            writes.append({"path": path, "content": content, "expected_before_hash": None})
            continue
        if existing == desired:
            continue
        observed = octopus_content_hash(existing)
        if ownership["paths"].get(path) != observed:
            raise RuntimeError(f"Matrix private topology target {path!r} drifted from its last package-authored bytes")
        writes.append({"path": path, "content": content, "expected_before_hash": observed})
    return writes


def load_topology_ownership():
    try:
        state = json.loads(read_bounded(TOPOLOGY_OWNERSHIP_PATH, TOPOLOGY_MAX_BYTES, "Matrix topology ownership"))
    except FileNotFoundError:
        return {"schema_version": 1, "paths": {}}
    state = require_keys(state, {"schema_version", "paths"}, set(), "Matrix topology ownership")
    if state["schema_version"] != 1:
        raise ValueError("Matrix topology ownership schema_version must be 1")
    paths = require_object(state["paths"], "Matrix topology ownership.paths")
    if len(paths) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("Matrix topology ownership has too many paths")
    for path, digest in paths.items():
        require_string(path, "Matrix topology ownership path")
        if not os.path.isabs(path):
            raise ValueError("Matrix topology ownership path must be absolute")
        if not isinstance(digest, str) or not DIGEST_RE.fullmatch(digest):
            raise ValueError("Matrix topology ownership digest is malformed")
    return state


def commit_topology_ownership(config, topology):
    ownership = load_topology_ownership()
    for path, content in topology.items():
        desired = content.encode("utf-8")
        existing = read_bounded(
            path,
            config["transport"]["max_response_bytes"],
            "Matrix topology document",
        )
        if existing != desired:
            raise RuntimeError(f"Matrix package-authored topology target {path!r} did not materialize exactly")
        ownership["paths"][path] = octopus_content_hash(existing)
    if len(ownership["paths"]) > TOPOLOGY_OWNERSHIP_MAX_PATHS:
        raise ValueError("Matrix topology ownership has too many paths")
    atomic_json(TOPOLOGY_OWNERSHIP_PATH, ownership, indent=2)


def package_owned_topology(topology):
    roots = root_slots()
    owned_roots = [roots["registry_interface_root"], roots["registry_ee_template_root"], roots["registry_tool_root"]]
    return {
        path: content
        for path, content in topology.items()
        if any(os.path.commonpath((os.path.realpath(path), root)) == root and os.path.realpath(path) != root for root in owned_roots)
    }


def ensure_private_binding(config, hub, room_id, thread_event_id):
    if config["routing"]["binding_authoring_mode"] != "automatic":
        return None
    with _AUTHORING_LOCK:
        binding = ensure_binding_record(config, room_id, thread_event_id, hub.host, hub.port)
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
                    {"tool": config["private_run"]["file_write_tool"], "arguments": {"writes": writes}, "correlation_id": f"matrix-binding-authoring:{binding['opaque_id']}:{time.time_ns()}"},
                    f"matrix-binding-authoring-{binding['opaque_id']}-{time.time_ns()}",
                )
            if not wait_for_shared_policy_admissions(
                config,
                binding,
                {write["path"]: write["expected_before_hash"] for write in shared_writes},
            ):
                if attempt + 1 < config["private_run"]["authoring_retry_attempts"]:
                    continue
                raise RuntimeError("Matrix exact parent and worker admissions could not be authored without overwriting concurrent state")
            try:
                hub.wait_for((room_id, thread_event_id), config["private_run"]["result_timeout_ms"])
            except TimeoutError:
                if attempt + 1 == config["private_run"]["authoring_retry_attempts"]:
                    raise
                continue
            break
        commit_topology_ownership(config, managed_topology)
        if binding["status"] != "ready":
            mark_binding_ready(binding)
        return binding


def reconcile_known_bindings(config, hub):
    with _AUTHORING_LOCK:
        coordinates = [
            (binding["room_id"], binding["thread_event_id"])
            for _, binding in sorted(load_bindings()["bindings"].items())
        ]
    for room_id, thread_event_id in coordinates:
        ensure_private_binding(config, hub, room_id, thread_event_id)


class JsonPeer:
    def __init__(self, sock, maximum, timeout_seconds, handler):
        self.sock = sock
        self.maximum = maximum
        self.timeout_seconds = timeout_seconds
        self.handler = handler
        self.write_lock = threading.Lock()
        self.condition = threading.Condition()
        self.responses = {}
        self.pending_requests = set()
        self.next_request_id = 1
        self.last_inbound_request_id = 0
        self.request_queue = queue.Queue(maxsize=PRIVATE_REQUEST_QUEUE_LIMIT)
        self.closed = threading.Event()
        self.error = None
        self.request_workers = [
            threading.Thread(target=self.request_worker, daemon=True)
            for _ in range(PRIVATE_REQUEST_WORKERS)
        ]
        for worker in self.request_workers:
            worker.start()
        self.reader = threading.Thread(target=self.read_loop, daemon=True)
        self.reader.start()

    def read_loop(self):
        try:
            while not self.closed.is_set():
                frame = read_frame(self.sock, self.maximum)
                frame_type = frame.get("type")
                request_id = frame.get("request_id")
                if frame_type == "response" and isinstance(request_id, int) and not isinstance(request_id, bool):
                    with self.condition:
                        if request_id in self.pending_requests:
                            if request_id in self.responses:
                                raise RuntimeError("Matrix IPC peer sent a duplicate response")
                            self.responses[request_id] = frame
                            self.condition.notify_all()
                        elif request_id >= self.next_request_id:
                            raise RuntimeError("Matrix IPC peer sent an unsolicited response")
                elif frame_type == "request" and isinstance(request_id, int) and not isinstance(request_id, bool):
                    if request_id <= self.last_inbound_request_id:
                        raise RuntimeError("Matrix IPC peer repeated or reordered a request id")
                    self.last_inbound_request_id = request_id
                    try:
                        self.request_queue.put_nowait((request_id, frame))
                    except queue.Full:
                        self.write_response(
                            {
                                "type": "response",
                                "request_id": request_id,
                                "ok": False,
                                "error": failure_details(
                                    RuntimeError("Matrix IPC request capacity is exhausted")
                                ),
                            }
                        )
                else:
                    raise RuntimeError("Matrix IPC peer sent an invalid frame")
        except Exception as error:
            self.error = error
            self.close()

    def request_worker(self):
        while not self.closed.is_set():
            try:
                request_id, frame = self.request_queue.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                self.handle_request(request_id, frame)
            finally:
                self.request_queue.task_done()

    def write_response(self, response):
        try:
            write_frame(self.sock, response, self.maximum, self.write_lock)
        except Exception as error:
            self.error = error
            self.close()

    def handle_request(self, request_id, frame):
        try:
            result = self.handler(frame.get("operation"), frame.get("payload"))
            response = {"type": "response", "request_id": request_id, "ok": True, "result": result}
        except Exception as error:
            response = {"type": "response", "request_id": request_id, "ok": False, "error": sanitize_external_error(error, _CONFIG)}
        self.write_response(response)

    def request(self, operation, payload):
        with self.write_lock:
            with self.condition:
                if len(self.pending_requests) >= PRIVATE_REQUEST_QUEUE_LIMIT:
                    raise RuntimeError("Matrix IPC outgoing request capacity is exhausted")
                request_id = self.next_request_id
                self.next_request_id += 1
                self.pending_requests.add(request_id)
            try:
                write_frame(
                    self.sock,
                    {"type": "request", "request_id": request_id, "operation": operation, "payload": payload},
                    self.maximum,
                )
            except Exception:
                with self.condition:
                    self.pending_requests.discard(request_id)
                self.close()
                raise
        deadline = time.monotonic() + self.timeout_seconds
        with self.condition:
            while request_id not in self.responses and not self.closed.is_set():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.pending_requests.discard(request_id)
                    self.responses.pop(request_id, None)
                    raise TimeoutError(f"Matrix IPC {operation} timed out")
                self.condition.wait(remaining)
            if request_id not in self.responses:
                self.pending_requests.discard(request_id)
                self.responses.pop(request_id, None)
                raise ConnectionError(f"Matrix IPC closed: {self.error}")
            response = self.responses.pop(request_id)
            self.pending_requests.discard(request_id)
        if response.get("ok") is not True:
            raise RuntimeError(f"Matrix IPC {operation} failed: {sanitize_external_error(response.get('error'), _CONFIG)}")
        return response.get("result")

    def close(self):
        if self.closed.is_set():
            return
        self.closed.set()
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass
        with self.condition:
            self.condition.notify_all()


class BindingEndpoint:
    def __init__(self, interface, room_id, thread_event_id, secret_sha256, peer):
        self.interface = interface
        self.room_id = room_id
        self.thread_event_id = thread_event_id
        self.secret_sha256 = secret_sha256
        self.peer = peer

    @property
    def coordinate(self):
        return self.room_id, self.thread_event_id


def enforce_binding_operation(config, endpoint, operation, args):
    if operation not in config["outbound"]["binding_allowed_operations"]:
        raise ValueError(f"Matrix operation {operation!r} is not permitted from a private binding")
    args = dict(require_object(args, "Matrix private operation args"))
    if operation not in {"presence.set", "device.keys.query", "device.verify", "account.data.get"}:
        room_id = args.get("room_id", endpoint.room_id)
        room_id = require_room(room_id, "Matrix private operation room_id")
        if room_id != endpoint.room_id:
            raise ValueError("Matrix private operation cannot leave its bound room")
        args["room_id"] = room_id
    thread = args.get("thread_event_id")
    if endpoint.thread_event_id is not None and operation in {"message.send", "message.edit", "media.send", "location.send"}:
        if thread is not None and thread != endpoint.thread_event_id:
            raise ValueError("Matrix private operation cannot leave its bound thread")
        args["thread_event_id"] = endpoint.thread_event_id
    if endpoint.thread_event_id is not None:
        state = load_state()
        target_keys = []
        if operation in {"message.edit", "message.redact", "reaction.send", "reaction.redact", "receipt.read"}:
            target_keys.append("event_id")
        if operation in {"message.send", "media.send", "location.send"} and args.get("reply_to_event_id") is not None:
            target_keys.append("reply_to_event_id")
        for key in target_keys:
            event_id = require_event(args.get(key), f"Matrix private operation {key}")
            if event_id != endpoint.thread_event_id and state["event_threads"].get(event_id) != endpoint.thread_event_id:
                raise ValueError("Matrix private operation target is outside its bound thread")
    return args


def binding_result_event_ids(result):
    result = require_object(result, "Matrix operation result")
    values = []
    candidates = [result]
    nested = result.get("result")
    if isinstance(nested, dict):
        candidates.append(nested)
    for candidate in candidates:
        for key in ("event_id", "message_id"):
            value = candidate.get(key)
            if isinstance(value, str) and EVENT_ID_RE.fullmatch(value):
                values.append(value)
        event_ids = candidate.get("event_ids")
        if isinstance(event_ids, list):
            values.extend(value for value in event_ids if isinstance(value, str) and EVENT_ID_RE.fullmatch(value))
    return list(dict.fromkeys(values))


def remember_thread_result(config, thread_event_id, result, participated=False):
    if thread_event_id is None:
        return
    event_ids = binding_result_event_ids(result)
    if not event_ids and not participated:
        return
    def mutate(state):
        for event_id in event_ids:
            state["event_threads"][event_id] = thread_event_id
        if participated and thread_event_id not in state["participated_threads"]:
            state["participated_threads"].append(thread_event_id)
    update_state(config, mutate)


def remember_binding_result(config, endpoint, result, operation=None):
    participated = config["gating"]["thread_sticky_after_bot_reply"] and operation in {"message.send", "media.send", "location.send"}
    remember_thread_result(config, endpoint.thread_event_id, result, participated)


def filter_binding_result(config, endpoint, operation, result):
    result = require_object(result, "Matrix private operation result")
    if endpoint.thread_event_id is None or operation != "room.history":
        return result
    events = result.get("events")
    if not isinstance(events, list):
        raise ValueError("Matrix room.history result has no event array")
    state = load_state()
    filtered = []
    for event in events:
        event = require_object(event, "Matrix room.history event")
        event_id = event.get("event_id")
        thread_id = event_thread(event)
        if isinstance(event_id, str) and thread_id is None:
            thread_id = state["event_threads"].get(event_id)
        if event_id == endpoint.thread_event_id or thread_id == endpoint.thread_event_id:
            filtered.append(event)
    result = dict(result)
    result["events"] = filtered
    result["thread_event_id"] = endpoint.thread_event_id
    return result


async def apply_status_reaction(config, wire, room_id, event_id, emoji, seed):
    status_key = f"{room_id}\n{event_id}"
    state = load_state()
    existing = state["status_reactions"].get(status_key)
    redaction_error = None
    if existing:
        existing_emoji, separator, reaction_event_id = existing.partition("|")
        if separator and existing_emoji == emoji:
            return {"disposition": "unchanged", "event_id": reaction_event_id, "emoji": emoji}
        if separator and EVENT_ID_RE.fullmatch(reaction_event_id):
            try:
                await wire.execute("reaction.redact", {"room_id": room_id, "event_id": reaction_event_id}, f"{seed}:status-redact")
            except Exception as error:
                redaction_error = failure_details(error)
    result = await wire.execute("reaction.send", {"room_id": room_id, "event_id": event_id, "key": emoji}, f"{seed}:status-reaction")
    if redaction_error is not None:
        result["previous_reaction_redaction_error"] = redaction_error
    def mutate(current):
        current["status_reactions"][status_key] = f"{emoji}|{result['event_id']}"
    update_state(config, mutate)
    return result


class BindingHub:
    def __init__(self, config, loop, wire):
        self.config = config
        self.loop = loop
        self.wire = wire
        self.maximum = config["routing"]["ipc_max_frame_bytes"]
        self.timeout_seconds = config["transport"]["request_timeout_ms"] / 1000.0
        self.condition = threading.Condition()
        self.endpoints = {}
        self.registration_slots = threading.BoundedSemaphore(PRIVATE_REGISTRATION_LIMIT)
        self.listener, self.host, self.port = self.bind_listener()
        threading.Thread(target=self.accept_loop, daemon=True).start()

    def bind_listener(self):
        configured_host = self.config["routing"]["ipc_listen"]
        configured_port = self.config["routing"]["ipc_port"]
        if not is_loopback_hostname(configured_host):
            raise ValueError("Matrix private IPC listener must be loopback")
        state = read_json(IPC_STATE_PATH, "Matrix IPC address", None)
        if state is not None:
            state = require_keys(state, {"schema_version", "host", "port"}, set(), "Matrix IPC address")
            if state["schema_version"] != 1:
                raise ValueError("Matrix IPC address schema_version must be 1")
            host = require_string(state["host"], "Matrix IPC address.host")
            port = require_int(state["port"], "Matrix IPC address.port", 1, 65535)
            if host != configured_host or port != configured_port:
                raise RuntimeError("Matrix durable IPC address disagrees with config")
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((configured_host, configured_port))
        listener.listen(128)
        actual_host, actual_port = listener.getsockname()
        atomic_json(IPC_STATE_PATH, {"schema_version": 1, "host": actual_host, "port": actual_port}, indent=2)
        return listener, actual_host, actual_port

    def accept_loop(self):
        while True:
            sock, _ = self.listener.accept()
            if not self.registration_slots.acquire(blocking=False):
                try:
                    sock.close()
                except OSError:
                    pass
                continue
            try:
                threading.Thread(target=self.register, args=(sock,), daemon=True).start()
            except Exception:
                self.registration_slots.release()
                try:
                    sock.close()
                except OSError:
                    pass
                raise

    def current_declaration(self, endpoint):
        row = load_binding_registry(self.config).get(endpoint.coordinate)
        if row is None or row["interface"] != endpoint.interface or row["secret_sha256"] != endpoint.secret_sha256:
            raise RuntimeError("Matrix private binding declaration is absent or changed")
        return row

    def handle_binding_request(self, endpoint, operation, payload):
        self.current_declaration(endpoint)
        if operation == "status.set":
            payload = require_keys(payload, {"emoji", "seed"}, {"event_id"}, "Matrix private status request")
            event_id = require_event(payload.get("event_id"), "Matrix private status event_id", nullable=True) or endpoint.thread_event_id
            if event_id is None:
                return {"disposition": "no_thread"}
            if endpoint.thread_event_id is not None:
                state = load_state()
                if event_id != endpoint.thread_event_id and state["event_threads"].get(event_id) != endpoint.thread_event_id:
                    raise ValueError("Matrix private status target is outside its bound thread")
            emoji = require_string(payload["emoji"], "Matrix private status emoji", maximum=64)
            seed = require_string(payload["seed"], "Matrix private status seed", maximum=1024)
            future = asyncio.run_coroutine_threadsafe(self.status_reaction(endpoint.room_id, event_id, emoji, seed), self.loop)
            result = future.result(self.timeout_seconds)
            remember_binding_result(self.config, endpoint, result)
            return result
        if operation != "effect.execute":
            raise ValueError(f"Matrix gateway does not accept private operation {operation!r}")
        payload = require_keys(payload, {"operation", "args", "seed"}, set(), "Matrix private effect request")
        effect_operation = require_string(payload["operation"], "Matrix private effect operation", maximum=128)
        seed = require_string(payload["seed"], "Matrix private effect seed", maximum=1024)
        args = enforce_binding_operation(self.config, endpoint, effect_operation, payload["args"])
        future = asyncio.run_coroutine_threadsafe(self.wire.execute(effect_operation, args, seed), self.loop)
        result = filter_binding_result(self.config, endpoint, effect_operation, future.result(self.timeout_seconds))
        remember_binding_result(self.config, endpoint, result, effect_operation)
        return result

    async def status_reaction(self, room_id, event_id, emoji, seed):
        return await apply_status_reaction(self.config, self.wire, room_id, event_id, emoji, seed)

    def register(self, sock):
        endpoint = None
        registration_released = False
        try:
            sock.settimeout(self.config["transport"]["connect_timeout_ms"] / 1000.0)
            registration_maximum = min(self.maximum, PRIVATE_REGISTRATION_MAX_BYTES)
            frame = read_frame(sock, registration_maximum)
            frame = require_keys(frame, {"type", "interface", "room_id", "thread_event_id", "secret"}, set(), "Matrix IPC registration")
            if frame["type"] != "register":
                raise ValueError("Matrix IPC first frame must register")
            interface = require_string(frame["interface"], "Matrix IPC interface", maximum=512)
            room_id = require_room(frame["room_id"], "Matrix IPC room_id")
            if not ROOM_ID_RE.fullmatch(room_id):
                raise ValueError("Matrix IPC room_id must be concrete")
            thread_id = require_event(frame["thread_event_id"], "Matrix IPC thread_event_id", nullable=True)
            secret = require_string(frame["secret"], "Matrix IPC secret", maximum=512)
            digest = hashlib.sha256(secret.encode("utf-8")).hexdigest()
            declaration = load_binding_registry(self.config).get((room_id, thread_id))
            if declaration is None or declaration["interface"] != interface or not hmac.compare_digest(declaration["secret_sha256"], digest):
                raise ValueError("Matrix IPC binding is not declared or its secret mismatched")
            write_frame(sock, {"type": "registered", "interface": interface}, registration_maximum)
            sock.settimeout(None)
            endpoint = BindingEndpoint(interface, room_id, thread_id, digest, None)
            peer = JsonPeer(sock, self.maximum, self.timeout_seconds, lambda operation, payload: self.handle_binding_request(endpoint, operation, payload))
            endpoint.peer = peer
            with self.condition:
                previous = self.endpoints.get(endpoint.coordinate)
                self.endpoints[endpoint.coordinate] = endpoint
                self.condition.notify_all()
            if previous is not None:
                previous.peer.close()
            self.registration_slots.release()
            registration_released = True
            peer.closed.wait()
        except Exception as error:
            stderr(self.config, f"Matrix IPC registration failed: {error}")
        finally:
            if not registration_released:
                self.registration_slots.release()
            if endpoint is not None:
                with self.condition:
                    if self.endpoints.get(endpoint.coordinate) is endpoint:
                        self.endpoints.pop(endpoint.coordinate, None)
                endpoint.peer.close()
            else:
                try:
                    sock.close()
                except OSError:
                    pass

    def connection(self, coordinate):
        with self.condition:
            endpoint = self.endpoints.get(coordinate)
        if endpoint is None or endpoint.peer.closed.is_set():
            return None
        try:
            self.current_declaration(endpoint)
        except Exception:
            endpoint.peer.close()
            with self.condition:
                if self.endpoints.get(coordinate) is endpoint:
                    self.endpoints.pop(coordinate, None)
            return None
        return endpoint

    def wait_for(self, coordinate, timeout_ms):
        deadline = time.monotonic() + timeout_ms / 1000.0
        with self.condition:
            while coordinate not in self.endpoints or self.endpoints[coordinate].peer.closed.is_set():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError(f"Matrix private binding {coordinate!r} did not activate")
                self.condition.wait(remaining)
            return self.endpoints[coordinate]

    def dispatch(self, coordinate, payload):
        endpoint = self.connection(coordinate)
        if endpoint is None:
            return None
        result = endpoint.peer.request("arrival", payload)
        if not isinstance(result, dict) or isinstance(result.get("admitted_seq"), bool) or not isinstance(result.get("admitted_seq"), int):
            raise RuntimeError("Matrix private binding returned malformed arrival evidence")
        return result["admitted_seq"]


def localize_binding_media(event, config):
    rows = event.get("media")
    if not isinstance(rows, list):
        raise ValueError("Matrix private event media must be an array")
    for index, row in enumerate(rows):
        row = require_object(row, f"Matrix private event media[{index}]")
        encoded = row.pop("data_base64", None)
        if encoded is None:
            continue
        encoded = require_string(encoded, f"Matrix private event media[{index}].data_base64", maximum=((config["media"]["max_download_bytes"] + 2) // 3) * 4 + 16)
        try:
            payload = base64.b64decode(encoded, validate=True)
        except Exception as error:
            raise ValueError("Matrix private media base64 is invalid") from error
        if len(payload) > config["media"]["max_download_bytes"]:
            raise ValueError("Matrix private media exceeds download ceiling")
        digest = hashlib.sha256(payload).hexdigest()
        if digest != row.get("sha256") or len(payload) != row.get("size"):
            raise ValueError("Matrix private media integrity mismatched")
        filename = safe_filename(row.get("filename"), f"matrix-{digest[:16]}")
        suffix = os.path.splitext(filename)[1][:20]
        path = os.path.join(MEDIA_DIR, f"{event['event_key']}-{index}-{digest}{suffix}")
        atomic_bytes(path, payload)
        row["payload_path"] = path
        if len(payload) <= config["media"]["text_preview_bytes"]:
            extension = os.path.splitext(filename)[1].lower()
            if str(row.get("mimetype") or "").startswith("text/") or extension in config["media"]["inline_text_extensions"]:
                row["text_preview"] = payload.decode("utf-8", "replace")


def validate_private_media_candidate(config, event, key, rows):
    if not isinstance(rows, list) or len(rows) > 1:
        raise ValueError("Matrix private candidate media must contain at most one row")
    content = media_content(event)
    if content is None:
        if rows:
            raise ValueError("Matrix private candidate attached media to a non-media event")
        return []
    if len(rows) != 1:
        raise ValueError("Matrix private candidate omitted its media result")
    row = require_object(rows[0], "Matrix private candidate media row")
    if row.get("ok") is False:
        row = require_keys(row, {"ok", "error", "event_key"}, set(), "Matrix private candidate media failure")
        if row["event_key"] != key:
            raise ValueError("Matrix private candidate media failure mismatched its event")
        return [{"ok": False, "error": require_string(row["error"], "Matrix private candidate media error", maximum=16384), "event_key": key}]
    row = require_keys(
        row,
        {"ok", "mxc_uri", "filename", "mimetype", "msgtype", "size", "sha256", "encrypted", "voice", "data_base64"},
        {"text_preview"},
        "Matrix private candidate media result",
    )
    if row["ok"] is not True:
        raise ValueError("Matrix private candidate media result has invalid status")
    descriptor = content.get("file") if isinstance(content.get("file"), dict) else None
    expected_uri = descriptor.get("url") if descriptor is not None else content.get("url")
    if row["mxc_uri"] != expected_uri:
        raise ValueError("Matrix private candidate media URI mismatched raw content")
    digest = require_string(row["sha256"], "Matrix private candidate media sha256")
    if not DIGEST_RE.fullmatch(digest):
        raise ValueError("Matrix private candidate media sha256 is malformed")
    size = require_int(row["size"], "Matrix private candidate media size", 0, config["media"]["max_download_bytes"])
    encoded = require_string(row["data_base64"], "Matrix private candidate media data_base64", maximum=((config["media"]["max_download_bytes"] + 2) // 3) * 4 + 16)
    try:
        payload = base64.b64decode(encoded, validate=True)
    except Exception as error:
        raise ValueError("Matrix private candidate media base64 is invalid") from error
    if len(payload) != size or hashlib.sha256(payload).hexdigest() != digest:
        raise ValueError("Matrix private candidate media bytes mismatched their evidence")
    info = content.get("info") if isinstance(content.get("info"), dict) else {}
    expected_mimetype = info.get("mimetype") if isinstance(info.get("mimetype"), str) else None
    expected_filename = safe_filename(content.get("filename") or content.get("body"), f"matrix-{digest[:16]}")
    if row["filename"] != expected_filename or row["mimetype"] != expected_mimetype or row["msgtype"] != content.get("msgtype"):
        raise ValueError("Matrix private candidate media metadata mismatched raw content")
    if row["encrypted"] is not isinstance(content.get("file"), dict) or row["voice"] is not isinstance(content.get("org.matrix.msc3245.voice"), dict):
        raise ValueError("Matrix private candidate media flags mismatched raw content")
    preview = None
    suffix = os.path.splitext(expected_filename)[1].lower()
    if len(payload) <= config["media"]["text_preview_bytes"] and ((expected_mimetype or "").startswith("text/") or suffix in config["media"]["inline_text_extensions"]):
        preview = payload.decode("utf-8", "replace")
    if row.get("text_preview") != preview or (preview is None and "text_preview" in row):
        raise ValueError("Matrix private candidate media preview mismatched its bytes")
    result = {
        "ok": True,
        "mxc_uri": expected_uri,
        "filename": expected_filename,
        "mimetype": expected_mimetype,
        "msgtype": content.get("msgtype"),
        "size": size,
        "sha256": digest,
        "encrypted": isinstance(content.get("file"), dict),
        "voice": isinstance(content.get("org.matrix.msc3245.voice"), dict),
        "data_base64": encoded,
    }
    if preview is not None:
        result["text_preview"] = preview
    return [result]


def validate_private_reply_context(event, value):
    reply_id = event_reply(event)
    if reply_id is None:
        if value is not None:
            raise ValueError("Matrix private candidate attached reply context to a non-reply event")
        return None
    value = require_object(value, "Matrix private candidate reply context")
    if "error" in value:
        value = require_keys(value, {"event_id", "error"}, set(), "Matrix private candidate reply failure")
        if value["event_id"] != reply_id:
            raise ValueError("Matrix private candidate reply failure mismatched its event")
        return {"event_id": reply_id, "error": require_string(value["error"], "Matrix private candidate reply error", maximum=16384)}
    value = require_keys(value, {"event_id", "sender", "event_type", "text", "content"}, set(), "Matrix private candidate reply context")
    if value["event_id"] != reply_id:
        raise ValueError("Matrix private candidate reply context mismatched its event")
    sender = value["sender"]
    if sender is not None:
        require_mxid(sender, "Matrix private candidate reply sender")
    event_type = value["event_type"]
    if event_type is not None:
        require_string(event_type, "Matrix private candidate reply event_type", maximum=255)
    text = require_string(value["text"], "Matrix private candidate reply text", allow_empty=True, maximum=1048576)
    content = require_object(value["content"], "Matrix private candidate reply content")
    return {"event_id": reply_id, "sender": sender, "event_type": event_type, "text": text, "content": content}


def validate_binding_candidate(config, candidate):
    candidate = require_keys(candidate, {"schema_version", "own_user_id", "records"}, set(), "Matrix private candidate")
    if candidate["schema_version"] != 1:
        raise ValueError("Matrix private candidate schema_version must be 1")
    own_user_id = require_mxid(candidate["own_user_id"], "Matrix private candidate own_user_id")
    records = candidate["records"]
    if not isinstance(records, list) or not records or len(records) > config["sync"]["max_pending_events"]:
        raise ValueError("Matrix private candidate records must be a finite non-empty array")
    state = load_state()
    binding = config["binding"]
    validated = []
    keys = set()
    for index, record in enumerate(records):
        record = require_keys(record, {"event_key", "raw_sha256", "event", "route_thread_event_id", "media", "reply_context"}, set(), f"Matrix private candidate.records[{index}]")
        key = require_string(record["event_key"], f"Matrix private candidate.records[{index}].event_key", maximum=512)
        if key in keys:
            raise ValueError("Matrix private candidate repeats an event key")
        keys.add(key)
        digest = require_string(record["raw_sha256"], f"Matrix private candidate.records[{index}].raw_sha256")
        if not DIGEST_RE.fullmatch(digest):
            raise ValueError("Matrix private candidate raw digest is malformed")
        event = require_object(record["event"], f"Matrix private candidate.records[{index}].event")
        raw = canonical_json(event)
        if hashlib.sha256(raw).hexdigest() != digest:
            raise ValueError("Matrix private candidate raw digest mismatched")
        if event_key(event) != key:
            raise ValueError("Matrix private candidate event key mismatched its event")
        admitted, reason = should_admit(config, event, own_user_id, set(state["participated_threads"]))
        if not admitted:
            raise ValueError(f"Matrix private candidate failed child admission: {reason}")
        route_thread = require_event(record["route_thread_event_id"], "Matrix private candidate route_thread_event_id", nullable=True)
        if event.get("room_id") != binding["room_id"] or route_thread != binding["thread_event_id"]:
            raise ValueError("Matrix private candidate crossed its bound coordinate")
        explicit_thread = event_thread(event)
        if explicit_thread is not None and explicit_thread != route_thread:
            raise ValueError("Matrix private candidate route contradicted its raw thread relation")
        media = validate_private_media_candidate(config, event, key, record["media"])
        reply = validate_private_reply_context(event, record["reply_context"])
        validated.append({"event_key": key, "raw_sha256": digest, "event": event, "raw": raw, "media": media, "reply_context": reply})
    return own_user_id, validated


def binding_payload(config, own_user_id, records):
    rows = []
    for source in records:
        evidence = {"raw_event_path": source["raw_path"], "raw_event_sha256": source["raw_sha256"], "raw_event": source["event"]}
        value = normalized_event(source["event"], evidence)
        value["text"] = event_text(source["event"], own_user_id)
        value["route_thread_event_id"] = config["binding"]["thread_event_id"]
        value["media"] = [dict(row) for row in source["media"]]
        value["reply_context"] = source["reply_context"]
        if source["event"].get("type") == "m.room.encrypted":
            value["decryption"] = source["event"].get("decryption") if isinstance(source["event"].get("decryption"), dict) else {"decrypted": False, "error": "encrypted event was not decrypted"}
        content = event_content(source["event"])
        if content.get("msgtype") == "m.location":
            value["location"] = {"geo_uri": content.get("geo_uri"), "description": content.get("body")}
        rows.append(value)
    return combine_events(rows)


def load_binding_events(config):
    try:
        value = json.loads(read_bounded(BINDING_EVENTS_PATH, config["routing"]["ipc_max_frame_bytes"], "Matrix binding event journal"))
    except FileNotFoundError:
        return {"schema_version": 1, "events": {}}
    value = require_keys(value, {"schema_version", "events"}, set(), "Matrix binding event journal")
    if value["schema_version"] != 1 or not isinstance(value["events"], dict):
        raise ValueError("Matrix binding event journal is malformed")
    if len(value["events"]) > config["sync"]["dedup_retention"]:
        raise ValueError("Matrix binding event journal exceeds its configured bound")
    for key, record in value["events"].items():
        require_string(key, "Matrix binding event journal key", maximum=512)
        record = require_keys(record, {"raw_sha256", "candidate_sha256", "payload_sha256", "payload_path", "raw_path", "admitted_seq"}, set(), f"Matrix binding event {key}")
        for field in ("raw_sha256", "candidate_sha256", "payload_sha256"):
            if not DIGEST_RE.fullmatch(require_string(record[field], f"Matrix binding event {key}.{field}")):
                raise ValueError(f"Matrix binding event {key}.{field} is malformed")
        for field, root in (("payload_path", PAYLOAD_DIR), ("raw_path", RAW_EVENT_DIR)):
            path = require_string(record[field], f"Matrix binding event {key}.{field}")
            if not os.path.isabs(path) or os.path.commonpath((os.path.realpath(path), os.path.realpath(root))) != os.path.realpath(root):
                raise ValueError(f"Matrix binding event {key}.{field} escaped private state")
        if record["admitted_seq"] is not None:
            require_int(record["admitted_seq"], f"Matrix binding event {key}.admitted_seq", 0)
    return value


def trim_binding_events(config, state):
    limit = config["sync"]["dedup_retention"]
    while len(state["events"]) > limit:
        oldest = next(iter(state["events"]))
        record = state["events"][oldest]
        if record["admitted_seq"] is None:
            raise RuntimeError("Matrix binding event journal cannot evict an uncommitted event")
        state["events"].pop(oldest)
        for field in ("payload_path", "raw_path"):
            path = record[field]
            if any(other[field] == path for other in state["events"].values()):
                continue
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass


def handle_binding_arrival(config, candidate):
    with _BINDING_EVENTS_LOCK:
        return handle_binding_arrival_locked(config, candidate)


def handle_binding_arrival_locked(config, candidate):
    own_user_id, records = validate_binding_candidate(config, candidate)
    keys = [record["event_key"] for record in records]
    batch_seed = "\0".join(keys)
    batch_id = hashlib.sha256(batch_seed.encode("utf-8")).hexdigest()
    candidate_sha256 = hashlib.sha256(canonical_json(candidate)).hexdigest()
    state = load_binding_events(config)
    recovered_seq = None
    existing = [state["events"].get(key) for key in keys]
    if any(record is not None for record in existing) and not all(record is not None for record in existing):
        raise RuntimeError("Matrix private candidate overlaps a previously admitted batch")
    if all(record is not None for record in existing):
        for source, record in zip(records, existing):
            if record["raw_sha256"] != source["raw_sha256"]:
                raise RuntimeError(f"Matrix private event {source['event_key']!r} was reused with different raw bytes")
        sequences = {record["admitted_seq"] for record in existing}
        if None not in sequences:
            if len(sequences) != 1:
                raise RuntimeError("Matrix private candidate receipt sequences disagree")
            recovered_seq = next(iter(sequences))
        payload_path = existing[0]["payload_path"]
        if any(record["payload_path"] != payload_path or record["payload_sha256"] != existing[0]["payload_sha256"] for record in existing):
            raise RuntimeError("Matrix private candidate payload receipts disagree")
        payload_bytes = read_bounded(payload_path, config["routing"]["ipc_max_frame_bytes"], "Matrix binding event payload")
        if hashlib.sha256(payload_bytes).hexdigest() != existing[0]["payload_sha256"]:
            raise RuntimeError("Matrix private candidate payload snapshot drifted")
    else:
        for source in records:
            raw_path = raw_event_path(source["event_key"])
            try:
                current = read_bounded(raw_path, config["routing"]["ipc_max_frame_bytes"], "Matrix binding raw event")
                if current != source["raw"]:
                    raise RuntimeError(f"Matrix private event {source['event_key']!r} raw snapshot drifted")
            except FileNotFoundError:
                atomic_bytes(raw_path, source["raw"])
            source["raw_path"] = raw_path
        payload_bytes = canonical_json(binding_payload(config, own_user_id, records))
        payload_sha256 = hashlib.sha256(payload_bytes).hexdigest()
        payload_path = os.path.join(PAYLOAD_DIR, f"binding-event-{batch_id}.json")
        atomic_bytes(payload_path, payload_bytes)
        for source in records:
            raw_path = source["raw_path"]
            state["events"][source["event_key"]] = {"raw_sha256": source["raw_sha256"], "candidate_sha256": candidate_sha256, "payload_sha256": payload_sha256, "payload_path": payload_path, "raw_path": raw_path, "admitted_seq": None}
        trim_binding_events(config, state)
        atomic_json(BINDING_EVENTS_PATH, state, indent=2)
    stable_payload = require_object(json.loads(payload_bytes), "Matrix binding event payload")
    if recovered_seq is None:
        if stable_payload.get("batched") is True:
            rows = stable_payload.get("messages")
            if not isinstance(rows, list):
                raise ValueError("Matrix binding batch lost its messages")
            for row in rows:
                localize_binding_media(row, config)
            stable_payload = combine_events(rows)
        else:
            localize_binding_media(stable_payload, config)
        label = "matrix.decryption.failed" if stable_payload.get("event_type") == "m.room.encrypted" else "matrix.event"
        runtime_seq = hand_json_idempotent(label, stable_payload, f"matrix-event-{batch_id[:32]}", config)
        for key in keys:
            state["events"][key]["admitted_seq"] = runtime_seq
        atomic_json(BINDING_EVENTS_PATH, state, indent=2)
    else:
        runtime_seq = recovered_seq
    event_ids = stable_payload.get("event_ids") if isinstance(stable_payload.get("event_ids"), list) else [stable_payload.get("event_id")]
    last_event_id = next((item for item in reversed(event_ids) if isinstance(item, str) and EVENT_ID_RE.fullmatch(item)), None)
    binding = config["binding"]
    atomic_json(BINDING_CONTEXT_PATH, {"schema_version": 1, "room_id": binding["room_id"], "thread_event_id": binding["thread_event_id"], "last_event_id": last_event_id, "updated_at_unix_ms": int(time.time() * 1000)}, indent=2)
    def mutate(current):
        for source in records:
            key = source["event_key"]
            if key not in current["processed"]:
                current["processed"].append(key)
            event = source["event"]
            event_id = event.get("event_id")
            if isinstance(event_id, str) and binding["thread_event_id"]:
                current["event_threads"][event_id] = binding["thread_event_id"]
            if event.get("type") == "m.room.encrypted" and key not in current["undecrypted"]:
                current["undecrypted"].append(key)
        if binding["thread_event_id"] and any(mentioned(source["event"], own_user_id) for source in records) and config["gating"]["thread_sticky_after_mention"] and binding["thread_event_id"] not in current["participated_threads"]:
            current["participated_threads"].append(binding["thread_event_id"])
    update_state(config, mutate)
    return runtime_seq


class BindingLink:
    def __init__(self, config):
        self.config = config
        self.maximum = config["routing"]["ipc_max_frame_bytes"]
        self.timeout_seconds = config["transport"]["request_timeout_ms"] / 1000.0
        self.condition = threading.Condition()
        self.peer = None
        self.closed = False
        threading.Thread(target=self.connect_loop, daemon=True).start()

    def secret(self):
        path = self.config["binding"]["secret_file"]
        if not is_loopback_hostname(self.config["binding"]["gateway_host"]):
            raise ValueError("Matrix private binding gateway must be loopback")
        with open_private_read(path) as handle:
            value = handle.read().decode("ascii").strip()
        if len(value) < 32 or len(value) > 512:
            raise ValueError("Matrix binding secret must contain 32..512 characters")
        return value

    def handle(self, operation, payload):
        if operation != "arrival":
            raise ValueError(f"Matrix private binding does not accept {operation!r}")
        return {"admitted_seq": handle_binding_arrival(self.config, payload)}

    def connect_loop(self):
        binding = self.config["binding"]
        delay = self.config["transport"]["reconnect_backoff_ms"]
        while not self.closed:
            peer = None
            try:
                sock = socket.create_connection((binding["gateway_host"], binding["gateway_port"]), self.config["transport"]["connect_timeout_ms"] / 1000.0)
                write_frame(
                    sock,
                    {"type": "register", "interface": INTERFACE_ID, "room_id": binding["room_id"], "thread_event_id": binding["thread_event_id"], "secret": self.secret()},
                    self.maximum,
                )
                registered = read_frame(sock, self.maximum)
                if registered != {"type": "registered", "interface": INTERFACE_ID}:
                    raise RuntimeError("Matrix gateway returned invalid registration")
                sock.settimeout(None)
                peer = JsonPeer(sock, self.maximum, self.timeout_seconds, self.handle)
                with self.condition:
                    self.peer = peer
                    self.condition.notify_all()
                delay = self.config["transport"]["reconnect_backoff_ms"]
                peer.closed.wait()
            except Exception as error:
                stderr(self.config, f"Matrix private IPC reconnecting after error: {error}")
            finally:
                with self.condition:
                    if self.peer is peer:
                        self.peer = None
                if peer is not None:
                    peer.close()
            time.sleep(delay / 1000.0)
            delay = min(delay * 2, self.config["transport"]["reconnect_backoff_cap_ms"])

    def request(self, operation, payload):
        deadline = time.monotonic() + self.config["transport"]["connect_timeout_ms"] / 1000.0
        with self.condition:
            while self.peer is None or self.peer.closed.is_set():
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("Matrix gateway IPC is unavailable")
                self.condition.wait(remaining)
            peer = self.peer
        return peer.request(operation, payload)


class MatrixService:
    def __init__(self, config):
        self.config = config
        self.loop = asyncio.get_running_loop()
        self.wire = MatrixWire(config, VAR_DIR, self.on_event)
        self.hub = None
        self.sync_source = ""
        self.sync_phase = "steady"
        self.sync_index = 0
        self.pending_lock = asyncio.Lock()
        self.outbound_executor = None

    async def on_event(self, event):
        for item in expand_ephemeral(event):
            index = self.sync_index
            self.sync_index += 1
            persist_event(self.config, item, self.sync_phase, self.sync_source, index)

    def begin_sync(self, since):
        self.sync_phase = "initial" if since is None else "steady"
        source = since if since is not None else "initial"
        self.sync_source = hashlib.sha256(source.encode("utf-8")).hexdigest()
        self.sync_index = 0

    async def connect(self):
        result = await self.wire.connect()
        self.hub = BindingHub(self.config, self.loop, self.wire)
        self.outbound_executor = GatewayExecutor(self)
        threading.Thread(target=outbound_loop, args=(self.config, self.outbound_executor), daemon=True).start()
        await async_hand_json("matrix.connection.ready", {"schema_version": 1, "diagnostics": result, "ipc": {"host": self.hub.host, "port": self.hub.port}}, f"matrix-ready-{int(time.time() * 1000)}")
        await asyncio.to_thread(reconcile_known_bindings, self.config, self.hub)

    async def execute_wire(self, operation, args, seed):
        return await self.wire.execute(operation, args, seed)

    async def status_reaction(self, room_id, event_id, emoji, seed):
        if not self.config["outbound"]["reactions"]["enabled"] or not event_id:
            return None
        return await apply_status_reaction(self.config, self.wire, room_id, event_id, emoji, seed)

    async def post_admission_effects(self, events, runtime_seq):
        last_message = None
        for event in events:
            if event.get("type") != "m.room.message":
                continue
            room_id = event.get("room_id")
            event_id = event.get("event_id")
            if not isinstance(room_id, str) or not isinstance(event_id, str):
                continue
            last_message = event
            try:
                receipt = await self.wire.execute("receipt.read", {"room_id": room_id, "event_id": event_id}, f"inbound:{event_key(event)}:receipt")
                await async_hand_json("matrix.receipt.result", {"schema_version": 1, "source_movement_seq": runtime_seq, "result": receipt}, f"matrix-receipt-{event_key(event)}")
            except Exception as error:
                await async_hand_json("matrix.failure", {"schema_version": 1, "source_movement_seq": runtime_seq, "failure_class": "receipt_failed", "details": {"error": sanitize_external_error(error, self.config), "event_id": event_id}}, f"matrix-receipt-failure-{event_key(event)}")
        if last_message is not None:
            room_id = last_message.get("room_id")
            event_id = last_message.get("event_id")
            try:
                result = await self.status_reaction(room_id, event_id, self.config["outbound"]["reactions"]["processing"], f"inbound:{event_key(last_message)}")
                if result is not None:
                    await async_hand_json("matrix.reaction.result", {"schema_version": 1, "source_movement_seq": runtime_seq, "status": "processing", "result": result}, f"matrix-processing-reaction-{event_key(last_message)}")
            except Exception as error:
                await async_hand_json("matrix.failure", {"schema_version": 1, "source_movement_seq": runtime_seq, "failure_class": "processing_reaction_failed", "details": {"error": sanitize_external_error(error, self.config), "event_id": event_id}}, f"matrix-processing-reaction-failure-{event_key(last_message)}")

    async def accept_invite(self, event, record):
        if event.get("type") != "m.room.member" or event.get("membership") != "invite":
            return record
        if event.get("state_key") != self.wire.user_id or not self.config["authz"]["auto_join_invites"]:
            return record
        if record.get("invite_result") is not None:
            return record
        room_id = require_room(event.get("room_id"), "Matrix invite room_id")
        result = await self.wire.execute("room.join", {"room_id": room_id}, f"inbound:{record['key']}:invite-join")
        await async_hand_json("matrix.invite.accepted", {"schema_version": 1, "event_key": record["key"], "room_id": room_id, "inviter": event.get("sender"), "result": result, "evidence": {"raw_event_path": record["raw_path"], "raw_event_sha256": record["raw_sha256"]}}, f"matrix-invite-accepted-{record['key']}")
        return update_pending(record, "invite_joined", invite_result=result, attempts=record["attempts"] + 1, last_attempt_unix_ms=int(time.time() * 1000))

    async def normalize_for_boundary(self, event, record, coordinate, private):
        evidence = {
            "raw_event_path": record["raw_path"],
            "raw_event_sha256": record["raw_sha256"],
            "raw_event": event,
        }
        value = normalized_event(event, evidence)
        value["text"] = event_text(event, self.wire.user_id)
        value["route_thread_event_id"] = coordinate[1]
        value["media"] = await localize_media(self.wire, self.config, event, record["key"], private)
        value["reply_context"] = await reply_context(self.wire, event)
        if event.get("type") == "m.room.encrypted":
            value["decryption"] = event.get("decryption") if isinstance(event.get("decryption"), dict) else {"decrypted": False, "error": "encrypted event was not decrypted"}
        content = event_content(event)
        if content.get("msgtype") == "m.location":
            value["location"] = {"geo_uri": content.get("geo_uri"), "description": content.get("body")}
        return value

    def privacy_required(self, coordinate, declarations):
        if coordinate[0] is None:
            return False
        if self.config["routing"]["private_scope"] == "all":
            return True
        return coordinate in declarations

    async def refuse_event(self, event, record, reason):
        await async_hand_json(
            "matrix.intake.refused",
            {
                "schema_version": 1,
                "event_key": record["key"],
                "reason": reason,
                "room_id": event.get("room_id"),
                "sender": event.get("sender"),
                "event_type": event.get("type"),
                "evidence": {"raw_event_path": record["raw_path"], "raw_event_sha256": record["raw_sha256"], "raw_event": event},
            },
            f"matrix-refused-{record['key']}",
        )
        mark_processed(self.config, [record["key"]])
        try:
            os.unlink(pending_path(record["key"]))
        except FileNotFoundError:
            pass

    async def process_group(self, records):
        state = load_state()
        live_records = []
        events = []
        for record in records:
            if record["key"] in state["processed"]:
                try:
                    os.unlink(pending_path(record["key"]))
                except FileNotFoundError:
                    pass
                continue
            event = load_raw_event(record)
            admitted, reason = should_admit(self.config, event, self.wire.user_id, set(state["participated_threads"]))
            if not admitted:
                await self.refuse_event(event, record, reason)
                continue
            record = await self.accept_invite(event, record)
            live_records.append(record)
            events.append(event)
        if not live_records:
            return True
        state = load_state()
        coordinates = [resolve_coordinate(self.config, event, state) for event in events]
        coordinate = coordinates[0]
        if any(item != coordinate for item in coordinates):
            raise RuntimeError("Matrix text batch crossed routing coordinates")
        declarations = load_binding_registry(self.config)
        private = self.privacy_required(coordinate, declarations)
        endpoint = self.hub.connection(coordinate) if private and self.hub is not None else None
        if private and endpoint is None and self.config["routing"]["unbound_policy"] == "retain" and self.config["routing"]["binding_authoring_mode"] == "automatic":
            if self.hub is None:
                raise RuntimeError("Matrix automatic private binding has no gateway hub")
            await asyncio.to_thread(ensure_private_binding, self.config, self.hub, coordinate[0], coordinate[1])
            endpoint = self.hub.connection(coordinate)
            if endpoint is None:
                raise RuntimeError("Matrix private binding activated without a live endpoint")
        if private and endpoint is None and self.config["routing"]["unbound_policy"] == "retain":
            first = live_records[0]
            request_seq = first.get("binding_request_seq")
            if request_seq is None:
                request_seq = await async_hand_json(
                    "matrix.binding.requested",
                    {
                        "schema_version": 1,
                        "event_keys": [record["key"] for record in live_records],
                        "room_id": coordinate[0],
                        "thread_event_id": coordinate[1],
                        "sender": events[0].get("sender"),
                        "event_type": events[0].get("type"),
                        "evidence": [{"raw_event_path": record["raw_path"], "raw_event_sha256": record["raw_sha256"]} for record in live_records],
                    },
                    f"matrix-binding-request-{first['key']}",
                )
            now = int(time.time() * 1000)
            for record in live_records:
                attempts = record["attempts"] + 1
                update_pending(record, "awaiting_binding", binding_request_seq=request_seq, attempts=attempts, last_attempt_unix_ms=now, available_after_unix_ms=pending_retry_at(self.config, attempts, now))
            return False
        if private and endpoint is None and self.config["routing"]["unbound_policy"] == "refuse":
            first = live_records[0]
            await async_hand_json(
                "matrix.binding.refused",
                {"schema_version": 1, "event_keys": [record["key"] for record in live_records], "room_id": coordinate[0], "thread_event_id": coordinate[1], "reason": "private_binding_missing", "evidence": [{"raw_event_path": record["raw_path"], "raw_event_sha256": record["raw_sha256"]} for record in live_records]},
                f"matrix-binding-refused-{first['key']}",
            )
            mark_processed(self.config, [record["key"] for record in live_records])
            for record in live_records:
                try:
                    os.unlink(pending_path(record["key"]))
                except FileNotFoundError:
                    pass
            return True
        normalized = []
        pinned_dispatch = private and endpoint is not None
        if pinned_dispatch:
            pin_private_dispatch(self.config, coordinate, live_records)
        for event, record in zip(events, live_records):
            normalized.append(await self.normalize_for_boundary(event, record, coordinate, private and endpoint is not None))
        payload = combine_events(normalized)
        if private and endpoint is not None:
            candidate = {
                "schema_version": 1,
                "own_user_id": self.wire.user_id,
                "records": [
                    {
                        "event_key": record["key"],
                        "raw_sha256": record["raw_sha256"],
                        "event": event,
                        "route_thread_event_id": coordinate[1],
                        "media": value["media"],
                        "reply_context": value["reply_context"],
                    }
                    for event, record, value in zip(events, live_records, normalized)
                ],
            }
            runtime_seq = await asyncio.to_thread(self.hub.dispatch, coordinate, candidate)
            if runtime_seq is None:
                raise RuntimeError("Matrix private binding disappeared during dispatch")
        else:
            label = "matrix.decryption.failed" if payload.get("event_type") == "m.room.encrypted" else "matrix.event"
            runtime_seq = await async_hand_json(label, payload, f"matrix-event-{live_records[0]['key']}")
        keys = [record["key"] for record in live_records]
        def mutate(current):
            for key in keys:
                if key not in current["processed"]:
                    current["processed"].append(key)
            for event in events:
                event_id = event.get("event_id")
                if isinstance(event_id, str) and coordinate[1]:
                    current["event_threads"][event_id] = coordinate[1]
                if event.get("type") == "m.room.encrypted":
                    key = event_key(event)
                    if key not in current["undecrypted"]:
                        current["undecrypted"].append(key)
            if coordinate[1] and any(mentioned(event, self.wire.user_id) for event in events) and self.config["gating"]["thread_sticky_after_mention"] and coordinate[1] not in current["participated_threads"]:
                current["participated_threads"].append(coordinate[1])
        update_state(self.config, mutate)
        for record in live_records:
            try:
                os.unlink(pending_path(record["key"]))
            except FileNotFoundError:
                pass
        await self.post_admission_effects(events, runtime_seq)
        if pinned_dispatch:
            clear_private_dispatch(self.config, coordinate, live_records)
        return True

    def group_pending(self, records, now):
        state = load_state()
        buckets = []
        dispatch = load_private_dispatch(self.config)
        if dispatch is not None:
            dispatch_keys = [row["key"] for row in dispatch["events"]]
            if all(key in state["processed"] for key in dispatch_keys):
                try:
                    os.unlink(PRIVATE_DISPATCH_PATH)
                except FileNotFoundError:
                    pass
            else:
                by_key = {record["key"]: record for record in records}
                pinned = []
                for row in dispatch["events"]:
                    record = by_key.get(row["key"])
                    if record is None or record["raw_sha256"] != row["raw_sha256"]:
                        raise RuntimeError("Matrix private dispatch journal cannot reconstruct its pending batch")
                    pinned.append(record)
                buckets.append(pinned)
                records = [record for record in records if record["key"] not in set(dispatch_keys)]
        text_groups = {}
        for record in records:
            event = load_raw_event(record)
            text = event_text(event, self.wire.user_id)
            content = event_content(event)
            relation = content.get("m.relates_to")
            batchable = event.get("type") == "m.room.message" and content.get("msgtype") in {"m.text", "m.notice"} and bool(text) and not (isinstance(relation, dict) and relation.get("rel_type") == "m.replace")
            if not batchable:
                buckets.append([record])
                continue
            coordinate = resolve_coordinate(self.config, event, state)
            group_key = (coordinate, event.get("sender"))
            text_groups.setdefault(group_key, []).append((record, len(text)))
        threshold = self.config["sync"]["text_batch_split_threshold"]
        split_delay = self.config["sync"]["text_batch_split_delay_ms"]
        for rows in text_groups.values():
            rows.sort(key=lambda item: item[0]["created_at_unix_ms"])
            current = []
            size = 0
            previous = None
            for record, length in rows:
                gap = record["created_at_unix_ms"] - previous if previous is not None else 0
                if current and (size + length > threshold or gap > split_delay):
                    buckets.append(current)
                    current = []
                    size = 0
                current.append(record)
                size += length
                previous = record["created_at_unix_ms"]
            if current:
                buckets.append(current)
        result = []
        for group in buckets:
            if max(record["available_after_unix_ms"] for record in group) <= now:
                result.append(group)
        result.sort(key=lambda group: min(record["created_at_unix_ms"] for record in group))
        return result

    async def retry_pending(self):
        async with self.pending_lock:
            records = []
            for name in sorted(os.listdir(PENDING_DIR)):
                if name.endswith(".json"):
                    records.append(load_pending_record(os.path.join(PENDING_DIR, name)))
            for group in self.group_pending(records, int(time.time() * 1000)):
                try:
                    await self.process_group(group)
                except Exception as error:
                    now = int(time.time() * 1000)
                    for record in group:
                        attempts = record["attempts"] + 1
                        update_pending(record, "retryable_failure", attempts=attempts, last_error=sanitize_external_error(error, self.config), last_attempt_unix_ms=now, available_after_unix_ms=pending_retry_at(self.config, attempts, now))
                    stderr(self.config, f"Matrix pending group retained after error: {error}")

    async def pending_loop(self):
        while True:
            await self.retry_pending()
            await asyncio.sleep(self.config["sync"]["pending_retry_ms"] / 1000.0)

    async def sync_loop(self):
        since = load_sync_cursor()
        delay = self.config["transport"]["reconnect_backoff_ms"]
        cap = self.config["transport"]["reconnect_backoff_cap_ms"]
        while True:
            try:
                self.begin_sync(since)
                value = await self.wire.sync(since, full_state=since is None)
                persist_sync(self.config, value)
                next_batch = require_string(value["next_batch"], "Matrix next_batch", maximum=8192)
                commit_sync_cursor(next_batch)
                since = next_batch
                delay = self.config["transport"]["reconnect_backoff_ms"]
            except Exception as error:
                details = error.evidence() if isinstance(error, MatrixWireFailure) else {"category": getattr(error, "category", "sync_failed"), "message": str(error), "details": getattr(error, "details", {})}
                details = sanitize_external_error(details, self.config)
                try:
                    await async_hand_json("matrix.sync.failure", {"schema_version": 1, "since_present": since is not None, "details": details}, f"matrix-sync-failure-{int(time.time() * 1000)}")
                except Exception:
                    pass
                await asyncio.sleep(delay / 1000.0)
                delay = min(delay * 2, cap)

    async def run(self):
        await self.connect()
        await asyncio.gather(self.pending_loop(), self.sync_loop())


def load_ledger_cursor():
    value = read_json(LEDGER_CURSOR_PATH, "Matrix ledger cursor", None)
    if value is None:
        return 0
    value = require_keys(value, {"schema_version", "next_seq"}, set(), "Matrix ledger cursor")
    if value["schema_version"] != 1:
        raise ValueError("Matrix ledger cursor schema_version must be 1")
    return require_int(value["next_seq"], "Matrix ledger cursor.next_seq", 0)


def commit_ledger_cursor(next_seq):
    atomic_json(LEDGER_CURSOR_PATH, {"schema_version": 1, "next_seq": require_int(next_seq, "Matrix next ledger seq", 0)}, indent=2)


def load_ledger_fallback_cursor():
    value = read_json(LEDGER_FALLBACK_CURSOR_PATH, "Matrix ledger fallback cursor", None)
    if value is None:
        return None
    value = require_keys(value, {"schema_version", "device", "inode", "byte_offset", "observed_size"}, set(), "Matrix ledger fallback cursor")
    if value["schema_version"] != 1:
        raise ValueError("Matrix ledger fallback cursor schema_version must be 1")
    for key in ("device", "inode", "byte_offset", "observed_size"):
        require_int(value[key], f"Matrix ledger fallback cursor.{key}", 0)
    if value["byte_offset"] > value["observed_size"]:
        raise ValueError("Matrix ledger fallback cursor is beyond its observed size")
    return value


def commit_ledger_fallback_cursor(status, byte_offset):
    byte_offset = require_int(byte_offset, "Matrix ledger fallback byte offset", 0, status.st_size)
    atomic_json(
        LEDGER_FALLBACK_CURSOR_PATH,
        {
            "schema_version": 1,
            "device": require_int(status.st_dev, "Matrix ledger device", 0),
            "inode": require_int(status.st_ino, "Matrix ledger inode", 0),
            "byte_offset": byte_offset,
            "observed_size": require_int(status.st_size, "Matrix ledger size", byte_offset),
        },
        indent=2,
    )


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 or item > 255 for item in content):
            raise ValueError("Matrix artifact content list is not a 32-byte hash")
        content = bytes(content)
    if not isinstance(content, bytes) or len(content) != 32:
        raise ValueError("Matrix artifact content must be a 32-byte hash")
    return content.hex()


def read_bounded(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ValueError(f"{name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ValueError(f"{name} changed while opening")
        if opened.st_size > maximum:
            raise ValueError(f"{name} exceeds configured byte ceiling")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            payload = handle.read(maximum + 1)
        if len(payload) > maximum:
            raise ValueError(f"{name} grew above configured byte ceiling")
        return payload
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def open_regular_file_stable(path, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise RuntimeError(f"{name} changed while opening")
        return os.fdopen(descriptor, "rb")
    except Exception:
        os.close(descriptor)
        raise


def materialize_movement_payload(content, manifest_maximum, payload_maximum):
    digest = content_hexdigest(content)
    try:
        manifest_bytes = read_bounded(os.path.join(BLOBS_DIR, digest), manifest_maximum, "Matrix cached artifact manifest")
        if octopus_content_hash(manifest_bytes) != digest:
            raise ValueError("Matrix cached artifact manifest hash mismatch")
        manifest = json.loads(manifest_bytes)
    except json.JSONDecodeError as error:
        raise ValueError("Matrix cached artifact manifest is invalid JSON") from error
    manifest = require_object(manifest, "Matrix cached artifact manifest")
    label = require_string(manifest.get("label"), "Matrix cached artifact label")
    payload_digest = manifest.get("payload_content_hash")
    if not isinstance(payload_digest, str) or not DIGEST_RE.fullmatch(payload_digest):
        raise ValueError("Matrix cached artifact manifest has no payload hash")
    payload = read_bounded(os.path.join(BLOBS_DIR, payload_digest), payload_maximum, "Matrix outbound payload")
    if octopus_content_hash(payload) != payload_digest:
        raise ValueError("Matrix cached artifact payload hash mismatch")
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
        "Matrix outbound movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "Matrix outbound movement runtime metadata",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"Matrix outbound movement runtime.{field}",
        )
    require_int(runtime.get("epoch"), "Matrix outbound movement runtime.epoch", 0)
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("Matrix outbound runtime interface does not match its exact target")
    return runtime


def read_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        schema = connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone()
        if schema != ("3",):
            raise RuntimeError("Matrix ledger index schema is unsupported")
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
        seq = require_int(seq, "Matrix ledger index seq", 0)
        require_string(source_interface, f"Matrix ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not DIGEST_RE.fullmatch(content)):
            raise RuntimeError(f"Matrix ledger index movement {seq} has malformed artifact hash")
        require_string(run, f"Matrix ledger index movement {seq}.run")
        require_string(checkpoint, f"Matrix ledger index movement {seq}.checkpoint")
        result.append((seq, bytes.fromhex(content) if content else None, run))
    return result


def read_ledger(next_seq, limit, max_row_bytes):
    next_seq = require_int(next_seq, "Matrix ledger next_seq", 0)
    limit = require_int(limit, "Matrix ledger read limit", 1, 1000000)
    max_row_bytes = require_int(max_row_bytes, "Matrix ledger.max_row_bytes", 256, 16777216)
    with _LEDGER_LOCK:
        try:
            handle = open_regular_file_stable(MOVEMENTS_JSONL, "Matrix movement ledger")
        except FileNotFoundError:
            return []
        result = []
        with handle:
            status = os.fstat(handle.fileno())
            cursor = load_ledger_fallback_cursor()
            if cursor is None or cursor["device"] != status.st_dev or cursor["inode"] != status.st_ino or cursor["byte_offset"] > status.st_size:
                byte_offset = 0
            else:
                byte_offset = cursor["byte_offset"]
            handle.seek(byte_offset)
            safe_offset = byte_offset
            has_uncommitted = False
            while True:
                row_offset = handle.tell()
                line = handle.readline(max_row_bytes + 1)
                if not line:
                    if not has_uncommitted:
                        safe_offset = handle.tell()
                    break
                if len(line) > max_row_bytes:
                    raise RuntimeError(f"Matrix ledger row at byte {row_offset} exceeds {max_row_bytes} bytes")
                if not line.endswith(b"\n"):
                    raise RuntimeError(f"Matrix ledger row at byte {row_offset} is partial")
                movement = require_object(json.loads(line), "Matrix ledger movement")
                seq = require_int(movement.get("seq"), "Matrix ledger movement.seq", 0)
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
            status = os.fstat(handle.fileno())
            commit_ledger_fallback_cursor(status, safe_offset)
        return result


def read_outbound(next_seq, limit=128, config=None):
    global _LEDGER_INDEX_WARNING
    config = _CONFIG if config is None else config
    if config is None:
        config = load_config()
    try:
        return read_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = sanitize_external_error(error, _CONFIG)
        if message != _LEDGER_INDEX_WARNING:
            stderr(_CONFIG, f"Matrix ledger index unavailable; reading ledger truth: {message}")
            _LEDGER_INDEX_WARNING = message
        return read_ledger(next_seq, limit, config["ledger"]["max_row_bytes"])


def effect_path(source_seq):
    return os.path.join(EFFECT_DIR, f"{source_seq}.json")


def validate_effect(source_seq, value):
    value = require_keys(
        value,
        {"schema_version", "source_movement_seq", "source_artifact_hash", "run", "artifact_label", "status", "attempts"},
        {"operation", "operation_args", "disposition", "result", "error", "evidence_movement_seq", "updated_at_unix_ms"},
        f"Matrix effect {source_seq}",
    )
    if value["schema_version"] != 1 or value["source_movement_seq"] != source_seq:
        raise ValueError(f"Matrix effect {source_seq} identity drifted")
    if not isinstance(value["source_artifact_hash"], str) or not DIGEST_RE.fullmatch(value["source_artifact_hash"]):
        raise ValueError(f"Matrix effect {source_seq} artifact hash is malformed")
    require_string(value["run"], f"Matrix effect {source_seq}.run")
    require_string(value["artifact_label"], f"Matrix effect {source_seq}.artifact_label")
    if value["status"] not in {"prepared", "in_flight", "succeeded", "indeterminate", "evidenced", "failure_evidenced"}:
        raise ValueError(f"Matrix effect {source_seq} status is invalid")
    require_int(value["attempts"], f"Matrix effect {source_seq}.attempts", 0)
    if "operation_args" in value:
        require_object(value["operation_args"], f"Matrix effect {source_seq}.operation_args")
    if "disposition" in value:
        require_object(value["disposition"], f"Matrix effect {source_seq}.disposition")
    if "result" in value:
        require_object(value["result"], f"Matrix effect {source_seq}.result")
    if "error" in value:
        require_string(value["error"], f"Matrix effect {source_seq}.error", maximum=16384)
    return value


def load_effect(source_seq):
    value = read_json(effect_path(source_seq), f"Matrix effect {source_seq}", None)
    if value is None:
        return None
    original = value
    value = sanitize_external_error(value, _CONFIG)
    validate_effect(source_seq, value)
    if value != original:
        atomic_json(effect_path(source_seq), value, indent=2)
    return value


def commit_effect(source_seq, value):
    value = sanitize_external_error(dict(value), _CONFIG)
    value["updated_at_unix_ms"] = int(time.time() * 1000)
    validate_effect(source_seq, value)
    atomic_json(effect_path(source_seq), value, indent=2)
    return value


def delivery_evidence(source_seq, content_hash, run, result):
    return hand_json(
        "matrix.delivery.result",
        {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "ok": True, "result": result},
        f"matrix-delivery-{source_seq}",
    )


def target_from_config(config):
    if config["mode"] == "binding":
        binding = config["binding"]
        target = {"room_id": binding["room_id"], "thread_event_id": binding["thread_event_id"]}
        context = read_json(BINDING_CONTEXT_PATH, "Matrix binding context", None)
        if context is not None:
            context = require_keys(context, {"schema_version", "room_id", "thread_event_id", "last_event_id", "updated_at_unix_ms"}, set(), "Matrix binding context")
            if context["schema_version"] != 1 or context["room_id"] != binding["room_id"] or context["thread_event_id"] != binding["thread_event_id"]:
                raise ValueError("Matrix binding context drifted from binding coordinates")
            require_int(context["updated_at_unix_ms"], "Matrix binding context.updated_at_unix_ms", 0)
            if context["last_event_id"] is not None:
                target["reply_to_event_id"] = require_event(context["last_event_id"], "Matrix binding context.last_event_id")
        return target
    room = os.environ.get(config["outbound"]["home_room_env"], "").strip()
    if not room:
        return {}
    room = require_room(room, "Matrix home room")
    if not ROOM_ID_RE.fullmatch(room):
        raise ValueError("Matrix home room must be a concrete room id")
    return {"room_id": room, "thread_event_id": None}


def load_stream_state():
    value = read_json(STREAM_STATE_PATH, "Matrix stream state", {"schema_version": 1, "streams": {}})
    value = require_keys(value, {"schema_version", "streams"}, set(), "Matrix stream state")
    if value["schema_version"] != 1 or not isinstance(value["streams"], dict):
        raise ValueError("Matrix stream state is malformed")
    return value


def stream_key(room_id, thread_event_id):
    return f"{room_id}\n{thread_event_id or ''}"


def stream_exists(target):
    room_id = target.get("room_id")
    if not room_id:
        return False
    return stream_key(room_id, target.get("thread_event_id")) in load_stream_state()["streams"]


def prepare_output(config, label, payload):
    try:
        body = json.loads(payload)
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise ValueError(f"Matrix outbound payload is invalid JSON: {error}") from error
    body = require_object(body, "Matrix outbound payload")
    target = target_from_config(config)
    outbound = config["outbound"]
    if label == config["private_run"]["file_write_result_label"]:
        return None, {"disposition": "topology_authoring_result", "label": label}
    if label in outbound["telemetry_labels"]:
        return None, {"disposition": "observed", "label": label}
    if label in outbound["stream_labels"]:
        body = require_keys(body, {"text", "seq"}, {"room_id", "thread_event_id"}, "Matrix stream payload")
        args = {"text": require_string(body["text"], "Matrix stream text", allow_empty=True, maximum=1048576), "seq": require_int(body["seq"], "Matrix stream seq", 0)}
        args["room_id"] = body.get("room_id") or target.get("room_id")
        args["thread_event_id"] = body.get("thread_event_id") if body.get("thread_event_id") is not None else target.get("thread_event_id")
        return ("stream.delta", args), {"disposition": "stream_delta", "sequence": args["seq"]}
    if label in outbound["error_labels"]:
        detail = body.get("message") or body.get("error") or body.get("detail") or json.dumps(body, ensure_ascii=False, sort_keys=True)
        operation, args = normalize_operation({"text": f"Request failed: {detail}"}, config, target)
        return (operation, args), {"disposition": "error_delivered"}
    if label not in outbound["reply_labels"]:
        raise ValueError(f"Matrix interface does not declare outbound label {label!r}")
    if label == "interface.artifact.output" and isinstance(body.get("text"), str) and stream_exists(target):
        return ("stream.final", {"room_id": target.get("room_id"), "thread_event_id": target.get("thread_event_id"), "text": body["text"]}), {"disposition": "stream_final"}
    operation, args = normalize_operation(body, config, target)
    return (operation, args), {"disposition": "delivered", "operation": operation}


def blob_bytes(blob_id, maximum):
    blob_id = require_string(blob_id, "Matrix media blob_id")
    if not DIGEST_RE.fullmatch(blob_id):
        raise ValueError("Matrix media blob_id must be a content hash")
    payload = read_bounded(os.path.join(BLOBS_DIR, blob_id), maximum, "Matrix media blob")
    if octopus_content_hash(payload) != blob_id:
        raise ValueError("Matrix media blob hash mismatch")
    return payload


def artifact_ref_blob(value):
    value = require_object(value, "Matrix media artifact_ref")
    for key in ("blob_id", "payload_content_hash", "content_hash"):
        candidate = value.get(key)
        if isinstance(candidate, str) and DIGEST_RE.fullmatch(candidate):
            return candidate
    content = value.get("content")
    if isinstance(content, list) and len(content) == 32 and all(isinstance(item, int) and not isinstance(item, bool) and 0 <= item <= 255 for item in content):
        return bytes(content).hex()
    raise ValueError("Matrix media artifact_ref has no content hash")


def hydrate_media_args(config, operation, args):
    if operation != "media.send":
        return args
    args = dict(require_object(args, "Matrix media operation args"))
    if "data_base64" in args:
        raise ValueError("Matrix media output must use artifact_ref or blob_id, not inline data_base64")
    blob_id = args.pop("blob_id", None)
    artifact_ref = args.pop("artifact_ref", None)
    if blob_id is not None and artifact_ref is not None:
        raise ValueError("Matrix media output cannot declare both blob_id and artifact_ref")
    if artifact_ref is not None:
        blob_id = artifact_ref_blob(artifact_ref)
    payload = blob_bytes(blob_id, config["media"]["max_upload_bytes"])
    filename = require_string(args.get("filename"), "Matrix media filename", maximum=1024)
    mimetype = args.get("mimetype")
    if mimetype is None:
        mimetype = mimetypes.guess_type(filename)[0] or "application/octet-stream"
    mimetype = require_string(mimetype, "Matrix media mimetype", maximum=255)
    msgtype = args.get("msgtype")
    if msgtype is None:
        if mimetype.startswith("image/"):
            msgtype = "m.image"
        elif mimetype.startswith("audio/"):
            msgtype = "m.audio"
        elif mimetype.startswith("video/"):
            msgtype = "m.video"
        else:
            msgtype = "m.file"
    args.update({"data_base64": base64.b64encode(payload).decode("ascii"), "filename": filename, "mimetype": mimetype, "msgtype": msgtype})
    return args


class GatewayExecutor:
    def __init__(self, service):
        self.service = service
        self.config = service.config

    def execute(self, operation, args, seed):
        future = asyncio.run_coroutine_threadsafe(self.service.execute_wire(operation, args, seed), self.service.loop)
        timeout = (self.config["transport"]["request_timeout_ms"] + self.config["transport"]["rate_limit_max_wait_ms"] * max(1, self.config["transport"]["safe_retry_limit"])) / 1000.0
        return future.result(timeout)

    def status(self, emoji, seed, target):
        room_id = target.get("room_id")
        event_id = target.get("reply_to_event_id") or target.get("thread_event_id")
        if not room_id or not event_id:
            return None
        future = asyncio.run_coroutine_threadsafe(self.service.status_reaction(room_id, event_id, emoji, seed), self.service.loop)
        return future.result(self.config["transport"]["request_timeout_ms"] / 1000.0)


class BindingExecutor:
    def __init__(self, config, link):
        self.config = config
        self.link = link

    def execute(self, operation, args, seed):
        return self.link.request("effect.execute", {"operation": operation, "args": args, "seed": seed})

    def status(self, emoji, seed, target):
        return self.link.request("status.set", {"emoji": emoji, "seed": seed, "event_id": target.get("reply_to_event_id") or target.get("thread_event_id")})


def execute_stream(config, executor, operation, args, seed):
    args = require_object(args, "Matrix stream args")
    room_id = require_room(args.get("room_id"), "Matrix stream room_id")
    if not ROOM_ID_RE.fullmatch(room_id):
        raise ValueError("Matrix stream room_id must be concrete")
    thread_id = require_event(args.get("thread_event_id"), "Matrix stream thread_event_id", nullable=True)
    key = stream_key(room_id, thread_id)
    state = load_stream_state()
    current = state["streams"].get(key)
    if current is not None and not isinstance(current, dict):
        raise ValueError("Matrix stream entry is malformed")
    if operation == "stream.delta":
        sequence = require_int(args.get("seq"), "Matrix stream seq", 0)
        delta = require_string(args.get("text"), "Matrix stream delta", allow_empty=True, maximum=1048576)
        if current is None:
            if sequence != 0:
                raise ValueError("Matrix stream must start at sequence zero")
            result = executor.execute("message.send", {"room_id": room_id, "thread_event_id": thread_id, "text": delta or "…"}, f"{seed}:stream-start")
            event_id = require_event(result.get("message_id"), "Matrix stream message_id")
            current = {"room_id": room_id, "thread_event_id": thread_id, "event_id": event_id, "last_seq": sequence, "text": delta, "last_edit_unix_ms": int(time.time() * 1000)}
            state["streams"][key] = current
            atomic_json(STREAM_STATE_PATH, state, indent=2)
            return {"operation": operation, "sequence": sequence, "disposition": "created", "result": result}
        previous = require_int(current.get("last_seq"), "Matrix stream last_seq", 0)
        if sequence <= previous:
            return {"operation": operation, "sequence": sequence, "disposition": "duplicate"}
        if sequence != previous + 1:
            raise ValueError(f"Matrix stream sequence jumped from {previous} to {sequence}")
        current["text"] = str(current.get("text") or "") + delta
        current["last_seq"] = sequence
        now = int(time.time() * 1000)
        if now - require_int(current.get("last_edit_unix_ms"), "Matrix stream last edit", 0) >= config["outbound"]["stream_edit_interval_ms"]:
            result = executor.execute("message.edit", {"room_id": room_id, "event_id": current["event_id"], "thread_event_id": thread_id, "text": current["text"] or "…"}, f"{seed}:stream-edit")
            current["last_edit_unix_ms"] = now
            disposition = "edited"
        else:
            result = {"buffered": True}
            disposition = "buffered"
        state["streams"][key] = current
        atomic_json(STREAM_STATE_PATH, state, indent=2)
        return {"operation": operation, "sequence": sequence, "disposition": disposition, "result": result}
    text = require_string(args.get("text"), "Matrix stream final text", maximum=1048576)
    if current is None:
        result = executor.execute("message.send", {"room_id": room_id, "thread_event_id": thread_id, "text": text}, f"{seed}:stream-final-new")
    else:
        result = executor.execute("message.edit", {"room_id": room_id, "event_id": current["event_id"], "thread_event_id": thread_id, "text": text}, f"{seed}:stream-final-edit")
        state["streams"].pop(key, None)
        atomic_json(STREAM_STATE_PATH, state, indent=2)
    return {"operation": operation, "disposition": "finalized", "result": result}


def effect_is_retry_safe(config, operation):
    return operation in config["outbound"]["retry_safe_operations"] or operation in {"stream.delta", "stream.final"}


def failure_details(error):
    if isinstance(error, MatrixWireFailure):
        return sanitize_external_error(error.evidence(), _CONFIG)
    if isinstance(error, CryptoFailure):
        return sanitize_external_error({"category": error.category, "message": str(error), "details": error.details}, _CONFIG)
    return sanitize_external_error({"category": type(error).__name__, "message": str(error)}, _CONFIG)


def status_target(config, args):
    target = target_from_config(config)
    if isinstance(args, dict):
        if isinstance(args.get("room_id"), str):
            target["room_id"] = args["room_id"]
        for key in ("thread_event_id", "reply_to_event_id"):
            if isinstance(args.get(key), str):
                target[key] = args[key]
    return target


def set_delivery_status(config, executor, success, source_seq, args):
    reactions = config["outbound"]["reactions"]
    if not reactions["enabled"]:
        return None
    emoji = reactions["success"] if success else reactions["failure"]
    try:
        return executor.status(emoji, f"outbound:{source_seq}:status", status_target(config, args))
    except Exception as error:
        return {"error": sanitize_external_error(error, config)}


def process_outbound(config, executor, source_seq, content, run):
    if content is None or run is None:
        hand_failure(source_seq, "poisoned_outbound", {"error": "movement carries no artifact or runtime run"}, f"matrix-poison-{source_seq}")
        return True
    if config["mode"] == "binding" and run != config["binding"]["run"]:
        hand_failure(source_seq, "boundary_rejected", {"error": "outbound movement run mismatched private binding", "expected_run": config["binding"]["run"], "observed_run": run}, f"matrix-run-refused-{source_seq}")
        return True
    label, payload, content_hash = materialize_movement_payload(
        content,
        config["outbound"]["max_manifest_bytes"],
        config["outbound"]["max_payload_bytes"],
    )
    effect = load_effect(source_seq)
    if effect is not None and effect["source_artifact_hash"] != content_hash:
        hand_failure(source_seq, "effect_journal_mismatch", {"expected": effect["source_artifact_hash"], "observed": content_hash}, f"matrix-effect-mismatch-{source_seq}")
        return True
    if effect is not None and effect["status"] in {"evidenced", "failure_evidenced"}:
        return True
    if effect is not None and effect["status"] == "succeeded":
        evidence = delivery_evidence(source_seq, content_hash, run, effect["result"])
        effect.update({"status": "evidenced", "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        return True
    if effect is None:
        try:
            prepared, disposition = prepare_output(config, label, payload)
        except Exception as error:
            safe_error = sanitize_external_error(error, config)
            evidence = hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "error": safe_error}, f"matrix-boundary-{source_seq}")
            effect = {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "artifact_label": label, "status": "failure_evidenced", "attempts": 0, "error": safe_error, "evidence_movement_seq": evidence}
            commit_effect(source_seq, effect)
            set_delivery_status(config, executor, False, source_seq, {})
            return True
        effect = {"schema_version": 1, "source_movement_seq": source_seq, "source_artifact_hash": content_hash, "run": run, "artifact_label": label, "status": "succeeded" if prepared is None else "prepared", "attempts": 0, "disposition": disposition}
        if prepared is None:
            effect["result"] = disposition
        else:
            effect["operation"], effect["operation_args"] = prepared
        commit_effect(source_seq, effect)
    operation = effect.get("operation")
    args = effect.get("operation_args", {})
    retry_safe = effect_is_retry_safe(config, operation)
    if effect["status"] in {"in_flight", "indeterminate"}:
        if retry_safe:
            effect.update({"status": "prepared", "error": effect.get("error", "recovered unfinished deterministic effect")})
            commit_effect(source_seq, effect)
        else:
            evidence = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "operation": operation, "error": effect.get("error")}, f"matrix-indeterminate-{source_seq}")
            effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence})
            commit_effect(source_seq, effect)
            set_delivery_status(config, executor, False, source_seq, args)
            return True
    if effect["status"] == "succeeded":
        evidence = delivery_evidence(source_seq, content_hash, run, effect["result"])
        effect.update({"status": "evidenced", "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        return True
    if effect["status"] != "prepared":
        raise RuntimeError(f"Matrix effect {source_seq} cannot execute from {effect['status']}")
    try:
        hydrated = hydrate_media_args(config, operation, args)
    except Exception as error:
        evidence = hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "operation": operation, "error": sanitize_external_error(error, config)}, f"matrix-boundary-{source_seq}")
        effect.update({"status": "failure_evidenced", "error": sanitize_external_error(error, config), "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        set_delivery_status(config, executor, False, source_seq, args)
        return True
    effect.update({"status": "in_flight", "attempts": effect["attempts"] + 1})
    effect.pop("error", None)
    commit_effect(source_seq, effect)
    try:
        if operation in {"stream.delta", "stream.final"}:
            result = execute_stream(config, executor, operation, hydrated, content_hash)
        else:
            result = executor.execute(operation, hydrated, content_hash)
        result = require_object(result, "Matrix operation result")
        result.update(effect["disposition"])
    except (ValueError, CryptoFailure) as error:
        details = failure_details(error)
        evidence = hand_failure(source_seq, "boundary_rejected", {"source_artifact_hash": content_hash, "operation": operation, "error": details}, f"matrix-boundary-{source_seq}")
        effect.update({"status": "failure_evidenced", "error": sanitize_external_error(error, config), "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        set_delivery_status(config, executor, False, source_seq, args)
        return True
    except Exception as error:
        details = failure_details(error)
        if retry_safe:
            effect.update({"status": "prepared", "error": sanitize_external_error(error, config)})
            commit_effect(source_seq, effect)
            stderr(config, f"Matrix deterministic outbound effect {source_seq} will retry: {details}")
            return False
        effect.update({"status": "indeterminate", "error": sanitize_external_error(error, config)})
        commit_effect(source_seq, effect)
        evidence = hand_failure(source_seq, "indeterminate_external_effect", {"source_artifact_hash": content_hash, "operation": operation, "error": details}, f"matrix-indeterminate-{source_seq}")
        effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence})
        commit_effect(source_seq, effect)
        set_delivery_status(config, executor, False, source_seq, args)
        return True
    if config["mode"] == "gateway":
        thread_event_id = hydrated.get("thread_event_id") if isinstance(hydrated, dict) else None
        participated = config["gating"]["thread_sticky_after_bot_reply"] and operation in {"message.send", "media.send", "location.send", "stream.delta", "stream.final"}
        remember_thread_result(config, thread_event_id, result, participated)
    effect.update({"status": "succeeded", "result": result})
    commit_effect(source_seq, effect)
    status = set_delivery_status(config, executor, True, source_seq, args)
    if status is not None:
        result["status_reaction"] = status
        effect["result"] = result
        commit_effect(source_seq, effect)
    evidence = delivery_evidence(source_seq, content_hash, run, result)
    effect.update({"status": "evidenced", "evidence_movement_seq": evidence})
    commit_effect(source_seq, effect)
    return True


def outbound_loop(config, executor):
    next_seq = load_ledger_cursor()
    delay = config["transport"]["reconnect_backoff_ms"]
    while True:
        rows = read_outbound(next_seq, config=config)
        advanced = False
        for source_seq, content, run in rows:
            try:
                finished = process_outbound(config, executor, source_seq, content, run)
            except Exception as error:
                stderr(config, f"Matrix outbound movement {source_seq} failed: {error}")
                finished = False
            if not finished:
                break
            next_seq = source_seq + 1
            commit_ledger_cursor(next_seq)
            advanced = True
        if advanced:
            delay = config["transport"]["reconnect_backoff_ms"]
        else:
            time.sleep(delay / 1000.0)
            delay = min(delay * 2, config["transport"]["reconnect_backoff_cap_ms"])


def run_binding(config):
    start_runtime_verdict_reader()
    link = BindingLink(config)
    executor = BindingExecutor(config, link)
    threading.Thread(target=outbound_loop, args=(config, executor), daemon=True).start()
    signal_ready()
    while True:
        time.sleep(3600)


async def run_gateway(config):
    ensure_gateway_run(config)
    start_runtime_verdict_reader()
    service = MatrixService(config)
    signal_ready()
    try:
        await service.run()
    finally:
        await service.wire.close()


def main():
    global _CONFIG
    config = load_config()
    _CONFIG = config
    if "--validate-config" in sys.argv:
        print(json.dumps({"ok": True, "mode": config["mode"], "interface": INTERFACE_ID}, sort_keys=True))
        return
    if config["mode"] == "binding":
        run_binding(config)
    else:
        asyncio.run(run_gateway(config))


if __name__ == "__main__":
    try:
        main()
    except Exception as error:
        stderr(_CONFIG, f"Matrix interface exited: {error}")
        raise SystemExit(1)
