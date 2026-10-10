import json
import os
import re
import sqlite3
import stat
import sys
import tempfile
import threading
import time
import urllib.parse

from octopus_content_hash import octopus_content_hash
from remote_operator_contract import ContractError
from remote_operator_contract import require_int
from remote_operator_contract import require_object
from remote_operator_contract import require_string


def atomic_bytes(path, value, mode=0o600):
    parent = os.path.dirname(path)
    os.makedirs(parent, exist_ok=True)
    descriptor, temporary = tempfile.mkstemp(prefix=".tmp-", dir=parent)
    try:
        os.fchmod(descriptor, mode)
        with os.fdopen(descriptor, "wb") as handle:
            descriptor = -1
            handle.write(value)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
    finally:
        if descriptor >= 0:
            os.close(descriptor)
        try:
            os.unlink(temporary)
        except FileNotFoundError:
            pass


def atomic_json(path, value):
    atomic_bytes(path, json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2).encode("utf-8"))


def immutable_bytes(path, value):
    try:
        with open(path, "rb") as handle:
            existing = handle.read()
    except FileNotFoundError:
        atomic_bytes(path, value)
        return
    if existing != value:
        raise RuntimeError(f"immutable remote operator evidence changed at {path}")


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ContractError(f"{name} is not a stable regular file")
        if opened.st_size > maximum:
            raise ContractError(f"{name} exceeds its size bound")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ContractError(f"{name} grew beyond its size bound")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


def read_private_json(path, maximum, name):
    before = os.lstat(path)
    if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
        raise ContractError(f"{name} is not a private regular file")
    if stat.S_IMODE(before.st_mode) != 0o600 or before.st_uid != os.geteuid():
        raise ContractError(f"{name} permissions are unsafe")
    raw = read_bounded_regular(path, maximum, name)
    return require_object(json.loads(raw), name)


class RuntimeIO:
    def __init__(self, config):
        self.config = config
        self.interface_id = os.environ["OCTOPUS_INTERFACE_ID"]
        self.interface_dir = os.environ["OCTOPUS_INTERFACE_DIR"]
        self.var_dir = os.environ["OCTOPUS_VAR_DIR"]
        self.movements_path = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
        self.blobs_dir = os.environ["OCTOPUS_BLOBS_DIR"]
        self.index_path = os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]
        self.payload_dir = os.path.join(self.var_dir, "payloads")
        self.effect_dir = os.path.join(self.var_dir, "effects")
        self.private_dir = os.path.join(self.var_dir, "private-state")
        self.cursor_path = os.path.join(self.var_dir, "ledger-cursor.json")
        for path in (self.var_dir, self.payload_dir, self.effect_dir, self.private_dir):
            os.makedirs(path, exist_ok=True)
            os.chmod(path, 0o700)
        self.stdout_lock = threading.Lock()
        self.verdict_condition = threading.Condition()
        self.verdicts = {}
        self.verdict_error = None
        self.next_request_id = 1
        self.index_warning = None

    def start(self):
        threading.Thread(target=self._verdict_reader, daemon=True).start()

    def _verdict_reader(self):
        try:
            for line in sys.stdin:
                message = require_object(json.loads(line), "remote operator runtime verdict")
                if message.get("kind") != "delivery_verdict":
                    raise RuntimeError("remote operator life received unexpected runtime input")
                request_id = require_int(message.get("request_id"), "remote operator runtime verdict request_id", 1)
                with self.verdict_condition:
                    self.verdicts[request_id] = require_object(message.get("verdict"), "remote operator delivery verdict")
                    self.verdict_condition.notify_all()
            raise RuntimeError("remote operator runtime verdict input closed")
        except Exception as error:
            with self.verdict_condition:
                self.verdict_error = error
                self.verdict_condition.notify_all()

    def ready(self):
        with self.stdout_lock:
            sys.stdout.write('{"kind":"ready"}\n')
            sys.stdout.flush()

    def hand_manifest(self, manifest_path):
        with self.verdict_condition:
            request_id = self.next_request_id
            self.next_request_id += 1
        frame = {"kind": "arrival", "request_id": request_id, "artifact_manifest_path": manifest_path, "evidence": []}
        with self.stdout_lock:
            sys.stdout.write(json.dumps(frame, separators=(",", ":")) + "\n")
            sys.stdout.flush()
        deadline = time.monotonic() + self.config["driver_ipc"]["verdict_timeout_ms"] / 1000
        with self.verdict_condition:
            while request_id not in self.verdicts and self.verdict_error is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise RuntimeError(f"remote operator runtime verdict timed out for {request_id}")
                self.verdict_condition.wait(remaining)
            if self.verdict_error is not None:
                raise RuntimeError(f"remote operator runtime verdict reader failed: {self.verdict_error}")
            verdict = self.verdicts.pop(request_id)
        admitted = verdict.get("admitted")
        if isinstance(admitted, dict):
            return require_int(admitted.get("seq"), "remote operator admitted movement seq", 0)
        raise RuntimeError(f"runtime refused remote operator artifact: {verdict!r}")

    def hand_json(self, label, value, key):
        payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        payload_path = os.path.join(self.payload_dir, f"{key}.payload")
        manifest_path = os.path.join(self.payload_dir, f"{key}.artifact.json")
        immutable_bytes(payload_path, payload)
        immutable_bytes(manifest_path, json.dumps({"label": label, "payload_path": payload_path}, sort_keys=True, separators=(",", ":")).encode("utf-8"))
        return self.hand_manifest(manifest_path)

    def load_cursor(self):
        try:
            value = read_private_json(self.cursor_path, 65536, "remote operator ledger cursor")
        except FileNotFoundError:
            return 0
        if set(value) != {"schema_version", "next_seq"} or value["schema_version"] != 1:
            raise ContractError("remote operator ledger cursor is invalid")
        return require_int(value["next_seq"], "remote operator ledger cursor next_seq", 0)

    def commit_cursor(self, next_seq):
        atomic_json(self.cursor_path, {"schema_version": 1, "next_seq": next_seq})

    def _index_rows(self, next_seq):
        uri = f"file:{urllib.parse.quote(os.path.abspath(self.index_path))}?mode=ro"
        connection = sqlite3.connect(uri, uri=True, timeout=0.25)
        try:
            schema = connection.execute("SELECT value FROM index_meta WHERE key = 'schema_version'").fetchone()
            if schema != ("3",):
                raise RuntimeError("remote operator ledger index schema is unsupported")
            rows = connection.execute(
                "SELECT seq, artifact_content, runtime_run, runtime_checkpoint FROM movements "
                "WHERE runtime_interface = ? AND kind = 'InterfaceOutbound' "
                "AND from_kind = 'interface' AND to_kind = 'interface' AND to_id = ? AND seq >= ? "
                "ORDER BY seq ASC LIMIT ?",
                (self.interface_id, self.interface_id, next_seq, self.config["driver_ipc"]["ledger_batch_size"]),
            ).fetchall()
        finally:
            connection.close()
        result = []
        for seq, content, run, checkpoint in rows:
            seq = require_int(seq, "remote operator ledger index seq", 0)
            if not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content):
                raise RuntimeError(f"remote operator ledger movement {seq} has no artifact hash")
            result.append((seq, content, require_string(run, "remote operator runtime run", 512), require_string(checkpoint, "remote operator runtime checkpoint", 512)))
        return result

    def _endpoint(self, value):
        if isinstance(value, dict) and set(value) == {"Interface"} and isinstance(value.get("Interface"), str):
            return value["Interface"]
        return None

    def _ledger_rows(self, next_seq):
        try:
            handle = open(self.movements_path, "rb")
        except FileNotFoundError:
            return []
        result = []
        maximum = self.config["driver_ipc"]["ledger_fallback_max_line_bytes"]
        with handle:
            for line in handle:
                if len(line) > maximum:
                    raise RuntimeError("remote operator ledger row exceeds its bound")
                movement = require_object(json.loads(line), "remote operator ledger movement")
                seq = require_int(movement.get("seq"), "remote operator ledger movement seq", 0)
                if seq < next_seq or movement.get("kind") != "InterfaceOutbound" or self._endpoint(movement.get("to")) != self.interface_id or self._endpoint(movement.get("from")) is None:
                    continue
                metadata = require_object(movement.get("invocation_metadata"), "remote operator movement metadata")
                runtime = require_object(metadata.get("runtime"), "remote operator movement runtime metadata")
                if runtime.get("interface") != self.interface_id:
                    raise ContractError("remote operator movement target mismatched its exact interface")
                artifact = require_object(movement.get("artifact"), "remote operator movement artifact")
                content = artifact.get("content")
                if isinstance(content, list):
                    if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or not 0 <= item <= 255 for item in content):
                        raise ContractError("remote operator movement artifact hash bytes are invalid")
                    content = bytes(content).hex()
                if not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content):
                    raise ContractError("remote operator movement artifact hash is invalid")
                result.append((seq, content, require_string(runtime.get("run"), "remote operator runtime run", 512), require_string(runtime.get("checkpoint"), "remote operator runtime checkpoint", 512)))
                if len(result) >= self.config["driver_ipc"]["ledger_batch_size"]:
                    break
        return result

    def read_outbound(self, next_seq):
        try:
            return self._index_rows(next_seq)
        except (OSError, RuntimeError, ValueError, sqlite3.DatabaseError) as error:
            if str(error) != self.index_warning:
                sys.stderr.write(f"remote operator ledger index unavailable; reading ledger truth: {error}\n")
                sys.stderr.flush()
                self.index_warning = str(error)
            return self._ledger_rows(next_seq)

    def materialize(self, content_hash):
        manifest_bytes = read_bounded_regular(os.path.join(self.blobs_dir, content_hash), self.config["driver_ipc"]["max_cached_manifest_bytes"], "remote operator cached manifest")
        if octopus_content_hash(manifest_bytes) != content_hash:
            raise ContractError("remote operator cached manifest failed content-address verification")
        manifest = require_object(json.loads(manifest_bytes), "remote operator cached manifest")
        label = require_string(manifest.get("label"), "remote operator cached artifact label", 256)
        payload_hash = manifest.get("payload_content_hash")
        if not isinstance(payload_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_hash):
            raise ContractError("remote operator cached manifest has no payload hash")
        payload = read_bounded_regular(os.path.join(self.blobs_dir, payload_hash), self.config["driver_ipc"]["max_outbound_payload_bytes"], "remote operator cached payload")
        if octopus_content_hash(payload) != payload_hash:
            raise ContractError("remote operator cached payload failed content-address verification")
        return label, payload

    def effect_path(self, source_seq):
        return os.path.join(self.effect_dir, f"{source_seq}.json")

    def load_effect(self, source_seq):
        try:
            value = read_private_json(self.effect_path(source_seq), 16777216, f"remote operator effect {source_seq}")
        except FileNotFoundError:
            return None
        required = {"schema_version", "source_seq", "source_hash", "request_id", "operation", "status"}
        optional = {"label", "payload", "failure_class", "error", "evidence_seq"}
        if set(value) - required - optional or required - set(value) or value["schema_version"] != 1 or value["source_seq"] != source_seq:
            raise ContractError(f"remote operator effect {source_seq} is invalid")
        if value["status"] not in {"started", "succeeded", "failed", "indeterminate", "evidenced"}:
            raise ContractError(f"remote operator effect {source_seq} status is invalid")
        return value

    def commit_effect(self, source_seq, value):
        atomic_json(self.effect_path(source_seq), value)
