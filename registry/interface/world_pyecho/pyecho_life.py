#!/usr/bin/env python3
import json
import os
import re
import stat
import sys
import time

from octopus_content_hash import octopus_content_hash


INTERFACE_ID = os.environ["OCTOPUS_INTERFACE_ID"]
VAR_DIR = os.path.abspath(os.environ["OCTOPUS_VAR_DIR"])
MOVEMENTS_JSONL = os.path.abspath(os.environ["OCTOPUS_MOVEMENTS_JSONL"])
BLOBS_DIR = os.path.abspath(os.environ["OCTOPUS_BLOBS_DIR"])
CURSOR_PATH = os.path.join(VAR_DIR, "ledger-cursor.json")
PAYLOADS_DIR = os.path.join(VAR_DIR, "payloads")
RECEIPTS_DIR = os.path.join(VAR_DIR, "receipts")
_NEXT_REQUEST_ID = 1


class DeliveryRefused(RuntimeError):
    def __init__(self, verdict):
        self.verdict = verdict
        super().__init__(f"runtime refused pyecho artifact: {verdict!r}")


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


def require_int(value, name, minimum=0, maximum=None):
    if isinstance(value, bool) or not isinstance(value, int) or value < minimum:
        raise ValueError(f"{name} must be an integer >= {minimum}")
    if maximum is not None and value > maximum:
        raise ValueError(f"{name} must be <= {maximum}")
    return value


def require_string(value, name, maximum=4096):
    if not isinstance(value, str) or not value or value.strip() != value:
        raise ValueError(f"{name} must be a non-empty trimmed string")
    if len(value.encode("utf-8")) > maximum:
        raise ValueError(f"{name} exceeds {maximum} bytes")
    return value


def load_io_limits():
    value = require_keys(
        json.loads(os.environ["OCTOPUS_INTERFACE_HOST_IO_LIMITS"]),
        {
            "max_control_frame_bytes",
            "max_manifest_bytes",
            "max_payload_bytes",
            "max_stderr_bytes",
        },
        set(),
        "pyecho host IO limits",
    )
    return {
        key: require_int(value[key], f"pyecho host IO limits.{key}", 1)
        for key in value
    }


IO_LIMITS = load_io_limits()


def private_directory(path):
    os.makedirs(path, mode=0o700, exist_ok=True)
    state = os.lstat(path)
    if stat.S_ISLNK(state.st_mode) or not stat.S_ISDIR(state.st_mode):
        raise PermissionError(f"pyecho private path is not a directory: {path}")
    if hasattr(os, "geteuid") and state.st_uid != os.geteuid():
        raise PermissionError(f"pyecho private path has the wrong owner: {path}")
    if stat.S_IMODE(state.st_mode) != 0o700:
        os.chmod(path, 0o700)
    return path


def stable_read(path, maximum, name, private=False):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError(f"pyecho {name} is not a regular file")
    if private:
        if stat.S_IMODE(before.st_mode) != 0o600:
            raise PermissionError(f"pyecho {name} is not private")
        if hasattr(os, "geteuid") and before.st_uid != os.geteuid():
            raise PermissionError(f"pyecho {name} has the wrong owner")
    if before.st_size > maximum:
        raise ValueError(f"pyecho {name} exceeds {maximum} bytes")
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode):
            raise PermissionError(f"pyecho {name} changed while opening")
        if (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino):
            raise PermissionError(f"pyecho {name} changed while opening")
        if (linked.st_dev, linked.st_ino) != (opened.st_dev, opened.st_ino):
            raise PermissionError(f"pyecho {name} changed while opening")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ValueError(f"pyecho {name} grew beyond {maximum} bytes")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def sync_directory(path):
    descriptor = os.open(path, os.O_RDONLY)
    try:
        os.fsync(descriptor)
    finally:
        os.close(descriptor)


def atomic_bytes(path, value):
    private_directory(os.path.dirname(path))
    temporary = f"{path}.{os.getpid()}.{time.time_ns()}.tmp"
    descriptor = os.open(
        temporary,
        os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, "O_NOFOLLOW", 0),
        0o600,
    )
    try:
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        os.chmod(path, 0o600)
        sync_directory(os.path.dirname(path))
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def canonical_json(value):
    return json.dumps(
        value,
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")


def atomic_json(path, value):
    body = canonical_json(value) + b"\n"
    if len(body) > IO_LIMITS["max_control_frame_bytes"]:
        raise ValueError("pyecho private JSON exceeds max_control_frame_bytes")
    atomic_bytes(path, body)


def empty_cursor():
    return {
        "schema_version": 1,
        "ledger_device": None,
        "ledger_inode": None,
        "byte_offset": 0,
        "next_seq": 0,
    }


def validate_cursor(value):
    value = require_keys(
        value,
        {
            "schema_version",
            "ledger_device",
            "ledger_inode",
            "byte_offset",
            "next_seq",
        },
        set(),
        "pyecho ledger cursor",
    )
    if value["schema_version"] != 1:
        raise ValueError("pyecho ledger cursor schema_version is unsupported")
    for key in ("ledger_device", "ledger_inode"):
        if value[key] is not None:
            require_int(value[key], f"pyecho ledger cursor.{key}")
    require_int(value["byte_offset"], "pyecho ledger cursor.byte_offset")
    require_int(value["next_seq"], "pyecho ledger cursor.next_seq")
    if (value["ledger_device"] is None) != (value["ledger_inode"] is None):
        raise ValueError("pyecho ledger cursor identity is incomplete")
    return value


def load_cursor():
    try:
        body = stable_read(
            CURSOR_PATH,
            IO_LIMITS["max_control_frame_bytes"],
            "ledger cursor",
            private=True,
        )
    except FileNotFoundError:
        return empty_cursor()
    return validate_cursor(json.loads(body))


def commit_cursor(cursor):
    atomic_json(CURSOR_PATH, validate_cursor(cursor))


def content_hexdigest(content):
    if isinstance(content, list):
        if len(content) != 32 or any(
            isinstance(item, bool)
            or not isinstance(item, int)
            or not 0 <= item <= 255
            for item in content
        ):
            raise ValueError("pyecho artifact content is not a 32-byte hash")
        return bytes(content).hex()
    if isinstance(content, str) and re.fullmatch(r"[0-9a-f]{64}", content):
        return content
    raise ValueError("pyecho artifact content is not a 32-byte hash")


def open_ledger():
    before = os.lstat(MOVEMENTS_JSONL)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise PermissionError("pyecho movement ledger is not a regular file")
    descriptor = os.open(
        MOVEMENTS_JSONL,
        os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0),
    )
    opened = os.fstat(descriptor)
    linked = os.lstat(MOVEMENTS_JSONL)
    if (
        not stat.S_ISREG(opened.st_mode)
        or stat.S_ISLNK(linked.st_mode)
        or (before.st_dev, before.st_ino) != (opened.st_dev, opened.st_ino)
        or (linked.st_dev, linked.st_ino) != (opened.st_dev, opened.st_ino)
    ):
        os.close(descriptor)
        raise PermissionError("pyecho movement ledger changed while opening")
    return os.fdopen(descriptor, "rb"), opened


def next_movement(cursor):
    try:
        handle, state = open_ledger()
    except FileNotFoundError:
        return None, cursor
    with handle:
        identity = (state.st_dev, state.st_ino)
        cursor_identity = (cursor["ledger_device"], cursor["ledger_inode"])
        reset = cursor_identity != identity or state.st_size < cursor["byte_offset"]
        offset = 0 if reset else cursor["byte_offset"]
        candidate = dict(cursor)
        candidate["ledger_device"] = state.st_dev
        candidate["ledger_inode"] = state.st_ino
        candidate["byte_offset"] = offset
        handle.seek(offset)
        previous_seq = None
        while True:
            row_offset = handle.tell()
            line = handle.readline(IO_LIMITS["max_control_frame_bytes"] + 1)
            if not line:
                return None, candidate
            if len(line) > IO_LIMITS["max_control_frame_bytes"]:
                raise RuntimeError(
                    f"pyecho ledger row at byte {row_offset} exceeds max_control_frame_bytes"
                )
            if not line.endswith(b"\n"):
                return None, candidate
            movement = require_object(json.loads(line), "pyecho ledger movement")
            sequence = require_int(movement.get("seq"), "pyecho ledger movement.seq")
            if previous_seq is not None and sequence != previous_seq + 1:
                raise RuntimeError(
                    f"pyecho ledger sequence discontinuity after {previous_seq}: {sequence}"
                )
            previous_seq = sequence
            if sequence < cursor["next_seq"]:
                if not reset:
                    raise RuntimeError("pyecho cursor points behind an already committed movement")
                candidate["byte_offset"] = handle.tell()
                continue
            if sequence != candidate["next_seq"]:
                raise RuntimeError(
                    f"pyecho ledger sequence discontinuity: expected {candidate['next_seq']}, got {sequence}"
                )
            candidate["byte_offset"] = handle.tell()
            candidate["next_seq"] = sequence + 1
            return movement, candidate


def materialize_payload(content):
    manifest_hash = content_hexdigest(content)
    manifest = stable_read(
        os.path.join(BLOBS_DIR, manifest_hash),
        IO_LIMITS["max_manifest_bytes"],
        "cached artifact manifest",
    )
    if octopus_content_hash(manifest) != manifest_hash:
        raise ValueError("pyecho cached artifact manifest hash mismatch")
    descriptor = require_keys(
        json.loads(manifest),
        {"label", "payload_path", "payload_content_hash"},
        set(),
        "pyecho cached artifact manifest",
    )
    require_string(descriptor["label"], "pyecho cached artifact manifest.label", 256)
    payload_hash = descriptor["payload_content_hash"]
    if not isinstance(payload_hash, str) or re.fullmatch(r"[0-9a-f]{64}", payload_hash) is None:
        raise ValueError("pyecho cached artifact manifest has invalid payload_content_hash")
    payload = stable_read(
        os.path.join(BLOBS_DIR, payload_hash),
        IO_LIMITS["max_payload_bytes"],
        "cached artifact payload",
    )
    if octopus_content_hash(payload) != payload_hash:
        raise ValueError("pyecho cached artifact payload hash mismatch")
    return payload


def materialize_artifact(label, payload, stem):
    require_string(label, "pyecho artifact label", 256)
    if len(payload) > IO_LIMITS["max_payload_bytes"]:
        raise ValueError("pyecho artifact payload exceeds max_payload_bytes")
    payload_hash = octopus_content_hash(payload)
    payload_path = os.path.join(PAYLOADS_DIR, f"{stem}-{payload_hash}.payload")
    try:
        existing = stable_read(
            payload_path,
            IO_LIMITS["max_payload_bytes"],
            "private artifact payload",
            private=True,
        )
        if existing != payload:
            raise RuntimeError("pyecho private payload conflicts with its content identity")
    except FileNotFoundError:
        atomic_bytes(payload_path, payload)
    manifest = {
        "label": label,
        "payload_path": payload_path,
        "payload_content_hash": payload_hash,
    }
    manifest_bytes = canonical_json(manifest)
    if len(manifest_bytes) > IO_LIMITS["max_manifest_bytes"]:
        raise ValueError("pyecho artifact manifest exceeds max_manifest_bytes")
    manifest_hash = octopus_content_hash(manifest_bytes)
    manifest_path = os.path.join(
        PAYLOADS_DIR,
        f"{stem}-{manifest_hash}.artifact.json",
    )
    try:
        existing = stable_read(
            manifest_path,
            IO_LIMITS["max_manifest_bytes"],
            "private artifact manifest",
            private=True,
        )
        if existing != manifest_bytes:
            raise RuntimeError("pyecho private manifest conflicts with its content identity")
    except FileNotFoundError:
        atomic_bytes(manifest_path, manifest_bytes)
    return manifest_path, manifest_hash


def hand_artifact(manifest_path, evidence):
    global _NEXT_REQUEST_ID
    request_id = _NEXT_REQUEST_ID
    _NEXT_REQUEST_ID += 1
    message = {
        "kind": "arrival",
        "request_id": request_id,
        "artifact_manifest_path": manifest_path,
        "evidence": evidence,
    }
    encoded = canonical_json(message) + b"\n"
    if len(encoded) > IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("pyecho arrival frame exceeds max_control_frame_bytes")
    sys.stdout.buffer.write(encoded)
    sys.stdout.buffer.flush()
    line = sys.stdin.buffer.readline(IO_LIMITS["max_control_frame_bytes"] + 1)
    if not line:
        raise RuntimeError("runtime verdict input closed before admission")
    if len(line) > IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("runtime verdict exceeds max_control_frame_bytes")
    if not line.endswith(b"\n"):
        raise RuntimeError("runtime verdict is partial")
    response = require_keys(
        json.loads(line),
        {"kind", "request_id", "verdict"},
        set(),
        "pyecho runtime verdict",
    )
    if response["kind"] != "delivery_verdict" or response["request_id"] != request_id:
        raise RuntimeError("pyecho runtime verdict does not match the pending arrival")
    verdict = require_object(response["verdict"], "pyecho runtime delivery verdict")
    if set(verdict) == {"refused"}:
        raise DeliveryRefused(verdict)
    if set(verdict) != {"admitted"}:
        raise RuntimeError("pyecho runtime delivery verdict has an invalid shape")
    admitted = require_keys(
        verdict["admitted"],
        {"seq"},
        set(),
        "pyecho admitted verdict",
    )
    return require_int(admitted["seq"], "pyecho admitted verdict.seq")


def signal_ready():
    encoded = canonical_json({"kind": "ready"}) + b"\n"
    if len(encoded) > IO_LIMITS["max_control_frame_bytes"]:
        raise RuntimeError("pyecho readiness frame exceeds max_control_frame_bytes")
    sys.stdout.buffer.write(encoded)
    sys.stdout.buffer.flush()


def arrival_identity(label, artifact_hash):
    artifact_id = f"{label}:{artifact_hash}"
    return artifact_id, [
        {
            "interface_id": INTERFACE_ID,
            "artifact_id": artifact_id,
            "label": "pyecho.arrival.idempotency",
            "bytes": list(bytes.fromhex(octopus_content_hash(artifact_id.encode("utf-8")))),
        }
    ]


def find_admitted_arrival_seq(artifact_id):
    try:
        handle, _ = open_ledger()
    except FileNotFoundError:
        return None
    with handle:
        while True:
            row_offset = handle.tell()
            line = handle.readline(IO_LIMITS["max_control_frame_bytes"] + 1)
            if not line:
                return None
            if len(line) > IO_LIMITS["max_control_frame_bytes"]:
                raise RuntimeError(
                    f"pyecho ledger lookup row at byte {row_offset} exceeds max_control_frame_bytes"
                )
            if not line.endswith(b"\n"):
                return None
            movement = require_object(json.loads(line), "pyecho ledger lookup movement")
            if (
                movement.get("kind") != "InterfaceArrival"
                or movement.get("from") != {"Interface": INTERFACE_ID}
            ):
                continue
            artifact = movement.get("artifact")
            if isinstance(artifact, dict) and artifact.get("id") == artifact_id:
                return require_int(
                    movement.get("seq"),
                    "pyecho admitted arrival movement.seq",
                )


def hand_artifact_idempotent(manifest_path, label, artifact_hash):
    artifact_id, evidence = arrival_identity(label, artifact_hash)
    try:
        return hand_artifact(manifest_path, evidence)
    except DeliveryRefused as error:
        refused = error.verdict.get("refused")
        detail = refused.get("detail") if isinstance(refused, dict) else None
        if (
            isinstance(detail, dict)
            and detail.get("artifact_id") == artifact_id
            and detail.get("message")
            == "arrival evidence already exists in the movement ledger"
        ):
            admitted = find_admitted_arrival_seq(artifact_id)
            if admitted is not None:
                return admitted
        raise


def transform(request_bytes):
    try:
        request = json.loads(request_bytes)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return canonical_json(
            {"text": request_bytes.decode("utf-8", "replace").upper(), "by": INTERFACE_ID}
        )
    if not isinstance(request, dict):
        raise ValueError("pyecho JSON request must be an object")
    text = request.get("text", "")
    if not isinstance(text, str):
        raise ValueError("pyecho request.text must be a string")
    return canonical_json({"text": text.upper(), "by": INTERFACE_ID})


def receipt_path(source_seq):
    return os.path.join(RECEIPTS_DIR, f"{source_seq}.json")


def load_receipt(source_seq):
    try:
        value = json.loads(
            stable_read(
                receipt_path(source_seq),
                IO_LIMITS["max_control_frame_bytes"],
                "arrival receipt",
                private=True,
            )
        )
    except FileNotFoundError:
        return None
    return require_keys(
        value,
        {
            "schema_version",
            "source_movement_seq",
            "source_artifact_hash",
            "arrival_artifact_hash",
            "status",
            "admitted_seq",
        },
        set(),
        "pyecho arrival receipt",
    )


def deliver(source_seq, source_hash, label, payload, stem):
    path, artifact_hash = materialize_artifact(label, payload, stem)
    existing = load_receipt(source_seq)
    if existing is not None:
        if (
            existing["schema_version"] != 1
            or existing["source_movement_seq"] != source_seq
            or existing["source_artifact_hash"] != source_hash
            or existing["arrival_artifact_hash"] != artifact_hash
        ):
            raise RuntimeError("pyecho arrival receipt conflicts with its source movement")
        if existing["status"] == "admitted":
            require_int(existing["admitted_seq"], "pyecho arrival receipt.admitted_seq")
            return
        if existing["status"] != "prepared" or existing["admitted_seq"] is not None:
            raise RuntimeError("pyecho arrival receipt has an invalid state")
        prepared = existing
    else:
        prepared = {
            "schema_version": 1,
            "source_movement_seq": source_seq,
            "source_artifact_hash": source_hash,
            "arrival_artifact_hash": artifact_hash,
            "status": "prepared",
            "admitted_seq": None,
        }
        atomic_json(receipt_path(source_seq), prepared)
    admitted_seq = hand_artifact_idempotent(path, label, artifact_hash)
    prepared["status"] = "admitted"
    prepared["admitted_seq"] = admitted_seq
    atomic_json(receipt_path(source_seq), prepared)


def process_outbound(movement):
    source_seq = require_int(movement.get("seq"), "pyecho outbound movement.seq")
    artifact = require_object(movement.get("artifact"), "pyecho outbound movement.artifact")
    source_hash = content_hexdigest(artifact.get("content"))
    try:
        request = materialize_payload(artifact.get("content"))
        deliver(
            source_seq,
            source_hash,
            "pyecho.uppercased",
            transform(request),
            f"reply-{source_seq}",
        )
    except (OSError, ValueError, json.JSONDecodeError) as error:
        failure = canonical_json(
            {
                "source_movement_seq": source_seq,
                "failure_class": "boundary_rejected",
                "details": {"error": str(error)},
            }
        )
        deliver(
            source_seq,
            source_hash,
            f"{INTERFACE_ID}.failure",
            failure,
            f"failure-{source_seq}",
        )


def interface_endpoint(value):
    if not isinstance(value, dict) or set(value) != {"Interface"}:
        return None
    identifier = value.get("Interface")
    if not isinstance(identifier, str) or not identifier or identifier.strip() != identifier:
        return None
    return identifier


def is_own_outbound(movement):
    if movement.get("kind") != "InterfaceOutbound":
        return False
    if interface_endpoint(movement.get("to")) != INTERFACE_ID:
        return False
    if interface_endpoint(movement.get("from")) is None:
        return False
    metadata = require_object(
        movement.get("invocation_metadata"),
        "pyecho outbound movement.invocation_metadata",
    )
    runtime = require_object(
        metadata.get("runtime"),
        "pyecho outbound movement.invocation_metadata.runtime",
    )
    for field in ("run", "ee", "worker", "checkpoint", "interface"):
        require_string(
            runtime.get(field),
            f"pyecho outbound movement runtime.{field}",
            256,
        )
    require_int(runtime.get("epoch"), "pyecho outbound movement runtime.epoch")
    if runtime["interface"] != INTERFACE_ID:
        raise ValueError("pyecho outbound runtime interface does not match its exact target")
    return True


def main():
    os.umask(0o077)
    for path in (VAR_DIR, PAYLOADS_DIR, RECEIPTS_DIR):
        private_directory(path)
    cursor = load_cursor()
    startup = (
        f"pyecho life up: interface={INTERFACE_ID} "
        f"offset={cursor['byte_offset']} seq={cursor['next_seq']}\n"
    ).encode("utf-8")
    if len(startup) > IO_LIMITS["max_stderr_bytes"]:
        raise RuntimeError("pyecho startup evidence exceeds max_stderr_bytes")
    sys.stderr.buffer.write(startup)
    sys.stderr.buffer.flush()
    signal_ready()
    while True:
        movement, candidate = next_movement(cursor)
        if movement is None:
            if candidate != cursor:
                commit_cursor(candidate)
                cursor = candidate
            time.sleep(0.05)
            continue
        if is_own_outbound(movement):
            process_outbound(movement)
        commit_cursor(candidate)
        cursor = candidate


if __name__ == "__main__":
    main()
