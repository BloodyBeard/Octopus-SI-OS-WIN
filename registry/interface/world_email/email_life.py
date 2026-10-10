#!/usr/bin/env python3
import hashlib
import json
import os
import re
import secrets
import sqlite3
import stat
import sys
import threading
import time
import urllib.parse
from email import policy
from email.parser import BytesParser

from octopus_content_hash import octopus_content_hash

from email_contract import ContractError
from email_contract import atomic_write
from email_contract import automated_sender
from email_contract import load_config as load_contract_config
from email_contract import normalize_address
from email_contract import persist_content_addressed
from email_contract import require_action_payload
from email_contract import require_bool
from email_contract import require_int
from email_contract import require_keys
from email_contract import require_object
from email_contract import require_string
from email_contract import sender_authorized
from email_transport import EmailTransport
from email_transport import TransportError


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
INTERFACE_DIR = os.environ["OCTOPUS_INTERFACE_DIR"]
VAR_DIR = os.environ["OCTOPUS_VAR_DIR"]
MOVEMENTS_JSONL = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
BLOBS_DIR = os.environ["OCTOPUS_BLOBS_DIR"]
LEDGER_INDEX_SQLITE = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
CONFIG_PATH = os.path.join(INTERFACE_DIR, "config.json")
ACTIVATION_PATH = os.path.join(INTERFACE_DIR, "activation.json")
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
STATE_PATH = os.path.join(VAR_DIR, "intake-state.json")
PENDING_DIR = os.path.join(VAR_DIR, "pending")
PAYLOAD_DIR = os.path.join(VAR_DIR, "payloads")
EFFECT_DIR = os.path.join(VAR_DIR, "outbound-effects")
PAIRING_EFFECT_DIR = os.path.join(VAR_DIR, "pairing-effects")
AUTHORIZATION_REQUEST_DIR = os.path.join(VAR_DIR, "authorization-requests")
QUARANTINE_DIR = os.path.join(VAR_DIR, "quarantine")

for directory in (VAR_DIR, PENDING_DIR, PAYLOAD_DIR, EFFECT_DIR, PAIRING_EFFECT_DIR, AUTHORIZATION_REQUEST_DIR, QUARANTINE_DIR):
    os.makedirs(directory, exist_ok=True)
os.chmod(AUTHORIZATION_REQUEST_DIR, 0o700)

_STDOUT_LOCK = threading.Lock()
_VERDICT_CONDITION = threading.Condition()
_VERDICTS = {}
_VERDICT_INPUT_ERROR = None
_NEXT_REQUEST_ID = 1
_STATE_LOCK = threading.RLock()
_PENDING_LOCK = threading.RLock()
_LEDGER_LOCK = threading.RLock()
_LEDGER_FALLBACK_OFFSET = 0
_LEDGER_FALLBACK_SIZE = 0
_LEDGER_FALLBACK_IDENTITY = None
_LEDGER_INDEX_WARNING = None
_HEALTH_LOCK = threading.Lock()
_HEALTH_STATE = {}


def atomic_json(path, value, indent=None):
    atomic_write(path, json.dumps(value, ensure_ascii=False, sort_keys=True, indent=indent).encode("utf-8"))


def immutable_bytes(path, payload):
    try:
        with open(path, "rb") as handle:
            existing = handle.read()
    except FileNotFoundError:
        atomic_write(path, payload)
        return
    if existing != payload:
        raise RuntimeError(f"immutable email evidence path changed: {path}")


def load_config():
    return load_contract_config(CONFIG_PATH)


def load_activation():
    with open(ACTIVATION_PATH, "r", encoding="utf-8") as handle:
        value = json.load(handle)
    value = require_keys(value, {"schema_version", "interface", "activations"}, set(), "email activation")
    if value["schema_version"] != 1:
        raise ContractError("email activation schema_version must be 1")
    if require_string(value["interface"], "email activation.interface") != INTERFACE_ID:
        raise ContractError("email activation interface does not match its registry folder")
    activations = value["activations"]
    if not isinstance(activations, list) or len(activations) != 1:
        raise ContractError("email activation must declare exactly one bootstrap activation")
    activation = require_keys(
        activations[0],
        {"enabled", "activation_driver_ref", "config"},
        set(),
        "email bootstrap activation",
    )
    require_bool(activation["enabled"], "email bootstrap activation.enabled")
    if require_string(activation["activation_driver_ref"], "email bootstrap activation driver") != "app_server_transport":
        raise ContractError("email bootstrap activation must use app_server_transport")
    config = require_keys(activation["config"], {"bind_interface_socket_when_env_unset"}, set(), "email bootstrap activation config")
    if config["bind_interface_socket_when_env_unset"] is not True:
        raise ContractError("email bootstrap activation must bind its interface socket")
    return activation


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


def hand_artifact(manifest_path, timeout_ms):
    global _NEXT_REQUEST_ID
    with _VERDICT_CONDITION:
        request_id = _NEXT_REQUEST_ID
        _NEXT_REQUEST_ID += 1
    with _STDOUT_LOCK:
        sys.stdout.write(
            json.dumps(
                {
                    "kind": "arrival",
                    "request_id": request_id,
                    "artifact_manifest_path": manifest_path,
                    "evidence": [],
                },
                separators=(",", ":"),
            )
            + "\n"
        )
        sys.stdout.flush()
    deadline = time.monotonic() + timeout_ms / 1000
    with _VERDICT_CONDITION:
        while request_id not in _VERDICTS and _VERDICT_INPUT_ERROR is None:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise RuntimeError(f"runtime verdict timed out for email request {request_id}")
            _VERDICT_CONDITION.wait(remaining)
        if _VERDICT_INPUT_ERROR is not None:
            raise RuntimeError(f"runtime verdict input failed before email request {request_id}: {_VERDICT_INPUT_ERROR}")
        verdict = _VERDICTS.pop(request_id)
    admitted = verdict.get("admitted")
    if isinstance(admitted, dict):
        seq = admitted.get("seq")
        if isinstance(seq, int) and not isinstance(seq, bool) and seq >= 0:
            return seq
    raise RuntimeError(f"runtime refused email artifact: {verdict!r}")


def materialize_artifact(label, value, key):
    payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    payload_path = os.path.join(PAYLOAD_DIR, f"{key}.payload")
    manifest_path = os.path.join(PAYLOAD_DIR, f"{key}.artifact.json")
    immutable_bytes(payload_path, payload)
    manifest = json.dumps({"label": label, "payload_path": payload_path}, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
    immutable_bytes(manifest_path, manifest)
    return manifest_path


def hand_json(config, label, value, key):
    return hand_artifact(materialize_artifact(label, value, key), config["driver_ipc"]["verdict_timeout_ms"])


def hand_file(config, label, path, expected_sha256, expected_size, key):
    resolved = os.path.realpath(require_string(path, "email file artifact path"))
    root = os.path.realpath(VAR_DIR)
    if resolved != root and not resolved.startswith(root + os.sep):
        raise ContractError("email file artifact path is outside the interface var root")
    if os.path.islink(path) or not os.path.isfile(resolved):
        raise ContractError("email file artifact path must be a regular non-symlink file")
    with open(resolved, "rb") as handle:
        payload = handle.read(expected_size + 1)
    if len(payload) != expected_size or hashlib.sha256(payload).hexdigest() != expected_sha256:
        raise ContractError("email file artifact failed integrity verification")
    manifest_path = os.path.join(PAYLOAD_DIR, f"{key}.artifact.json")
    immutable_bytes(
        manifest_path,
        json.dumps({"label": label, "payload_path": resolved}, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8"),
    )
    return hand_artifact(manifest_path, config["driver_ipc"]["verdict_timeout_ms"])


def hand_failure(config, source_seq, failure_class, details, key):
    return hand_json(
        config,
        "email.failure",
        {
            "schema_version": 1,
            "source_movement_seq": source_seq,
            "failure_class": failure_class,
            "details": details,
        },
        key,
    )


def default_intake_state():
    return {
        "schema_version": 1,
        "mailboxes": {},
        "processed": [],
        "approved": [],
        "pairing": {},
    }


def validate_intake_state(value):
    value = require_keys(value, {"schema_version", "mailboxes", "processed", "approved", "pairing"}, set(), "email intake state")
    if value["schema_version"] != 1:
        raise ContractError("email intake state schema_version must be 1")
    if not isinstance(value["mailboxes"], dict):
        raise ContractError("email intake state mailboxes must be an object")
    for mailbox, item in value["mailboxes"].items():
        require_string(mailbox, "email intake state mailbox")
        item = require_keys(item, {"uidvalidity", "last_uid"}, set(), f"email mailbox state {mailbox}")
        if item["uidvalidity"] is not None:
            require_int(item["uidvalidity"], f"email mailbox state {mailbox}.uidvalidity", 1)
        require_int(item["last_uid"], f"email mailbox state {mailbox}.last_uid", 0, 4294967295)
    for key in ("processed", "approved"):
        if not isinstance(value[key], list) or any(not isinstance(item, str) or not item for item in value[key]):
            raise ContractError(f"email intake state {key} must be a non-empty string array")
    if not isinstance(value["pairing"], dict):
        raise ContractError("email intake state pairing must be an object")
    for address, entry in value["pairing"].items():
        normalize_address(address)
        entry = require_keys(
            entry,
            {"code", "created_at_unix_ms", "expires_at_unix_ms", "notice_status"},
            {"notice_result", "notice_error"},
            f"email pairing state {address}",
        )
        require_string(entry["code"], f"email pairing state {address}.code")
        require_int(entry["created_at_unix_ms"], f"email pairing state {address}.created_at_unix_ms", 0)
        require_int(entry["expires_at_unix_ms"], f"email pairing state {address}.expires_at_unix_ms", 0)
        if entry["notice_status"] not in {"pending", "started", "succeeded", "indeterminate"}:
            raise ContractError(f"email pairing state {address}.notice_status is invalid")
    return value


def load_intake_state():
    try:
        with open(STATE_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return default_intake_state()
    return validate_intake_state(value)


def mutate_intake_state(config, mutator):
    with _STATE_LOCK:
        state = load_intake_state()
        result = mutator(state)
        state["processed"] = list(dict.fromkeys(state["processed"]))[-config["intake"]["dedup_retention"] :]
        state["approved"] = sorted({normalize_address(value).lower() for value in state["approved"]})
        now = int(time.time() * 1000)
        state["pairing"] = {
            address: entry
            for address, entry in state["pairing"].items()
            if entry["expires_at_unix_ms"] > now or entry["notice_status"] in {"started", "indeterminate"}
        }
        validate_intake_state(state)
        atomic_json(STATE_PATH, state, indent=2)
        return result


def mailbox_progress(mailbox):
    with _STATE_LOCK:
        state = load_intake_state()
        item = state["mailboxes"].get(mailbox)
        if item is None:
            return None, None
        return item["uidvalidity"], item["last_uid"]


def set_mailbox_progress(config, mailbox, uidvalidity, last_uid, processed_key=None):
    def update(state):
        current = state["mailboxes"].get(mailbox)
        if current is not None and current["uidvalidity"] == uidvalidity and last_uid < current["last_uid"]:
            raise ContractError("email mailbox cursor cannot move backward")
        state["mailboxes"][mailbox] = {"uidvalidity": uidvalidity, "last_uid": last_uid}
        if processed_key is not None:
            state["processed"].append(processed_key)

    mutate_intake_state(config, update)


def is_processed(key):
    with _STATE_LOCK:
        return key in set(load_intake_state()["processed"])


def pending_path(key):
    return os.path.join(PENDING_DIR, f"{key}.json")


def load_pending(path):
    with open(path, "r", encoding="utf-8") as handle:
        value = json.load(handle)
    value = require_keys(
        value,
        {
            "schema_version",
            "key",
            "mailbox",
            "uidvalidity",
            "uid",
            "status",
            "label",
            "payload",
            "mark_seen",
        },
        {"runtime_seq", "last_error"},
        "email pending message",
    )
    if value["schema_version"] != 1:
        raise ContractError("email pending message schema_version must be 1")
    if not re.fullmatch(r"[0-9a-f]{64}", require_string(value["key"], "email pending key")):
        raise ContractError("email pending key must be a SHA-256 digest")
    require_string(value["mailbox"], "email pending mailbox")
    require_int(value["uidvalidity"], "email pending uidvalidity", 1)
    require_int(value["uid"], "email pending uid", 1, 4294967295)
    if value["status"] not in {"fetched", "handed", "completed"}:
        raise ContractError("email pending status is invalid")
    require_string(value["label"], "email pending label")
    require_object(value["payload"], "email pending payload")
    if not isinstance(value["mark_seen"], bool):
        raise ContractError("email pending mark_seen must be a boolean")
    if "runtime_seq" in value:
        require_int(value["runtime_seq"], "email pending runtime_seq", 0)
    return value


def commit_pending(value):
    atomic_json(pending_path(value["key"]), value, indent=2)


def message_key(account, mailbox, uidvalidity, uid, raw):
    material = f"{account}\0{mailbox}\0{uidvalidity}\0{uid}\0{hashlib.sha256(raw).hexdigest()}"
    return hashlib.sha256(material.encode("utf-8")).hexdigest()


def approved_addresses():
    with _STATE_LOCK:
        return set(load_intake_state()["approved"])


def classify_inbound(config, transport, parsed, raw):
    sender = parsed["from"]["address"]
    if config["authz"]["reject_self_messages"] and sender.lower() == transport.address.lower():
        return False, "self_message"
    message = BytesParser(policy=policy.default).parsebytes(raw)
    automated, reason = automated_sender(config, sender, message)
    if automated:
        return False, reason or "automated_sender"
    allowed = sender_authorized(config, sender, approved_addresses())
    if not allowed and config["authz"]["deny_unmatched"]:
        return False, "sender_not_allowed"
    return True, None


def pairing_entry(config, address):
    normalized = normalize_address(address).lower()
    now = int(time.time() * 1000)

    def update(state):
        current = state["pairing"].get(normalized)
        if current is not None and current["expires_at_unix_ms"] > now:
            return dict(current)
        if len(state["pairing"]) >= config["authz"]["pairing"]["max_pending"]:
            raise ContractError("email pairing pending limit reached")
        created = {
            "code": secrets.token_urlsafe(9),
            "created_at_unix_ms": now,
            "expires_at_unix_ms": now + config["authz"]["pairing"]["code_ttl_ms"],
            "notice_status": "pending",
        }
        state["pairing"][normalized] = created
        return dict(created)

    return normalized, mutate_intake_state(config, update)


def update_pairing_notice(config, address, status, result=None, error=None):
    def update(state):
        entry = state["pairing"].get(address)
        if entry is None:
            raise ContractError("email pairing entry disappeared")
        entry["notice_status"] = status
        entry.pop("notice_result", None)
        entry.pop("notice_error", None)
        if result is not None:
            entry["notice_result"] = result
        if error is not None:
            entry["notice_error"] = require_string(str(error), "email pairing notice error")

    mutate_intake_state(config, update)


def send_pairing_notice(config, transport, address, entry):
    if entry["notice_status"] in {"succeeded", "started", "indeterminate"}:
        return entry["notice_status"]
    update_pairing_notice(config, address, "started")
    payload = {
        "to": [address],
        "subject": config["authz"]["pairing"]["subject"],
        "text": config["authz"]["pairing"]["body_template"].format(code=entry["code"]),
    }
    source_key = hashlib.sha256(f"pairing\0{address}\0{entry['code']}".encode("utf-8")).hexdigest()
    try:
        transport.throttle_action()
        result = transport.message_send("message.send", payload, source_key, (address,))
    except Exception as error:
        update_pairing_notice(config, address, "indeterminate", error=error)
        raise
    update_pairing_notice(config, address, "succeeded", result=result)
    return "succeeded"


def prepare_pending(config, transport, mailbox, uidvalidity, message):
    raw = message["raw"]
    key = message_key(transport.account_id, mailbox, uidvalidity, message["uid"], raw)
    path = pending_path(key)
    try:
        return load_pending(path)
    except FileNotFoundError:
        pass
    if "boundary_failure" in message:
        quarantine_path, digest = persist_content_addressed(QUARANTINE_DIR, raw, ".partial.eml")
        label = "email.intake.failure"
        payload = {
            "schema_version": 1,
            "account": transport.account_id,
            "mailbox": mailbox,
            "uidvalidity": uidvalidity,
            "uid": message["uid"],
            "failure_class": "message_size_rejected",
            "details": require_object(message["boundary_failure"], "email boundary failure"),
            "evidence": {"path": quarantine_path, "sha256": digest, "captured_size": len(raw), "truncated": True},
        }
    else:
        try:
            parsed = transport.parse_inbound(mailbox, uidvalidity, message)
            admitted, refusal = classify_inbound(config, transport, parsed, raw)
            if admitted:
                label = "email.inbound"
                parsed["reply_context"] = {
                    "action": "message.reply",
                    "mailbox": mailbox,
                    "uidvalidity": uidvalidity,
                    "uid": message["uid"],
                    "message_id": parsed["message_id"],
                    "thread_key": parsed["thread_key"],
                }
                payload = parsed
            else:
                label = "email.intake.refused"
                payload = {
                    "schema_version": 1,
                    "account": transport.account_id,
                    "mailbox": mailbox,
                    "uidvalidity": uidvalidity,
                    "uid": message["uid"],
                    "sender_address": normalize_address(parsed["from"]["address"]).lower(),
                    "reason": refusal,
                    "raw_sha256": parsed["raw"]["sha256"],
                    "raw_size": parsed["raw"]["size"],
                }
                if refusal == "sender_not_allowed" and config["authz"]["pairing"]["enabled"]:
                    address, entry = pairing_entry(config, parsed["from"]["address"])
                    payload["pairing"] = {
                        "address": address,
                        "expires_at_unix_ms": entry["expires_at_unix_ms"],
                    }
                    try:
                        payload["pairing"]["notice_status"] = send_pairing_notice(config, transport, address, entry)
                    except Exception as error:
                        payload["pairing"]["notice_status"] = "indeterminate"
                        payload["pairing"]["notice_error"] = str(error)
        except Exception as error:
            quarantine_path, digest = persist_content_addressed(QUARANTINE_DIR, raw, ".eml")
            label = "email.intake.failure"
            payload = {
                "schema_version": 1,
                "account": transport.account_id,
                "mailbox": mailbox,
                "uidvalidity": uidvalidity,
                "uid": message["uid"],
                "failure_class": "message_parse_rejected",
                "details": {"error": str(error)},
                "evidence": {"path": quarantine_path, "sha256": digest, "size": len(raw)},
            }
    pending = {
        "schema_version": 1,
        "key": key,
        "mailbox": mailbox,
        "uidvalidity": uidvalidity,
        "uid": message["uid"],
        "status": "fetched",
        "label": label,
        "payload": payload,
        "mark_seen": config["intake"]["mark_seen_after_admission"],
    }
    commit_pending(pending)
    return pending


def complete_pending(config, transport, pending):
    if pending["status"] == "fetched":
        runtime_seq = hand_json(config, pending["label"], pending["payload"], f"email-in-{pending['key']}")
        pending["status"] = "handed"
        pending["runtime_seq"] = runtime_seq
        pending.pop("last_error", None)
        commit_pending(pending)
    if pending["status"] == "handed" and pending["mark_seen"]:
        transport.mark_seen(pending["mailbox"], pending["uid"])
    set_mailbox_progress(
        config,
        pending["mailbox"],
        pending["uidvalidity"],
        pending["uid"],
        pending["key"],
    )
    pending["status"] = "completed"
    pending.pop("last_error", None)
    commit_pending(pending)


def complete_stale_pending(config, pending):
    if pending["status"] == "fetched":
        runtime_seq = hand_json(config, pending["label"], pending["payload"], f"email-in-{pending['key']}")
        pending["status"] = "handed"
        pending["runtime_seq"] = runtime_seq
        commit_pending(pending)

    def record(state):
        state["processed"].append(pending["key"])

    mutate_intake_state(config, record)
    pending["status"] = "completed"
    pending["mark_seen"] = False
    pending.pop("last_error", None)
    commit_pending(pending)


def _recover_pending(config, transport, mailbox, current_uidvalidity):
    values = []
    for name in os.listdir(PENDING_DIR):
        if not name.endswith(".json"):
            continue
        path = os.path.join(PENDING_DIR, name)
        try:
            value = load_pending(path)
        except Exception as error:
            with open(path, "rb") as handle:
                raw = handle.read(config["driver_ipc"]["ledger_fallback_max_line_bytes"] + 1)
            digest = hashlib.sha256(raw).hexdigest()
            hand_json(
                config,
                "email.failure",
                {
                    "schema_version": 1,
                    "failure_class": "pending_evidence_corrupt",
                    "details": {"error": str(error), "path": path, "sha256": digest, "size": len(raw)},
                },
                f"email-pending-corrupt-{digest}",
            )
            os.replace(path, path + ".rejected")
            continue
        if value["mailbox"] == mailbox and value["status"] != "completed":
            values.append(value)
    for pending in sorted(values, key=lambda item: (item["uidvalidity"], item["uid"])):
        if pending["uidvalidity"] == current_uidvalidity:
            complete_pending(config, transport, pending)
        else:
            complete_stale_pending(config, pending)


def recover_pending(config, transport, mailbox, current_uidvalidity):
    with _PENDING_LOCK:
        _recover_pending(config, transport, mailbox, current_uidvalidity)


def prune_pending(config):
    with _PENDING_LOCK:
        completed = []
        for name in os.listdir(PENDING_DIR):
            if not name.endswith(".json"):
                continue
            path = os.path.join(PENDING_DIR, name)
            try:
                value = load_pending(path)
            except Exception:
                continue
            if value["status"] == "completed":
                completed.append((os.path.getmtime(path), path))
        completed.sort(reverse=True)
        for _, path in completed[config["intake"]["pending_retention"] :]:
            os.unlink(path)


def emit_health_transition(config, mailbox, status, details):
    if not config["intake"]["emit_health_transitions"]:
        return
    with _HEALTH_LOCK:
        if _HEALTH_STATE.get(mailbox) == status:
            return
    observed_at = int(time.time() * 1000)
    key = hashlib.sha256(f"health\0{mailbox}\0{status}\0{observed_at}\0{json.dumps(details, sort_keys=True, default=str)}".encode("utf-8")).hexdigest()
    hand_json(
        config,
        "email.health",
        {
            "schema_version": 1,
            "mailbox": mailbox,
            "status": status,
            "details": details,
            "observed_at_unix_ms": observed_at,
        },
        f"email-health-{key}",
    )
    with _HEALTH_LOCK:
        _HEALTH_STATE[mailbox] = status


def mailbox_loop(config, transport, mailbox):
    initial_backoff = config["intake"]["reconnect_backoff_ms"]
    cap = config["intake"]["reconnect_backoff_cap_ms"]
    backoff = initial_backoff
    while True:
        try:
            stored_uidvalidity, last_uid = mailbox_progress(mailbox)
            result = transport.poll_mailbox(
                mailbox,
                last_uid,
                config["intake"]["startup"],
                config["intake"]["max_messages_per_poll"],
            )
            current_uidvalidity = result["uidvalidity"]
            if stored_uidvalidity is not None and stored_uidvalidity != current_uidvalidity:
                recover_pending(config, transport, mailbox, current_uidvalidity)
                set_mailbox_progress(config, mailbox, current_uidvalidity, 0)
                emit_health_transition(
                    config,
                    mailbox,
                    "uidvalidity_changed",
                    {"previous": stored_uidvalidity, "current": current_uidvalidity},
                )
                continue
            recover_pending(config, transport, mailbox, current_uidvalidity)
            if stored_uidvalidity is None and not result["messages"]:
                set_mailbox_progress(config, mailbox, current_uidvalidity, result["last_uid"])
            for message in result["messages"]:
                key = message_key(transport.account_id, mailbox, current_uidvalidity, message["uid"], message["raw"])
                if is_processed(key):
                    set_mailbox_progress(config, mailbox, current_uidvalidity, message["uid"])
                    continue
                pending = prepare_pending(config, transport, mailbox, current_uidvalidity, message)
                complete_pending(config, transport, pending)
            prune_pending(config)
            emit_health_transition(
                config,
                mailbox,
                "connected",
                {
                    "uidvalidity": current_uidvalidity,
                    "last_uid": mailbox_progress(mailbox)[1] or 0,
                    "transport": "idle" if config["intake"]["idle_enabled"] else "poll",
                },
            )
            backoff = initial_backoff
            if len(result["messages"]) >= config["intake"]["max_messages_per_poll"]:
                continue
            if config["intake"]["idle_enabled"]:
                transport.wait_for_change(mailbox, config["intake"]["idle_timeout_ms"])
            else:
                time.sleep(config["intake"]["poll_interval_ms"] / 1000)
        except Exception as error:
            if isinstance(error, TransportError):
                error = transport._sanitize_transport_error(error)
            try:
                emit_health_transition(
                    config,
                    mailbox,
                    "disconnected",
                    {"error": str(error), "retry_in_ms": backoff},
                )
            except Exception as evidence_error:
                sys.stderr.write(f"email health evidence failed for {mailbox}: {evidence_error}\n")
                sys.stderr.flush()
            sys.stderr.write(f"email mailbox {mailbox} reconnecting after error: {error}\n")
            sys.stderr.flush()
            time.sleep(backoff / 1000)
            backoff = min(backoff * 2, cap)


def load_cursor():
    try:
        with open(CURSOR_PATH, "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return 0
    value = require_keys(value, {"schema_version", "next_seq"}, set(), "email ledger cursor")
    if value["schema_version"] != 1:
        raise ContractError("email ledger cursor schema_version must be 1")
    return require_int(value["next_seq"], "email ledger cursor.next_seq", 0)


def commit_cursor(next_seq):
    atomic_json(CURSOR_PATH, {"schema_version": 1, "next_seq": next_seq})


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content):
            raise ContractError("email artifact content list is not a 32-byte hash")
        content = bytes(content)
    if not isinstance(content, bytes) or len(content) != 32:
        raise ContractError("email artifact content must be a 32-byte hash")
    return content.hex()


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or not stat.S_ISREG(linked.st_mode):
            raise ContractError(f"email {name} is not a regular file")
        if (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ContractError(f"email {name} changed while opening")
        if opened.st_size > maximum:
            raise ContractError(f"email {name} exceeds its size bound")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ContractError(f"email {name} grew above its size bound while reading")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def materialize_movement_payload(config, content):
    digest = content_hexdigest(content)
    manifest_path = os.path.join(BLOBS_DIR, digest)
    manifest_bytes = read_bounded_regular(
        manifest_path,
        config["driver_ipc"]["max_cached_manifest_bytes"],
        "cached artifact manifest",
    )
    if octopus_content_hash(manifest_bytes) != digest:
        raise ContractError("email cached artifact manifest failed content-address verification")
    manifest = require_object(json.loads(manifest_bytes), "email cached artifact manifest")
    label = require_string(manifest.get("label"), "email cached artifact label")
    payload_digest = manifest.get("payload_content_hash")
    if not isinstance(payload_digest, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_digest):
        raise ContractError("email cached artifact manifest has no payload hash")
    payload = read_bounded_regular(
        os.path.join(BLOBS_DIR, payload_digest),
        config["driver_ipc"]["max_outbound_payload_bytes"],
        "cached artifact payload",
    )
    if octopus_content_hash(payload) != payload_digest:
        raise ContractError("email cached artifact payload failed content-address verification")
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
        "email ledger movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "email ledger movement.invocation_metadata.runtime",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"email ledger movement.invocation_metadata.runtime.{field}",
        )
    require_int(
        runtime.get("epoch"),
        "email ledger movement.invocation_metadata.runtime.epoch",
        0,
    )
    if runtime["interface"] != INTERFACE_ID:
        raise ContractError("email outbound runtime interface does not match its exact target")
    return runtime


def read_index(next_seq, limit):
    uri = f"file:{urllib.parse.quote(os.path.abspath(LEDGER_INDEX_SQLITE))}?mode=ro"
    connection = sqlite3.connect(uri, uri=True, timeout=0.25)
    try:
        schema = connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone()
        if schema != ("3",):
            raise RuntimeError("email ledger index schema is unsupported")
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
        seq = require_int(seq, "email ledger index seq", 0)
        require_string(source_interface, f"email ledger index movement {seq}.from_id")
        if content is not None and (not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content)):
            raise RuntimeError(f"email ledger index movement {seq} has malformed artifact hash")
        require_string(run, f"email ledger index movement {seq}.run")
        require_string(checkpoint, f"email ledger index movement {seq}.checkpoint")
        result.append((seq, bytes.fromhex(content) if content else None, run, checkpoint))
    return result


def read_ledger(next_seq, limit, maximum_line_bytes):
    global _LEDGER_FALLBACK_OFFSET, _LEDGER_FALLBACK_SIZE, _LEDGER_FALLBACK_IDENTITY
    with _LEDGER_LOCK:
        try:
            descriptor = os.open(MOVEMENTS_JSONL, os.O_RDONLY | os.O_NOFOLLOW)
        except FileNotFoundError:
            return []
        try:
            opened = os.fstat(descriptor)
            linked = os.lstat(MOVEMENTS_JSONL)
            identity = (opened.st_dev, opened.st_ino)
            if (
                not stat.S_ISREG(opened.st_mode)
                or stat.S_ISLNK(linked.st_mode)
                or not stat.S_ISREG(linked.st_mode)
                or identity != (linked.st_dev, linked.st_ino)
            ):
                raise RuntimeError("email movement ledger changed while opening")
            offset = _LEDGER_FALLBACK_OFFSET
            if (
                _LEDGER_FALLBACK_IDENTITY != identity
                or opened.st_size < _LEDGER_FALLBACK_SIZE
                or offset > opened.st_size
            ):
                offset = 0
            result = []
            with os.fdopen(descriptor, "rb") as handle:
                descriptor = -1
                handle.seek(offset)
                frontier_open = True
                while True:
                    line = handle.readline(maximum_line_bytes + 1)
                    if not line:
                        break
                    if len(line) > maximum_line_bytes:
                        raise RuntimeError("email ledger movement exceeds configured line bound")
                    if not line.endswith(b"\n"):
                        break
                    candidate_offset = handle.tell()
                    movement = require_object(json.loads(line), "email ledger movement")
                    seq = require_int(movement.get("seq"), "email ledger movement.seq", 0)
                    if seq < next_seq:
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    if movement.get("kind") != "InterfaceOutbound":
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    if movement_interface_endpoint(movement.get("to")) != INTERFACE_ID:
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    if movement_interface_endpoint(movement.get("from")) is None:
                        if frontier_open:
                            _LEDGER_FALLBACK_OFFSET = candidate_offset
                            _LEDGER_FALLBACK_SIZE = os.fstat(handle.fileno()).st_size
                            _LEDGER_FALLBACK_IDENTITY = identity
                        continue
                    runtime = require_outbound_runtime(movement)
                    artifact = movement.get("artifact") if isinstance(movement.get("artifact"), dict) else None
                    frontier_open = False
                    result.append((seq, artifact.get("content") if artifact else None, runtime.get("run"), runtime.get("checkpoint")))
                    if len(result) >= limit:
                        break
            return result
        finally:
            if descriptor >= 0:
                os.close(descriptor)


def read_outbound(config, next_seq):
    global _LEDGER_INDEX_WARNING
    limit = config["outbound"]["ledger_batch_size"]
    try:
        return read_index(next_seq, limit)
    except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
        message = str(error)
        if message != _LEDGER_INDEX_WARNING:
            sys.stderr.write(f"email ledger index unavailable; reading ledger truth: {message}\n")
            sys.stderr.flush()
            _LEDGER_INDEX_WARNING = message
        return read_ledger(next_seq, limit, config["driver_ipc"]["ledger_fallback_max_line_bytes"])


def effect_path(source_seq):
    return os.path.join(EFFECT_DIR, f"{source_seq}.json")


def validate_effect(source_seq, value):
    value = require_keys(
        value,
        {"schema_version", "source_movement_seq", "source_artifact_hash", "run", "source_checkpoint", "artifact_label", "status"},
        {"action", "result", "error", "failure_class", "evidence_movement_seq"},
        f"email effect {source_seq}",
    )
    if value["schema_version"] != 1 or value["source_movement_seq"] != source_seq:
        raise ContractError(f"email effect {source_seq} identity drifted")
    if not isinstance(value["source_artifact_hash"], str) or not re.fullmatch(r"[0-9a-f]{64}", value["source_artifact_hash"]):
        raise ContractError(f"email effect {source_seq} artifact hash is malformed")
    require_string(value["run"], f"email effect {source_seq}.run")
    require_string(value["source_checkpoint"], f"email effect {source_seq}.source_checkpoint")
    require_string(value["artifact_label"], f"email effect {source_seq}.artifact_label")
    if value["status"] not in {"started", "succeeded", "failed", "indeterminate", "evidenced", "failure_evidenced"}:
        raise ContractError(f"email effect {source_seq} status is invalid")
    if "result" in value:
        require_object(value["result"], f"email effect {source_seq}.result")
    if "error" in value:
        require_string(value["error"], f"email effect {source_seq}.error")
    if "failure_class" in value:
        require_string(value["failure_class"], f"email effect {source_seq}.failure_class")
    if "evidence_movement_seq" in value:
        require_int(value["evidence_movement_seq"], f"email effect {source_seq}.evidence_movement_seq", 0)
    return value


def load_effect(source_seq):
    try:
        with open(effect_path(source_seq), "r", encoding="utf-8") as handle:
            value = json.load(handle)
    except FileNotFoundError:
        return None
    return validate_effect(source_seq, value)


def commit_effect(source_seq, value):
    validate_effect(source_seq, value)
    atomic_json(effect_path(source_seq), value, indent=2)


def hand_delivery_result(config, source_seq, content_hash, run, result):
    status = result.get("status")
    applied = status not in {"refused", "failed"}
    return hand_json(
        config,
        "email.delivery.result",
        {
            "schema_version": 1,
            "source_movement_seq": source_seq,
            "source_artifact_hash": content_hash,
            "run": run,
            "effect_applied": applied,
            "result": result,
        },
        f"email-delivery-{source_seq}",
    )


APPROVED_OPERATOR_DECISIONS = {"accept", "accept_for_prefix", "accept_for_session", "approved"}


def authorization_request_path(request_id):
    request_id = require_string(request_id, "email authorization request id")
    if not re.fullmatch(r"email-auth-[0-9a-f]{40}", request_id):
        raise ContractError("email authorization request id is malformed")
    return os.path.join(AUTHORIZATION_REQUEST_DIR, f"{request_id}.json")


def validate_authorization_request(value):
    value = require_keys(
        value,
        {
            "schema_version",
            "request_id",
            "source_artifact_hash",
            "request_digest",
            "action",
            "arguments",
            "status",
            "created_at_unix_ms",
            "expires_at_unix_ms",
        },
        {"operator_movement_seq", "result"},
        "email authorization request",
    )
    if value["schema_version"] != 1:
        raise ContractError("email authorization request schema_version must be 1")
    authorization_request_path(value["request_id"])
    for key in ("source_artifact_hash", "request_digest"):
        if not isinstance(value[key], str) or not re.fullmatch(r"[0-9a-f]{64}", value[key]):
            raise ContractError(f"email authorization request {key} is malformed")
    if value["action"] not in {"authorization.approve", "authorization.revoke"}:
        raise ContractError("email authorization request action is unsupported")
    require_object(value["arguments"], "email authorization request arguments")
    if value["status"] not in {"requested", "applying", "applied"}:
        raise ContractError("email authorization request status is invalid")
    created = require_int(value["created_at_unix_ms"], "email authorization request created_at_unix_ms", 0)
    expires = require_int(value["expires_at_unix_ms"], "email authorization request expires_at_unix_ms", created + 1)
    if expires <= created:
        raise ContractError("email authorization request expiry must follow creation")
    if "operator_movement_seq" in value:
        require_int(value["operator_movement_seq"], "email authorization request operator_movement_seq", 0)
    if "result" in value:
        require_object(value["result"], "email authorization request result")
    return value


def read_private_authorization_request(path):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise ContractError("email authorization request is not a regular file")
    if stat.S_IMODE(before.st_mode) != 0o600 or before.st_uid != os.geteuid():
        raise ContractError("email authorization request permissions are unsafe")
    flags = os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags)
    try:
        current = os.fstat(descriptor)
        if not stat.S_ISREG(current.st_mode) or (before.st_dev, before.st_ino) != (current.st_dev, current.st_ino):
            raise ContractError("email authorization request changed while opening")
        if stat.S_IMODE(current.st_mode) != 0o600 or current.st_uid != os.geteuid():
            raise ContractError("email authorization request permissions changed while opening")
        with os.fdopen(descriptor, "rb", closefd=False) as handle:
            body = handle.read(65537)
    finally:
        os.close(descriptor)
    if len(body) > 65536:
        raise ContractError("email authorization request exceeds its private state bound")
    return validate_authorization_request(require_object(json.loads(body), "email authorization request"))


def load_authorization_request(request_id):
    return read_private_authorization_request(authorization_request_path(request_id))


def commit_authorization_request(value):
    value = validate_authorization_request(value)
    atomic_json(authorization_request_path(value["request_id"]), value, indent=2)


def pending_authorization_request_count(now):
    count = 0
    for name in sorted(os.listdir(AUTHORIZATION_REQUEST_DIR)):
        if not re.fullmatch(r"email-auth-[0-9a-f]{40}\.json", name):
            raise ContractError(f"email authorization request directory contains unexpected entry {name!r}")
        request = read_private_authorization_request(os.path.join(AUTHORIZATION_REQUEST_DIR, name))
        if request["status"] != "applied" and request["expires_at_unix_ms"] > now:
            count += 1
    return count


def validate_authorization_intent(config, requested_action, arguments):
    arguments = require_object(arguments, "email authorization request arguments")
    now = int(time.time() * 1000)
    if requested_action == "authorization.approve":
        arguments = require_keys(arguments, {"code"}, {"address"}, "email authorization approve request")
        code = require_string(arguments["code"], "email authorization code")
        address = arguments.get("address")
        if address is not None:
            address = normalize_address(require_string(address, "email authorization address")).lower()
        with _STATE_LOCK:
            state = load_intake_state()
        matches = [candidate for candidate, entry in state["pairing"].items() if secrets.compare_digest(entry["code"], code)]
        if len(matches) != 1:
            raise ContractError("email authorization code is unknown or ambiguous")
        selected = matches[0]
        if address is not None and not secrets.compare_digest(selected, address):
            raise ContractError("email authorization address does not match pairing code")
        if state["pairing"][selected]["expires_at_unix_ms"] <= now:
            raise ContractError("email authorization code expired")
        return {"address": selected, "code": code}
    if requested_action == "authorization.revoke":
        arguments = require_keys(arguments, {"address"}, set(), "email authorization revoke request")
        address = normalize_address(require_string(arguments["address"], "email authorization address")).lower()
        return {"address": address}
    raise ContractError("email authorization request must target approve or revoke")


def create_authorization_request(config, payload, source_key):
    payload = require_keys(payload, {"requested_action", "arguments"}, set(), "email authorization.request payload")
    requested_action = require_string(payload["requested_action"], "email authorization requested_action")
    arguments = validate_authorization_intent(config, requested_action, payload["arguments"])
    intent = {"action": requested_action, "arguments": arguments}
    intent_bytes = json.dumps(intent, sort_keys=True, separators=(",", ":")).encode("utf-8")
    request_digest = hashlib.sha256(intent_bytes).hexdigest()
    source_key = require_string(source_key, "email authorization request source artifact")
    if not re.fullmatch(r"[0-9a-f]{64}", source_key):
        raise ContractError("email authorization request source artifact hash is malformed")
    request_id = f"email-auth-{hashlib.sha256((source_key + request_digest).encode('ascii')).hexdigest()[:40]}"
    path = authorization_request_path(request_id)
    try:
        request = read_private_authorization_request(path)
        if request["source_artifact_hash"] != source_key or request["request_digest"] != request_digest or request["action"] != requested_action or request["arguments"] != arguments:
            raise ContractError("email authorization request identity collided with different intent")
    except FileNotFoundError:
        now = int(time.time() * 1000)
        if pending_authorization_request_count(now) >= config["authorization_control"]["max_pending_requests"]:
            raise ContractError("email authorization request backlog reached its configured bound")
        request = {
            "schema_version": 1,
            "request_id": request_id,
            "source_artifact_hash": source_key,
            "request_digest": request_digest,
            "action": requested_action,
            "arguments": arguments,
            "status": "requested",
            "created_at_unix_ms": now,
            "expires_at_unix_ms": now + config["authorization_control"]["request_ttl_ms"],
        }
        commit_authorization_request(request)
    if request["status"] == "applied":
        return {"status": "applied", "request_id": request_id, "request_digest": request_digest, "result": request["result"]}
    operator_payload = {
        "request_id": request_id,
        "request_digest": request_digest,
        "title": "Approve email sender" if requested_action == "authorization.approve" else "Revoke email sender",
        "action": requested_action,
        "address": arguments["address"],
    }
    movement_seq = hand_json(
        config,
        config["authorization_control"]["request_artifact_label"],
        operator_payload,
        f"email-authorization-request-{request_id}",
    )
    request["operator_movement_seq"] = movement_seq
    commit_authorization_request(request)
    return {"status": "approval_requested", "request_id": request_id, "request_digest": request_digest, "operator_movement_seq": movement_seq}


def apply_authorization_request(config, payload):
    payload = require_keys(payload, {"request_id", "decision"}, set(), "email operator authorization response")
    request_id = require_string(payload["request_id"], "email operator authorization request_id")
    decision = payload["decision"]
    if isinstance(decision, bool):
        decision = "approved" if decision else "denied"
    decision = require_string(decision, "email operator authorization decision").lower()
    if decision not in APPROVED_OPERATOR_DECISIONS:
        raise ContractError("email authorization request was not approved")
    request = load_authorization_request(request_id)
    if request["status"] == "applied":
        return request["result"]
    now = int(time.time() * 1000)
    if request["status"] == "requested" and request["expires_at_unix_ms"] <= now:
        raise ContractError("email authorization request expired")
    request["status"] = "applying"
    commit_authorization_request(request)
    action = request["action"]
    arguments = request["arguments"]
    address = arguments["address"]
    if action == "authorization.approve":
        code = arguments["code"]

        def approve(state):
            if address in state["approved"]:
                return {"status": "approved", "address": address}
            entry = state["pairing"].get(address)
            if entry is None or not secrets.compare_digest(entry["code"], code):
                raise ContractError("email authorization code no longer matches pending state")
            if entry["expires_at_unix_ms"] <= int(time.time() * 1000):
                raise ContractError("email authorization code expired")
            state["approved"].append(address)
            state["pairing"].pop(address, None)
            return {"status": "approved", "address": address}

        result = mutate_intake_state(config, approve)
    else:

        def revoke(state):
            before = len(state["approved"])
            state["approved"] = [candidate for candidate in state["approved"] if candidate != address]
            pending_removed = state["pairing"].pop(address, None) is not None
            removed = before != len(state["approved"]) or pending_removed
            return {"status": "revoked" if removed else "absent", "address": address}

        result = mutate_intake_state(config, revoke)
    request["status"] = "applied"
    request["result"] = result
    commit_authorization_request(request)
    return result


def authorization_action(config, action, payload, source_key):
    if action == "authorization.list":
        with _STATE_LOCK:
            state = load_intake_state()
        return {
            "status": "completed",
            "approved": state["approved"],
            "pending": [
                {
                    "address": address,
                    "created_at_unix_ms": entry["created_at_unix_ms"],
                    "expires_at_unix_ms": entry["expires_at_unix_ms"],
                    "notice_status": entry["notice_status"],
                }
                for address, entry in sorted(state["pairing"].items())
            ],
        }
    if action == "authorization.request":
        return create_authorization_request(config, payload, source_key)
    if action == "authorization.apply":
        return apply_authorization_request(config, payload)
    raise ContractError(f"unsupported email authorization action {action!r}")


SAFE_RETRY_ACTIONS = {
    "capabilities.get",
    "health.get",
    "folder.list",
    "folder.status",
    "message.list",
    "message.search",
    "message.fetch",
    "message.raw",
    "message.thread",
    "attachment.fetch",
    "authorization.list",
    "authorization.request",
    "authorization.apply",
}


def normalize_outbound_payload(label, payload):
    if "action" in payload:
        return payload
    if label != "interface.artifact.output":
        return payload
    nested = payload.get("email")
    if nested is None:
        nested = payload.get("reply_context")
    if nested is not None:
        nested = require_object(nested, "email output projection")
        merged = dict(nested)
        for key in ("text", "html", "attachments", "headers", "subject"):
            if key in payload and key not in merged:
                merged[key] = payload[key]
        payload = merged
    if "mailbox" in payload and "uid" in payload:
        return {"action": "message.reply", **payload}
    if "to" in payload:
        return {"action": "message.send", **payload}
    raise ContractError("interface.artifact.output requires email reply_context fields or explicit recipients")


def execute_action(config, transport, action, payload, source_key):
    if action.startswith("authorization."):
        return authorization_action(config, action, payload, source_key)
    attempts = config["outbound"]["retry_max_attempts"] if action in SAFE_RETRY_ACTIONS else 1
    backoff = config["outbound"]["retry_backoff_ms"]
    cap = config["outbound"]["retry_backoff_cap_ms"]
    for attempt in range(attempts):
        try:
            if action not in SAFE_RETRY_ACTIONS:
                transport.throttle_action()
            result = transport.execute(action, payload, source_key)
            if action == "attachment.fetch":
                attachment = result["attachment"]
                result["attachment_artifact_seq"] = hand_file(
                    config,
                    "email.attachment",
                    attachment["stored_path"],
                    attachment["sha256"],
                    attachment["size"],
                    f"email-attachment-{source_key}",
                )
            elif action == "message.raw":
                result["raw_artifact_seq"] = hand_file(
                    config,
                    "email.raw",
                    result["path"],
                    result["sha256"],
                    result["size"],
                    f"email-raw-{source_key}",
                )
            return result
        except TransportError as error:
            error = transport._sanitize_transport_error(error)
            if not error.retryable or attempt + 1 >= attempts:
                raise
            time.sleep(backoff / 1000)
            backoff = min(backoff * 2, cap)
    raise RuntimeError("email action retry loop exhausted without a result")


def normalize_authorization_control_payload(payload):
    payload = require_keys(payload, {"operator_approval_response"}, set(), "email operator authorization payload")
    response = require_keys(
        payload["operator_approval_response"],
        {"request_id", "decision"},
        set(),
        "email operator approval response",
    )
    return {
        "request_id": require_string(response["request_id"], "email operator approval request_id"),
        "decision": response["decision"],
    }


def decode_outbound_action(config, label, payload_bytes, checkpoint):
    payload = require_object(json.loads(payload_bytes), "email outbound artifact")
    control = config["authorization_control"]
    if checkpoint == control["checkpoint"]:
        if label != control["artifact_label"]:
            raise ContractError("email authorization checkpoint received the wrong artifact label")
        payload = normalize_authorization_control_payload(payload)
        action = "authorization.apply"
        if action not in set(control["allowed_actions"]):
            raise ContractError("email authorization checkpoint does not grant authorization.apply")
        return action, payload
    if label not in config["outbound"]["accepted_labels"]:
        raise ContractError(f"email interface does not accept outbound label {label!r}")
    payload = normalize_outbound_payload(label, payload)
    return require_action_payload(payload, set(config["outbound"]["allowed_actions"]))


def process_outbound(config, transport, source_seq, content, run, checkpoint):
    if content is None or run is None or checkpoint is None:
        hand_failure(config, source_seq, "poisoned_outbound", {"error": "movement carries no artifact content or runtime coordinate"}, f"email-poison-{source_seq}")
        return
    try:
        label, payload_bytes, content_hash = materialize_movement_payload(config, content)
    except Exception as error:
        hand_failure(
            config,
            source_seq,
            "artifact_cache_rejected",
            {"error": str(error)},
            f"email-artifact-cache-{source_seq}",
        )
        return
    try:
        effect = load_effect(source_seq)
    except Exception as error:
        path = effect_path(source_seq)
        with open(path, "rb") as handle:
            raw_effect = handle.read(config["driver_ipc"]["ledger_fallback_max_line_bytes"] + 1)
        digest = hashlib.sha256(raw_effect).hexdigest()
        hand_failure(
            config,
            source_seq,
            "effect_journal_corrupt",
            {"source_artifact_hash": content_hash, "error": str(error), "journal_sha256": digest, "journal_size": len(raw_effect)},
            f"email-effect-corrupt-{source_seq}-{digest}",
        )
        os.replace(path, path + ".rejected")
        return
    if effect is not None and effect["source_artifact_hash"] != content_hash:
        hand_failure(
            config,
            source_seq,
            "effect_journal_mismatch",
            {"expected": effect["source_artifact_hash"], "observed": content_hash},
            f"email-effect-mismatch-{source_seq}",
        )
        return
    if effect is not None and effect["source_checkpoint"] != checkpoint:
        hand_failure(
            config,
            source_seq,
            "effect_journal_mismatch",
            {"expected_checkpoint": effect["source_checkpoint"], "observed_checkpoint": checkpoint},
            f"email-effect-checkpoint-mismatch-{source_seq}",
        )
        return
    if effect is not None and effect["status"] in {"evidenced", "failure_evidenced"}:
        return
    if effect is not None and effect["status"] == "succeeded":
        evidence_seq = hand_delivery_result(config, source_seq, content_hash, run, effect["result"])
        effect.update({"status": "evidenced", "evidence_movement_seq": evidence_seq})
        commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] == "failed":
        failure_class = effect.get("failure_class") or "boundary_rejected"
        evidence_seq = hand_failure(
            config,
            source_seq,
            failure_class,
            {"source_artifact_hash": content_hash, "action": effect.get("action"), "error": effect.get("error")},
            f"email-{failure_class}-{source_seq}",
        )
        effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence_seq})
        commit_effect(source_seq, effect)
        return
    if effect is not None and effect["status"] == "started" and effect.get("action") in SAFE_RETRY_ACTIONS:
        effect = None
    if effect is not None and effect["status"] in {"started", "indeterminate"}:
        evidence_seq = hand_failure(
            config,
            source_seq,
            "indeterminate_external_effect",
            {"source_artifact_hash": content_hash, "action": effect.get("action"), "error": effect.get("error")},
            f"email-indeterminate-{source_seq}",
        )
        effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence_seq})
        commit_effect(source_seq, effect)
        return
    try:
        action, payload = decode_outbound_action(config, label, payload_bytes, checkpoint)
    except Exception as error:
        hand_failure(
            config,
            source_seq,
            "boundary_rejected",
            {"source_artifact_hash": content_hash, "error": str(error)},
            f"email-boundary-{source_seq}",
        )
        return
    effect = {
        "schema_version": 1,
        "source_movement_seq": source_seq,
        "source_artifact_hash": content_hash,
        "run": run,
        "source_checkpoint": checkpoint,
        "artifact_label": label,
        "action": action,
        "status": "started",
    }
    commit_effect(source_seq, effect)
    try:
        result = execute_action(config, transport, action, payload, content_hash)
    except ContractError as error:
        effect.update({"status": "failed", "failure_class": "boundary_rejected", "error": str(error)})
        commit_effect(source_seq, effect)
        process_outbound(config, transport, source_seq, content, run, checkpoint)
        return
    except Exception as error:
        if isinstance(error, TransportError):
            error = transport._sanitize_transport_error(error)
        if action in SAFE_RETRY_ACTIONS:
            effect.update({"status": "failed", "failure_class": "transport_failure", "error": str(error)})
            commit_effect(source_seq, effect)
            process_outbound(config, transport, source_seq, content, run, checkpoint)
            return
        effect.update({"status": "indeterminate", "error": str(error)})
        commit_effect(source_seq, effect)
        evidence_seq = hand_failure(
            config,
            source_seq,
            "indeterminate_external_effect",
            {"source_artifact_hash": content_hash, "action": action, "error": str(error)},
            f"email-indeterminate-{source_seq}",
        )
        effect.update({"status": "failure_evidenced", "evidence_movement_seq": evidence_seq})
        commit_effect(source_seq, effect)
        return
    effect.update({"status": "succeeded", "result": result})
    commit_effect(source_seq, effect)
    evidence_seq = hand_delivery_result(config, source_seq, content_hash, run, result)
    effect.update({"status": "evidenced", "evidence_movement_seq": evidence_seq})
    commit_effect(source_seq, effect)


def outbound_loop(config, transport):
    next_seq = load_cursor()
    while True:
        rows = read_outbound(config, next_seq)
        for source_seq, content, run, checkpoint in rows:
            try:
                process_outbound(config, transport, source_seq, content, run, checkpoint)
            except Exception as error:
                if isinstance(error, TransportError):
                    error = transport._sanitize_transport_error(error)
                sys.stderr.write(f"email outbound movement {source_seq} retained after error: {error}\n")
                sys.stderr.flush()
                time.sleep(0.25)
                break
            next_seq = source_seq + 1
            commit_cursor(next_seq)
        time.sleep(config["driver_ipc"]["idle_sleep_ms"] / 1000)


def main():
    config = load_config()
    load_activation()
    transport = EmailTransport(config, VAR_DIR, BLOBS_DIR)
    transport.preflight_environment()
    start_runtime_verdict_reader()
    if config["intake"]["enabled"]:
        for alias in config["intake"]["mailboxes"]:
            mailbox = config["account"]["mailboxes"][alias]
            threading.Thread(target=mailbox_loop, args=(config, transport, mailbox), daemon=True).start()
    signal_ready()
    sys.stderr.write(
        f"email interface up: interface={INTERFACE_ID} gateway_run={config['gateway_run_template']['id']} account={transport.account_id}\n"
    )
    sys.stderr.flush()
    outbound_loop(config, transport)


if __name__ == "__main__":
    main()
