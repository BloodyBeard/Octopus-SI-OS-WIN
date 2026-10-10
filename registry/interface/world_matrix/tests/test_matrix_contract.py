import copy
import ast
import base64
import concurrent.futures
import hashlib
import json
import os
import pathlib
import socket
import sys
import tempfile
import threading
import time
import types
import unittest
from types import SimpleNamespace
from unittest import mock


PACKAGE_DIR = pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0, str(PACKAGE_DIR))
TEST_ROOT = tempfile.mkdtemp(prefix="octopus-matrix-tests-")
for name, value in {
    "OCTOPUS_INTERFACE_ID": "world_matrix",
    "OCTOPUS_INTERFACE_DIR": str(PACKAGE_DIR),
    "OCTOPUS_VAR_DIR": os.path.join(TEST_ROOT, "var"),
    "OCTOPUS_MOVEMENTS_JSONL": os.path.join(TEST_ROOT, "movements.jsonl"),
    "OCTOPUS_BLOBS_DIR": os.path.join(TEST_ROOT, "blobs"),
    "OCTOPUS_LEDGER_INDEX_SQLITE": os.path.join(TEST_ROOT, "ledger-index.sqlite"),
    "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"registry_interface_root": os.path.join(TEST_ROOT, "registry-interface"), "registry_ee_template_root": os.path.join(TEST_ROOT, "registry-ee-template"), "registry_tool_root": os.path.join(TEST_ROOT, "registry-tool"), "registry_worker_environment_kind_root": os.path.join(TEST_ROOT, "registry-wek"), "run_records_root": os.path.join(TEST_ROOT, "runs"), "ee_instances_root": os.path.join(TEST_ROOT, "ees")}),
}.items():
    os.environ[name] = value
for name in ("blobs", "registry-interface", "registry-ee-template", "registry-tool", "registry-wek", "runs", "ees"):
    os.makedirs(os.path.join(TEST_ROOT, name), exist_ok=True)

import matrix_contract
import matrix_bootstrap
import matrix_crypto
import matrix_life
import matrix_rendering
import matrix_wire


with open(PACKAGE_DIR / "config.json", "r", encoding="utf-8") as handle:
    BASE_CONFIG = json.load(handle)


def seed_topology_sources():
    root = pathlib.Path(TEST_ROOT)
    owners = BASE_CONFIG["private_run"]["interface_owners"]
    fixtures = {
        owners["matrix"]: {"id": owners["matrix"], "interface_type": "world", "role": "service", "direction": "bidirectional", "source_names": ["matrix"], "interface_driver_ref": "matrix_life"},
        owners["provider"]: {"id": owners["provider"], "interface_type": "world", "role": "provider", "direction": "bidirectional", "source_names": ["fixture.matrix_provider"], "interface_driver_ref": "fixture_provider", "interface_driver_config": {"private_config": {"owner_marker": "matrix"}}, "interface_policy": {}},
        owners["interface_authoring"]: {"id": owners["interface_authoring"], "interface_type": "system", "role": "system", "direction": "bidirectional", "source_names": ["interface_authoring"], "interface_driver_ref": "fixture_authoring"},
        owners["worker_environment"]: {"id": owners["worker_environment"], "interface_type": "system", "role": "system", "direction": "bidirectional", "source_names": ["system_room"], "interface_driver_ref": "system_room"},
        owners["worker"]: {"id": owners["worker"], "interface_type": "system", "role": "worker", "direction": "bidirectional", "source_names": ["matrix"]},
    }
    for identifier, descriptor in fixtures.items():
        owner_dir = root / "registry-interface" / identifier
        owner_dir.mkdir(exist_ok=True)
        (owner_dir / "interface.json").write_text(json.dumps(descriptor), encoding="utf-8")
        if descriptor.get("role") != "worker":
            (owner_dir / "life.json").write_text(json.dumps({"processes": [{"bin": f"fixture-{identifier}", "args": []}]}), encoding="utf-8")
    gateway_tool = BASE_CONFIG["private_run"]["file_write_tool"]
    worker_policy = {"id": owners["worker"], "ceiling": {"interfaces": [owners["matrix"], owners["provider"], owners["interface_authoring"], owners["worker_environment"], owners["worker"]], "tools": ["artifact.read", "artifact.send", gateway_tool], "wek": ["system_room"], "mark_outcomes": ["tool.call.allowed"], "hard_blocked_tools": []}, "role_artifacts": BASE_CONFIG["private_run"]["role_artifacts"]}
    (root / "registry-interface" / owners["worker"] / "policy.json").write_text(json.dumps(worker_policy), encoding="utf-8")
    tool_dir = root / "registry-tool" / gateway_tool
    tool_dir.mkdir(exist_ok=True)
    (tool_dir / "tool.json").write_text(json.dumps({"id": gateway_tool, "interface": owners["worker_environment"], "process": {"bin": "fixture-file-write", "authority": {}}, "supported_worker_environment_kinds": ["system_room"], "description": "fixture", "args_schema": {"type": "object"}, "is_world_effect": True}), encoding="utf-8")
    template_owner = BASE_CONFIG["private_run"]["ee_template_owner"]
    template_dir = root / "registry-ee-template" / template_owner
    template_dir.mkdir(exist_ok=True)
    (template_dir / "ee_template.json").write_text(json.dumps({"id": template_owner, "goal_class": "fixture", "allowed_interfaces": list(fixtures), "allowed_worker_environment_kinds": ["system_room"], "allowed_root_slots": ["run_records_root"], "hard_blocked_tools": [], "allowed_child_ee_templates": [], "bootstrap_root": False}), encoding="utf-8")
    existence_dir = root / "registry-ee-template" / "existence"
    existence_dir.mkdir(exist_ok=True)
    (existence_dir / "ee_template.json").write_text(json.dumps({"id": "existence", "goal_class": "fixture", "allowed_interfaces": list(fixtures), "allowed_worker_environment_kinds": ["system_room"], "hard_blocked_tools": [], "allowed_child_ee_templates": [template_owner], "bootstrap_root": True}), encoding="utf-8")
    (root / "ees" / "ee_existence.json").write_text(json.dumps({"ee_id": "ee_existence", "template_name": "existence", "parent_ee_id": None, "goal": "existence"}), encoding="utf-8")
    kind_dir = root / "registry-wek" / "system_room"
    kind_dir.mkdir(exist_ok=True)
    (kind_dir / "worker_environment_kind.json").write_text(json.dumps({"worker_environment_kind_id": "system_room", "substrate_family": "process_exec", "supported_executor_kinds": ["exec"], "capabilities": ["filesystem"], "boundary_defaults": {"read_root_refs": [], "write_root_refs": [], "protected_read_only_subpaths": [], "network_ceiling": "denied"}, "lifecycle": {"requires_release": False}}), encoding="utf-8")


seed_topology_sources()


class MatrixContractTests(unittest.TestCase):
    def test_topology_ownership_rotates_with_cas_and_rejects_drift(self):
        target = pathlib.Path(TEST_ROOT) / "registry-interface" / "world_matrix_private_rotation" / "life.json"
        target.parent.mkdir(exist_ok=True)
        before = '{"runtime":"lock-one"}\n'
        after = '{"runtime":"lock-two"}\n'
        matrix_life.atomic_bytes(str(target), before.encode("utf-8"))
        matrix_life.commit_topology_ownership(BASE_CONFIG, {str(target): before})
        writes = matrix_life.missing_topology_writes(BASE_CONFIG, {str(target): after})
        self.assertEqual(writes, [{"path": str(target), "content": after, "expected_before_hash": matrix_life.octopus_content_hash(before.encode("utf-8"))}])
        matrix_life.atomic_bytes(str(target), after.encode("utf-8"))
        matrix_life.commit_topology_ownership(BASE_CONFIG, {str(target): after})
        matrix_life.atomic_bytes(str(target), b'{"runtime":"operator-drift"}\n')
        with self.assertRaisesRegex(RuntimeError, "drifted from its last package-authored bytes"):
            matrix_life.missing_topology_writes(BASE_CONFIG, {str(target): before})
        target.unlink()
        with self.assertRaisesRegex(RuntimeError, "disappeared"):
            matrix_life.missing_topology_writes(BASE_CONFIG, {str(target): before})
        known = {"schema_version": 1, "bindings": {"known": {"room_id": "!known:example.org", "thread_event_id": None}}}
        hub = object()
        with mock.patch.object(matrix_life, "load_bindings", return_value=known), mock.patch.object(matrix_life, "ensure_private_binding") as ensure:
            matrix_life.reconcile_known_bindings(BASE_CONFIG, hub)
        ensure.assert_called_once_with(BASE_CONFIG, hub, "!known:example.org", None)

    def test_provider_child_is_a_complete_concrete_interface(self):
        owner = BASE_CONFIG["private_run"]["interface_owners"]["provider"]
        descriptor = matrix_life.provider_descriptor("world_provider_private", owner, {"model": "fixture-model"})
        self.assertNotIn("reference", descriptor)
        self.assertEqual(descriptor["id"], "world_provider_private")
        self.assertEqual(descriptor["source_names"], ["fixture.matrix_provider"])
        self.assertEqual(descriptor["interface_type"], "world")
        self.assertEqual(descriptor["role"], "provider")
        self.assertEqual(descriptor["interface_driver_ref"], "fixture_provider")
        self.assertEqual(descriptor["interface_driver_config"]["private_config"]["owner_marker"], "matrix")
        self.assertEqual(descriptor["interface_driver_config"]["private_config"]["model"], "fixture-model")
        secret = pathlib.Path(TEST_ROOT) / "var" / "private-secrets" / "fixture.key"
        secret.parent.mkdir(parents=True, exist_ok=True)
        secret.write_text("0" * 64 + "\n", encoding="ascii")
        secret.chmod(0o600)
        process = matrix_life.child_life_json(str(secret))["processes"][0]
        self.assertEqual(process["io_limits"]["max_payload_bytes"], 268435456)
        self.assertEqual(process["args"][0]["repository_file"], "registry/interface/world_matrix/matrix_bootstrap.py")
        self.assertEqual(process["env"]["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"], os.path.realpath(os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"]))
        self.assertIn(process["env"]["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"], process["authority"]["read_paths"])
        self.assertEqual(process["authority"]["executable_paths"], [process["env"]["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"]])
        self.assertFalse(process["authority"]["allow_network"])
        gateway = json.loads((PACKAGE_DIR / "life.json").read_text(encoding="utf-8"))["processes"][0]
        self.assertEqual(gateway["private_var_directories"], ["python-environment", "python-environment-staging"])
        self.assertEqual(gateway["authority"]["executable_env_paths"], [])
        self.assertEqual(gateway["authority"]["executable_env_subpaths"], [{"env": "OCTOPUS_VAR_DIR", "relative_path": "python-environment"}, {"env": "OCTOPUS_VAR_DIR", "relative_path": "python-environment-staging"}])

    def test_private_topology_owns_exact_surfaces_and_policy(self):
        binding = matrix_life.ensure_binding_record(BASE_CONFIG, "!exact:example.org", "$thread", "127.0.0.1", 32742)
        topology = matrix_life.binding_topology(BASE_CONFIG, binding)
        roots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        interface_root = pathlib.Path(os.path.realpath(roots["registry_interface_root"]))
        template_id = BASE_CONFIG["private_run"]["id_prefixes"]["ee_template"] + binding["opaque_id"]
        authoring = binding["worker_environment_interface"]
        exact_interfaces = {binding["interface"], binding["provider_interface"], binding["interface_authoring_interface"], authoring, binding["worker_interface"]}
        for identifier in (binding["interface"], binding["provider_interface"], binding["interface_authoring_interface"], binding["worker_environment_interface"], binding["worker_interface"]):
            descriptor = json.loads(topology[str(interface_root / identifier / "interface.json")])
            self.assertEqual(descriptor["id"], identifier)
            self.assertNotIn("reference", descriptor)
        self.assertIn(str(interface_root / binding["provider_interface"] / "life.json"), topology)
        self.assertIn(str(interface_root / binding["interface_authoring_interface"] / "life.json"), topology)
        self.assertIn(str(interface_root / authoring / "interface.json"), topology)
        self.assertIn(str(interface_root / authoring / "life.json"), topology)
        tool_root = pathlib.Path(os.path.realpath(roots["registry_tool_root"]))
        private_tool = json.loads(topology[str(tool_root / binding["file_write_tool"] / "tool.json")])
        self.assertEqual(private_tool["id"], binding["file_write_tool"])
        self.assertEqual(private_tool["interface"], binding["worker_environment_interface"])
        private_paths = [write["path"] for write in matrix_life.missing_topology_writes(BASE_CONFIG, matrix_life.package_owned_topology(topology))]
        private_paths.extend(write["path"] for write in matrix_life.private_state_writes(BASE_CONFIG, binding, topology))
        for sidecar in (str(interface_root / binding["interface"] / "config.json"), str(interface_root / binding["interface"] / "life.json")):
            self.assertLess(private_paths.index(sidecar), private_paths.index(str(interface_root / binding["interface"] / "interface.json")))
        self.assertLess(private_paths.index(str(interface_root / binding["provider_interface"] / "life.json")), private_paths.index(str(interface_root / binding["provider_interface"] / "interface.json")))
        for sidecar in (str(interface_root / binding["interface_authoring_interface"] / "life.json"), str(interface_root / binding["interface_authoring_interface"] / "activation.json")):
            self.assertLess(private_paths.index(sidecar), private_paths.index(str(interface_root / binding["interface_authoring_interface"] / "interface.json")))
        self.assertLess(private_paths.index(str(interface_root / binding["worker_interface"] / "policy.json")), private_paths.index(str(interface_root / binding["worker_interface"] / "interface.json")))
        worker_policy = json.loads(topology[str(interface_root / binding["worker_interface"] / "policy.json")])
        self.assertTrue(exact_interfaces.issubset(worker_policy["ceiling"]["interfaces"]))
        self.assertIn("artifact.read", worker_policy["ceiling"]["tools"])
        self.assertIn(binding["file_write_tool"], worker_policy["ceiling"]["tools"])
        self.assertNotIn(BASE_CONFIG["private_run"]["file_write_tool"], worker_policy["ceiling"]["tools"])
        self.assertNotIn("file.write", worker_policy["ceiling"]["tools"])
        self.assertNotIn("system_room", worker_policy["ceiling"]["interfaces"])
        for owner in BASE_CONFIG["private_run"]["interface_owners"].values():
            if owner != BASE_CONFIG["private_run"]["interface_owners"]["operator"]:
                self.assertNotIn(owner, worker_policy["ceiling"]["interfaces"])
        template_root = pathlib.Path(os.path.realpath(roots["registry_ee_template_root"]))
        template = json.loads(topology[str(template_root / template_id / "ee_template.json")])
        self.assertEqual(template["id"], template_id)
        self.assertEqual(set(template["allowed_interfaces"]), exact_interfaces)
        run_root = pathlib.Path(os.path.realpath(roots["run_records_root"]))
        ee_root = pathlib.Path(os.path.realpath(roots["ee_instances_root"]))
        self.assertEqual(private_paths[-3:], [str(template_root / template_id / "ee_template.json"), str(ee_root / f"{binding['ee']}.json"), str(run_root / f"{binding['run']}.json")])
        run = json.loads(topology[str(run_root / f"{binding['run']}.json")])
        authoring_call = next(row for row in run["checkpoints"] if row["id"].endswith("_authoring_call"))
        authoring_result = next(row for row in run["checkpoints"] if row["id"].endswith("_authoring_result"))
        self.assertEqual(authoring_call["outbound_interface_bindings"][0]["interface"], binding["worker_environment_interface"])
        self.assertEqual(authoring_result["interface_binding"]["interface"], binding["worker_environment_interface"])
        original_parent = json.loads((template_root / "existence" / "ee_template.json").read_text(encoding="utf-8"))
        shared = matrix_life.shared_policy_writes(BASE_CONFIG, binding)
        shared_by_path = {row["path"]: json.loads(row["content"]) for row in shared}
        parent = shared_by_path[str(template_root / "existence" / "ee_template.json")]
        self.assertEqual(parent["allowed_interfaces"][:len(original_parent["allowed_interfaces"])], original_parent["allowed_interfaces"])
        self.assertEqual(parent["allowed_child_ee_templates"][:len(original_parent["allowed_child_ee_templates"])], original_parent["allowed_child_ee_templates"])
        self.assertTrue(exact_interfaces.issubset(parent["allowed_interfaces"]))
        self.assertIn(template_id, parent["allowed_child_ee_templates"])
        kind_root = pathlib.Path(os.path.realpath(roots["registry_worker_environment_kind_root"]))
        self.assertNotIn(str(kind_root / "system_room" / "worker_environment_kind.json"), shared_by_path)

    def test_legacy_binding_door_axis_is_rejected(self):
        path = os.path.join(TEST_ROOT, "legacy-bindings.json")
        with mock.patch.object(matrix_life, "BINDINGS_PATH", path):
            binding = matrix_life.ensure_binding_record(BASE_CONFIG, "!legacy:example.org", None, "127.0.0.1", 32742)
            state = json.loads(pathlib.Path(path).read_text(encoding="utf-8"))
            state["bindings"][matrix_life.coordinate_key("!legacy:example.org", None)]["authoring_environment_interface"] = "system_room"
            matrix_life.atomic_json(path, state, indent=2)
            with self.assertRaisesRegex(ValueError, "unknown keys"):
                matrix_life.load_bindings()

    def test_shared_policy_wait_detects_fresh_cas_conflict(self):
        attempted = {"/shared.json": "1" * 64}
        pending = [{"path": "/shared.json", "content": "{}", "expected_before_hash": "2" * 64}]
        with mock.patch.object(matrix_life, "shared_policy_writes", return_value=pending):
            self.assertFalse(matrix_life.wait_for_shared_policy_admissions(BASE_CONFIG, {}, attempted))

    def test_existing_private_run_with_legacy_door_fails_closed(self):
        binding = matrix_life.ensure_binding_record(BASE_CONFIG, "!legacy-run:example.org", None, "127.0.0.1", 32742)
        topology = matrix_life.binding_topology(BASE_CONFIG, binding)
        roots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        ee_path = str(pathlib.Path(os.path.realpath(roots["ee_instances_root"])) / f"{binding['ee']}.json")
        run_path = str(pathlib.Path(os.path.realpath(roots["run_records_root"])) / f"{binding['run']}.json")
        pathlib.Path(ee_path).write_text(topology[ee_path], encoding="utf-8")
        run = json.loads(topology[run_path])
        run["checkpoints"][1]["outbound_interface_bindings"] = [matrix_contract.authored_interface_binding("system_context_matrix_legacy")]
        pathlib.Path(run_path).write_text(json.dumps(run), encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "stale or foreign outbound"):
            matrix_life.private_state_writes(BASE_CONFIG, binding, topology)

    def test_authoring_cas_conflict_recomputes_shared_writes(self):
        config = copy.deepcopy(BASE_CONFIG)
        config["private_run"]["authoring_retry_attempts"] = 2
        binding = {"opaque_id": "b" * 32, "status": "ready"}
        hub = mock.Mock(host="127.0.0.1", port=32742)
        hub.wait_for.return_value = object()
        shared = [[{"path": "/shared-first.json", "content": "{}", "expected_before_hash": "1" * 64}], [{"path": "/shared-second.json", "content": "{}", "expected_before_hash": "2" * 64}]]
        with mock.patch.object(matrix_life, "ensure_binding_record", return_value=binding), mock.patch.object(matrix_life, "binding_topology", return_value={}) as topology, mock.patch.object(matrix_life, "missing_topology_writes", side_effect=[[], []]), mock.patch.object(matrix_life, "private_state_writes", side_effect=[[], []]), mock.patch.object(matrix_life, "shared_policy_writes", side_effect=shared) as shared_writes, mock.patch.object(matrix_life, "wait_for_shared_policy_admissions", side_effect=[False, True]) as wait_shared, mock.patch.object(matrix_life, "gateway_rearm_write", return_value={"path": "/runs/gateway.json", "content": "{}", "expected_before_hash": "f" * 64}), mock.patch.object(matrix_life, "hand_json") as hand, mock.patch.object(matrix_life, "commit_topology_ownership"):
            matrix_life.ensure_private_binding(config, hub, "!retry:example.org", None)
        self.assertEqual(topology.call_count, 2)
        self.assertEqual(shared_writes.call_count, 2)
        self.assertEqual(wait_shared.call_count, 2)
        self.assertEqual(hand.call_count, 2)
        self.assertEqual(hand.call_args.args[1]["arguments"]["writes"][1]["path"], "/shared-second.json")

    def test_authoring_batch_orders_gateway_private_topology_then_shared_admissions(self):
        binding = {"opaque_id": "a" * 32, "status": "declared"}
        hub = mock.Mock(host="127.0.0.1", port=32742)
        hub.wait_for.return_value = object()
        topology_writes = [{"path": "/registry/interface/private/interface.json", "content": "{}", "expected_before_hash": None}]
        state_writes = [{"path": "/ees/private.json", "content": "{}", "expected_before_hash": None}, {"path": "/runs/private.json", "content": "{}", "expected_before_hash": None}]
        shared_writes = [{"path": "/registry/ee-template/parent.json", "content": "{}", "expected_before_hash": "1" * 64}, {"path": "/registry/wek/system_room.json", "content": "{}", "expected_before_hash": "2" * 64}]
        with mock.patch.object(matrix_life, "ensure_binding_record", return_value=binding), mock.patch.object(matrix_life, "binding_topology", return_value={}), mock.patch.object(matrix_life, "missing_topology_writes", return_value=topology_writes), mock.patch.object(matrix_life, "private_state_writes", return_value=state_writes), mock.patch.object(matrix_life, "shared_policy_writes", return_value=shared_writes), mock.patch.object(matrix_life, "wait_for_shared_policy_admissions", return_value=True), mock.patch.object(matrix_life, "gateway_rearm_write", return_value={"path": "/runs/gateway.json", "content": "{}", "expected_before_hash": "f" * 64}), mock.patch.object(matrix_life, "hand_json") as hand, mock.patch.object(matrix_life, "mark_binding_ready"):
            matrix_life.ensure_private_binding(BASE_CONFIG, hub, "!order:example.org", None)
        arguments = hand.call_args.args[1]["arguments"]["writes"]
        self.assertEqual([write["path"] for write in arguments], ["/runs/gateway.json", "/registry/interface/private/interface.json", "/ees/private.json", "/runs/private.json", "/registry/ee-template/parent.json", "/registry/wek/system_room.json"])

    def test_binding_child_owns_final_admission_and_receipt(self):
        for path in (matrix_life.BINDING_EVENTS_PATH, matrix_life.BINDING_CONTEXT_PATH):
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
        config = copy.deepcopy(BASE_CONFIG)
        config["mode"] = "binding"
        config["binding"] = {"interface": "world_matrix_private", "room_id": "!room:example.org", "thread_event_id": None, "run": "run_matrix_private", "gateway_host": "127.0.0.1", "gateway_port": 32742, "secret_file": "/tmp/matrix.secret"}
        event = {"type": "m.room.message", "event_id": "$private", "room_id": "!room:example.org", "sender": "@alice:example.org", "origin_server_ts": 1, "content": {"msgtype": "m.text", "body": "hello"}, "room": {"is_direct": True}, "_octopus_sync_source": "test", "_octopus_sync_index": 0}
        raw = matrix_contract.canonical_json(event)
        key = matrix_contract.event_key(event)
        digest = hashlib.sha256(raw).hexdigest()
        candidate = {"schema_version": 1, "own_user_id": "@octopus:example.org", "records": [{"event_key": key, "raw_sha256": digest, "event": event, "route_thread_event_id": None, "media": [], "reply_context": None}]}
        with mock.patch.dict(os.environ, {"MATRIX_ALLOW_ALL_USERS": "true", "MATRIX_ALLOWED_USERS": "", "MATRIX_ALLOWED_ROOMS": "", "MATRIX_ALLOWED_SERVERS": ""}, clear=False), mock.patch.object(matrix_life, "hand_json_idempotent", return_value=83) as hand:
            self.assertEqual(matrix_life.handle_binding_arrival(config, candidate), 83)
            self.assertEqual(matrix_life.handle_binding_arrival(config, candidate), 83)
        hand.assert_called_once()
        handed = hand.call_args.args[1]
        self.assertEqual(handed["text"], "hello")
        self.assertTrue(handed["evidence"]["raw_event_path"].startswith(matrix_life.RAW_EVENT_DIR + os.sep))
        changed = json.loads(json.dumps(candidate))
        changed["records"][0]["reply_context"] = {"event_id": "$forged", "error": "forged"}
        with mock.patch.dict(os.environ, {"MATRIX_ALLOW_ALL_USERS": "true"}, clear=False), self.assertRaisesRegex(ValueError, "reply context"):
            matrix_life.handle_binding_arrival(config, changed)

    def test_private_dispatch_journal_pins_retry_batch(self):
        try:
            os.unlink(matrix_life.PRIVATE_DISPATCH_PATH)
        except FileNotFoundError:
            pass
        config = copy.deepcopy(BASE_CONFIG)
        records = [
            {"key": "$first", "raw_sha256": "1" * 64, "created_at_unix_ms": 1, "available_after_unix_ms": 1},
            {"key": "$second", "raw_sha256": "2" * 64, "created_at_unix_ms": 2, "available_after_unix_ms": 2},
        ]
        coordinate = ("!room:example.org", None)
        matrix_life.pin_private_dispatch(config, coordinate, records)
        service = object.__new__(matrix_life.MatrixService)
        service.config = config
        groups = service.group_pending(list(reversed(records)), 10)
        self.assertEqual([[record["key"] for record in group] for group in groups], [["$first", "$second"]])
        matrix_life.clear_private_dispatch(config, coordinate, records)
        self.assertFalse(os.path.exists(matrix_life.PRIVATE_DISPATCH_PATH))

    def test_dependency_subprocess_environment_is_allowlisted(self):
        proxy = "https://person:secret@proxy.example:8443"
        inherited = {
            "PATH": "/usr/bin:/bin",
            "TMPDIR": "/tmp/matrix-dependencies",
            "REQUESTS_CA_BUNDLE": "/etc/ssl/matrix-ca.pem",
            "MATRIX_ACCESS_TOKEN": "matrix-secret",
            "MATRIX_RECOVERY_KEY": "recovery-secret",
            "OCTOPUS_VAR_DIR": "/private/interface-state",
            "OCTOPUS_MOVEMENTS_JSONL": "/private/session/movements.jsonl",
            "PYTHONPATH": "/ambient/python",
            "HTTP_PROXY": "http://ambient-proxy.example",
        }
        with mock.patch.dict(os.environ, inherited, clear=True):
            environment = matrix_bootstrap.dependency_process_environment(proxy)
        self.assertEqual(environment["PATH"], inherited["PATH"])
        self.assertEqual(environment["TMPDIR"], inherited["TMPDIR"])
        self.assertEqual(environment["REQUESTS_CA_BUNDLE"], inherited["REQUESTS_CA_BUNDLE"])
        self.assertEqual(environment["HTTP_PROXY"], proxy)
        self.assertEqual(environment["HTTPS_PROXY"], proxy)
        for name in ("MATRIX_ACCESS_TOKEN", "MATRIX_RECOVERY_KEY", "OCTOPUS_VAR_DIR", "OCTOPUS_MOVEMENTS_JSONL", "PYTHONPATH"):
            self.assertNotIn(name, environment)

    def test_secret_transport_requires_tls_outside_loopback(self):
        wire = matrix_wire.MatrixWire(copy.deepcopy(BASE_CONFIG), TEST_ROOT, lambda _event: None)
        variables = {
            "MATRIX_HOMESERVER": "http://matrix.example",
            "MATRIX_ACCESS_TOKEN": "secret",
            "MATRIX_USER_ID": "",
            "MATRIX_PASSWORD": "",
            "MATRIX_DEVICE_ID": "",
        }
        with mock.patch.dict(os.environ, variables, clear=False):
            with self.assertRaisesRegex(matrix_wire.MatrixWireFailure, "HTTPS outside loopback"):
                wire.resolve_credentials()
            os.environ["MATRIX_HOMESERVER"] = "http://127.0.0.1:8008"
            wire.resolve_credentials()
            self.assertEqual(wire.homeserver, "http://127.0.0.1:8008")

    def test_config_and_deny_default_authorization(self):
        config = matrix_contract.validate_config(copy.deepcopy(BASE_CONFIG), "world_matrix")
        event = {
            "type": "m.room.message",
            "event_id": "$event",
            "room_id": "!room:example.org",
            "sender": "@alice:example.org",
            "content": {"msgtype": "m.text", "body": "hello"},
            "room": {"is_direct": True},
        }
        names = {
            "MATRIX_ALLOWED_USERS": "",
            "MATRIX_ALLOWED_ROOMS": "",
            "MATRIX_ALLOWED_SERVERS": "",
            "MATRIX_ALLOWED_INVITERS": "",
            "MATRIX_FREE_RESPONSE_ROOMS": "",
            "MATRIX_IGNORE_USER_PATTERNS": "",
        }
        with mock.patch.dict(os.environ, names, clear=False):
            os.environ.pop("MATRIX_ALLOW_ALL_USERS", None)
            self.assertEqual(matrix_contract.authorized(config, event, "@octopus:example.org"), (False, "user_not_allowed"))
            os.environ["MATRIX_ALLOWED_USERS"] = "@alice:example.org"
            self.assertEqual(matrix_contract.authorized(config, event, "@octopus:example.org"), (True, "allowed"))

    def test_binding_registry_is_coordinate_strict(self):
        value = {
            "schema_version": 1,
            "bindings": [
                {
                    "interface": "world_matrix_private_1",
                    "room_id": "!room:example.org",
                    "thread_event_id": "$root",
                    "secret_sha256": "a" * 64,
                }
            ],
        }
        result = matrix_contract.binding_registry(value)
        self.assertEqual(result[("!room:example.org", "$root")]["interface"], "world_matrix_private_1")
        value["bindings"][0]["room_id"] = "#alias:example.org"
        with self.assertRaisesRegex(ValueError, "concrete room"):
            matrix_contract.binding_registry(value)

    def test_thread_reply_edit_and_reaction_coordinates(self):
        threaded = {"room_id": "!room:example.org", "content": {"m.relates_to": {"rel_type": "m.thread", "event_id": "$root", "m.in_reply_to": {"event_id": "$parent"}}}}
        self.assertEqual(matrix_contract.event_thread(threaded), "$root")
        self.assertEqual(matrix_contract.event_reply(threaded), "$parent")
        state = matrix_life.default_state()
        state["event_threads"]["$message"] = "$root"
        reaction = {"type": "m.reaction", "room_id": "!room:example.org", "content": {"m.relates_to": {"rel_type": "m.annotation", "event_id": "$message", "key": "👍"}}}
        self.assertEqual(matrix_life.resolve_coordinate(BASE_CONFIG, reaction, state), ("!room:example.org", "$root"))
        reply = {"type": "m.room.message", "room_id": "!room:example.org", "content": {"m.relates_to": {"m.in_reply_to": {"event_id": "$message"}}}}
        self.assertEqual(matrix_life.resolve_coordinate(BASE_CONFIG, reply, state), ("!room:example.org", "$root"))
        operation, args = matrix_contract.normalize_operation({"text": "reply"}, BASE_CONFIG, {"room_id": "!room:example.org", "thread_event_id": "$root", "reply_to_event_id": "$message"})
        self.assertEqual(operation, "message.send")
        self.assertEqual(args["reply_to_event_id"], "$message")

    def test_senderless_ephemeral_events_are_split_before_authorization(self):
        event = {
            "type": "m.receipt",
            "room_id": "!room:example.org",
            "content": {"$event": {"m.read": {"@alice:example.org": {"ts": 1}, "@bob:example.org": {"ts": 2}}}},
        }
        rows = matrix_life.expand_ephemeral(event)
        self.assertEqual({row["sender"] for row in rows}, {"@alice:example.org", "@bob:example.org"})
        self.assertTrue(all(len(next(iter(row["content"].values()))["m.read"]) == 1 for row in rows))

    def test_html_mentions_and_chunking_remain_safe(self):
        content = matrix_rendering.build_text_content("Hi @alice:example.org\n\n```py\nprint(1)\n``` <script>x</script> [x](javascript:boom)")
        self.assertEqual(content["m.mentions"]["user_ids"], ["@alice:example.org"])
        formatted = content["formatted_body"]
        self.assertNotIn("<script", formatted)
        self.assertNotIn("javascript:", formatted)
        self.assertIn("matrix.to", formatted)
        chunks = matrix_rendering.split_text("```py\n" + "x" * 120 + "\n```", 50)
        self.assertGreater(len(chunks), 1)
        self.assertTrue(all(len(chunk) <= 58 for chunk in chunks))

    def test_private_operations_cannot_escape_coordinate(self):
        endpoint = SimpleNamespace(room_id="!room:example.org", thread_event_id="$root")
        state = matrix_life.default_state()
        state["event_threads"]["$inside"] = "$root"
        matrix_life.atomic_json(matrix_life.STATE_PATH, state)
        args = matrix_life.enforce_binding_operation(BASE_CONFIG, endpoint, "message.send", {"text": "hi"})
        self.assertEqual(args["room_id"], endpoint.room_id)
        self.assertEqual(args["thread_event_id"], endpoint.thread_event_id)
        args = matrix_life.enforce_binding_operation(BASE_CONFIG, endpoint, "message.edit", {"event_id": "$inside", "text": "fixed"})
        self.assertEqual(args["event_id"], "$inside")
        with self.assertRaisesRegex(ValueError, "leave its bound room"):
            matrix_life.enforce_binding_operation(BASE_CONFIG, endpoint, "message.send", {"room_id": "!other:example.org", "text": "hi"})
        with self.assertRaisesRegex(ValueError, "outside its bound thread"):
            matrix_life.enforce_binding_operation(BASE_CONFIG, endpoint, "message.redact", {"event_id": "$outside"})
        with self.assertRaisesRegex(ValueError, "not permitted"):
            matrix_life.enforce_binding_operation(BASE_CONFIG, endpoint, "room.create", {})
        history = matrix_life.filter_binding_result(
            BASE_CONFIG,
            endpoint,
            "room.history",
            {
                "events": [
                    {"event_id": "$root", "content": {}},
                    {"event_id": "$reply", "content": {"m.relates_to": {"rel_type": "m.thread", "event_id": "$root"}}},
                    {"event_id": "$outside", "content": {}},
                ]
            },
        )
        self.assertEqual([event["event_id"] for event in history["events"]], ["$root", "$reply"])

    def test_ledger_fallback_does_not_advance_before_cursor_commit(self):
        foreign = {
            "seq": 5,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_foreign"},
            "to": {"Interface": "world_other"},
            "invocation_metadata": {"runtime": 7},
        }
        legacy = {
            "seq": 6,
            "kind": "InterfaceOutbound",
            "from": {"Run": "run_legacy"},
            "to": {"Interface": "world_matrix"},
            "invocation_metadata": {"runtime": 7},
        }
        movement = {
            "seq": 7,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_matrix"},
            "invocation_metadata": {
                "runtime": {
                    "run": "run_matrix",
                    "ee": "ee_matrix",
                    "worker": "system_worker_matrix",
                    "checkpoint": "cp_matrix",
                    "interface": "world_matrix",
                    "epoch": 0,
                }
            },
            "artifact": {"content": [0] * 32},
        }
        pathlib.Path(matrix_life.MOVEMENTS_JSONL).write_text(
            "".join(json.dumps(row) + "\n" for row in (foreign, legacy, movement)),
            encoding="utf-8",
        )
        pathlib.Path(matrix_life.LEDGER_FALLBACK_CURSOR_PATH).unlink(missing_ok=True)
        expected = [(7, [0] * 32, "run_matrix")]
        maximum = BASE_CONFIG["ledger"]["max_row_bytes"]
        self.assertEqual(matrix_life.read_ledger(7, 128, maximum), expected)
        first_cursor = matrix_life.read_json(matrix_life.LEDGER_FALLBACK_CURSOR_PATH, "Matrix ledger fallback cursor")
        self.assertGreater(first_cursor["byte_offset"], 0)
        self.assertLess(first_cursor["byte_offset"], pathlib.Path(matrix_life.MOVEMENTS_JSONL).stat().st_size)
        self.assertEqual(matrix_life.read_ledger(7, 128, maximum), expected)
        self.assertEqual(matrix_life.read_ledger(8, 128, maximum), [])
        final_cursor = matrix_life.read_json(matrix_life.LEDGER_FALLBACK_CURSOR_PATH, "Matrix ledger fallback cursor")
        self.assertEqual(final_cursor["byte_offset"], pathlib.Path(matrix_life.MOVEMENTS_JSONL).stat().st_size)

        malformed = {
            "seq": 8,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_matrix"},
            "invocation_metadata": {"runtime": {"interface": "world_matrix"}},
        }
        pathlib.Path(matrix_life.MOVEMENTS_JSONL).write_text(
            json.dumps(malformed) + "\n",
            encoding="utf-8",
        )
        pathlib.Path(matrix_life.LEDGER_FALLBACK_CURSOR_PATH).unlink(missing_ok=True)
        with self.assertRaises(ValueError):
            matrix_life.read_ledger(8, 128, maximum)

    def test_ledger_fallback_bounds_rows_before_json_allocation(self):
        path = pathlib.Path(matrix_life.MOVEMENTS_JSONL)
        cursor = pathlib.Path(matrix_life.LEDGER_FALLBACK_CURSOR_PATH)
        cursor.unlink(missing_ok=True)
        path.write_bytes(b"{" + b"x" * 256 + b"}\n")
        with self.assertRaisesRegex(RuntimeError, "exceeds 256 bytes"):
            matrix_life.read_ledger(0, 128, 256)
        cursor.unlink(missing_ok=True)
        path.write_bytes(b'{"seq":0}')
        with self.assertRaisesRegex(RuntimeError, "is partial"):
            matrix_life.read_ledger(0, 128, 256)

    def test_media_uses_content_addressed_blob_and_not_inline_input(self):
        payload = b"binary media"
        digest = matrix_life.octopus_content_hash(payload)
        with open(os.path.join(matrix_life.BLOBS_DIR, digest), "wb") as handle:
            handle.write(payload)
        args = matrix_life.hydrate_media_args(BASE_CONFIG, "media.send", {"room_id": "!room:example.org", "blob_id": digest, "filename": "clip.ogg"})
        self.assertEqual(args["msgtype"], "m.audio")
        self.assertEqual(args["data_base64"], "YmluYXJ5IG1lZGlh")
        with self.assertRaisesRegex(ValueError, "not inline"):
            matrix_life.hydrate_media_args(BASE_CONFIG, "media.send", {"data_base64": "eA=="})

    def test_crypto_pickle_key_is_private_random_state(self):
        with tempfile.TemporaryDirectory() as root:
            crypto = matrix_crypto.MatrixCrypto(BASE_CONFIG["crypto"], root, set())
            first = crypto.pickle_key()
            second = crypto.pickle_key()
            self.assertEqual(first, second)
            self.assertEqual(len(first), 64)
            self.assertEqual(os.stat(os.path.join(root, BASE_CONFIG["crypto"]["store_subdir"], "pickle-key")).st_mode & 0o777, 0o600)

    def test_crypto_store_refuses_symlink(self):
        with tempfile.TemporaryDirectory() as root, tempfile.TemporaryDirectory() as outside:
            os.symlink(outside, os.path.join(root, BASE_CONFIG["crypto"]["store_subdir"]))
            crypto = matrix_crypto.MatrixCrypto(BASE_CONFIG["crypto"], root, set())
            with self.assertRaisesRegex(matrix_crypto.CryptoFailure, "owner-controlled"):
                crypto.pickle_key()

    def test_retry_classification_separates_deterministic_send_from_upload(self):
        self.assertTrue(matrix_life.effect_is_retry_safe(BASE_CONFIG, "message.send"))
        self.assertTrue(matrix_life.effect_is_retry_safe(BASE_CONFIG, "message.redact"))
        self.assertFalse(matrix_life.effect_is_retry_safe(BASE_CONFIG, "media.send"))
        self.assertFalse(matrix_life.effect_is_retry_safe(BASE_CONFIG, "room.create"))

    def test_every_declared_operation_has_wire_implementation(self):
        self.assertEqual(set(BASE_CONFIG["outbound"]["allowed_operations"]), matrix_wire.MatrixWire.SUPPORTED_OPERATIONS)
        tree = ast.parse((PACKAGE_DIR / "matrix_wire.py").read_text(encoding="utf-8"))
        branches = set()
        for node in ast.walk(tree):
            if not isinstance(node, ast.Compare) or not isinstance(node.left, ast.Name) or node.left.id != "operation":
                continue
            for operator, right in zip(node.ops, node.comparators):
                if isinstance(operator, ast.Eq) and isinstance(right, ast.Constant) and isinstance(right.value, str):
                    branches.add(right.value)
                if isinstance(operator, (ast.In, ast.NotIn)) and isinstance(right, (ast.Set, ast.List, ast.Tuple)):
                    branches.update(item.value for item in right.elts if isinstance(item, ast.Constant) and isinstance(item.value, str))
        self.assertEqual(matrix_wire.MatrixWire.SUPPORTED_OPERATIONS, branches)

    def test_proxy_is_explicit_and_ambient_proxy_is_ignored(self):
        fake_aiohttp = types.ModuleType("aiohttp")
        fake_aiohttp.ClientTimeout = mock.Mock(return_value=object())
        fake_aiohttp.ClientSession = mock.Mock(return_value=object())
        wire = matrix_wire.MatrixWire(copy.deepcopy(BASE_CONFIG), TEST_ROOT, lambda _event: None)
        with mock.patch.dict(sys.modules, {"aiohttp": fake_aiohttp}), mock.patch.dict(os.environ, {"HTTP_PROXY": "http://ambient.invalid:8080", "HTTPS_PROXY": "http://ambient.invalid:8080", "MATRIX_PROXY": ""}, clear=False):
            wire.create_session()
        kwargs = fake_aiohttp.ClientSession.call_args.kwargs
        self.assertIs(kwargs["trust_env"], False)
        self.assertNotIn("proxy", kwargs)
        self.assertNotIn("connector", kwargs)

    def test_private_file_and_sanitizer_boundaries(self):
        root = pathlib.Path(TEST_ROOT)
        secret_path = root / "matrix-secret.key"
        secret_path.write_text("secret\n", encoding="utf-8")
        secret_path.chmod(0o600)
        with matrix_life.open_private_read(str(secret_path)) as handle:
            self.assertEqual(handle.read(), b"secret\n")
        link = root / "matrix-secret-link.key"
        link.symlink_to(secret_path)
        with self.assertRaisesRegex(PermissionError, "regular file"):
            matrix_life.open_private_read(str(link))
        token = "matrix-super-secret"
        proxy = "socks5h://proxy-user:proxy-pass@proxy.example:1080"
        with mock.patch.dict(os.environ, {"MATRIX_ACCESS_TOKEN": token, "MATRIX_PROXY": proxy}, clear=False):
            value = matrix_life.sanitize_external_error({token: [token, base64.b64encode(token.encode()).decode(), proxy]}, BASE_CONFIG)
            bootstrap_error = matrix_bootstrap.sanitize(f"{token} {base64.b64encode(token.encode()).decode()} {proxy}")
        encoded = json.dumps(value, sort_keys=True)
        self.assertNotIn(token, encoded)
        self.assertNotIn("proxy-pass", encoded)
        self.assertNotIn(token, bootstrap_error)
        self.assertNotIn("proxy-pass", bootstrap_error)

    def test_bootstrap_profile_is_exact_and_mode_whitespace_fails(self):
        with open(PACKAGE_DIR / "dependency_lock.json", "r", encoding="utf-8") as handle:
            lock = json.load(handle)
        with mock.patch.dict(os.environ, {"MATRIX_E2EE_MODE": "optional"}, clear=False):
            self.assertEqual(matrix_bootstrap.selected_profile(lock, matrix_bootstrap.crypto_mode(BASE_CONFIG))[0]["id"], "darwin-arm64-py313-base")
        with mock.patch.dict(os.environ, {"MATRIX_E2EE_MODE": " optional "}, clear=False):
            with self.assertRaisesRegex(ValueError, "mode is invalid"):
                matrix_bootstrap.crypto_mode(BASE_CONFIG)
        with self.assertRaisesRegex(RuntimeError, "no unique fully locked"):
            matrix_bootstrap.selected_profile(lock, "required")


class MatrixTransportTests(unittest.IsolatedAsyncioTestCase):
    async def test_http_refusal_body_is_reduced_to_digest_evidence(self):
        raw = json.dumps({"errcode": "M_FORBIDDEN", "error": "remote-private-body"}).encode("utf-8")

        class Content:
            async def read(self, _maximum):
                return raw

        class Response:
            status = 403
            content = Content()

            async def __aenter__(self):
                return self

            async def __aexit__(self, _kind, _error, _traceback):
                return False

        class Session:
            def request(self, _method, _url, **_kwargs):
                return Response()

        wire = matrix_wire.MatrixWire(copy.deepcopy(BASE_CONFIG), TEST_ROOT, lambda _event: None)
        wire.session = Session()
        wire.homeserver = "https://matrix.example"
        wire.access_token = "token"
        with self.assertRaises(matrix_wire.MatrixWireFailure) as raised:
            await wire.http_json("GET", "/_matrix/client/v3/account/whoami")
        error = raised.exception
        self.assertEqual(error.details["body_bytes"], len(raw))
        self.assertEqual(error.details["body_sha256"], hashlib.sha256(raw).hexdigest())
        self.assertNotIn("remote-private-body", str(error))
        self.assertNotIn("remote-private-body", json.dumps(error.evidence()))

    async def test_media_download_enforces_limit_plus_one_and_cleans_staging(self):
        config = copy.deepcopy(BASE_CONFIG)
        config["media"]["max_download_bytes"] = 5

        class Content:
            def __init__(self):
                self.chunks = [b"abc", b"def"]

            async def read(self, maximum):
                if not self.chunks:
                    return b""
                chunk = self.chunks.pop(0)
                self.assert_chunk = maximum
                return chunk

        class Response:
            status = 200
            headers = {}

            def __init__(self):
                self.content = Content()

            async def __aenter__(self):
                return self

            async def __aexit__(self, _kind, _error, _traceback):
                return False

        class Session:
            def get(self, _url, **_kwargs):
                return Response()

        with tempfile.TemporaryDirectory() as root:
            wire = matrix_wire.MatrixWire(config, root, lambda _event: None)
            wire.session = Session()
            wire.homeserver = "https://matrix.example"
            wire.access_token = "token"
            with self.assertRaisesRegex(matrix_wire.MatrixWireFailure, "byte ceiling"):
                await wire.download_media_bytes("/_matrix/client/v1/media/download/example/id")
            self.assertEqual(list((pathlib.Path(root) / "media-downloads").iterdir()), [])

    async def test_sync_uses_bounded_http_path_before_sdk_dispatch(self):
        config = copy.deepcopy(BASE_CONFIG)
        wire = matrix_wire.MatrixWire(config, TEST_ROOT, lambda _event: None)
        response = {"next_batch": "s1", "rooms": {"join": {}}}
        wire.http_json = mock.AsyncMock(return_value=response)
        wire.refresh_dm_rooms = mock.AsyncMock()
        client = SimpleNamespace(handle_sync=mock.Mock(return_value=[]))
        wire.client = client
        matrix_package = types.ModuleType("mautrix")
        types_package = types.ModuleType("mautrix.types")

        class SyncResponse:
            @staticmethod
            def deserialize(value):
                return {"decoded": value}

        types_package.SyncResponse = SyncResponse
        with mock.patch.dict(sys.modules, {"mautrix": matrix_package, "mautrix.types": types_package}):
            result = await wire.sync("s0")
        self.assertEqual(result, response)
        wire.http_json.assert_awaited_once()
        self.assertEqual(client.handle_sync.call_args.args[0], {"decoded": response})

    async def test_encrypted_room_send_fails_closed_without_crypto(self):
        wire = matrix_wire.MatrixWire(copy.deepcopy(BASE_CONFIG), TEST_ROOT, lambda _event: None)
        wire.client = SimpleNamespace()
        wire.crypto.room_encrypted = mock.AsyncMock(return_value=True)
        matrix_package = types.ModuleType("mautrix")
        types_package = types.ModuleType("mautrix.types")
        types_package.EventType = SimpleNamespace(find=lambda value: value)
        types_package.RoomID = str
        with mock.patch.dict(sys.modules, {"mautrix": matrix_package, "mautrix.types": types_package}):
            with self.assertRaisesRegex(matrix_crypto.CryptoFailure, "without crypto"):
                await wire.send_event("!room:example.org", "m.room.message", {"body": "secret"}, "seed")

    def test_private_ipc_drops_late_response_and_remains_usable(self):
        local, remote = socket.socketpair()
        peer = matrix_life.JsonPeer(local, 4096, 0.01, lambda operation, payload: {})
        failures = []

        def serve():
            try:
                first = matrix_life.read_frame(remote, 4096)
                time.sleep(0.04)
                matrix_life.write_frame(remote, {"type": "response", "request_id": first["request_id"], "ok": True, "result": {"late": True}}, 4096)
                second = matrix_life.read_frame(remote, 4096)
                matrix_life.write_frame(remote, {"type": "response", "request_id": second["request_id"], "ok": True, "result": {"late": False}}, 4096)
            except Exception as error:
                failures.append(error)

        server = threading.Thread(target=serve)
        server.start()
        with self.assertRaises(TimeoutError):
            peer.request("slow", {})
        time.sleep(0.06)
        self.assertEqual(peer.request("fast", {}), {"late": False})
        self.assertEqual(peer.responses, {})
        self.assertEqual(peer.pending_requests, set())
        peer.close()
        remote.close()
        server.join(5)
        self.assertFalse(failures)

    def test_private_ipc_bounds_concurrent_request_handlers(self):
        active = 0
        peak = 0
        lock = threading.Lock()

        def handler(_operation, payload):
            nonlocal active, peak
            with lock:
                active += 1
                peak = max(peak, active)
            time.sleep(0.02)
            with lock:
                active -= 1
            return payload

        local, remote = socket.socketpair()
        local_peer = matrix_life.JsonPeer(local, 1024 * 1024, 3, handler)
        remote_peer = matrix_life.JsonPeer(remote, 1024 * 1024, 3, lambda _operation, payload: payload)
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=24) as executor:
                results = list(executor.map(lambda value: remote_peer.request("bounded", {"n": value}), range(24)))
            self.assertEqual(results, [{"n": value} for value in range(24)])
            self.assertLessEqual(peak, matrix_life.PRIVATE_REQUEST_WORKERS)
        finally:
            local_peer.close()
            remote_peer.close()


if __name__ == "__main__":
    unittest.main()
