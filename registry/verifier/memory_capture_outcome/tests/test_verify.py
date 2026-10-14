import copy
import hashlib
import json
import pathlib
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).resolve().parents[1] / "verify.py"
WRITER = "file.write.selected_publication"


def reference(identity):
    return {"id": identity, "content": list(hashlib.sha256(identity.encode()).digest())}


def evidence(seq, label, payload):
    return {"source_artifact": reference(str(seq)), "source_movement_seq": seq,
            "label": label, "payload": payload}


class CaptureOutcomeVerifierTests(unittest.TestCase):
    def test_exact_publication_settlement_is_required_for_every_success(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = pathlib.Path(temporary)
            content = json.dumps({"scope": {"ee_ids": ["selected-ee"]}, "selection": {}, "requires": []})
            digest = hashlib.sha256(content.encode()).hexdigest()
            path = str(root / "entry.json")
            arguments = {"ordered": True, "writes": [{"path": path, "content": content, "expected_before_hash": None}]}
            prepared = evidence(2, "prepare.result", {"tool": "prepare", "label": "prepare.result", "ok": True, "error": None,
                "body": {"status": "prepared", "file_write": arguments}})
            call = evidence(3, "tool.call", {"tool": WRITER, "correlation_id": "write-1", "arguments": arguments})
            result = evidence(4, WRITER + ".result", {"label": WRITER + ".result", "tool": WRITER,
                "correlation_id": "write-1", "ok": True, "error": None,
                "body": {"result": "ordered_file_writes", "writes": [{"path": path,
                    "outcome": "applied", "error": None, "before_hash": None,
                    "after_hash": digest, "observed_hash": digest, "bytes": len(content.encode())}]}})
            payload = {"schema_version": 1, "label": "memory.capture.outcome",
                "source": {"run": "source", "ee": "selected-ee", "provider_interface": "provider",
                    "terminal_artifact": reference("terminal"), "terminal_source_movement_seq": 20},
                "experience": {"source_artifact": reference("experience"), "source_movement_seq": 1},
                "omission": {"before_current_experience": True, "selected_evidence_trimmed": False},
                "outcome": {"schema_version": 1, "status": "noop", "source_run": "source",
                    "reason": "No additional reusable knowledge.", "receipts": []}, "evidence": []}
            request = {"params": {"max_bytes": 65536, "source_run": "source", "source_ee": "selected-ee",
                "provider_interface": "provider", "publication_tool": WRITER, "prepare_tool": "prepare",
                "entries_root": str(root)}, "artifact_label": "memory.capture.outcome", "payload_path": str(root / "outcome.json")}

            def check(entries, status, passed):
                candidate = copy.deepcopy(payload)
                candidate["evidence"] = entries
                candidate["outcome"]["status"] = status
                if status == "published":
                    candidate["outcome"]["receipts"] = [{"tool": WRITER, "correlation_id": "write-1"}]
                pathlib.Path(request["payload_path"]).write_text(json.dumps(candidate))
                completed = subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(request),
                    text=True, capture_output=True, check=True, timeout=10)
                verdict = json.loads(completed.stdout)
                self.assertEqual(verdict["passed"], passed, verdict)

            check([], "noop", True)
            check([prepared], "noop", True)
            check([prepared, call, result], "published", True)
            for status in ("noop", "published"):
                for entries in ([call], [result], [prepared, result], [prepared, call]):
                    with self.subTest(status=status, entries=[item["label"] for item in entries]):
                        check(entries, status, False)
                for code in ("tool_effect_indeterminate", "file_write_conflict"):
                    failed = copy.deepcopy(result)
                    failed["payload"].update(ok=False, body=None, error={"code": code})
                    with self.subTest(status=status, error=code):
                        check([prepared, call, failed], status, False)
                        failed["source_artifact"] = reference("second-result")
                        failed["source_movement_seq"] = 6
                        failed["payload"]["correlation_id"] = "write-2"
                        second_call = evidence(5, "tool.call", {**call["payload"], "correlation_id": "write-2"})
                        check([prepared, call, result, second_call, failed], status, False)
            check([prepared, call, result], "noop", False)
            for change in ("call", "late", "label", "error", "body"):
                changed = copy.deepcopy(prepared)
                if change == "call":
                    changed["label"] = "tool.call"
                    changed["payload"].update(arguments={}, correlation_id="prepare-call")
                elif change == "late":
                    changed["source_movement_seq"] = 5
                elif change == "label":
                    changed["payload"]["label"] = "unrelated.result"
                elif change == "error":
                    changed["payload"]["error"] = {"code": "invalid_preparation"}
                else:
                    changed["payload"]["body"] = None
                with self.subTest(preparation=change):
                    check([changed, call, result], "published", False)
            for change in ("correlation", "duplicate", "order", "partial"):
                changed = copy.deepcopy(result)
                entries = [prepared, call, changed]
                if change == "correlation":
                    changed["payload"]["correlation_id"] = "unmatched"
                elif change == "duplicate":
                    entries.append(evidence(5, result["label"], result["payload"]))
                elif change == "order":
                    changed["source_movement_seq"] = 2
                else:
                    changed["payload"]["body"]["writes"][0]["outcome"] = "indeterminate"
                with self.subTest(change=change):
                    check(entries, "published", False)


if __name__ == "__main__":
    unittest.main()
