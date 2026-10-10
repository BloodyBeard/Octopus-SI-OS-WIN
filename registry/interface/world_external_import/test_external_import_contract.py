import json
import os
import pathlib
import sys
import tempfile
import time
import unittest


ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from external_import_contract import ContractError
from external_import_contract import inspect_request
from external_import_contract import load_config
from external_import_contract import validate_request


def write(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(value, (dict, list)):
        path.write_text(json.dumps(value), encoding="utf-8")
    else:
        path.write_text(value, encoding="utf-8")


def request(product, classes=None):
    value = {
        "schema_version": 1,
        "request_id": f"inspect-{product}",
        "operation": "external_import.inspect",
        "source": {
            "product": product,
            "root_slot": "external_import_source_root",
            "scope": "home",
            "relative_root": ".",
        },
        "session_limits": {"max_age_days": 30, "max_sessions": 50},
    }
    if classes is not None:
        value["classes"] = classes
    return value


class ExternalImportContractTests(unittest.TestCase):
    def setUp(self):
        self.config = load_config(ROOT)

    def test_application_descriptor_and_distribution_are_self_contained(self):
        interface = json.loads((ROOT / "interface.json").read_text("utf-8"))
        life = json.loads((ROOT / "life.json").read_text("utf-8"))
        application = json.loads((ROOT / "application.json").read_text("utf-8"))
        context = json.loads(
            (ROOT.parent.parent / "artifact" / "artifact-application-external-import-context" / "artifact.json").read_text("utf-8")
        )
        package = json.loads(
            (ROOT.parent.parent.parent / "app" / "distribution" / "local-packages" / "packages" / "world_external_import.json").read_text("utf-8")
        )
        process = life["processes"][0]
        self.assertEqual(interface["id"], "world_external_import")
        self.assertEqual((interface["interface_type"], interface["role"]), ("world", "service"))
        self.assertEqual(application["interfaces"], ["world_external_import"])
        self.assertEqual(package["product_id"], "world_external_import")
        self.assertEqual(process["authority"]["read_root_slots"], ["external_import_source_root"])
        self.assertEqual(process["authority"]["write_root_slots"], [])
        self.assertFalse(process["authority"]["allow_network"])
        self.assertTrue(all((ROOT.parent.parent.parent / path).is_file() for path in process["required_files"]))
        self.assertIn("candidate is not installed", context["text"])
        self.assertIn("never copies credentials", context["text"])

    def test_request_has_no_install_merge_target_or_activation_shape(self):
        valid = request("claude_code", ["configuration"])
        self.assertEqual(validate_request(valid, self.config)["classes"], ["configuration"])
        for field in ("target", "install", "enable", "bind", "overwrite", "apply"):
            invalid = dict(valid)
            invalid[field] = True
            with self.subTest(field=field), self.assertRaises(ContractError):
                validate_request(invalid, self.config)
        invalid = request("claude_code", ["configuration"])
        invalid["source"] = {**invalid["source"], "relative_root": "/tmp"}
        with self.assertRaises(ContractError):
            validate_request(invalid, self.config)
        with self.assertRaises(ContractError):
            validate_request(request("unknown", ["configuration"]), self.config)

    def test_claude_surface_emits_candidates_and_redacted_receipts_without_effects(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write(
                root / ".claude" / "settings.json",
                {
                    "env": {"API_TOKEN": "super-secret-token", "PLAIN": "private-value"},
                    "model": "source-model",
                    "effort": "high",
                    "sandbox": {"enabled": True, "password": "sandbox-secret"},
                    "enabledPlugins": {"review@market": True, "off@market": False},
                    "extraKnownMarketplaces": {"scoped": {"source": "github", "repo": "example/scoped"}},
                    "hooks": {
                        "PostToolUse": [
                            {"matcher": "Write", "hooks": [{"type": "command", "command": ".claude/hooks/format.sh", "timeout": 10}]},
                            {"hooks": [{"type": "command", "command": "API_TOKEN=private formatter"}]},
                        ]
                    },
                    "unmappedFeature": {"state": "source-only"},
                },
            )
            write(root / ".claude" / "auth.json", {"access_token": "must-never-appear"})
            write(root / "CLAUDE.md", "Use the repository contract.\n")
            write(root / ".claude" / "skills" / "review" / "SKILL.md", "# Review\nInspect evidence.\n")
            write(root / ".claude" / "skills" / "review" / "scripts" / "check.py", "print('ok')\n")
            write(root / ".claude" / "commands" / "ship.md", "---\ndescription: Ship safely\n---\nRun checks.\n")
            write(root / ".claude" / "commands" / "private.md", "API_TOKEN=instruction-private-secret\n")
            write(
                root / ".claude" / "agents" / "builder.md",
                "---\nname: Builder\ndescription: Builds carefully\neffort: high\npermissionMode: acceptEdits\n---\nBuild from evidence.\n",
            )
            write(root / ".claude" / "projects" / "project-a" / "memory" / "MEMORY.md", "Project memory.\n")
            write(
                root / ".claude" / "projects" / "project-a" / "history.jsonl",
                '{"type":"user","cwd":"/workspace","message":{"content":"Repair import"}}\n'
                '{"type":"assistant","message":{"content":"Done"},"remoteMcpServersConfig":[{"name":"Calendar"}]}\n',
            )
            write(
                root / ".mcp.json",
                {
                    "mcpServers": {
                        "remote": {
                            "type": "http",
                            "url": "https://example.invalid/mcp",
                            "headers": {"Authorization": "Bearer ${REMOTE_TOKEN}", "X-Static": "private-header"},
                        },
                        "local": {
                            "type": "stdio",
                            "command": "node",
                            "args": ["server.js"],
                            "env": {"PUBLIC_NAME": "literal", "PASSTHROUGH": "${PASSTHROUGH}"},
                        },
                        "unknown": {"socket": "private.sock"},
                    }
                },
            )
            write(
                root / ".claude.json",
                {"projects": {str(root): {"mcpServers": {"project": {"url": "https://project.invalid/mcp"}}}}},
            )
            write(
                root / ".claude" / "plugins" / "known_marketplaces.json",
                {"market": {"source": {"source": "github", "repo": "example/plugins"}, "token": "market-secret"}},
            )
            bundle = inspect_request(request("claude_code"), {"external_import_source_root": str(root)}, self.config, 123)
            serialized = json.dumps(bundle, sort_keys=True)
            self.assertNotIn("super-secret-token", serialized)
            self.assertNotIn("must-never-appear", serialized)
            self.assertNotIn("sandbox-secret", serialized)
            self.assertNotIn("private-header", serialized)
            self.assertNotIn("market-secret", serialized)
            self.assertNotIn("instruction-private-secret", serialized)
            self.assertNotIn('"source_cwd"', serialized)
            self.assertEqual(bundle["observed_at_unix_ms"], 123)
            self.assertTrue(bundle["facts"]["inspection_completed"])
            self.assertEqual(bundle["facts"]["registry_writes"], 0)
            self.assertEqual(bundle["facts"]["bindings_authored"], 0)
            self.assertEqual(bundle["facts"]["secrets_copied"], 0)
            types = {item["candidate_type"] for item in bundle["candidates"]}
            self.assertTrue(
                {
                    "provider_interface_candidate",
                    "named_worker_template_candidate",
                    "worker_environment_kind_candidate",
                    "context_artifact_candidate",
                    "skill_artifact_candidate",
                    "interface_behavior_candidate",
                    "interface_application_candidate",
                    "application_source_candidate",
                    "memory_artifact_candidate",
                    "history_artifact_candidate",
                }.issubset(types)
            )
            remote = next(item for item in bundle["candidates"] if item["title"] == "Remote capability remote")
            self.assertEqual(remote["payload"]["transport"]["environment_header_requirements"], {"Authorization": "REMOTE_TOKEN"})
            self.assertTrue(any(item["title"] == "Remote capability project" for item in bundle["candidates"]))
            self.assertTrue(any(item["title"] == "Scoped plugin marketplace scoped" for item in bundle["candidates"]))
            local = next(item for item in bundle["candidates"] if item["title"] == "Local capability local")
            self.assertEqual(local["payload"]["process"]["environment_requirements"], ["PASSTHROUGH"])
            worker = next(item for item in bundle["candidates"] if item["title"] == "Worker template Builder")
            self.assertFalse(worker["payload"]["worker_pin_authored"])
            history = next(item for item in bundle["candidates"] if item["candidate_type"] == "history_artifact_candidate")
            self.assertFalse(history["payload"]["run_created"])
            self.assertFalse(history["payload"]["session_identity_created"])
            statuses = {item["status"] for item in bundle["receipts"]}
            self.assertTrue({"candidate", "refused", "unsupported"}.issubset(statuses))
            self.assertTrue(any(item["reason"] == "text_contains_private_value" for item in bundle["receipts"]))
            self.assertTrue(all(item["state"]["candidate_only"] and not item["state"]["installed"] for item in bundle["candidates"]))

    def test_cursor_surface_keeps_history_as_artifact_and_local_mcp_as_driver(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            write(root / ".cursorrules", "Keep changes narrow.\n")
            write(root / ".cursor" / "cli-config.json", {"model": "cursor-model", "env": {"CURSOR_TOKEN": "cursor-secret"}})
            write(
                root / ".cursor" / "mcp.json",
                {"mcpServers": {"files": {"command": "python3", "args": ["server.py"]}, "docs": {"url": "https://docs.invalid/mcp"}}},
            )
            write(root / ".cursor" / "skills-cursor" / "inspect" / "SKILL.md", "# Inspect\nRead the source.\n")
            write(
                root / ".cursor" / "agents" / "researcher.md",
                "---\nname: Researcher\ndescription: Finds evidence\n---\nResearch carefully.\n",
            )
            write(
                root / ".cursor" / "hooks.json",
                {"hooks": {"postToolUse": [{"command": "python3 format.py", "type": "command"}]}},
            )
            transcript = root / ".cursor" / "projects" / "repo" / "agent-transcripts" / "main.jsonl"
            write(
                transcript,
                '{"type":"message","role":"user","message":{"content":"Find docs"}}\n'
                '{"type":"tool_use","name":"mcp__docs__search","input":{"query":"x"}}\n',
            )
            now = time.time()
            os.utime(transcript, (now, now))
            bundle = inspect_request(request("cursor"), {"external_import_source_root": str(root)}, self.config, 456)
            serialized = json.dumps(bundle, sort_keys=True)
            self.assertNotIn("cursor-secret", serialized)
            self.assertTrue(any(item["candidate_type"] == "context_artifact_candidate" for item in bundle["candidates"]))
            self.assertTrue(any(item["candidate_type"] == "worker_environment_kind_candidate" for item in bundle["candidates"]))
            self.assertTrue(any(item["candidate_type"] == "interface_application_candidate" for item in bundle["candidates"]))
            history = next(item for item in bundle["candidates"] if item["candidate_type"] == "history_artifact_candidate")
            self.assertFalse(history["payload"]["transcript_inheritance"])
            self.assertIn("docs", history["payload"]["connector_evidence"])
            self.assertTrue(any(item["reason"] == "source_memory_not_supported" for item in bundle["receipts"]))

    def test_symlinks_are_refused_and_candidate_ids_are_content_stable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory)
            outside = root / "outside.md"
            write(outside, "outside\n")
            skill = root / ".claude" / "skills" / "safe"
            write(skill / "SKILL.md", "# Safe\n")
            os.symlink(outside, skill / "leak.md")
            first = inspect_request(request("claude_code", ["skills"]), {"external_import_source_root": str(root)}, self.config, 1)
            second = inspect_request(request("claude_code", ["skills"]), {"external_import_source_root": str(root)}, self.config, 2)
            self.assertEqual([item["candidate_id"] for item in first["candidates"]], [item["candidate_id"] for item in second["candidates"]])
            self.assertTrue(any(item["reason"] == "symlink_not_followed" for item in first["receipts"]))
            self.assertNotIn("outside", json.dumps(first))


if __name__ == "__main__":
    unittest.main()
