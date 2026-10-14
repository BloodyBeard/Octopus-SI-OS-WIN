#!/usr/bin/env python3
import json
import os
import re
import sys


def reference_key(value):
    assert isinstance(value, dict) and set(value) == {"id", "content"}, "invalid receipt reference"
    assert isinstance(value["id"], str) and value["id"] and value["id"].strip() == value["id"], "invalid receipt identity"
    assert isinstance(value["content"], list) and len(value["content"]) == 32 and all(type(byte) is int and 0 <= byte <= 255 for byte in value["content"]), "invalid receipt hash"
    return value["id"], tuple(value["content"])


def verify(request):
    params = request["params"]
    limit = params["max_bytes"]
    assert isinstance(limit, int) and 0 < limit <= 268435456, "invalid outcome byte bound"
    with open(request["payload_path"], "rb") as handle:
        data = handle.read(limit + 1)
    assert len(data) <= limit, "outcome exceeds its declared bound"
    payload = json.loads(data)
    assert request["artifact_label"] == payload["label"] == "memory.capture.outcome" and payload["schema_version"] == 1, "outcome label mismatch"
    source = payload["source"]
    assert source["run"] == params["source_run"] and source["ee"] == params["source_ee"] and source["provider_interface"] == params["provider_interface"], "outcome evidence source mismatch"
    reference_key(source["terminal_artifact"])
    assert isinstance(source["terminal_source_movement_seq"], int), "original observer terminal movement absent"
    experience = payload["experience"]
    reference_key(experience["source_artifact"])
    assert isinstance(experience["source_movement_seq"], int), "current experience movement absent"
    assert payload["omission"]["before_current_experience"] is True and payload["omission"]["selected_evidence_trimmed"] is False, "outcome is not bounded to current experience"
    outcome = payload["outcome"]
    assert set(outcome) == {"schema_version", "status", "source_run", "reason", "receipts"}, "outcome fields mismatch"
    assert outcome["schema_version"] == 1 and outcome["source_run"] == params["source_run"], "outcome source mismatch"
    assert isinstance(outcome["reason"], str) and outcome["reason"].strip(), "outcome reason absent"
    assert outcome["status"] in ("noop", "published"), "capture failed"
    receipts = outcome["receipts"]
    assert isinstance(receipts, list), "outcome receipts must be an array"
    entries = payload["evidence"]
    assert isinstance(entries, list), "outcome context malformed"
    by_ref = {}
    for entry in entries:
        key = reference_key(entry["source_artifact"])
        assert key not in by_ref and isinstance(entry["source_movement_seq"], int) and experience["source_movement_seq"] < entry["source_movement_seq"] < source["terminal_source_movement_seq"], "duplicate or evidence outside current experience"
        by_ref[key] = entry
    prepared = []
    calls = {}
    results = {}
    applied = set()
    writer = params["publication_tool"]
    prepare = params["prepare_tool"]
    for key, entry in by_ref.items():
        value = entry["payload"]
        is_result = entry["label"] == writer + ".result"
        assert not is_result or isinstance(value, dict), "publication receipt malformed"
        if not isinstance(value, dict):
            continue
        if entry["label"] == prepare + ".result" and value.get("label") == entry["label"] and value.get("tool") == prepare and value.get("ok") is True and value.get("error") is None and isinstance(value.get("body"), dict) and value["body"].get("status") == "prepared":
            prepared.append((entry["source_movement_seq"], value["body"]))
        if value.get("tool") != writer and not is_result:
            continue
        assert value.get("tool") == writer, "publication receipt tool mismatch"
        correlation = value["correlation_id"]
        assert isinstance(correlation, str) and correlation and correlation.strip() == correlation, "publication correlation malformed"
        if entry["label"] == "tool.call" and "arguments" in value:
            assert correlation not in calls, "duplicate publication correlation"
            calls[correlation] = (entry["source_movement_seq"], value["arguments"])
        else:
            assert is_result and value.get("label") == entry["label"], "publication receipt label mismatch"
            assert correlation not in results, "duplicate publication result"
            assert value.get("ok") is True and value.get("error") is None and isinstance(value.get("body"), dict) and value["body"].get("result") == "ordered_file_writes", "publication failed or has an uncertain effect"
            results[correlation] = key
            applied.add(key)
    assert calls.keys() == results.keys(), "publication call has no exact result or result has no call"
    for correlation, key in results.items():
        assert calls[correlation][0] < by_ref[key]["source_movement_seq"], "publication result precedes its call"
    if outcome["status"] == "noop":
        assert not receipts and not calls, "no-op has publication evidence"
        return
    selected = []
    for receipt in receipts:
        assert isinstance(receipt, dict) and set(receipt) == {"tool", "correlation_id"} and receipt["tool"] == writer and isinstance(receipt["correlation_id"], str) and receipt["correlation_id"], "publication receipt selector malformed"
        matches = [key for key in applied if by_ref[key]["payload"].get("correlation_id") == receipt["correlation_id"]]
        assert len(matches) == 1, "publication selector has no unique admitted receipt"
        selected.append(matches[0])
    assert selected and len(set(selected)) == len(selected) and set(selected) == applied, "publication must report every actual writer receipt"
    root = params["entries_root"]
    assert isinstance(root, str) and os.path.isabs(root) and os.path.normpath(root) == root, "publication root malformed"
    for key in selected:
        result = by_ref[key]["payload"]
        assert result.get("error") is None and result["tool"] == writer and result["label"] == writer + ".result", "publication receipt malformed"
        call_seq, arguments = calls[result["correlation_id"]]
        assert any(seq < call_seq and plan["file_write"] == arguments for seq, plan in prepared), "writer call differs from prior exact prepared bytes"
        assert arguments["ordered"] is True and isinstance(arguments["writes"], list) and arguments["writes"], "publication batch malformed"
        writes = result["body"]["writes"]
        assert len(writes) == len(arguments["writes"]), "publication receipt count mismatch"
        for receipt, planned in zip(writes, arguments["writes"]):
            path = receipt["path"]
            assert path == planned["path"] and os.path.dirname(path) == root and os.path.basename(path).endswith(".json"), "publication path escaped selected entries"
            assert receipt["outcome"] == "applied" and receipt["error"] is None and receipt["after_hash"] == receipt["observed_hash"], "publication was not applied"
            assert receipt["before_hash"] == planned["expected_before_hash"] and isinstance(receipt["after_hash"], str) and re.fullmatch(r"[0-9a-f]{64}", receipt["after_hash"]), "publication receipt hash mismatch"
            assert receipt["bytes"] == len(planned["content"].encode("utf-8")), "publication byte receipt mismatch"
            entry = json.loads(planned["content"])
            assert entry["scope"]["ee_ids"] == [params["source_ee"]], "publication broadened source EE scope"
            assert entry["selection"] == {} and entry.get("requires", []) == [], "capture cannot install capability selections"


if __name__ == "__main__":
    try:
        verify(json.load(sys.stdin))
        result = {"passed": True, "details": {"contract": "memory_capture_outcome"}}
    except (AssertionError, KeyError, TypeError, ValueError, OSError) as error:
        result = {"passed": False, "details": {"contract": "memory_capture_outcome", "error": str(error)}}
    print(json.dumps(result))
