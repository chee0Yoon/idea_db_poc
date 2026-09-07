"""Adversarial unit checks for the read-only export integrity audit."""
from __future__ import annotations

import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock


_spec = importlib.util.spec_from_file_location(
    "integrity_audit", Path(__file__).with_name("integrity_audit.py")
)
assert _spec and _spec.loader
audit = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(audit)


def _row(record_id, kind, seq, data):
    return {"id": record_id, "kind": kind, "seq": seq, "recorded_at": "2026-09-07T00:00:00.000Z", "data": data}


def valid_export():
    """A scoped export with a shared foreign-origin idea and only local heads."""
    records = [
        _row("proj_a", "project", 1, {"title": "consumer", "description": ""}),
        _row("proj_b", "project", 1, {"title": "origin", "description": ""}),
        _row("ent_shared", "entity", 2, {"entity_kind": "idea", "project_id": "proj_b", "title": "shared", "tags": [], "derived_from": [], "lineage_kind": "none"}),
        _row("rev_shared", "revision", 3, {"entity_id": "ent_shared", "body": "shared body", "tags": [], "slots": [], "change_kind": "initial", "correction_of": None, "correction_reason": None, "previous_revision_id": None, "source": {"origin": "human", "capture_id": None, "model": None, "skill": None, "claim_mode": "inferred", "source_anchor": None}}),
        _row("rev_root", "revision", 4, {"entity_id": "proj_a", "body": "root", "tags": [], "slots": [{"slot_id": "shared", "revision_id": "rev_shared", "roles": []}], "change_kind": "initial", "correction_of": None, "correction_reason": None, "previous_revision_id": None, "source": {"origin": "human", "capture_id": None, "model": None, "skill": None, "claim_mode": "inferred", "source_anchor": None}}),
        _row("goal_a", "goal", 5, {"project_id": "proj_a", "scope": {"root_revision_id": "rev_root", "slot_path": ["shared"], "target_revision_id": "rev_shared"}, "statement": "evaluate shared", "criteria": [{"criterion_id": "c", "kind": "qualitative", "statement": "works", "metric": None, "comparator": None, "threshold": None, "unit": None, "required": True}], "origin": "official", "actor": "human:test"}),
        _row("baseline_a", "baseline", 6, {"goal_id": "goal_a", "criterion_ids": ["c"], "constraints": [], "effective_from": "2026-09-07T00:00:00Z", "supersedes": None, "actor": "human:test", "reason": "initial"}),
        _row("obs_a", "observation", 7, {"project_id": "proj_a", "target_revision_id": "rev_shared", "metric": None, "value": None, "unit": None, "status": "not_observed", "occurred_at": "2026-09-06T00:00:00Z", "method": None, "environment": {}, "artifact_ids": [], "capture_id": None, "actor": "human:test", "note": ""}),
        _row("assessment_a", "assessment", 8, {"root_revision_id": "rev_root", "slot_path": ["shared"], "target_revision_id": "rev_shared", "baseline_id": "baseline_a", "evidence_cutoff_seq": 7, "evidence_cutoff_at": "2026-09-07T00:00:00Z", "evaluator": "human:test", "rubric_version": "v1", "origin": "official", "status": "unknown", "criteria_results": [{"criterion_id": "c", "status": "unknown", "observed_value": None, "note": ""}], "evidence_observation_ids": ["obs_a"], "note": ""}),
    ]
    content = {"seq": 8, "records": records, "heads": [{"project_id": "proj_a", "working_head": "rev_root", "working_head_seq": 4, "official_head": None, "official_publication_id": None}], "receipts": [], "manifest": {"external_artifacts": [], "unresolved_external_artifacts": [], "complete_backup": True, "receipts_included": 0, "receipts_omitted": 0}}
    return {"format": "idea_db.export", "format_version": 1, "protocol_version": 1, "scope": {"project_id": "proj_a", "closure_included": True}, "content": content, "digest": audit.canonical_digest(content)}


def reseal(document):
    document["digest"] = audit.canonical_digest(document["content"])
    return document


class IntegrityAuditTest(unittest.TestCase):
    def test_summary_source_cannot_point_to_another_or_missing_capture(self):
        document = valid_export()
        document["content"]["records"][3]["data"]["summary"] = {
            "text": "shared summary", "source": {"origin": "ai", "claim_mode": "inferred", "skill": "test", "capture_id": "cap_missing"}}
        self.assert_detected(reseal(document), "invalid summary source")

    def assert_detected(self, document, needle):
        report = audit.audit_export(document)
        self.assertFalse(report["ok"], report)
        self.assertTrue(any(needle in message for message in report["errors"]), report["errors"])

    def test_valid_scoped_cross_project_reuse_has_no_false_positive(self):
        report = audit.audit_export(valid_export())
        self.assertTrue(report["ok"], report)

    def test_http_export_parser_preserves_number_lexemes_without_false_format_failure(self):
        class Response:
            def read(self):
                return json.dumps(valid_export(), separators=(",", ":")).encode("utf-8")
            def __enter__(self):
                return self
            def __exit__(self, *unused):
                return False

        with mock.patch.object(audit.request, "urlopen", return_value=Response()):
            document = audit._get_export("http://audit.test", None, 1.0)
        self.assertIsInstance(document["format_version"], audit.JsonNumber)
        self.assertTrue(audit.audit_export(document)["ok"])

    def test_detects_forged_export_hash(self):
        document = valid_export()
        document["digest"] = "sha256:" + "0" * 64
        self.assert_detected(document, "digest mismatch")

    def test_detects_missing_record_reference(self):
        document = valid_export()
        document["content"]["records"][4]["data"]["slots"][0]["revision_id"] = "rev_missing"
        self.assert_detected(reseal(document), "missing record")

    def test_detects_revision_cycle(self):
        document = valid_export()
        document["content"]["records"][3]["data"]["slots"] = [{"slot_id": "back", "revision_id": "rev_root", "roles": []}]
        self.assert_detected(reseal(document), "slot graph has cycle")

    def test_detects_invalid_goal_scope_path(self):
        document = valid_export()
        document["content"]["records"][5]["data"]["scope"]["target_revision_id"] = "rev_root"
        self.assert_detected(reseal(document), "goal scope path")

    def test_detects_future_evidence_sequence(self):
        document = valid_export()
        document["content"]["records"][7]["seq"] = 9
        document["content"]["records"][8]["seq"] = 10
        document["content"]["seq"] = 10
        self.assert_detected(reseal(document), "after cutoff seq")

    def test_detects_future_evidence_occurrence_time(self):
        document = valid_export()
        document["content"]["records"][7]["data"]["occurred_at"] = "2026-09-08T00:00:00Z"
        self.assert_detected(reseal(document), "after cutoff occurred_at")

    def test_ordinary_evidence_allows_late_recorded_early_occurred_observation(self):
        document = valid_export()
        # validate.rs intentionally gates ordinary evidence by seq and
        # occurred_at, not recorded_at; delayed collection is still admissible.
        document["content"]["records"][7]["recorded_at"] = "2026-09-08T00:00:00.000Z"
        report = audit.audit_export(reseal(document))
        self.assertTrue(report["ok"], report)

    def test_detects_progress_evidence_missing_or_wrong_type(self):
        document = valid_export()
        result = document["content"]["records"][8]["data"]["criteria_results"][0]
        result["progress_estimate"] = {"percent": 20, "rationale": "plan", "evidence_record_ids": ["goal_a", "missing"]}
        self.assert_detected(reseal(document), "progress_estimate.evidence_record_ids")

    def test_detects_orphan_head_projection(self):
        document = valid_export()
        document["content"]["heads"][0]["working_head"] = "rev_orphan"
        self.assert_detected(reseal(document), "head.working_head: missing record")

    def test_detects_head_revision_rooted_at_other_project(self):
        document = valid_export()
        document["content"]["records"][4]["data"]["entity_id"] = "ent_shared"
        self.assert_detected(reseal(document), "not rooted at project")

    def test_evidence_roundtrip_retains_export_number_tokens(self):
        class Response:
            def read(self):
                return json.dumps(valid_export(), separators=(",", ":")).encode("utf-8")
            def __enter__(self):
                return self
            def __exit__(self, *unused):
                return False

        with tempfile.TemporaryDirectory() as directory:
            with mock.patch.object(audit.request, "urlopen", return_value=Response()):
                report = audit.run_export_audit("http://audit.test", directory, timeout=1.0)
            raw = Path(report["evidence_path"]).read_text("utf-8")
            self.assertIn('"format_version":1', raw)
            self.assertNotIn('"format_version":"1"', raw)
            saved = json.loads(raw, parse_int=audit.JsonNumber, parse_float=audit.JsonNumber)
            self.assertTrue(audit.audit_export(saved["export"])["ok"])


if __name__ == "__main__":
    unittest.main()
