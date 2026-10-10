#!/usr/bin/env python3
import json
import sys

HANDLER = "pre_tool_use_guard"


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
            return None, "tool-call payload is unavailable"
        try:
            with open(payload_path, "r", encoding="utf-8") as handle:
                payload_text = handle.read()
        except Exception as exc:
            return None, f"tool-call payload unreadable: {exc}"
    try:
        body = json.loads(payload_text)
    except Exception as exc:
        return None, f"tool-call payload is not valid JSON: {exc}"
    if not isinstance(body, dict):
        return None, "tool-call payload is not a JSON object"
    return body, None


def _collect_command_text(body):
    parts = []
    for key in ("command", "cmd", "arguments", "args"):
        value = body.get(key)
        if value is None:
            continue
        if isinstance(value, str):
            parts.append(value)
        else:
            parts.append(json.dumps(value, ensure_ascii=False))
    return " ".join(parts).lower()


def main():
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        print(json.dumps(_fail(f"admission request is not valid JSON: {exc}")))
        return

    if not isinstance(request, dict):
        print(json.dumps(_fail("admission request is not a JSON object")))
        return

    label = request.get("artifact_label")
    if label != "tool.call":
        print(json.dumps(_fail("artifact label is not tool.call", artifact_label=label)))
        return

    body, err = _load_payload(request)
    if err is not None:
        print(json.dumps(_fail(err)))
        return

    params = request.get("params")
    if not isinstance(params, dict):
        params = {}
    denied_tools = params.get("denied_tools") or []
    denied_substrings = params.get("denied_command_substrings") or []
    if not isinstance(denied_tools, list) or not isinstance(denied_substrings, list):
        print(json.dumps(_fail("guard params denied_tools/denied_command_substrings must be arrays")))
        return

    tool = body.get("tool")
    if not isinstance(tool, str) or not tool:
        print(json.dumps(_fail("tool-call artifact carries no string 'tool' id")))
        return

    if tool in denied_tools:
        print(json.dumps(_fail("tool is denied by the pre-tool-use guard", tool=tool)))
        return

    command_text = _collect_command_text(body)
    for needle in denied_substrings:
        if isinstance(needle, str) and needle and needle.lower() in command_text:
            print(json.dumps(_fail(
                "tool call matches a denied command substring",
                tool=tool,
                matched=needle,
            )))
            return

    print(json.dumps(_pass(tool=tool)))


if __name__ == "__main__":
    main()
