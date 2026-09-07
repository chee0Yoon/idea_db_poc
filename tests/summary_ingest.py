#!/usr/bin/env python3
"""Live MCP regression for optional Revision summaries and summary-aware search.

Requires a running isolated idea-db/Neo4j stack and IDEA_DB_MCP_COMMAND_JSON.
The fixture is deliberately small and uses new globally-unique IDs; it never
creates containers or modifies existing records.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import uuid
from pathlib import Path
from typing import Any, Callable

from acceptance import (
    AcceptanceFailure, Api, ApiError, entity_record, expected_head, package,
    project_record, require, revision_record, root_change, slot,
)


Json = dict[str, Any]


def write_json(path: Path, value: Json) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n", "utf-8")


def mcp(api: Api, tool: str, body: Json) -> Json:
    return api._mcp(tool, {"body": body})


def expect_error(action: Callable[[], Any], label: str) -> ApiError:
    try:
        action()
    except ApiError as exc:
        require(exc.status in {400, 422}, f"{label}: unexpected status {exc.status}: {exc.body!r}")
        return exc
    raise AcceptanceFailure(f"{label}: request unexpectedly succeeded")


def ai_source(capture_id: str) -> Json:
    return {
        "origin": "ai", "capture_id": capture_id, "model": "summary-ingest-fixture",
        "skill": None, "claim_mode": "inferred", "source_anchor": None,
    }


def summary(text: str, capture_id: str) -> Json:
    return {"text": text, "source": ai_source(capture_id)}


def with_summary(record: Json, text: str, capture_id: str) -> Json:
    record["data"]["summary"] = summary(text, capture_id)
    return record


def preview(api: Api, raw: Json) -> Json:
    result = mcp(api, "idea_upload_preview", {"package": raw})
    require(isinstance(result.get("upload_id"), str), f"preview upload_id missing: {result!r}")
    require(isinstance(result.get("prepared_digest"), str), f"preview digest missing: {result!r}")
    require(result.get("validation", {}).get("valid") is True, f"preview invalid: {result!r}")
    return result


def apply(api: Api, staged: Json) -> Json:
    return mcp(api, "idea_package_apply", {
        "upload_id": staged["upload_id"], "prepared_digest": staged["prepared_digest"],
    })


def rank(result: Json, revision_id: str) -> int | None:
    for position, hit in enumerate(result.get("results", []), 1):
        if hit.get("revision_id") == revision_id:
            return position
    return None


def canonical_json_chars(value: Any) -> int:
    """Match the contract's Unicode-character charge for a JSON packet."""
    return len(json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":")))


def search(api: Api, project_id: str, query: str, **extra: Any) -> Json:
    body: Json = {
        "project_id": project_id, "scope": "snapshot", "query": query,
        "stage": "working", "roles": [], "tags": [], "lanes": ["official"],
        "limit": 20, "embedding_mode": "hybrid",
    }
    body.update(extra)
    return mcp(api, "idea_search", body)


def run(base_url: str, timeout: float, output_dir: Path) -> Json:
    api = Api(base_url, os.environ.get("IDEA_DB_TOKEN"), timeout)
    # Preserve every live request/result as it happens.  The counter makes a
    # partial failure auditable without overwriting any earlier evidence.
    request_number = 0
    original_mcp, original_get = api._mcp, api.get
    def record(kind: str, request: Json, action: Callable[[], Json]) -> Json:
        nonlocal request_number
        request_number += 1
        path = output_dir / "requests" / f"{request_number:03d}-{kind}.json"
        try:
            response = action()
        except ApiError as exc:
            write_json(path, {"request": request, "error": {"status": exc.status, "body": exc.body}})
            raise
        write_json(path, {"request": request, "response": response})
        return response
    api._mcp = lambda tool, arguments: record("mcp-" + tool, {"tool": tool, "arguments": arguments},
                                               lambda: original_mcp(tool, arguments))
    api.get = lambda path, **params: record("http-get", {"path": path, "params": params},
                                             lambda: original_get(path, **params))
    prefix = "si" + uuid.uuid4().hex[:18]
    project, capture = f"{prefix}_project", f"{prefix}_capture"
    # Paired, equally-sized retrieval cohorts.  Pair 0 is intentionally an
    # incomplete legacy/pronoun input; the remaining rows retain concrete body
    # conditions and use a summary only to add retrieval synonyms.
    pairs = [
        ("legacy_pronoun", "그 경우에는 이를 하지 않고, 그 뒤에만 다시 한다.", "야간 배포 중에는 실패 알림을 억제하고 승인 뒤에만 알림을 재개한다."),
        ("deploy_alert", "야간 배포가 진행 중이면 실패 알림을 억제하고 승인 후 재개한다.", "night deployment suppresses failure notifications until approval."),
        ("rollback", "롤백 중에는 고객 알림을 보내지 않으며 검증 완료 뒤에만 발송한다.", "rollback verification gates customer notification delivery."),
        ("maintenance", "정기 점검 창에서는 경보를 묵음 처리하고 종료 확인 후 해제한다.", "maintenance window silences alerts until completion confirmation."),
        ("incident", "심각도 1 사고에서는 자동 재시작을 하지 않고 운영 책임자 승인 뒤에만 실행한다.", "severity-one incident restart requires operator approval."),
        ("privacy", "개인정보 삭제 요청은 법정 보존 예외를 확인한 뒤에만 영구 삭제한다.", "retention exception check precedes permanent privacy deletion."),
    ]
    capture_text = "조건부 알림 규칙 원문: 야간 배포 중에는 실패 알림을 억제한다. 승인 뒤에만 재개한다.\n" + "\n".join(body for _, body, _ in pairs[1:])
    created = mcp(api, "idea_capture_create", {
        "id": capture, "project_id": None, "content": capture_text,
        "media_type": "text/plain", "source_kind": "note", "source_ref": "fixture://summary-ingest",
        "occurred_at": "2026-09-07T00:00:00Z", "content_digest": None,
    })
    require(created.get("capture", {}).get("id") == capture, f"capture was not created: {created!r}")

    ids = {name: f"{prefix}_{name}" for name in (
        "schema_entity", "schema_revision", "core_entity", "core_revision", "root", "goal",
    )}
    pair_ids: list[Json] = []
    pair_records: list[Json] = []
    schema_slots: list[Json] = []
    for index, (label, body, text) in enumerate(pairs):
        plain_entity, plain_revision = f"{prefix}_{label}_plain_entity", f"{prefix}_{label}_plain_revision"
        summary_entity, summary_revision = f"{prefix}_{label}_summary_entity", f"{prefix}_{label}_summary_revision"
        pair_ids.append({"label": label, "plain_revision": plain_revision, "summary_revision": summary_revision})
        pair_records.extend([
            entity_record(plain_entity, project, "idea", "운영 규칙", tags=["summary-paired"]),
            revision_record(plain_revision, plain_entity, body, tags=["summary-plain"], source=ai_source(capture)),
            entity_record(summary_entity, project, "idea", "운영 규칙", tags=["summary-paired"]),
            with_summary(revision_record(summary_revision, summary_entity, body, tags=["summary-augmented"] + (["summary-shared"] if index == 0 else []), source=ai_source(capture)), text, capture),
        ])
        schema_slots.append(slot(f"pair_{index}_plain", plain_revision, "be"))
        schema_slots.append(slot(f"pair_{index}_summary", summary_revision, "be"))
    primary = pair_ids[0]
    ids["plain_revision"], ids["summary_revision"] = primary["plain_revision"], primary["summary_revision"]
    summary_text = pairs[0][2]
    enriched = next(row for row in pair_records if row.get("id") == ids["summary_revision"])
    records = [
        project_record(project, "Summary ingest integration"),
        entity_record(ids["schema_entity"], project, "schema", "운영 규칙"),
        entity_record(ids["core_entity"], project, "core", "운영 규칙"),
        *pair_records,
        revision_record(ids["core_revision"], ids["core_entity"], "알림 운용", source=ai_source(capture),
                        slots=[slot("shared_summary", ids["summary_revision"], "fe")]),
        revision_record(ids["schema_revision"], ids["schema_entity"], "운영 스키마", source=ai_source(capture),
                        slots=[*schema_slots, slot("summary_direct", ids["summary_revision"], "be"),
                               slot("core", ids["core_revision"], "be")]),
        revision_record(ids["root"], project, "운영 프로젝트", source=ai_source(capture),
                        slots=[slot("operations", ids["schema_revision"], "ops")]),
    ]
    initial = package(f"{prefix}_initial", "install summary and body-only comparison fixture",
                      expected_head(project, None), records,
                      root_change=root_change(project, ids["root"], "none", "install summary fixture"))
    staged = preview(api, initial)
    inputs = staged.get("embedding_inputs")
    require(isinstance(inputs, list) and len(inputs) == 12, f"preview embedding_inputs missing: {inputs!r}")
    by_revision = {item.get("revision_id"): item for item in inputs if isinstance(item, dict)}
    expected_revisions = {item[key] for item in pair_ids for key in ("plain_revision", "summary_revision")}
    require(set(by_revision) == expected_revisions, f"wrong embedding inputs: {inputs!r}")
    require(by_revision[ids["plain_revision"]].get("profile", "").endswith("/idea-body-v1"), "body-only profile changed")
    require(by_revision[ids["plain_revision"]].get("includes_summary") is False, "body-only input claims summary")
    require(by_revision[ids["summary_revision"]].get("profile", "").endswith("/idea-body-summary-v2"), "summary profile missing")
    require(by_revision[ids["summary_revision"]].get("includes_summary") is True, "summary input not marked")
    preview_profiles = staged.get("embedding_profiles")
    require(isinstance(preview_profiles, list)
            and any(str(profile).endswith("/idea-body-v1") for profile in preview_profiles)
            and any(str(profile).endswith("/idea-body-summary-v2") for profile in preview_profiles),
            f"preview did not disclose both embedding profiles: {preview_profiles!r}")
    for pair, (_, body, text) in zip(pair_ids, pairs):
        plain_digest = "sha256:" + hashlib.sha256(body.encode("utf-8")).hexdigest()
        summary_input = f"Summary:\n{text}\n\nBody:\n{body}"
        summary_digest = "sha256:" + hashlib.sha256(summary_input.encode("utf-8")).hexdigest()
        require(by_revision[pair["plain_revision"]].get("input_digest") == plain_digest,
                "plain input_digest did not bind the exact body")
        require(by_revision[pair["summary_revision"]].get("input_digest") == summary_digest,
                "summary input_digest did not bind the exact rendered input")
    review_records = staged.get("review_records")
    require(isinstance(review_records, list) and any(row.get("id") == ids["summary_revision"] for row in review_records),
            "preview lost summary revision from review_records")
    reviewed = next(row for row in review_records if row.get("id") == ids["summary_revision"])
    require(reviewed.get("data", {}).get("summary") == enriched["data"]["summary"], "preview rewrote summary review material")
    require(staged.get("summary_review_required") is True, "summary preview did not require review")
    receipt = apply(api, staged).get("receipt")
    require(isinstance(receipt, dict) and receipt.get("replay") is False, "initial summary fixture did not apply")

    exported = api.get("/api/export", project_id=project, include_receipts="true")
    stored = {row["id"]: row for row in exported["content"]["records"]}
    require(stored[ids["summary_revision"]]["data"].get("summary") == enriched["data"]["summary"], "stored summary differs from reviewed summary")
    vectors = [row for row in stored.values() if row.get("kind") == "embedding"]
    vector_by_revision = {row["data"].get("revision_id"): row for row in vectors}
    require(vector_by_revision[ids["plain_revision"]]["data"].get("model", "").endswith("/idea-body-v1"), "stored v1 vector missing")
    require(vector_by_revision[ids["summary_revision"]]["data"].get("model", "").endswith("/idea-body-summary-v2"), "stored v2 vector missing")
    require(vector_by_revision[ids["summary_revision"]]["data"].get("values"), "summary vector has no actual values")

    before_seq = api.get("/api/state").get("seq")
    invalid_cases = {
        "human_origin": {"text": "valid text", "source": {**ai_source(capture), "origin": "human"}},
        "extracted_claim": {"text": "valid text", "source": {**ai_source(capture), "claim_mode": "extracted"}},
        "no_attribution": {"text": "valid text", "source": {**ai_source(capture), "model": None, "skill": None}},
        "missing_provenance": {"text": "valid text", "source": {**ai_source(capture), "capture_id": None}},
        "capture_mismatch": {"text": "valid text", "source": ai_source(f"{prefix}_other_capture")},
        "has_anchor": {"text": "valid text", "source": {**ai_source(capture), "source_anchor": {"capture_id": capture, "start": 0, "end": 1}}},
        "empty_text": {"text": "", "source": ai_source(capture)},
        "blank_text": {"text": "  \n\t", "source": ai_source(capture)},
        "too_long": {"text": "x" * 2001, "source": ai_source(capture)},
        "unknown_field": {"text": "valid text", "source": ai_source(capture), "unrecognized": True},
    }
    invalid_outcomes: dict[str, str | None] = {}
    for label, bad_summary in invalid_cases.items():
        entity, revision = f"{prefix}_{label}_entity", f"{prefix}_{label}_revision"
        bad = revision_record(revision, entity, "독립 규칙", source=ai_source(capture))
        bad["data"]["summary"] = bad_summary
        raw = package(f"{prefix}_{label}", "reject invalid summary source", expected_head(project, ids["root"]),
                      [entity_record(entity, project, "idea", "운영 규칙"), bad])
        invalid_outcomes[label] = expect_error(lambda raw=raw: preview(api, raw), label).code
    require(api.get("/api/state").get("seq") == before_seq, "invalid summary preview changed sequence")
    require(api.get(f"/api/records/{capture}")["record"]["data"]["content"] == capture_text,
            "failed summary previews altered the original capture")

    # Source provenance validates, but truthfulness is deliberately a human
    # review question.  A plausible-but-false paraphrase must therefore stage
    # as review material rather than silently becoming trusted content.
    false_entity, false_revision = f"{prefix}_false_entity", f"{prefix}_false_revision"
    false_record = with_summary(
        revision_record(false_revision, false_entity, "승인 뒤에만 다시 한다.", source=ai_source(capture)),
        "승인 전에도 실패 알림을 즉시 재개한다.", capture,
    )
    false_preview = preview(api, package(
        f"{prefix}_false_claim", "stage a deliberately false summary for human review",
        expected_head(project, ids["root"]),
        [entity_record(false_entity, project, "idea", "운영 규칙"), false_record],
    ))
    require(false_preview.get("summary_review_required") is True, "false summary bypassed review requirement")
    false_review = next((row for row in false_preview.get("review_records", []) if row.get("id") == false_revision), None)
    require(isinstance(false_review, dict) and false_review.get("data", {}).get("summary") == false_record["data"]["summary"],
            "false summary was not retained as review material")
    discarded = api._mcp("idea_upload_discard", {"upload_id": false_preview["upload_id"]})
    require(discarded.get("discarded") is True and discarded.get("replay") is False,
            f"false-summary review handle was not auditable/discarded: {discarded!r}")

    # Add this only after the first receipt.  Historical search at that receipt
    # must not leak a goal later attached to one of the shared occurrences.
    later_goal = {"id": ids["goal"], "kind": "goal", "data": {
        "project_id": project,
        "scope": {"root_revision_id": ids["root"], "slot_path": ["operations", "summary_direct"],
                  "target_revision_id": ids["summary_revision"]},
        "statement": "야간 배포 알림 예외를 검토한다",
        "criteria": [{"criterion_id": "review", "kind": "qualitative", "statement": "예외와 재개 조건이 보인다",
                      "metric": None, "comparator": None, "threshold": None, "unit": None, "required": True}],
        "origin": "official", "actor": "human:summary-ingest",
    }}
    later_preview = preview(api, package(f"{prefix}_later_goal", "attach one scoped goal after initial receipt",
                                         expected_head(project, ids["root"]), [later_goal]))
    require(apply(api, later_preview).get("receipt", {}).get("replay") is False, "later goal did not apply")

    # Auto must make both formats eligible; explicit modes must expose their
    # profiles and every vector-backed hit must disclose the selected profile.
    auto = search(api, project, "야간 배포 때 실패 알림을 하지 않는 조건", embedding_format="auto")
    profiles = auto.get("embedding_profiles")
    require(isinstance(profiles, list) and any(str(p).endswith("/idea-body-v1") for p in profiles)
            and any(str(p).endswith("/idea-body-summary-v2") for p in profiles), f"auto profiles missing: {profiles!r}")
    auto_hit = next((h for h in auto.get("results", []) if h.get("revision_id") == ids["summary_revision"]), None)
    require(isinstance(auto_hit, dict) and auto_hit.get("embedding_profile") in profiles,
            f"auto result did not disclose its selected profile: {auto_hit!r}")
    v1 = search(api, project, "야간 배포 때 실패 알림을 하지 않는 조건", embedding_format="body_v1")
    v2 = search(api, project, "야간 배포 때 실패 알림을 하지 않는 조건", embedding_format="body_summary_v2")
    require(all(str(p).endswith("/idea-body-v1") for p in v1.get("embedding_profiles", [])), "body_v1 admitted another profile")
    require(all(str(p).endswith("/idea-body-summary-v2") for p in v2.get("embedding_profiles", [])), "body_summary_v2 admitted another profile")
    v2_hit = next((h for h in v2.get("results", []) if h.get("revision_id") == ids["summary_revision"]), None)
    require(isinstance(v2_hit, dict) and str(v2_hit.get("embedding_profile", "")).endswith("/idea-body-summary-v2"),
            f"v2 search did not expose the summary vector profile: {v2_hit!r}")

    no_context = search(api, project, "승인 뒤 알림 재개", embedding_format="auto", tags=["summary-shared"], context_budget_chars=0)
    require(no_context.get("context", {}).get("budget_chars") == 0, "zero context budget not retained")
    default_context = search(api, project, "승인 뒤 알림 재개", embedding_format="auto", tags=["summary-shared"])
    context = default_context.get("context", {})
    require(context.get("budget_chars") == 8000 and isinstance(context.get("used_chars"), int)
            and context["used_chars"] <= 8000, f"MCP default context budget invalid: {context!r}")
    charged_contexts = [packet for hit in default_context.get("results", [])
                        for packet in hit.get("occurrence_contexts", [])]
    require(sum(canonical_json_chars(packet) for packet in charged_contexts) == context["used_chars"],
            "context used_chars is not the serialized Unicode-character charge")
    matching_hit = next((item for item in default_context.get("results", [])
                         if item.get("revision_id") == ids["summary_revision"]), None)
    require(isinstance(matching_hit, dict), "summary revision absent from context search")
    occurrence_contexts = matching_hit.get("occurrence_contexts")
    require(isinstance(occurrence_contexts, list), "search result lacks occurrence_contexts")
    shared_contexts = occurrence_contexts
    require(len(shared_contexts) >= 2, f"shared revision did not retain both occurrence paths: {shared_contexts!r}")
    require(any(item.get("goal_history") for item in shared_contexts),
            "scoped goal missing from occurrence context")
    require(any(item.get("roles") == ["be"] and item.get("goal_history")
                and item.get("goal_selection") == "scope_history_not_active_baseline" for item in shared_contexts),
            "later goal did not stay scoped to the be occurrence")
    require(any(item.get("roles") == ["fe"] and not item.get("goal_history") for item in shared_contexts),
            "shared fe occurrence inherited the be-only goal")
    historical = search(api, project, "승인 뒤 알림 재개", embedding_format="auto", tags=["summary-shared"],
                        known_seq=receipt["seq"], context_budget_chars=8000)
    require(any(item.get("revision_id") == ids["summary_revision"] for item in historical.get("results", [])),
            "known_seq search did not retain the committed summary occurrence")
    historical_hit = next(item for item in historical["results"] if item.get("revision_id") == ids["summary_revision"])
    require(not any(packet.get("goal_history") for packet in historical_hit.get("occurrence_contexts", [])),
            "known_seq context leaked the later scoped goal")
    tiny = search(api, project, "승인 뒤 알림 재개", embedding_format="auto", tags=["summary-shared"], context_budget_chars=1)
    tiny_context = tiny.get("context", {})
    require(tiny_context.get("budget_chars") == 1 and tiny_context.get("used_chars", 2) <= 1,
            f"tiny JSON-character context budget exceeded: {tiny_context!r}")

    # Six labelled questions, each searched over six equally sized candidates.
    # The two deployment descriptions are equivalent relevant targets; other
    # rows are distractors, never counted as gold merely for being in the cohort.
    queries = [
        ("deployment", "When are deployment failure notifications suppressed?", [0, 1]),
        ("rollback", "customer notification delivery after rollback verification", [2]),
        ("maintenance", "maintenance silence until completion confirmation", [3]),
        ("incident", "severity one incident automatic restart approval", [4]),
        ("privacy", "retention exception before permanent privacy deletion", [5]),
        ("negative", "야간 배포 알림을 승인 전에 재개할 수 있는가", [0, 1]),
    ]
    def cohort_metrics(result: Json, arm: str, relevant: list[int]) -> Json:
        targets = [pair[arm] for pair in pair_ids]
        found = result.get("results", [])
        require(set(h["revision_id"] for h in found) == set(targets),
                "tagged arm did not restrict to its six paired candidates")
        gold = [pair_ids[index][arm] for index in relevant]
        positions = {target: rank(result, target) for target in gold}
        vector_positions = {h["revision_id"]: h["vector_rank"] for h in found}
        require(all(isinstance(vector_positions.get(target), int) for target in targets), "paired candidate missing vector rank")
        ranks = list(positions.values())
        return {"candidate_count": len(targets), "gold_revision_ids": gold,
                "gold_ranks": positions, "vector_only_gold_ranks": {t:vector_positions[t] for t in gold},
                "reciprocal_rank": 1.0 / min(ranks),
                "recall_at_1": sum(r == 1 for r in ranks) / len(ranks),
                "recall_at_3": sum(r <= 3 for r in ranks) / len(ranks)}
    measurements = []
    for category, text, relevant in queries:
        body_only = search(api, project, text, embedding_format="body_v1", tags=["summary-plain"], context_budget_chars=0)
        summary_aware = search(api, project, text, embedding_format="body_summary_v2", tags=["summary-augmented"], context_budget_chars=0)
        measurements.append({
            "category": category, "query": text,
            "body_v1": cohort_metrics(body_only, "plain_revision", relevant),
            "body_summary_v2": cohort_metrics(summary_aware, "summary_revision", relevant),
            "body_v1_raw_ranks": body_only["results"],
            "body_summary_v2_raw_ranks": summary_aware["results"],
        })
    metrics = {arm: {"mrr": sum(m[arm]["reciprocal_rank"] for m in measurements) / len(measurements),
                     "mean_recall_at_3": sum(m[arm]["recall_at_3"] for m in measurements) / len(measurements)}
               for arm in ("body_v1", "body_summary_v2")}
    return {
        "status": "passed", "project_id": project, "capture_id": capture, "receipt_seq": receipt.get("seq"),
        "checks": [
            "v1 and summary-v2 embeddings staged, reviewed, applied, and exported",
            "invalid summary sources reject without changing the sequence or source capture",
            "a false-but-well-formed summary remains explicit human review material",
            "auto and explicit embedding formats disclose allowed and selected profiles",
            "MCP occurrence context honors budgets and retains shared paths with scoped goals",
            "equal paired body-only versus summary-aware cohorts record raw/vector ranks and descriptive MRR/Recall@k without a promised win",
        ],
        "invalid_summary_outcomes": invalid_outcomes,
        "embedding_input_binding": {
            "checked": "sha256 of the exact UTF-8 body or Summary:\\n{text}\\n\\nBody:\\n{body} render",
            "paired_inputs": len(pair_ids) * 2,
            "provider_reembed": "not independently requested; staged input digests and stored provider vectors are the live binding evidence",
        },
        "paired_retrieval_measurements": measurements,
        "paired_metrics": metrics,
        "fixture_ids": ids,
        "limitations": "Six synthetic labelled queries, hand-authored AI-shaped summaries, one deliberately incomplete legacy pronoun case; multilingual synonyms favor this fixture. Measures retrieval, not generated answers or autonomous atomization.",
    }


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=120.0)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    if args.output_dir.exists() and any(args.output_dir.iterdir()):
        raise SystemExit(f"refusing to overwrite nonempty output directory: {args.output_dir}")
    args.output_dir.mkdir(parents=True, exist_ok=True)
    try:
        report = run(args.base_url, args.timeout, args.output_dir)
    except Exception as exc:
        report = {"status": "failed", "error_type": type(exc).__name__, "error": str(exc)}
        write_json(args.output_dir / "summary-ingest-report.json", report)
        print(json.dumps(report, ensure_ascii=False), file=sys.stderr)
        return 1
    write_json(args.output_dir / "summary-ingest-report.json", report)
    print(json.dumps(report, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
