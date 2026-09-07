#!/usr/bin/env python3
"""Replay explicitly reviewed real model outputs through the official MCP.

No LLM calls here. Model attempts/repairs are scored separately; this verifies
their client packaging, local embedding and persisted graph/candidate semantics.
"""
import argparse
import json
import os
import uuid
from pathlib import Path

from acceptance import Api, entity_record, expected_head, package, project_record, require, revision_record, root_change, slot
from atomization_evidence import audit_response
from context_evidence import save
from evidence_corpus import ATOMIZATION_CASES


def run(api, out):
    out.mkdir(parents=True, exist_ok=False)
    models = json.loads((Path(__file__).resolve().parents[1] / "examples/evidence/model-responses-reviewed.json").read_text())
    report = {"status": "passed", "providers": [], "note": "Reviewed/normalized outputs, not first-pass model success rates."}
    for model, response in models.items():
        scored = audit_response(json.dumps(response, ensure_ascii=False))
        require(scored.get("schema_valid") and scored["quote_error_count"] == 0, "Reviewed model contract invalid")
        prefix = "mr" + uuid.uuid4().hex[:12]
        project, root, schema = prefix + "_project", prefix + "_root", prefix + "_schema"
        api.post("/api/packages/apply", package(prefix + "_create", "Recorded model-output roundtrip",
            expected_head(project, None), [project_record(project, model + " 원자화 검증"),
                                          entity_record(schema, project, "schema", "조건·범위·출처 검토")]))
        core_slots, accepted_ids, pending_ids, source_ids, package_checks = [], [], [], [], []
        source_cases = {c["id"]: c for c in ATOMIZATION_CASES}
        for case in response["cases"]:
            label = case["id"]
            raw = source_cases[label]["source"]
            capture = api.post("/api/captures", {"project_id": project, "content": raw, "source_kind": "note",
                              "source_ref": "recorded-model-corpus/" + label})["capture"]["id"]
            source_ids.append(capture)
            # Retain coverage ledger and exact original output separately from the
            # transformed Idea bodies. Model actor/origin are provenance claims.
            ledger = {"case": label, "source_capture": capture, "model": model,
                      "review": "Main selected recorded response; parent context retained", "atoms": case["atoms"]}
            ledger_id = api.post("/api/captures", {"project_id": project,
                "content": json.dumps(ledger, ensure_ascii=False), "source_kind": "note",
                "source_ref": "coverage-ledger/" + label})["capture"]["id"]
            source_ids.append(ledger_id)
            core, core_rev = prefix + "_core_" + label, prefix + "_core_rev_" + label
            records = [entity_record(core, project, "core", label)]
            atom_slots = []
            for number, atom in enumerate(case["atoms"]):
                atom_id = prefix + "_" + label + "_a" + str(number)
                if atom["status"] == "pending":
                    records.append({"id": atom_id, "kind": "candidate", "data": {
                        "project_id": project, "capture_id": capture, "proposed_kind": "idea",
                        "title": label + " 미확정 사항", "body": atom["body"], "origin": "ai", "model": model,
                        "skill": "atomization-evidence-v1", "claim_mode": "inferred", "status": "pending"}})
                    pending_ids.append(atom_id)
                else:
                    records.extend([entity_record(atom_id + "_entity", project, "idea", label),
                        revision_record(atom_id, atom_id + "_entity", atom["body"], source={
                            "origin": "ai", "model": model, "skill": "atomization-evidence-v1",
                            "capture_id": capture, "claim_mode": "inferred"})])
                    atom_slots.append(slot("a" + str(number), atom_id, "planning"))
                    accepted_ids.append(atom_id)
            records.append(revision_record(core_rev, core, raw, slots=atom_slots))
            packet = package(prefix + "_case_" + label, "Preserve atomic body and parent scope",
                             expected_head(project, None), records)
            preview = api._mcp("idea_upload_preview", {"body": {"package": packet}})
            # Review packet content before apply, not only the constant merge flag.
            reviewed = {r["id"]: r for r in preview["review_records"]}
            require(all(reviewed[r["id"]]["data"] == r["data"] for r in records),
                    "Preview changed model-authored record content")
            result = api._mcp("idea_upload_apply", {"body": {"upload_id": preview["upload_id"],
                "prepared_digest": preview["prepared_digest"]}})
            save(out / (prefix + "-" + label + ".json"), {"packet": packet, "preview": preview, "apply": result})
            core_slots.append(slot(label, core_rev, "planning"))
            package_checks.append(label)
        schema_rev = prefix + "_schema_rev"
        api.post("/api/packages/apply", package(prefix + "_root_apply", "Compose reviewed model atoms",
            expected_head(project, None), [revision_record(schema_rev, schema, "조건과 범위를 부모 맥락으로 유지한다.", slots=core_slots),
                revision_record(root, project, "실제 모델 출력을 검토한 뒤 MCP와 로컬 임베딩을 거쳐 보존한다.",
                                slots=[slot("review", schema_rev, "planning")])],
            root_change=root_change(project, root, "원문", "검토한 원자와 미확정 후보를 분리해 저장한다.")))
        exported = api.get("/api/export", project_id=project)
        stored = {r["id"]: r for r in exported["content"]["records"]}
        require(all(stored[r]["kind"] == "revision" for r in accepted_ids), "Accepted Idea lost")
        require(all(stored[r]["kind"] == "candidate" and stored[r]["data"]["status"] == "pending" for r in pending_ids), "Unknowns were promoted")
        composed = {s["revision_id"] for r in stored.values() if r["kind"] == "revision" for s in r["data"]["slots"]}
        require(not set(pending_ids) & composed, "Pending candidate entered composition")
        indexed = {r["data"]["revision_id"] for r in stored.values() if r["kind"] == "embedding"}
        require(set(accepted_ids) <= indexed, "Model Idea lacks local embedding")
        require(set(source_ids) <= stored.keys(), "Source or coverage ledger lost")
        report["providers"].append({"model": model, "project": project, "cases": len(package_checks),
            "accepted_ideas": len(accepted_ids), "pending_candidates": len(pending_ids),
            "source_and_ledger_captures": len(source_ids), "embedding_complete": True})
        print("PASS recorded model MCP roundtrip " + model, flush=True)
    save(out / "report.json", report)
    return report


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    run(Api(args.base_url, os.environ.get("IDEA_DB_TOKEN"), 30), args.output_dir)
