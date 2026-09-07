#!/usr/bin/env python3
"""Black-box acceptance checks for a running idea_db API and real Neo4j.

The suite uses only the Python standard library.  Every run creates globally
unique application IDs and never clears or mutates pre-existing records.
"""

from __future__ import annotations

import argparse
import concurrent.futures
import hashlib
import json
import os
import sys
import threading
import time
import uuid
from dataclasses import dataclass
from typing import Any, Callable
from urllib import error, parse, request


Json = dict[str, Any]


class AcceptanceFailure(AssertionError):
    pass


class ApiError(RuntimeError):
    def __init__(self, status: int, body: Any, method: str, url: str) -> None:
        self.status = status
        self.body = body
        self.method = method
        self.url = url
        super().__init__(f"{method} {url} returned {status}: {body!r}")

    @property
    def code(self) -> str | None:
        return self.body.get("code") if isinstance(self.body, dict) else None

    @property
    def issue_codes(self) -> set[str]:
        if not isinstance(self.body, dict):
            return set()
        details = self.body.get("details")
        if not isinstance(details, dict):
            return set()
        issues = details.get("issues", [])
        return {
            issue.get("code")
            for issue in issues
            if isinstance(issue, dict) and isinstance(issue.get("code"), str)
        }


class Api:
    def __init__(self, base_url: str, token: str | None, timeout: float) -> None:
        self.base_url = base_url.rstrip("/")
        self.token = token
        self.timeout = timeout

    def get(self, path: str, **params: Any) -> Json:
        query = {key: value for key, value in params.items() if value is not None}
        suffix = "?" + parse.urlencode(query) if query else ""
        return self._request("GET", path + suffix, None)[1]

    def post(self, path: str, body: Json) -> Json:
        return self._request("POST", path, body)[1]

    def _request(self, method: str, path: str, body: Json | None) -> tuple[int, Json]:
        url = self.base_url + path
        payload = None
        headers = {"Accept": "application/json"}
        if body is not None:
            payload = json.dumps(body, ensure_ascii=False, separators=(",", ":")).encode()
            headers["Content-Type"] = "application/json"
        if self.token:
            headers["Authorization"] = f"Bearer {self.token}"
        req = request.Request(url, data=payload, method=method, headers=headers)
        try:
            with request.urlopen(req, timeout=self.timeout) as response:
                raw = response.read()
                parsed = json.loads(raw) if raw else {}
                return response.status, parsed
        except error.HTTPError as exc:
            raw = exc.read()
            try:
                parsed = json.loads(raw) if raw else {}
            except json.JSONDecodeError:
                parsed = raw.decode("utf-8", errors="replace")
            raise ApiError(exc.code, parsed, method, url) from exc
        except error.URLError as exc:
            raise AcceptanceFailure(f"cannot reach {url}: {exc.reason}") from exc


def require(condition: Any, message: str) -> None:
    if not condition:
        raise AcceptanceFailure(message)


def canonical_digest(value: Any) -> str:
    encoded = json.dumps(
        value, ensure_ascii=False, sort_keys=True, separators=(",", ":")
    ).encode("utf-8")
    return "sha256:" + hashlib.sha256(encoded).hexdigest()


def inferred_source() -> Json:
    return {
        "origin": "human",
        "capture_id": None,
        "model": None,
        "skill": None,
        "claim_mode": "inferred",
        "source_anchor": None,
    }


def extracted_source(capture_id: str, text: str) -> Json:
    return {
        "origin": "human",
        "capture_id": capture_id,
        "model": None,
        "skill": None,
        "claim_mode": "extracted",
        "source_anchor": {"capture_id": capture_id, "start": 0, "end": len(text)},
    }


def project_record(record_id: str, title: str) -> Json:
    return {"id": record_id, "kind": "project", "data": {"title": title, "description": ""}}


def entity_record(
    record_id: str,
    project_id: str,
    entity_kind: str,
    title: str,
    *,
    tags: list[str] | None = None,
    derived_from: list[str] | None = None,
    lineage_kind: str = "none",
) -> Json:
    return {
        "id": record_id,
        "kind": "entity",
        "data": {
            "entity_kind": entity_kind,
            "project_id": project_id,
            "title": title,
            "tags": tags or [],
            "derived_from": derived_from or [],
            "lineage_kind": lineage_kind,
        },
    }


def revision_record(
    record_id: str,
    entity_id: str,
    body: str,
    *,
    slots: list[Json] | None = None,
    change_kind: str = "initial",
    previous_revision_id: str | None = None,
    correction_of: str | None = None,
    correction_reason: str | None = None,
    source: Json | None = None,
    tags: list[str] | None = None,
) -> Json:
    return {
        "id": record_id,
        "kind": "revision",
        "data": {
            "entity_id": entity_id,
            "body": body,
            "tags": tags or [],
            "slots": slots or [],
            "change_kind": change_kind,
            "correction_of": correction_of,
            "correction_reason": correction_reason,
            "previous_revision_id": previous_revision_id,
            "source": source or inferred_source(),
        },
    }


def slot(slot_id: str, revision_id: str, *roles: str) -> Json:
    return {"slot_id": slot_id, "revision_id": revision_id, "roles": list(roles)}


def package(
    key: str,
    reason: str,
    expected: list[Json],
    records: list[Json],
    *,
    root_change: Json | None = None,
    publish: Json | None = None,
) -> Json:
    return {
        "protocol_version": 1,
        "idempotency_key": key,
        "actor": "human:acceptance",
        "reason": reason,
        "expected_heads": expected,
        "records": records,
        "root_change": root_change,
        "publish": publish,
    }


def expected_head(project_id: str, revision_id: str | None, stage: str = "working") -> list[Json]:
    return [{"project_id": project_id, "stage": stage, "revision_id": revision_id}]


def root_change(project_id: str, revision_id: str, before: str, after: str) -> Json:
    return {
        "project_id": project_id,
        "stage": "working",
        "after_revision_id": revision_id,
        "reason": after,
        "meaningful": True,
        "decision": {"before": before, "after": after, "rationale": after},
    }


def find_path(tree: Json, path: list[str]) -> Json:
    node = tree
    for component in path:
        matches = [child for child in node.get("children", []) if child.get("slot_id") == component]
        require(len(matches) == 1, f"slot path {path!r} is ambiguous/missing at {component!r}")
        node = matches[0]
    return node


@dataclass
class SoftwareState:
    project: str
    initial_root: str
    current_root: str
    initial_seq: int
    second_seq: int
    old_atom_entity: str
    old_atom_revision: str
    new_atom_entity: str
    new_atom_revision: str
    be_path: list[str]
    fe_path: list[str]
    all_record_ids: set[str]
    receipt_keys: set[str]


@dataclass
class ModelingState:
    project: str
    root: str
    target_revision: str
    artifact: str
    observation: str
    old_goal: str
    old_baseline: str
    old_assessment: str
    new_goal: str
    new_baseline: str
    new_assessment: str
    initial_seq: int
    all_record_ids: set[str]


class Suite:
    def __init__(self, api: Api, prefix: str) -> None:
        self.api = api
        self.prefix = prefix
        self._steps = 0

    def ident(self, suffix: str) -> str:
        return f"{self.prefix}_{suffix}"

    def ok(self, name: str) -> None:
        self._steps += 1
        print(f"ok {self._steps} - {name}", flush=True)

    def wait_ready(self, wait_seconds: float) -> None:
        deadline = time.monotonic() + wait_seconds
        while True:
            try:
                ready = self.api.get("/health/ready")
                require(ready.get("status") == "ready", f"unexpected readiness body: {ready!r}")
                require(ready.get("protocol_version") == 1, "API protocol is not v1")
                self.ok("real API reports ready")
                return
            except (ApiError, AcceptanceFailure):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.25)

    def apply(self, body: Json) -> Json:
        result = self.api.post("/api/packages/apply", body)
        require(isinstance(result.get("receipt"), dict), f"missing receipt: {result!r}")
        return result

    def project_state(self, project_id: str) -> Json:
        state = self.api.get("/api/state", project_id=project_id, limit=1)
        projects = [p for p in state.get("projects", []) if p.get("project_id") == project_id]
        require(len(projects) == 1, f"project {project_id} missing from state")
        return projects[0]

    def head(self, project_id: str) -> str:
        head = self.project_state(project_id).get("working_head")
        require(isinstance(head, str), f"project {project_id} has no working head")
        return head

    def official_head(self, project_id: str) -> str | None:
        head = self.project_state(project_id).get("official_head")
        require(head is None or isinstance(head, str), f"project {project_id} has invalid official head")
        return head

    def software_domain(self) -> SoftwareState:
        p = self.ident("sw_project")
        ids = {name: self.ident("sw_" + name) for name in (
            "rev_root", "ent_schema", "rev_schema", "ent_backend", "rev_backend",
            "ent_auth", "rev_auth", "ent_session", "rev_session", "ent_policy",
            "rev_policy", "ent_frontend", "rev_frontend", "ent_atom", "rev_atom",
        )}
        be_path = ["login_schema", "backend", "auth", "session", "policy", "shared_policy"]
        fe_path = ["login_schema", "frontend", "shared_policy"]
        records = [
            project_record(p, "Acceptance software project"),
            entity_record(ids["ent_schema"], p, "schema", "Login schema"),
            entity_record(ids["ent_backend"], p, "core", "Backend"),
            entity_record(ids["ent_auth"], p, "core", "Authentication"),
            entity_record(ids["ent_session"], p, "core", "Session"),
            entity_record(ids["ent_policy"], p, "core", "Policy"),
            entity_record(ids["ent_frontend"], p, "core", "Frontend"),
            entity_record(ids["ent_atom"], p, "idea", "Shared lockout policy", tags=["lockout"]),
            revision_record(ids["rev_atom"], ids["ent_atom"], "계정 잠금 30분 공유 정책"),
            revision_record(
                ids["rev_policy"], ids["ent_policy"], "Policy core",
                slots=[slot("shared_policy", ids["rev_atom"], "be")],
            ),
            revision_record(
                ids["rev_session"], ids["ent_session"], "Session core",
                slots=[slot("policy", ids["rev_policy"], "be")],
            ),
            revision_record(
                ids["rev_auth"], ids["ent_auth"], "Authentication core",
                slots=[slot("session", ids["rev_session"], "be")],
            ),
            revision_record(
                ids["rev_backend"], ids["ent_backend"], "Backend core",
                slots=[slot("auth", ids["rev_auth"], "be")],
            ),
            revision_record(
                ids["rev_frontend"], ids["ent_frontend"], "Frontend core",
                slots=[slot("shared_policy", ids["rev_atom"], "fe")],
            ),
            revision_record(
                ids["rev_schema"], ids["ent_schema"], "Login schema body",
                slots=[
                    slot("backend", ids["rev_backend"], "be"),
                    slot("frontend", ids["rev_frontend"], "fe"),
                ],
            ),
            revision_record(
                ids["rev_root"], p, "Software project root",
                slots=[slot("login_schema", ids["rev_schema"], "software")],
            ),
        ]
        initial_key = self.ident("sw_initial_pkg")
        initial = self.apply(package(
            initial_key,
            "Create six-depth diamond fixture without an LLM",
            expected_head(p, None) + expected_head(p, None, "official"),
            records,
            root_change=root_change(p, ids["rev_root"], "none", "initial software graph"),
            publish={
                "project_id": p,
                "root_revision_id": ids["rev_root"],
                "label": "software-v1",
                "published_at": "2026-01-01T00:00:00Z",
                "notes": "initial acceptance publication",
            },
        ))
        initial_seq = initial["receipt"]["seq"]
        snapshot = self.api.get("/api/snapshot", project_id=p, stage="working", depth=32, max_nodes=200)
        require(snapshot.get("root_revision_id") == ids["rev_root"], "initial root head mismatch")
        require(snapshot.get("max_depth_reached", -1) >= 6, "fixture did not reach six levels")
        require(find_path(snapshot["tree"], be_path)["revision_id"] == ids["rev_atom"], "BE atom missing")
        require(find_path(snapshot["tree"], fe_path)["revision_id"] == ids["rev_atom"], "FE atom missing")
        occurrences = self.api.get("/api/occurrences", project_id=p, entity_id=ids["ent_atom"], stage="working", max_nodes=200)
        paths = {tuple(item["slot_path"]): tuple(item["roles"]) for item in occurrences["occurrences"]}
        require(paths.get(tuple(be_path)) == ("be",), "BE occurrence role/path missing")
        require(paths.get(tuple(fe_path)) == ("fe",), "FE occurrence role/path missing")
        self.ok("software graph has six-depth diamond and contextual roles")

        new_entity = self.ident("sw_ent_atom_10m")
        new_revision = self.ident("sw_rev_atom_10m")
        replace_key = self.ident("sw_replace_be")
        replace_body = {
            "protocol_version": 1,
            "idempotency_key": replace_key,
            "actor": "human:acceptance",
            "reason": "Change only the backend occurrence",
            "project_id": p,
            "stage": "working",
            "expected_root_revision_id": ids["rev_root"],
            "slot_path": be_path,
            "replacement": {
                "mode": "new_revision",
                "new_revision": revision_record(
                    new_revision,
                    new_entity,
                    "계정 잠금 10분 백엔드 정책",
                    change_kind="semantic",
                ),
                "new_records": [entity_record(
                    new_entity,
                    p,
                    "idea",
                    "Backend lockout policy",
                    tags=["lockout"],
                    derived_from=[ids["ent_atom"]],
                    lineage_kind="semantic_edit",
                )],
                "target_revision_id": None,
            },
            "root_change_reason": "Backend-only policy change",
            "decision": {
                "before": "30 minute shared policy",
                "after": "10 minute backend policy",
                "rationale": "exercise occurrence isolation",
            },
        }
        # occurrences/replace embeds a revision without its outer kind.
        replace_body["replacement"]["new_revision"] = {
            "id": new_revision,
            "data": replace_body["replacement"]["new_revision"]["data"],
        }
        replaced = self.api.post("/api/occurrences/replace", replace_body)
        new_root = replaced.get("new_root_revision_id")
        require(isinstance(new_root, str) and new_root != ids["rev_root"], "replace did not create a root")
        mapping_paths = {tuple(item["slot_path"]) for item in replaced.get("mapping", [])}
        require(tuple(be_path) in mapping_paths and () in mapping_paths, "replace mapping omits path/root")

        old_snapshot = self.api.get("/api/snapshot", project_id=p, root_revision_id=ids["rev_root"], depth=32)
        new_snapshot = self.api.get("/api/snapshot", project_id=p, root_revision_id=new_root, depth=32)
        require(find_path(old_snapshot["tree"], be_path)["revision_id"] == ids["rev_atom"], "old BE path mutated")
        require(find_path(old_snapshot["tree"], fe_path)["revision_id"] == ids["rev_atom"], "old FE path mutated")
        require(find_path(new_snapshot["tree"], be_path)["revision_id"] == new_revision, "new BE path not replaced")
        require(find_path(new_snapshot["tree"], fe_path)["revision_id"] == ids["rev_atom"], "diamond peer path changed")
        pinned = {(tuple(item["slot_path"]), item["revision_id"]) for item in replaced.get("pinned_unchanged", [])}
        require((tuple(fe_path), ids["rev_atom"]) in pinned, "response did not prove peer pinning")
        self.ok("path edit copies ancestors and preserves peer occurrence/history")

        publish_key = self.ident("sw_publish_v2")
        official_before_v2 = self.official_head(p)
        require(official_before_v2 == ids["rev_root"], "initial official head mismatch before v2 publish")
        published = self.apply(package(
            publish_key,
            "Publish the edited software root",
            expected_head(p, official_before_v2, "official"),
            [],
            publish={
                "project_id": p,
                "root_revision_id": new_root,
                "label": "software-v2",
                "published_at": "2026-01-02T00:00:00Z",
                "notes": "backdated publication recorded later",
            },
        ))
        second_seq = published["receipt"]["seq"]
        require(published["receipt"].get("publication_id"), "second publication not recorded")

        # A later fractional UTC instant must sort after an exact-second instant,
        # even when the latter was supplied with an equivalent non-UTC offset.
        exact_key = self.ident("sw_publish_exact_second")
        official_before_exact = self.official_head(p)
        require(official_before_exact == new_root, "v2 official head mismatch before exact-second publish")
        exact_publication = self.apply(package(
            exact_key,
            "Publish old root at an exact second using a timezone offset",
            expected_head(p, official_before_exact, "official"),
            [],
            publish={
                "project_id": p,
                "root_revision_id": ids["rev_root"],
                "label": "software-exact-second",
                "published_at": "2026-01-04T09:00:00+09:00",
                "notes": "equivalent to 2026-01-04T00:00:00Z",
            },
        ))
        fractional_key = self.ident("sw_publish_fractional_second")
        official_before_fraction = self.official_head(p)
        require(official_before_fraction == ids["rev_root"], "exact-second official head mismatch")
        self.apply(package(
            fractional_key,
            "Publish new root one millisecond later",
            expected_head(p, official_before_fraction, "official"),
            [],
            publish={
                "project_id": p,
                "root_revision_id": new_root,
                "label": "software-fractional-second",
                "published_at": "2026-01-04T00:00:00.001Z",
                "notes": "fractional timestamp ordering regression",
            },
        ))
        at_exact_second = self.api.get(
            "/api/snapshot", project_id=p, stage="official",
            effective_at="2026-01-04T09:00:00+09:00", depth=32,
        )
        after_fraction = self.api.get(
            "/api/snapshot", project_id=p, stage="official",
            effective_at="2026-01-04T00:00:00.002Z", depth=32,
        )
        before_fraction_was_known = self.api.get(
            "/api/snapshot", project_id=p, stage="official",
            known_seq=exact_publication["receipt"]["seq"],
            effective_at="2026-01-04T00:00:00.002Z", depth=32,
        )
        require(at_exact_second.get("root_revision_id") == ids["rev_root"],
                "timezone-equivalent exact-second cutoff included later fractional event")
        require(after_fraction.get("root_revision_id") == new_root,
                "fractional event did not sort after exact-second event")
        require(before_fraction_was_known.get("root_revision_id") == ids["rev_root"],
                "knowledge cutoff leaked fractional publication")
        self.ok("fractional and timezone-equivalent publication times order chronologically")

        early = self.api.get(
            "/api/snapshot", project_id=p, stage="official",
            effective_at="2026-01-01T12:00:00Z", depth=32,
        )
        late = self.api.get(
            "/api/snapshot", project_id=p, stage="official",
            effective_at="2026-01-03T00:00:00Z", depth=32,
        )
        formerly_known = self.api.get(
            "/api/snapshot", project_id=p, stage="official", known_seq=initial_seq,
            effective_at="2026-01-03T00:00:00Z", depth=32,
        )
        require(early.get("root_revision_id") == ids["rev_root"], "effective-time snapshot selected future publication")
        require(late.get("root_revision_id") == new_root, "latest effective publication missing")
        require(formerly_known.get("root_revision_id") == ids["rev_root"], "knowledge cutoff leaked later publication")
        self.ok("official snapshots separate effective time from knowledge time")

        search_body = {
            "project_id": p,
            "query": "10분 백엔드 정책",
            "stage": "official",
            "known_seq": None,
            "known_at": None,
            "effective_at": "2026-01-03T00:00:00Z",
            "roles": ["be"],
            "tags": [],
            "lanes": ["official"],
            "limit": 20,
            "vector": None,
        }
        be_search = self.api.post("/api/search", search_body)
        require(new_revision in {row["revision_id"] for row in be_search.get("results", [])}, "BE role search missed edited atom")
        require(be_search.get("vector_status", {}).get("used") is False, "no-vector search did not report lexical fallback")
        fe_body = dict(search_body)
        fe_body["roles"] = ["fe"]
        fe_search = self.api.post("/api/search", fe_body)
        require(new_revision not in {row["revision_id"] for row in fe_search.get("results", [])}, "hard role filter leaked BE result")
        require(new_revision in {row["revision_id"] for row in fe_search.get("related", [])}, "role mismatch was not separated as related")
        past_body = dict(search_body)
        past_body["effective_at"] = "2026-01-01T12:00:00Z"
        past = self.api.post("/api/search", past_body)
        past_ids = {row["revision_id"] for lane in ("results", "related") for row in past.get(lane, [])}
        require(new_revision not in past_ids, "future revision leaked into historical search")
        self.ok("historical role search is strict and falls back without vectors")

        return SoftwareState(
            project=p,
            initial_root=ids["rev_root"],
            current_root=new_root,
            initial_seq=initial_seq,
            second_seq=second_seq,
            old_atom_entity=ids["ent_atom"],
            old_atom_revision=ids["rev_atom"],
            new_atom_entity=new_entity,
            new_atom_revision=new_revision,
            be_path=be_path,
            fe_path=fe_path,
            all_record_ids={p, *ids.values(), new_entity, new_revision},
            receipt_keys={initial_key, replace_key, publish_key, exact_key, fractional_key},
        )

    def modeling_domain(self) -> ModelingState:
        p = self.ident("ml_project")
        names = (
            "rev_root", "ent_schema", "rev_schema", "ent_h0", "rev_h0", "ent_h1",
            "rev_h1", "artifact", "observation", "goal_old", "baseline_old",
            "assessment_old", "goal_new", "baseline_new", "assessment_new",
        )
        i = {name: self.ident("ml_" + name) for name in names}
        path = ["research_schema", "hypothesis"]
        records = [
            project_record(p, "Acceptance modeling project"),
            entity_record(i["ent_schema"], p, "schema", "Modeling schema"),
            entity_record(i["ent_h0"], p, "idea", "Original hypothesis"),
            revision_record(i["rev_h0"], i["ent_h0"], "기존 모델은 기준 성능을 낼 것이다"),
            entity_record(
                i["ent_h1"], p, "idea", "Derived hypothesis",
                derived_from=[i["ent_h0"]], lineage_kind="semantic_edit",
            ),
            revision_record(
                i["rev_h1"], i["ent_h1"], "새 특징을 추가하면 기준 성능을 넘을 것이다",
                change_kind="semantic",
            ),
            revision_record(
                i["rev_schema"], i["ent_schema"], "Research schema",
                slots=[slot("hypothesis", i["rev_h1"], "research")],
            ),
            revision_record(
                i["rev_root"], p, "Modeling project root",
                slots=[slot("research_schema", i["rev_schema"], "research")],
            ),
            {
                "id": i["artifact"],
                "kind": "artifact",
                "data": {
                    "uri": "file:///acceptance/model-run.json",
                    "digest": "sha256:" + "a" * 64,
                    "digest_alg": "sha256",
                    "size_bytes": 81234,
                    "media_type": "application/json",
                    "included_in_export": False,
                    "note": "external bytes intentionally absent",
                },
            },
            {
                "id": i["observation"],
                "kind": "observation",
                "data": {
                    "project_id": p,
                    "target_revision_id": i["rev_h1"],
                    "metric": "f1",
                    "value": None,
                    "unit": "score",
                    "status": "failed",
                    "occurred_at": "2026-03-01T09:00:00Z",
                    "method": "held-out evaluation",
                    "environment": {
                        "code_ref": "git:acceptance123",
                        "data_version": "dataset-v1",
                        "model_version": "model-v2",
                        "config_digest": "sha256:" + "b" * 64,
                        "runtime": "acceptance-runtime",
                    },
                    "artifact_ids": [i["artifact"]],
                    "capture_id": None,
                    "actor": "human:acceptance",
                    "note": "runner failed before metric production",
                },
            },
            {
                "id": i["goal_old"],
                "kind": "goal",
                "data": {
                    "project_id": p,
                    "scope": {"root_revision_id": i["rev_root"], "slot_path": path, "target_revision_id": i["rev_h1"]},
                    "statement": "모델 F1이 기준을 충족한다",
                    "criteria": [{
                        "criterion_id": "f1_gate", "kind": "quantitative", "statement": "F1 minimum",
                        "metric": "f1", "comparator": "gte", "threshold": 0.80, "unit": "score", "required": True,
                    }],
                    "origin": "official",
                    "actor": "human:acceptance",
                },
            },
            {
                "id": i["baseline_old"],
                "kind": "baseline",
                "data": {
                    "goal_id": i["goal_old"], "criterion_ids": ["f1_gate"],
                    "constraints": ["dataset-v1"], "effective_from": "2026-03-01T00:00:00Z",
                    "supersedes": None, "actor": "human:acceptance", "reason": "initial baseline",
                },
            },
        ]
        initial = self.apply(package(
            self.ident("ml_initial_pkg"), "Create modeling evidence fixture", expected_head(p, None), records,
            root_change=root_change(p, i["rev_root"], "none", "initial modeling graph"),
        ))
        seq = initial["receipt"]["seq"]
        old_assessment_record = {
            "id": i["assessment_old"],
            "kind": "assessment",
            "data": {
                "root_revision_id": i["rev_root"], "slot_path": path, "target_revision_id": i["rev_h1"],
                "baseline_id": i["baseline_old"], "evidence_cutoff_seq": seq,
                "evidence_cutoff_at": "2026-03-02T00:00:00Z", "evaluator": "human:acceptance",
                "rubric_version": "rubric-v1", "origin": "official", "status": "unknown",
                "criteria_results": [{"criterion_id": "f1_gate", "status": "unknown", "observed_value": None, "note": "run failed"}],
                "evidence_observation_ids": [i["observation"]], "note": "failure is not an unmet metric",
            },
        }
        assessed = self.apply(package(
            self.ident("ml_assess_old_pkg"), "Assess initial baseline", expected_head(p, i["rev_root"]),
            [old_assessment_record],
        ))
        old_assessment_seq = assessed["receipt"]["seq"]

        changed_records = [
            {
                "id": i["goal_new"], "kind": "goal", "data": {
                    "project_id": p,
                    "scope": {"root_revision_id": i["rev_root"], "slot_path": path, "target_revision_id": i["rev_h1"]},
                    "statement": "모델 F1이 강화된 기준을 충족한다",
                    "criteria": [{
                        "criterion_id": "f1_gate_v2", "kind": "quantitative", "statement": "Stricter F1 minimum",
                        "metric": "f1", "comparator": "gte", "threshold": 0.90, "unit": "score", "required": True,
                    }],
                    "origin": "official", "actor": "human:acceptance",
                },
            },
            {
                "id": i["baseline_new"], "kind": "baseline", "data": {
                    "goal_id": i["goal_new"], "criterion_ids": ["f1_gate_v2"],
                    "constraints": ["dataset-v2"], "effective_from": "2026-03-03T00:00:00Z",
                    "supersedes": i["baseline_old"], "actor": "human:acceptance", "reason": "threshold and dataset changed",
                },
            },
        ]
        changed = self.apply(package(
            self.ident("ml_baseline_new_pkg"), "Create changed baseline and immutable goal", expected_head(p, i["rev_root"]),
            changed_records,
        ))
        new_baseline_seq = changed["receipt"]["seq"]
        new_assessment_record = {
            "id": i["assessment_new"], "kind": "assessment", "data": {
                "root_revision_id": i["rev_root"], "slot_path": path, "target_revision_id": i["rev_h1"],
                "baseline_id": i["baseline_new"], "evidence_cutoff_seq": new_baseline_seq,
                "evidence_cutoff_at": "2026-03-04T00:00:00Z", "evaluator": "human:acceptance",
                "rubric_version": "rubric-v2", "origin": "official", "status": "unknown",
                "criteria_results": [{"criterion_id": "f1_gate_v2", "status": "unknown", "observed_value": None, "note": "new run required"}],
                "evidence_observation_ids": [], "note": "old evidence was not inherited",
            },
        }
        self.apply(package(
            self.ident("ml_assess_new_pkg"), "Assess changed baseline independently", expected_head(p, i["rev_root"]),
            [new_assessment_record],
        ))

        old_after = self.api.get(f"/api/records/{i['assessment_old']}")["record"]
        require(old_after["seq"] == old_assessment_seq, "old assessment sequence changed")
        require(old_after["data"] == old_assessment_record["data"], "old assessment was overwritten")
        new_after = self.api.get(f"/api/records/{i['assessment_new']}")["record"]
        require(new_after["data"]["evidence_observation_ids"] == [], "new baseline inherited old evidence")
        goals = self.api.get("/api/goals", project_id=p, stage="working", max_nodes=200)
        goal_ids = {goal["goal_id"] for goal in goals.get("goals", [])}
        require({i["goal_old"], i["goal_new"]} <= goal_ids, "baseline goal history missing")
        old_detail = self.api.get(
            f"/api/records/{i['rev_h1']}", known_seq=seq, effective_at="2026-03-02T00:00:00Z",
        )
        visible_assessments = {row["id"] for row in old_detail.get("evidence", {}).get("assessments", [])}
        require(i["assessment_old"] not in visible_assessments and i["assessment_new"] not in visible_assessments,
                "historical detail leaked assessments recorded after cutoff")
        self.ok("modeling baseline change preserves prior assessment and evidence cutoff")

        return ModelingState(
            project=p, root=i["rev_root"], target_revision=i["rev_h1"], artifact=i["artifact"],
            observation=i["observation"], old_goal=i["goal_old"], old_baseline=i["baseline_old"],
            old_assessment=i["assessment_old"], new_goal=i["goal_new"], new_baseline=i["baseline_new"],
            new_assessment=i["assessment_new"], initial_seq=seq,
            all_record_ids={p, *i.values()},
        )

    def planning_domain(self) -> tuple[str, set[str]]:
        p = self.ident("plan_project")
        i = {name: self.ident("plan_" + name) for name in (
            "rev_root1", "rev_root2", "ent_schema", "rev_schema1", "rev_schema2",
            "ent_brief", "rev_brief", "ent_owner", "rev_owner", "goal", "baseline",
            "assessment", "capture", "candidate", "promotion", "ent_promoted", "rev_promoted",
            "observation",
        )}
        goal_path = ["planning_schema", "brief"]
        initial_records = [
            project_record(p, "Acceptance non-development planning"),
            entity_record(i["ent_schema"], p, "schema", "Campaign planning"),
            entity_record(i["ent_brief"], p, "idea", "Campaign brief"),
            revision_record(i["rev_brief"], i["ent_brief"], "캠페인 목적과 독자를 설명한다"),
            entity_record(i["ent_owner"], p, "idea", "Owner assignment"),
            revision_record(i["rev_owner"], i["ent_owner"], "담당자를 정한다"),
            revision_record(
                i["rev_schema1"], i["ent_schema"], "Campaign planning schema",
                slots=[slot("brief", i["rev_brief"], "planning"), slot("owner", i["rev_owner"], "planning")],
            ),
            revision_record(
                i["rev_root1"], p, "Planning project root",
                slots=[slot("planning_schema", i["rev_schema1"], "planning")],
            ),
            {
                "id": i["goal"], "kind": "goal", "data": {
                    "project_id": p,
                    "scope": {"root_revision_id": i["rev_root1"], "slot_path": goal_path, "target_revision_id": i["rev_brief"]},
                    "statement": "캠페인 브리프가 사람이 검토할 수 있다",
                    "criteria": [{
                        "criterion_id": "reviewable", "kind": "qualitative", "statement": "검토자가 목적과 독자를 이해한다",
                        "metric": None, "comparator": None, "threshold": None, "unit": None, "required": True,
                    }],
                    "origin": "official", "actor": "human:acceptance",
                },
            },
            {
                "id": i["baseline"], "kind": "baseline", "data": {
                    "goal_id": i["goal"], "criterion_ids": ["reviewable"], "constraints": ["human review"],
                    "effective_from": "2026-04-01T00:00:00Z", "supersedes": None,
                    "actor": "human:acceptance", "reason": "qualitative acceptance baseline",
                },
            },
            {
                "id": i["observation"], "kind": "observation", "data": {
                    "project_id": p, "target_revision_id": i["rev_brief"],
                    "metric": "review_feedback", "value": "검토자가 목적과 독자를 이해함",
                    "unit": None, "status": "observed", "occurred_at": "2026-04-01T12:00:00Z",
                    "method": "structured human review",
                    "environment": {
                        "code_ref": None, "data_version": None, "model_version": None,
                        "config_digest": None, "runtime": None,
                    },
                    "artifact_ids": [], "capture_id": None, "actor": "human:acceptance",
                    "note": "qualitative evidence must not be forced into a number",
                },
            },
        ]
        initial = self.apply(package(
            self.ident("plan_initial_pkg"), "Create qualitative planning fixture", expected_head(p, None), initial_records,
            root_change=root_change(p, i["rev_root1"], "none", "initial planning graph"),
        ))
        seq = initial["receipt"]["seq"]
        assessment = {
            "id": i["assessment"], "kind": "assessment", "data": {
                "root_revision_id": i["rev_root1"], "slot_path": goal_path, "target_revision_id": i["rev_brief"],
                "baseline_id": i["baseline"], "evidence_cutoff_seq": seq,
                "evidence_cutoff_at": "2026-04-02T00:00:00Z", "evaluator": "human:acceptance",
                "rubric_version": "qualitative-v1", "origin": "official", "status": "met",
                "criteria_results": [{"criterion_id": "reviewable", "status": "met", "observed_value": None, "note": "human review"}],
                "evidence_observation_ids": [i["observation"]], "note": "qualitative decision",
            },
        }
        self.apply(package(
            self.ident("plan_assess_pkg"), "Record qualitative assessment", expected_head(p, i["rev_root1"]), [assessment],
        ))
        goals = self.api.get("/api/goals", project_id=p, stage="working", max_nodes=200)
        require(any(item.get("revision_id") == i["rev_owner"] for item in goals.get("missing_goal_occurrences", [])),
                "missing goal occurrence was not surfaced")
        official_goal = next(goal for goal in goals.get("goals", []) if goal["goal_id"] == i["goal"])
        require(official_goal.get("gate_status") == "met", "qualitative gate did not use explicit assessment")

        capture_text = "새 캠페인의 승인 기준과 담당자를 명시한다."
        capture_body = {
            "id": i["capture"], "project_id": p, "content": capture_text,
            "media_type": "text/plain", "source_kind": "note", "source_ref": "acceptance://planning",
            "occurred_at": "2026-04-03T00:00:00Z", "content_digest": None,
        }
        captured = self.api.post("/api/captures", capture_body)
        replayed = self.api.post("/api/captures", capture_body)
        require(captured["capture"]["id"] == i["capture"] and captured.get("replay") is False, "capture creation failed")
        require(replayed["capture"]["seq"] == captured["capture"]["seq"] and replayed.get("replay") is True,
                "explicit capture ID retry was not idempotent")
        changed_capture = dict(capture_body)
        changed_capture["content"] = capture_text + " 변경"
        self.expect_error(lambda: self.api.post("/api/captures", changed_capture), 409, "idempotency_conflict")

        candidate_record = {
            "id": i["candidate"], "kind": "candidate", "data": {
                "project_id": p, "capture_id": i["capture"], "proposed_kind": "idea",
                "title": "Approval criterion and owner", "body": capture_text,
                "origin": "human", "model": None, "skill": None, "claim_mode": "extracted",
                "source_anchor": {"capture_id": i["capture"], "start": 0, "end": len(capture_text)},
                "status": "pending",
            },
        }
        self.apply(package(
            self.ident("plan_candidate_pkg"), "Store a pending human candidate", expected_head(p, i["rev_root1"]), [candidate_record],
        ))
        candidate_search = self.api.post("/api/search", {
            "project_id": p, "query": "승인 기준 담당자", "stage": "working",
            "known_seq": None, "known_at": None, "effective_at": None, "roles": [], "tags": [],
            "lanes": ["candidate"], "limit": 20, "vector": None,
        })
        require(i["candidate"] in {row["candidate_id"] for row in candidate_search.get("candidates", [])},
                "pending candidate not searchable in separate lane")
        require(i["rev_promoted"] not in {row["revision_id"] for row in candidate_search.get("results", [])},
                "pending candidate leaked into formal results")

        promoted_revision = revision_record(
            i["rev_promoted"], i["ent_promoted"], capture_text,
            source=extracted_source(i["capture"], capture_text),
        )
        promoted_records = [
            entity_record(i["ent_promoted"], p, "idea", "Approval criterion and owner"),
            promoted_revision,
            {
                "id": i["promotion"], "kind": "promotion", "data": {
                    "candidate_id": i["candidate"], "entity_id": i["ent_promoted"],
                    "revision_id": i["rev_promoted"], "actor": "human:acceptance", "reason": "reviewed and adopted",
                },
            },
            revision_record(
                i["rev_schema2"], i["ent_schema"], "Campaign planning schema",
                slots=[
                    slot("brief", i["rev_brief"], "planning"),
                    slot("owner", i["rev_owner"], "planning"),
                    slot("approval", i["rev_promoted"], "planning"),
                ],
                change_kind="composition", previous_revision_id=i["rev_schema1"],
            ),
            revision_record(
                i["rev_root2"], p, "Planning project root",
                slots=[slot("planning_schema", i["rev_schema2"], "planning")],
                change_kind="composition", previous_revision_id=i["rev_root1"],
            ),
        ]
        self.apply(package(
            self.ident("plan_promote_pkg"), "Promote candidate and include it in a new composition",
            expected_head(p, i["rev_root1"]), promoted_records,
            root_change=root_change(p, i["rev_root2"], "candidate pending", "candidate promoted and composed"),
        ))
        promoted_snapshot = self.api.get("/api/snapshot", project_id=p, stage="working", depth=32)
        require(find_path(promoted_snapshot["tree"], ["planning_schema", "approval"])["revision_id"] == i["rev_promoted"],
                "promoted candidate was not included in new composition")
        candidate_after = self.api.get(f"/api/records/{i['candidate']}")["record"]
        promotion_after = self.api.get(f"/api/records/{i['promotion']}")["record"]
        require(candidate_after["data"]["status"] == "pending", "immutable candidate was rewritten")
        require(promotion_after["data"]["candidate_id"] == i["candidate"], "promotion provenance missing")
        self.ok("planning candidate stays pending evidence while promotion enters a new composition")
        return p, {p, *i.values()}

    def expect_error(
        self,
        action: Callable[[], Any],
        status: int,
        code: str,
        issue_code: str | None = None,
    ) -> ApiError:
        try:
            action()
        except ApiError as exc:
            require(exc.status == status, f"expected HTTP {status}, got {exc.status}: {exc.body!r}")
            require(exc.code == code, f"expected error {code}, got {exc.code}: {exc.body!r}")
            if issue_code:
                require(issue_code in exc.issue_codes, f"expected issue {issue_code}, got {exc.issue_codes}: {exc.body!r}")
            return exc
        raise AcceptanceFailure(f"expected HTTP {status}/{code}, request succeeded")

    def prefix_snapshot(self) -> tuple[int, set[str], Json]:
        exported = self.api.get("/api/export", include_receipts="true")
        records = exported["content"]["records"]
        return exported["content"]["seq"], {row["id"] for row in records if row["id"].startswith(self.prefix)}, exported

    def expect_atomic_reject(
        self,
        body: Json,
        status: int,
        code: str,
        record_ids: set[str],
        issue_code: str | None = None,
    ) -> ApiError:
        before_seq, before_ids, _ = self.prefix_snapshot()
        exc = self.expect_error(lambda: self.api.post("/api/packages/apply", body), status, code, issue_code)
        after_seq, after_ids, _ = self.prefix_snapshot()
        require(after_seq == before_seq, f"rejected package advanced global seq {before_seq}->{after_seq}")
        require(after_ids == before_ids, "rejected package changed prefixed record set")
        for record_id in record_ids:
            self.expect_error(lambda rid=record_id: self.api.get(f"/api/records/{rid}"), 404, "not_found")
        return exc

    def adversarial_transactions(self, sw: SoftwareState, planning_project: str) -> None:
        p = sw.project
        head = self.head(p)
        bad_ent = self.ident("badref_ent")
        bad_rev = self.ident("badref_rev")
        missing = self.ident("badref_missing")
        bad_reference = package(
            self.ident("badref_pkg"), "must reject unknown reference", expected_head(p, head),
            [
                entity_record(bad_ent, p, "core", "Bad reference core"),
                revision_record(bad_rev, bad_ent, "Bad reference", slots=[slot("missing", missing)]),
            ],
        )
        self.expect_atomic_reject(bad_reference, 422, "validation_failed", {bad_ent, bad_rev}, "unknown_reference")

        ent_a, ent_b = self.ident("cycle_ent_a"), self.ident("cycle_ent_b")
        rev_a, rev_b = self.ident("cycle_rev_a"), self.ident("cycle_rev_b")
        cycle = package(
            self.ident("cycle_pkg"), "must reject cyclic composition", expected_head(p, head),
            [
                entity_record(ent_a, p, "core", "Cycle A"), entity_record(ent_b, p, "core", "Cycle B"),
                revision_record(rev_a, ent_a, "Cycle A", slots=[slot("b", rev_b)]),
                revision_record(rev_b, ent_b, "Cycle B", slots=[slot("a", rev_a)]),
            ],
        )
        self.expect_atomic_reject(cycle, 422, "validation_failed", {ent_a, ent_b, rev_a, rev_b}, "cycle_detected")

        capture_id = self.ident("source_capture")
        source_text = "정확한 원문"
        self.api.post("/api/captures", {
            "id": capture_id, "project_id": p, "content": source_text, "media_type": "text/plain",
            "source_kind": "note", "source_ref": None, "occurred_at": "2026-05-01T00:00:00Z", "content_digest": None,
        })
        source_ent, source_rev = self.ident("source_ent"), self.ident("source_rev")
        mismatched = revision_record(
            source_rev, source_ent, "원문과 다른 본문", source=extracted_source(capture_id, source_text),
        )
        bad_source = package(
            self.ident("source_pkg"), "must reject a false extracted claim", expected_head(p, head),
            [entity_record(source_ent, p, "idea", "False extraction"), mismatched],
        )
        self.expect_atomic_reject(
            bad_source, 422, "validation_failed", {source_ent, source_rev}, "source_anchor_text_mismatch"
        )

        stale_ent, stale_rev = self.ident("stale_ent"), self.ident("stale_rev")
        stale = package(
            self.ident("stale_pkg"), "must reject stale head", expected_head(p, sw.initial_root),
            [entity_record(stale_ent, p, "idea", "Stale write"), revision_record(stale_rev, stale_ent, "Stale write")],
        )
        self.expect_atomic_reject(stale, 409, "head_conflict", {stale_ent, stale_rev})
        require(self.head(p) == head, "adversarial failures changed working head")
        require(self.head(planning_project), "planning project disappeared during invalid writes")
        self.ok("invalid reference/cycle/source/stale packages roll back atomically")

    def idempotency(self, sw: SoftwareState) -> None:
        p, head = sw.project, self.head(sw.project)
        ent, rev = self.ident("idem_ent"), self.ident("idem_rev")
        key = self.ident("idem_pkg")
        body = package(
            key, "idempotent independent idea", expected_head(p, head),
            [entity_record(ent, p, "idea", "Idempotent idea"), revision_record(rev, ent, "Idempotent idea body")],
        )
        before_seq, _, _ = self.prefix_snapshot()
        first = self.apply(body)
        second = self.apply(body)
        r1, r2 = first["receipt"], second["receipt"]
        require(r1.get("replay") is False and r2.get("replay") is True, "replay flags are wrong")
        for field in ("seq", "request_digest", "records_written"):
            require(r1.get(field) == r2.get(field), f"idempotent receipt changed {field}")
        after_replay_seq, ids_after, _ = self.prefix_snapshot()
        require(after_replay_seq == r1["seq"], "idempotent retry advanced sequence")
        require(after_replay_seq > before_seq and {ent, rev} <= ids_after, "initial idempotent write missing")
        altered = dict(body)
        altered["reason"] = "same key with a different request digest"
        self.expect_atomic_reject(altered, 409, "idempotency_conflict", set())
        self.ok("digest-aware package replay is stable and conflicting reuse is rejected")

    def concurrent_same_head(self, sw: SoftwareState) -> None:
        p = sw.project
        current = self.head(p)
        requests: list[tuple[Json, str, str]] = []
        for label, path, parent_entity in (
            ("be", sw.be_path, sw.new_atom_entity),
            ("fe", sw.fe_path, sw.old_atom_entity),
        ):
            ent, rev = self.ident(f"race_{label}_ent"), self.ident(f"race_{label}_rev")
            revision = revision_record(rev, ent, f"{label} concurrent policy", change_kind="semantic")
            body = {
                "protocol_version": 1, "idempotency_key": self.ident(f"race_{label}_key"),
                "actor": "human:acceptance", "reason": f"concurrent {label} edit", "project_id": p,
                "stage": "working", "expected_root_revision_id": current, "slot_path": path,
                "replacement": {
                    "mode": "new_revision", "new_revision": {"id": rev, "data": revision["data"]},
                    "new_records": [entity_record(
                        ent, p, "idea", f"Concurrent {label} policy", derived_from=[parent_entity],
                        lineage_kind="semantic_edit",
                    )],
                    "target_revision_id": None,
                },
                "root_change_reason": f"concurrent {label} path edit",
                "decision": {"before": "current policy", "after": f"{label} race policy", "rationale": "CAS acceptance test"},
            }
            requests.append((body, ent, rev))

        barrier = threading.Barrier(2)

        def submit(item: tuple[Json, str, str]) -> tuple[str, Any, str, str]:
            body, ent, rev = item
            barrier.wait(timeout=5)
            try:
                return "ok", self.api.post("/api/occurrences/replace", body), ent, rev
            except ApiError as exc:
                return "error", exc, ent, rev

        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            outcomes = list(pool.map(submit, requests))
        successes = [item for item in outcomes if item[0] == "ok"]
        failures = [item for item in outcomes if item[0] == "error"]
        require(len(successes) == 1 and len(failures) == 1, f"same-head race outcome was {outcomes!r}")
        failure = failures[0][1]
        require(isinstance(failure, ApiError) and failure.status == 409 and failure.code == "head_conflict",
                f"race loser was not a head conflict: {failure!r}")
        winner = successes[0]
        require(self.head(p) == winner[1]["new_root_revision_id"], "race winner did not own final head")
        self.api.get(f"/api/records/{winner[2]}")
        self.api.get(f"/api/records/{winner[3]}")
        self.expect_error(lambda: self.api.get(f"/api/records/{failures[0][2]}"), 404, "not_found")
        self.expect_error(lambda: self.api.get(f"/api/records/{failures[0][3]}"), 404, "not_found")
        self.ok("concurrent same-head edits have exactly one atomic winner")

    def verify_export(self, project_id: str, expected_ids: set[str], *, unresolved_artifact: str | None = None) -> None:
        document = self.api.get("/api/export", project_id=project_id, include_receipts="true")
        require(document.get("format") == "idea_db.export" and document.get("format_version") == 1,
                "unexpected export format")
        require(document.get("scope", {}).get("project_id") == project_id, "export scope mismatch")
        require(document.get("digest") == canonical_digest(document["content"]), "export digest mismatch")
        records = document["content"]["records"]
        record_ids = [record["id"] for record in records]
        require(len(record_ids) == len(set(record_ids)), "export contains duplicate application IDs")
        require(expected_ids <= set(record_ids), f"export omitted records: {sorted(expected_ids - set(record_ids))}")
        seqs = [record["seq"] for record in records]
        require(seqs == sorted(seqs), "export does not preserve commit order")
        heads = [head for head in document["content"]["heads"] if head["project_id"] == project_id]
        require(len(heads) == 1 and heads[0].get("working_head"), "export omitted project head")
        manifest = document["content"]["manifest"]
        if unresolved_artifact:
            require(unresolved_artifact in manifest.get("unresolved_external_artifacts", []),
                    "unresolved external artifact absent from manifest")
            require(manifest.get("complete_backup") is False, "export falsely claimed complete backup")

    def export_integrity(self, sw: SoftwareState, ml: ModelingState, planning: tuple[str, set[str]]) -> None:
        self.verify_export(sw.project, sw.all_record_ids)
        self.verify_export(ml.project, ml.all_record_ids, unresolved_artifact=ml.artifact)
        self.verify_export(planning[0], planning[1])
        self.ok("project exports preserve IDs/order/heads/digest and disclose missing artifact bytes")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:8080", help="running idea_db API base URL")
    parser.add_argument(
        "--token",
        default=os.environ.get("IDEA_DB_TOKEN"),
        help="optional API token (defaults to IDEA_DB_TOKEN)",
    )
    parser.add_argument("--timeout", type=float, default=15.0, help="per-request timeout in seconds")
    parser.add_argument("--wait-seconds", type=float, default=30.0, help="maximum readiness wait")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    prefix = "t" + uuid.uuid4().hex[:14]
    suite = Suite(Api(args.base_url, args.token, args.timeout), prefix)
    print(f"TAP version 13\n# acceptance run prefix: {prefix}", flush=True)
    try:
        suite.wait_ready(args.wait_seconds)
        software = suite.software_domain()
        modeling = suite.modeling_domain()
        planning = suite.planning_domain()
        suite.adversarial_transactions(software, planning[0])
        suite.idempotency(software)
        suite.concurrent_same_head(software)
        suite.export_integrity(software, modeling, planning)
    except Exception as exc:
        print(f"not ok {suite._steps + 1} - {type(exc).__name__}: {exc}", flush=True)
        return 1
    print(f"1..{suite._steps}\n# all acceptance invariants passed", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
