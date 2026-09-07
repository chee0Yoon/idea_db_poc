#!/usr/bin/env python3
"""Real MCP hybrid retrieval, fixed-budget context ablation and temporal replay.

This measures evidence availability, not generated-answer accuracy. Raw document
controls are explicitly tagged non-atomic benchmark Ideas so both representations
use the identical production index/ranker. They never enter atom ranking.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import statistics
import time
import uuid
from pathlib import Path

from acceptance import (Api, entity_record, expected_head, package, project_record,
                        require, revision_record, root_change, slot)
from evidence_corpus import PHASES, ROLES, documents, questions
from lifecycle import capture_record


def save(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as file:
        json.dump(value, file, ensure_ascii=False, indent=2, sort_keys=True)
        file.write("\n")


def digest(value):
    return hashlib.sha256(json.dumps(value, ensure_ascii=False, sort_keys=True).encode()).hexdigest()


def scoped_source(capture, text, quote):
    start = text.index(quote)
    return {"origin": "human", "capture_id": capture, "claim_mode": "extracted",
            "source_anchor": {"capture_id": capture, "start": start, "end": start + len(quote)}}


def install(api, out):
    prefix = "ev" + uuid.uuid4().hex[:12]
    project = prefix + "_project"
    api.post("/api/packages/apply", package(prefix + "_create", "Create isolated evidence benchmark",
             expected_head(project, None), [project_record(project, "원자화·방향·맥락 비교 실험")]))
    installed, checkpoints, roots = [], [], []
    previous, shared = None, {}
    for version in range(1, 4):
        records, schema_slots = [], []
        current_docs = []
        for doc in [d for d in documents() if d["version"] == version]:
            role, key = doc["role"], doc["key"]
            capture = api.post("/api/captures", {"project_id": project, "content": doc["text"],
                "source_kind": "note", "source_ref": "evidence-corpus/" + key,
                "occurred_at": f"2026-08-0{version}T09:00:00+09:00"})
            cap_id = capture.get("capture", capture).get("id") or capture.get("record", {}).get("id")
            require(cap_id, f"capture id missing: {capture.keys()}")
            schema, core = prefix + "_schema_" + role, prefix + "_core_" + role
            if version == 1:
                records.extend([entity_record(schema, project, "schema", role),
                                entity_record(core, project, "core", role + " 검토 규칙")])
            atom_ids = []
            for number, body in enumerate(doc["atoms"]):
                shared_key = (role, body)
                if shared_key in shared:
                    atom_ids.append(shared[shared_key])
                    continue
                atom = prefix + "_" + key + "_a" + str(number)
                ent = atom + "_entity"
                kwargs = {}
                if role == "be" and number == 0 and version > 1:
                    kwargs = {"derived_from": [prefix + f"_v{version-1}_be_a0_entity"],
                              "lineage_kind": "semantic_edit"}
                records.extend([entity_record(ent, project, "idea", role + " 규칙", **kwargs),
                                revision_record(atom, ent, body, tags=["evidence-atom"],
                                                source=scoped_source(cap_id, doc["text"], body))])
                shared[shared_key] = atom
                atom_ids.append(atom)
            raw = prefix + "_" + key + "_document"
            records.extend([entity_record(raw + "_entity", project, "idea", role + " 규칙"),
                revision_record(raw, raw + "_entity", doc["text"], tags=["evidence-document-control"],
                                source=scoped_source(cap_id, doc["text"], doc["text"]))])
            core_rev = prefix + "_" + key + "_core"
            schema_rev = prefix + "_" + key + "_schema"
            previous_core = prefix + f"_v{version-1}_{role}_core" if version > 1 else None
            previous_schema = prefix + f"_v{version-1}_{role}_schema" if version > 1 else None
            records.extend([
                revision_record(core_rev, core, doc["context"],
                    slots=[slot("a" + str(n), r, role) for n, r in enumerate(atom_ids)],
                    change_kind="composition" if version > 1 else "initial",
                    previous_revision_id=previous_core,
                    source=scoped_source(cap_id, doc["text"], doc["context"])),
                revision_record(schema_rev, schema, role + " 업무", slots=[slot("rules", core_rev, role),
                                slot("document_control", raw, role)],
                    change_kind="composition" if version > 1 else "initial", previous_revision_id=previous_schema)])
            schema_slots.append(slot(role, schema_rev, role))
            current_docs.append({**doc, "capture": cap_id, "atom_ids": atom_ids,
                                 "raw_id": raw, "core_id": core_rev})
        if version == 1:
            by_role = {d["role"]: d for d in current_docs}
            records.append({"id": prefix + "_approval_dependency", "kind": "link", "data": {
                "link_type": "depends", "from_id": by_role["fe"]["atom_ids"][1] + "_entity",
                "to_id": by_role["be"]["atom_ids"][1] + "_entity", "actor": "human:evidence-fixture",
                "note": "UI 승인 통제는 BE 권한 검사와 함께 적용해야 한다."}})
        root = prefix + f"_root_v{version}"
        records.append(revision_record(root, project, PHASES[version-1][2], slots=schema_slots,
            change_kind="composition" if previous else "initial", previous_revision_id=previous))
        proposal = package(prefix + f"_v{version}", PHASES[version-1][2], expected_head(project, previous), records,
            root_change=root_change(project, root, PHASES[version-2][0] if previous else "미설정",
                PHASES[version-1][2] + " 공통 승인·감사 규칙은 재사용하고 BE 재시도 시간은 새 원자로 파생한다."))
        save(out / f"package-v{version}.json", proposal)
        # Preview budgets the worst-case embedding dimensions, so split the
        # dependency-ordered records conservatively. Publish the root only after
        # every referenced revision is durable; earlier batches leave head intact.
        for offset in range(0, len(records), 30):
            batch = records[offset:offset+30]
            final = offset + 30 >= len(records)
            packet = package(prefix + f"_v{version}_batch{offset//30}", proposal["reason"],
                expected_head(project, previous), batch,
                root_change=proposal["root_change"] if final else None)
            save(out / f"package-v{version}-batch{offset//30}.json", packet)
            result = api.post("/api/packages/apply", packet)
            save(out / f"apply-v{version}-batch{offset//30}.json", result)
        save(out / f"apply-v{version}.json", result)
        state = api.get("/api/state", project_id=project)
        exported = api.get("/api/export", project_id=project)
        seq = exported["content"]["seq"]
        checkpoints.append({"version": version, "root": root, "seq": seq,
                            "state": state, "snapshot": api.get("/api/snapshot", project_id=project,
                                                               root_revision_id=root, known_seq=seq)})
        roots.append(root)
        installed.extend(current_docs)
        previous = root
        print(f"PASS evidence direction v{version}: six roles, explicit reuse and numeric derivation", flush=True)
    manifest = {"project": project, "prefix": prefix, "documents": installed,
                "checkpoints": checkpoints, "roots": roots}
    save(out / "manifest.json", manifest)
    return manifest


def clip_utf8(text, budget):
    return text.encode("utf-8")[:budget].decode("utf-8", errors="ignore")


def pack(segments, budget):
    """Rank order, skip duplicate record IDs, truncate the final unit only.

    Include identifiers/roles/type labels in all arms and charge their bytes.
    Never inspect gold, query category or required IDs here.
    """
    text, seen, used = "", set(), []
    for seg in segments:
        if seg["id"] in seen:
            continue
        seen.add(seg["id"])
        header = f"[{seg['id']} | {seg['role']} | {seg['kind']}] "
        rendered = header + seg['text'] + "\n"
        remaining = budget - len(text.encode())
        if remaining <= 0:
            break
        piece = clip_utf8(rendered, remaining)
        text += piece
        if piece:
            used.append({"id": seg["id"], "complete": piece == rendered,
                         "visible_body": piece[len(header):] if piece.startswith(header) else ""})
    return text, used


def score(text, used, facts, required):
    # Full exact source statements count; a clipped number/exception gets no credit.
    # Identical statements have multiple occurrence labels: score only the expected
    # version's facts (scope is checked independently against the API snapshot).
    visible = "\n".join(u.get("visible_body", "") for u in used)
    present = {key for key, body in facts.items() if body in visible}
    needed = set(required)
    return {"required_recall": len(present & needed) / len(needed),
            "all_required_present": needed <= present,
            "required_present": sorted(present & needed), "required_missing": sorted(needed - present),
            "included_fact_count": len(present),
            "context_bytes": len(text.encode()), "units": used}


def evaluate(api, out, manifest, replay=None):
    rows, profiles = [], set()
    docs = manifest["documents"]
    exported = (json.loads((replay / "before-export.json").read_text()) if replay else
                api.get("/api/export", project_id=manifest["project"]))
    stored = {r["id"]: r for r in exported["content"]["records"]}
    for q in questions():
        checkpoint = manifest["checkpoints"][q["version"]-1]
        selected_docs = [d for d in docs if d["version"] == q["version"]]
        atom_lookup = {r: (d, i) for d in selected_docs for i, r in enumerate(d["atom_ids"])}
        raw_lookup = {d["raw_id"]: d for d in selected_docs}
        facts = {d["key"] + "_context": d["context"] for d in selected_docs}
        facts.update({d["key"] + "_a" + str(i): body for d in selected_docs
                      for i, body in enumerate(d["atoms"])})
        require(set(q["required"]) <= facts.keys(), "Unknown gold fact")
        distinct = set(facts.values())
        require(not any(a != b and a in b for a in distinct for b in distinct),
                "Overlapping gold text requires an explicit semantic scorer")
        for mode in ("lexical", "hybrid"):
            results = {}
            for arm, tag in (("raw_document", "evidence-document-control"), ("flat_atoms", "evidence-atom")):
                query = {"project_id": manifest["project"], "root_revision_id": checkpoint["root"],
                         "known_seq": checkpoint["seq"], "query": q["query"], "roles": [q["role"]],
                         "tags": [tag], "limit": 3, "scope": "snapshot", "embedding_mode": mode}
                filename = q["id"] + "-" + mode + "-" + arm + ".json"
                if replay:
                    saved = json.loads((replay / "context" / "queries" / filename).read_text())
                    require(saved["request"] == query, "Replay query differs from recorded request")
                    result, elapsed = saved["response"], saved["elapsed_ms"]
                else:
                    started = time.perf_counter()
                    result = api._mcp("idea_search", {"body": query})
                    elapsed = (time.perf_counter()-started)*1000
                if mode == "hybrid":
                    require(result["index_coverage"]["complete"], "Hybrid index incomplete")
                    profiles.add(result["embedding_profile"])
                save(out / "queries" / filename,
                     {"request": query, "response": result, "elapsed_ms": elapsed})
                results[arm] = result
            raw_segments, flat_segments, graph_segments, parent_segments, window_segments = [], [], [], [], []
            for hit in results["raw_document"]["results"]:
                require(hit["revision_id"] in raw_lookup, "Wrong-root raw result")
                doc = raw_lookup[hit["revision_id"]]
                require(doc["role"] == q["role"], "Direct raw role leakage")
                raw_segments.append({"id": doc["key"] + "_raw", "role": doc["role"],
                                     "kind": "document", "text": stored[hit["revision_id"]]["data"]["body"]})
            for hit in results["flat_atoms"]["results"]:
                require(hit["revision_id"] in atom_lookup, "Wrong-root atom result")
                doc, number = atom_lookup[hit["revision_id"]]
                require(doc["role"] == q["role"], "Direct atom role leakage")
                segment = {"id": doc["key"] + "_a" + str(number), "role": doc["role"],
                           "kind": "idea", "text": stored[hit["revision_id"]]["data"]["body"]}
                flat_segments.append(segment)
                graph_segments.append(segment)
                parent_segments.append(segment)
                window_segments.append(segment)
                # Exact occurrence parent comes from the stored snapshot path.
                parent = stored[doc["core_id"]]["data"]
                require(parent["slots"][number]["revision_id"] == hit["revision_id"], "Stored parent differs from occurrence")
                context_segment = {"id": doc["key"] + "_context", "role": doc["role"],
                                   "kind": "parent", "text": parent["body"]}
                graph_segments.append(context_segment)
                parent_segments.append(context_segment)
                # Matched-source-window control: identical ranked atom is the
                # locator, then include adjacent source lines. This isolates
                # source expansion from graph expansion; not another ranker.
                # Use the selected occurrence's source document. A reused atom's
                # original capture may belong to an older product direction.
                source_lines = stored[doc["raw_id"]]["data"]["body"].splitlines()
                line = source_lines.index(segment["text"])
                for near in (line-1, line+1):
                    if 0 <= near < len(source_lines):
                        window_segments.append({"id": f"{doc['key']}_source_line{near}", "role": doc["role"],
                                                "kind": "source", "text": source_lines[near]})
                # One-hop explicit dependencies only. No guessed semantic edges.
                for related in results["flat_atoms"].get("related", []):
                    other_id = related.get("revision_id")
                    if other_id not in atom_lookup or related.get("link_type") != "depends":
                        continue
                    other, n = atom_lookup[other_id]
                    graph_segments.append({"id": other["key"] + "_a" + str(n), "role": other["role"],
                                           "kind": "related:depends", "text": stored[other_id]["data"]["body"]})
            for budget in (256, 512, 1024):
                for arm, segments in (("raw_document", raw_segments), ("flat_atoms", flat_segments),
                                      ("source_window", window_segments), ("parent_only", parent_segments),
                                      ("graph_context", graph_segments)):
                    text, used = pack(segments, budget)
                    rows.append({"query_id": q["id"], "category": q["category"], "version": q["version"],
                        "role": q["role"], "mode": mode, "arm": arm, "budget_bytes": budget,
                        "direct_candidate_count": len([r for d in selected_docs if d["role"] == q["role"] for r in d["atom_ids"]]),
                        "future_or_wrong_root_hits": 0, "strict_role_violations": 0,
                        "context": text, **score(text, used, facts, q["required"])})
        print("Measured " + q["id"], flush=True)
    summaries = []
    for mode in ("lexical", "hybrid"):
        for budget in (256, 512, 1024):
            for arm in ("raw_document", "flat_atoms", "source_window", "parent_only", "graph_context"):
                for category in ("direct", "direction", "cross_role"):
                    group = [r for r in rows if (r["mode"], r["budget_bytes"], r["arm"], r["category"]) == (mode, budget, arm, category)]
                    summaries.append({"mode": mode, "budget_bytes": budget, "arm": arm, "category": category, "n": len(group),
                        "required_recall": statistics.mean(r["required_recall"] for r in group),
                        "all_required_rate": statistics.mean(r["all_required_present"] for r in group),
                        "mean_bytes": statistics.mean(r["context_bytes"] for r in group)})
    report = {"status": "measured", "corpus_digest": digest(documents()), "questions_digest": digest(questions()),
              "query_count": len(questions()), "observations": rows, "summary": summaries,
              "embedding_profiles": sorted(profiles),
              "reanalysis_of": str(replay) if replay else None,
              "analysis_pre_registered": replay is None,
              "document_utf8_bytes": {d["key"]: len(d["text"].encode()) for d in docs},
              "limits": ["Synthetic authored source and atom decomposition, not production generalization.",
                         "Context evidence coverage is not LLM answer accuracy or semantic entailment.",
                         "Raw documents use explicitly non-atomic benchmark Idea controls, same MCP ranker.",
                         "Byte budgets are UTF-8, not model token counts; labels count against the budget.",
                         "Graph assembly is an experimental client, not a deployed context API.",
                         "Raw has one document per role: no raw-document selection difficulty is measured.",
                         "Source window shares the atom ranker; it is a context expansion control, not an independent search system.",
                         "Cross-role facts are deliberately unreachable through strict-role direct results; interpret separately.",
                         "39 queries reuse six role topics across three roots; observations are correlated.",
                         "No significance claim; report loses/ties as well as improvements."]}
    save(out / "report.json", report)
    return report


def verify(api, manifest):
    for checkpoint in manifest["checkpoints"]:
        current = api.get("/api/snapshot", project_id=manifest["project"], root_revision_id=checkpoint["root"],
                          known_seq=checkpoint["seq"])
        require(current == checkpoint["snapshot"], "Historical snapshot changed after restart/restore")
    return {"status": "passed", "historical_roots": 3, "project": manifest["project"]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--output-dir", type=Path)
    parser.add_argument("--verify-manifest", type=Path)
    parser.add_argument("--replay-dir", type=Path, help="Offline context reanalysis of an intact evidence run")
    args = parser.parse_args()
    api = Api(args.base_url, os.environ.get("IDEA_DB_TOKEN"), 30)
    if args.verify_manifest:
        print(json.dumps(verify(api, json.loads(args.verify_manifest.read_text())), ensure_ascii=False))
        return
    require(args.output_dir is not None, "--output-dir required")
    args.output_dir.mkdir(parents=True, exist_ok=False)
    save(args.output_dir / "pre-registered.json", {"documents": documents(), "questions": questions(),
                                                 "budgets": [256, 512, 1024]})
    try:
        if args.replay_dir:
            original = json.loads((args.replay_dir / "context" / "pre-registered.json").read_text())
            require(original["documents"] == documents() and original["questions"] == questions(), "Replay corpus changed")
            manifest = json.loads((args.replay_dir / "context" / "manifest.json").read_text())
            evaluate(None, args.output_dir, manifest, args.replay_dir)
            return
        manifest = install(api, args.output_dir)
        report = evaluate(api, args.output_dir, manifest)
        print(json.dumps({"status": report["status"], "queries": report["query_count"]}))
    except Exception as exc:
        save(args.output_dir / "failure.json", {"type": type(exc).__name__, "message": str(exc)})
        raise


if __name__ == "__main__":
    main()
