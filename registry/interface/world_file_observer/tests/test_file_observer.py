import base64
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

OWNER = Path(__file__).resolve().parents[1]
PYECHO = OWNER.parent / "world_pyecho"
sys.path.insert(0, str(OWNER))
sys.path.insert(0, str(PYECHO))
from authoring import binding_repair_starter, observer_package, wait_starter
from file_observer import FileObserver, validate_authority, validate_config


class FileObserverContract(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name).resolve()
        self.source = self.root / "input.txt"
        self.config = {"schema_version": 1, "interface": "world_test_observer", "poll_interval_ms": 50,
                       "read_roots": [str(self.root)],
                       "files": [{"id": "input", "path": str(self.source),
                                  "label": "file.input.observed", "max_bytes": 128}]}
        self.package = observer_package(self.config["interface"], self.config["files"],
                                        read_roots=self.config["read_roots"], poll_interval_ms=50)
        self.interface_dir = self.root / "interface"
        self.interface_dir.mkdir()
        for name, value in self.package.items():
            (self.interface_dir / Path(name).name).write_text(json.dumps(value))
        life = self.package["registry/interface/world_test_observer/life.json"]["processes"][0]
        self.env = dict(os.environ, OCTOPUS_INTERFACE_ID=self.config["interface"],
                        OCTOPUS_INTERFACE_DIR=str(self.interface_dir), OCTOPUS_VAR_DIR=str(self.root / "state"),
                        OCTOPUS_MOVEMENTS_JSONL=str(self.root / "movements.jsonl"),
                        OCTOPUS_INTERFACE_HOST_IO_LIMITS=json.dumps(life["io_limits"]),
                        PYTHONDONTWRITEBYTECODE="1")
        self.env.pop("OCTOPUS_BLOBS_DIR", None)
        with patch.dict(os.environ, self.env):
            spec = importlib.util.spec_from_file_location("test_observer_boundary", PYECHO / "pyecho_life.py")
            self.boundary = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(self.boundary)
        self.deliveries = []
        self.boundary.hand_artifact_idempotent = self.admit

    def admit(self, path, label, digest):
        manifest = json.loads(Path(path).read_bytes())
        payload = Path(manifest["payload_path"]).read_bytes()
        self.assertEqual(self.boundary.octopus_content_hash(payload), manifest["payload_content_hash"])
        self.assertEqual(self.boundary.octopus_content_hash(Path(path).read_bytes()), digest)
        self.deliveries.append((path, label, json.loads(payload)))
        return len(self.deliveries) + 10

    def test_present_content_changes_and_reattach_preserve_snapshot_identity(self):
        observer = FileObserver(self.config, self.boundary)
        observer.poll_once()
        self.assertEqual(self.deliveries, [])
        self.source.write_bytes(b"first\0bytes")
        (self.root / "unselected.txt").write_bytes(b"not an artifact")
        observer.poll_once()
        self.source.touch()
        FileObserver(dict(self.config, poll_interval_ms=1000), self.boundary).poll_once()
        self.assertEqual(len(self.deliveries), 1)
        self.assertEqual(base64.b64decode(self.deliveries[0][2]["content_base64"]), b"first\0bytes")
        self.source.unlink()
        observer.poll_once()
        self.source.write_bytes(b"first\0bytes")
        observer.poll_once()
        self.assertEqual(len(self.deliveries), 2)
        self.assertNotEqual(self.deliveries[0][0], self.deliveries[1][0])

    def test_pending_snapshot_survives_refusal_and_source_change(self):
        self.source.write_bytes(b"before refusal")
        observer = FileObserver(self.config, self.boundary)
        def refuse(*args):
            raise self.boundary.DeliveryRefused({"refused": {"code": "no_binding"}})
        self.boundary.hand_artifact_idempotent = refuse
        with self.assertRaises(self.boundary.DeliveryRefused):
            observer.poll_once()
        pending = observer.inspection()["observations"][0]["delivery"]
        self.assertIsNotNone(pending["pending"])
        self.assertIsNone(pending["admitted_seq"])
        self.source.write_bytes(b"after refusal")
        self.boundary.hand_artifact_idempotent = self.admit
        FileObserver(self.config, self.boundary).poll_once()
        self.assertEqual([base64.b64decode(item[2]["content_base64"]) for item in self.deliveries],
                         [b"before refusal", b"after refusal"])

    def test_corrupt_pending_payload_and_config_identity_fail_closed(self):
        self.source.write_bytes(b"original")
        observer = FileObserver(self.config, self.boundary)
        self.boundary.hand_artifact_idempotent = lambda *args: (_ for _ in ()).throw(RuntimeError("closed"))
        with self.assertRaises(RuntimeError):
            observer.poll_once()
        pending = observer.state["files"]["input"]["pending"]
        manifest = json.loads(Path(pending["manifest_path"]).read_bytes())
        Path(manifest["payload_path"]).write_bytes(b"corrupt")
        self.boundary.hand_artifact_idempotent = self.admit
        with self.assertRaisesRegex(ValueError, "integrity"):
            FileObserver(self.config, self.boundary).poll_once()
        changed = dict(self.config, interface="world_other_observer")
        with self.assertRaisesRegex(ValueError, "exact interface"):
            FileObserver(changed, self.boundary)
        self.assertEqual(self.deliveries, [])

    def test_symlink_oversize_and_authority_mismatch_are_not_observations(self):
        target = self.root / "target"
        target.write_bytes(b"secret")
        self.source.symlink_to(target)
        with self.assertRaises(PermissionError):
            FileObserver(self.config, self.boundary).poll_once()
        self.source.unlink()
        self.source.write_bytes(b"x" * 129)
        with self.assertRaisesRegex(ValueError, "max_bytes"):
            FileObserver(self.config, self.boundary).poll_once()
        life = self.package["registry/interface/world_test_observer/life.json"]
        life["processes"][0]["authority"]["read_paths"] = [str(self.root.parent)]
        with self.assertRaisesRegex(ValueError, "match exactly"):
            validate_authority(life, self.config)
        self.assertEqual(self.deliveries, [])

    def test_starter_has_independent_requirements_and_no_broader_grants(self):
        source = dict(self.config["files"][0], id="other", path=str(self.root / "other"), label="file.other.observed")
        package = wait_starter("world_test_observer", self.config["files"] + [source], "run_test", "system_worker_test", "ee_test",
                               read_roots=self.config["read_roots"])
        run = package["runs/run_test.json"]
        requirements = run["checkpoints"][0]["required_verified_artifacts"]
        self.assertEqual(len(requirements), 2)
        self.assertEqual(len({item["artifact_label"] for item in requirements}), 2)
        ceiling = package["registry/interface/system_worker_test/policy.json"]["ceiling"]
        self.assertEqual(ceiling["mark_outcomes"], [item["mark"] for item in requirements])
        self.assertEqual(ceiling["interfaces"], ["world_test_observer", "system_worker_test"])
        self.assertEqual(ceiling["tools"], [])
        life = package["registry/interface/world_test_observer/life.json"]
        self.assertEqual(life["processes"][0]["authority"]["read_paths"], self.config["read_roots"])
        self.assertEqual(life["processes"][0]["authority"]["write_paths"], [])
        with self.assertRaises(ValueError):
            validate_config(dict(self.config, files=self.config["files"] * 2), self.config["interface"])
        with self.assertRaisesRegex(ValueError, "distinct"):
            wait_starter("system_worker_test", [source], "run_test", "system_worker_test", "ee_test",
                         read_roots=self.config["read_roots"])

    def test_explicit_existing_roots_are_required_without_inferring_parent_scope(self):
        for roots in ([], [str(self.root / "absent")], [str(self.source)], [str(self.root)] * 2):
            with self.subTest(roots=roots), self.assertRaises(ValueError):
                observer_package(self.config["interface"], self.config["files"], read_roots=roots)
        alias = self.root / "alias"
        alias.symlink_to(self.root, target_is_directory=True)
        with self.assertRaisesRegex(ValueError, "canonical directory"):
            observer_package(self.config["interface"], [], read_roots=[str(alias)])
        outside = dict(self.config["files"][0], path=str(self.root) + "-sibling/input.txt")
        with self.assertRaisesRegex(ValueError, "beneath"):
            observer_package(self.config["interface"], [outside], read_roots=self.config["read_roots"])
        empty = observer_package(self.config["interface"], [], read_roots=[])
        self.assertEqual(empty["registry/interface/world_test_observer/observer.json"]["read_roots"], [])
        self.assertEqual(empty["registry/interface/world_test_observer/life.json"]["processes"][0]["authority"]["read_paths"], [])

    def test_binding_repair_preserves_generation_requirements_and_declared_authority(self):
        files = self.config["files"] + [dict(self.config["files"][0], id="second",
                                             path=str(self.root / "second"), label="second")]
        example = binding_repair_starter("world_test_observer", files, "producer", "receiver",
                                        "system_worker_producer", "system_worker_relay", "ee_test",
                                        read_roots=self.config["read_roots"])
        initial, repair = example["initial"], example["repair"]
        self.assertEqual(list(repair), ["runs/receiver.json"])
        before, after = initial["runs/receiver.json"], repair["runs/receiver.json"]
        self.assertEqual(before["checkpoints"][0]["interface_binding"]["interface"], "system_worker_producer")
        self.assertEqual(after["checkpoints"][0]["interface_binding"]["interface"], "system_worker_relay")
        self.assertEqual(before["checkpoints"][0]["epoch"], after["checkpoints"][0]["epoch"])
        self.assertEqual(before["checkpoints"][0]["required_verified_artifacts"], after["checkpoints"][0]["required_verified_artifacts"])
        self.assertEqual(initial["runs/producer.json"]["checkpoints"][0]["outbound_interface_bindings"],
                         [after["checkpoints"][0]["interface_binding"]])
        for worker in ("system_worker_producer", "system_worker_relay"):
            ceiling = initial[f"registry/interface/{worker}/policy.json"]["ceiling"]
            self.assertEqual(ceiling["tools"], [])
            self.assertEqual(ceiling["wek"], [])
        self.assertEqual(initial["registry/interface/world_test_observer/life.json"],
                         observer_package("world_test_observer", files, read_roots=self.config["read_roots"])
                         ["registry/interface/world_test_observer/life.json"])
        self.assertFalse(self.source.exists())
        self.assertFalse(example["demonstration"]["evidence"]["arrival_alone_is_proof"])
        with self.assertRaisesRegex(ValueError, "distinct"):
            binding_repair_starter("world_test_observer", files, "same", "same",
                                   "system_worker_producer", "system_worker_relay", "ee_test",
                                   read_roots=self.config["read_roots"])

    def test_real_python_life_hands_in_manifest_and_records_only_matching_verdict(self):
        self.source.write_bytes(b"protocol")
        process = subprocess.Popen([sys.executable, "-B", str(OWNER / "file_observer_life.py"), str(PYECHO / "pyecho_life.py")],
                                   env=self.env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        lines = []
        import threading
        def exchange():
            lines.append(json.loads(process.stdout.readline()))
            arrival = json.loads(process.stdout.readline())
            lines.append(arrival)
            process.stdin.write(json.dumps({"kind": "delivery_verdict", "request_id": arrival["request_id"] + 1,
                                           "verdict": {"admitted": {"seq": 42}}}).encode() + b"\n")
            process.stdin.flush()
        thread = threading.Thread(target=exchange, daemon=True)
        thread.start()
        try:
            process.wait(timeout=5)
            thread.join(timeout=1)
            self.assertNotEqual(process.returncode, 0)
            self.assertEqual(lines[0], {"kind": "ready"})
            self.assertEqual(lines[1]["kind"], "arrival")
            state = json.loads((self.root / "state/file-observer-state.json").read_bytes())
            self.assertIsNotNone(state["files"]["input"]["pending"])
            self.assertIsNone(state["files"]["input"]["admitted_seq"])
            self.assertIn(b"does not match", process.stderr.read())
        finally:
            if process.poll() is None:
                process.kill()
            process.communicate(timeout=5)


if __name__ == "__main__":
    unittest.main()
