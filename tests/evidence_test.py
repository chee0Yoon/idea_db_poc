"""Tests of the evidence harness, including misleading measurement cases."""
import json
import sys
import unittest
from pathlib import Path
sys.path.insert(0, str(Path(__file__).parent))
from atomization_evidence import audit_response
from context_evidence import clip_utf8, pack, score
from evidence_corpus import ATOMIZATION_CASES


class EvidenceTest(unittest.TestCase):
    def test_concatenated_json_is_not_silently_repaired(self):
        self.assertFalse(audit_response('{"cases":[]}{"cases":[]}')["strict_json"])

    def test_repeated_quotes_do_not_inflate_coverage(self):
        cases = [{"id": c["id"], "atoms": [{"body": "Unsupported claim", "quotes": [c["source"], c["source"]],
                                           "status": "pending"}]} for c in ATOMIZATION_CASES]
        result = audit_response(json.dumps({"cases": cases}))
        self.assertEqual(result["mean_source_character_coverage"], 1)
        # Deliberately unsupported bodies illustrate that coverage cannot score truth.
        self.assertEqual(result["cases"][0]["body_equals_one_quote_count"], 0)

    def test_utf8_budget_includes_labels_and_never_breaks_encoding(self):
        text, used = pack([{"id": "abc", "role": "fe", "kind": "idea", "text": "🔒조건이 충족된 경우에만 허용한다."}], 35)
        self.assertLessEqual(len(text.encode()), 35)
        self.assertNotIn("�", text)
        self.assertFalse(used[0]["complete"])

    def test_partial_condition_gets_no_full_fact_credit(self):
        result = score("조건이 충족된", [], {"rule": "조건이 충족된 경우에만 허용한다."}, ["rule"])
        self.assertEqual(result["required_recall"], 0)


if __name__ == "__main__":
    unittest.main()
