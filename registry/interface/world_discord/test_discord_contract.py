import asyncio
import copy
import base64
import concurrent.futures
import hashlib
import importlib
import json
import os
import shutil
import socket
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock


ROOT = os.path.dirname(__file__)
REPOSITORY_ROOT = os.path.abspath(os.path.join(ROOT, "..", "..", ".."))
TEMP = tempfile.TemporaryDirectory()
DEPENDENCY_STATE = os.path.join(TEMP.name, "dependency-state")
VAR = os.path.join(TEMP.name, "var")
BLOBS = os.path.join(TEMP.name, "blobs")
REGISTRY = os.path.join(TEMP.name, "registry-interface")
TOOLS = os.path.join(TEMP.name, "registry-tool")
EE_TEMPLATES = os.path.join(TEMP.name, "registry-ee-template")
WORKER_ENVIRONMENT_KINDS = os.path.join(TEMP.name, "registry-worker-environment-kind")
RUNS = os.path.join(TEMP.name, "runs")
EES = os.path.join(TEMP.name, "ees")
os.makedirs(VAR)
os.makedirs(DEPENDENCY_STATE)
os.makedirs(BLOBS)
os.makedirs(REGISTRY)
os.makedirs(TOOLS)
os.makedirs(EE_TEMPLATES)
os.makedirs(WORKER_ENVIRONMENT_KINDS)
os.makedirs(RUNS)
os.makedirs(EES)
sys.path.insert(0, ROOT)
os.environ["OCTOPUS_VAR_DIR"] = DEPENDENCY_STATE
boot = importlib.import_module("discord_boot")
DEPENDENCY_RUNTIME = boot.dependency_root()
sys.path.insert(0, DEPENDENCY_RUNTIME)
os.environ.update({
    "OCTOPUS_INTERFACE_ID": "world_discord",
    "OCTOPUS_INTERFACE_DIR": ROOT,
    "OCTOPUS_VAR_DIR": VAR,
    "OCTOPUS_MOVEMENTS_JSONL": os.path.join(TEMP.name, "movements.jsonl"),
    "OCTOPUS_BLOBS_DIR": BLOBS,
    "OCTOPUS_LEDGER_INDEX_SQLITE": os.path.join(TEMP.name, "ledger.sqlite"),
    "OCTOPUS_CONTENT_HASH_RUNTIME": DEPENDENCY_RUNTIME,
    "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"registry_interface_root": REGISTRY, "registry_tool_root": TOOLS, "registry_ee_template_root": EE_TEMPLATES, "registry_worker_environment_kind_root": WORKER_ENVIRONMENT_KINDS, "run_records_root": RUNS, "ee_instances_root": EES}),
})
boundary = importlib.import_module("discord_boundary")
life = importlib.import_module("discord_life")
voice = importlib.import_module("discord_voice")


class DiscordContractTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        with open(os.path.join(ROOT, "config.json"), "r", encoding="utf-8") as handle:
            cls.config = json.load(handle)

    def setUp(self):
        for root in (VAR, REGISTRY, TOOLS, EE_TEMPLATES, WORKER_ENVIRONMENT_KINDS, RUNS, EES):
            shutil.rmtree(root, ignore_errors=True)
            os.makedirs(root)
        for path in (boundary.PAYLOADS_DIR, boundary.MEDIA_DIR, boundary.EFFECTS_DIR):
            os.makedirs(path, exist_ok=True)
        interface_owners = set(self.config["private_run"]["owners"].values())
        interface_owners.add(self.config["private_run"]["worker_environment_door_owner"])
        interface_owners.add(self.config["private_run"]["gateway_worker_environment_interface"])
        for owner in interface_owners:
            owner_dir = os.path.join(REGISTRY, owner)
            os.makedirs(owner_dir)
            for filename in ("interface.json", "life.json"):
                source = os.path.join(
                    REPOSITORY_ROOT, "registry", "interface", owner, filename
                )
                if os.path.isfile(source):
                    shutil.copyfile(source, os.path.join(owner_dir, filename))
        for owner in (
            self.config["private_run"]["file_write_tool"],
            self.config["private_run"]["gateway_file_write_tool"],
        ):
            owner_dir = os.path.join(TOOLS, owner)
            os.makedirs(owner_dir)
            shutil.copyfile(
                os.path.join(REPOSITORY_ROOT, "registry", "tool", owner, "tool.json"),
                os.path.join(owner_dir, "tool.json"),
            )
        for template in (self.config["private_run"]["ee_template"], "existence"):
            target = os.path.join(EE_TEMPLATES, template)
            os.makedirs(target)
            shutil.copyfile(
                os.path.join(
                    REPOSITORY_ROOT,
                    "registry",
                    "ee_template",
                    template,
                    "ee_template.json",
                ),
                os.path.join(target, "ee_template.json"),
            )
        boundary.atomic_write_json(
            os.path.join(EES, f"{self.config['private_run']['parent_ee']}.json"),
            {
                "ee_id": self.config["private_run"]["parent_ee"],
                "template_name": "existence",
                "parent_ee_id": None,
                "goal": "Exist",
            },
        )
        kind = self.config["private_run"]["worker_environment_kind"]
        target = os.path.join(WORKER_ENVIRONMENT_KINDS, kind)
        os.makedirs(target)
        shutil.copyfile(
            os.path.join(
                REPOSITORY_ROOT,
                "registry",
                "worker_environment_kind",
                kind,
                "worker_environment_kind.json",
            ),
            os.path.join(target, "worker_environment_kind.json"),
        )
        for path in (os.environ["OCTOPUS_MOVEMENTS_JSONL"], os.environ["OCTOPUS_LEDGER_INDEX_SQLITE"]):
            try:
                os.unlink(path)
            except FileNotFoundError:
                pass
        boundary.reset_authoring_coordinator()

    def test_config_is_strict(self):
        boundary.validate_config(copy.deepcopy(self.config))
        changed = copy.deepcopy(self.config)
        changed["discord_api"]["hidden_fallback"] = True
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            boundary.validate_config(changed)
        changed = copy.deepcopy(self.config)
        changed["gating"]["max_batch_events"] = 0
        with self.assertRaisesRegex(ValueError, "max_batch_events"):
            boundary.validate_config(changed)
        changed = copy.deepcopy(self.config)
        changed["gating"]["max_batch_bytes"] = 0
        with self.assertRaisesRegex(ValueError, "max_batch_bytes"):
            boundary.validate_config(changed)

    def test_ledger_fallback_bounds_rows_before_json_allocation(self):
        maximum = 256
        path = os.environ["OCTOPUS_MOVEMENTS_JSONL"]
        with open(path, "wb") as handle:
            handle.write(b"{" + b"x" * maximum + b"}\n")
        boundary._LEDGER_FALLBACK_OFFSET = 0
        boundary._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "exceeds 256 bytes"):
            boundary._read_outbound_ledger(0, 128, maximum)
        with open(path, "wb") as handle:
            handle.write(b'{"seq":0}')
        boundary._LEDGER_FALLBACK_OFFSET = 0
        boundary._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "is partial"):
            boundary._read_outbound_ledger(0, 128, maximum)
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
            "to": {"Interface": "world_discord"},
            "invocation_metadata": {"runtime": 7},
        }
        movement = {
            "seq": 7,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_discord"},
            "invocation_metadata": {
                "runtime": {
                    "run": "run_discord",
                    "ee": "ee_discord",
                    "worker": "system_worker_discord",
                    "checkpoint": "cp_discord",
                    "interface": "world_discord",
                    "epoch": 0,
                }
            },
            "artifact": {"content": [0] * 32},
        }
        with open(path, "w", encoding="utf-8") as handle:
            for row in (foreign, legacy, movement):
                handle.write(json.dumps(row) + "\n")
        boundary._LEDGER_FALLBACK_IDENTITY = None
        boundary._LEDGER_FALLBACK_OFFSET = 0
        boundary._LEDGER_FALLBACK_SIZE = 0
        expected = [(7, [0] * 32, "run_discord")]
        self.assertEqual(boundary._read_outbound_ledger(7, 128, 1024), expected)
        self.assertEqual(boundary._read_outbound_ledger(7, 128, 1024), expected)
        self.assertEqual(boundary._read_outbound_ledger(8, 128, 1024), [])
        malformed = {
            "seq": 8,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_discord"},
            "invocation_metadata": {"runtime": {"interface": "world_discord"}},
        }
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(json.dumps(malformed) + "\n")
        boundary._LEDGER_FALLBACK_IDENTITY = None
        boundary._LEDGER_FALLBACK_OFFSET = 0
        boundary._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaises(ValueError):
            boundary._read_outbound_ledger(8, 128, 1024)
        changed = copy.deepcopy(self.config)
        changed["private_run"]["id_prefixes"]["discord"] = "../../escape_"
        with self.assertRaisesRegex(ValueError, "registry-safe identifier prefix"):
            boundary.validate_config(changed)
        changed = copy.deepcopy(self.config)
        changed["mode"] = "binding"
        changed.pop("gateway_run_template")
        changed["binding"] = {"interface": "world_discord_private", "channel_id": "123456789012345678", "thread_id": None, "gateway_interface": "world_discord", "secret_handle": os.path.join(VAR, "missing-secret"), "host": "192.0.2.10", "port": 45678, "run": "run_discord_private"}
        with mock.patch.object(boundary, "INTERFACE_ID", "world_discord_private"):
            with self.assertRaisesRegex(ValueError, "loopback"):
                boundary.validate_config(changed)
        with self.assertRaisesRegex(ValueError, "loopback"):
            boundary.ensure_binding_record(self.config, "123456789012345678", None, "192.0.2.10", 45678)

    def test_media_stream_is_bounded_before_materialization(self):
        async def accepted():
            yield b"abc"
            yield b"def"

        path, size, digest = asyncio.run(life.materialize_media_chunks(accepted(), 6, "test", ".bin"))
        self.assertEqual(size, 6)
        self.assertEqual(digest, hashlib.sha256(b"abcdef").hexdigest())
        with open(path, "rb") as handle:
            self.assertEqual(handle.read(), b"abcdef")

        async def rejected():
            yield b"abcd"
            yield b"ef"

        with self.assertRaises(life.InboundMediaTooLarge):
            asyncio.run(life.materialize_media_chunks(rejected(), 5, "oversized", ".bin"))
        self.assertFalse(any(name.startswith(".attachment-") for name in os.listdir(boundary.MEDIA_DIR)))

    def test_token_transport_requires_https_outside_loopback(self):
        self.assertEqual(boundary.validate_api_base_url("https://discord.com/api/v10"), "https://discord.com/api/v10")
        self.assertEqual(boundary.validate_api_base_url("http://localhost:8080/api/v10/"), "http://localhost:8080/api/v10")
        self.assertEqual(boundary.validate_api_base_url("http://127.2.3.4:8080/api/v10"), "http://127.2.3.4:8080/api/v10")
        self.assertEqual(boundary.validate_api_base_url("http://[::1]:8080/api/v10"), "http://[::1]:8080/api/v10")
        for value in ("http://discord.com/api/v10", "https://token@discord.com/api/v10", "https://discord.com/api/v10?token=x", "https://discord.com/api/v10#x", "https://discord.com/api/v10?", "https://discord.com/api/v10#"):
            with self.assertRaises(ValueError):
                boundary.validate_api_base_url(value)
        changed = copy.deepcopy(self.config)
        changed["discord_api"]["max_response_bytes"] = 1024
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            boundary.validate_config(changed)

    def test_proxy_transport_and_error_evidence_are_secret_safe(self):
        self.assertEqual(boundary.validate_proxy_url("https://user:pass@proxy.example:8443"), "https://user:pass@proxy.example:8443")
        self.assertEqual(boundary.validate_proxy_url("http://user:pass@127.0.0.1:8080/"), "http://user:pass@127.0.0.1:8080")
        for value in (
            "http://user:pass@proxy.example:8080",
            "socks5://proxy.example:1080",
            "https://proxy.example/path",
            "https://proxy.example?token=x",
            "https://proxy.example#fragment",
            "https://user%ZZ:pass@proxy.example",
        ):
            with self.assertRaises(ValueError):
                boundary.validate_proxy_url(value)
        proxy_env = self.config["discord_api"]["proxy_env"]
        token_env = self.config["discord_api"]["token_env"]
        with mock.patch.dict(os.environ, {proxy_env: "https://person:p%40ss@proxy.example:8443", token_env: "discord-token-secret"}):
            error = "failed via https://person:p%40ss@proxy.example:8443 for p@ss and discord-token-secret"
            sanitized = boundary.sanitize_external_error(self.config, error)
            self.assertNotIn("person", sanitized)
            self.assertNotIn("p%40ss", sanitized)
            self.assertNotIn("p@ss", sanitized)
            self.assertNotIn("discord-token-secret", sanitized)
            self.assertIn("<redacted>", sanitized)

    def test_dependency_subprocess_environment_is_allowlisted(self):
        proxy = "https://person:secret@proxy.example:8443"
        inherited = {
            "PATH": "/usr/bin:/bin",
            "TMPDIR": "/tmp/discord-dependencies",
            "SSL_CERT_FILE": "/etc/ssl/cert.pem",
            "DISCORD_BOT_TOKEN": "discord-secret",
            "OCTOPUS_VAR_DIR": "/private/interface-state",
            "OCTOPUS_MOVEMENTS_JSONL": "/private/session/movements.jsonl",
            "PYTHONPATH": "/ambient/python",
            "HTTP_PROXY": "http://ambient-proxy.example",
        }
        with mock.patch.dict(os.environ, inherited, clear=True):
            environment = boot.dependency_process_environment(proxy)
        self.assertEqual(environment["PATH"], inherited["PATH"])
        self.assertEqual(environment["TMPDIR"], inherited["TMPDIR"])
        self.assertEqual(environment["SSL_CERT_FILE"], inherited["SSL_CERT_FILE"])
        self.assertEqual(environment["HTTP_PROXY"], proxy)
        self.assertEqual(environment["HTTPS_PROXY"], proxy)
        for name in ("DISCORD_BOT_TOKEN", "OCTOPUS_VAR_DIR", "OCTOPUS_MOVEMENTS_JSONL", "PYTHONPATH"):
            self.assertNotIn(name, environment)

    def test_concurrent_private_authoring_serializes_gateway_cas(self):
        template = copy.deepcopy(self.config["gateway_run_template"])
        gateway_path = os.path.join(RUNS, f"{template['id']}.json")
        boundary.atomic_write_json(gateway_path, template)
        kind_path = os.path.join(
            WORKER_ENVIRONMENT_KINDS,
            self.config["private_run"]["worker_environment_kind"],
            "worker_environment_kind.json",
        )
        with open(kind_path, "rb") as handle:
            kind_before = handle.read()
        lock = threading.Lock()
        active = 0
        maximum_active = 0
        admitted = 0

        class Hub:
            host = "127.0.0.1"
            port = 45678

            def wait_for(self, interface, timeout_ms):
                return interface, timeout_ms

        def apply_file_write(label, payload, key):
            nonlocal active, maximum_active, admitted
            self.assertEqual(label, self.config["private_run"]["file_write_label"])
            self.assertEqual(
                payload["tool"], self.config["private_run"]["gateway_file_write_tool"]
            )
            writes = payload["arguments"]["writes"]
            self.assertEqual(
                os.path.realpath(writes[0]["path"]), os.path.realpath(gateway_path)
            )
            shared_paths = {
                os.path.realpath(
                    os.path.join(EE_TEMPLATES, "existence", "ee_template.json")
                ),
            }
            first_shared = next(
                index
                for index, write in enumerate(writes)
                if os.path.realpath(write["path"]) in shared_paths
            )
            self.assertGreater(first_shared, 1)
            self.assertTrue(
                all(
                    os.path.realpath(write["path"]) in shared_paths
                    for write in writes[first_shared:]
                )
            )
            with lock:
                active += 1
                maximum_active = max(maximum_active, active)
            time.sleep(0.02)
            try:
                for write in writes:
                    path = write["path"]
                    try:
                        with open(path, "rb") as handle:
                            before = handle.read()
                    except FileNotFoundError:
                        before = None
                    observed = None if before is None else boundary.octopus_content_hash(before)
                    expected = write.get("expected_before_hash", observed)
                    desired = write["content"].encode("utf-8")
                    if expected != observed and before != desired:
                        raise RuntimeError(f"optimistic mismatch for {path}")
                    boundary.atomic_write_bytes(path, desired)
                with lock:
                    admitted += 1
                    return admitted
            finally:
                with lock:
                    active -= 1

        original = boundary.hand_json
        boundary.hand_json = apply_file_write
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                futures = [
                    pool.submit(boundary.ensure_private_binding, self.config, Hub(), "123456789012345678", None),
                    pool.submit(boundary.ensure_private_binding, self.config, Hub(), "223456789012345678", None),
                ]
                bindings = [future.result(timeout=5) for future in futures]
        finally:
            boundary.hand_json = original
        with open(gateway_path, "r", encoding="utf-8") as handle:
            gateway = json.load(handle)
        self.assertEqual(maximum_active, 1)
        self.assertEqual(admitted, 2)
        self.assertEqual(gateway["interface_state"]["world_discord"]["authoring_generation"], 2)
        self.assertEqual({binding["status"] for binding in bindings}, {"ready"})
        self.assertEqual(len({binding["interface"] for binding in bindings}), 2)
        with open(
            os.path.join(EE_TEMPLATES, "existence", "ee_template.json"),
            encoding="utf-8",
        ) as handle:
            parent_template = json.load(handle)
        for binding in bindings:
            self.assertIn(binding["interface"], parent_template["allowed_interfaces"])
            self.assertIn(binding["provider_interface"], parent_template["allowed_interfaces"])
            self.assertIn(binding["interface_authoring_interface"], parent_template["allowed_interfaces"])
            self.assertIn(binding["worker_environment_interface"], parent_template["allowed_interfaces"])
            self.assertIn(binding["worker_interface"], parent_template["allowed_interfaces"])
            self.assertIn(binding["ee_template"], parent_template["allowed_child_ee_templates"])
            with open(
                os.path.join(EES, f"{binding['ee']}.json"), encoding="utf-8"
            ) as handle:
                self.assertEqual(json.load(handle)["template_name"], binding["ee_template"])
        with open(kind_path, "rb") as handle:
            self.assertEqual(handle.read(), kind_before)

    def test_pending_event_replays_after_interruption_with_retry_evidence(self):
        config = copy.deepcopy(self.config)
        config["inbound_journal"]["delivery_max_attempts"] = 2
        config["inbound_journal"]["delivery_retry_backoff_ms"] = 1
        config["inbound_journal"]["delivery_retry_backoff_cap_ms"] = 1
        event_id = "messages:" + "a" * 64
        source_ids = ["123456789012345678", "223456789012345678"]
        payload = {"source": "discord", "event_type": "message.create", "event_id": source_ids[-1], "channel_id": "323456789012345678", "thread_id": None, "text": "durable"}
        boundary.enqueue_pending_event(config, payload["channel_id"], None, payload, event_id, source_ids)
        boundary.begin_pending_attempt(config, event_id)
        ensure_calls = 0

        class Hub:
            def __init__(self, fail_dispatch=False):
                self.dispatches = 0
                self.fail_dispatch = fail_dispatch

            def dispatch(self, binding, dispatched_payload):
                if self.fail_dispatch:
                    raise AssertionError("delivered journal entry was dispatched again")
                self.dispatches += 1
                self.dispatched_payload = dispatched_payload
                return 77

        binding = {"interface": "world_discord_private", "channel_id": payload["channel_id"], "thread_id": None}

        def ensure(config_value, hub, channel_id, thread_id, evidence):
            nonlocal ensure_calls
            state = boundary.load_pending_events(config_value)
            current = next(iter(state["events"].values()))
            self.assertEqual(current["status"], "delivering")
            ensure_calls += 1
            if ensure_calls == 1:
                raise RuntimeError("simulated authoring interruption")
            evidence("topology_authoring", "binding_ready", {"binding_interface": binding["interface"]})
            return binding

        original_ensure = life.ensure_private_binding
        original_grant = life.grant_payload_media
        life.ensure_private_binding = ensure
        life.grant_payload_media = lambda binding_value, payload_value: payload_value
        try:
            driver = life.GatewayDriver(config, "token")
            driver.hub = Hub()
            result = asyncio.run(driver._deliver_pending_event(event_id))
        finally:
            life.ensure_private_binding = original_ensure
            life.grant_payload_media = original_grant
        self.assertEqual(result[1], 77)
        state = boundary.load_pending_events(config)
        entry = next(iter(state["events"].values()))
        self.assertEqual(entry["status"], "delivered")
        self.assertEqual([attempt["outcome"] for attempt in entry["attempts"]], ["interrupted", "failed", "delivered"])
        self.assertEqual(driver.hub.dispatches, 1)
        self.assertEqual(driver.hub.dispatched_payload["event_id"], event_id)
        os.unlink(boundary.DEDUP_PATH)
        replay = life.GatewayDriver(config, "token")
        replay.hub = Hub(fail_dispatch=True)
        asyncio.run(replay._replay_pending_events())
        self.assertEqual(set(boundary.load_bounded_ids(boundary.DEDUP_PATH, "discord gateway dedup", 10000)), set(source_ids))

    def test_private_arrival_recovers_original_seq_after_crash_gap(self):
        payload = {"event_id": "123456789012345678", "channel_id": "223456789012345678", "thread_id": None, "text": "once"}
        manifest = boundary.materialize_artifact("discord.inbound", boundary._canonical_json_bytes(payload), "idempotent-arrival")
        artifact_id = boundary.materialized_artifact_id(
            manifest,
            self.config["driver_ipc"]["max_manifest_bytes"],
            self.config["driver_ipc"]["max_frame_bytes"],
        )
        movement = {"seq": 91, "kind": "InterfaceArrival", "from": {"Interface": "world_discord"}, "artifact": {"id": artifact_id}}
        with open(os.environ["OCTOPUS_MOVEMENTS_JSONL"], "w", encoding="utf-8") as handle:
            handle.write(json.dumps(movement) + "\n")
        original = boundary.hand_artifact

        def refuse(manifest_path, evidence=None):
            self.assertEqual(evidence[0]["artifact_id"], artifact_id)
            raise boundary.ArtifactDeliveryRefused({"refused": {"reason": "malformed", "detail": {"message": "arrival evidence already exists in the movement ledger", "artifact_id": artifact_id}}})

        boundary.hand_artifact = refuse
        try:
            admitted = boundary.hand_json_idempotent("discord.inbound", payload, "idempotent-arrival")
        finally:
            boundary.hand_artifact = original
        self.assertEqual(admitted, 91)

    def test_private_target_cannot_escape(self):
        binding = {
            "channel_id": "123456789012345678",
            "thread_id": None,
        }
        request = {"operation": "message.create", "target": {"channel_id": "223456789012345678"}, "text": "escape"}
        with self.assertRaisesRegex(ValueError, "escapes"):
            life.operation_request(self.config, request, binding, 7)

    def test_blob_is_materialized_for_voice(self):
        data = b"voice-data"
        digest = life.octopus_content_hash(data)
        with open(os.path.join(BLOBS, digest), "wb") as handle:
            handle.write(data)
        binding = {"channel_id": "123456789012345678", "thread_id": None}
        request = life.operation_request(self.config, {"operation": "voice.play", "blob_id": digest}, binding, 8)
        self.assertEqual(os.path.commonpath((request["audio_path"], os.path.join(VAR, "media"))), os.path.join(VAR, "media"))
        with open(request["audio_path"], "rb") as handle:
            self.assertEqual(handle.read(), data)
        self.assertTrue(request["idempotency_key"])

    def test_chunking_preserves_content(self):
        text = "a" * 1990 + "\n" + "b" * 1990 + "\n" + "c" * 120
        chunks = life.split_message(text, 2000)
        self.assertTrue(all(len(chunk) <= 2000 for chunk in chunks))
        self.assertEqual("\n".join(chunks), text)

    def test_text_batch_has_fixed_deadline_and_hard_count_and_byte_bounds(self):
        async def exercise():
            config = copy.deepcopy(self.config)
            config["gating"]["text_batch_delay_ms"] = 600000
            config["gating"]["text_batch_split_delay_ms"] = 600000
            config["gating"]["max_batch_events"] = 2
            config["gating"]["max_batch_bytes"] = 1024
            driver = life.GatewayDriver(config, "token")
            delivered = []
            driver._already_seen = lambda _event_id: False

            async def dispatch(channel_id, thread_id, payload, event_id, source_event_ids=None):
                delivered.append((channel_id, thread_id, copy.deepcopy(payload), event_id, list(source_event_ids or [])))
                return None

            driver._ensure_dispatch = dispatch

            def payload(event_id, text):
                return {
                    "author": {"id": "423456789012345678"},
                    "message_id": event_id,
                    "event_id": event_id,
                    "created_at": event_id,
                    "text": text,
                }

            await driver._queue_or_dispatch("123456789012345678", None, payload("1", "one"), "1", True)
            key = "123456789012345678::423456789012345678"
            first_deadline = driver.batch_tasks[key]
            await driver._queue_or_dispatch("123456789012345678", None, payload("2", "two"), "2", True)
            self.assertIs(driver.batch_tasks[key], first_deadline)
            await driver._queue_or_dispatch("123456789012345678", None, payload("3", "three"), "3", True)
            self.assertEqual(delivered[0][4], ["1", "2"])
            self.assertEqual(driver.batch_payloads[key]["event_ids"], ["3"])
            self.assertLessEqual(driver.batch_payloads[key]["byte_count"], config["gating"]["max_batch_bytes"])
            tasks = list(driver.batch_tasks.values())
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)

            config["gating"]["max_batch_events"] = 100
            config["gating"]["max_batch_bytes"] = 1048576
            driver = life.GatewayDriver(config, "token")
            delivered = []
            driver._already_seen = lambda _event_id: False
            driver._ensure_dispatch = dispatch
            first_payload = payload("4", "abc")
            config["gating"]["max_batch_bytes"] = driver._batch_size_bytes(first_payload, ["4"])
            await driver._queue_or_dispatch("223456789012345678", None, first_payload, "4", True)
            await driver._queue_or_dispatch("223456789012345678", None, payload("5", "def"), "5", True)
            key = "223456789012345678::423456789012345678"
            self.assertEqual(delivered[0][4], ["4"])
            self.assertEqual(driver.batch_payloads[key]["event_ids"], ["5"])
            self.assertLessEqual(driver.batch_payloads[key]["byte_count"], config["gating"]["max_batch_bytes"])
            tasks = list(driver.batch_tasks.values())
            for task in tasks:
                task.cancel()
            await asyncio.gather(*tasks, return_exceptions=True)

        asyncio.run(exercise())

    def test_outbound_dispatch_never_retries_a_whole_operation(self):
        class RateLimited(Exception):
            retry_after = 0.001

        class HttpException(Exception):
            pass

        driver = life.GatewayDriver(copy.deepcopy(self.config), "token")
        driver.discord = type("Discord", (), {"RateLimited": RateLimited, "HTTPException": HttpException})
        attempts = 0

        async def fail(_binding, _request):
            nonlocal attempts
            attempts += 1
            raise RateLimited()

        driver._execute_once = fail
        with self.assertRaises(RateLimited):
            asyncio.run(driver._execute({}, {}))
        self.assertEqual(attempts, 1)

    def test_native_voice_metadata_and_contract(self):
        pcm = (b"\x00\x10\x00\x10" * 48000)
        duration, waveform = life.native_voice_metadata(pcm)
        self.assertEqual(duration, 1.0)
        self.assertEqual(len(base64.b64decode(waveform)), 256)
        data = b"ogg-opus"
        digest = life.octopus_content_hash(data)
        with open(os.path.join(BLOBS, digest), "wb") as handle:
            handle.write(data)
        binding = {"channel_id": "123456789012345678", "thread_id": None}
        request = life.operation_request(self.config, {"operation": "message.create", "native_voice": True, "attachments": [{"blob_id": digest, "filename": "voice.ogg"}]}, binding, 9)
        self.assertTrue(request["native_voice"])
        with self.assertRaisesRegex(ValueError, "cannot carry text"):
            life.operation_request(self.config, {"operation": "message.create", "native_voice": True, "text": "invalid", "attachments": [{"blob_id": digest, "filename": "voice.ogg"}]}, binding, 10)

    def test_effect_journal_rejects_false_success(self):
        state = {
            "schema_version": 1,
            "source_movement_seq": 3,
            "source_artifact_hash": "a" * 64,
            "run": "run_discord_test",
            "artifact_label": "discord.outbound",
            "status": "succeeded",
        }
        with self.assertRaisesRegex(ValueError, "no result"):
            boundary.validate_effect(3, state)

    def test_stream_sequence_is_durable_and_rate_bounded(self):
        binding = {"channel_id": "123456789012345678", "thread_id": None}
        request, result, updates = life.decode_output(self.config, "agent.message.delta", b'{"seq":0,"text":"a"}', binding, 1)
        self.assertEqual(request["operation"], "message.create")
        boundary.update_stream_state(**updates, message_id="223456789012345678", last_edit_ms=10**15)
        request, result, updates = life.decode_output(self.config, "agent.message.delta", b'{"seq":1,"text":"b"}', binding, 2)
        self.assertIsNone(request)
        self.assertEqual(result["disposition"], "buffered_stream_delta")
        self.assertEqual(updates["text"], "ab")

    def test_private_binding_does_not_receive_discord_secret(self):
        binding = boundary.ensure_binding_record(self.config, "123456789012345678", None, "127.0.0.1", 45678)
        topology = boundary.binding_topology(self.config, binding)
        config_path = next(path for path in topology if path.endswith(f"/{binding['interface']}/config.json"))
        life_path = next(path for path in topology if path.endswith(f"/{binding['interface']}/life.json"))
        child_config = json.loads(topology[config_path])
        process = json.loads(topology[life_path])["processes"][0]
        provider_path = next(path for path in topology if path.endswith(f"/{binding['provider_interface']}/interface.json"))
        provider_descriptor = json.loads(topology[provider_path])
        provider_owner = self.config["private_run"]["owners"]["provider"]
        with open(
            os.path.join(REGISTRY, provider_owner, "interface.json"),
            encoding="utf-8",
        ) as handle:
            owner_descriptor = json.load(handle)
        self.assertNotIn("reference", provider_descriptor)
        self.assertEqual(provider_descriptor["id"], binding["provider_interface"])
        self.assertEqual(provider_descriptor["source_names"], owner_descriptor["source_names"])
        self.assertEqual(provider_descriptor["interface_driver_config"]["private_config"]["model"], self.config["private_run"]["model"])
        self.assertEqual(binding["ee_template"], self.config["private_run"]["id_prefixes"]["ee_template"] + binding["opaque_id"])
        template_path = next(path for path in topology if path.endswith(f"/{binding['ee_template']}/ee_template.json"))
        exact_template = json.loads(topology[template_path])
        self.assertEqual(exact_template["id"], binding["ee_template"])
        self.assertEqual(
            exact_template["allowed_interfaces"],
            [
                binding["interface"],
                binding["provider_interface"],
                binding["interface_authoring_interface"],
                binding["worker_environment_interface"],
                binding["worker_interface"],
            ],
        )
        run_path = next(path for path in topology if path.endswith(f"/{binding['run']}.json"))
        run_record = json.loads(topology[run_path])
        self.assertIn(
            binding["worker_environment_interface"],
            {checkpoint["interface_binding"]["interface"] for checkpoint in run_record["checkpoints"]},
        )
        tool_path = next(path for path in topology if path.endswith(f"/{binding['file_write_tool']}/tool.json"))
        tool_descriptor = json.loads(topology[tool_path])
        self.assertEqual(tool_descriptor["id"], binding["file_write_tool"])
        self.assertEqual(tool_descriptor["interface"], binding["worker_environment_interface"])
        worker_policy_path = next(path for path in topology if path.endswith(f"/{binding['worker_interface']}/policy.json"))
        worker_policy = json.loads(topology[worker_policy_path])
        self.assertEqual(worker_policy["ceiling"]["tools"], [binding["file_write_tool"]])
        secret_handle = child_config["binding"]["secret_handle"]
        secret = boundary.load_secret_handle(secret_handle)
        metadata = os.lstat(secret_handle)
        self.assertTrue(os.path.isfile(secret_handle))
        self.assertFalse(os.path.islink(secret_handle))
        self.assertEqual(metadata.st_mode & 0o777, 0o600)
        self.assertNotIn("secret", child_config["binding"])
        self.assertNotIn("secret", binding)
        runtime_source = os.path.realpath(os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"])
        self.assertEqual(process["authority"]["read_paths"], [secret_handle, runtime_source])
        self.assertEqual(process["env"], {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source})
        self.assertEqual(process["args"][0]["repository_file"], "registry/interface/world_discord/discord_boot.py")
        self.assertNotIn(runtime_source, process["authority"]["write_paths"])
        self.assertEqual(process["authority"]["executable_paths"], [runtime_source])
        self.assertEqual(process["max_in_flight_tool_processes"], 16)
        self.assertEqual(process["io_limits"]["max_payload_bytes"], 268435456)
        with open(os.path.join(ROOT, "life.json"), encoding="utf-8") as handle:
            gateway = json.load(handle)["processes"][0]
        self.assertEqual(gateway["private_var_directories"], ["python-runtime"])
        self.assertEqual(gateway["authority"]["executable_env_paths"], [])
        self.assertEqual(gateway["authority"]["executable_env_subpaths"], [{"env": "OCTOPUS_VAR_DIR", "relative_path": "python-runtime"}])
        self.assertTrue(all(secret.hex() not in value for value in topology.values()))
        self.assertEqual(process["inherit_env"], ["PATH"])
        self.assertNotIn("required_env", process)
        self.assertFalse(process["authority"]["allow_network"])
        self.assertEqual(process["authority"]["executable_commands"], ["python3"])
        os.chmod(secret_handle, 0o644)
        with self.assertRaisesRegex(RuntimeError, "mode 0600"):
            boundary.load_secret_handle(secret_handle)
        os.chmod(secret_handle, 0o600)
        other = os.path.join(VAR, "other-secret")
        boundary.atomic_write_bytes(other, secret)
        os.unlink(secret_handle)
        os.symlink(other, secret_handle)
        with self.assertRaisesRegex(RuntimeError, "non-symlink"):
            boundary.load_secret_handle(secret_handle)

    def test_identity_salt_uses_private_file_contract(self):
        salt = boundary._identity_salt()
        metadata = os.lstat(boundary.IDENTITY_SALT_PATH)
        self.assertEqual(len(salt), 32)
        self.assertTrue(os.path.isfile(boundary.IDENTITY_SALT_PATH))
        self.assertFalse(os.path.islink(boundary.IDENTITY_SALT_PATH))
        self.assertEqual(metadata.st_mode & 0o777, 0o600)
        os.chmod(boundary.IDENTITY_SALT_PATH, 0o644)
        with self.assertRaisesRegex(RuntimeError, "mode 0600"):
            boundary._identity_salt()
        os.chmod(boundary.IDENTITY_SALT_PATH, 0o600)
        with mock.patch.object(boundary.os, "geteuid", return_value=metadata.st_uid + 1):
            with self.assertRaisesRegex(RuntimeError, "owned by"):
                boundary._identity_salt()
        os.unlink(boundary.IDENTITY_SALT_PATH)
        boundary.atomic_write_bytes(boundary.IDENTITY_SALT_PATH, b"short")
        with self.assertRaisesRegex(RuntimeError, "exactly 32 bytes"):
            boundary._identity_salt()
        other = os.path.join(VAR, "other-identity-salt")
        boundary.atomic_write_bytes(other, salt)
        os.unlink(boundary.IDENTITY_SALT_PATH)
        os.symlink(other, boundary.IDENTITY_SALT_PATH)
        with self.assertRaisesRegex(RuntimeError, "non-symlink"):
            boundary._identity_salt()

    def test_media_grant_is_authenticated_and_materialized_in_binding(self):
        data = (b"discord-media" * 10000)
        source = os.path.join(boundary.MEDIA_DIR, "attachment-1.bin")
        with open(source, "wb") as handle:
            handle.write(data)
        binding = boundary.ensure_binding_record(self.config, "123456789012345678", None, "127.0.0.1", 45678)
        grant = boundary.grant_media(binding, source)
        with self.assertRaisesRegex(ValueError, "token is invalid"):
            boundary.read_granted_media(binding["interface"], grant["media_handle"], grant["media_sha256"], grant["media_size"], "0" * 64, 0, 1024)

        class Link:
            def request(self, operation, payload):
                if operation != "media.read":
                    raise AssertionError(operation)
                return boundary.read_granted_media(
                    payload["interface"], payload["handle"], payload["sha256"], payload["size"],
                    payload["token"], payload["offset"], min(payload["limit"], 8192),
                )

        payload = {"event_id": "media-event", "attachments": [{"filename": "attachment.bin", **grant}]}
        previous_interface = life.INTERFACE_ID
        life.INTERFACE_ID = binding["interface"]
        try:
            materialized = life.materialize_granted_media(self.config, Link(), payload)
        finally:
            life.INTERFACE_ID = previous_interface
        attachment = materialized["attachments"][0]
        self.assertTrue(attachment["payload_path"].startswith(boundary.MEDIA_DIR + os.sep))
        self.assertEqual(boundary.file_hexdigest(attachment["payload_path"]), grant["media_sha256"])
        self.assertNotIn("media_token", attachment)

    def test_authenticated_peer_is_bidirectional(self):
        left, right = socket.socketpair()
        left_peer = boundary.JsonPeer(left, 1024 * 1024, 2, lambda operation, payload: {"side": "left", "operation": operation, "payload": payload})
        right_peer = boundary.JsonPeer(right, 1024 * 1024, 2, lambda operation, payload: {"side": "right", "operation": operation, "payload": payload})
        try:
            self.assertEqual(left_peer.request("ping", {"n": 1})["side"], "right")
            self.assertEqual(right_peer.request("pong", {"n": 2})["side"], "left")
        finally:
            left_peer.close()
            right_peer.close()

    def test_pairing_approval_consumes_finite_expiring_intent(self):
        code = boundary.register_pairing("123456789", now_ms=1000)
        self.assertFalse(boundary.pairing_approved("123456789", now_ms=1001))
        self.assertEqual(
            boundary.approve_pairing(code, now_ms=1001),
            {"status": "approved", "user_id": "123456789"},
        )
        self.assertTrue(boundary.pairing_approved("123456789", now_ms=1002))
        with self.assertRaisesRegex(ValueError, "absent or expired"):
            boundary.approve_pairing(code, now_ms=1003)
        expired = boundary.register_pairing("987654321", now_ms=2000)
        with self.assertRaisesRegex(ValueError, "absent or expired"):
            boundary.approve_pairing(expired, now_ms=2000 + boundary.PAIRING_TTL_MS)

    @unittest.skipUnless(os.name == "posix", "no-follow topology reads require POSIX")
    def test_topology_comparison_refuses_symlink_leaf(self):
        outside = os.path.join(TEMP.name, "outside-topology.json")
        link = os.path.join(REGISTRY, "linked-topology.json")
        with open(outside, "w", encoding="utf-8") as handle:
            handle.write("{}")
        os.symlink(outside, link)
        with self.assertRaises(OSError):
            boundary.missing_topology_writes(self.config, {link: "{}"})

    def test_topology_ownership_rotates_with_cas_and_rejects_drift(self):
        target = os.path.join(REGISTRY, "world_discord_private", "life.json")
        os.makedirs(os.path.dirname(target))
        before = '{"runtime":"lock-one"}\n'
        after = '{"runtime":"lock-two"}\n'
        boundary.atomic_write_bytes(target, before.encode("utf-8"))
        boundary.commit_topology_ownership(self.config, {target: before})
        writes = boundary.missing_topology_writes(self.config, {target: after})
        self.assertEqual(writes, [{"path": target, "content": after, "expected_before_hash": boundary.octopus_content_hash(before.encode("utf-8"))}])
        boundary.atomic_write_bytes(target, after.encode("utf-8"))
        boundary.commit_topology_ownership(self.config, {target: after})
        boundary.atomic_write_bytes(target, b'{"runtime":"operator-drift"}\n')
        with self.assertRaisesRegex(RuntimeError, "drifted from its last package-authored bytes"):
            boundary.missing_topology_writes(self.config, {target: before})
        os.unlink(target)
        with self.assertRaisesRegex(RuntimeError, "disappeared"):
            boundary.missing_topology_writes(self.config, {target: before})
        known = {"schema_version": 1, "bindings": {"known": {"channel_id": "123456789012345678", "thread_id": None}}}
        hub = object()
        with mock.patch.object(boundary, "load_bindings", return_value=known), mock.patch.object(boundary, "ensure_private_binding") as ensure:
            boundary.reconcile_known_bindings(self.config, hub)
        ensure.assert_called_once_with(self.config, hub, "123456789012345678", None, None)

    def test_private_peer_bounds_concurrent_request_handlers(self):
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

        left, right = socket.socketpair()
        left_peer = boundary.JsonPeer(left, 1024 * 1024, 3, handler)
        right_peer = boundary.JsonPeer(right, 1024 * 1024, 3, lambda _operation, payload: payload)
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=24) as executor:
                results = list(executor.map(lambda value: right_peer.request("bounded", {"n": value}), range(24)))
            self.assertEqual(results, [{"n": value} for value in range(24)])
            self.assertLessEqual(peak, boundary.PRIVATE_REQUEST_WORKERS)
        finally:
            left_peer.close()
            right_peer.close()

    def test_private_peer_admits_only_active_response_ids(self):
        left, right = socket.socketpair()
        peer = boundary.JsonPeer(left, 1024 * 1024, 2, lambda _operation, _payload: {})
        try:
            with peer.condition:
                peer.next_id = 3
                peer.pending_requests.add(1)
            peer._record_response(1, {"type": "response", "request_id": 1, "ok": True})
            self.assertIn(1, peer.responses)
            with self.assertRaisesRegex(RuntimeError, "duplicate response"):
                peer._record_response(1, {"type": "response", "request_id": 1, "ok": True})
            with peer.condition:
                peer.pending_requests.discard(1)
                peer.responses.pop(1)
            peer._record_response(2, {"type": "response", "request_id": 2, "ok": True})
            self.assertNotIn(2, peer.responses)
            with self.assertRaisesRegex(RuntimeError, "unsolicited response"):
                peer._record_response(3, {"type": "response", "request_id": 3, "ok": True})
        finally:
            peer.close()
            right.close()

    def test_private_peer_reduces_handler_failures_to_digest_evidence(self):
        left, right = socket.socketpair()
        left_peer = boundary.JsonPeer(left, 1024 * 1024, 2, lambda _operation, _payload: {})
        right_peer = boundary.JsonPeer(right, 1024 * 1024, 2, lambda _operation, _payload: (_ for _ in ()).throw(RuntimeError("remote-private-body")))
        try:
            with self.assertRaises(RuntimeError) as raised:
                left_peer.request("fail", {})
            rendered = str(raised.exception)
            self.assertNotIn("remote-private-body", rendered)
            self.assertIn("error_sha256", rendered)
        finally:
            left_peer.close()
            right_peer.close()

    def test_mixer_frame_geometry_and_ducking(self):
        mixer = voice.VoiceMixer()
        mixer.set_ambient(voice.synth_ambient_pcm(0.1))
        self.assertEqual(len(mixer.read()), voice.FRAME_SIZE)
        mixer.play_speech(b"\x01\x00" * (voice.FRAME_SIZE // 2))
        self.assertTrue(mixer.speech_active)
        self.assertEqual(len(mixer.read()), voice.FRAME_SIZE)
        mixer.cleanup()

    def test_voice_receiver_releases_bounded_speaker_state(self):
        receiver = voice.VoiceReceiver(object(), 1.0, 0.1, 120.0, voice.FRAME_SIZE, voice.FRAME_SIZE * 2, 2, lambda _user_id: True)
        receiver.buffers[7] = bytearray(b"x" * voice.FRAME_SIZE)
        receiver.last_packet[7] = time.monotonic()
        receiver.ssrc_users[7] = 42
        receiver.decoders[7] = object()
        completed = receiver.completed()
        self.assertEqual(completed, [(42, b"x" * voice.FRAME_SIZE)])
        self.assertNotIn(7, receiver.buffers)
        self.assertNotIn(7, receiver.decoders)


if __name__ == "__main__":
    unittest.main()
