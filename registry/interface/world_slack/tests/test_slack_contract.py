import importlib
import hashlib
import json
import os
import socket
import struct
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

import slack_contract
from slack_wire import SlackApi
from slack_wire import SlackWireError
from slack_wire import WebSocket
from slack_wire import validate_proxy_url


class SlackContractTests(unittest.TestCase):
    def setUp(self):
        with open(ROOT / "config.json", "r", encoding="utf-8") as handle:
            self.config = json.load(handle)

    def test_config_and_private_binding_are_strict(self):
        slack_contract.validate_config(self.config, "world_slack")
        private = json.loads(json.dumps(self.config))
        private["mode"] = "binding"
        private["gateway_run_template"] = None
        private["binding"] = {
            "interface": "world_slack_private_1",
            "team_id": "T12345678",
            "channel_id": "D12345678",
            "thread_ts": "1720000000.000100",
            "run": "run_slack_private_1",
            "gateway_host": "127.0.0.1",
            "gateway_port": 32741,
            "secret_file": "/var/run/octopus/slack-private-1.secret",
            "bot_token_file": "/var/run/octopus/slack-private-1.bot-token",
        }
        slack_contract.validate_config(private, "world_slack_private_1")
        private["binding"]["unexpected"] = True
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            slack_contract.validate_config(private, "world_slack_private_1")

    def test_rich_text_preserves_quotes_lists_links_and_mentions(self):
        blocks = [
            {
                "type": "rich_text",
                "elements": [
                    {
                        "type": "rich_text_quote",
                        "elements": [
                            {
                                "type": "rich_text_section",
                                "elements": [
                                    {"type": "text", "text": "See "},
                                    {"type": "link", "text": "proof", "url": "https://example.com"},
                                    {"type": "user", "user_id": "U12345678"},
                                ],
                            }
                        ],
                    },
                    {
                        "type": "rich_text_list",
                        "style": "ordered",
                        "elements": [
                            {"type": "rich_text_section", "elements": [{"type": "text", "text": "one"}]},
                            {"type": "rich_text_section", "elements": [{"type": "text", "text": "two"}]},
                        ],
                    },
                ],
            }
        ]
        rendered = slack_contract.rich_text(blocks)
        self.assertIn("> See proof (https://example.com)<@U12345678>", rendered)
        self.assertIn("1. one", rendered)
        self.assertIn("2. two", rendered)

    def test_coordinates_cover_reactions_edits_and_interactions(self):
        reaction = {
            "type": "events_api",
            "payload": {
                "team_id": "T12345678",
                "event": {"type": "reaction_added", "item": {"channel": "C12345678", "ts": "1720000000.000100"}},
            },
        }
        self.assertEqual(
            slack_contract.event_coordinates(reaction),
            ("T12345678", "C12345678", None, "1720000000.000100"),
        )
        changed = {
            "type": "events_api",
            "payload": {
                "team_id": "T12345678",
                "event": {
                    "type": "message",
                    "subtype": "message_changed",
                    "channel": "C12345678",
                    "message": {"ts": "1720000001.000200", "thread_ts": "1720000000.000100"},
                },
            },
        }
        self.assertEqual(slack_contract.event_coordinates(changed)[2:], ("1720000000.000100", "1720000001.000200"))
        interactive = {
            "type": "interactive",
            "payload": {
                "team": {"id": "T12345678"},
                "container": {"channel_id": "C12345678", "thread_ts": "1720000000.000100", "message_ts": "1720000001.000200"},
            },
        }
        self.assertEqual(slack_contract.event_coordinates(interactive), ("T12345678", "C12345678", "1720000000.000100", "1720000001.000200"))

    def test_operation_vocabulary_rejects_unknown_effects(self):
        operation, args = slack_contract.normalize_operation(
            {"operation": "view.open", "args": {"trigger_handle": "a" * 64, "view": {"type": "modal"}}},
            self.config,
        )
        self.assertEqual(operation, "view.open")
        self.assertEqual(args["view"]["type"], "modal")
        with self.assertRaisesRegex(ValueError, "not supported"):
            slack_contract.normalize_operation({"operation": "admin.users.remove", "args": {}}, self.config)

    def test_markdown_is_rendered_without_touching_code(self):
        rendered = slack_contract.slack_mrkdwn("# Title\n**bold** [proof](https://example.com) `**code**`")
        self.assertEqual(rendered, "*Title*\n*bold* <https://example.com|proof> `**code**`")

    def test_websocket_reader_handles_ping_and_fragmentation(self):
        client, server = socket.socketpair()
        wire = WebSocket("wss://example.test/socket", 1000, 4096)
        wire.sock = client
        payload = json.dumps({"type": "events_api", "envelope_id": "e1"}).encode("utf-8")
        first = bytes([0x01, 2]) + payload[:2]
        ping = bytes([0x89, 1]) + b"p"
        second = bytes([0x80, len(payload) - 2]) + payload[2:]
        server.sendall(first + ping + second)
        self.assertEqual(wire.receive_json()["envelope_id"], "e1")
        pong = server.recv(1024)
        self.assertEqual(pong[0] & 0x0F, 10)
        wire.sock = None
        client.close()
        server.close()

    def test_websocket_rejects_fragmented_control_frame(self):
        client, server = socket.socketpair()
        wire = WebSocket("wss://example.test/socket", 1000, 4096)
        wire.sock = client
        server.sendall(bytes([0x09, 1]) + b"p")
        with self.assertRaisesRegex(SlackWireError, "control frame"):
            wire.receive_json()
        wire.sock = None
        client.close()
        server.close()

    def test_proxy_is_explicit_and_remote_plaintext_is_refused(self):
        with self.assertRaisesRegex(ValueError, "must use HTTPS"):
            validate_proxy_url("http://proxy.example:8080")
        with self.assertRaisesRegex(ValueError, "invalid characters"):
            validate_proxy_url(" https://proxy.example:8443")
        with self.assertRaisesRegex(ValueError, "credentials require"):
            validate_proxy_url("https://user@proxy.example:8443")
        with mock.patch.dict(os.environ, {"HTTP_PROXY": "http://ambient.invalid:8080", "HTTPS_PROXY": "http://ambient.invalid:8080", "OCTOPUS_SLACK_PROXY": ""}, clear=False):
            api = SlackApi(self.config, ["xoxb-" + "a" * 32], "xapp-" + "b" * 32)
            self.assertEqual(api.proxy_url, "")

    def test_remote_refusal_body_is_reduced_to_digest_evidence(self):
        api = SlackApi(self.config, ["xoxb-" + "a" * 32], "xapp-" + "b" * 32)
        body = json.dumps({"ok": False, "error": "invalid_auth", "secret": "remote-private-body"}).encode("utf-8")
        api._request = mock.Mock(return_value=(200, {}, body))
        with self.assertRaises(SlackWireError) as raised:
            api.call("auth.test")
        error = raised.exception
        self.assertEqual(error.details["body_bytes"], len(body))
        self.assertEqual(error.details["body_sha256"], hashlib.sha256(body).hexdigest())
        self.assertNotIn("remote-private-body", str(error))
        self.assertNotIn("remote-private-body", json.dumps(error.details))

    def test_external_ids_become_safe_stable_artifact_keys(self):
        first = slack_contract.envelope_key({"type": "events_api", "payload": {"event_id": "../../same"}})
        second = slack_contract.envelope_key({"type": "events_api", "payload": {"event_id": "../../same"}})
        self.assertEqual(first, second)
        self.assertRegex(first, r"^event-[0-9a-f]{64}$")
        self.assertRegex(slack_contract.envelope_key({"envelope_id": "../outside", "payload": {}}), r"^envelope-[0-9a-f]{64}$")


class SlackLifeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        root = Path(self.temp.name)
        for name in ("blobs", "var", "registry-interface", "registry-ee-template", "registry-tool", "registry-wek", "runs", "ees"):
            (root / name).mkdir()
        os.environ.update(
            {
                "OCTOPUS_INTERFACE_ID": "world_slack",
                "OCTOPUS_INTERFACE_DIR": str(ROOT),
                "OCTOPUS_VAR_DIR": str(root / "var"),
                "OCTOPUS_MOVEMENTS_JSONL": str(root / "movements.jsonl"),
                "OCTOPUS_BLOBS_DIR": str(root / "blobs"),
                "OCTOPUS_LEDGER_INDEX_SQLITE": str(root / "ledger-index.sqlite"),
                "OCTOPUS_ROOT_SLOTS_JSON": json.dumps({"registry_interface_root": str(root / "registry-interface"), "registry_ee_template_root": str(root / "registry-ee-template"), "registry_tool_root": str(root / "registry-tool"), "registry_worker_environment_kind_root": str(root / "registry-wek"), "run_records_root": str(root / "runs"), "ee_instances_root": str(root / "ees")}),
            }
        )
        sys.modules.pop("slack_life", None)
        self.life = importlib.import_module("slack_life")
        self.config = self.life.load_config()
        owners = self.config["private_run"]["interface_owners"]
        fixtures = {
            owners["slack"]: {"id": owners["slack"], "interface_type": "world", "role": "service", "direction": "bidirectional", "source_names": ["slack"], "interface_driver_ref": "slack_life"},
            owners["provider"]: {"id": owners["provider"], "interface_type": "world", "role": "provider", "direction": "bidirectional", "source_names": ["fixture.slack_provider"], "interface_driver_ref": "fixture_provider", "interface_driver_config": {"private_config": {"owner_marker": "slack"}}, "interface_policy": {}},
            owners["interface_authoring"]: {"id": owners["interface_authoring"], "interface_type": "system", "role": "system", "direction": "bidirectional", "source_names": ["interface_authoring"], "interface_driver_ref": "fixture_authoring"},
            owners["worker_environment"]: {"id": owners["worker_environment"], "interface_type": "system", "role": "system", "direction": "bidirectional", "source_names": ["system_room"], "interface_driver_ref": "system_room"},
            owners["worker"]: {"id": owners["worker"], "interface_type": "system", "role": "worker", "direction": "bidirectional", "source_names": ["slack"]},
        }
        for identifier, descriptor in fixtures.items():
            owner_dir = root / "registry-interface" / identifier
            owner_dir.mkdir()
            (owner_dir / "interface.json").write_text(json.dumps(descriptor), encoding="utf-8")
            if descriptor.get("role") != "worker":
                (owner_dir / "life.json").write_text(json.dumps({"processes": [{"bin": f"fixture-{identifier}", "args": []}]}), encoding="utf-8")
        gateway_tool = self.config["private_run"]["file_write_tool"]
        worker_policy = {"id": owners["worker"], "ceiling": {"interfaces": [owners["slack"], owners["provider"], owners["interface_authoring"], owners["worker_environment"], owners["worker"]], "tools": ["artifact.read", "artifact.send", gateway_tool], "wek": ["system_room"], "mark_outcomes": ["tool.call.allowed"], "hard_blocked_tools": []}, "role_artifacts": self.config["private_run"]["role_artifacts"]}
        (root / "registry-interface" / owners["worker"] / "policy.json").write_text(json.dumps(worker_policy), encoding="utf-8")
        tool_dir = root / "registry-tool" / gateway_tool
        tool_dir.mkdir()
        (tool_dir / "tool.json").write_text(json.dumps({"id": gateway_tool, "interface": owners["worker_environment"], "process": {"bin": "fixture-file-write", "authority": {}}, "supported_worker_environment_kinds": ["system_room"], "description": "fixture", "args_schema": {"type": "object"}, "is_world_effect": True}), encoding="utf-8")
        template_owner = self.config["private_run"]["ee_template_owner"]
        template_dir = root / "registry-ee-template" / template_owner
        template_dir.mkdir()
        (template_dir / "ee_template.json").write_text(json.dumps({"id": template_owner, "goal_class": "fixture", "allowed_interfaces": list(fixtures), "allowed_worker_environment_kinds": ["system_room"], "allowed_root_slots": ["run_records_root"], "hard_blocked_tools": [], "allowed_child_ee_templates": [], "bootstrap_root": False}), encoding="utf-8")
        existence_dir = root / "registry-ee-template" / "existence"
        existence_dir.mkdir()
        (existence_dir / "ee_template.json").write_text(json.dumps({"id": "existence", "goal_class": "fixture", "allowed_interfaces": list(fixtures), "allowed_worker_environment_kinds": ["system_room"], "hard_blocked_tools": [], "allowed_child_ee_templates": [template_owner], "bootstrap_root": True}), encoding="utf-8")
        (root / "ees" / "ee_existence.json").write_text(json.dumps({"ee_id": "ee_existence", "template_name": "existence", "parent_ee_id": None, "goal": "existence"}), encoding="utf-8")
        kind_dir = root / "registry-wek" / "system_room"
        kind_dir.mkdir()
        (kind_dir / "worker_environment_kind.json").write_text(json.dumps({"worker_environment_kind_id": "system_room", "substrate_family": "process_exec", "supported_executor_kinds": ["exec"], "capabilities": ["filesystem"], "boundary_defaults": {"read_root_refs": [], "write_root_refs": [], "protected_read_only_subpaths": [], "network_ceiling": "denied"}, "lifecycle": {"requires_release": False}}), encoding="utf-8")

    def tearDown(self):
        self.temp.cleanup()

    def test_topology_ownership_rotates_with_cas_and_rejects_drift(self):
        registry = Path(json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])["registry_interface_root"])
        target = registry / "world_slack_private_rotation" / "life.json"
        target.parent.mkdir()
        before = '{"runtime":"lock-one"}\n'
        after = '{"runtime":"lock-two"}\n'
        self.life.atomic_bytes(str(target), before.encode("utf-8"))
        self.life.commit_topology_ownership(self.config, {str(target): before})
        writes = self.life.missing_topology_writes(self.config, {str(target): after})
        self.assertEqual(writes, [{"path": str(target), "content": after, "expected_before_hash": self.life.octopus_content_hash(before.encode("utf-8"))}])
        self.life.atomic_bytes(str(target), after.encode("utf-8"))
        self.life.commit_topology_ownership(self.config, {str(target): after})
        self.life.atomic_bytes(str(target), b'{"runtime":"operator-drift"}\n')
        with self.assertRaisesRegex(RuntimeError, "drifted from its last package-authored bytes"):
            self.life.missing_topology_writes(self.config, {str(target): before})
        target.unlink()
        with self.assertRaisesRegex(RuntimeError, "disappeared"):
            self.life.missing_topology_writes(self.config, {str(target): before})
        known = {"schema_version": 1, "bindings": {"known": {"team_id": "T12345678", "channel_id": "C12345678", "thread_ts": None}}}
        api = object()
        hub = object()
        with mock.patch.object(self.life, "load_bindings", return_value=known), mock.patch.object(self.life, "ensure_private_binding") as ensure:
            self.life.reconcile_known_bindings(api, self.config, hub)
        ensure.assert_called_once_with(api, self.config, hub, "T12345678", "C12345678", None)

    def test_mention_gate_becomes_thread_sticky(self):
        os.environ["SLACK_ALLOWED_USERS"] = "U12345678"
        os.environ["SLACK_ALLOWED_CHANNELS"] = "C12345678"
        state = self.life.load_state()
        state["bot_user_ids"] = {"T12345678": "U87654321"}
        self.life.atomic_json(self.life.STATE_PATH, state)
        mention = {
            "type": "events_api",
            "payload": {
                "team_id": "T12345678",
                "event": {
                    "type": "message",
                    "channel": "C12345678",
                    "user": "U12345678",
                    "ts": "1720000000.000100",
                    "text": "<@U87654321> help",
                },
            },
        }
        self.assertEqual(self.life.should_admit(self.config, mention, self.life.load_state()), (True, "mentioned"))
        reply = json.loads(json.dumps(mention))
        reply["payload"]["event"].update({"thread_ts": "1720000000.000100", "ts": "1720000001.000200", "text": "more"})
        self.assertEqual(self.life.should_admit(self.config, reply, self.life.load_state()), (True, "mentioned_thread"))

    def test_private_boundary_rejects_cross_channel_target(self):
        binding = {"channel_id": "C12345678", "thread_ts": "1720000000.000100"}
        with self.assertRaisesRegex(ValueError, "private channel"):
            self.life.target_channel({"channel": "C87654321"}, binding)
        with self.assertRaisesRegex(ValueError, "private thread"):
            self.life.target_thread({"thread_ts": "1720000001.000200"}, binding)

    def test_private_file_and_artifact_paths_fail_closed(self):
        root = Path(os.environ["OCTOPUS_VAR_DIR"])
        secret = root / "secret.key"
        secret.write_text("secret\n", encoding="utf-8")
        secret.chmod(0o600)
        with self.life.open_private_read(str(secret)) as handle:
            self.assertEqual(handle.read(), b"secret\n")
        secret.chmod(0o644)
        with self.assertRaisesRegex(PermissionError, "0600"):
            self.life.open_private_read(str(secret))
        link = root / "secret-link.key"
        link.symlink_to(secret)
        with self.assertRaisesRegex(PermissionError, "regular file"):
            self.life.open_private_read(str(link))
        with self.assertRaisesRegex(ValueError, "unsafe characters"):
            self.life.materialize_artifact("slack.event", b"{}", "../escape")

    def test_sanitizer_redacts_nested_secret_encodings(self):
        token = "xoxb-super-secret-value"
        proxy = "https://proxy-user:proxy-pass@proxy.example:8443"
        with mock.patch.dict(os.environ, {"SLACK_BOT_TOKEN": token, "OCTOPUS_SLACK_PROXY": proxy}, clear=False):
            value = self.life.sanitize_external_error({token: [token, "eG94Yi1zdXBlci1zZWNyZXQtdmFsdWU=", proxy]}, self.config)
        encoded = json.dumps(value, sort_keys=True)
        self.assertNotIn(token, encoded)
        self.assertNotIn("proxy-pass", encoded)

    def test_ledger_fallback_bounds_rows_before_json_allocation(self):
        path = Path(os.environ["OCTOPUS_MOVEMENTS_JSONL"])
        path.write_bytes(b"{" + b"x" * 256 + b"}\n")
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "exceeds 256 bytes"):
            self.life.read_ledger(0, 128, 256)
        path.write_bytes(b'{"seq":0}')
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaisesRegex(RuntimeError, "is partial"):
            self.life.read_ledger(0, 128, 256)
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
            "to": {"Interface": "world_slack"},
            "invocation_metadata": {"runtime": 7},
        }
        movement = {
            "seq": 7,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_slack"},
            "invocation_metadata": {
                "runtime": {
                    "run": "run_slack",
                    "ee": "ee_slack",
                    "worker": "system_worker_slack",
                    "checkpoint": "cp_slack",
                    "interface": "world_slack",
                    "epoch": 0,
                }
            },
            "artifact": {"content": [0] * 32},
        }
        path.write_text(
            "".join(json.dumps(row) + "\n" for row in (foreign, legacy, movement)),
            encoding="utf-8",
        )
        self.life._LEDGER_FALLBACK_IDENTITY = None
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        expected = [(7, [0] * 32, "run_slack")]
        self.assertEqual(self.life.read_ledger(7, 128, 1024), expected)
        self.assertEqual(self.life.read_ledger(7, 128, 1024), expected)
        self.assertEqual(self.life.read_ledger(8, 128, 1024), [])
        malformed = {
            "seq": 8,
            "kind": "InterfaceOutbound",
            "from": {"Interface": "world_provider_test"},
            "to": {"Interface": "world_slack"},
            "invocation_metadata": {"runtime": {"interface": "world_slack"}},
        }
        path.write_text(json.dumps(malformed) + "\n", encoding="utf-8")
        self.life._LEDGER_FALLBACK_IDENTITY = None
        self.life._LEDGER_FALLBACK_OFFSET = 0
        self.life._LEDGER_FALLBACK_SIZE = 0
        with self.assertRaises(ValueError):
            self.life.read_ledger(8, 128, 1024)

    def test_binding_child_receives_one_bot_token_and_no_app_token(self):
        bot_token = "xoxb-" + "c" * 32
        token_file = self.life.store_binding_bot_token("b" * 32, bot_token)
        ipc_file, _ = self.life.ensure_binding_secret("b" * 32)
        config = json.loads(json.dumps(self.config))
        config["mode"] = "binding"
        config["gateway_run_template"] = None
        config["binding"] = {
            "interface": "world_slack",
            "team_id": "T12345678",
            "channel_id": "C12345678",
            "thread_ts": None,
            "run": "run_slack_private",
            "gateway_host": "127.0.0.1",
            "gateway_port": 32741,
            "secret_file": ipc_file,
            "bot_token_file": token_file,
        }
        slack_contract.validate_config(config, "world_slack")
        with mock.patch.dict(os.environ, {"SLACK_BOT_TOKEN": "xoxb-" + "d" * 32, "SLACK_APP_TOKEN": "xapp-" + "e" * 32}, clear=False):
            self.assertEqual(self.life.resolve_tokens(config), ([bot_token], None))
        life = self.life.child_life_json(config, ipc_file, token_file)["processes"][0]
        self.assertNotIn(config["transport"]["bot_token_env"], life["inherit_env"])
        self.assertNotIn(config["transport"]["app_token_env"], life["inherit_env"])
        self.assertEqual(life["required_env"], [])
        runtime_source = os.path.realpath(os.environ["OCTOPUS_CONTENT_HASH_RUNTIME"])
        self.assertEqual(life["authority"]["read_paths"], [ipc_file, token_file, runtime_source])
        self.assertEqual(life["env"], {"OCTOPUS_CONTENT_HASH_RUNTIME_SOURCE": runtime_source})
        self.assertEqual(life["args"][0]["repository_file"], "registry/interface/world_slack/octopus_python_boot.py")
        self.assertNotIn(runtime_source, life["authority"]["write_paths"])
        self.assertEqual(life["authority"]["executable_paths"], [runtime_source])
        self.assertNotIn(bot_token, json.dumps(life))
        root_life = json.loads((ROOT / "life.json").read_text(encoding="utf-8"))["processes"][0]
        self.assertIn(config["transport"]["bot_token_env"], root_life["required_env"])
        self.assertIn(config["transport"]["app_token_env"], root_life["required_env"])
        self.assertEqual(root_life["private_var_directories"], ["content-hash-python-runtime"])
        self.assertEqual(root_life["authority"]["executable_env_paths"], [])
        self.assertEqual(root_life["authority"]["executable_env_subpaths"], [{"env": "OCTOPUS_VAR_DIR", "relative_path": "content-hash-python-runtime"}])
        self.assertEqual(life["io_limits"]["max_payload_bytes"], 268435456)

    def test_provider_child_is_a_complete_concrete_interface(self):
        owner = self.config["private_run"]["interface_owners"]["provider"]
        descriptor = self.life.provider_descriptor("world_provider_private", owner, {"model": "fixture-model"})
        self.assertNotIn("reference", descriptor)
        self.assertEqual(descriptor["id"], "world_provider_private")
        self.assertEqual(descriptor["source_names"], ["fixture.slack_provider"])
        self.assertEqual(descriptor["interface_type"], "world")
        self.assertEqual(descriptor["role"], "provider")
        self.assertEqual(descriptor["interface_driver_ref"], "fixture_provider")
        self.assertEqual(descriptor["interface_driver_config"]["private_config"]["owner_marker"], "slack")
        self.assertEqual(descriptor["interface_driver_config"]["private_config"]["model"], "fixture-model")

    def test_private_topology_owns_exact_surfaces_and_policy(self):
        binding = self.life.ensure_binding_record(self.config, "T12345678", "D12345678", "1720000000.000100", "127.0.0.1", 32741)
        self.life.store_binding_bot_token(binding["opaque_id"], "xoxb-" + "a" * 32)
        topology = self.life.binding_topology(self.config, binding)
        roots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        interface_root = Path(os.path.realpath(roots["registry_interface_root"]))
        template_id = self.config["private_run"]["id_prefixes"]["ee_template"] + binding["opaque_id"]
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
        tool_root = Path(os.path.realpath(roots["registry_tool_root"]))
        private_tool = json.loads(topology[str(tool_root / binding["file_write_tool"] / "tool.json")])
        self.assertEqual(private_tool["id"], binding["file_write_tool"])
        self.assertEqual(private_tool["interface"], binding["worker_environment_interface"])
        private_paths = [write["path"] for write in self.life.missing_topology_writes(self.config, self.life.package_owned_topology(topology))]
        private_paths.extend(write["path"] for write in self.life.private_state_writes(self.config, binding, topology))
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
        self.assertNotIn(self.config["private_run"]["file_write_tool"], worker_policy["ceiling"]["tools"])
        self.assertNotIn("file.write", worker_policy["ceiling"]["tools"])
        self.assertNotIn("system_room", worker_policy["ceiling"]["interfaces"])
        for owner in self.config["private_run"]["interface_owners"].values():
            if owner != self.config["private_run"]["interface_owners"]["operator"]:
                self.assertNotIn(owner, worker_policy["ceiling"]["interfaces"])
        template_root = Path(os.path.realpath(roots["registry_ee_template_root"]))
        template = json.loads(topology[str(template_root / template_id / "ee_template.json")])
        self.assertEqual(template["id"], template_id)
        self.assertEqual(set(template["allowed_interfaces"]), exact_interfaces)
        run_root = Path(os.path.realpath(roots["run_records_root"]))
        ee_root = Path(os.path.realpath(roots["ee_instances_root"]))
        self.assertEqual(private_paths[-3:], [str(template_root / template_id / "ee_template.json"), str(ee_root / f"{binding['ee']}.json"), str(run_root / f"{binding['run']}.json")])
        run = json.loads(topology[str(run_root / f"{binding['run']}.json")])
        authoring_call = next(row for row in run["checkpoints"] if row["id"].endswith("_authoring_call"))
        authoring_result = next(row for row in run["checkpoints"] if row["id"].endswith("_authoring_result"))
        self.assertEqual(authoring_call["outbound_interface_bindings"][0]["interface"], binding["worker_environment_interface"])
        self.assertEqual(authoring_result["interface_binding"]["interface"], binding["worker_environment_interface"])
        original_parent = json.loads((template_root / "existence" / "ee_template.json").read_text(encoding="utf-8"))
        shared = self.life.shared_policy_writes(self.config, binding)
        shared_by_path = {row["path"]: json.loads(row["content"]) for row in shared}
        parent = shared_by_path[str(template_root / "existence" / "ee_template.json")]
        self.assertEqual(parent["allowed_interfaces"][:len(original_parent["allowed_interfaces"])], original_parent["allowed_interfaces"])
        self.assertEqual(parent["allowed_child_ee_templates"][:len(original_parent["allowed_child_ee_templates"])], original_parent["allowed_child_ee_templates"])
        self.assertTrue(exact_interfaces.issubset(parent["allowed_interfaces"]))
        self.assertIn(template_id, parent["allowed_child_ee_templates"])
        kind_root = Path(os.path.realpath(roots["registry_worker_environment_kind_root"]))
        self.assertNotIn(str(kind_root / "system_room" / "worker_environment_kind.json"), shared_by_path)

    def test_legacy_binding_door_axis_is_rejected(self):
        binding = self.life.ensure_binding_record(self.config, "T12345678", "D12345678", None, "127.0.0.1", 32741)
        state = json.loads(Path(self.life.BINDINGS_PATH).read_text(encoding="utf-8"))
        state["bindings"][self.life.coordinate_key("T12345678", "D12345678", None)]["authoring_environment_interface"] = "system_room"
        self.life.atomic_json(self.life.BINDINGS_PATH, state, indent=2)
        with self.assertRaisesRegex(ValueError, "unknown keys"):
            self.life.load_bindings()

    def test_shared_policy_wait_detects_fresh_cas_conflict(self):
        attempted = {"/shared.json": "1" * 64}
        pending = [{"path": "/shared.json", "content": "{}", "expected_before_hash": "2" * 64}]
        with mock.patch.object(self.life, "shared_policy_writes", return_value=pending):
            self.assertFalse(self.life.wait_for_shared_policy_admissions(self.config, {}, attempted))

    def test_existing_private_run_with_legacy_door_fails_closed(self):
        binding = self.life.ensure_binding_record(self.config, "T12345678", "D12345678", "1720000000.000200", "127.0.0.1", 32741)
        self.life.store_binding_bot_token(binding["opaque_id"], "xoxb-" + "b" * 32)
        topology = self.life.binding_topology(self.config, binding)
        roots = json.loads(os.environ["OCTOPUS_ROOT_SLOTS_JSON"])
        ee_path = str(Path(os.path.realpath(roots["ee_instances_root"])) / f"{binding['ee']}.json")
        run_path = str(Path(os.path.realpath(roots["run_records_root"])) / f"{binding['run']}.json")
        Path(ee_path).write_text(topology[ee_path], encoding="utf-8")
        run = json.loads(topology[run_path])
        run["checkpoints"][1]["outbound_interface_bindings"] = [slack_contract.authored_interface_binding("system_context_slack_legacy")]
        Path(run_path).write_text(json.dumps(run), encoding="utf-8")
        with self.assertRaisesRegex(RuntimeError, "stale or foreign outbound"):
            self.life.private_state_writes(self.config, binding, topology)

    def test_binding_child_owns_final_admission_and_receipt(self):
        config = json.loads(json.dumps(self.config))
        config["mode"] = "binding"
        config["gateway_run_template"] = None
        config["binding"] = {"interface": "world_slack_private", "team_id": "T12345678", "channel_id": "D12345678", "thread_ts": "1720000000.000100", "run": "run_slack_private", "gateway_host": "127.0.0.1", "gateway_port": 32741, "secret_file": "/tmp/slack.secret", "bot_token_file": "/tmp/slack.bot"}
        envelope = {"envelope_id": "Ev-private-final-gate", "type": "events_api", "payload": {"team_id": "T12345678", "event": {"type": "message", "channel": "D12345678", "user": "U12345678", "ts": "1720000000.000100", "text": "hello"}}}
        raw = json.dumps(envelope, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
        candidate = {"schema_version": 1, "event_key": slack_contract.envelope_key(envelope), "raw_sha256": hashlib.sha256(raw).hexdigest(), "envelope": envelope}
        with mock.patch.dict(os.environ, {"SLACK_ALLOW_ALL_USERS": "true", "SLACK_ALLOWED_USERS": "", "SLACK_ALLOWED_CHANNELS": "", "SLACK_ALLOWED_TEAMS": ""}, clear=False), mock.patch.object(self.life, "hand_json_idempotent", return_value=71) as hand:
            self.assertEqual(self.life.process_private_candidate(mock.Mock(), config, candidate), 71)
            self.assertEqual(self.life.process_private_candidate(mock.Mock(), config, candidate), 71)
        hand.assert_called_once()
        changed = json.loads(json.dumps(candidate))
        changed["envelope"]["payload"]["event"]["text"] = "changed"
        changed_raw = json.dumps(changed["envelope"], ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
        changed["raw_sha256"] = hashlib.sha256(changed_raw).hexdigest()
        with mock.patch.dict(os.environ, {"SLACK_ALLOW_ALL_USERS": "true"}, clear=False), self.assertRaisesRegex(RuntimeError, "different raw bytes"):
            self.life.process_private_candidate(mock.Mock(), config, changed)

    def test_authoring_batch_orders_gateway_private_topology_then_shared_admissions(self):
        binding = {"opaque_id": "a" * 32, "status": "declared"}
        api = mock.Mock()
        api.token_for_team.return_value = "xoxb-" + "a" * 32
        hub = mock.Mock(host="127.0.0.1", port=32741)
        hub.wait_for.return_value = object()
        topology_writes = [{"path": "/registry/interface/private/interface.json", "content": "{}", "expected_before_hash": None}]
        state_writes = [{"path": "/ees/private.json", "content": "{}", "expected_before_hash": None}, {"path": "/runs/private.json", "content": "{}", "expected_before_hash": None}]
        shared_writes = [{"path": "/registry/ee-template/parent.json", "content": "{}", "expected_before_hash": "1" * 64}, {"path": "/registry/wek/system_room.json", "content": "{}", "expected_before_hash": "2" * 64}]
        with mock.patch.object(self.life, "ensure_binding_record", return_value=binding), mock.patch.object(self.life, "binding_topology", return_value={}), mock.patch.object(self.life, "missing_topology_writes", return_value=topology_writes), mock.patch.object(self.life, "private_state_writes", return_value=state_writes), mock.patch.object(self.life, "shared_policy_writes", return_value=shared_writes), mock.patch.object(self.life, "wait_for_shared_policy_admissions", return_value=True), mock.patch.object(self.life, "gateway_rearm_write", return_value={"path": "/runs/gateway.json", "content": "{}", "expected_before_hash": "f" * 64}), mock.patch.object(self.life, "hand_json") as hand, mock.patch.object(self.life, "mark_binding_ready"):
            self.life.ensure_private_binding(api, self.config, hub, "T12345678", "C12345678", None)
        arguments = hand.call_args.args[1]["arguments"]["writes"]
        self.assertEqual(arguments[0]["path"], "/runs/gateway.json")
        self.assertEqual([write["path"] for write in arguments], ["/runs/gateway.json", "/registry/interface/private/interface.json", "/ees/private.json", "/runs/private.json", "/registry/ee-template/parent.json", "/registry/wek/system_room.json"])

    def test_authoring_cas_conflict_recomputes_shared_writes(self):
        config = json.loads(json.dumps(self.config))
        config["private_run"]["authoring_retry_attempts"] = 2
        binding = {"opaque_id": "b" * 32, "status": "ready"}
        api = mock.Mock()
        api.token_for_team.return_value = "xoxb-" + "b" * 32
        hub = mock.Mock(host="127.0.0.1", port=32741)
        hub.wait_for.return_value = object()
        shared = [[{"path": "/shared-first.json", "content": "{}", "expected_before_hash": "1" * 64}], [{"path": "/shared-second.json", "content": "{}", "expected_before_hash": "2" * 64}]]
        with mock.patch.object(self.life, "ensure_binding_record", return_value=binding), mock.patch.object(self.life, "binding_topology", return_value={}) as topology, mock.patch.object(self.life, "missing_topology_writes", side_effect=[[], []]), mock.patch.object(self.life, "private_state_writes", side_effect=[[], []]), mock.patch.object(self.life, "shared_policy_writes", side_effect=shared) as shared_writes, mock.patch.object(self.life, "wait_for_shared_policy_admissions", side_effect=[False, True]) as wait_shared, mock.patch.object(self.life, "gateway_rearm_write", return_value={"path": "/runs/gateway.json", "content": "{}", "expected_before_hash": "f" * 64}), mock.patch.object(self.life, "hand_json") as hand, mock.patch.object(self.life, "commit_topology_ownership"):
            self.life.ensure_private_binding(api, config, hub, "T12345678", "C12345678", None)
        self.assertEqual(topology.call_count, 2)
        self.assertEqual(shared_writes.call_count, 2)
        self.assertEqual(wait_shared.call_count, 2)
        self.assertEqual(hand.call_count, 2)
        self.assertEqual(hand.call_args.args[1]["arguments"]["writes"][1]["path"], "/shared-second.json")

    def test_binding_hub_removes_idle_peer_after_eof(self):
        config = json.loads(json.dumps(self.config))
        config["routing"]["ipc_port"] = 0
        coordinate = ("T12345678", "C12345678", None)
        secret = "s" * 64
        declaration = {coordinate: {"interface": "world_slack_private_test", "secret_sha256": hashlib.sha256(secret.encode()).hexdigest()}}
        with mock.patch.object(self.life, "load_binding_registry", return_value=declaration):
            hub = self.life.BindingHub(config)
            client = socket.create_connection((hub.host, hub.port), timeout=1)
            self.life.write_frame(client, {"kind": "register", "interface": "world_slack_private_test", "team_id": coordinate[0], "channel_id": coordinate[1], "thread_ts": None, "secret": secret}, hub.maximum)
            self.assertEqual(self.life.read_frame(client, hub.maximum)["kind"], "registered")
            self.assertIsNotNone(hub.wait_for(*coordinate, 1000))
            client.close()
            deadline = time.monotonic() + 1
            while hub.private_connection(*coordinate) is not None and time.monotonic() < deadline:
                time.sleep(0.01)
            self.assertIsNone(hub.private_connection(*coordinate))
            hub.listener.close()

    def test_binding_hub_retires_unauthenticated_registration_by_deadline(self):
        config = json.loads(json.dumps(self.config))
        config["routing"]["ipc_port"] = 0
        config["transport"]["connect_timeout_ms"] = 50
        hub = self.life.BindingHub(config)
        client = socket.create_connection((hub.host, hub.port), timeout=1)
        try:
            client.settimeout(1)
            response = self.life.read_frame(client, hub.maximum)
            self.assertEqual(response["kind"], "registration_refused")
        finally:
            client.close()
            hub.listener.close()


if __name__ == "__main__":
    unittest.main()
