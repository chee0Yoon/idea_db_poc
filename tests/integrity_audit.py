#!/usr/bin/env python3
"""Read-only, export-based integrity audit for a running idea_db instance.

This intentionally audits persisted structure, not the truth or semantic quality
of user-authored bodies, sources, decisions, or assessments.  It never issues a
mutation and writes only a new evidence JSON file below ``--output-dir``.
"""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
import os
import sys
import uuid
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any
from urllib import error, parse, request

Json = dict[str, Any]
KINDS = {
    "project", "entity", "revision", "capture", "candidate", "promotion",
    "artifact", "observation", "goal", "baseline", "assessment", "link",
    "embedding", "head_change", "publication",
}


class AuditFailure(AssertionError):
    pass


class JsonNumber(str):
    """A JSON number spelling retained for the export canonical digest."""


def canonical_json(value: Any) -> str:
    """Serialize JSON while retaining ``JsonNumber`` tokens as JSON numbers."""
    def encode(item: Any) -> str:
        if isinstance(item, JsonNumber):
            return str(item)
        if item is None:
            return "null"
        if item is True:
            return "true"
        if item is False:
            return "false"
        if isinstance(item, (int, float)):
            return json.dumps(item, allow_nan=False, separators=(",", ":"))
        if isinstance(item, str):
            return json.dumps(item, ensure_ascii=False, separators=(",", ":"))
        if isinstance(item, list):
            return "[" + ",".join(encode(x) for x in item) + "]"
        if isinstance(item, dict):
            return "{" + ",".join(
                encode(k) + ":" + encode(item[k]) for k in sorted(item)
            ) + "}"
        raise TypeError(f"unsupported JSON value: {type(item)!r}")
    return encode(value)


def canonical_digest(value: Any) -> str:
    return "sha256:" + hashlib.sha256(canonical_json(value).encode("utf-8")).hexdigest()


def _get_export(base_url: str, token: str | None, timeout: float) -> Json:
    headers = {"Accept": "application/json"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    url = base_url.rstrip("/") + "/api/export?" + parse.urlencode({"include_receipts": "true"})
    try:
        with request.urlopen(request.Request(url, headers=headers), timeout=timeout) as response:
            raw = response.read()
    except error.HTTPError as exc:
        raise AuditFailure(f"GET /api/export returned {exc.code}: {exc.read().decode(errors='replace')}") from exc
    except error.URLError as exc:
        raise AuditFailure(f"cannot reach {url}: {exc.reason}") from exc
    try:
        value = json.loads(raw, parse_int=JsonNumber, parse_float=JsonNumber)
    except json.JSONDecodeError as exc:
        raise AuditFailure("GET /api/export returned invalid JSON") from exc
    if not isinstance(value, dict):
        raise AuditFailure("GET /api/export returned a non-object")
    return value


def _as_int(value: Any, label: str, errors: list[str]) -> int | None:
    try:
        return int(value)
    except (TypeError, ValueError):
        errors.append(f"{label}: expected integer, got {value!r}")
        return None


def _kind(records: dict[str, Json], record_id: Any) -> str | None:
    row = records.get(record_id) if isinstance(record_id, str) else None
    return row.get("kind") if isinstance(row, dict) else None


def _ref(records: dict[str, Json], record_id: Any, allowed: set[str] | None,
         label: str, errors: list[str]) -> None:
    kind = _kind(records, record_id)
    if kind is None:
        errors.append(f"{label}: missing record {record_id!r}")
    elif allowed is not None and kind not in allowed:
        errors.append(f"{label}: expected {sorted(allowed)}, found {kind} ({record_id})")


def _resolve_path(records: dict[str, Json], root: Any, path: Any) -> str | None:
    if not isinstance(root, str) or not isinstance(path, list):
        return None
    current = root
    for slot_id in path:
        row = records.get(current)
        data = row.get("data", {}) if isinstance(row, dict) else {}
        matches = [s for s in data.get("slots", []) if isinstance(s, dict) and s.get("slot_id") == slot_id]
        if len(matches) != 1 or not isinstance(matches[0].get("revision_id"), str):
            return None
        current = matches[0]["revision_id"]
    return current


def _timestamp_at_or_before(value: Any, cutoff: Any, label: str, errors: list[str]) -> bool:
    if not isinstance(value, str) or not isinstance(cutoff, str):
        errors.append(f"{label}: timestamp/cutoff is not text")
        return False
    try:
        parse = lambda text: dt.datetime.fromisoformat(text.replace("Z", "+00:00"))
        return parse(value) <= parse(cutoff)
    except ValueError:
        errors.append(f"{label}: invalid RFC3339 timestamp")
        return False


def _anchor(records: dict[str, Json], anchor: Any, body: Any, label: str, errors: list[str]) -> None:
    if not isinstance(anchor, dict):
        errors.append(f"{label}: extracted source has no anchor")
        return
    capture = anchor.get("capture_id")
    _ref(records, capture, {"capture"}, label + ".capture_id", errors)
    start = _as_int(anchor.get("start"), label + ".start", errors)
    end = _as_int(anchor.get("end"), label + ".end", errors)
    if start is None or end is None or start < 0 or end <= start:
        errors.append(f"{label}: invalid anchor range")
        return
    content = records.get(capture, {}).get("data", {}).get("content") if isinstance(capture, str) else None
    if not isinstance(content, str) or not isinstance(body, str) or end > len(content):
        errors.append(f"{label}: anchor out of range or non-text body")
    elif content[start:end] != body:
        errors.append(f"{label}: anchor text differs from body")


def audit_export(document: Json) -> Json:
    """Return a JSON-safe evidence report for one authoritative export document.

    The returned ``ok`` means the listed structural checks passed.  It does not
    establish that natural-language claims, source authenticity, or assessment
    judgments are correct.
    """
    errors: list[str] = []
    warnings: list[str] = []
    checks: list[str] = []
    format_version = _as_int(document.get("format_version"), "format_version", errors)
    if document.get("format") != "idea_db.export" or format_version != 1:
        errors.append("export format/version is not idea_db.export v1")
    content = document.get("content")
    if not isinstance(content, dict):
        errors.append("export has no object content")
        return {"ok": False, "checks": checks, "errors": errors, "warnings": warnings}
    declared = document.get("digest")
    actual = canonical_digest(content)
    if declared != actual:
        errors.append(f"export digest mismatch: declared={declared!r} actual={actual}")
    else:
        checks.append("canonical export content digest matches")
    rows = content.get("records")
    if not isinstance(rows, list):
        errors.append("content.records is not an array")
        rows = []
    records: dict[str, Json] = {}
    previous_seq = -1
    seqs: list[int] = []
    for index, row in enumerate(rows):
        if not isinstance(row, dict):
            errors.append(f"records[{index}] is not an object")
            continue
        rid, kind = row.get("id"), row.get("kind")
        if not isinstance(rid, str) or not rid:
            errors.append(f"records[{index}] has invalid id")
            continue
        if rid in records:
            errors.append(f"duplicate application id: {rid}")
        records[rid] = row
        if kind not in KINDS:
            errors.append(f"{rid}: unknown kind {kind!r}")
        if not isinstance(row.get("data"), dict):
            errors.append(f"{rid}: data is not an object")
        seq = _as_int(row.get("seq"), f"{rid}.seq", errors)
        if seq is not None:
            seqs.append(seq)
            if seq < previous_seq:
                errors.append(f"records are not ordered by seq at {rid}")
            previous_seq = max(previous_seq, seq)
    final_seq = _as_int(content.get("seq"), "content.seq", errors)
    if final_seq is not None and seqs and max(seqs) > final_seq:
        errors.append("a record seq exceeds content.seq")
    checks.append("record IDs, kinds, seq bounds, and export ordering inspected")

    # References and source-anchor byte/Unicode-code-point evidence.
    for rid, row in records.items():
        kind, d = row.get("kind"), row.get("data", {})
        if not isinstance(d, dict):
            continue
        if kind == "entity":
            _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
            for ref in d.get("derived_from", []): _ref(records, ref, {"entity"}, rid + ".derived_from", errors)
        elif kind == "revision":
            _ref(records, d.get("entity_id"), {"entity", "project"}, rid + ".entity_id", errors)
            for slot in d.get("slots", []):
                if not isinstance(slot, dict): errors.append(f"{rid}: non-object slot"); continue
                _ref(records, slot.get("revision_id"), {"revision"}, rid + ".slot", errors)
            for field in ("correction_of", "previous_revision_id"):
                if d.get(field) is not None: _ref(records, d[field], {"revision"}, rid + "." + field, errors)
            source = d.get("source", {})
            summary = d.get("summary")
            if summary is not None:
                ss = summary.get("source", {}) if isinstance(summary, dict) else {}
                text = summary.get("text") if isinstance(summary, dict) else None
                if not isinstance(text, str) or not text.strip() or len(text) > 2000:
                    errors.append(f"{rid}: invalid summary text")
                if (not isinstance(ss, dict) or ss.get("origin") != "ai"
                    or ss.get("claim_mode") != "inferred" or ss.get("source_anchor") is not None
                    or not any(isinstance(ss.get(k), str) and ss[k].strip() for k in ("model", "skill"))
                    or not isinstance(source, dict)
                    or not ss.get("capture_id") or ss.get("capture_id") != source.get("capture_id")):
                    errors.append(f"{rid}: invalid summary source")
                if isinstance(ss, dict):
                    _ref(records, ss.get("capture_id"), {"capture"}, rid + ".summary.source.capture_id", errors)
            if isinstance(source, dict):
                capture = source.get("capture_id")
                if capture is not None: _ref(records, capture, {"capture"}, rid + ".source.capture_id", errors)
                if source.get("claim_mode") == "extracted":
                    anchor = source.get("source_anchor")
                    _anchor(records, anchor, d.get("body"), rid + ".source_anchor", errors)
                    if capture is not None and isinstance(anchor, dict) and anchor.get("capture_id") != capture:
                        errors.append(f"{rid}: source capture_id differs from source anchor capture_id")
        elif kind == "capture":
            if d.get("project_id") is not None: _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
        elif kind == "candidate":
            _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
            if d.get("capture_id") is not None: _ref(records, d.get("capture_id"), {"capture"}, rid + ".capture_id", errors)
            if d.get("claim_mode") == "extracted": _anchor(records, d.get("source_anchor"), d.get("body"), rid + ".source_anchor", errors)
        elif kind == "promotion":
            for field, types in (("candidate_id", {"candidate"}), ("entity_id", {"entity"}), ("revision_id", {"revision"})):
                _ref(records, d.get(field), types, rid + "." + field, errors)
        elif kind == "observation":
            _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
            if d.get("target_revision_id") is not None: _ref(records, d.get("target_revision_id"), {"revision"}, rid + ".target_revision_id", errors)
            if d.get("capture_id") is not None: _ref(records, d.get("capture_id"), {"capture"}, rid + ".capture_id", errors)
            for ref in d.get("artifact_ids", []): _ref(records, ref, {"artifact"}, rid + ".artifact_ids", errors)
        elif kind == "goal":
            _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
            scope = d.get("scope", {})
            if isinstance(scope, dict):
                _ref(records, scope.get("root_revision_id"), {"revision"}, rid + ".scope.root", errors)
                _ref(records, scope.get("target_revision_id"), {"revision"}, rid + ".scope.target", errors)
                if _resolve_path(records, scope.get("root_revision_id"), scope.get("slot_path")) != scope.get("target_revision_id"):
                    errors.append(f"{rid}: goal scope path does not resolve to target")
        elif kind == "baseline":
            _ref(records, d.get("goal_id"), {"goal"}, rid + ".goal_id", errors)
            if d.get("supersedes") is not None: _ref(records, d.get("supersedes"), {"baseline"}, rid + ".supersedes", errors)
            goal = records.get(d.get("goal_id"), {}).get("data", {}) if isinstance(d.get("goal_id"), str) else {}
            criteria = {c.get("criterion_id") for c in goal.get("criteria", []) if isinstance(c, dict)} if isinstance(goal, dict) else set()
            selected = d.get("criterion_ids", [])
            if not isinstance(selected, list) or not set(selected) <= criteria:
                errors.append(f"{rid}: baseline criterion_ids are not a subset of its goal")
            if isinstance(d.get("supersedes"), str) and d["supersedes"] in records:
                older = records[d["supersedes"]].get("data", {})
                older_goal = records.get(older.get("goal_id"), {}).get("data", {}) if isinstance(older, dict) else {}
                if isinstance(goal, dict) and isinstance(older_goal, dict) and goal.get("scope") != older_goal.get("scope"):
                    errors.append(f"{rid}: superseded baseline goal scope differs")
        elif kind == "assessment":
            for field, types in (("root_revision_id", {"revision"}), ("target_revision_id", {"revision"}), ("baseline_id", {"baseline"})):
                _ref(records, d.get(field), types, rid + "." + field, errors)
            if _resolve_path(records, d.get("root_revision_id"), d.get("slot_path")) != d.get("target_revision_id"):
                errors.append(f"{rid}: assessment scope path does not resolve to target")
            baseline = records.get(d.get("baseline_id"), {}).get("data", {}) if isinstance(d.get("baseline_id"), str) else {}
            goal_id = baseline.get("goal_id") if isinstance(baseline, dict) else None
            goal = records.get(goal_id, {}).get("data", {}) if isinstance(goal_id, str) else {}
            expected_scope = goal.get("scope") if isinstance(goal, dict) else None
            actual_scope = {"root_revision_id": d.get("root_revision_id"), "slot_path": d.get("slot_path", []), "target_revision_id": d.get("target_revision_id")}
            if isinstance(expected_scope, dict) and expected_scope != actual_scope:
                errors.append(f"{rid}: assessment scope differs from its baseline goal scope")
            selected = set(baseline.get("criterion_ids", [])) if isinstance(baseline, dict) else set()
            result_ids = [x.get("criterion_id") for x in d.get("criteria_results", []) if isinstance(x, dict)]
            if len(result_ids) != len(set(result_ids)) or not set(result_ids) <= selected:
                errors.append(f"{rid}: assessment criteria do not uniquely match baseline")
            cutoff = _as_int(d.get("evidence_cutoff_seq"), rid + ".evidence_cutoff_seq", errors)
            cutoff_at = d.get("evidence_cutoff_at")
            for ref in d.get("evidence_observation_ids", []):
                _ref(records, ref, {"observation"}, rid + ".evidence_observation_ids", errors)
                if ref in records:
                    evidence = records[ref]
                    evidence_seq = _as_int(evidence.get("seq"), ref + ".seq", errors)
                    if cutoff is not None and evidence_seq is not None and evidence_seq > cutoff:
                        errors.append(f"{rid}: evidence observation {ref} is after cutoff seq")
                    occurred = evidence.get("data", {}).get("occurred_at")
                    if not _timestamp_at_or_before(occurred, cutoff_at, rid + f": evidence observation {ref} occurred_at", errors):
                        errors.append(f"{rid}: evidence observation {ref} is after cutoff occurred_at")
            for result in d.get("criteria_results", []):
                estimate = result.get("progress_estimate") if isinstance(result, dict) else None
                if not isinstance(estimate, dict):
                    continue
                for ref in estimate.get("evidence_record_ids", []):
                    _ref(records, ref, {"capture", "revision", "observation"}, rid + ".progress_estimate.evidence_record_ids", errors)
                    if ref not in records:
                        continue
                    evidence = records[ref]
                    evidence_seq = _as_int(evidence.get("seq"), ref + ".seq", errors)
                    if cutoff is not None and evidence_seq is not None and evidence_seq > cutoff:
                        errors.append(f"{rid}: progress evidence {ref} is after cutoff seq")
                    if not _timestamp_at_or_before(evidence.get("recorded_at"), cutoff_at, rid + f": progress evidence {ref} recorded_at", errors):
                        errors.append(f"{rid}: progress evidence {ref} is after cutoff recorded_at")
                    if evidence.get("kind") == "observation" and not _timestamp_at_or_before(evidence.get("data", {}).get("occurred_at"), cutoff_at, rid + f": progress observation {ref} occurred_at", errors):
                        errors.append(f"{rid}: progress observation {ref} is after cutoff occurred_at")
        elif kind == "link":
            _ref(records, d.get("from_id"), None, rid + ".from_id", errors)
            _ref(records, d.get("to_id"), None, rid + ".to_id", errors)
            for ref in (d.get("impact") or {}).get("observation_ids", []) if isinstance(d.get("impact"), dict) else []:
                _ref(records, ref, {"observation"}, rid + ".impact.observation_ids", errors)
        elif kind == "embedding":
            _ref(records, d.get("revision_id"), {"revision"}, rid + ".revision_id", errors)
        elif kind == "head_change":
            _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
            _ref(records, d.get("after_revision_id"), {"revision"}, rid + ".after_revision_id", errors)
        elif kind == "publication":
            _ref(records, d.get("project_id"), {"project"}, rid + ".project_id", errors)
            _ref(records, d.get("root_revision_id"), {"revision"}, rid + ".root_revision_id", errors)
    checks.append("record reference types, source anchors, scope paths, and temporal cutoff sequence inspected")

    # Revision graph acyclicity; all edges are fixed revision slots.
    graph = {rid: [s.get("revision_id") for s in row.get("data", {}).get("slots", []) if isinstance(s, dict) and isinstance(s.get("revision_id"), str)] for rid, row in records.items() if row.get("kind") == "revision"}
    visiting, visited = set(), set()
    def visit(node: str) -> None:
        if node in visiting: errors.append(f"revision slot graph has cycle through {node}"); return
        if node in visited: return
        visiting.add(node)
        for child in graph.get(node, []): visit(child)
        visiting.remove(node); visited.add(node)
    for node in graph: visit(node)
    checks.append("revision slot DAG inspected")

    heads = content.get("heads", [])
    if not isinstance(heads, list): errors.append("content.heads is not an array"); heads = []
    seen_head_projects: set[str] = set()
    for head in heads:
        if not isinstance(head, dict): errors.append("non-object head"); continue
        project = head.get("project_id")
        _ref(records, project, {"project"}, "head.project_id", errors)
        if isinstance(project, str) and project in seen_head_projects: errors.append(f"duplicate head row for project {project}")
        if isinstance(project, str): seen_head_projects.add(project)
        for field in ("working_head", "official_head"):
            revision_id = head.get(field)
            if revision_id is not None:
                _ref(records, revision_id, {"revision"}, "head." + field, errors)
                revision = records.get(revision_id)
                if isinstance(revision, dict) and revision.get("data", {}).get("entity_id") != project:
                    errors.append(f"head.{field}: revision {revision_id} is not rooted at project {project}")
    checks.append("export head projections inspected")

    manifest = content.get("manifest", {})
    if isinstance(manifest, dict):
        artifact_ids = {rid for rid, row in records.items() if row.get("kind") == "artifact"}
        unresolved = set(manifest.get("unresolved_external_artifacts", []))
        if unresolved != artifact_ids: errors.append("manifest unresolved artifact IDs differ from exported artifacts")
        if artifact_ids and manifest.get("complete_backup") is not False: errors.append("manifest claims complete backup despite external artifacts")
        if not artifact_ids and manifest.get("complete_backup") is False and int(manifest.get("receipts_omitted", 0) or 0) == 0: warnings.append("incomplete backup without artifacts or omitted receipts")
    else: errors.append("content.manifest is not an object")
    checks.append("backup manifest artifact disclosure inspected")

    receipt_keys = Counter()
    for receipt in content.get("receipts", []) if isinstance(content.get("receipts"), list) else []:
        if not isinstance(receipt, dict): errors.append("non-object receipt"); continue
        key = receipt.get("idempotency_key")
        if not isinstance(key, str): errors.append("receipt missing idempotency key")
        else: receipt_keys[key] += 1
    for key, count in receipt_keys.items():
        if count > 1: errors.append(f"duplicate idempotency receipt key {key}")
    checks.append("receipt key uniqueness inspected")
    return {"ok": not errors, "checks": checks, "errors": errors, "warnings": warnings,
            "counts": {"records": len(records), "revisions": len(graph), "heads": len(heads), "receipts": sum(receipt_keys.values())}}


def run_export_audit(base_url: str, output_dir: str | Path, *, token: str | None = None,
                     timeout: float = 30.0) -> Json:
    """Fetch the authoritative full export, audit it, and save exclusive evidence."""
    document = _get_export(base_url, token, timeout)
    report = audit_export(document)
    report["audit_kind"] = "read_only_structural_export_audit"
    report["audited_at"] = dt.datetime.now(dt.timezone.utc).isoformat(timespec="milliseconds")
    report["base_url"] = base_url.rstrip("/")
    report["export_digest"] = document.get("digest")
    report["limitations"] = [
        "This validates exported record structure and references; it does not prove natural-language semantics, source authenticity, or assessment correctness.",
        "It does not compare Neo4j relationship projections directly; export records are the authoritative API evidence used here.",
        "It does not mutate data or prove concurrent write behavior; acceptance.py exercises CAS and atomic rejection separately.",
        "A project-scoped export may include cross-project reference closure while listing a head only for the selected project; absent heads for referenced origin projects are therefore not errors.",
    ]
    out = Path(output_dir)
    out.mkdir(parents=True, exist_ok=True)
    path = out / f"integrity-audit-{dt.datetime.now(dt.timezone.utc):%Y%m%dT%H%M%SZ}-{uuid.uuid4().hex[:10]}.json"
    # x makes each artifact exclusive: an existing result is never overwritten.
    report["evidence_path"] = str(path)
    # json.dump treats JsonNumber (a str subclass) as a JSON string.  Write
    # through the canonical encoder so the embedded authoritative export keeps
    # its number spellings and can be audited/digest-checked after reload.
    with path.open("x", encoding="utf-8") as fh:
        fh.write(canonical_json({"report": report, "export": document}))
        fh.write("\n")
    return report


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--token", default=os.environ.get("IDEA_DB_TOKEN"))
    parser.add_argument("--timeout", type=float, default=30.0)
    args = parser.parse_args(argv)
    try:
        report = run_export_audit(args.base_url, args.output_dir, token=args.token, timeout=args.timeout)
    except AuditFailure as exc:
        print(f"integrity audit failed to run: {exc}", file=sys.stderr)
        return 2
    print(json.dumps({k: report[k] for k in ("ok", "counts", "evidence_path", "errors", "warnings")}, ensure_ascii=False))
    return 0 if report["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
