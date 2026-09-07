#!/usr/bin/env python3
"""Separate structural rejection guarantees from semantic review gaps.

Expected semantic counterexamples are measured outcomes, never disguised as
successful semantic detection. Domain mutations use MCP; Cypher is read-only.
"""
import argparse
import base64
import copy
import json
import os
import subprocess
import unicodedata
import uuid
from pathlib import Path

from acceptance import Api, ApiError, entity_record, expected_head, package, project_record, require, revision_record
from context_evidence import save, scoped_source
from mcp_ingest import docker_mcp_container


def projection_audit(api):
    export = api.get("/api/export")["content"]
    expected = sorted([[r["id"], s["revision_id"], s["slot_id"], sorted(s["roles"])]
                       for r in export["records"] if r["kind"] == "revision"
                       for s in r["data"]["slots"]])
    statement = "MATCH (a:Record)-[r:CONTAINS]->(b:Record) RETURN a.id, b.id, r.slot_id, r.roles ORDER BY a.id, b.id, r.slot_id"
    payload = json.dumps({"statements": [{"statement": statement}]}).encode()
    encoded = base64.b64encode(payload).decode()
    shell = (f"payload=$(printf '%s' '{encoded}' | base64 -d)\n"
             "exec wget --quiet --output-document=- --method=POST --header='Content-Type: application/json' "
             '--user=neo4j --password="$NEO4J_PASSWORD" --body-data="$payload" '
             "http://127.0.0.1:7474/db/neo4j/tx/commit")
    result = subprocess.run(["docker", "exec", docker_mcp_container(), "bash", "-ceu", shell],
                            capture_output=True, text=True, timeout=30)
    require(result.returncode == 0, "Neo4j read-only relationship audit failed")
    data = json.loads(result.stdout)
    require(not data.get("errors"), "Neo4j relationship audit query error")
    rows = sorted([[a, b, slot, sorted(roles)] for item in data["results"][0]["data"]
                   for a, b, slot, roles in [item["row"]]])
    require(rows == expected, "CONTAINS projections differ from authoritative slots")
    return {"ok": True, "relationship_count": len(rows), "checked": "Exact from/to/slot/roles equality", "rows": rows}


def run(api, out):
    out.mkdir(parents=True, exist_ok=False)
    prefix = "sp" + uuid.uuid4().hex[:12]
    project, entity, revision = prefix + "_project", prefix + "_idea", prefix + "_revision"
    text = "🔒 안내문\n로그인 실패가 5회 이상이면 접근을 허용하지 않는다.\n조사 중인 로그는 삭제하지 않는다."
    body = "로그인 실패가 5회 이상이면 접근을 허용하지 않는다."
    api.post("/api/packages/apply", package(prefix + "_project_create", "Semantic diagnostics",
        expected_head(project, None), [project_record(project, "의미·Unicode 무결성 반례")]))
    capture = api.post("/api/captures", {"project_id": project, "content": text, "source_kind": "note"})["capture"]["id"]
    record = revision_record(revision, entity, body, source=scoped_source(capture, text, body))
    api.post("/api/packages/apply", package(prefix + "_base", "Unicode scalar middle anchor",
        expected_head(project, None), [entity_record(entity, project, "idea", "접근 규칙"), record]))
    checks = [{"case": "emoji_middle_scalar_anchor", "expected": "accepted", "actual": "accepted"}]
    # Every probe is validated in Neo4j and compared to an exact export digest;
    # validation must never create records, receipts, heads, or advance seq.
    variants = []
    nfd = copy.deepcopy(record)
    nfd["data"]["body"] = unicodedata.normalize("NFD", body)
    variants.append(("nfd_body_nfc_anchor", nfd, False))
    utf16 = copy.deepcopy(record)
    utf16["data"]["source"]["source_anchor"]["start"] += 1
    utf16["data"]["source"]["source_anchor"]["end"] += 1
    variants.append(("utf16_offset_after_emoji", utf16, False))
    inferred = copy.deepcopy(record)
    inferred["data"]["source"]["claim_mode"] = "inferred"
    variants.append(("inferred_claim_with_anchor", inferred, False))
    forged = copy.deepcopy(record)
    forged["data"]["source"]["capture_id"] = prefix + "_missing"
    variants.append(("missing_source_capture", forged, False))
    for label, newbody in [
        ("negation_flip", body.replace("허용하지 않는다", "허용한다")),
        ("comparator_flip", body.replace("이상", "미만")),
        ("numeric_change", body.replace("5회", "3회")),
        ("typo_only", body.replace("접근을", "접근 을"))]:
        correction = revision_record(prefix + "_" + label, entity, newbody,
            change_kind="correction", correction_of=revision, correction_reason="Diagnostic claimed correction")
        variants.append((label, correction, label not in ("numeric_change",)))
    for label, newrecord, expected_valid in variants:
        newrecord["id"] = prefix + "_" + label
        records = [newrecord]
        if newrecord["data"]["change_kind"] == "initial":
            newrecord["data"]["entity_id"] = prefix + "_" + label + "_entity"
            records.insert(0, entity_record(newrecord["data"]["entity_id"], project, "idea", "인용 반례"))
        proposal = package(prefix + "_probe_" + label, "Read-only diagnostic validation",
            expected_head(project, None), records)
        before = api.get("/api/export")["digest"]
        try:
            response = api.post("/api/packages/validate", proposal)
            valid = response.get("valid", False)
        except ApiError as exc:
            valid, response = False, exc.body
        after = api.get("/api/export")["digest"]
        require(before == after, label + ": validation mutated authoritative export")
        if label not in ("negation_flip", "comparator_flip"):
            require(valid == expected_valid, label + ": structural expectation differs")
        checks.append({"case": label, "valid": valid, "response": response, "state_unchanged": True,
                       "semantic_gap": valid and label in ("negation_flip", "comparator_flip")})
    # Quote mining: an exact extracted substring can omit a governing exception.
    quote = "접근을 허용하지 않는다."
    mined = revision_record(prefix + "_mined", prefix + "_mined_entity", quote,
                           source=scoped_source(capture, text, quote))
    result = api.post("/api/packages/validate", package(prefix + "_quote_mine", "Partial quote diagnostic",
        expected_head(project, None), [entity_record(prefix + "_mined_entity", project, "idea", "부분 인용"), mined]))
    checks.append({"case": "exact_quote_loses_condition", "valid": result["valid"],
                   "semantic_gap": result["valid"], "response": result})
    projection = projection_audit(api)
    save(out / "neo4j-projection.json", projection)
    report = {"status": "measured", "project": project, "checks": checks,
              "semantic_gap_count": sum(bool(c.get("semantic_gap")) for c in checks),
              "limits": ["DB structural validation cannot establish semantic equivalence or quotation completeness.",
                         "Validation probes do not apply intentionally invalid semantic edits.",
                         "Restart check happens after committed writes; it is not an in-flight commit crash injection."]}
    save(out / "report.json", report)
    print(json.dumps({"status": report["status"], "cases": len(checks), "semantic_gaps": report["semantic_gap_count"]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--projection-only", action="store_true")
    args = parser.parse_args()
    api = Api(args.base_url, os.environ.get("IDEA_DB_TOKEN"), 30)
    if args.projection_only:
        save(args.output_dir / "neo4j-projection.json", projection_audit(api))
    else:
        run(api, args.output_dir)
