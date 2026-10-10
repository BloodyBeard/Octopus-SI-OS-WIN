import base64
import json
import os
import pathlib
import sys
import unittest
from unittest import mock


ROOT = pathlib.Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from remote_operator_contract import ClientSegmentReassembler
from remote_operator_contract import ContractError
from remote_operator_contract import default_private_state
from remote_operator_contract import load_operation_catalog
from remote_operator_contract import normalize_clients
from remote_operator_contract import normalize_target
from remote_operator_contract import public_status
from remote_operator_contract import split_server_message
from remote_operator_contract import validate_artifact_message
from remote_operator_contract import validate_config
from remote_operator_contract import validate_request
from remote_operator_life import RemoteHttp
from remote_operator_life import WebSocketConnection


def config():
    return validate_config(json.loads((ROOT / "config.json").read_text("utf-8")))


def request(operation, arguments):
    return {
        "schema_version": 1,
        "request_id": "remote-request-1",
        "operation": operation,
        "arguments": arguments,
    }


class FakeSocket:
    def __init__(self):
        self.shutdown_called = False
        self.close_called = False

    def shutdown(self, mode):
        self.shutdown_called = True

    def close(self):
        self.close_called = True


class FrameSocket(FakeSocket):
    def __init__(self, value):
        super().__init__()
        self.value = bytearray(value)
        self.sent = []

    def recv(self, size):
        result = bytes(self.value[:size])
        del self.value[:size]
        return result

    def sendall(self, value):
        self.sent.append(value)


class RemoteOperatorContractTests(unittest.TestCase):
    def test_application_descriptor_and_life_are_self_contained(self):
        interface = json.loads((ROOT / "interface.json").read_text("utf-8"))
        life = json.loads((ROOT / "life.json").read_text("utf-8"))
        application = json.loads((ROOT / "application.json").read_text("utf-8"))
        context = json.loads(
            (ROOT.parent.parent / "artifact" / "artifact-application-remote-operator-context" / "artifact.json").read_text("utf-8")
        )
        transport = interface["interface_driver_config"]["transport"]
        self.assertEqual(interface["id"], "world_remote_operator")
        self.assertEqual((interface["interface_type"], interface["role"]), ("world", "admin"))
        self.assertEqual(transport["auth_env_key"], "REMOTE_OPERATOR_AUTH_TOKEN")
        self.assertEqual(interface["interface_driver_config"]["private_config"]["auth_kind_env"], "REMOTE_OPERATOR_AUTH_KIND")
        self.assertNotIn("auth_token_env", transport)
        self.assertEqual(application["category"], "connector")
        self.assertEqual(application["interfaces"], ["world_remote_operator"])
        self.assertIn("artifact-application-remote-operator-context", [item["artifact_id"] for item in application["artifact_bindings"]])
        required = life["processes"][0]["required_files"]
        self.assertIn("registry/interface/world_remote_operator/remote_operator_life.py", required)
        self.assertTrue(all((ROOT.parent.parent.parent / item).is_file() for item in required))
        self.assertIn("installed, available, connected, enabled, or previously paired are separate facts", context["text"])

    def test_operation_catalog_is_complete_and_pairing_requires_one_code(self):
        catalog = load_operation_catalog(ROOT)
        self.assertEqual(len(catalog), 9)
        self.assertEqual(catalog["remote_operator.deliver"]["output_label"], "remote_operator.delivery")
        validate_request(request("remote_operator.pairing.status", {"pairing_code": "pair-1"}), config())
        with self.assertRaisesRegex(ContractError, "exactly one"):
            validate_request(request("remote_operator.pairing.status", {}), config())
        with self.assertRaisesRegex(ContractError, "exactly one"):
            validate_request(
                request(
                    "remote_operator.pairing.status",
                    {"pairing_code": "pair-1", "manual_pairing_code": "manual-1"},
                ),
                config(),
            )

    def test_target_allowlist_is_exact_and_fail_closed(self):
        self.assertEqual(normalize_target("https://chatgpt.com")["websocket_url"], "wss://chatgpt.com/wham/remote/control/server")
        self.assertEqual(normalize_target("http://localhost:8080")["websocket_url"], "ws://localhost:8080/wham/remote/control/server")
        self.assertEqual(normalize_target("http://127.0.0.2:8080")["websocket_url"], "ws://127.0.0.2:8080/wham/remote/control/server")
        normalize_target("https://alpha.chatgpt-staging.com/base")
        for value in (
            "https://chatgpt.com.evil.example",
            "http://chatgpt.com",
            "https://example.com",
            "https://user:password@chatgpt.com",
        ):
            with self.subTest(value=value), self.assertRaises(ContractError):
                normalize_target(value)

    def test_api_key_auth_is_rejected_explicitly(self):
        environment = {
            "REMOTE_OPERATOR_BASE_URL": "https://chatgpt.com/backend-api",
            "REMOTE_OPERATOR_ACCOUNT_ID": "account-1",
            "REMOTE_OPERATOR_SERVER_NAME": "Octopus",
            "REMOTE_OPERATOR_INSTALLATION_ID": "installation-1",
            "REMOTE_OPERATOR_AUTH_TOKEN": "api-key-value",
            "REMOTE_OPERATOR_AUTH_KIND": "api_key",
        }
        with mock.patch.dict(os.environ, environment, clear=False):
            with self.assertRaisesRegex(ContractError, "API keys are unsupported"):
                RemoteHttp(config())
            os.environ["REMOTE_OPERATOR_AUTH_KIND"] = "chatgpt_account"
            self.assertEqual(RemoteHttp(config()).target["base_url"], "https://chatgpt.com/backend-api/")

    def test_remote_messages_are_artifact_native_and_reject_rpc_session_ontology(self):
        message = {"schema_version": 1, "label": "operator.command", "payload": {"text": "status"}}
        self.assertEqual(validate_artifact_message(message), message)
        with self.assertRaisesRegex(ContractError, "forbidden"):
            validate_artifact_message({"schema_version": 1, "label": "thread.start", "payload": {}})
        with self.assertRaisesRegex(ContractError, "forbidden"):
            validate_artifact_message({"schema_version": 1, "label": "operator.command", "payload": {"method": "turn/start"}})
        with self.assertRaisesRegex(ContractError, "forbidden"):
            validate_artifact_message({"schema_version": 1, "label": "operator.command", "payload": {"session_id": "s1"}})

    def test_private_auth_state_is_redacted_from_public_status(self):
        state = default_private_state()
        state["desired_mode"] = "durable"
        state["connection_state"] = "connected"
        state["enrollment"] = {
            "base_url": "https://chatgpt.com/",
            "account_id": "account-secret",
            "server_id": "server-1",
            "environment_id": "environment-1",
            "server_name": "Octopus",
            "installation_id": "installation-secret",
            "remote_control_token": "token-secret",
            "expires_at": "2026-08-28T12:00:00Z",
        }
        status = public_status(state, True, {"websocket": "connected"})
        serialized = json.dumps(status, sort_keys=True)
        self.assertEqual(status["interface_process"], "live")
        self.assertEqual(status["connection_state"], "connected")
        self.assertNotIn("remote_control_token", status)
        self.assertNotIn("installation_id", status)
        self.assertNotIn("account-secret", serialized)
        self.assertNotIn("token-secret", serialized)
        self.assertNotIn("installation-secret", serialized)

    def test_client_catalog_preserves_cursor_and_typed_rows(self):
        result = normalize_clients(
            {
                "items": [
                    {
                        "client_id": "client-1",
                        "display_name": "Phone",
                        "platform": "ios",
                        "last_seen_at": "2026-08-28T10:00:00Z",
                    }
                ],
                "cursor": "next-page",
            }
        )
        self.assertEqual(result["clients"][0]["client_id"], "client-1")
        self.assertEqual(result["next_cursor"], "next-page")

    def test_segmentation_is_bounded_and_client_reassembly_is_ordered(self):
        protocol = config()["protocol"]
        message = {"schema_version": 1, "label": "operator.document", "payload": {"text": "x" * 180000}}
        outgoing = split_server_message("client-1", "stream-1", 7, message, protocol)
        self.assertGreater(len(outgoing), 1)
        self.assertTrue(all(item["type"] == "server_message_chunk" for item in outgoing))
        self.assertTrue(all(len(json.dumps(item, separators=(",", ":")).encode()) <= protocol["segment_max_bytes"] for item in outgoing))
        raw = json.dumps(message, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode()
        chunks = [raw[index : index + protocol["segment_target_bytes"]] for index in range(0, len(raw), protocol["segment_target_bytes"])]
        reassembler = ClientSegmentReassembler(protocol)
        result = None
        for index, chunk in enumerate(chunks):
            result = reassembler.observe(
                {
                    "type": "client_message_chunk",
                    "client_id": "client-1",
                    "stream_id": "stream-1",
                    "seq_id": 8,
                    "segment_id": index,
                    "segment_count": len(chunks),
                    "message_size_bytes": len(raw),
                    "message_chunk_base64": base64.b64encode(chunk).decode(),
                }
            )
        self.assertEqual(result[0], "message")
        self.assertEqual(result[1]["message"], message)
        duplicate = reassembler.observe(
            {
                "type": "client_message_chunk",
                "client_id": "client-1",
                "stream_id": "stream-1",
                "seq_id": 8,
                "segment_id": 0,
                "segment_count": len(chunks),
                "message_size_bytes": len(raw),
                "message_chunk_base64": base64.b64encode(chunks[0]).decode(),
            }
        )
        self.assertEqual(duplicate[0], "duplicate")
        with self.assertRaisesRegex(ContractError, "segment zero"):
            ClientSegmentReassembler(protocol).observe(
                {
                    "type": "client_message_chunk",
                    "client_id": "client-1",
                    "stream_id": "stream-1",
                    "seq_id": 9,
                    "segment_id": 1,
                    "segment_count": 2,
                    "message_size_bytes": 2,
                    "message_chunk_base64": base64.b64encode(b"x").decode(),
                }
            )

    def test_server_close_still_releases_the_private_socket(self):
        connection = WebSocketConnection("ws://localhost", {}, 1, 1024)
        connection.socket = FakeSocket()
        connection.closed = True
        connection.close()
        self.assertTrue(connection.closed)
        self.assertTrue(connection.socket is None)

    def test_interleaved_control_frame_preserves_fragmented_text(self):
        wire = b"\x01\x03hel" + b"\x89\x01x" + b"\x80\x02lo"
        connection = WebSocketConnection("ws://localhost", {}, 1, 1024)
        connection.socket = FrameSocket(wire)
        self.assertEqual(connection.recv_event(), ("ping", b"x"))
        self.assertEqual(connection.recv_event(), ("text", "hello"))
        self.assertEqual(len(connection.socket.sent), 1)


if __name__ == "__main__":
    unittest.main()
