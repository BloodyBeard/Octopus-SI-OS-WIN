#!/usr/bin/env python3
import json
import math
import re
import sys


def verdict(passed, details):
    print(json.dumps({"passed": passed, "details": details}, separators=(",", ":")))


def fail(reason, **details):
    verdict(False, {"handler": "tool_call_allowed", "reason": reason, **details})


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError(f"duplicate JSON key: {key}")
        result[key] = value
    return result


def invalid_constant(value):
    raise ValueError(f"non-finite JSON number: {value}")


def finite_float(value):
    result = float(value)
    if not math.isfinite(result):
        invalid_constant(value)
    return result


def strict_json(value):
    return json.loads(value, object_pairs_hook=unique_object,
                      parse_constant=invalid_constant, parse_float=finite_float)


def pointer_parts(pointer):
    if not isinstance(pointer, str) or (pointer and not pointer.startswith("/")):
        raise ValueError("JSON pointer must be empty or start with /")
    if re.search(r"~(?:[^01]|$)", pointer):
        raise ValueError("JSON pointer has an invalid escape")
    return tuple(part.replace("~1", "/").replace("~0", "~")
                 for part in pointer.split("/")[1:])


def resolve_pointer(value, parts):
    for part in parts:
        if isinstance(value, dict) and part in value:
            value = value[part]
        elif isinstance(value, list) and re.fullmatch(r"0|[1-9][0-9]*", part):
            if len(part) > len(str(len(value))) or int(part) >= len(value):
                return False, None
            value = value[int(part)]
        else:
            return False, None
    return True, value


def equal_json(left, right):
    if type(left) is not type(right):
        return False
    if isinstance(left, dict):
        return left.keys() == right.keys() and all(
            equal_json(value, right[key]) for key, value in left.items()
        )
    if isinstance(left, list):
        return len(left) == len(right) and all(map(equal_json, left, right))
    return left == right


def metadata_matches(params, payload, metadata):
    matches = params["metadata_matches"]
    if not isinstance(matches, list) or not matches:
        fail("metadata_matches_not_nonempty_array")
        return False
    if not isinstance(metadata, dict):
        fail("metadata_not_object")
        return False
    seen = set()
    for index, match in enumerate(matches):
        if not isinstance(match, dict) or set(match) != {"payload", "metadata"}:
            fail("metadata_match_shape_invalid", index=index)
            return False
        try:
            pointers = (pointer_parts(match["payload"]), pointer_parts(match["metadata"]))
        except ValueError as error:
            fail("metadata_match_pointer_invalid", index=index, error=str(error))
            return False
        if pointers in seen:
            fail("metadata_match_duplicate", index=index)
            return False
        seen.add(pointers)
        values = []
        for side, value, parts in zip(("payload", "metadata"), (payload, metadata), pointers):
            found, value = resolve_pointer(value, parts)
            if not found or value is None:
                fail("metadata_match_missing" if not found else "metadata_match_null",
                     index=index, side=side, pointer=match[side])
                return False
            values.append(value)
        if not equal_json(*values):
            fail("metadata_match_mismatch", index=index,
                 payload=match["payload"], metadata=match["metadata"])
            return False
    return True


def main():
    try:
        request_utf8 = sys.stdin.read()
        request = json.loads(request_utf8)
    except Exception as error:
        fail("request_json_invalid", error=str(error))
        return
    if not isinstance(request, dict):
        fail("request_not_object")
        return
    params = request.get("params")
    if not isinstance(params, dict):
        fail("params_not_object")
        return
    if "metadata_matches" in params:
        try:
            request = strict_json(request_utf8)
        except Exception as error:
            fail("request_json_invalid", error=str(error))
            return
    tools = params.get("tools")
    if not isinstance(tools, list) or not tools:
        fail("tools_not_nonempty_array")
        return
    if any(not isinstance(tool, str) or not tool for tool in tools):
        fail("tool_id_invalid")
        return
    if len(set(tools)) != len(tools):
        fail("tool_id_duplicate")
        return
    payload_utf8 = request.get("payload_utf8")
    if not isinstance(payload_utf8, str):
        fail("payload_utf8_not_string")
        return
    try:
        payload = strict_json(payload_utf8) if "metadata_matches" in params else json.loads(payload_utf8)
    except Exception as error:
        fail("payload_json_invalid", error=str(error))
        return
    if not isinstance(payload, dict):
        fail("payload_not_object")
        return
    keys = set(payload)
    if not {"tool", "arguments"}.issubset(keys):
        fail("payload_shape_invalid", keys=sorted(keys))
        return
    tool = payload.get("tool")
    if not isinstance(tool, str) or not tool:
        fail("payload_tool_invalid")
        return
    if not isinstance(payload.get("arguments"), dict):
        fail("payload_arguments_not_object", tool=tool)
        return
    if tool not in tools:
        fail("tool_not_allowed", tool=tool, tools=tools)
        return
    if "metadata_matches" in params and not metadata_matches(params, payload, request.get("metadata")):
        return
    verdict(True, {
        "handler": "tool_call_allowed",
        "tool": tool,
    })


if __name__ == "__main__":
    main()
