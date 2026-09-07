#!/usr/bin/env python3
"""Integration regression for staged MCP ingestion against local Ollama.

The input package is a deterministic *client-side structured fixture*.  It does
not claim that idea_db atomizes text or that an LLM autonomously decomposes the
capture.  A running Docker test database and IDEA_DB_MCP_COMMAND_JSON are
required; all writes go through its stdio MCP server.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import subprocess
import sys
import uuid
from pathlib import Path
from typing import Any, Callable

from acceptance import (
    AcceptanceFailure,
    Api,
    ApiError,
    entity_record,
    expected_head,
    inferred_source,
    package,
    project_record,
    require,
    revision_record,
    root_change,
    slot,
)


Json = dict[str, Any]


def write_json(path: Path, value: Json) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + "\n", "utf-8")


def source(capture_id: str, *, extracted: bool = False, start: int | None = None, end: int | None = None) -> Json:
    return {
        "origin": "ai",
        "capture_id": capture_id,
        "model": "fixture-structured-client",
        "skill": "mcp_ingest_fixture",
        "claim_mode": "extracted" if extracted else "inferred",
        "source_anchor": {"capture_id": capture_id, "start": start, "end": end} if extracted else None,
    }


def candidate_record(record_id: str, project_id: str, capture_id: str, body: str, end: int) -> Json:
    return {
        "id": record_id,
        "kind": "candidate",
        "data": {
            "project_id": project_id,
            "capture_id": capture_id,
            "proposed_kind": "idea",
            "title": "로그인 차단 후보",
            "body": body,
            "origin": "ai",
            "model": "fixture-structured-client",
            "skill": "mcp_ingest_fixture",
            "claim_mode": "extracted",
            "source_anchor": {"capture_id": capture_id, "start": 0, "end": end},
            "status": "pending",
        },
    }


def expect_error(action: Callable[[], Any], statuses: set[int], label: str) -> ApiError:
    try:
        action()
    except ApiError as exc:
        require(exc.status in statuses, f"{label}: unexpected status {exc.status}: {exc.body!r}")
        require(isinstance(exc.code, str) and exc.code, f"{label}: structured error code missing")
        return exc
    raise AcceptanceFailure(f"{label}: request unexpectedly succeeded")


def mcp(api: Api, tool: str, body: Json) -> Json:
    return api._mcp(tool, {"body": body})


def preview(api: Api, raw_package: Json) -> Json:
    result = mcp(api, "idea_upload_preview", {"package": raw_package})
    require(isinstance(result.get("upload_id"), str), f"preview missing upload_id: {result!r}")
    require(isinstance(result.get("prepared_digest"), str), f"preview missing prepared_digest: {result!r}")
    require("package" not in result, "staged preview leaked a raw/prepared package")
    require('"values"' not in json.dumps(result, ensure_ascii=False), "staged preview leaked embedding vector values")
    validation = result.get("validation")
    require(isinstance(validation, dict) and validation.get("valid") is True, f"preview validation failed: {validation!r}")
    return result


def apply(api: Api, staged: Json) -> Json:
    return mcp(api, "idea_package_apply", {
        "upload_id": staged["upload_id"],
        "prepared_digest": staged["prepared_digest"],
    })


def discard(api: Api, upload_id: str) -> Json:
    # This MCP tool deliberately takes its small operational argument directly,
    # unlike the domain-package tools that preserve the historic {body: ...}
    # envelope.
    return api._mcp("idea_upload_discard", {"upload_id": upload_id})


def bad_embedding_command() -> str:
    raw = os.environ.get("IDEA_DB_MCP_COMMAND_JSON")
    if not raw:
        raise AcceptanceFailure("IDEA_DB_MCP_COMMAND_JSON is required")
    command = json.loads(raw)
    require(isinstance(command, list) and command and command[-1] == "idea-db-mcp",
            "provider failure probe requires docker exec … idea-db-mcp command")
    container_index = len(command) - 2
    require(container_index >= 0, "MCP command has no Docker container argument")
    command[container_index:container_index] = ["-e", "IDEA_DB_EMBEDDING_URL=http://127.0.0.1:9"]
    return json.dumps(command)


def docker_mcp_container() -> str:
    """Return the owned Docker container behind the integration MCP command."""
    raw = os.environ.get("IDEA_DB_MCP_COMMAND_JSON")
    if not raw:
        raise AcceptanceFailure("IDEA_DB_MCP_COMMAND_JSON is required")
    try:
        command = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise AcceptanceFailure("IDEA_DB_MCP_COMMAND_JSON is not JSON") from exc
    require(
        isinstance(command, list)
        and len(command) >= 4
        and command[0] == "docker"
        and command[1] == "exec"
        and command[-1] == "idea-db-mcp",
        "staging verification requires docker exec … idea-db-mcp command",
    )
    container = command[-2]
    require(isinstance(container, str) and container, "MCP command has no Docker container argument")
    return container


def read_staging_rows(upload_ids: set[str]) -> dict[str, Json]:
    """Read staging nodes directly from the owned Neo4j container.

    This intentionally bypasses MCP only for read-only persistence invariants.
    The database password expands inside the container from its environment and
    is never included in this process's command arguments or output.
    """
    require(upload_ids, "staging verification requires upload ids")
    for upload_id in upload_ids:
        require(isinstance(upload_id, str) and upload_id.replace("_", "").isalnum(), "unsafe upload id")

    ids = ", ".join(json.dumps(upload_id) for upload_id in sorted(upload_ids))
    query = (
        "MATCH (s:IdeaDbUpload) "
        f"WHERE s.upload_id IN [{ids}] "
        "RETURN s.upload_id AS upload_id, s.applied_seq AS applied_seq, "
        "s.withdrawn_at AS withdrawn_at, s.document AS document, "
        "(s.applied_seq IS NULL AND s.invalidated_at IS NULL AND s.withdrawn_at IS NULL) AS pending "
        "ORDER BY upload_id"
    )
    payload = json.dumps({"statements": [{"statement": query}]}, separators=(",", ":"))
    encoded_payload = base64.b64encode(payload.encode("utf-8")).decode("ascii")
    shell = (
        f"payload=$(printf '%s' '{encoded_payload}' | base64 -d)\n"
        "exec wget --quiet --output-document=- --method=POST "
        "--header='Content-Type: application/json' --user=neo4j "
        '--password="$NEO4J_PASSWORD" --body-data="$payload" '
        "http://127.0.0.1:7474/db/neo4j/tx/commit"
    )
    completed = subprocess.run(
        ["docker", "exec", docker_mcp_container(), "bash", "-ceu", shell],
        check=False,
        capture_output=True,
        text=True,
        timeout=30,
    )
    require(completed.returncode == 0, "read-only Neo4j staging verification failed")
    try:
        response = json.loads(completed.stdout)
    except json.JSONDecodeError as exc:
        raise AcceptanceFailure("Neo4j staging verification returned non-JSON") from exc
    require(not response.get("errors"), f"Neo4j staging verification error: {response['errors']!r}")
    results = response.get("results")
    require(isinstance(results, list) and len(results) == 1, "Neo4j staging query returned no result")
    rows = results[0].get("data")
    require(isinstance(rows, list), "Neo4j staging query has invalid rows")

    actual: dict[str, Json] = {}
    for item in rows:
        row = item.get("row") if isinstance(item, dict) else None
        require(isinstance(row, list) and len(row) == 5, f"invalid staging row: {item!r}")
        upload_id, applied_seq, withdrawn_at, document, pending = row
        require(isinstance(upload_id, str), f"invalid staged upload id: {item!r}")
        require(isinstance(document, str), f"staged upload {upload_id} lost its retained document")
        actual[upload_id] = {
            "applied_seq": applied_seq,
            "withdrawn_at": withdrawn_at,
            "document": document,
            "pending": pending,
        }
    require(set(actual) == upload_ids,
            f"staged uploads missing from Neo4j: expected={sorted(upload_ids)} actual={sorted(actual)}")
    return actual


def assert_staged_commit_rows(expected: dict[str, int]) -> Json:
    """Verify applied staged handles share their domain receipt sequence."""
    require(expected, "staging verification requires at least one applied upload")
    for upload_id, seq in expected.items():
        require(isinstance(seq, int) and seq > 0, f"invalid receipt sequence for {upload_id}: {seq!r}")
    actual = read_staging_rows(set(expected))
    for upload_id, commit_seq in expected.items():
        row = actual[upload_id]
        require(row["applied_seq"] == commit_seq,
                f"staged upload {upload_id} applied_seq differs from receipt: {row!r} vs {commit_seq}")
        require(row["pending"] is False, f"successfully applied upload {upload_id} remains pending")
    # Documents contain prepared embeddings.  They are read only for the
    # withdrawal assertion and must never be copied into the test report.
    return {
        upload_id: {"applied_seq": row["applied_seq"], "pending": row["pending"]}
        for upload_id, row in actual.items()
    }


def build_initial(prefix: str, capture_id: str, captured: str) -> tuple[Json, Json]:
    ids = {name: f"{prefix}_{name}" for name in (
        "project", "schema_entity", "schema_revision", "core_entity", "core_revision",
        "old_entity", "old_revision", "candidate", "promotion", "root_revision",
    )}
    candidate_body = "🔒 로그인 실패는 5회까지 허용하지 않는다."
    require(captured.startswith(candidate_body), "fixture candidate must be the exact captured prefix")
    records = [
        project_record(ids["project"], "MCP staged ingestion fixture"),
        entity_record(ids["schema_entity"], ids["project"], "schema", "인증 스키마"),
        entity_record(ids["core_entity"], ids["project"], "core", "로그인 정책"),
        entity_record(ids["old_entity"], ids["project"], "idea", "로그인 차단 규칙"),
        revision_record(ids["old_revision"], ids["old_entity"], candidate_body,
                        source=source(capture_id, extracted=True, start=0, end=len(candidate_body))),
        candidate_record(ids["candidate"], ids["project"], capture_id, candidate_body, len(candidate_body)),
        {
            "id": ids["promotion"], "kind": "promotion", "data": {
                "candidate_id": ids["candidate"], "entity_id": ids["old_entity"],
                "revision_id": ids["old_revision"], "actor": "human:mcp-ingest",
                "reason": "fixture candidate was explicitly reviewed",
            },
        },
        revision_record(ids["core_revision"], ids["core_entity"], "로그인 실패 정책을 조합한다",
                        slots=[slot("block_rule", ids["old_revision"], "be", "security")]),
        revision_record(ids["schema_revision"], ids["schema_entity"], "인증 기능을 조합한다",
                        slots=[slot("login_policy", ids["core_revision"], "be")]),
        revision_record(ids["root_revision"], ids["project"], "프로젝트 인증 구조",
                        slots=[slot("auth", ids["schema_revision"], "be")]),
    ]
    raw = package(
        f"{prefix}_initial_package", "client fixture creates recursive reviewable structure",
        expected_head(ids["project"], None), records,
        root_change=root_change(ids["project"], ids["root_revision"], "none", "initial auth composition"),
    )
    return raw, ids


def build_new_ideas(prefix: str, ids: Json, capture_id: str) -> tuple[Json, Json]:
    new = {name: f"{prefix}_{name}" for name in ("paraphrase_entity", "paraphrase_revision", "changed_entity", "changed_revision")}
    records = [
        entity_record(new["paraphrase_entity"], ids["project"], "idea", "접근 차단 재표현",
                      derived_from=[ids["old_entity"]], lineage_kind="semantic_edit"),
        revision_record(new["paraphrase_revision"], new["paraphrase_entity"], "로그인 오류가 다섯 번이면 접근을 막는다.",
                        source={**source(capture_id), "claim_mode": "inferred"}),
        entity_record(new["changed_entity"], ids["project"], "idea", "허용 횟수 반대 규칙"),
        revision_record(new["changed_revision"], new["changed_entity"], "로그인 오류는 3회까지 허용한다.",
                        source={**source(capture_id), "claim_mode": "inferred"}),
    ]
    raw = package(
        f"{prefix}_new_ideas_package", "review paraphrase and changed numeric negation separately",
        expected_head(ids["project"], ids["root_revision"]), records,
    )
    return raw, new


def run(base_url: str, timeout: float, output_dir: Path) -> Json:
    prefix = "mi" + uuid.uuid4().hex[:18]
    api = Api(base_url, os.environ.get("IDEA_DB_TOKEN"), timeout)
    checks: list[str] = []
    captured = "🔒 로그인 실패는 5회까지 허용하지 않는다. 예외 없이 차단한다."
    capture_id = f"{prefix}_source_capture"

    capture = mcp(api, "idea_capture_create", {
        "id": capture_id, "project_id": None, "content": captured,
        "media_type": "text/plain", "source_kind": "note", "source_ref": "fixture://mcp-ingest",
        "occurred_at": "2026-09-07T00:00:00.000Z", "content_digest": None,
    })
    require(capture.get("capture", {}).get("id") == capture_id, f"capture create failed: {capture!r}")
    checks.append("saved immutable Unicode source capture")

    initial_raw, ids = build_initial(prefix, capture_id, captured)
    initial_preview = preview(api, initial_raw)
    initial_receipt = apply(api, initial_preview).get("receipt")
    require(isinstance(initial_receipt, dict) and initial_receipt.get("replay") is False, "initial staged apply failed")
    checks.append("staged preview returns handle/digest without raw vectors and applies recursive package")

    exported = api.get("/api/export", project_id=ids["project"], include_receipts="true")
    embeddings = [row for row in exported["content"]["records"] if row.get("kind") == "embedding"]
    old_embeddings = [row for row in embeddings if row.get("data", {}).get("revision_id") == ids["old_revision"]]
    require(len(old_embeddings) == 1 and old_embeddings[0]["data"].get("values"), "stored Idea embedding missing after apply")
    require(old_embeddings[0]["data"].get("model") == initial_preview.get("embedding_profile"), "stored embedding profile changed")
    candidate = next(row for row in exported["content"]["records"] if row.get("id") == ids["candidate"])
    anchor = candidate["data"].get("source_anchor", {})
    require(anchor == {"capture_id": capture_id, "start": 0, "end": len(candidate["data"]["body"])},
            "candidate Unicode scalar source span changed")
    require(candidate["data"]["body"] == captured[:len(candidate["data"]["body"])], "candidate body no longer matches captured span")
    checks.append("export contains stored Idea embedding and exact captured candidate span")

    before_bad = api.get("/api/state")["seq"]
    bad_candidate = candidate_record(f"{prefix}_bad_candidate", ids["project"], capture_id, "다른 문장", len("다른 문장"))
    bad_raw = package(f"{prefix}_bad_package", "reject fabricated extracted candidate", expected_head(ids["project"], ids["root_revision"]), [bad_candidate])
    original_command = os.environ.get("IDEA_DB_MCP_COMMAND_JSON")
    os.environ["IDEA_DB_MCP_COMMAND_JSON"] = bad_embedding_command()
    try:
        invalid = expect_error(lambda: mcp(api, "idea_upload_preview", {"package": bad_raw}), {400, 422}, "bad source package")
    finally:
        if original_command is None:
            os.environ.pop("IDEA_DB_MCP_COMMAND_JSON", None)
        else:
            os.environ["IDEA_DB_MCP_COMMAND_JSON"] = original_command
    require(invalid.code == "validation_failed", f"bad source package did not return validation_failed: {invalid.body!r}")
    require(api.get("/api/state")["seq"] == before_bad, "bad preview changed sequence")
    source_after = api.get(f"/api/records/{capture_id}")["record"]
    require(source_after["data"]["content"] == captured, "original source capture changed after bad preview")
    checks.append("invalid preview fails before an unavailable embedding provider and leaves source/sequence unchanged")

    new_raw, new_ids = build_new_ideas(prefix, ids, capture_id)
    new_preview = preview(api, new_raw)
    mappings = new_preview.get("mappings")
    require(isinstance(mappings, list) and {row.get("revision_id") for row in mappings} == {
        new_ids["paraphrase_revision"], new_ids["changed_revision"]
    }, f"preview mappings did not cover both reviewed new ideas: {mappings!r}")
    require(all(row.get("requires_semantic_review") is True for row in mappings), "mapping skipped semantic review")
    require(new_preview.get("automatically_merged") is False,
            "numeric/negation change was auto-merged")
    checks.append("paraphrase and changed numeric/negation mappings remain explicit review material")

    before_wrong = api.get("/api/state")["seq"]
    wrong = expect_error(lambda: mcp(api, "idea_package_apply", {
        "upload_id": new_preview["upload_id"], "prepared_digest": "sha256:" + "0" * 64,
    }), {400, 409, 422}, "wrong prepared digest")
    require(api.get("/api/state")["seq"] == before_wrong, "wrong digest changed sequence")
    missing = expect_error(lambda: mcp(api, "idea_package_apply", {
        "upload_id": f"{prefix}_missing_upload", "prepared_digest": new_preview["prepared_digest"],
    }), {400, 404, 409, 422}, "missing staged upload")
    require(wrong.code != "" and missing.code != "", "staging errors were not structured")
    raw_bypass = expect_error(lambda: mcp(api, "idea_package_apply", {
        "package": new_raw, "prepared_digest": new_preview["prepared_digest"],
    }), {400, 422}, "raw package apply bypass")
    require(raw_bypass.code != "", "raw apply bypass error was not structured")
    require(api.get("/api/state")["seq"] == before_wrong, "rejected staged apply changed sequence")
    checks.append("wrong digest, missing handle, and raw package bypass are rejected without writes")

    new_first = apply(api, new_preview).get("receipt")
    new_replay = apply(api, new_preview).get("receipt")
    require(isinstance(new_first, dict) and new_first.get("replay") is False, "new staged upload did not apply")
    require(isinstance(new_replay, dict) and new_replay.get("replay") is True, "same staged handle did not replay")
    checks.append("same staged upload handle is idempotent")

    hybrid = mcp(api, "idea_search", {
        "project_id": ids["project"], "scope": "project_history", "query": "로그인 오류 접근 차단",
        "stage": "working", "roles": [], "tags": [], "lanes": ["official"], "limit": 20,
        "embedding_mode": "hybrid",
    })
    require(hybrid.get("embedding_mode") == "hybrid" and isinstance(hybrid.get("embedding_profile"), str), "hybrid MCP search did not create a provider query vector")
    require(hybrid.get("vector_status", {}).get("requested") is True, "hybrid search did not request vector retrieval")
    hybrid_coverage = hybrid.get("index_coverage", {})
    require(hybrid_coverage.get("checked") is True and hybrid_coverage.get("complete") is True,
            f"hybrid search did not report complete eligible-Idea index coverage: {hybrid_coverage!r}")
    require(hybrid_coverage.get("eligible_idea_revisions") == hybrid_coverage.get("indexed_idea_revisions"),
            "hybrid index coverage counted an eligible Idea without its matching embedding")
    lexical = mcp(api, "idea_search", {
        "project_id": ids["project"], "scope": "project_history", "query": "로그인 실패 5회",
        "stage": "working", "roles": [], "tags": [], "lanes": ["official"], "limit": 20,
        "embedding_mode": "lexical",
    })
    require(lexical.get("embedding_mode") == "lexical" and lexical.get("embedding_profile") is None, "explicit lexical MCP search called embedding provider")
    require(ids["old_revision"] in {row.get("revision_id") for row in lexical.get("results", [])}, "lexical search missed stored old idea")
    lexical_coverage = lexical.get("index_coverage", {})
    require(lexical_coverage.get("checked") is False and lexical_coverage.get("complete") is None,
            f"lexical search incorrectly claimed embedding index coverage: {lexical_coverage!r}")
    checks.append("hybrid verifies eligible-Idea index completeness while lexical search leaves it unchecked")

    link_id = f"{prefix}_reviewed_similar"
    link_raw = package(
        f"{prefix}_similar_package", "record human review of similarity without auto merge",
        expected_head(ids["project"], ids["root_revision"]), [{
            "id": link_id, "kind": "link", "data": {
                "link_type": "similar", "from_id": ids["old_revision"], "to_id": new_ids["paraphrase_revision"],
                "note": "human reviewed paraphrase; numeric/negation counterpart remains separate",
                "actor": "human:mcp-ingest", "impact": None,
                "similarity": {"score": 0.82, "method": "human review after staged mapping"},
            },
        }],
    )
    link_preview = preview(api, link_raw)
    link_receipt = apply(api, link_preview).get("receipt")
    require(isinstance(link_receipt, dict), "reviewed similar link did not apply")
    link_export = api.get("/api/export", project_id=ids["project"], include_receipts="true")
    stored_link = next(row for row in link_export["content"]["records"] if row.get("id") == link_id)
    require(stored_link["data"]["link_type"] == "similar" and stored_link["data"]["similarity"]["score"] == 0.82,
            "explicit reviewed similar link was not retained")
    checks.append("reviewed similar link is an explicit staged client package")

    withdrawn_link_id = f"{prefix}_withdrawn_similar"
    withdrawn_raw = package(
        f"{prefix}_withdrawn_package", "stage then explicitly withdraw an unreviewed similarity link",
        expected_head(ids["project"], ids["root_revision"]), [{
            "id": withdrawn_link_id, "kind": "link", "data": {
                "link_type": "similar", "from_id": ids["old_revision"], "to_id": new_ids["changed_revision"],
                "note": "unreviewed stage must be discarded without changing the domain graph",
                "actor": "human:mcp-ingest", "impact": None,
                "similarity": {"score": 0.31, "method": "discard fixture"},
            },
        }],
    )
    withdrawn_preview = preview(api, withdrawn_raw)
    before_discard = api.get("/api/state", project_id=ids["project"], limit=1)
    before_project = next(project for project in before_discard["projects"] if project["project_id"] == ids["project"])
    first_discard = discard(api, withdrawn_preview["upload_id"])
    require(first_discard.get("upload_id") == withdrawn_preview["upload_id"]
            and first_discard.get("discarded") is True and first_discard.get("replay") is False,
            f"first upload discard failed: {first_discard!r}")
    withdrawn_at = first_discard.get("withdrawn_at")
    require(isinstance(withdrawn_at, str) and withdrawn_at, f"discard did not record withdrawn_at: {first_discard!r}")
    second_discard = discard(api, withdrawn_preview["upload_id"])
    require(second_discard.get("replay") is True and second_discard.get("withdrawn_at") == withdrawn_at,
            f"discard replay changed withdrawal history: {second_discard!r}")
    after_discard = api.get("/api/state", project_id=ids["project"], limit=1)
    after_project = next(project for project in after_discard["projects"] if project["project_id"] == ids["project"])
    require(after_discard["seq"] == before_discard["seq"], "discard changed the domain sequence")
    require(
        after_project.get("working_head") == before_project.get("working_head")
        and after_project.get("official_head") == before_project.get("official_head"),
        "discard changed a project head",
    )
    withdrawn_apply = expect_error(lambda: apply(api, withdrawn_preview), {422}, "withdrawn upload apply")
    require(withdrawn_apply.code == "validation_failed" and "staging_withdrawn" in withdrawn_apply.issue_codes,
            f"withdrawn upload did not expose the required validation issue: {withdrawn_apply.body!r}")
    already_applied = expect_error(lambda: discard(api, initial_preview["upload_id"]), {409}, "applied upload discard")
    require(already_applied.code == "staging_already_applied", f"applied upload had wrong discard error: {already_applied.body!r}")
    withdrawn_row = read_staging_rows({withdrawn_preview["upload_id"]})[withdrawn_preview["upload_id"]]
    require(withdrawn_row["applied_seq"] is None and withdrawn_row["withdrawn_at"] == withdrawn_at
            and withdrawn_row["pending"] is False,
            f"withdrawn staged packet did not leave the pending set: {withdrawn_row!r}")
    withdrawn_document = json.loads(withdrawn_row["document"])
    require(withdrawn_document.get("prepared_digest") == withdrawn_preview["prepared_digest"],
            "withdrawn packet did not retain its prepared document")
    require(withdrawn_document["package"]["records"][0].get("id") == withdrawn_link_id,
            "withdrawn packet did not retain its reviewed record history")
    checks.append("discard retains the staged document, is idempotent, and blocks apply without changing sequence or heads")

    provider_raw = package(
        f"{prefix}_provider_package", "provider failure must not create semantic index fallback",
        expected_head(ids["project"], ids["root_revision"]), [
            entity_record(f"{prefix}_provider_entity", ids["project"], "idea", "provider failure probe"),
            revision_record(f"{prefix}_provider_revision", f"{prefix}_provider_entity", "임베딩 공급자가 없으면 저장하지 않는다.",
                            source=inferred_source()),
        ],
    )
    before_provider = api.get("/api/state")["seq"]
    original_command = os.environ.get("IDEA_DB_MCP_COMMAND_JSON")
    os.environ["IDEA_DB_MCP_COMMAND_JSON"] = bad_embedding_command()
    try:
        provider_error = expect_error(lambda: mcp(api, "idea_upload_preview", {"package": provider_raw}), {503}, "provider failure")
    finally:
        if original_command is None:
            os.environ.pop("IDEA_DB_MCP_COMMAND_JSON", None)
        else:
            os.environ["IDEA_DB_MCP_COMMAND_JSON"] = original_command
    require(provider_error.code.startswith("embedding_"), f"provider failure silently fell back: {provider_error.body!r}")
    require(api.get("/api/state")["seq"] == before_provider, "provider failure changed sequence")
    checks.append("bad local embedding provider fails closed without fallback or write")

    def receipt_seq(receipt: Any, label: str) -> int:
        require(isinstance(receipt, dict), f"{label} receipt is missing")
        seq = receipt.get("seq")
        require(isinstance(seq, int) and seq > 0, f"{label} receipt has no positive commit sequence: {receipt!r}")
        return seq

    staged_rows = assert_staged_commit_rows({
        str(initial_preview["upload_id"]): receipt_seq(initial_receipt, "initial upload"),
        str(new_preview["upload_id"]): receipt_seq(new_first, "new ideas upload"),
        str(link_preview["upload_id"]): receipt_seq(link_receipt, "similar link upload"),
    })
    checks.append("direct Neo4j read confirms applied staged handles share receipt sequences and are not pending")

    return {
        "status": "passed", "project_id": ids["project"], "capture_id": capture_id,
        "checks": checks, "initial_upload_id": initial_preview["upload_id"],
        "new_upload_id": new_preview["upload_id"], "embedding_profile": initial_preview.get("embedding_profile"),
        "staging_commit_rows": staged_rows,
        "note": "Fixture structure is supplied by this test client; idea_db does not atomize text or claim autonomous AI decomposition.",
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
        write_json(args.output_dir / "mcp-ingest-report.json", report)
        print(json.dumps(report, ensure_ascii=False), file=sys.stderr)
        return 1
    write_json(args.output_dir / "mcp-ingest-report.json", report)
    print(json.dumps(report, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
