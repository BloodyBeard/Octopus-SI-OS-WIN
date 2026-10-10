import json
import pathlib
import subprocess
import sys
import unittest

REGISTRY = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = pathlib.Path(__file__).with_name("verify.py")
CONTRACT = REGISTRY / "interface" / "system_lifecycle_events" / "lifecycle_contract.json"
EVENT_SCHEMA = REGISTRY / "interface" / "system_lifecycle_events" / "event.schema.json"
DECISION_SCHEMA = REGISTRY / "interface" / "system_lifecycle_events" / "decision.schema.json"
CONNECTION_SCHEMA = REGISTRY / "interface" / "system_lifecycle_events" / "connection.schema.json"
AUTHORING_SCHEMA = REGISTRY / "interface" / "system_lifecycle_events" / "connection-authoring.schema.json"
AUTHORING_EXAMPLE = REGISTRY / "interface" / "system_lifecycle_events" / "connection-authoring.example.json"
DIGEST = "a" * 64
CONNECTION_DIGEST = "b" * 64
EVENT_ID = "lifecycle:" + "c" * 64


def verify(event, decision, handler_type="command", body_patch=None, params_patch=None):
    source_artifact = {"id": "subject-1", "content": DIGEST}
    event_artifact = {
        "label": "lifecycle.event",
        "schema_version": 1,
        "event_id": EVENT_ID,
        "connection_id": "connection-1",
        "connection_artifact": {
            "id": "lifecycle.connection:connection-1",
            "content": CONNECTION_DIGEST,
        },
        "emitter_run": "run-emitter",
        "owner_run": "run-owner",
        "handler_id": "handler-1",
        "event": event,
        "mode": "sync",
        "handler_type": handler_type,
        "subject_artifact": source_artifact,
        "trigger": "test-trigger",
        "data": {},
        "continuation": {
            "kind": "start_turn",
            "turn_input": [],
            "source_artifact": source_artifact,
        },
    }
    body = {
        "label": "lifecycle.decision",
        "schema_version": 1,
        "connection_id": "connection-1",
        "connection_artifact": {
            "id": "lifecycle.connection:connection-1",
            "content": CONNECTION_DIGEST,
        },
        "event_id": EVENT_ID,
        "emitter_run": "run-emitter",
        "owner_run": "run-owner",
        "handler_id": "handler-1",
        "event": event,
        "mode": "sync",
        "decision": decision,
        "handler_type": handler_type,
        "subject_artifact": source_artifact,
        "event_artifact": event_artifact,
    }
    if body_patch:
        body.update(body_patch)
    params = {
        "expected_event": event,
        "expected_decision": decision,
        "expected_handler_type": handler_type,
        "connection_id": "connection-1",
        "connection_artifact_id": "lifecycle.connection:connection-1",
        "connection_artifact_content": CONNECTION_DIGEST,
        "event_id": EVENT_ID,
        "emitter_run": "run-emitter",
        "owner_run": "run-owner",
        "handler_id": "handler-1",
        "mode": "sync",
        "subject_artifact_id": "subject-1",
        "subject_artifact_content": DIGEST,
        "event_artifact": event_artifact,
    }
    if params_patch:
        params.update(params_patch)
    request = {
        "artifact_label": "lifecycle.decision",
        "payload_utf8": json.dumps(body),
        "params": params,
    }
    completed = subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(request),
        text=True,
        capture_output=True,
        check=False,
    )
    if completed.returncode != 0:
        raise AssertionError(completed.stderr)
    return json.loads(completed.stdout)


class LifecycleDecisionVerifierTest(unittest.TestCase):
    def test_contract_covers_exact_events_and_handler_projections(self):
        contract = json.loads(CONTRACT.read_text())
        event_schema = json.loads(EVENT_SCHEMA.read_text())
        decision_schema = json.loads(DECISION_SCHEMA.read_text())
        connection_schema = json.loads(CONNECTION_SCHEMA.read_text())
        events = {entry["name"]: entry for entry in contract["events"]}
        self.assertEqual(len(events), 12)
        self.assertEqual(
            set(event_schema["$defs"]["event"]["enum"]),
            set(events),
        )
        self.assertEqual(
            set(decision_schema["properties"]["event"]["enum"]),
            {name for name, entry in events.items() if entry["positive_decision"]},
        )
        self.assertEqual(
            {entry["handler_type"] for entry in contract["handler_projections"]},
            {"command", "mcp_tool", "prompt", "agent"},
        )
        self.assertEqual(
            {
                destination["properties"]["type"]["const"]
                for destination in connection_schema["$defs"]["destination"]["oneOf"]
            },
            {"command", "mcp_tool", "prompt", "agent"},
        )
        correlation = {
            "connection_id",
            "connection_artifact",
            "event_id",
            "emitter_run",
            "owner_run",
            "handler_id",
            "event",
            "mode",
            "handler_type",
            "subject_artifact",
        }
        self.assertTrue(correlation.issubset(set(event_schema["required"])))
        self.assertTrue((correlation | {"event_artifact"}).issubset(set(decision_schema["required"])))

    def test_authoring_surface_keeps_installation_separate_from_exact_connection(self):
        schema = json.loads(AUTHORING_SCHEMA.read_text())
        authoring = json.loads(AUTHORING_EXAMPLE.read_text())
        self.assertEqual(schema["properties"]["installation_effect"]["const"], "none")
        self.assertEqual(authoring["installation_effect"], "none")
        self.assertEqual(
            authoring["connection_effect"],
            "exact_run_json_and_artifact_binding_only",
        )
        self.assertEqual(
            authoring["blocking_checkpoint_derivation"],
            {
                "event_label": "lifecycle.event",
                "decision_label": "lifecycle.decision",
                "mark": "lifecycle.decision.allowed",
                "verifier": "lifecycle_decision",
                "decision_interface": "system_lifecycle_events",
                "decision_outbound_interface": "world_provider_codex",
            },
        )
        purposes = set()
        for run in authoring["run_connections"]:
            self.assertEqual(run["connection_artifact_binding"], authoring["connection_artifact"])
            self.assertEqual(
                run["lifecycle_connection_pin"]["artifact"],
                authoring["connection_artifact"],
            )
            self.assertIn(
                "system_lifecycle_events",
                run["required_formed_grant"]["interfaces"],
            )
            self.assertEqual(
                set(run["connected_checkpoint_ids"]),
                {checkpoint["id"] for checkpoint in run["checkpoints"]},
            )
            for checkpoint in run["checkpoints"]:
                self.assertIn(
                    {"id": authoring["connection_artifact"]["id"]},
                    checkpoint["artifact_bindings"],
                )
                for binding in [
                    checkpoint["interface_binding"],
                    *checkpoint["outbound_interface_bindings"],
                ]:
                    policy = binding["config"]["artifact_policy"]
                    self.assertEqual(policy["mode"], "allow")
                    self.assertTrue(policy["names"] or policy["namespaces"])
                inbound = checkpoint["interface_binding"]
                if inbound["interface"] == "system_lifecycle_events":
                    self.assertNotIn(
                        "lifecycle.decision",
                        inbound["config"]["artifact_policy"]["names"],
                    )
                purposes.add(checkpoint["purpose"])
        self.assertEqual(
            purposes,
            {
                "lifecycle_event_output",
                "lifecycle_event_input",
                "handler_destination_output",
                "agent_destination_input",
                "handler_result_output",
            },
        )
        command = next(
            checkpoint
            for checkpoint in authoring["run_connections"][0]["checkpoints"]
            if checkpoint["id"] == "command_destination"
        )
        self.assertEqual(
            command["outbound_interface_bindings"][0]["interface"],
            "system_coder_env",
        )
        self.assertTrue((REGISTRY / "interface" / "system_coder_env" / "interface.json").is_file())
        reviewer = next(
            run
            for run in authoring["run_connections"]
            if run["roles"] == ["agent_destination"]
        )
        agent_input = next(
            checkpoint
            for checkpoint in reviewer["checkpoints"]
            if checkpoint["purpose"] == "agent_destination_input"
        )
        self.assertEqual(
            agent_input["interface_binding"]["interface"],
            "system_worker_reviewer",
        )
        self.assertEqual(
            agent_input["outbound_interface_bindings"][0]["interface"],
            "world_provider_codex",
        )
        for run in authoring["run_connections"]:
            for checkpoint in run["checkpoints"]:
                if checkpoint["purpose"] != "handler_result_output":
                    continue
                self.assertEqual(
                    checkpoint["interface_binding"]["interface"],
                    "world_provider_codex",
                )
                self.assertEqual(
                    checkpoint["outbound_interface_bindings"][0]["interface"],
                    "system_lifecycle_events",
                )

    def test_all_blocking_events_admit_their_positive_decision(self):
        contract = json.loads(CONTRACT.read_text())
        handlers = ["command", "mcp_tool", "prompt", "agent"]
        blocking = [
            entry for entry in contract["events"] if entry["positive_decision"] is not None
        ]
        for index, entry in enumerate(blocking):
            with self.subTest(event=entry["name"]):
                verdict = verify(
                    entry["name"],
                    entry["positive_decision"],
                    handlers[index % len(handlers)],
                )
                self.assertTrue(verdict["passed"], verdict)

    def test_reactive_events_cannot_close_the_blocking_mark(self):
        for event in ("session_end", "subagent_start", "interrupt"):
            with self.subTest(event=event):
                verdict = verify(event, "continue")
                self.assertFalse(verdict["passed"], verdict)

    def test_negative_and_mismatched_decisions_fail_closed(self):
        negative = verify(
            "pre_tool_use",
            "allow",
            body_patch={"decision": "deny", "reason": "policy refused"},
        )
        self.assertFalse(negative["passed"])
        mismatch = verify(
            "pre_tool_use",
            "allow",
            body_patch={"subject_artifact": {"id": "other", "content": DIGEST}},
        )
        self.assertFalse(mismatch["passed"])
        wrong_event = verify(
            "pre_tool_use",
            "allow",
            body_patch={"event_id": "lifecycle:" + "d" * 64},
        )
        self.assertFalse(wrong_event["passed"])
        wrong_connection = verify(
            "pre_tool_use",
            "allow",
            body_patch={
                "connection_artifact": {
                    "id": "lifecycle.connection:other",
                    "content": CONNECTION_DIGEST,
                }
            },
        )
        self.assertFalse(wrong_connection["passed"])
        wrong_source_event = verify(
            "pre_tool_use",
            "allow",
            body_patch={
                "event_artifact": {
                    "label": "lifecycle.event",
                    "trigger": "different-trigger",
                }
            },
        )
        self.assertFalse(wrong_source_event["passed"])

    def test_context_and_replacement_are_artifact_refs_with_event_limits(self):
        context_ref = {"id": "context-1", "content": "b" * 64}
        replacement_ref = {"id": "replacement-1", "content": "c" * 64}
        admitted = verify(
            "pre_tool_use",
            "allow",
            body_patch={
                "additional_context_artifacts": [context_ref],
                "replacement_artifact": replacement_ref,
            },
        )
        self.assertTrue(admitted["passed"], admitted)
        refused_context = verify(
            "pre_compact",
            "continue",
            body_patch={"additional_context_artifacts": [context_ref]},
        )
        self.assertFalse(refused_context["passed"])
        refused_replacement = verify(
            "post_tool_use",
            "continue",
            body_patch={"replacement_artifact": replacement_ref},
        )
        self.assertFalse(refused_replacement["passed"])


if __name__ == "__main__":
    unittest.main()
