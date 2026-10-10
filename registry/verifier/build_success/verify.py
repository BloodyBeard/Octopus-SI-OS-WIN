#!/usr/bin/env python3
import json
import sys


def main():
    request = json.load(sys.stdin)
    try:
        value = json.loads(request.get("payload_utf8") or "")
    except Exception:
        value = None
    body = value.get("body") if isinstance(value, dict) else None
    output = body.get("output") if isinstance(body, dict) else None
    correlation_id = value.get("correlation_id") if isinstance(value, dict) else None
    correlation_id_is_valid = (
        correlation_id is None
        or (
            isinstance(correlation_id, str)
            and correlation_id
            and correlation_id.strip() == correlation_id
        )
    )
    passed = (
        isinstance(value, dict)
        and set(value).issubset({"label", "tool", "correlation_id", "ok", "body"})
        and value.get("label") == "coder.build.result"
        and value.get("tool") == "coder.build"
        and value.get("ok") is True
        and correlation_id_is_valid
        and isinstance(body, dict)
        and body.get("result") == "command_finished"
        and isinstance(output, dict)
        and output.get("exit_code") == 0
    )
    print(json.dumps({
        "passed": passed,
        "details": {
            "handler": "build_success",
            "artifact": request.get("artifact", {}).get("id"),
            "error": None if passed else "build evidence did not prove success",
        },
    }))


if __name__ == "__main__":
    main()
