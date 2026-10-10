import base64
import copy
import importlib.util
import io
import json
import os
import pathlib
import shutil
import stat
import sys
import tempfile
import unittest
import urllib.error
import urllib.parse
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))
from octopus_content_hash import octopus_content_hash_reference

TEMP = tempfile.TemporaryDirectory()
STATE = pathlib.Path(TEMP.name)
for name in (
    "var",
    "blobs",
    "runs",
    "interfaces",
    "tools",
    "ees",
    "ee-templates",
    "worker-environment-kinds",
):
    (STATE / name).mkdir(parents=True, exist_ok=True, mode=0o700 if name == "var" else 0o755)
(STATE / "movements.jsonl").write_bytes(b"")
for owner in (
    "world_telegram",
    "world_provider_codex",
    "system_interface_authoring",
    "system_worker_telegram",
    "system_room",
    "system_room_telegram_gateway",
):
    target = STATE / "interfaces" / owner
    target.mkdir()
    for filename in ("interface.json", "life.json"):
        source = ROOT.parent / owner / filename
        if source.is_file():
            shutil.copyfile(source, target / filename)
for owner in ("file.write", "file.write.telegram_gateway"):
    target = STATE / "tools" / owner
    target.mkdir()
    shutil.copyfile(
        ROOT.parent.parent / "tool" / owner / "tool.json",
        target / "tool.json",
    )
for template in ("operator_interaction", "existence"):
    target = STATE / "ee-templates" / template
    target.mkdir()
    shutil.copyfile(
        ROOT.parent.parent / "ee_template" / template / "ee_template.json",
        target / "ee_template.json",
    )
kind_target = STATE / "worker-environment-kinds" / "system_room"
kind_target.mkdir()
shutil.copyfile(
    ROOT.parent.parent
    / "worker_environment_kind"
    / "system_room"
    / "worker_environment_kind.json",
    kind_target / "worker_environment_kind.json",
)
(STATE / "ees" / "ee_existence.json").write_text(
    json.dumps(
        {
            "ee_id": "ee_existence",
            "template_name": "existence",
            "parent_ee_id": None,
            "goal": "Exist",
        }
    ),
    encoding="utf-8",
)
os.environ["OCTOPUS_INTERFACE_ID"] = "world_telegram"
os.environ["OCTOPUS_INTERFACE_DIR"] = str(ROOT)
os.environ["OCTOPUS_VAR_DIR"] = str(STATE / "var")
os.environ["OCTOPUS_MOVEMENTS_JSONL"] = str(STATE / "movements.jsonl")
os.environ["OCTOPUS_BLOBS_DIR"] = str(STATE / "blobs")
os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"] = str(STATE / "ledger-index.sqlite")
os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"] = os.path.realpath(sys.executable)
os.environ["OCTOPUS_ROOT_SLOTS_JSON"] = json.dumps(
    {
        "registry_interface_root": str(STATE / "interfaces"),
        "registry_tool_root": str(STATE / "tools"),
        "registry_ee_template_root": str(STATE / "ee-templates"),
        "registry_worker_environment_kind_root": str(
            STATE / "worker-environment-kinds"
        ),
        "run_records_root": str(STATE / "runs"),
        "ee_instances_root": str(STATE / "ees"),
    }
)
SPEC = importlib.util.spec_from_file_location("telegram_life", ROOT / "telegram_life.py")
TELEGRAM = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TELEGRAM)
TELEGRAM.octopus_content_hash = octopus_content_hash_reference


class Api:
    def __init__(self):
        self.calls = []
        self.next_id = 100

    def _result(self, method, params):
        self.calls.append((method, params))
        self.next_id += 1
        return {"message_id": self.next_id}

    def call(self, method, params=None, timeout_s=None):
        return self._result(method, params or {})

    def call_flood_safe(self, method, params, config):
        return self._result(method, params)

    def call_multipart_flood_safe(self, method, params, files, config):
        return self._result(method, params)


class TelegramContractTests(unittest.TestCase):
    def test_policy_union_preserves_owner_and_addition_order(self):
        document = {"allowed_interfaces": ["owner_z", "owner_a", "owner_z"]}
        TELEGRAM._merge_string_list(
            document,
            "allowed_interfaces",
            ["exact_b", "owner_a", "exact_a", "exact_b"],
            "test policy",
        )
        self.assertEqual(
            document["allowed_interfaces"],
            ["owner_z", "owner_a", "owner_z", "exact_b", "exact_a"],
        )

    def test_shared_policy_cas_conflict_reloads_and_retries(self):
        config = copy.deepcopy(TELEGRAM.load_config())
        config["private_run"]["authoring_max_attempts"] = 3
        config["private_run"]["result_timeout_ms"] = 100
        binding = {
            "chat_id": -100808080,
            "thread_id": 8,
            "opaque_id": "8" * 24,
            "interface": "world_telegram_" + "8" * 24,
            "provider_interface": "world_provider_codex_telegram_" + "8" * 24,
            "interface_authoring_interface": "system_interface_authoring_telegram_" + "8" * 24,
            "worker_environment_interface": "system_room_telegram_" + "8" * 24,
            "file_write_tool": "file.write.telegram_" + "8" * 24,
            "worker_interface": "system_worker_telegram_" + "8" * 24,
            "run": "run_telegram_" + "8" * 24,
            "ee": "ee_telegram_" + "8" * 24,
            "host": "127.0.0.1",
            "port": 38080,
            "status": "planned",
        }
        with tempfile.TemporaryDirectory() as root:
            root = pathlib.Path(root)
            roots = {
                "registry_interface_root": str(root / "interfaces"),
                "registry_tool_root": str(root / "tools"),
                "registry_ee_template_root": str(root / "ee-templates"),
                "registry_worker_environment_kind_root": str(root / "worker-environment-kinds"),
                "run_records_root": str(root / "runs"),
                "ee_instances_root": str(root / "ees"),
            }
            for path in roots.values():
                pathlib.Path(path).mkdir(parents=True)
            parent_path = root / "ee-templates" / "existence" / "ee_template.json"
            parent_path.parent.mkdir()
            parent_path.write_text(
                TELEGRAM._json_text(
                    {
                        "id": "existence",
                        "allowed_interfaces": ["owner_z", "owner_a"],
                        "allowed_child_ee_templates": ["child_owner"],
                        "allowed_worker_environment_kinds": ["system_room"],
                    }
                ),
                encoding="utf-8",
            )
            kind_path = (
                root
                / "worker-environment-kinds"
                / "system_room"
                / "worker_environment_kind.json"
            )
            kind_path.parent.mkdir()
            kind_before = TELEGRAM._json_text(
                {
                    "worker_environment_kind_id": "system_room",
                    "executor_kind": "process",
                    "capabilities": ["filesystem"],
                    "boundary": {"allowed_root_slots": ["run_records_root"]},
                    "release": {"mode": "ephemeral"},
                    "transport": {"family": "local_process", "config_schema": {}},
                    "instance_config_schema": {},
                }
            )
            kind_path.write_text(kind_before, encoding="utf-8")
            (root / "ees" / "ee_existence.json").write_text(
                TELEGRAM._json_text(
                    {
                        "ee_id": "ee_existence",
                        "template_name": "existence",
                        "parent_ee_id": None,
                        "goal": "Exist",
                    }
                ),
                encoding="utf-8",
            )
            handed_writes = []

            def fake_hand(manifest_path):
                manifest = json.loads(pathlib.Path(manifest_path).read_text("utf-8"))
                request = json.loads(
                    pathlib.Path(manifest["payload_path"]).read_text("utf-8")
                )
                self.assertEqual(
                    request["tool"], config["private_run"]["gateway_file_write_tool"]
                )
                writes = request["arguments"]["writes"]
                handed_writes.append(writes)
                if len(handed_writes) == 1:
                    parent = json.loads(parent_path.read_text("utf-8"))
                    parent["allowed_interfaces"].append("world_concurrent")
                    parent["allowed_child_ee_templates"].append("child_concurrent")
                    parent_path.write_text(TELEGRAM._json_text(parent), encoding="utf-8")
                    return 1
                for write in writes:
                    if write["path"] != str(parent_path):
                        continue
                    current = pathlib.Path(write["path"]).read_bytes()
                    self.assertEqual(
                        TELEGRAM.octopus_content_hash(current),
                        write["expected_before_hash"],
                    )
                    pathlib.Path(write["path"]).write_text(
                        write["content"], encoding="utf-8"
                    )
                return 2

            hub = mock.Mock()
            hub.host = binding["host"]
            hub.port = binding["port"]
            hub.wait_for.return_value = object()
            with mock.patch.object(TELEGRAM, "_root_slots", return_value=roots), mock.patch.object(
                TELEGRAM, "_ensure_binding_record", return_value=binding
            ), mock.patch.object(TELEGRAM, "_binding_topology", return_value={}), mock.patch.object(
                TELEGRAM, "_gateway_rearm_write", return_value={
                    "path": str(root / "gateway.json"),
                    "content": "{}\n",
                    "expected_before_hash": None,
                }
            ), mock.patch.object(TELEGRAM, "hand_artifact", side_effect=fake_hand), mock.patch.object(
                TELEGRAM, "_commit_topology_ownership"
            ), mock.patch.object(TELEGRAM, "_mark_binding_ready"):
                result = TELEGRAM.ensure_private_binding(
                    config, hub, binding["chat_id"], binding["thread_id"]
                )

            self.assertIs(result, binding)
            self.assertEqual(len(handed_writes), 2)
            for writes in handed_writes:
                self.assertEqual(writes[0]["path"], str(root / "gateway.json"))
                self.assertEqual(
                    {write["path"] for write in writes[1:]},
                    {str(parent_path)},
                )
            parent = json.loads(parent_path.read_text("utf-8"))
            self.assertEqual(
                parent["allowed_interfaces"],
                [
                    "owner_z",
                    "owner_a",
                    "world_concurrent",
                    binding["interface"],
                    binding["provider_interface"],
                    binding["interface_authoring_interface"],
                    binding["worker_environment_interface"],
                    binding["worker_interface"],
                ],
            )
            self.assertEqual(
                parent["allowed_child_ee_templates"],
                [
                    "child_owner",
                    "child_concurrent",
                    TELEGRAM._exact_ee_template_id(config, binding),
                ],
            )
            self.assertEqual(kind_path.read_text("utf-8"), kind_before)

    def test_topology_ownership_rotates_with_cas_and_rejects_drift(self):
        target = STATE / "interfaces" / "world_telegram_private_rotation" / "life.json"
        target.parent.mkdir(exist_ok=True)
        before = '{"runtime":"lock-one"}\n'
        after = '{"runtime":"lock-two"}\n'
        TELEGRAM._atomic_write_bytes(str(target), before.encode("utf-8"))
        TELEGRAM._commit_topology_ownership(TELEGRAM.load_config(), {str(target): before})
        writes = TELEGRAM._missing_topology_writes(TELEGRAM.load_config(), {str(target): after})
        self.assertEqual(writes, [{"path": str(target), "content": after, "expected_before_hash": TELEGRAM.octopus_content_hash(before.encode("utf-8"))}])
        TELEGRAM._atomic_write_bytes(str(target), after.encode("utf-8"))
        TELEGRAM._commit_topology_ownership(TELEGRAM.load_config(), {str(target): after})
        TELEGRAM._atomic_write_bytes(str(target), b'{"runtime":"operator-drift"}\n')
        with self.assertRaisesRegex(RuntimeError, "drifted from its last package-authored bytes"):
            TELEGRAM._missing_topology_writes(TELEGRAM.load_config(), {str(target): before})
        target.unlink()
        with self.assertRaisesRegex(RuntimeError, "disappeared"):
            TELEGRAM._missing_topology_writes(TELEGRAM.load_config(), {str(target): before})
        known = {"schema_version": 1, "bindings": {"known": {"chat_id": -100123456, "thread_id": None}}}
        hub = object()
        with mock.patch.object(TELEGRAM, "_load_bindings", return_value=known), mock.patch.object(TELEGRAM, "ensure_private_binding") as ensure:
            TELEGRAM._reconcile_known_bindings(TELEGRAM.load_config(), hub)
        ensure.assert_called_once_with(TELEGRAM.load_config(), hub, -100123456, None)

    def test_pairing_approval_consumes_finite_expiring_intent(self):
        try:
            pathlib.Path(TELEGRAM.PAIRING_PATH).unlink()
        except FileNotFoundError:
            pass
        code = TELEGRAM.register_pending_pairing(424242, now_ms=1000)
        self.assertFalse(TELEGRAM.pairing_approved(424242, now_ms=1001))
        self.assertEqual(
            TELEGRAM.approve_pairing(code, now_ms=1001),
            {"status": "approved", "user_id": 424242},
        )
        self.assertTrue(TELEGRAM.pairing_approved(424242, now_ms=1002))
        with self.assertRaisesRegex(ValueError, "absent or expired"):
            TELEGRAM.approve_pairing(code, now_ms=1003)
        expired = TELEGRAM.register_pending_pairing(313131, now_ms=2000)
        with self.assertRaisesRegex(ValueError, "absent or expired"):
            TELEGRAM.approve_pairing(expired, now_ms=2000 + TELEGRAM.PAIRING_TTL_MS)

    def test_driver_ipc_secret_transport_is_loopback_only(self):
        config = copy.deepcopy(TELEGRAM.load_config())
        invalid_gateway = copy.deepcopy(config)
        invalid_gateway["driver_ipc"]["listen"] = "0.0.0.0"
        with self.assertRaises(ValueError):
            TELEGRAM._validate_config(invalid_gateway)
        with self.assertRaises(ValueError):
            TELEGRAM._ensure_binding_record(
                config,
                -100989898,
                None,
                "198.51.100.17",
                32323,
            )

    def test_local_bot_api_file_authority_is_declared_and_resolved(self):
        gateway_life = json.loads((ROOT / "life.json").read_text("utf-8"))
        authority = gateway_life["processes"][0]["authority"]
        self.assertEqual(gateway_life["processes"][0]["private_var_directories"], ["content-hash-python-runtime"])
        self.assertEqual(authority["executable_env_paths"], [])
        self.assertEqual(authority["executable_env_subpaths"], [{"env": "OCTOPUS_VAR_DIR", "relative_path": "content-hash-python-runtime"}])
        self.assertNotIn("telegram_bot_api_files_root", authority["read_root_slots"])
        self.assertEqual(authority["read_env_paths"], [])
        self.assertIn("TELEGRAM_LOCAL_FILES_ROOTS", authority["optional_read_env_paths"])
        child_authority = TELEGRAM._child_life_json(str(STATE / "var" / "binding-secrets" / "handle.key"))["processes"][0]["authority"]
        self.assertEqual(child_authority["read_root_slots"], [])
        self.assertEqual(child_authority["read_env_paths"], [])
        self.assertEqual(child_authority["optional_read_env_paths"], ["TELEGRAM_LOCAL_FILES_ROOTS"])
        config = TELEGRAM.load_config()
        self.assertEqual(config["bot_api"]["local_file_root_slots"], [])

        env_root = STATE / "telegram-local-env"
        env_root.mkdir()
        previous_local = os.environ.get("TELEGRAM_LOCAL")
        previous_roots = os.environ.get("TELEGRAM_LOCAL_FILES_ROOTS")
        previous_fallback = os.environ.get("OCTOPUS_TELEGRAM_FALLBACK_IPS")
        try:
            os.environ.pop("TELEGRAM_LOCAL", None)
            os.environ.pop("TELEGRAM_LOCAL_FILES_ROOTS", None)
            remote = TELEGRAM.BotApi("https://api.telegram.org", "remote-token", config)
            self.assertEqual(remote.local_file_roots, [])
            os.environ["TELEGRAM_LOCAL"] = "true"
            os.environ["TELEGRAM_LOCAL_FILES_ROOTS"] = str(env_root)
            os.environ["OCTOPUS_TELEGRAM_FALLBACK_IPS"] = "149.154.167.220"
            api = TELEGRAM.BotApi("http://127.0.0.1:8081", "local-token", config)
            self.assertEqual(set(api.local_file_roots), {str(env_root.resolve())})
        finally:
            for name, value in (
                ("TELEGRAM_LOCAL", previous_local),
                ("TELEGRAM_LOCAL_FILES_ROOTS", previous_roots),
                ("OCTOPUS_TELEGRAM_FALLBACK_IPS", previous_fallback),
            ):
                if value is None:
                    os.environ.pop(name, None)
                else:
                    os.environ[name] = value

    def test_secret_transport_requires_tls_outside_loopback(self):
        config = TELEGRAM.load_config()
        with self.assertRaises(ValueError):
            TELEGRAM.BotApi("http://telegram.example", "secret", config)
        api = TELEGRAM.BotApi("http://127.0.0.1:8081", "secret", config)
        self.assertEqual(api.base, "http://127.0.0.1:8081")

    def test_transport_error_redacts_bot_token_and_proxy_credentials(self):
        config = copy.deepcopy(TELEGRAM.load_config())
        token = "123456789:ABC_def-token"
        proxy = "https://name%40tenant:p%40ss%2Fword@proxy.example:8443"
        os.environ[config["bot_api"]["proxy_env"]] = proxy
        api = TELEGRAM.BotApi("https://api.telegram.org", token, config)
        credentials = "name@tenant:p@ss/word"
        basic = base64.b64encode(credentials.encode("utf-8")).decode("ascii")
        transport_error = urllib.error.HTTPError(
            f"{api.file_endpoint}/photos/file.jpg",
            502,
            f"Proxy {proxy} rejected Basic {basic}",
            {},
            io.BytesIO(b"not-json"),
        )

        def fail_open(_request, _timeout):
            raise transport_error

        api._open = fail_open
        with self.assertRaises(TELEGRAM.TelegramError) as raised:
            api.call("getMe")
        rendered = str(raised.exception)
        raw_error = f"transport={api.file_endpoint} proxy={proxy} Basic {basic}"
        TELEGRAM.commit_effect(
            9182,
            {
                "schema_version": 1,
                "source_movement_seq": 9182,
                "source_artifact_hash": "b" * 64,
                "run": "run_transport_redaction",
                "artifact_label": "telegram.transport.failure",
                "status": "indeterminate",
                "error": raw_error,
            },
        )
        effect_evidence = pathlib.Path(TELEGRAM.effect_path(9182)).read_text("utf-8")
        original_hand = TELEGRAM.hand_artifact
        try:
            TELEGRAM.hand_artifact = lambda path: path
            manifest_path = TELEGRAM.hand_failure(
                9182,
                "transport_failed",
                {"error": raw_error},
                "telegram-transport-redaction-contract",
            )
        finally:
            TELEGRAM.hand_artifact = original_hand
        manifest = json.loads(pathlib.Path(manifest_path).read_text("utf-8"))
        failure_evidence = pathlib.Path(manifest["payload_path"]).read_text("utf-8")
        rendered += effect_evidence + failure_evidence
        for secret in (
            token,
            urllib.parse.quote(token, safe=""),
            proxy,
            "name@tenant:p@ss/word",
            "p@ss/word",
            "name%40tenant:p%40ss%2Fword",
            "p%40ss%2Fword",
            basic,
        ):
            self.assertNotIn(secret, rendered)
        self.assertIn("HTTPError", rendered)
        self.assertIn("api.telegram.org", rendered)
        self.assertIn("proxy.example:8443", rendered)

    def test_remote_refusal_and_oversized_local_media_fail_closed(self):
        error = TELEGRAM.TelegramError(
            "sendMessage",
            {"error_code": 400, "description": "remote-private-body", "secret": "remote-secret"},
        )
        self.assertEqual(str(error), "telegram sendMessage failed with error_code 400")
        self.assertNotIn("remote-private-body", str(error))
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "media.bin"
            path.write_bytes(b"123456")
            with self.assertRaises(TELEGRAM.TelegramMediaTooLarge):
                TELEGRAM._read_rooted_regular(directory, str(path), 5)

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "ledger hardening requires O_NOFOLLOW")
    def test_ledger_fallback_commits_only_complete_admitted_rows(self):
        def row(seq, padding=""):
            return json.dumps(
                {
                    "seq": seq,
                    "kind": "InterfaceOutbound",
                    "from": {"Interface": "world_provider_test"},
                    "to": {"Interface": TELEGRAM.INTERFACE_ID},
                    "invocation_metadata": {
                        "runtime": {
                            "run": f"run_{seq}",
                            "ee": "ee_test",
                            "worker": "system_worker_test",
                            "checkpoint": f"cp_{seq}",
                            "interface": TELEGRAM.INTERFACE_ID,
                            "epoch": 0,
                        }
                    },
                    "artifact": {"content": [seq % 256] * 32},
                    "padding": padding,
                },
                separators=(",", ":"),
            ).encode("utf-8") + b"\n"

        original_path = TELEGRAM.MOVEMENTS_JSONL
        original_cache = (
            TELEGRAM._LEDGER_FALLBACK_OFFSET,
            TELEGRAM._LEDGER_FALLBACK_SIZE,
            TELEGRAM._LEDGER_FALLBACK_IDENTITY,
        )
        with tempfile.TemporaryDirectory() as directory:
            path = pathlib.Path(directory) / "movements.jsonl"
            try:
                TELEGRAM.MOVEMENTS_JSONL = str(path)
                TELEGRAM._LEDGER_FALLBACK_OFFSET = 0
                TELEGRAM._LEDGER_FALLBACK_SIZE = 0
                TELEGRAM._LEDGER_FALLBACK_IDENTITY = None
                complete = row(7)
                path.write_bytes(complete[:-1])
                self.assertEqual(TELEGRAM._read_own_outbound_from_ledger(7, 8, 4096), [])
                self.assertEqual(
                    (
                        TELEGRAM._LEDGER_FALLBACK_OFFSET,
                        TELEGRAM._LEDGER_FALLBACK_SIZE,
                        TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                    ),
                    (0, 0, None),
                )
                with path.open("ab") as handle:
                    handle.write(b"\n")
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(7, 8, 4096),
                    [(7, [7] * 32, "run_7")],
                )
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(7, 8, 4096),
                    [(7, [7] * 32, "run_7")],
                )
                admitted = (
                    TELEGRAM._LEDGER_FALLBACK_OFFSET,
                    TELEGRAM._LEDGER_FALLBACK_SIZE,
                    TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                )
                path.write_bytes(b"{malformed]\n")
                with self.assertRaises(json.JSONDecodeError):
                    TELEGRAM._read_own_outbound_from_ledger(8, 8, 4096)
                self.assertEqual(
                    (
                        TELEGRAM._LEDGER_FALLBACK_OFFSET,
                        TELEGRAM._LEDGER_FALLBACK_SIZE,
                        TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                    ),
                    admitted,
                )
                for metadata in ({}, {"runtime": {}}, {"runtime": {"interface": 7}}):
                    path.write_bytes(
                        json.dumps(
                            {
                                "seq": 8,
                                "kind": "InterfaceOutbound",
                                "from": {"Interface": "world_provider_test"},
                                "to": {"Interface": TELEGRAM.INTERFACE_ID},
                                "invocation_metadata": metadata,
                            }
                        ).encode("utf-8")
                        + b"\n"
                    )
                    with self.assertRaises(ValueError):
                        TELEGRAM._read_own_outbound_from_ledger(8, 8, 4096)
                    self.assertEqual(
                        (
                            TELEGRAM._LEDGER_FALLBACK_OFFSET,
                            TELEGRAM._LEDGER_FALLBACK_SIZE,
                            TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                        ),
                        admitted,
                    )
                foreign = {
                    "seq": 8,
                    "kind": "InterfaceOutbound",
                    "from": {"Interface": "world_provider_foreign"},
                    "to": {"Interface": "world_other"},
                    "invocation_metadata": {"runtime": 7},
                }
                legacy = {
                    "seq": 9,
                    "kind": "InterfaceOutbound",
                    "from": {"Run": "run_legacy"},
                    "to": {"Interface": TELEGRAM.INTERFACE_ID},
                    "invocation_metadata": {"runtime": 7},
                }
                path.write_bytes(
                    json.dumps(foreign, separators=(",", ":")).encode("utf-8")
                    + b"\n"
                    + json.dumps(legacy, separators=(",", ":")).encode("utf-8")
                    + b"\n"
                    + row(10)
                )
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(8, 8, 4096),
                    [(10, [10] * 32, "run_10")],
                )
                replacement = path.with_suffix(".next")
                ignored = json.dumps({"seq": 10, "kind": "Mark"}).encode("utf-8") + b"\n"
                replacement.write_bytes(row(6, "x" * 256) + row(8) + ignored)
                os.replace(replacement, path)
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(8, 8, 4096),
                    [(8, [8] * 32, "run_8")],
                )
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(8, 8, 4096),
                    [(8, [8] * 32, "run_8")],
                )
                rotated_identity = TELEGRAM._LEDGER_FALLBACK_IDENTITY
                path.write_bytes(row(7) + row(9))
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(9, 8, 4096),
                    [(9, [9] * 32, "run_9")],
                )
                self.assertEqual(
                    TELEGRAM._read_own_outbound_from_ledger(9, 8, 4096),
                    [(9, [9] * 32, "run_9")],
                )
                self.assertEqual(TELEGRAM._LEDGER_FALLBACK_IDENTITY, rotated_identity)
                admitted = (
                    TELEGRAM._LEDGER_FALLBACK_OFFSET,
                    TELEGRAM._LEDGER_FALLBACK_SIZE,
                    TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                )
                path.write_bytes(b"x" * 33 + b"\n")
                with self.assertRaisesRegex(RuntimeError, "line bound"):
                    TELEGRAM._read_own_outbound_from_ledger(10, 8, 32)
                self.assertEqual(
                    (
                        TELEGRAM._LEDGER_FALLBACK_OFFSET,
                        TELEGRAM._LEDGER_FALLBACK_SIZE,
                        TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                    ),
                    admitted,
                )
                outside = pathlib.Path(directory) / "outside.jsonl"
                outside.write_bytes(row(10))
                path.unlink()
                path.symlink_to(outside)
                with self.assertRaises(OSError):
                    TELEGRAM._read_own_outbound_from_ledger(10, 8, 4096)
            finally:
                TELEGRAM.MOVEMENTS_JSONL = original_path
                (
                    TELEGRAM._LEDGER_FALLBACK_OFFSET,
                    TELEGRAM._LEDGER_FALLBACK_SIZE,
                    TELEGRAM._LEDGER_FALLBACK_IDENTITY,
                ) = original_cache

    def test_gateway_rearm_uses_declared_topology(self):
        config = TELEGRAM.load_config()
        config = copy.deepcopy(config)
        config["gateway_run_template"]["id"] = "run_configured_gateway"
        config["gateway_run_template"]["worker"] = "system_worker_configured"
        worker_root = STATE / "interfaces" / "system_worker_configured"
        worker_root.mkdir(exist_ok=True)
        (worker_root / "policy.json").write_text(
            json.dumps(
                {
                    "id": "system_worker_configured",
                    "ceiling": {},
                    "role_artifacts": ["artifact-role-z", "artifact-role-a"],
                }
            ),
            encoding="utf-8",
        )
        old_door = config["private_run"]["gateway_worker_environment_interface"]
        config["private_run"]["gateway_worker_environment_interface"] = "system_room_configured"
        for checkpoint in config["gateway_run_template"]["checkpoints"]:
            if checkpoint["interface_binding"]["interface"] == old_door:
                checkpoint["interface_binding"]["interface"] = "system_room_configured"
            for binding in checkpoint["outbound_interface_bindings"]:
                if binding["interface"] == old_door:
                    binding["interface"] = "system_room_configured"
        record = copy.deepcopy(config["gateway_run_template"])
        record["interface_state"] = {}
        path = STATE / "runs" / "run_configured_gateway.json"
        path.write_text(json.dumps(record), encoding="utf-8")
        write = TELEGRAM._gateway_rearm_write(config, "a" * 64)
        rearmed = json.loads(write["content"])
        self.assertEqual(os.path.realpath(write["path"]), os.path.realpath(path))
        self.assertEqual(rearmed["worker"], "system_worker_configured")
        self.assertEqual(
            rearmed["checkpoints"][0]["outbound_interface_bindings"][0]["interface"],
            "system_room_configured",
        )
        self.assertEqual(
            rearmed["checkpoints"][1]["interface_binding"]["interface"],
            "system_room_configured",
        )
        self.assertTrue(
            all(
                checkpoint["artifact_bindings"]
                == [{"id": "artifact-role-z"}, {"id": "artifact-role-a"}]
                for checkpoint in rearmed["checkpoints"]
            )
        )

    def test_binding_rejects_unknown_config_and_renders_bounded_text(self):
        config = copy.deepcopy(TELEGRAM.load_config())
        config["mode"] = "binding"
        config.pop("gateway_run_template")
        secret_handle = STATE / "var" / "binding-secrets" / f"{'a' * 24}.key"
        TELEGRAM._atomic_write_bytes(str(secret_handle), b"a" * 32)
        config["binding"] = {
            "interface": "world_telegram",
            "chat_id": 4242,
            "thread_id": None,
            "gateway_interface": "world_telegram",
            "secret_handle": str(secret_handle),
            "host": "127.0.0.1",
            "port": 31337,
            "run": "run_contract",
        }
        TELEGRAM._validate_config(config)
        invalid = copy.deepcopy(config)
        invalid["outbound"]["unknown"] = True
        with self.assertRaises(ValueError):
            TELEGRAM._validate_config(invalid)
        TELEGRAM.load_or_create_chat_state(4242, None)
        api = Api()
        reply, _, updates = TELEGRAM.prepare_interface_output(
            config,
            "interface.artifact.output",
            json.dumps({"text": "z" * 5000}).encode(),
        )
        result = TELEGRAM.render_reply(config, api, json.dumps(reply).encode(), config["binding"])
        TELEGRAM.update_chat_state(4242, None, **updates)
        self.assertGreater(result["message_id"], 100)
        for method, params in api.calls:
            if method in {"sendMessage", "editMessageText"}:
                self.assertLessEqual(
                    TELEGRAM.telegram_text_units(params["text"]),
                    config["outbound"]["max_message_len"],
                )

    @unittest.skipUnless(os.name == "posix", "private mode and symlink checks require POSIX")
    def test_private_topology_contains_only_a_0600_secret_handle(self):
        config = copy.deepcopy(TELEGRAM.load_config())
        config["private_run"]["context_routes"] = [
            {
                "chat_ids": [-100424242],
                "thread_ids": [73],
                "artifact_bindings": ["artifact-optional-chat-context"],
            }
        ]
        binding = TELEGRAM._ensure_binding_record(
            config,
            -100424242,
            73,
            "127.0.0.1",
            32424,
        )
        secret_handle = pathlib.Path(TELEGRAM._binding_secret_path(binding))
        secret = TELEGRAM._binding_secret(binding)
        topology = TELEGRAM._binding_topology(config, binding)
        interface_root = pathlib.Path(TELEGRAM._root_slots()["registry_interface_root"])
        child_config = json.loads(topology[str(interface_root / binding["interface"] / "config.json")])
        child_life = json.loads(topology[str(interface_root / binding["interface"] / "life.json")])
        generated_interfaces = (
            binding["interface"],
            binding["provider_interface"],
            binding["interface_authoring_interface"],
            binding["worker_environment_interface"],
            binding["worker_interface"],
        )
        policy_interfaces = (
            binding["interface"],
            binding["provider_interface"],
            binding["interface_authoring_interface"],
            binding["worker_environment_interface"],
            binding["worker_interface"],
        )
        for interface in generated_interfaces:
            descriptor = json.loads(
                topology[str(interface_root / interface / "interface.json")]
            )
            self.assertEqual(descriptor["id"], interface)
            self.assertNotIn("reference", descriptor)

        provider_descriptor = json.loads(
            topology[
                str(
                    interface_root
                    / binding["provider_interface"]
                    / "interface.json"
                )
            ]
        )
        provider_config = provider_descriptor["interface_driver_config"]
        self.assertIn("supported_reasoning_efforts", provider_config)
        self.assertIn("residual_ambient", provider_config)
        self.assertIn("model_catalog", provider_config["private_config"])
        self.assertIn("provider_operations", provider_config["private_config"])
        self.assertIn("interface_policy", provider_descriptor)

        worker_policy = json.loads(
            topology[str(interface_root / binding["worker_interface"] / "policy.json")]
        )
        self.assertEqual(worker_policy["ceiling"]["interfaces"], list(policy_interfaces))
        self.assertEqual(worker_policy["ceiling"]["tools"], [binding["file_write_tool"]])
        run_root = pathlib.Path(TELEGRAM._root_slots()["run_records_root"])
        run = json.loads(topology[str(run_root / f"{binding['run']}.json")])
        mandatory = [{"id": artifact} for artifact in worker_policy["role_artifacts"]]
        for checkpoint in run["checkpoints"]:
            if checkpoint["id"].endswith("_telegram_input"):
                self.assertEqual(
                    checkpoint["artifact_bindings"],
                    mandatory + [{"id": "artifact-optional-chat-context"}],
                )
            else:
                self.assertEqual(checkpoint["artifact_bindings"], mandatory)
        tool_root = pathlib.Path(TELEGRAM._root_slots()["registry_tool_root"])
        tool_descriptor = json.loads(
            topology[str(tool_root / binding["file_write_tool"] / "tool.json")]
        )
        self.assertEqual(tool_descriptor["id"], binding["file_write_tool"])
        self.assertEqual(tool_descriptor["interface"], binding["worker_environment_interface"])
        for interface in (
            binding["provider_interface"],
            binding["interface_authoring_interface"],
        ):
            self.assertIn(str(interface_root / interface / "life.json"), topology)

        ee_template_id = TELEGRAM._exact_ee_template_id(config, binding)
        ee_template_root = pathlib.Path(
            TELEGRAM._root_slots()["registry_ee_template_root"]
        )
        exact_template = json.loads(
            topology[
                str(ee_template_root / ee_template_id / "ee_template.json")
            ]
        )
        self.assertEqual(exact_template["id"], ee_template_id)
        self.assertTrue(set(policy_interfaces).issubset(exact_template["allowed_interfaces"]))

        policy_writes = {
            write["path"]: json.loads(write["content"])
            for write in TELEGRAM._binding_policy_writes(config, binding)
        }
        parent_template_path = str(
            ee_template_root / "existence" / "ee_template.json"
        )
        parent_template = policy_writes[parent_template_path]
        self.assertTrue(set(policy_interfaces).issubset(parent_template["allowed_interfaces"]))
        self.assertIn(ee_template_id, parent_template["allowed_child_ee_templates"])
        kind_path = str(
            pathlib.Path(
                TELEGRAM._root_slots()["registry_worker_environment_kind_root"]
            )
            / "system_room"
            / "worker_environment_kind.json"
        )
        self.assertNotIn(kind_path, policy_writes)

        self.assertNotIn("secret", binding)
        self.assertNotIn("secret", child_config["binding"])
        self.assertEqual(child_config["binding"]["secret_handle"], str(secret_handle))
        self.assertEqual(
            child_life["processes"][0]["authority"]["read_paths"],
            [str(secret_handle), os.path.realpath(os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"])],
        )
        self.assertEqual(child_life["processes"][0]["args"][0]["repository_file"], "registry/interface/world_telegram/octopus_python_boot.py")
        self.assertEqual(child_life["processes"][0]["read_handles"], ["ledger", "blobs", "ledger_index"])
        self.assertNotIn(os.path.realpath(os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"]), child_life["processes"][0]["authority"]["write_paths"])
        self.assertEqual(child_life["processes"][0]["authority"]["executable_paths"], [os.path.realpath(os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"])])
        self.assertNotIn("max_in_flight_tool_processes", child_life["processes"][0])
        self.assertEqual(stat.S_IMODE(secret_handle.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(secret_handle.parent.stat().st_mode), 0o700)

        generated_json = "\n".join(topology.values()).encode("utf-8")
        binding_state = pathlib.Path(TELEGRAM.BINDINGS_PATH).read_bytes()
        self.assertNotIn(secret, generated_json)
        self.assertNotIn(secret.hex().encode("ascii"), generated_json)
        self.assertNotIn(secret, binding_state)
        self.assertNotIn(secret.hex().encode("ascii"), binding_state)

        os.chmod(secret_handle, 0o644)
        with self.assertRaises(PermissionError):
            TELEGRAM._binding_secret(binding)
        os.chmod(secret_handle, 0o600)
        secret_handle.unlink()
        outside = STATE / "outside-telegram-secret"
        outside.write_bytes(secret)
        os.chmod(outside, 0o600)
        secret_handle.symlink_to(outside)
        with self.assertRaises(PermissionError):
            TELEGRAM._binding_secret(binding)
        self.assertEqual(outside.read_bytes(), secret)


if __name__ == "__main__":
    unittest.main()
