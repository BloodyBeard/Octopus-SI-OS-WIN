import base64
import copy
import concurrent.futures
import hashlib
import importlib.util
import json
import os
import socket
import stat
import subprocess
import shutil
import tempfile
import threading
import time
import unittest
from pathlib import Path
from unittest import mock

PACKAGE = Path(__file__).resolve().parent
STATE = Path(tempfile.mkdtemp(prefix="octopus-whatsapp-contract-"))
for name in (
    "blobs",
    "runs",
    "ee",
    "registry-ee-template",
    "registry-interface",
    "registry-tool",
    "registry-worker-environment-kind",
):
    (STATE / name).mkdir()
os.environ.update({
    "OCTOPUS_INTERFACE_ID": "world_whatsapp",
    "OCTOPUS_INTERFACE_DIR": str(PACKAGE),
    "OCTOPUS_VAR_DIR": str(STATE / "var"),
    "OCTOPUS_MOVEMENTS_JSONL": str(STATE / "movements.jsonl"),
    "OCTOPUS_BLOBS_DIR": str(STATE / "blobs"),
    "OCTOPUS_LEDGER_INDEX_SQLITE": str(STATE / "ledger-index.sqlite"),
    "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({
        "registry_interface_root": str(STATE / "registry-interface"),
        "registry_tool_root": str(STATE / "registry-tool"),
        "registry_ee_template_root": str(STATE / "registry-ee-template"),
        "registry_worker_environment_kind_root": str(
            STATE / "registry-worker-environment-kind"
        ),
        "run_records_root": str(STATE / "runs"),
        "ee_instances_root": str(STATE / "ee"),
    }),
})
SPEC = importlib.util.spec_from_file_location("whatsapp_life_contract", PACKAGE / "whatsapp_life.py")
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class WhatsAppBootstrapEnvironmentTest(unittest.TestCase):
    def test_dependency_subprocess_environment_is_allowlisted(self):
        proxy = "https://person:secret@proxy.example:8443"
        inherited = {
            "PATH": "/usr/local/bin:/usr/bin:/bin",
            "TMPDIR": "/tmp/whatsapp-dependencies",
            "NODE_EXTRA_CA_CERTS": "/etc/ssl/node-ca.pem",
            "OCTOPUS_WHATSAPP_SESSION_DIR": "/private/whatsapp-session",
            "OCTOPUS_VAR_DIR": "/private/interface-state",
            "OCTOPUS_MOVEMENTS_JSONL": "/private/session/movements.jsonl",
            "NODE_PATH": "/ambient/node-modules",
            "HTTPS_PROXY": "http://ambient-proxy.example",
        }
        with mock.patch.dict(os.environ, inherited, clear=True):
            environment = MODULE.dependency_process_environment(
                "/opt/node/bin/node",
                proxy,
                "/private/dependency-cache",
            )
        self.assertEqual(environment["PATH"], "/opt/node/bin:" + inherited["PATH"])
        self.assertEqual(environment["TMPDIR"], inherited["TMPDIR"])
        self.assertEqual(environment["NODE_EXTRA_CA_CERTS"], inherited["NODE_EXTRA_CA_CERTS"])
        self.assertEqual(environment["HTTP_PROXY"], proxy)
        self.assertEqual(environment["HTTPS_PROXY"], proxy)
        self.assertEqual(environment["npm_config_cache"], "/private/dependency-cache")
        for name in ("OCTOPUS_WHATSAPP_SESSION_DIR", "OCTOPUS_VAR_DIR", "OCTOPUS_MOVEMENTS_JSONL", "NODE_PATH"):
            self.assertNotIn(name, environment)


class WhatsAppContractTest(unittest.TestCase):
    def setUp(self):
        for name in (
            "OCTOPUS_WHATSAPP_ALLOWED_USERS",
            "OCTOPUS_WHATSAPP_ALLOWED_GROUPS",
            "OCTOPUS_WHATSAPP_ALLOW_ALL_USERS",
            "OCTOPUS_WHATSAPP_FREE_RESPONSE_GROUPS",
            "OCTOPUS_WHATSAPP_MENTION_PATTERNS",
            "OCTOPUS_WHATSAPP_MODE",
            "OCTOPUS_WHATSAPP_PROXY",
            "OCTOPUS_WHATSAPP_SESSION_DIR",
        ):
            os.environ.pop(name, None)
        self.config = MODULE.load_config()
        interface_owners = set(self.config["private_run"]["owners"].values())
        interface_owners.add(self.config["private_run"]["worker_environment_door_owner"])
        interface_owners.add(self.config["private_run"]["gateway_worker_environment_interface"])
        for owner in interface_owners:
            owner_dir = STATE / "registry-interface" / owner
            owner_dir.mkdir(exist_ok=True)
            for filename in ("interface.json", "life.json"):
                source = PACKAGE.parent / owner / filename
                target = owner_dir / filename
                if source.is_file():
                    shutil.copyfile(source, target)
                elif target.exists():
                    target.unlink()
        for owner in (
            self.config["private_run"]["file_write_tool"],
            self.config["private_run"]["gateway_file_write_tool"],
        ):
            owner_dir = STATE / "registry-tool" / owner
            owner_dir.mkdir(exist_ok=True)
            shutil.copyfile(
                PACKAGE.parent.parent / "tool" / owner / "tool.json",
                owner_dir / "tool.json",
            )
        template_owner = self.config["private_run"]["ee_template"]
        template_dir = STATE / "registry-ee-template" / template_owner
        template_dir.mkdir(exist_ok=True)
        shutil.copyfile(
            PACKAGE.parent.parent
            / "ee_template"
            / template_owner
            / "ee_template.json",
            template_dir / "ee_template.json",
        )
        parent_template = "existence"
        parent_template_dir = STATE / "registry-ee-template" / parent_template
        parent_template_dir.mkdir(exist_ok=True)
        shutil.copyfile(
            PACKAGE.parent.parent
            / "ee_template"
            / parent_template
            / "ee_template.json",
            parent_template_dir / "ee_template.json",
        )
        (STATE / "ee" / f"{self.config['private_run']['parent_ee']}.json").write_text(
            json.dumps(
                {
                    "ee_id": self.config["private_run"]["parent_ee"],
                    "template_name": parent_template,
                    "parent_ee_id": None,
                    "goal": "Exist",
                }
            ),
            encoding="utf-8",
        )
        kind = self.config["private_run"]["worker_environment_kind"]
        kind_dir = STATE / "registry-worker-environment-kind" / kind
        kind_dir.mkdir(exist_ok=True)
        shutil.copyfile(
            PACKAGE.parent.parent
            / "worker_environment_kind"
            / kind
            / "worker_environment_kind.json",
            kind_dir / "worker_environment_kind.json",
        )

    def test_topology_ownership_rotates_with_cas_and_rejects_drift(self):
        target = STATE / "registry-interface" / "world_whatsapp_private_rotation" / "life.json"
        target.parent.mkdir(exist_ok=True)
        before = '{"runtime":"lock-one"}\n'
        after = '{"runtime":"lock-two"}\n'
        MODULE.atomic_write_bytes(str(target), before.encode("utf-8"))
        MODULE.commit_topology_ownership(self.config, {str(target): before})
        writes = MODULE.missing_topology_writes(self.config, {str(target): after})
        self.assertEqual(writes, [{"path": str(target), "content": after, "expected_before_hash": MODULE.octopus_content_hash(before.encode("utf-8"))}])
        MODULE.atomic_write_bytes(str(target), after.encode("utf-8"))
        MODULE.commit_topology_ownership(self.config, {str(target): after})
        MODULE.atomic_write_bytes(str(target), b'{"runtime":"operator-drift"}\n')
        with self.assertRaisesRegex(RuntimeError, "drifted from its last package-authored bytes"):
            MODULE.missing_topology_writes(self.config, {str(target): before})
        target.unlink()
        with self.assertRaisesRegex(RuntimeError, "disappeared"):
            MODULE.missing_topology_writes(self.config, {str(target): before})
        known = {"schema_version": 1, "bindings": {"known": {"chat_id": "15550001111@s.whatsapp.net"}}}
        hub = object()
        with mock.patch.object(MODULE, "load_bindings", return_value=known), mock.patch.object(MODULE, "ensure_private_binding") as ensure:
            MODULE.reconcile_known_bindings(self.config, hub)
        ensure.assert_called_once_with(self.config, hub, "15550001111@s.whatsapp.net")

    def test_config_and_identifier_contract(self):
        self.assertEqual(self.config["mode"], "gateway")
        self.assertEqual(MODULE.normalize_identifier("+15551234567:8@s.whatsapp.net"), "15551234567")
        self.assertEqual(MODULE.normalize_jid("+1 (555) 123-4567"), "15551234567@s.whatsapp.net")

    def test_ledger_fallback_bounds_rows_before_json_allocation(self):
        path = Path(MODULE.MOVEMENTS_JSONL)
        path.write_bytes(b"{" + b"x" * 256 + b"}\n")
        MODULE._LEDGER_FALLBACK_OFFSET = 0
        MODULE._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "exceeds 256 bytes"):
            MODULE._read_outbound_ledger(0, 128, 256)
        path.write_bytes(b'{"seq":0}')
        MODULE._LEDGER_FALLBACK_OFFSET = 0
        MODULE._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "is partial"):
            MODULE._read_outbound_ledger(0, 128, 256)
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
            "to": {"Interface": "world_whatsapp"},
            "invocation_metadata": {"runtime": 7},
        }
        movement = {
            "seq": 7,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_whatsapp"},
            "invocation_metadata": {
                "runtime": {
                    "run": "run_whatsapp",
                    "ee": "ee_whatsapp",
                    "worker": "system_worker_whatsapp",
                    "checkpoint": "cp_whatsapp",
                    "interface": "world_whatsapp",
                    "epoch": 0,
                }
            },
            "artifact": {"content": [0] * 32},
        }
        path.write_text(
            "".join(json.dumps(row) + "\n" for row in (foreign, legacy, movement)),
            encoding="utf-8",
        )
        MODULE._LEDGER_FALLBACK_IDENTITY = None
        MODULE._LEDGER_FALLBACK_OFFSET = 0
        MODULE._LEDGER_FALLBACK_SIZE = 0
        expected = [(7, [0] * 32, "run_whatsapp")]
        self.assertEqual(MODULE._read_outbound_ledger(7, 128, 1024), expected)
        self.assertEqual(MODULE._read_outbound_ledger(7, 128, 1024), expected)
        self.assertEqual(MODULE._read_outbound_ledger(8, 128, 1024), [])
        malformed = {
            "seq": 8,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_whatsapp"},
            "invocation_metadata": {"runtime": {"interface": "world_whatsapp"}},
        }
        path.write_text(json.dumps(malformed) + "\n", encoding="utf-8")
        MODULE._LEDGER_FALLBACK_IDENTITY = None
        MODULE._LEDGER_FALLBACK_OFFSET = 0
        MODULE._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaises(ValueError):
            MODULE._read_outbound_ledger(8, 128, 1024)

    def test_driver_ipc_secret_transport_is_loopback_only(self):
        invalid_gateway = copy.deepcopy(self.config)
        invalid_gateway["driver_ipc"]["listen"] = "0.0.0.0"
        with self.assertRaises(ValueError):
            MODULE.validate_config(invalid_gateway)
        with self.assertRaises(ValueError):
            MODULE.ensure_binding_record(
                self.config,
                "15550009999@s.whatsapp.net",
                "198.51.100.12",
                43219,
            )
        binding = MODULE.ensure_binding_record(
            self.config,
            "15550009999@s.whatsapp.net",
            "127.0.0.1",
            43219,
        )
        topology = MODULE.binding_topology(self.config, binding)
        config_path = next(path for path in topology if path.endswith(f"/{binding['interface']}/config.json"))
        child = json.loads(topology[config_path])
        child["binding"]["host"] = "198.51.100.12"
        with self.assertRaises(ValueError):
            MODULE.validate_config(child)

    def test_proxy_transport_rejects_remote_plaintext_and_url_smuggling(self):
        self.assertEqual(MODULE.validate_proxy_url("http://127.0.0.1:8080"), "http://127.0.0.1:8080")
        self.assertEqual(MODULE.validate_proxy_url("http://user:secret@[::1]:8080/"), "http://user:secret@[::1]:8080/")
        self.assertEqual(MODULE.validate_proxy_url("https://user:secret@proxy.example:8443"), "https://user:secret@proxy.example:8443")
        for value in (
            "http://user:secret@proxy.example:8080",
            "http://proxy.example:8080",
            "https://proxy.example/forward",
            "https://proxy.example?target=other",
            "https://proxy.example#secret",
            "https://user%0Aname:secret@proxy.example",
            "https://user%ZZ:secret@proxy.example",
            "socks5://127.0.0.1:1080",
        ):
            with self.subTest(value=value), self.assertRaises(ValueError):
                MODULE.validate_proxy_url(value)

    def test_proxy_error_redaction_preserves_class_and_host(self):
        proxy = "https://name%40tenant:p%40ss%2Fword@proxy.example:8443"
        basic = base64.b64encode(b"name@tenant:p@ss/word").decode("ascii")
        error = {
            "class": "ProxyConnectionError",
            "message": (
                f"ProxyConnectionError through {proxy}; "
                "decoded=name@tenant:p@ss/word password=p@ss/word "
                f"encoded=name%40tenant:p%40ss%2Fword Basic {basic}"
            ),
        }
        sanitized = MODULE.sanitize_external_error(error, proxy)
        rendered = json.dumps(sanitized, sort_keys=True)
        previous_proxy = MODULE._CONFIGURED_PROXY_URL
        original_hand = MODULE.hand_artifact
        try:
            MODULE._CONFIGURED_PROXY_URL = proxy
            MODULE.hand_artifact = lambda path: path
            manifest_path = MODULE.hand_failure(
                7291,
                "proxy_failed",
                {"error": error["message"]},
                "whatsapp-proxy-redaction-contract",
            )
        finally:
            MODULE.hand_artifact = original_hand
            MODULE._CONFIGURED_PROXY_URL = previous_proxy
        manifest = json.loads(Path(manifest_path).read_text("utf-8"))
        rendered += Path(manifest["payload_path"]).read_text("utf-8")
        for secret in (
            proxy,
            "name@tenant:p@ss/word",
            "p@ss/word",
            "name%40tenant:p%40ss%2Fword",
            "p%40ss%2Fword",
            basic,
        ):
            self.assertNotIn(secret, rendered)
        self.assertIn("ProxyConnectionError", rendered)
        self.assertIn("proxy.example:8443", rendered)

    def test_markdown_conversion_preserves_code(self):
        value = MODULE.format_message("# **Title**\n~~gone~~\n```py\n**code**\n```\n[x](https://x.test)")
        self.assertIn("*Title*", value)
        self.assertIn("~gone~", value)
        self.assertIn("**code**", value)
        self.assertIn("x (https://x.test)", value)

    def test_authz_is_deny_default_and_alias_aware(self):
        event = {"event_type": "message.create", "chat_id": "15551234567@s.whatsapp.net", "sender_id": "15551234567@s.whatsapp.net", "is_group": False}
        self.assertEqual(MODULE.event_authorized(self.config, event), (False, "user_allowlist"))
        configured = copy.deepcopy(self.config)
        configured["authz"]["allowed_users"] = ["15551234567"]
        self.assertEqual(MODULE.event_authorized(configured, event), (True, None))

    def test_group_needs_both_group_and_user_grants(self):
        event = {"event_type": "message.create", "chat_id": "12345@g.us", "sender_id": "15551234567@s.whatsapp.net", "is_group": True, "text": "@octopus hello", "bot_ids": ["999@s.whatsapp.net"], "mentioned_ids": []}
        configured = copy.deepcopy(self.config)
        configured["authz"]["allowed_users"] = ["15551234567"]
        self.assertEqual(MODULE.event_authorized(configured, event), (False, "group_allowlist"))
        configured["authz"]["allowed_groups"] = ["12345@g.us"]
        self.assertEqual(MODULE.event_authorized(configured, event), (True, None))

    def test_group_trigger_accepts_mention_reply_and_pattern(self):
        configured = copy.deepcopy(self.config)
        event = {"event_type": "message.create", "chat_id": "12345@g.us", "is_group": True, "text": "hello", "bot_ids": ["999@s.whatsapp.net"], "mentioned_ids": ["999@s.whatsapp.net"], "quoted": None}
        self.assertTrue(MODULE.event_should_trigger(configured, event))
        event["mentioned_ids"] = []
        self.assertFalse(MODULE.event_should_trigger(configured, event))
        configured["gating"]["mention_patterns"] = ["\\boctopus\\b"]
        event["text"] = "Octopus please"
        self.assertTrue(MODULE.event_should_trigger(configured, event))

    def test_outbound_operation_is_pinned_to_private_interface(self):
        binding = {"chat_id": "15551234567@s.whatsapp.net"}
        request = MODULE.operation_request(self.config, {"operation": "message.send", "text": "**hello**"}, binding, 42)
        self.assertEqual(request["payload"]["chat_id"], binding["chat_id"])
        self.assertEqual(request["payload"]["text"], "*hello*")
        with self.assertRaises(ValueError):
            MODULE.operation_request(self.config, {"operation": "message.send", "chat_id": "1999@s.whatsapp.net", "text": "escape"}, binding, 43)

    def test_media_operation_proves_size_and_hash(self):
        body = b"artifact-media"
        digest = MODULE.octopus_content_hash(body)
        (STATE / "blobs" / digest).write_bytes(body)
        request = MODULE.operation_request(self.config, {"operation": "media.send", "blob_id": digest, "mime_type": "image/png", "media_type": "image", "filename": "frame.png"}, {"chat_id": "15551234567@s.whatsapp.net"}, 44)
        self.assertEqual(request["payload"]["sha256"], hashlib.sha256(body).hexdigest())
        self.assertEqual(request["payload"]["size"], len(body))

    def test_product_operations_are_coordinate_bound_and_strict(self):
        binding = {"chat_id": "15551234567@s.whatsapp.net"}
        poll = MODULE.operation_request(self.config, {"operation": "poll.send", "question": "Choose", "options": ["A", "B"]}, binding, 45)
        self.assertEqual(poll["payload"]["options"], ["A", "B"])
        with self.assertRaises(ValueError):
            MODULE.operation_request(self.config, {"operation": "poll.send", "question": "Choose", "options": ["A", "A"]}, binding, 46)

        with self.assertRaises(ValueError):
            MODULE.operation_request(self.config, {"operation": "interactive.list.send", "text": "Choose", "button_text": "Open", "sections": [{"rows": [{"id": "same", "title": "A"}, {"id": "same", "title": "B"}]}]}, binding, 47)
        with self.assertRaises(ValueError):
            MODULE.operation_request(self.config, {"operation": "group.subject.set", "subject": "Escape"}, binding, 48)

    def test_declared_operation_catalog_rejects_unimplemented_entries(self):
        changed = copy.deepcopy(self.config)
        changed["outbound"]["allowed_operations"].append("product.fake")
        with self.assertRaisesRegex(ValueError, "unsupported operations"):
            MODULE.validate_config(changed)

    def test_private_topology_carries_only_a_secret_handle(self):
        binding = MODULE.ensure_binding_record(self.config, "15550001111@s.whatsapp.net", "127.0.0.1", 43211)
        topology = MODULE.binding_topology(self.config, binding)
        config_path = next(path for path in topology if path.endswith(f"/{binding['interface']}/config.json"))
        life_path = next(path for path in topology if path.endswith(f"/{binding['interface']}/life.json"))
        child_config = json.loads(topology[config_path])
        child_life = json.loads(topology[life_path])
        provider_path = next(path for path in topology if path.endswith(f"/{binding['provider_interface']}/interface.json"))
        provider_descriptor = json.loads(topology[provider_path])
        self.assertNotIn("reference", provider_descriptor)
        self.assertEqual(provider_descriptor["id"], binding["provider_interface"])
        self.assertEqual(provider_descriptor["source_names"], ["codex"])
        self.assertEqual(provider_descriptor["interface_driver_ref"], "codex_app_server")
        self.assertEqual(
            provider_descriptor["interface_driver_config"]["supported_reasoning_efforts"],
            ["low", "medium", "high", "xhigh"],
        )
        self.assertEqual(
            provider_descriptor["interface_driver_config"]["residual_ambient"]["transport"],
            "codex_app_server_cli",
        )
        self.assertEqual(provider_descriptor["interface_driver_config"]["private_config"]["model"], self.config["private_run"]["model"])
        self.assertEqual(
            provider_descriptor["interface_policy"]["variables"]["max_attempts"]["default"],
            5,
        )
        provider_life_path = next(
            path
            for path in topology
            if path.endswith(f"/{binding['provider_interface']}/life.json")
        )
        self.assertEqual(
            json.loads(topology[provider_life_path])["processes"][0]["bin"],
            "target/debug/octopus-provider-codex-life",
        )
        descriptor_paths = [
            path
            for path in topology
            if path.endswith("/interface.json")
            and str(STATE / "registry-interface") in path
        ]
        self.assertEqual(len(descriptor_paths), 5)
        for descriptor_path in descriptor_paths:
            descriptor = json.loads(topology[descriptor_path])
            self.assertNotIn("reference", descriptor)
            self.assertEqual(descriptor["id"], Path(descriptor_path).parent.name)
        worker_policy_path = next(
            path
            for path in topology
            if path.endswith(f"/{binding['worker_interface']}/policy.json")
        )
        worker_policy = json.loads(topology[worker_policy_path])
        self.assertEqual(
            worker_policy["ceiling"]["interfaces"],
            [
                binding["interface"],
                binding["provider_interface"],
                binding["interface_authoring_interface"],
                binding["worker_environment_interface"],
                binding["worker_interface"],
            ],
        )
        self.assertEqual(worker_policy["ceiling"]["tools"], [binding["file_write_tool"]])
        tool_path = next(
            path
            for path in topology
            if path.endswith(f"/{binding['file_write_tool']}/tool.json")
        )
        tool_descriptor = json.loads(topology[tool_path])
        self.assertEqual(tool_descriptor["id"], binding["file_write_tool"])
        self.assertEqual(tool_descriptor["interface"], binding["worker_environment_interface"])
        template_path = next(
            path
            for path in topology
            if path.endswith(f"/{binding['ee_template']}/ee_template.json")
        )
        exact_template = json.loads(topology[template_path])
        self.assertEqual(exact_template["id"], binding["ee_template"])
        self.assertEqual(
            exact_template["allowed_interfaces"],
            worker_policy["ceiling"]["interfaces"],
        )
        ee_path = next(path for path in topology if path.endswith(f"/{binding['ee']}.json"))
        self.assertEqual(json.loads(topology[ee_path])["template_name"], binding["ee_template"])
        secret_handle = child_config["binding"]["secret_handle"]
        self.assertTrue(Path(secret_handle).is_file())
        self.assertTrue(Path(secret_handle).resolve().is_relative_to((STATE / "var" / "binding-secrets").resolve()))
        self.assertNotIn("secret", child_config["binding"])
        secret_bytes = Path(secret_handle).read_bytes()
        topology_bytes = "".join(topology.values()).encode("utf-8")
        binding_state = Path(MODULE.BINDINGS_PATH).read_bytes()
        self.assertNotIn(secret_bytes, topology_bytes)
        self.assertNotIn(secret_bytes.hex().encode("ascii"), topology_bytes)
        self.assertNotIn(secret_bytes, binding_state)
        self.assertNotIn(secret_bytes.hex().encode("ascii"), binding_state)
        self.assertEqual(child_life["processes"][0]["max_in_flight_tool_processes"], 16)
        self.assertEqual(child_life["processes"][0]["io_limits"]["max_payload_bytes"], 268435456)

    def test_generated_child_clean_boot_reuses_only_verified_owner_implementation(self):
        binding = MODULE.ensure_binding_record(self.config, "15550002222@s.whatsapp.net", "127.0.0.1", 43212)
        topology = MODULE.binding_topology(self.config, binding)
        interface_dir = STATE / "registry-interface" / binding["interface"]
        interface_dir.mkdir(parents=True, exist_ok=True)
        for name in ("interface.json", "config.json", "life.json"):
            path = interface_dir / name
            source_path = next(key for key in topology if key.endswith(f"/{binding['interface']}/{name}"))
            path.write_text(topology[source_path], encoding="utf-8")
        child_life = json.loads((interface_dir / "life.json").read_text(encoding="utf-8"))
        child_config = json.loads((interface_dir / "config.json").read_text(encoding="utf-8"))
        process = child_life["processes"][0]
        child_var = STATE / "clean-child-var"
        shutil.rmtree(child_var, ignore_errors=True)
        child_var.mkdir(mode=0o700)
        movements = STATE / "clean-child-movements.jsonl"
        movements.write_bytes(b"")
        blobs = STATE / "clean-child-blobs"
        blobs.mkdir(exist_ok=True)
        environment = os.environ.copy()
        environment.update(process["env"])
        environment.update(
            {
                "OCTOPUS_INTERFACE_ID": binding["interface"],
                "OCTOPUS_INTERFACE_DIR": str(interface_dir),
                "OCTOPUS_VAR_DIR": str(child_var),
                "OCTOPUS_MOVEMENTS_JSONL": str(movements),
                "OCTOPUS_BLOBS_DIR": str(blobs),
                "OCTOPUS_LEDGER_INDEX_SQLITE": str(STATE / "clean-child-ledger-index.sqlite"),
                "OCTOPUS_INTERFACE_HOST_IO_LIMITS": json.dumps(process["io_limits"]),
            }
        )
        repo_root = Path(__file__).resolve().parents[3]
        arguments = [
            str(repo_root / argument["repository_file"])
            for argument in process["args"]
        ]
        cold_environment = dict(environment)
        cold_environment["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"] = str(
            STATE / "missing-owner-runtime" / Path(process["env"]["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"]).name
        )
        cold_result = subprocess.run(
            [process["bin"], *arguments, "--validate-config"],
            cwd=repo_root,
            env=cold_environment,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertNotEqual(cold_result.returncode, 0)
        self.assertEqual(cold_result.stdout, "")
        self.assertIn("failed its package integrity proof", cold_result.stderr)
        result = subprocess.run(
            [process["bin"], *arguments, "--validate-config"],
            cwd=repo_root,
            env=environment,
            capture_output=True,
            text=True,
            timeout=20,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["mode"], "binding")
        self.assertFalse((child_var / "content-hash-python-runtime").exists())
        runtime_source = process["env"]["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"]
        self.assertEqual(process["args"][0]["repository_file"], "registry/interface/world_whatsapp/octopus_python_boot.py")
        self.assertEqual(process["authority"]["read_paths"], [child_config["binding"]["secret_handle"], runtime_source])
        self.assertNotIn(runtime_source, process["authority"]["write_paths"])
        self.assertEqual(process["authority"]["executable_paths"], [runtime_source])
        gateway = json.loads((PACKAGE / "life.json").read_text(encoding="utf-8"))["processes"][0]
        self.assertEqual(gateway["private_var_directories"], ["bridge-runtime", "content-hash-python-runtime"])
        self.assertEqual(gateway["authority"]["executable_env_paths"], [])
        self.assertEqual(gateway["authority"]["executable_env_subpaths"], [{"env": "OCTOPUS_VAR_DIR", "relative_path": "bridge-runtime"}, {"env": "OCTOPUS_VAR_DIR", "relative_path": "content-hash-python-runtime"}])
        next_runtime = runtime_source + "-next-lock-identity"
        with mock.patch.dict(os.environ, {"OCTOPUS_CONTENT_HASH_RUNTIME": next_runtime}, clear=False):
            rewritten = MODULE.child_life_json(child_config["binding"]["secret_handle"])["processes"][0]
        self.assertEqual(rewritten["env"]["OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE"], next_runtime)
        self.assertNotEqual(json.dumps(process, sort_keys=True), json.dumps(rewritten, sort_keys=True))

    @unittest.skipUnless(os.name == "posix", "Unix permission contract")
    def test_private_state_is_0600_and_rejects_permissions_and_symlinks(self):
        target = STATE / "var" / "payloads" / "permission-contract.bin"
        MODULE.atomic_write_bytes(str(target), b"private")
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(target.parent.stat().st_mode), 0o700)
        os.chmod(target, 0o644)
        with self.assertRaises(PermissionError):
            MODULE.open_private_read(str(target))
        os.chmod(target, 0o600)

        outside = STATE / "outside-private-file"
        descriptor = os.open(outside, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        os.write(descriptor, b"outside")
        os.close(descriptor)
        link = STATE / "var" / "payloads" / "private-link"
        os.symlink(outside, link)
        with self.assertRaises((OSError, PermissionError)):
            MODULE.open_private_read(str(link))
        with self.assertRaises((OSError, PermissionError)):
            MODULE.atomic_write_bytes(str(link), b"replacement")
        self.assertEqual(outside.read_bytes(), b"outside")

        binding = MODULE.ensure_binding_record(self.config, "15550002222@s.whatsapp.net", "127.0.0.1", 43212)
        secret_path = Path(MODULE.binding_secret_path(binding))
        self.assertEqual(stat.S_IMODE(secret_path.stat().st_mode), 0o600)
        os.chmod(secret_path, 0o644)
        with self.assertRaises(PermissionError):
            MODULE.binding_secret(binding)
        os.chmod(secret_path, 0o600)
        with self.assertRaises(RuntimeError):
            MODULE.load_secret_handle(os.path.relpath(secret_path, Path.cwd()))

        MODULE._identity_salt()
        salt_path = Path(MODULE.IDENTITY_SALT_PATH)
        self.assertEqual(stat.S_IMODE(salt_path.stat().st_mode), 0o600)
        os.chmod(salt_path, 0o644)
        with self.assertRaises(PermissionError):
            MODULE._identity_salt()
        os.chmod(salt_path, 0o600)

    def test_topology_is_data_until_file_write_artifact_executes(self):
        binding = MODULE.ensure_binding_record(self.config, "15551234567@s.whatsapp.net", "127.0.0.1", 43210)
        topology = MODULE.binding_topology(self.config, binding)
        self.assertEqual(len(topology), 16)
        self.assertTrue(any(path.endswith(f"{binding['run']}.json") for path in topology))
        self.assertFalse((STATE / "runs" / f"{binding['run']}.json").exists())

    def test_worker_environment_door_and_authoring_commit_order_are_descriptor_owned(self):
        binding = MODULE.ensure_binding_record(
            self.config,
            "15550005555@s.whatsapp.net",
            "127.0.0.1",
            43215,
        )
        self.assertIn(
            binding["worker_environment_interface"],
            MODULE.binding_exact_interfaces(self.config, binding),
        )
        handed = []
        gateway_write = {
            "path": str(STATE / "gateway.json"),
            "content": "{}\n",
            "expected_before_hash": None,
        }
        topology_write = {
            "path": str(STATE / "topology.json"),
            "content": "{}\n",
            "expected_before_hash": None,
        }
        shared_write = {
            "path": str(STATE / "shared.json"),
            "content": "{}\n",
            "expected_before_hash": None,
        }
        hub = mock.Mock(host=binding["host"], port=binding["port"])
        with mock.patch.object(MODULE, "ensure_binding_record", return_value=binding), mock.patch.object(
            MODULE, "binding_topology", return_value={}
        ), mock.patch.object(MODULE, "package_owned_topology", return_value={}), mock.patch.object(
            MODULE, "missing_topology_writes", return_value=[topology_write]
        ), mock.patch.object(MODULE, "shared_admission_writes", return_value=[shared_write]), mock.patch.object(
            MODULE, "gateway_rearm_write", return_value=gateway_write
        ), mock.patch.object(
            MODULE,
            "hand_json",
            side_effect=lambda label, payload, key: self.assertEqual(
                payload["tool"], self.config["private_run"]["gateway_file_write_tool"]
            )
            or handed.append(payload["arguments"]["writes"])
            or 1,
        ), mock.patch.object(MODULE, "wait_for_shared_admissions", return_value=True), mock.patch.object(
            MODULE, "commit_topology_ownership"
        ), mock.patch.object(MODULE, "mark_binding_ready"):
            MODULE.ensure_private_binding(self.config, hub, binding["chat_id"])
        self.assertEqual(
            [write["path"] for write in handed[0]],
            [gateway_write["path"], topology_write["path"], shared_write["path"]],
        )

    def test_shared_admission_does_not_mutate_worker_kind(self):
        binding = MODULE.ensure_binding_record(
            self.config,
            "15550003333@s.whatsapp.net",
            "127.0.0.1",
            43213,
        )
        roots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        kind_path = (
            Path(roots["registry_worker_environment_kind_root"])
            / self.config["private_run"]["worker_environment_kind"]
            / "worker_environment_kind.json"
        )
        before = kind_path.read_bytes()
        writes = MODULE.shared_admission_writes(self.config, binding)
        self.assertNotIn(str(kind_path), {write["path"] for write in writes})
        self.assertEqual(kind_path.read_bytes(), before)
        topology = MODULE.binding_topology(self.config, binding)
        self.assertNotIn(str(kind_path), MODULE.package_owned_topology(topology))

    def test_parent_template_admission_is_stable_cas_and_not_topology_owned(self):
        binding = MODULE.ensure_binding_record(
            self.config,
            "15550004444@s.whatsapp.net",
            "127.0.0.1",
            43214,
        )
        first_writes = MODULE.parent_template_admission_writes(self.config, binding)
        self.assertEqual(len(first_writes), 1)
        first = first_writes[0]
        parent_path = Path(first["path"])
        original = json.loads(parent_path.read_text(encoding="utf-8"))
        first_content = json.loads(first["content"])
        exact_interfaces = MODULE.binding_exact_interfaces(self.config, binding)
        self.assertEqual(
            first_content["allowed_interfaces"][: len(original["allowed_interfaces"])],
            original["allowed_interfaces"],
        )
        self.assertEqual(
            first_content["allowed_interfaces"][len(original["allowed_interfaces"]) :],
            [value for value in exact_interfaces if value not in original["allowed_interfaces"]],
        )
        self.assertEqual(
            first_content["allowed_child_ee_templates"],
            [*original["allowed_child_ee_templates"], binding["ee_template"]],
        )
        concurrent = json.loads(parent_path.read_text(encoding="utf-8"))
        concurrent["allowed_interfaces"].append("world_concurrent_interface")
        concurrent["allowed_child_ee_templates"].append("concurrent_child_template")
        parent_path.write_text(json.dumps(concurrent), encoding="utf-8")
        second_writes = MODULE.parent_template_admission_writes(self.config, binding)
        self.assertEqual(len(second_writes), 1)
        second = second_writes[0]
        self.assertNotEqual(first["expected_before_hash"], second["expected_before_hash"])
        second_content = json.loads(second["content"])
        self.assertEqual(
            second_content["allowed_interfaces"][: len(concurrent["allowed_interfaces"])],
            concurrent["allowed_interfaces"],
        )
        self.assertEqual(
            second_content["allowed_child_ee_templates"][: len(concurrent["allowed_child_ee_templates"])],
            concurrent["allowed_child_ee_templates"],
        )
        topology = MODULE.binding_topology(self.config, binding)
        self.assertNotIn(str(parent_path), MODULE.package_owned_topology(topology))

    def test_bridge_command_frames_are_serialized_and_late_responses_retire(self):
        class PartialStream:
            def __init__(self):
                self.closed = False
                self.body = bytearray()

            def write(self, value):
                chunk = bytes(value[:7])
                time.sleep(0.0001)
                self.body.extend(chunk)
                return len(chunk)

            def flush(self):
                return None

        class Process:
            def __init__(self, stream):
                self.stdin = stream

            def poll(self):
                return None

        stream = PartialStream()
        bridge = MODULE.BridgeProcess(self.config, MODULE.queue.Queue())
        bridge.process = Process(stream)
        frames = [b'{"id":1,"payload":"' + b"a" * 8192 + b'"}\n', b'{"id":2,"payload":"' + b"b" * 8192 + b'"}\n']
        barrier = threading.Barrier(3)
        failures = []

        def writer(frame):
            try:
                barrier.wait()
                bridge._write_command_frame(frame)
            except Exception as error:
                failures.append(error)

        threads = [threading.Thread(target=writer, args=(frame,)) for frame in frames]
        for thread in threads:
            thread.start()
        barrier.wait()
        for thread in threads:
            thread.join(5)
        self.assertFalse(failures)
        self.assertIn(bytes(stream.body), (frames[0] + frames[1], frames[1] + frames[0]))

        with self.assertRaises(TimeoutError):
            bridge.command("bridge.health", {}, timeout_ms=5)
        retired_id = bridge.next_id - 1
        bridge._record_response(retired_id, {"type": "response", "id": retired_id, "ok": True, "result": {}})
        self.assertNotIn(retired_id, bridge.responses)
        self.assertNotIn(retired_id, bridge.pending_requests)

    def test_json_peer_drops_late_response_and_remains_usable(self):
        local, remote = socket.socketpair()
        peer = MODULE.JsonPeer(local, 4096, 0.01, lambda operation, payload: {})
        failures = []

        def serve():
            try:
                buffer = bytearray()
                first = MODULE.read_frame(remote, buffer, 4096)
                time.sleep(0.04)
                MODULE.write_frame(remote, {"type": "response", "id": first["id"], "ok": True, "result": {"late": True}}, 4096)
                second = MODULE.read_frame(remote, buffer, 4096)
                MODULE.write_frame(remote, {"type": "response", "id": second["id"], "ok": True, "result": {"late": False}}, 4096)
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

    def test_json_peer_bounds_concurrent_request_handlers(self):
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
        local_peer = MODULE.JsonPeer(local, 1024 * 1024, 3, handler)
        remote_peer = MODULE.JsonPeer(remote, 1024 * 1024, 3, lambda _operation, payload: payload)
        try:
            with concurrent.futures.ThreadPoolExecutor(max_workers=24) as executor:
                results = list(executor.map(lambda value: remote_peer.request("bounded", {"n": value}), range(24)))
            self.assertEqual(results, [{"n": value} for value in range(24)])
            self.assertLessEqual(peak, MODULE.PRIVATE_REQUEST_WORKERS)
        finally:
            local_peer.close()
            remote_peer.close()

    def test_pairing_qr_has_one_private_frame_and_no_stderr_copy(self):
        source = (PACKAGE / "bridge" / "bridge.mjs").read_text(encoding="utf-8")
        self.assertIn("writeFrame({ type: 'pairing', status: 'qr', qr: update.qr", source)
        self.assertNotIn("qrcode.generate", source)
        self.assertNotIn("from 'qrcode-terminal'", source)


if __name__ == "__main__":
    unittest.main()
