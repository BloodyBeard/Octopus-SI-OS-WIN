import base64
import copy
import hashlib
import importlib
import json
import os
import smtplib
import sys
import tempfile
import unittest
import urllib.parse
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parent
sys.path.insert(0, str(ROOT))

from email_contract import ContractError
from email_contract import build_outbound_message
from email_contract import parse_inbound_message
from email_contract import validate_config
from email_transport import EmailTransport
from email_transport import TransportError
from email_transport import read_rooted_regular


def config():
    with open(ROOT / "config.json", "r", encoding="utf-8") as handle:
        return validate_config(json.load(handle))


RAW = (
    b"From: Alice <alice@example.com>\r\n"
    b"To: Octopus <octopus@example.com>\r\n"
    b"Cc: Bob <bob@example.com>\r\n"
    b"Subject: Re: Roadmap\r\n"
    b"Message-ID: <m1@example.com>\r\n"
    b"Date: Fri, 18 Jul 2026 10:00:00 +0000\r\n"
    b"MIME-Version: 1.0\r\n"
    b"Content-Type: multipart/mixed; boundary=mixed\r\n\r\n"
    b"--mixed\r\n"
    b"Content-Type: multipart/alternative; boundary=alt\r\n\r\n"
    b"--alt\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nhello\nworld\r\n"
    b"--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<p>hello<br>world</p>\r\n"
    b"--alt--\r\n"
    b"--mixed\r\nContent-Type: application/octet-stream\r\n"
    b"Content-Disposition: attachment; filename=sample.bin\r\n"
    b"Content-Transfer-Encoding: base64\r\n\r\naGVsbG8=\r\n"
    b"--mixed--\r\n"
)


class ContractTests(unittest.TestCase):
    def test_root_life_executes_only_the_package_runtime_subtree(self):
        process = json.loads((ROOT / "life.json").read_text("utf-8"))["processes"][0]
        self.assertEqual(process["private_var_directories"], ["content-hash-python-runtime"])
        self.assertEqual(process["authority"]["executable_env_paths"], [])
        self.assertEqual(process["authority"]["executable_env_subpaths"], [{"env": "OCTOPUS_VAR_DIR", "relative_path": "content-hash-python-runtime"}])

    def test_operator_authorization_interface_is_an_independent_admin_root(self):
        package = ROOT.parent / "world_email_operator"
        interface = json.loads((package / "interface.json").read_text("utf-8"))
        activation = json.loads((package / "activation.json").read_text("utf-8"))
        life = json.loads((package / "life.json").read_text("utf-8"))
        self.assertEqual(interface["id"], "world_email_operator")
        self.assertEqual((interface["interface_type"], interface["role"], interface["direction"]), ("world", "admin", "bidirectional"))
        self.assertEqual(interface["interface_driver_ref"], "app_server")
        self.assertNotIn("reference", interface)
        self.assertEqual(activation["interface"], interface["id"])
        self.assertEqual(activation["activations"][0]["config"]["listen_env"], "OCTOPUS_EMAIL_OPERATOR_LISTEN")
        self.assertEqual(life["processes"][0]["bin"], "target/debug/octopus-app-server-activation-life")
        self.assertNotIn("run", activation["activations"][0])
        self.assertNotIn("arrival", activation["activations"][0])

    def test_plaintext_requires_explicit_scope(self):
        value = config()
        value = copy.deepcopy(value)
        value["account"]["imap"]["tls"] = "plain"
        with self.assertRaises(ContractError):
            validate_config(value)

    def test_declared_actions_must_exist_in_the_closed_execution_catalog(self):
        value = copy.deepcopy(config())
        value["outbound"]["allowed_actions"].append("message.unsupported")
        with self.assertRaisesRegex(ContractError, "unsupported actions"):
            validate_config(value)
        value = copy.deepcopy(config())
        value["authorization_control"]["allowed_actions"] = ["authorization.unsupported"]
        with self.assertRaisesRegex(ContractError, "unsupported actions"):
            validate_config(value)
        value = copy.deepcopy(config())
        value["outbound"]["allowed_actions"].remove("health.get")
        validate_config(value)

    def test_parse_and_reply_all_preserve_thread_and_mime(self):
        value = config()
        with tempfile.TemporaryDirectory() as directory:
            parsed = parse_inbound_message(
                RAW,
                value,
                directory,
                directory,
                "octopus@example.com",
                "INBOX",
                91,
                7,
                ["\\Seen"],
                "2026-07-18T10:00:00+00:00",
            )
            self.assertEqual(parsed["body"]["text"], "hello\nworld")
            self.assertEqual(parsed["attachments"][0]["sha256"], hashlib.sha256(b"hello").hexdigest())
            message, recipients, attachments = build_outbound_message(
                "message.reply_all",
                {
                    "text": "Thanks\n\nDone",
                    "attachments": [
                        {
                            "inline_base64": base64.b64encode(b"reply-file").decode("ascii"),
                            "filename": "reply.txt",
                            "media_type": "text/plain",
                        }
                    ],
                },
                "octopus@example.com",
                "Octopus",
                "source-artifact",
                value["outbound"],
                value["composition"],
                lambda item: (base64.b64decode(item["inline_base64"]), {"carrier": "inline"}),
                parsed,
            )
        self.assertEqual(recipients, ["alice@example.com", "bob@example.com"])
        self.assertEqual(message["In-Reply-To"], "<m1@example.com>")
        self.assertIn("On 2026-07-18T10:00:00+00:00, Alice wrote:", message.get_body(preferencelist=("plain",)).get_content())
        self.assertEqual(attachments[0]["filename"], "reply.txt")

    def test_forward_can_carry_original_attachments(self):
        value = config()
        with tempfile.TemporaryDirectory() as directory:
            parsed = parse_inbound_message(RAW, value, directory, directory, "octopus@example.com", "INBOX", 1, 1, [], None)
            message, recipients, attachments = build_outbound_message(
                "message.forward",
                {"to": ["carol@example.com"], "text": "For review", "include_attachments": True},
                "octopus@example.com",
                "Octopus",
                "forward-source",
                value["outbound"],
                value["composition"],
                lambda item: (Path(item["path"]).read_bytes(), {"carrier": "path"}),
                parsed,
            )
        self.assertEqual(recipients, ["carol@example.com"])
        self.assertEqual(attachments[0]["sha256"], hashlib.sha256(b"hello").hexdigest())
        self.assertIn("Forwarded message", message.get_body(preferencelist=("plain",)).get_content())


class TransportTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.value = config()
        self.previous = os.environ.get("EMAIL_ADDRESS")
        os.environ["EMAIL_ADDRESS"] = "octopus@example.com"
        self.transport = EmailTransport(self.value, self.directory.name, self.directory.name)

    def tearDown(self):
        if self.previous is None:
            os.environ.pop("EMAIL_ADDRESS", None)
        else:
            os.environ["EMAIL_ADDRESS"] = self.previous
        self.directory.cleanup()

    def test_attachment_claims_are_verified(self):
        encoded = base64.b64encode(b"hello").decode("ascii")
        data, evidence = self.transport._resolve_attachment(
            {
                "inline_base64": encoded,
                "filename": "hello.txt",
                "media_type": "text/plain",
                "sha256": hashlib.sha256(b"hello").hexdigest(),
                "size": 5,
            }
        )
        self.assertEqual(data, b"hello")
        self.assertEqual(evidence["size"], 5)
        with self.assertRaises(ContractError):
            self.transport._resolve_attachment(
                {
                    "inline_base64": encoded,
                    "filename": "hello.txt",
                    "media_type": "text/plain",
                    "sha256": "0" * 64,
                }
            )
        path = Path(self.directory.name) / "oversized.bin"
        path.write_bytes(b"123456")
        with self.assertRaises(ContractError):
            read_rooted_regular(self.directory.name, str(path), 5)

    def test_mailbox_and_recipient_scopes_fail_closed(self):
        self.transport.scopes["read_mailboxes"] = ["INBOX"]
        with self.assertRaises(ContractError):
            self.transport._require_mailbox_scope("Archive", "read")
        self.transport.scopes["deny_unmatched_recipients"] = True
        with self.assertRaises(ContractError):
            self.transport._require_recipients(["unknown@example.net"])
        self.transport._require_recipients(["unknown@example.net"], ("unknown@example.net",))

    def test_plaintext_transport_is_lexical_loopback_only_at_preflight_and_connect(self):
        value = copy.deepcopy(self.value)
        for name in ("imap", "smtp"):
            value["account"][name]["tls"] = "plain"
            value["account"][name]["allow_plaintext"] = True
        transport = EmailTransport(value, self.directory.name, self.directory.name)
        environment = {
            "EMAIL_PASSWORD": "mail-password",
            "EMAIL_IMAP_HOST": "mail.example.net",
            "EMAIL_SMTP_HOST": "smtp.example.net",
        }
        with mock.patch.dict(os.environ, environment, clear=False):
            with self.assertRaises(ContractError):
                transport.preflight_environment()
            os.environ["EMAIL_IMAP_HOST"] = "localhost"
            with self.assertRaises(ContractError):
                transport.preflight_environment()
            os.environ["EMAIL_IMAP_HOST"] = "mail.example.net"
            with self.assertRaises(ContractError):
                with transport.imap():
                    pass
            with self.assertRaises(ContractError):
                with transport.smtp():
                    pass
            os.environ["EMAIL_IMAP_HOST"] = "127.0.0.1"
            os.environ["EMAIL_SMTP_HOST"] = "::1"
            preflight = transport.preflight_environment()
            self.assertEqual(preflight["imap_host"], "127.0.0.1")
            self.assertEqual(preflight["smtp_host"], "::1")

    def test_transport_errors_remove_password_oauth_and_authenticated_server_responses(self):
        password = "mail-password+/with?symbols"
        token = "oauth-access-token+/with?symbols"
        with mock.patch.dict(
            os.environ,
            {
                "EMAIL_PASSWORD": password,
                "EMAIL_OAUTH_ACCESS_TOKEN": token,
            },
            clear=False,
        ):
            xoauth = base64.b64encode(
                f"user=octopus@example.com\x01auth=Bearer {token}\x01\x01".encode("utf-8")
            ).decode("ascii")
            plain = base64.b64encode(f"\x00octopus@example.com\x00{password}".encode("utf-8")).decode("ascii")
            variants = {
                password,
                token,
                urllib.parse.quote(password, safe=""),
                urllib.parse.quote_plus(token, safe=""),
                base64.b64encode(password.encode("utf-8")).decode("ascii"),
                base64.urlsafe_b64encode(token.encode("utf-8")).decode("ascii").rstrip("="),
                xoauth,
                f"XOAUTH2 {xoauth}",
                urllib.parse.quote(f"AUTH XOAUTH2 {xoauth}", safe=""),
                plain,
            }
            source = smtplib.SMTPAuthenticationError(535, f"reflected {token}".encode("utf-8"))
            error = TransportError(
                "authentication failed " + " ".join(sorted(variants)),
                False,
                {
                    "response": "server reflected " + token,
                    "smtp_error": f"535 {xoauth}",
                    "nested": {"password": password, "url": urllib.parse.quote(password, safe="")},
                },
            )
            error.__cause__ = source
            error.__context__ = source
            sanitized = self.transport._sanitize_transport_error(error)
            surface = str(sanitized) + json.dumps(sanitized.details, sort_keys=True)
            for variant in variants:
                self.assertNotIn(variant, surface)
            self.assertNotIn("server reflected", surface)
            self.assertIn("response_bytes", sanitized.details)
            self.assertIn("response_sha256", sanitized.details)
            self.assertIn("[REDACTED_EMAIL_CREDENTIAL]", surface)
            self.assertIsNone(sanitized.__cause__)
            self.assertIsNone(sanitized.__context__)
            self.transport._retire_source_exception(source)
            self.assertNotIn(token, repr(source))
            self.assertEqual(source.smtp_error, b"")


class LifeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.directory = tempfile.TemporaryDirectory()
        cls.previous = {}
        values = {
            "OCTOPUS_INTERFACE_ID": "world_email",
            "OCTOPUS_INTERFACE_DIR": str(ROOT),
            "OCTOPUS_VAR_DIR": cls.directory.name,
            "OCTOPUS_MOVEMENTS_JSONL": os.path.join(cls.directory.name, "movements.jsonl"),
            "OCTOPUS_BLOBS_DIR": os.path.join(cls.directory.name, "blobs"),
            "OCTOPUS_LEDGER_INDEX_SQLITE": os.path.join(cls.directory.name, "ledger.sqlite"),
        }
        os.makedirs(values["OCTOPUS_BLOBS_DIR"])
        for key, value in values.items():
            cls.previous[key] = os.environ.get(key)
            os.environ[key] = value
        cls.life = importlib.import_module("email_life")

    @classmethod
    def tearDownClass(cls):
        for key, value in cls.previous.items():
            if value is None:
                os.environ.pop(key, None)
            else:
                os.environ[key] = value
        cls.directory.cleanup()

    def test_output_projection_requires_explicit_email_coordinates(self):
        payload = self.life.normalize_outbound_payload(
            "interface.artifact.output",
            {"text": "Reply", "reply_context": {"mailbox": "INBOX", "uid": 4}},
        )
        self.assertEqual(payload["action"], "message.reply")
        self.assertEqual(payload["uid"], 4)
        with self.assertRaises(ContractError):
            self.life.normalize_outbound_payload("interface.artifact.output", {"text": "No target"})

    def test_activation_is_explicit_and_owned(self):
        activation = self.life.load_activation()
        self.assertEqual(activation["activation_driver_ref"], "app_server_transport")
        self.assertNotIn("run", activation)
        self.assertNotIn("arrival", activation)

    @unittest.skipUnless(hasattr(os, "O_NOFOLLOW"), "ledger hardening requires O_NOFOLLOW")
    def test_ledger_fallback_commits_only_complete_admitted_rows(self):
        def row(seq, padding=""):
            return json.dumps(
                {
                    "seq": seq,
                    "kind": "InterfaceOutbound",
                    "from": {"Interface": "world_provider_test"},
                    "to": {"Interface": self.life.INTERFACE_ID},
                    "invocation_metadata": {
                        "runtime": {
                            "run": f"run_{seq}",
                            "ee": "ee_test",
                            "worker": "system_worker_test",
                            "checkpoint": f"cp_{seq}",
                            "interface": self.life.INTERFACE_ID,
                            "epoch": 0,
                        }
                    },
                    "artifact": {"content": f"{seq % 256:02x}" * 32},
                    "padding": padding,
                },
                separators=(",", ":"),
            ).encode("utf-8") + b"\n"

        original_path = self.life.MOVEMENTS_JSONL
        original_cache = (
            self.life._LEDGER_FALLBACK_OFFSET,
            self.life._LEDGER_FALLBACK_SIZE,
            self.life._LEDGER_FALLBACK_IDENTITY,
        )
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "movements.jsonl"
            try:
                self.life.MOVEMENTS_JSONL = str(path)
                self.life._LEDGER_FALLBACK_OFFSET = 0
                self.life._LEDGER_FALLBACK_SIZE = 0
                self.life._LEDGER_FALLBACK_IDENTITY = None
                complete = row(7)
                path.write_bytes(complete[:-1])
                self.assertEqual(self.life.read_ledger(7, 8, 4096), [])
                self.assertEqual(
                    (
                        self.life._LEDGER_FALLBACK_OFFSET,
                        self.life._LEDGER_FALLBACK_SIZE,
                        self.life._LEDGER_FALLBACK_IDENTITY,
                    ),
                    (0, 0, None),
                )
                with path.open("ab") as handle:
                    handle.write(b"\n")
                self.assertEqual(
                    self.life.read_ledger(7, 8, 4096),
                    [(7, "07" * 32, "run_7", "cp_7")],
                )
                self.assertEqual(
                    self.life.read_ledger(7, 8, 4096),
                    [(7, "07" * 32, "run_7", "cp_7")],
                )
                admitted = (
                    self.life._LEDGER_FALLBACK_OFFSET,
                    self.life._LEDGER_FALLBACK_SIZE,
                    self.life._LEDGER_FALLBACK_IDENTITY,
                )
                path.write_bytes(b"{malformed]\n")
                with self.assertRaises(json.JSONDecodeError):
                    self.life.read_ledger(8, 8, 4096)
                self.assertEqual(
                    (
                        self.life._LEDGER_FALLBACK_OFFSET,
                        self.life._LEDGER_FALLBACK_SIZE,
                        self.life._LEDGER_FALLBACK_IDENTITY,
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
                                "to": {"Interface": self.life.INTERFACE_ID},
                                "invocation_metadata": metadata,
                            }
                        ).encode("utf-8")
                        + b"\n"
                    )
                    with self.assertRaises(ContractError):
                        self.life.read_ledger(8, 8, 4096)
                    self.assertEqual(
                        (
                            self.life._LEDGER_FALLBACK_OFFSET,
                            self.life._LEDGER_FALLBACK_SIZE,
                            self.life._LEDGER_FALLBACK_IDENTITY,
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
                    "to": {"Interface": self.life.INTERFACE_ID},
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
                    self.life.read_ledger(8, 8, 4096),
                    [(10, "0a" * 32, "run_10", "cp_10")],
                )
                replacement = path.with_suffix(".next")
                ignored = json.dumps({"seq": 10, "kind": "Mark"}).encode("utf-8") + b"\n"
                replacement.write_bytes(row(6, "x" * 256) + row(8) + ignored)
                os.replace(replacement, path)
                self.assertEqual(
                    self.life.read_ledger(8, 8, 4096),
                    [(8, "08" * 32, "run_8", "cp_8")],
                )
                self.assertEqual(
                    self.life.read_ledger(8, 8, 4096),
                    [(8, "08" * 32, "run_8", "cp_8")],
                )
                rotated_identity = self.life._LEDGER_FALLBACK_IDENTITY
                path.write_bytes(row(7) + row(9))
                self.assertEqual(
                    self.life.read_ledger(9, 8, 4096),
                    [(9, "09" * 32, "run_9", "cp_9")],
                )
                self.assertEqual(
                    self.life.read_ledger(9, 8, 4096),
                    [(9, "09" * 32, "run_9", "cp_9")],
                )
                self.assertEqual(self.life._LEDGER_FALLBACK_IDENTITY, rotated_identity)
                admitted = (
                    self.life._LEDGER_FALLBACK_OFFSET,
                    self.life._LEDGER_FALLBACK_SIZE,
                    self.life._LEDGER_FALLBACK_IDENTITY,
                )
                path.write_bytes(b"x" * 33 + b"\n")
                with self.assertRaisesRegex(RuntimeError, "line bound"):
                    self.life.read_ledger(10, 8, 32)
                self.assertEqual(
                    (
                        self.life._LEDGER_FALLBACK_OFFSET,
                        self.life._LEDGER_FALLBACK_SIZE,
                        self.life._LEDGER_FALLBACK_IDENTITY,
                    ),
                    admitted,
                )
                outside = Path(directory) / "outside.jsonl"
                outside.write_bytes(row(10))
                path.unlink()
                path.symlink_to(outside)
                with self.assertRaises(OSError):
                    self.life.read_ledger(10, 8, 4096)
            finally:
                self.life.MOVEMENTS_JSONL = original_path
                (
                    self.life._LEDGER_FALLBACK_OFFSET,
                    self.life._LEDGER_FALLBACK_SIZE,
                    self.life._LEDGER_FALLBACK_IDENTITY,
                ) = original_cache

    def test_authorization_requires_operator_approval_of_private_intent(self):
        value = copy.deepcopy(config())
        value["authz"]["pairing"]["enabled"] = True
        address, entry = self.life.pairing_entry(value, "pending@example.net")
        with self.assertRaises(ContractError):
            self.life.authorization_action(
                value,
                "authorization.request",
                {"requested_action": "authorization.approve", "arguments": {"address": address, "code": "wrong-code"}},
                "1" * 64,
            )
        with mock.patch.object(self.life, "hand_json", return_value=77) as hand:
            requested = self.life.authorization_action(
                value,
                "authorization.request",
                {"requested_action": "authorization.approve", "arguments": {"address": address, "code": entry["code"]}},
                "2" * 64,
            )
        operator_payload = hand.call_args.args[2]
        self.assertNotIn(entry["code"], json.dumps(operator_payload))
        self.assertEqual(requested["status"], "approval_requested")
        approved = self.life.authorization_action(
            value,
            "authorization.apply",
            {"request_id": requested["request_id"], "decision": "accept"},
            "3" * 64,
        )
        self.assertEqual(approved, {"status": "approved", "address": address})

    def test_authorization_codes_never_leave_the_private_state(self):
        value = copy.deepcopy(config())
        value["authz"]["pairing"]["enabled"] = True
        address, entry = self.life.pairing_entry(value, "listed@example.net")
        result = self.life.authorization_action(value, "authorization.list", {}, "4" * 64)
        pending = next(item for item in result["pending"] if item["address"] == address)
        self.assertNotIn("code", pending)
        self.assertNotIn(entry["code"], json.dumps(result))

    def test_authorization_mutations_require_the_verified_operator_checkpoint(self):
        value = config()
        body = json.dumps(
            {
                "operator_approval_response": {
                    "request_id": "email-auth-" + "a" * 40,
                    "decision": "accept",
                }
            }
        ).encode("utf-8")
        with self.assertRaises(ContractError):
            self.life.decode_outbound_action(value, "email.action", body, "cp_email_gateway_output")
        with self.assertRaises(ContractError):
            self.life.decode_outbound_action(
                value,
                "email.action",
                json.dumps({"action": "authorization.approve", "code": "model-code"}).encode("utf-8"),
                "cp_email_gateway_output",
            )
        action, payload = self.life.decode_outbound_action(
            value,
            "operator.approval.response",
            body,
            value["authorization_control"]["checkpoint"],
        )
        self.assertEqual(action, "authorization.apply")
        self.assertEqual(payload["request_id"], "email-auth-" + "a" * 40)

    def test_oversized_intake_becomes_admissible_failure_evidence(self):
        transport = type("Transport", (), {"account_id": "test-account"})()
        pending = self.life.prepare_pending(
            config(),
            transport,
            "INBOX",
            7,
            {
                "uid": 11,
                "raw": b"partial-message",
                "boundary_failure": {
                    "error": "message exceeds limit",
                    "declared_size": 1000,
                    "captured_size": 15,
                    "maximum": 14,
                },
            },
        )
        self.assertEqual(pending["label"], "email.intake.failure")
        self.assertEqual(pending["payload"]["failure_class"], "message_size_rejected")
        self.assertTrue(pending["payload"]["evidence"]["truncated"])
        self.assertTrue(os.path.isfile(pending["payload"]["evidence"]["path"]))


if __name__ == "__main__":
    unittest.main()
