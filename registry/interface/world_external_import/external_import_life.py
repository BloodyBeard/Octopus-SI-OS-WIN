#!/usr/bin/env python3
import json
import os
import re
import stat
import sys
import tempfile
import threading
import time

from external_import_contract import ContractError
from external_import_contract import inspect_request
from external_import_contract import load_config
from external_import_contract import require_int
from external_import_contract import require_object
from octopus_content_hash import octopus_content_hash


def strict_json_loads(value):
    def reject_constant(constant):
        raise ContractError(f"non-finite JSON number {constant!r} is unsupported")

    return json.loads(value, parse_constant=reject_constant)


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
        raise RuntimeError(f"immutable external import evidence changed at {path}")


def read_bounded_regular(path, maximum, name):
    descriptor = os.open(path, os.O_RDONLY | getattr(os, "O_NOFOLLOW", 0))
    try:
        opened = os.fstat(descriptor)
        linked = os.lstat(path)
        if not stat.S_ISREG(opened.st_mode) or stat.S_ISLNK(linked.st_mode) or (opened.st_dev, opened.st_ino) != (linked.st_dev, linked.st_ino):
            raise ContractError(f"{name} is not a stable regular file")
        if opened.st_size > maximum:
            raise ContractError(f"{name} exceeds its byte bound")
        with os.fdopen(descriptor, "rb") as handle:
            descriptor = -1
            value = handle.read(maximum + 1)
        if len(value) > maximum:
            raise ContractError(f"{name} grew beyond its byte bound")
        return value
    finally:
        if descriptor >= 0:
            os.close(descriptor)


class RuntimeIO:
    def __init__(self, config):
        self.config = config
        self.interface_id = os.environ["OCTOPUS_INTERFACE_ID"]
        self.interface_dir = os.environ["OCTOPUS_INTERFACE_DIR"]
        self.var_dir = os.environ["OCTOPUS_VAR_DIR"]
        self.movements_path = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
        self.blobs_dir = os.environ["OCTOPUS_BLOBS_DIR"]
        self.payload_dir = os.path.join(self.var_dir, "payloads")
        self.effect_dir = os.path.join(self.var_dir, "effects")
        self.cursor_path = os.path.join(self.var_dir, "ledger-cursor.json")
        for path in (self.var_dir, self.payload_dir, self.effect_dir):
            os.makedirs(path, exist_ok=True)
        try:
            root_slots = strict_json_loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        except (KeyError, json.JSONDecodeError, ContractError) as error:
            raise ContractError(f"external import root-slot projection is unavailable: {error}") from error
        self.root_slots = require_object(root_slots, "external import root-slot projection")
        self.stdout_lock = threading.Lock()
        self.verdict_condition = threading.Condition()
        self.verdicts = {}
        self.verdict_error = None
        self.next_request_id = 1

    def start(self):
        threading.Thread(target=self._verdict_reader, daemon=True).start()

    def _verdict_reader(self):
        try:
            for line in sys.stdin:
                message = require_object(strict_json_loads(line), "external import runtime verdict")
                if message.get("kind") != "delivery_verdict":
                    raise RuntimeError("external import life received unexpected runtime input")
                request_id = require_int(message.get("request_id"), "external import verdict request_id", 1)
                with self.verdict_condition:
                    self.verdicts[request_id] = require_object(message.get("verdict"), "external import delivery verdict")
                    self.verdict_condition.notify_all()
            raise RuntimeError("external import runtime verdict input closed")
        except Exception as error:
            with self.verdict_condition:
                self.verdict_error = error
                self.verdict_condition.notify_all()

    def ready(self):
        with self.stdout_lock:
            sys.stdout.write('{"kind":"ready"}\n')
            sys.stdout.flush()

    def hand_json(self, label, value, key):
        payload = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode("utf-8")
        payload_path = os.path.join(self.payload_dir, f"{key}.payload")
        manifest_path = os.path.join(self.payload_dir, f"{key}.artifact.json")
        immutable_bytes(payload_path, payload)
        immutable_bytes(
            manifest_path,
            json.dumps({"label": label, "payload_path": payload_path}, sort_keys=True, separators=(",", ":")).encode("utf-8"),
        )
        return self.hand_manifest(manifest_path)

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
                    raise RuntimeError(f"external import delivery verdict timed out for {request_id}")
                self.verdict_condition.wait(remaining)
            if self.verdict_error is not None:
                raise RuntimeError(f"external import verdict reader failed: {self.verdict_error}")
            verdict = self.verdicts.pop(request_id)
        admitted = verdict.get("admitted")
        if isinstance(admitted, dict):
            return require_int(admitted.get("seq"), "external import admitted movement seq", 0)
        raise RuntimeError(f"runtime refused external import artifact: {verdict!r}")

    def load_cursor(self):
        try:
            value = strict_json_loads(read_bounded_regular(self.cursor_path, 4096, "external import cursor"))
        except FileNotFoundError:
            return 0
        if not isinstance(value, dict) or set(value) != {"schema_version", "next_seq"} or value.get("schema_version") != 1:
            raise ContractError("external import cursor is invalid")
        return require_int(value["next_seq"], "external import cursor next_seq", 0)

    def commit_cursor(self, next_seq):
        atomic_json(self.cursor_path, {"schema_version": 1, "next_seq": next_seq})

    def read_outbound(self, next_seq):
        try:
            handle = open(self.movements_path, "rb")
        except FileNotFoundError:
            return [], next_seq
        result = []
        scanned_next_seq = next_seq
        maximum = self.config["driver_ipc"]["ledger_fallback_max_line_bytes"]
        with handle:
            for line in handle:
                if len(line) > maximum:
                    raise ContractError("external import ledger row exceeds its bound")
                movement = require_object(strict_json_loads(line), "external import ledger movement")
                seq = require_int(movement.get("seq"), "external import movement seq", 0)
                if seq < next_seq:
                    continue
                scanned_next_seq = max(scanned_next_seq, seq + 1)
                if movement.get("kind") != "InterfaceOutbound":
                    continue
                if self._endpoint(movement.get("to")) != self.interface_id or self._endpoint(movement.get("from")) is None:
                    continue
                metadata = require_object(movement.get("invocation_metadata"), "external import movement metadata")
                runtime = require_object(metadata.get("runtime"), "external import runtime metadata")
                if runtime.get("interface") != self.interface_id:
                    raise ContractError("external import movement mismatched its exact interface")
                artifact = require_object(movement.get("artifact"), "external import movement artifact")
                content = artifact.get("content")
                if isinstance(content, list):
                    if len(content) != 32 or any(isinstance(item, bool) or not isinstance(item, int) or item < 0 or item > 255 for item in content):
                        raise ContractError("external import artifact hash bytes are invalid")
                    content = bytes(content).hex()
                if not isinstance(content, str) or not re.fullmatch(r"[0-9a-f]{64}", content):
                    raise ContractError("external import artifact hash is invalid")
                result.append((seq, content, runtime.get("run"), runtime.get("checkpoint")))
                if len(result) >= self.config["driver_ipc"]["ledger_batch_size"]:
                    break
        return result, scanned_next_seq

    def materialize(self, content_hash):
        manifest_bytes = read_bounded_regular(
            os.path.join(self.blobs_dir, content_hash),
            self.config["driver_ipc"]["max_cached_manifest_bytes"],
            "external import cached manifest",
        )
        if octopus_content_hash(manifest_bytes) != content_hash:
            raise ContractError("external import cached manifest failed content verification")
        manifest = require_object(strict_json_loads(manifest_bytes), "external import cached manifest")
        label = manifest.get("label")
        if not isinstance(label, str):
            raise ContractError("external import cached manifest has no label")
        payload_hash = manifest.get("payload_content_hash")
        if not isinstance(payload_hash, str) or not re.fullmatch(r"[0-9a-f]{64}", payload_hash):
            raise ContractError("external import cached manifest has no payload hash")
        payload = read_bounded_regular(
            os.path.join(self.blobs_dir, payload_hash),
            self.config["driver_ipc"]["max_outbound_payload_bytes"],
            "external import cached payload",
        )
        if octopus_content_hash(payload) != payload_hash:
            raise ContractError("external import cached payload failed content verification")
        return label, payload

    def effect_path(self, seq):
        return os.path.join(self.effect_dir, f"{seq}.json")

    def load_effect(self, seq):
        try:
            value = strict_json_loads(read_bounded_regular(self.effect_path(seq), 1048576, f"external import effect {seq}"))
        except FileNotFoundError:
            return None
        if not isinstance(value, dict) or value.get("schema_version") != 1 or value.get("source_seq") != seq:
            raise ContractError(f"external import effect {seq} is invalid")
        return value

    def commit_effect(self, seq, value):
        atomic_json(self.effect_path(seq), value)

    @staticmethod
    def _endpoint(value):
        if isinstance(value, dict) and set(value) == {"Interface"} and isinstance(value.get("Interface"), str):
            return value["Interface"]
        return None


def process_outbound(runtime, source_seq, content_hash, run, checkpoint):
    existing = runtime.load_effect(source_seq)
    if existing is not None and existing.get("status") == "evidenced":
        return
    observed_at_unix_ms = existing.get("observed_at_unix_ms") if isinstance(existing, dict) else None
    if isinstance(observed_at_unix_ms, bool) or not isinstance(observed_at_unix_ms, int) or observed_at_unix_ms < 0:
        observed_at_unix_ms = int(time.time() * 1000)
    candidate_evidence = existing.get("candidate_evidence", []) if isinstance(existing, dict) else []
    if not isinstance(candidate_evidence, list):
        raise ContractError(f"external import effect {source_seq} candidate evidence is invalid")
    try:
        label, payload = runtime.materialize(content_hash)
        if label not in runtime.config["contract"]["accepted_labels"]:
            raise ContractError(f"external import label {label!r} is unsupported")
        request = strict_json_loads(payload)
        request_id = request.get("request_id") if isinstance(request, dict) else None
        state = {
            "schema_version": 1,
            "source_seq": source_seq,
            "source_hash": content_hash,
            "request_id": request_id,
            "observed_at_unix_ms": observed_at_unix_ms,
            "candidate_evidence": candidate_evidence,
            "status": "started",
        }
        runtime.commit_effect(source_seq, state)
        runtime.hand_json(
            "external_import.progress",
            {"schema_version": 1, "request_id": request_id, "source_movement_seq": source_seq, "phase": "inspecting"},
            f"external-import-progress-{source_seq}-inspecting",
        )
        bundle = inspect_request(request, runtime.root_slots, runtime.config, observed_at_unix_ms)
        evidenced_ids = {item.get("candidate_id") for item in candidate_evidence if isinstance(item, dict)}
        for candidate in bundle["candidates"]:
            if candidate["candidate_id"] in evidenced_ids:
                continue
            evidence_seq = runtime.hand_json(
                "external_import.candidate",
                candidate,
                candidate["candidate_id"],
            )
            candidate_evidence.append({"candidate_id": candidate["candidate_id"], "movement_seq": evidence_seq})
            state["candidate_evidence"] = candidate_evidence
            runtime.commit_effect(source_seq, state)
        receipt = {
            "schema_version": 1,
            "request_id": bundle["request_id"],
            "source_movement_seq": source_seq,
            "source": bundle["source"],
            "observed_at_unix_ms": bundle["observed_at_unix_ms"],
            "facts": bundle["facts"],
            "counts": bundle["counts"],
            "candidate_evidence": candidate_evidence,
            "item_receipts": bundle["receipts"],
        }
        receipt_seq = existing.get("receipt_movement_seq") if isinstance(existing, dict) else None
        if isinstance(receipt_seq, bool) or not isinstance(receipt_seq, int) or receipt_seq < 0:
            receipt_seq = runtime.hand_json(
                "external_import.receipt",
                receipt,
                f"external-import-receipt-{source_seq}-{content_hash}",
            )
            state["receipt_movement_seq"] = receipt_seq
            state["status"] = "receipt_emitted"
            runtime.commit_effect(source_seq, state)
        runtime.hand_json(
            "external_import.progress",
            {"schema_version": 1, "request_id": bundle["request_id"], "source_movement_seq": source_seq, "phase": "completed", "receipt_movement_seq": receipt_seq},
            f"external-import-progress-{source_seq}-completed",
        )
        runtime.commit_effect(
            source_seq,
            {**state, "status": "evidenced", "receipt_movement_seq": receipt_seq},
        )
    except Exception as error:
        request_id = None
        try:
            request_id = request.get("request_id") if isinstance(request, dict) else None
        except UnboundLocalError:
            pass
        failure = {
            "schema_version": 1,
            "request_id": request_id,
            "source_movement_seq": source_seq,
            "source_hash": content_hash,
            "facts": {"inspection_completed": False, "source_writes": 0, "registry_writes": 0, "secrets_copied": 0},
            "counts": {"candidates": 0, "receipts": 1, "candidate": 0, "refused": 0, "unsupported": 0, "error": 1},
            "candidate_evidence": [],
            "item_receipts": [
                {
                    "item_class": "request",
                    "status": "error",
                    "reason": "inspection_failed",
                    "message": str(error)[:4096],
                    "source_provenance": {"root_slot": "external_import_source_root", "content_hash": content_hash, "content_read": True},
                    "candidate_ids": [],
                }
            ],
        }
        try:
            receipt_seq = runtime.hand_json(
                "external_import.receipt",
                failure,
                f"external-import-failure-{source_seq}-{content_hash}",
            )
            runtime.commit_effect(
                source_seq,
                {"schema_version": 1, "source_seq": source_seq, "source_hash": content_hash, "request_id": request_id, "status": "evidenced", "receipt_movement_seq": receipt_seq},
            )
        except Exception as hand_error:
            runtime.commit_effect(
                source_seq,
                {"schema_version": 1, "source_seq": source_seq, "source_hash": content_hash, "request_id": request_id, "status": "failed", "error": str(hand_error)[:4096]},
            )
            raise


def main():
    interface_dir = os.environ.get("OCTOPUS_INTERFACE_DIR", os.path.dirname(os.path.abspath(__file__)))
    config = load_config(interface_dir)
    runtime = RuntimeIO(config)
    runtime.start()
    runtime.ready()
    next_seq = runtime.load_cursor()
    while True:
        rows, scanned_next_seq = runtime.read_outbound(next_seq)
        if not rows:
            if scanned_next_seq > next_seq:
                next_seq = scanned_next_seq
                runtime.commit_cursor(next_seq)
            time.sleep(config["driver_ipc"]["idle_sleep_ms"] / 1000)
            continue
        for seq, content_hash, run, checkpoint in rows:
            process_outbound(runtime, seq, content_hash, run, checkpoint)
            next_seq = seq + 1
            runtime.commit_cursor(next_seq)
        if scanned_next_seq > next_seq:
            next_seq = scanned_next_seq
            runtime.commit_cursor(next_seq)


if __name__ == "__main__":
    main()
