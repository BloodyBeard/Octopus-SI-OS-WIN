import copy
import hashlib
import json
import pathlib
import sys
import unittest


ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from cloud_work_contract import ContractError
from cloud_work_contract import build_apply_candidate
from cloud_work_contract import build_create_body
from cloud_work_contract import extract_prompt
from cloud_work_contract import load_operation_catalog
from cloud_work_contract import normalize_attempts
from cloud_work_contract import normalize_task_id
from cloud_work_contract import normalize_task_summary
from cloud_work_contract import validate_config
from cloud_work_contract import validate_request
from cloud_work_life import CloudTransport


def config():
    return validate_config(json.loads((ROOT / "config.json").read_text("utf-8")))


def unified_diff():
    return "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -0,0 +1 @@\n+cloud\n"


class CloudWorkContractTests(unittest.TestCase):
    def test_application_descriptor_and_life_are_self_contained(self):
        interface = json.loads((ROOT / "interface.json").read_text("utf-8"))
        life = json.loads((ROOT / "life.json").read_text("utf-8"))
        application = json.loads((ROOT / "application.json").read_text("utf-8"))
        context = json.loads(
            (ROOT.parent.parent / "artifact" / "artifact-application-cloud-work-context" / "artifact.json").read_text("utf-8")
        )
        transport = interface["interface_driver_config"]["transport"]
        self.assertEqual(interface["id"], "world_cloud_work")
        self.assertEqual((interface["interface_type"], interface["role"]), ("world", "service"))
        self.assertEqual(transport["auth_env_key"], "CLOUD_WORK_AUTH_TOKEN")
        self.assertNotIn("auth_token_env", transport)
        self.assertEqual(application["category"], "plugin")
        self.assertEqual(application["interfaces"], ["world_cloud_work"])
        self.assertIn("artifact-application-cloud-work-context", [item["artifact_id"] for item in application["artifact_bindings"]])
        required = life["processes"][0]["required_files"]
        self.assertIn("registry/interface/world_cloud_work/cloud_work_life.py", required)
        self.assertTrue(all((ROOT.parent.parent.parent / item).is_file() for item in required))
        self.assertIn("installed, available, or connected alone never proves liveness", context["text"])

    def test_operation_catalog_is_complete_and_exact(self):
        catalog = load_operation_catalog(ROOT)
        self.assertEqual(len(catalog), 9)
        self.assertEqual(catalog["cloud_work.task.apply_candidate"]["output_label"], "cloud_work.apply_candidate")
        self.assertEqual(catalog["cloud_work.task.diff"]["output_label"], "cloud_work.diff")

    def test_starting_diff_is_explicit_evidence_but_wire_exact(self):
        diff = unified_diff()
        digest = hashlib.sha256(diff.encode()).hexdigest()
        request = validate_request(
            {
                "schema_version": 1,
                "request_id": "create-1",
                "operation": "cloud_work.task.create",
                "arguments": {
                    "environment_id": "env-1",
                    "prompt": "Repair the contract",
                    "git_ref": "main",
                    "attempts": 4,
                    "starting_diff": {
                        "artifact_ref": "artifact://starting-diff",
                        "sha256": digest,
                        "unified_diff": diff,
                    },
                },
            },
            config(),
        )
        body = build_create_body(request["arguments"])
        self.assertEqual(body["input_items"][1]["output_diff"], {"diff": diff})
        self.assertEqual(body["metadata"], {"best_of_n": 4})
        bad = copy.deepcopy(request)
        bad["arguments"]["starting_diff"]["sha256"] = "0" * 64
        with self.assertRaisesRegex(ContractError, "does not match"):
            validate_request(bad, config())

    def test_environment_lists_accept_wire_arrays_and_merge_repository_evidence(self):
        transport = object.__new__(CloudTransport)
        transport.config = config()
        transport.last_evidence = None
        responses = [
            [{"id": "env-a", "label": "Repository", "is_pinned": True}],
            [
                {"id": "env-a", "label": "Global A", "is_pinned": False},
                {"id": "env-b", "label": "Global B", "is_pinned": False},
            ],
        ]
        transport.request = lambda *args, **kwargs: responses.pop(0)
        result = transport.list_environments("git@github.com:openai/codex.git")
        self.assertEqual([item["id"] for item in result["environments"]], ["env-a", "env-b"])
        self.assertTrue(result["environments"][0]["pinned"])
        self.assertEqual(result["environments"][0]["label"], "Repository")
        self.assertEqual(result["environments"][0]["repository_hint"], "https://github.com/openai/codex")
        self.assertEqual(result["partial_failures"], {})

    def test_attempt_normalization_is_deterministic_across_timestamp_shapes(self):
        details = {
            "current_assistant_turn": {
                "id": "turn-current",
                "attempt_placement": 2,
                "created_at": "2026-08-27T10:00:00Z",
                "turn_status": "completed",
                "output_items": [],
            }
        }
        siblings = {
            "sibling_turns": [
                {
                    "id": "turn-first",
                    "attempt_placement": 1,
                    "created_at": 100.5,
                    "turn_status": "failed",
                    "output_items": [],
                }
            ]
        }
        attempts = normalize_attempts(details, siblings)
        self.assertEqual([item["turn_id"] for item in attempts], ["turn-first", "turn-current"])
        self.assertEqual([item["status"] for item in attempts], ["failed", "completed"])
        self.assertEqual(normalize_task_id("https://chatgpt.com/codex/tasks/task-123"), "task-123")
        self.assertEqual(
            validate_request(
                {
                    "schema_version": 1,
                    "request_id": "environment-1",
                    "operation": "cloud_work.environment.list",
                    "arguments": {"repository_remote": "ssh://git@github.com/openai/codex.git"},
                },
                config(),
            )["arguments"]["repository_remote"],
            "ssh://git@github.com/openai/codex.git",
        )
        with self.assertRaises(ContractError):
            validate_request(
                {
                    "schema_version": 1,
                    "request_id": "list-1",
                    "operation": "cloud_work.task.list",
                    "arguments": {"cursor": None},
                },
                config(),
            )

    def test_status_diff_stats_and_zero_based_attempts_match_wire_contract(self):
        request_value = validate_request(
            {
                "schema_version": 1,
                "request_id": "result-1",
                "operation": "cloud_work.task.result",
                "arguments": {"task_id": "task-1", "attempt_placement": 0},
            },
            config(),
        )
        self.assertEqual(request_value["arguments"]["attempt_placement"], 0)
        summary = normalize_task_summary(
            {
                "task": {"id": "task-1", "title": "Task"},
                "task_status_display": {
                    "state": "applied",
                    "latest_turn_status_display": {
                        "diff_stats": {"files_modified": 3, "lines_added": 8, "lines_removed": 2},
                        "sibling_turn_ids": ["turn-2", "turn-3"],
                    },
                },
            }
        )
        self.assertEqual(summary["status"], "applied")
        self.assertEqual(summary["diff_summary"], {"files_changed": 3, "lines_added": 8, "lines_removed": 2})
        self.assertEqual(summary["attempt_total"], 3)

    def test_wire_turn_families_feed_prompt_messages_and_current_diff(self):
        details = {
            "task": {"id": "task-1", "title": "Task"},
            "current_user_turn": {
                "input_items": [
                    {
                        "type": "message",
                        "role": "user",
                        "content": [
                            {"content_type": "text", "text": "First line"},
                            {"content_type": "text", "text": "Second line"},
                        ],
                    }
                ]
            },
            "current_assistant_turn": {
                "id": "turn-1",
                "attempt_placement": 0,
                "turn_status": "completed",
                "output_items": [
                    {"type": "message", "content": [{"content_type": "text", "text": "Assistant response"}]}
                ],
            },
            "current_diff_task_turn": {
                "output_items": [
                    {"type": "output_diff", "diff": "diff --git a/a.txt b/a.txt\n+new line\n"}
                ]
            },
        }
        attempts = normalize_attempts(details)
        self.assertEqual(extract_prompt(details), "First line\n\nSecond line")
        self.assertEqual(attempts[0]["messages"], ["Assistant response"])
        self.assertEqual(attempts[0]["diff"], "diff --git a/a.txt b/a.txt\n+new line\n")

    def test_apply_candidate_is_non_applied_and_hands_off_exact_review_evidence(self):
        target = {
            "root_slot": "worktree_root",
            "repository_identity": "https://github.com/example/project",
            "branch": "codex/cloud-candidate",
            "base_ref": "main",
            "head_commit": "1" * 40,
        }
        candidate = build_apply_candidate(
            "task-1",
            {"environment_id": "env-1"},
            {"turn_id": "turn-2", "placement": 2, "status": "completed", "diff": unified_diff()},
            target,
        )
        handoff = candidate["review_handoff"]
        self.assertEqual(len(handoff["required_verified_artifacts"]), 1)
        requirement = handoff["required_verified_artifacts"][0]
        self.assertFalse(candidate["applied"])
        self.assertEqual(candidate["effect_status"], "not_applied")
        self.assertEqual(handoff["worker_environment_kind"], "git_worktree")
        self.assertEqual(handoff["door_interface"], "system_coder_env")
        self.assertEqual(requirement["mark"], "operator.approval")
        self.assertEqual(requirement["verifier"], "operator_approval")
        self.assertEqual(requirement["params"]["request_id"], handoff["review_request"]["request_id"])
        self.assertEqual(handoff["candidate_digest"], candidate["source"]["diff"]["sha256"])
        self.assertEqual(candidate["target"], target)
        self.assertFalse(handoff["effect_result_contract"]["cloud_interface_may_report_apply_success"])


if __name__ == "__main__":
    unittest.main()
