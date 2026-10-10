#!/usr/bin/env python3
import json
import sys

HANDLER = "operator_approval"

APPROVED_TOKENS = {"accept", "accept_for_prefix", "accept_for_session", "acceptforsession", "approved"}
DENIED_TOKENS = {"block_host", "cancel", "denied", "deny", "reject", "rejected"}
IDENTITY_FIELDS = ("request_id", "call_id")


def _fail(reason, **extra):
    details = {"handler": HANDLER, "error": reason}
    details.update(extra)
    return {"passed": False, "details": details}


def _pass(**extra):
    details = {"handler": HANDLER}
    details.update(extra)
    return {"passed": True, "details": details}


def _meaningful_permission(value):
    if value is None:
        return False
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        return bool(value.strip())
    if isinstance(value, list):
        return any(_meaningful_permission(item) for item in value)
    if isinstance(value, dict):
        return any(_meaningful_permission(item) for item in value.values())
    return True


def _authored_identities(params):
    if not isinstance(params, dict):
        return None, "operator approval params is not a JSON object"
    identities = {}
    for field in IDENTITY_FIELDS:
        if field not in params:
            continue
        value = params[field]
        if not isinstance(value, str) or not value.strip():
            return None, f"operator approval params field '{field}' is not a non-empty string"
        identities[field] = value.strip()
    if not identities:
        return None, "operator approval params requires request_id or call_id"
    return identities, None


def _resolve_decision(body):
    if not isinstance(body, dict):
        return None, None, "response payload is not a JSON object"

    response = body.get("operator_approval_response", body)
    if not isinstance(response, dict):
        return None, None, "operator approval response is not a JSON object"

    if isinstance(response.get("approved"), bool):
        return ("approved" if response["approved"] else "denied"), response, None

    decision = response.get("decision")
    if isinstance(decision, bool):
        return ("approved" if decision else "denied"), response, None
    if isinstance(decision, str) and decision.strip():
        return decision.strip().lower(), response, None
    if isinstance(decision, dict) and isinstance(decision.get("permissions"), dict):
        granted = _meaningful_permission(decision["permissions"])
        return ("approved" if granted else "denied"), response, None

    return None, response, "response payload carries no recognizable approval decision"


def _permission_choice_error(params, body, label):
    if "offered_choices" not in params:
        return None
    choices = params["offered_choices"]
    if not isinstance(choices, list) or not 1 <= len(choices) <= 6:
        return "authored permission choices must be a non-empty bounded array"
    pairs = set()
    for choice in choices:
        if not isinstance(choice, dict) or set(choice) != {"decision", "persistence"}:
            return "authored permission choice has invalid fields"
        decision, persistence = choice["decision"], choice["persistence"]
        if decision not in ("accept", "decline") or persistence not in ("once", "session", "always"):
            return "authored permission choice is not canonical"
        pair = (decision, persistence)
        if pair in pairs:
            return "authored permission choices repeat a pair"
        pairs.add(pair)
    if label != "operator.approval.response" or not isinstance(body, dict):
        return "permission choices require an ordinary operator approval response"
    response = body.get("operator_approval_response")
    if not isinstance(response, dict) or not {"request_id", "call_id", "decision"} <= set(response) or set(response) - {"request_id", "call_id", "decision", "selected_persistence"}:
        return "permission response has invalid fields"
    for field in IDENTITY_FIELDS:
        expected = params.get(field)
        if not isinstance(expected, str) or not expected or expected.strip() != expected or response.get(field) != expected:
            return "permission response differs from its exact authored request/call"
    if response["decision"] == "cancel":
        return None if "selected_persistence" not in response else "cancellation cannot select persistence"
    decision, persistence = response["decision"], response.get("selected_persistence")
    if not isinstance(decision, str) or not isinstance(persistence, str) or (decision, persistence) not in pairs:
        return "permission response selected a choice absent from its immutable offer"
    return None


def main():
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        print(json.dumps(_fail(f"admission request is not valid JSON: {exc}")))
        return

    if not isinstance(request, dict):
        print(json.dumps(_fail("admission request is not a JSON object")))
        return

    expected_identities, identity_error = _authored_identities(request.get("params"))
    if identity_error is not None:
        print(json.dumps(_fail(identity_error)))
        return

    label = request.get("artifact_label")
    if label != "operator.approval.response":
        print(json.dumps(_fail(
            "artifact label is not operator.approval.response",
            artifact_label=label,
        )))
        return

    payload_text = request.get("payload_utf8")
    if not isinstance(payload_text, str) or not payload_text.strip():
        payload_path = request.get("payload_path")
        if not isinstance(payload_path, str) or not payload_path:
            print(json.dumps(_fail("operator response payload is unavailable")))
            return
        try:
            with open(payload_path, "r", encoding="utf-8") as handle:
                payload_text = handle.read()
        except Exception as exc:
            print(json.dumps(_fail(f"operator response payload unreadable: {exc}")))
            return

    try:
        body = json.loads(payload_text)
    except Exception as exc:
        print(json.dumps(_fail(f"operator response is not valid JSON: {exc}")))
        return

    choice_error = _permission_choice_error(request["params"], body, label)
    if choice_error is not None:
        print(json.dumps(_fail(choice_error)))
        return

    decision, response, err = _resolve_decision(body)
    if err is not None:
        print(json.dumps(_fail(err)))
        return
    admitted_identities = {}
    for field, expected in expected_identities.items():
        actual = response.get(field)
        if not isinstance(actual, str) or not actual.strip():
            print(json.dumps(_fail(
                f"operator approval response requires a non-empty {field}",
                identity_field=field,
            )))
            return
        actual = actual.strip()
        if actual != expected:
            print(json.dumps(_fail(
                f"operator approval response {field} does not match the authored requirement",
                identity_field=field,
                expected_identity=expected,
                actual_identity=actual,
            )))
            return
        admitted_identities[field] = actual

    if decision in APPROVED_TOKENS:
        print(json.dumps(_pass(decision=decision, **admitted_identities)))
        return

    if decision in DENIED_TOKENS:
        print(json.dumps(_fail("operator denied the request", decision=decision)))
        return

    print(json.dumps(_fail("operator decision is not a recognized approval", decision=decision)))


if __name__ == "__main__":
    main()
