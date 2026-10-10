import json
import pathlib
import subprocess
import sys
import unittest


SCRIPT = pathlib.Path(__file__).with_name("verify.py")


def verify(params, response):
    request = {
        "params": params,
        "artifact_label": "operator.approval.response",
        "payload_utf8": json.dumps({"operator_approval_response": response}),
    }
    completed = subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps(request),
        text=True,
        capture_output=True,
        check=True,
        timeout=10,
    )
    return json.loads(completed.stdout)


class OperatorApprovalVerifierTests(unittest.TestCase):
    def test_exact_authored_request_and_call_identities_pass(self):
        for params in (
            {"request_id": "request-7"},
            {"call_id": "call-9"},
            {"request_id": "request-7", "call_id": "call-9"},
        ):
            with self.subTest(params=params):
                verdict = verify(params, {**params, "decision": "accept"})
                self.assertTrue(verdict["passed"])
                for field, value in params.items():
                    self.assertEqual(verdict["details"][field], value)

    def test_native_session_decision_keeps_exact_identity_checks(self):
        params = {"request_id": "request-7", "call_id": "call-9"}
        response = {**params, "decision": "acceptForSession"}
        self.assertTrue(verify(params, response)["passed"])
        for field in params:
            with self.subTest(field=field):
                self.assertFalse(verify(params, {**response, field: "other"})["passed"])
        self.assertFalse(verify(params, {**response, "decision": "accept-for-session"})["passed"])

    def test_offered_choice_is_exact_and_never_promotes_persistence(self):
        identity = {"request_id": "request-7", "call_id": "call-9"}
        choices = [{"decision": "accept", "persistence": "session"}, {"decision": "decline", "persistence": "once"}]
        params = {**identity, "offered_choices": choices}
        response = {**identity, "decision": "accept", "selected_persistence": "session"}
        self.assertTrue(verify(params, response)["passed"])
        for changed in (
            {**response, "selected_persistence": "always"}, {**response, "selected_persistence": None},
            {**identity, "decision": "accept"}, {**response, "call_id": "other"},
            {**response, "decision": "acceptForSession"}, {**response, "approved": True},
            {**identity, "decision": "decline", "selected_persistence": "once"}, {**identity, "decision": "cancel"},
        ):
            self.assertFalse(verify(params, changed)["passed"])
        self.assertFalse(verify({**params, "offered_choices": choices + choices[:1]}, response)["passed"])

    def test_mismatched_identity_fails_with_exact_evidence(self):
        verdict = verify(
            {"request_id": "request-expected"},
            {"request_id": "request-other", "decision": "accept"},
        )
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["identity_field"], "request_id")
        self.assertEqual(verdict["details"]["expected_identity"], "request-expected")
        self.assertEqual(verdict["details"]["actual_identity"], "request-other")

    def test_missing_authored_or_response_identity_fails_closed(self):
        self.assertFalse(verify({}, {"request_id": "request-7", "approved": True})["passed"])
        self.assertFalse(verify({"call_id": "call-9"}, {"approved": True})["passed"])

    def test_denial_never_admits_the_mark(self):
        verdict = verify(
            {"call_id": "call-9"},
            {"call_id": "call-9", "decision": "deny"},
        )
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["decision"], "deny")


if __name__ == "__main__":
    unittest.main()
