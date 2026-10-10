import importlib
import json
import os
import pathlib
import shutil
import sys
import tempfile
import unittest
from unittest import mock


PACKAGE = pathlib.Path(__file__).resolve().parent
ROOT = pathlib.Path(tempfile.mkdtemp(prefix="octopus-pyecho-contract-"))
VAR = ROOT / "var"
BLOBS = ROOT / "blobs"
MOVEMENTS = ROOT / "movements.jsonl"
VAR.mkdir(mode=0o700)
BLOBS.mkdir()
MOVEMENTS.write_bytes(b"")
os.environ.update(
    {
        "OCTOPUS_INTERFACE_ID": "world_pyecho",
        "OCTOPUS_VAR_DIR": str(VAR),
        "OCTOPUS_MOVEMENTS_JSONL": str(MOVEMENTS),
        "OCTOPUS_BLOBS_DIR": str(BLOBS),
        "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps(
            {
                "max_control_frame_bytes": 1024,
                "max_stderr_bytes": 1024,
                "max_manifest_bytes": 1024,
                "max_payload_bytes": 64,
            }
        ),
    }
)
sys.path.insert(0, str(PACKAGE))
life = importlib.import_module("pyecho_life")


def cached_artifact(payload, label="operator.message"):
    payload_hash = life.octopus_content_hash(payload)
    (BLOBS / payload_hash).write_bytes(payload)
    manifest = life.canonical_json(
        {
            "label": label,
            "payload_path": "/source/payload",
            "payload_content_hash": payload_hash,
        }
    )
    manifest_hash = life.octopus_content_hash(manifest)
    (BLOBS / manifest_hash).write_bytes(manifest)
    return manifest_hash


class PyechoContractTests(unittest.TestCase):
    def setUp(self):
        for path in (VAR, BLOBS):
            shutil.rmtree(path, ignore_errors=True)
        VAR.mkdir(mode=0o700)
        BLOBS.mkdir()
        MOVEMENTS.write_bytes(b"")
        for path in (life.PAYLOADS_DIR, life.RECEIPTS_DIR):
            life.private_directory(path)

    def test_cached_artifact_uses_blake3_and_exact_payload_ceiling(self):
        payload = b"x" * 64
        manifest_hash = cached_artifact(payload)
        self.assertEqual(life.materialize_payload(manifest_hash), payload)
        oversized_hash = cached_artifact(b"x" * 65)
        with self.assertRaisesRegex(ValueError, "exceeds 64 bytes"):
            life.materialize_payload(oversized_hash)
        self.assertEqual(
            life.octopus_content_hash(b"abc"),
            "6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85",
        )

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "stable-open check requires O_NOFOLLOW")
    def test_cached_artifact_rejects_symlink_and_content_drift(self):
        manifest_hash = cached_artifact(b"safe")
        manifest_path = BLOBS / manifest_hash
        body = manifest_path.read_bytes()
        manifest_path.write_bytes(body + b" ")
        with self.assertRaisesRegex(ValueError, "manifest hash mismatch"):
            life.materialize_payload(manifest_hash)
        manifest_path.unlink()
        outside = ROOT / "outside-manifest"
        outside.write_bytes(body)
        manifest_path.symlink_to(outside)
        with self.assertRaises(PermissionError):
            life.materialize_payload(manifest_hash)

    def test_ledger_reader_never_advances_over_partial_or_malformed_row(self):
        first = life.canonical_json({"seq": 0, "kind": "Mark"}) + b"\n"
        partial = life.canonical_json({"seq": 1, "kind": "Mark"})
        MOVEMENTS.write_bytes(first + partial)
        movement, committed = life.next_movement(life.empty_cursor())
        self.assertEqual(movement["seq"], 0)
        movement, unchanged = life.next_movement(committed)
        self.assertIsNone(movement)
        self.assertEqual(unchanged, committed)
        with MOVEMENTS.open("ab") as handle:
            handle.write(b"\n")
            handle.flush()
            os.fsync(handle.fileno())
        movement, advanced = life.next_movement(committed)
        self.assertEqual(movement["seq"], 1)
        self.assertEqual(advanced["next_seq"], 2)
        MOVEMENTS.write_bytes(b"not-json\n")
        with self.assertRaises(json.JSONDecodeError):
            life.next_movement(life.empty_cursor())
        MOVEMENTS.write_bytes(b"x" * 1025 + b"\n")
        with self.assertRaisesRegex(RuntimeError, "exceeds max_control_frame_bytes"):
            life.next_movement(life.empty_cursor())

    def test_outbound_identity_requires_exact_interface_route_and_runtime(self):
        runtime = {
            "run": "run_pyecho",
            "ee": "ee_pyecho",
            "worker": "system_worker_pyecho",
            "checkpoint": "cp_pyecho",
            "interface": "world_pyecho",
            "epoch": 0,
        }
        canonical = {
            "seq": 4,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_pyecho"},
            "invocation_metadata": {"runtime": runtime},
        }
        self.assertTrue(life.is_own_outbound(canonical))
        foreign = dict(canonical)
        foreign["to"] = {"Interface": "world_other"}
        foreign["invocation_metadata"] = {"runtime": 7}
        self.assertFalse(life.is_own_outbound(foreign))
        legacy = dict(canonical)
        legacy["from"] = {"Run": "run_legacy"}
        legacy["invocation_metadata"] = {"runtime": 7}
        self.assertFalse(life.is_own_outbound(legacy))
        mismatched = dict(canonical)
        mismatched["invocation_metadata"] = {
            "runtime": {**runtime, "interface": "world_other"}
        }
        with self.assertRaisesRegex(ValueError, "exact target"):
            life.is_own_outbound(mismatched)
        malformed = dict(canonical)
        malformed["invocation_metadata"] = {
            "runtime": {"interface": "world_pyecho"}
        }
        with self.assertRaises(ValueError):
            life.is_own_outbound(malformed)

    def test_prepared_receipt_recovers_from_durable_arrival_evidence(self):
        payload = life.canonical_json({"text": "RECOVERED", "by": "world_pyecho"})
        manifest_path, artifact_hash = life.materialize_artifact(
            "pyecho.uppercased",
            payload,
            "reply-4",
        )
        artifact_id, _ = life.arrival_identity("pyecho.uppercased", artifact_hash)
        arrival = {
            "seq": 9,
            "kind": "InterfaceArrival",
            "from": {"Interface": "world_pyecho"},
            "artifact": {"id": artifact_id},
        }
        MOVEMENTS.write_bytes(life.canonical_json(arrival) + b"\n")
        prepared = {
            "schema_version": 1,
            "source_movement_seq": 4,
            "source_artifact_hash": "a" * 64,
            "arrival_artifact_hash": artifact_hash,
            "status": "prepared",
            "admitted_seq": None,
        }
        life.atomic_json(life.receipt_path(4), prepared)
        refused = life.DeliveryRefused(
            {
                "refused": {
                    "reason": "malformed",
                    "detail": {
                        "message": "arrival evidence already exists in the movement ledger",
                        "artifact_id": artifact_id,
                    },
                }
            }
        )
        with mock.patch.object(life, "hand_artifact", side_effect=refused):
            life.deliver(
                4,
                "a" * 64,
                "pyecho.uppercased",
                payload,
                "reply-4",
            )
        receipt = life.load_receipt(4)
        self.assertEqual(receipt["status"], "admitted")
        self.assertEqual(receipt["admitted_seq"], 9)
        self.assertEqual(pathlib.Path(manifest_path).stat().st_mode & 0o777, 0o600)


if __name__ == "__main__":
    unittest.main()
