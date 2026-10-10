#!/usr/bin/env python3
import json
import sys


def find_call(value, tool, interface):
    if isinstance(value, dict):
        arguments = value.get("arguments")
        if value.get("tool", value.get("name")) == tool and isinstance(arguments, dict) and arguments.get("interface") == interface:
            return True
        return any(find_call(nested, tool, interface) for nested in value.values())
    if isinstance(value, list):
        return any(find_call(nested, tool, interface) for nested in value)
    return False


def main():
    request = json.load(sys.stdin)
    params = request.get("params") if isinstance(request.get("params"), dict) else {}
    runtime = request.get("metadata", {}).get("runtime", {})
    tool = params.get("tool") or "artifact.send"
    interface = params.get("interface") or runtime.get("interface")
    try:
        payload = json.loads(request.get("payload_utf8") or "")
    except Exception:
        payload = None
    passed = isinstance(tool, str) and isinstance(interface, str) and find_call(payload, tool, interface)
    print(json.dumps({
        "passed": passed,
        "details": {
            "handler": "emit_tool_call",
            "artifact": request.get("artifact", {}).get("id"),
            "tool": tool,
            "interface": interface,
            "error": None if passed else "the artifact carries no matching emission tool call",
        },
    }))


if __name__ == "__main__":
    main()
