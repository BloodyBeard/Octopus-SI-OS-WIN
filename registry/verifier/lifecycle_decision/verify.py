#!/usr/bin/env python3
import json
import re
import sys

HANDLER = "lifecycle_decision"
EVENT_DECISIONS = {
    "pre_tool_use": "allow",
    "permission_request": "allow",
    "post_tool_use": "continue",
    "pre_compact": "continue",
    "post_compact": "continue",
    "session_start": "continue",
    "user_prompt_submit": "continue",
    "subagent_stop": "continue",
    "stop": "continue",
}
REACTIVE_EVENTS = {"session_end", "subagent_start", "interrupt"}
CONTEXT_EVENTS = {
    "pre_tool_use",
    "post_tool_use",
    "session_start",
    "user_prompt_submit",
}
HANDLER_TYPES = {"command", "mcp_tool", "prompt", "agent"}
DECISIONS = {"allow", "continue", "deny", "block", "stop"}
PARAM_KEYS = {
    "expected_event",
    "expected_decision",
    "expected_handler_type",
    "connection_id",
    "connection_artifact_id",
    "connection_artifact_content",
    "event_id",
    "emitter_run",
    "owner_run",
    "handler_id",
    "mode",
    "subject_artifact_id",
    "subject_artifact_content",
    "event_artifact",
    "timeout_ms",
}
PAYLOAD_KEYS = {
    "label",
    "schema_version",
    "connection_id",
    "connection_artifact",
    "event_id",
    "emitter_run",
    "owner_run",
    "handler_id",
    "event",
    "mode",
    "decision",
    "handler_type",
    "subject_artifact",
    "event_artifact",
    "reason",
    "additional_context_artifacts",
    "replacement_artifact",
}
HEX_256 = re.compile(r"^[0-9a-f]{64}$")


def _fail(reason, **extra):
    details = {"handler": HANDLER, "error": reason}
    details.update(extra)
    return {"passed": False, "details": details}


def _pass(**extra):
    details = {"handler": HANDLER}
    details.update(extra)
    return {"passed": True, "details": details}


def _load_payload(request):
    payload_text = request.get("payload_utf8")
    if not isinstance(payload_text, str) or not payload_text.strip():
        payload_path = request.get("payload_path")
        if not isinstance(payload_path, str) or not payload_path:
            return None, "lifecycle decision payload is unavailable"
        try:
            with open(payload_path, "r", encoding="utf-8") as handle:
                payload_text = handle.read()
        except Exception as exc:
            return None, f"lifecycle decision payload unreadable: {exc}"
    try:
        body = json.loads(payload_text)
    except Exception as exc:
        return None, f"lifecycle decision payload is not valid JSON: {exc}"
    if not isinstance(body, dict):
        return None, "lifecycle decision payload is not a JSON object"
    unknown = sorted(set(body) - PAYLOAD_KEYS)
    if unknown:
        return None, f"lifecycle decision payload has unknown fields: {', '.join(unknown)}"
    return body, None


def _string(value, field):
    if not isinstance(value, str) or not value.strip() or value != value.strip():
        return None, f"{field} must be a non-empty trimmed string"
    return value, None


def _artifact_ref(value, field):
    if not isinstance(value, dict):
        return None, f"{field} must be an artifact ref object"
    if set(value) != {"id", "content"}:
        return None, f"{field} must contain exactly id and content"
    artifact_id, error = _string(value.get("id"), f"{field}.id")
    if error is not None:
        return None, error
    content = value.get("content")
    if not isinstance(content, str) or HEX_256.fullmatch(content) is None:
        return None, f"{field}.content must be a lowercase sha256 hex digest"
    return {"id": artifact_id, "content": content}, None


def _params(request):
    params = request.get("params")
    if not isinstance(params, dict):
        return None, "lifecycle decision params must be a JSON object"
    unknown = sorted(set(params) - PARAM_KEYS)
    if unknown:
        return None, f"lifecycle decision params have unknown fields: {', '.join(unknown)}"
    required = {
        "expected_event",
        "expected_decision",
        "expected_handler_type",
        "connection_id",
        "connection_artifact_id",
        "connection_artifact_content",
        "event_id",
        "emitter_run",
        "owner_run",
        "handler_id",
        "mode",
        "subject_artifact_id",
        "subject_artifact_content",
        "event_artifact",
    }
    missing = sorted(required - set(params))
    if missing:
        return None, f"lifecycle decision params miss fields: {', '.join(missing)}"
    normalized = {}
    for field in required - {"event_artifact"}:
        normalized[field], error = _string(params.get(field), f"params.{field}")
        if error is not None:
            return None, error
    event_artifact = params.get("event_artifact")
    if not isinstance(event_artifact, dict):
        return None, "params.event_artifact must be a JSON object"
    normalized["event_artifact"] = event_artifact
    return normalized, None


def _validate_context(body, event):
    context = body.get("additional_context_artifacts")
    if context is None:
        return None
    if event not in CONTEXT_EVENTS:
        return f"event {event} does not admit additional context artifacts on a blocking decision"
    if not isinstance(context, list) or not context:
        return "additional_context_artifacts must be a non-empty array"
    seen = set()
    for index, value in enumerate(context):
        artifact, error = _artifact_ref(value, f"additional_context_artifacts[{index}]")
        if error is not None:
            return error
        key = (artifact["id"], artifact["content"])
        if key in seen:
            return "additional_context_artifacts contains a duplicate artifact ref"
        seen.add(key)
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
    if request.get("artifact_label") != "lifecycle.decision":
        print(json.dumps(_fail(
            "artifact label is not lifecycle.decision",
            artifact_label=request.get("artifact_label"),
        )))
        return
    params, error = _params(request)
    if error is not None:
        print(json.dumps(_fail(error)))
        return
    event = params["expected_event"]
    if event in REACTIVE_EVENTS:
        print(json.dumps(_fail(
            "reactive lifecycle events cannot close a blocking decision mark",
            event=event,
        )))
        return
    positive = EVENT_DECISIONS.get(event)
    if positive is None:
        print(json.dumps(_fail("unknown lifecycle event", event=event)))
        return
    if params["expected_decision"] != positive:
        print(json.dumps(_fail(
            "authored expected decision conflicts with the lifecycle event contract",
            event=event,
            expected=positive,
            authored=params["expected_decision"],
        )))
        return
    if params["expected_handler_type"] not in HANDLER_TYPES:
        print(json.dumps(_fail(
            "authored handler type is unknown",
            handler_type=params["expected_handler_type"],
        )))
        return
    if HEX_256.fullmatch(params["subject_artifact_content"]) is None:
        print(json.dumps(_fail("authored subject artifact content is not a lowercase sha256 digest")))
        return
    if HEX_256.fullmatch(params["connection_artifact_content"]) is None:
        print(json.dumps(_fail("authored connection artifact content is not a lowercase sha256 digest")))
        return
    if re.fullmatch(r"lifecycle:[0-9a-f]{64}", params["event_id"]) is None:
        print(json.dumps(_fail("authored event_id is not a lifecycle content identity")))
        return
    if params["mode"] != "sync":
        print(json.dumps(_fail("authored lifecycle decision mode is not sync")))
        return
    body, error = _load_payload(request)
    if error is not None:
        print(json.dumps(_fail(error)))
        return
    if body.get("label") != "lifecycle.decision":
        print(json.dumps(_fail("payload label conflicts with lifecycle.decision")))
        return
    if body.get("schema_version") != 1:
        print(json.dumps(_fail("lifecycle decision schema_version must be 1")))
        return
    for field in (
        "connection_id",
        "event_id",
        "emitter_run",
        "owner_run",
        "handler_id",
        "mode",
    ):
        if body.get(field) != params[field]:
            print(json.dumps(_fail(f"decision {field} does not match the authored requirement")))
            return
    connection_artifact, error = _artifact_ref(
        body.get("connection_artifact"),
        "connection_artifact",
    )
    if error is not None:
        print(json.dumps(_fail(error)))
        return
    if (
        connection_artifact["id"] != params["connection_artifact_id"]
        or connection_artifact["content"] != params["connection_artifact_content"]
    ):
        print(json.dumps(_fail("decision connection artifact does not match the authored requirement")))
        return
    if body.get("event") != event:
        print(json.dumps(_fail("decision event does not match the authored requirement")))
        return
    if body.get("event_artifact") != params["event_artifact"]:
        print(json.dumps(_fail("decision event artifact does not match the authored requirement")))
        return
    handler_type = body.get("handler_type")
    if handler_type != params["expected_handler_type"] or handler_type not in HANDLER_TYPES:
        print(json.dumps(_fail("decision handler type does not match the authored requirement")))
        return
    subject, error = _artifact_ref(body.get("subject_artifact"), "subject_artifact")
    if error is not None:
        print(json.dumps(_fail(error)))
        return
    if subject["id"] != params["subject_artifact_id"] or subject["content"] != params["subject_artifact_content"]:
        print(json.dumps(_fail("decision subject artifact does not match the authored requirement")))
        return
    decision = body.get("decision")
    if decision not in DECISIONS:
        print(json.dumps(_fail("decision is not recognized", decision=decision)))
        return
    reason = body.get("reason")
    if reason is not None:
        _, error = _string(reason, "reason")
        if error is not None:
            print(json.dumps(_fail(error)))
            return
    if decision != positive:
        print(json.dumps(_fail(
            "lifecycle decision did not admit the next edge",
            event=event,
            decision=decision,
            decision_reason=reason,
        )))
        return
    error = _validate_context(body, event)
    if error is not None:
        print(json.dumps(_fail(error)))
        return
    replacement = body.get("replacement_artifact")
    if replacement is not None:
        if event != "pre_tool_use":
            print(json.dumps(_fail("only pre_tool_use may carry a replacement artifact")))
            return
        _, error = _artifact_ref(replacement, "replacement_artifact")
        if error is not None:
            print(json.dumps(_fail(error)))
            return
    print(json.dumps(_pass(
        event=event,
        decision=decision,
        handler_type=handler_type,
        connection_id=params["connection_id"],
        connection_artifact=connection_artifact,
        event_id=params["event_id"],
        emitter_run=params["emitter_run"],
        owner_run=params["owner_run"],
        handler_id=params["handler_id"],
        subject_artifact=subject,
        additional_context_artifacts=len(body.get("additional_context_artifacts", [])),
        replacement_artifact=replacement is not None,
    )))


if __name__ == "__main__":
    main()
