import json
import pathlib
import subprocess
import sys
import unittest


SCRIPT = pathlib.Path(__file__).with_name("verify.py")


def verify(label, params, response_key, response):
    request = {
        "params": params,
        "artifact_label": label,
        "payload_utf8": json.dumps({response_key: response}),
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


class OperatorInputVerifierTests(unittest.TestCase):
    def test_exact_elicitation_and_user_input_identities_pass(self):
        elicitation = verify(
            "operator.elicitation.response",
            {"request_id": "request-7"},
            "operator_elicitation_response",
            {"request_id": "request-7", "answer": {"value": "yes"}},
        )
        user_input = verify(
            "operator.user_input.response",
            {"call_id": "call-9"},
            "operator_user_input_response",
            {"call_id": "call-9", "answers": {"question": ["yes"]}},
        )
        self.assertTrue(elicitation["passed"])
        self.assertTrue(user_input["passed"])

    def test_approval_denial_passes_but_approval_never_does(self):
        denied = verify(
            "operator.approval.response",
            {"request_id": "request-7"},
            "operator_approval_response",
            {"request_id": "request-7", "decision": "decline"},
        )
        approved = verify(
            "operator.approval.response",
            {"request_id": "request-7"},
            "operator_approval_response",
            {"request_id": "request-7", "decision": "accept"},
        )
        self.assertTrue(denied["passed"])
        self.assertFalse(approved["passed"])

    def test_native_session_decision_is_approval_not_input(self):
        params = {"request_id": "request-7", "call_id": "call-9"}
        response = {**params, "decision": "acceptForSession"}
        def decision(value):
            return verify("operator.approval.response", params, "operator_approval_response", value)
        verdict = decision(response)
        self.assertFalse(verdict["passed"])
        self.assertEqual(verdict["details"]["error"], "approved operator response cannot satisfy operator.input.provided")
        mismatched = decision({**response, "call_id": "other"})
        self.assertEqual(mismatched["details"]["error"], "operator response call_id does not match the authored requirement")
        unknown = decision({**response, "decision": "accept-for-session"})
        self.assertFalse(unknown["passed"])
        self.assertEqual(unknown["details"]["error"], "operator decision is not a recognized denial")

    def test_offered_decline_and_cancel_do_not_admit_an_approval(self):
        identity = {"request_id": "request-7", "call_id": "call-9"}
        params = {**identity, "offered_choices": [{"decision": "accept", "persistence": "always"}, {"decision": "decline", "persistence": "once"}]}
        def decision(value):
            return verify("operator.approval.response", params, "operator_approval_response", value)["passed"]
        self.assertTrue(decision({**identity, "decision": "decline", "selected_persistence": "once"}))
        self.assertTrue(decision({**identity, "decision": "cancel"}))
        for changed in (
            {**identity, "decision": "accept", "selected_persistence": "always"},
            {**identity, "decision": "decline", "selected_persistence": "session"},
            {**identity, "decision": "cancel", "selected_persistence": "once"},
            {**identity, "decision": "decline"},
            {**identity, "call_id": "other", "decision": "decline", "selected_persistence": "once"},
        ):
            self.assertFalse(decision(changed))

    def test_missing_or_mismatched_authored_identity_fails_closed(self):
        missing = verify(
            "operator.elicitation.response",
            {},
            "operator_elicitation_response",
            {"request_id": "request-7", "answer": "yes"},
        )
        mismatched = verify(
            "operator.user_input.response",
            {"request_id": "request-7"},
            "operator_user_input_response",
            {"request_id": "request-other", "answers": {"question": ["yes"]}},
        )
        self.assertFalse(missing["passed"])
        self.assertFalse(mismatched["passed"])


if __name__ == "__main__":
    unittest.main()
