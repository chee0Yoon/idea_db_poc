#!/usr/bin/env python3
"""Audit recorded model responses without silently repairing their JSON.

Only literal source coverage/format are automated. These are diagnostic proxies,
not a semantic judge. Human review decisions are separate, explicit artifacts.
"""
import argparse
import hashlib
import json
from pathlib import Path

from evidence_corpus import ATOMIZATION_CASES
from context_evidence import save


def audit_response(text):
    try:
        value = json.loads(text)
    except json.JSONDecodeError as exc:
        return {"strict_json": False, "error": str(exc), "cases": []}
    if not isinstance(value, dict) or set(value) != {"cases"} or not isinstance(value["cases"], list):
        return {"strict_json": True, "schema_valid": False, "cases": [], "error": "Expected {cases: [...]}"}
    source_by_id = {case["id"]: case for case in ATOMIZATION_CASES}
    ids = [case.get("id") for case in value["cases"] if isinstance(case, dict)]
    if len(ids) != len(set(ids)) or set(ids) != set(source_by_id):
        return {"strict_json": True, "schema_valid": False, "cases": [], "error": "Missing/duplicate/unknown case IDs"}
    rows = []
    for case in value["cases"]:
        expected = source_by_id[case["id"]]
        source = expected["source"]
        atoms = case.get("atoms", [])
        if not isinstance(atoms, list) or not atoms or any(
            not isinstance(atom, dict) or set(atom) != {"body", "quotes", "status"}
            or not isinstance(atom["body"], str) or not atom["body"].strip()
            or atom["status"] not in ("accepted", "pending")
            or not isinstance(atom["quotes"], list) or not atom["quotes"]
            or any(not isinstance(q, str) or not q for q in atom["quotes"]) for atom in atoms):
            return {"strict_json": True, "schema_valid": False, "cases": rows, "error": "Invalid atom contract"}
        covered, invalid = set(), []
        for i, atom in enumerate(atoms):
            for quote in atom["quotes"]:
                if quote not in source:
                    invalid.append({"atom": i, "quote": quote})
                    continue
                # All matching spans are reported; repeated quotations do not gain
                # extra coverage. The fixture does not assume a unique occurrence.
                start = source.find(quote)
                while start >= 0:
                    covered.update(range(start, start+len(quote)))
                    start = source.find(quote, start+1)
        significant = {i for i, char in enumerate(source) if not char.isspace()}
        quoted = " ".join(q for a in atoms for q in a["quotes"])
        guards = expected["units"]
        rows.append({"id": case["id"], "atom_count": len(atoms), "invalid_quotes": invalid,
            "source_character_coverage": len(significant & covered) / len(significant),
            "source_guard_groups_cited": sum(all(term in quoted for term in group) for group in guards),
            "source_guard_group_count": len(guards),
            "unresolved_pending": any(a["status"] == "pending" for a in atoms) if expected.get("unresolved") else None,
            "body_equals_one_quote_count": sum(a["body"] in a["quotes"] for a in atoms),
            "atoms": atoms})
    return {"strict_json": True, "schema_valid": True, "cases": rows,
            "atom_count": sum(r["atom_count"] for r in rows),
            "quote_error_count": sum(len(r["invalid_quotes"]) for r in rows),
            "mean_source_character_coverage": sum(r["source_character_coverage"] for r in rows)/len(rows)}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--response", action="append", required=True, help="label=path; no automatic repair")
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    args.output_dir.mkdir(parents=True, exist_ok=False)
    report = {"responses": {}, "limits": [
        "Source quotation coverage is not semantic correctness or independently reusable atomicity.",
        "A correct quote does not guarantee that the inferred body is entailed.",
        "Twelve synthetic paragraphs, one initial response per provider; not population quality estimates.",
        "Malformed output is retained and counted; edited/repaired responses are separate attempts."]}
    for entry in args.response:
        label, filename = entry.split("=", 1)
        raw = Path(filename).read_text("utf-8")
        result = audit_response(raw)
        result["input_sha256"] = hashlib.sha256(raw.encode()).hexdigest()
        report["responses"][label] = result
    save(args.output_dir / "report.json", report)
    print(json.dumps({label: {k: v for k, v in r.items() if k not in ("cases",)}
                      for label, r in report["responses"].items()}, ensure_ascii=False))


if __name__ == "__main__":
    main()
