#!/usr/bin/env python3
"""Thirty-step black-box lifecycle validation for a running idea_db API.

This suite writes only globally unique IDs through the stdio MCP client.  It
never clears the database.  A normal run executes deterministic local workloads,
records their real artifacts and observations, then writes a report that can be
replayed read-only after a restart or an export/import restore.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import time
import uuid
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from acceptance import (
    AcceptanceFailure,
    Api,
    canonical_digest,
    entity_record,
    expected_head,
    inferred_source,
    package,
    project_record,
    require,
    revision_record,
    slot,
)
from lifecycle_workloads import run_workloads


Json = dict[str, Any]
ROLES = ("fe", "be", "infra", "ml", "dl", "llm")
WORK_IDS = tuple(f"{role}_{number}" for role in ROLES for number in range(1, 4))
REUSED_V2 = frozenset({"fe_2", "infra_3"})
REUSED_V3 = frozenset({"fe_2", "infra_3"})

# Fixed before workloads run.  These are prototype-level acceptance rubrics,
# independent of the value later measured by the executable fixture.
RUBRICS: dict[str, tuple[str, float | int]] = {
    "fe_1": ("gte", 2), "fe_2": ("gte", 2), "fe_3": ("gte", 3),
    "be_1": ("gte", 1), "be_2": ("gte", 1), "be_3": ("gte", 1),
    "infra_1": ("gte", 1), "infra_2": ("lte", 0), "infra_3": ("gte", 3),
    "ml_1": ("gte", 0.90), "ml_2": ("lte", 0.30), "ml_3": ("lte", 0.02),
    "dl_1": ("gte", 0.85), "dl_2": ("gte", 1), "dl_3": ("lte", 0.25),
    "llm_1": ("gte", 10), "llm_2": ("gte", 1), "llm_3": ("gte", 1),
}

ROLE_KO = {
    "fe": "사용자 화면",
    "be": "서비스 상태 전이",
    "infra": "운영 기반",
    "ml": "통계 모델",
    "dl": "표현 학습",
    "llm": "언어 모델 어댑터",
}

V1_TEXT = {
    "fe_1": "상담화면 문의카드에 분류와 답변 근거를 함께 표시한다 고객지원분류",
    "fe_2": "사람이 AI 제안을 검토하고 승인 또는 반려 이유를 남긴다 사람승인",
    "fe_3": "고객 문의 처리 이력을 시간순으로 비교한다 상담타임라인",
    "be_1": "채널 문의를 멱등 수집하고 원문 사건을 보존한다 문의수집",
    "be_2": "분류 초안 승인 발송의 상태 전이를 검증한다 답변상태",
    "be_3": "승인된 고객 답변만 채널 어댑터로 전달한다 승인발송",
    "infra_1": "고객지원 API의 지연 오류 준비 상태를 관측한다 지원관제",
    "infra_2": "고객 데이터와 비밀값을 환경별로 격리한다 개인정보격리",
    "infra_3": "원문과 감사 기록을 보존하여 export import 복구한다 불변복구",
    "ml_1": "문의 의도와 긴급도 분류 정확도를 측정한다 의도분류",
    "ml_2": "답변 후보의 근거 일치와 금지 표현을 평가한다 답변평가",
    "ml_3": "상담원 수정량과 반려 사유 편향을 분석한다 수정분석",
    "dl_1": "한국어 문의 표현 모델의 검색 품질을 평가한다 문의임베딩",
    "dl_2": "긴 문의에서 문맥 손실을 점검한다 장문문의",
    "dl_3": "문의 모델 지연 메모리 배치를 재현 측정한다 문의추론",
    "llm_1": "검색된 고객 근거만 쓰는 프롬프트 계약을 정의한다 근거초안",
    "llm_2": "개인정보와 확정할 수 없는 내용을 생성 초안에서 차단한다 안전초안",
    "llm_3": "승인 반려 고객 사례로 결정적 회귀 평가를 실행한다 초안회귀",
}

V2_TEXT = {
    "fe_1": "장애 경보 로그 배포 변경 가설을 관제타임라인 화면에 표시한다 관제타임라인",
    "fe_2": V1_TEXT["fe_2"],
    "fe_3": "현재 장애와 과거 유사 사고를 차이 중심으로 비교한다 사고비교",
    "be_1": "경보 공급자의 중복 이벤트를 멱등 수집한다 경보수집",
    "be_2": "장애 가설 근거 대응 조치의 상태 전이를 강제한다 가설상태",
    "be_3": "승인된 런북 단계만 실행하고 결과를 기록한다 런북실행",
    "infra_1": "서비스 포화도 오류 예산 의존성 신호를 모은다 서비스관제",
    "infra_2": "대응 권한과 운영 비밀값을 서비스별로 격리한다 대응격리",
    "infra_3": V1_TEXT["infra_3"],
    "ml_1": "경보 묶음의 사건 군집 정확도와 중복률을 측정한다 경보군집",
    "ml_2": "장애 가설 순위와 실제 원인의 일치도를 평가한다 원인가설",
    "ml_3": "서비스 관측 편향과 드문 장애 누락률을 분석한다 장애편향",
    "dl_1": "로그 트레이스 표현 모델의 검색 품질을 평가한다 로그표현",
    "dl_2": "긴 장애 구간과 다중 서비스 문맥 손실을 점검한다 다중문맥",
    "dl_3": "장애 추론 지연 GPU 메모리 입력 한계를 측정한다 장애추론",
    "llm_1": "근거 ID를 동반한 장애 가설만 내는 구조화 계약을 사용한다 장애가설",
    "llm_2": "위험 조치를 제안에 고정하고 사람 승인을 요구한다 조치승인",
    "llm_3": "고정 장애 픽스처로 가설 요약 런북 회귀를 실행한다 장애회귀",
}

V3_TEXT = {
    "fe_1": "원문 관련 문서 충돌 문장 제안을 지식검토 화면에 표시한다 지식대조",
    "fe_2": V1_TEXT["fe_2"],
    "fe_3": "특정 시점 공식 지식과 이후 정정을 비교한다 지식연혁",
    "be_1": "파일 URL 메모 입력을 지식 사건으로 보존한다 문서수집",
    "be_2": V2_TEXT["be_2"],
    "be_3": "문서 원본과 파생 원자의 출처 앵커 해시를 제공한다 출처추적",
    "infra_1": "지식 색인 지연 실패율 저장소 준비 상태를 관측한다 색인관제",
    "infra_2": "부서별 문서 접근과 모델 전달 범위를 격리한다 문서격리",
    "infra_3": V1_TEXT["infra_3"],
    "ml_1": "중복 지식 후보의 정밀도와 재현율을 측정한다 중복지식",
    "ml_2": "충돌 후보가 실제 정책 모순인지 평가한다 정책충돌",
    "ml_3": "부서 문서 유형별 누락 과탐 편향을 분석한다 지식편향",
    "dl_1": "한국어 문서 임베딩 검색 품질을 고정 집합에서 비교한다 문서임베딩",
    "dl_2": "긴 문서 분할이 인용 범위와 의미를 보존하는지 점검한다 문서분할",
    "dl_3": "지식 색인 추론 자원과 입력 크기를 측정한다 색인자원",
    "llm_1": "출처 문장과 지식 변경 제안을 분리한 구조화 출력을 쓴다 지식제안",
    "llm_2": "불확실한 지식 충돌을 질문으로 남긴다 충돌질문",
    "llm_3": "고정 문서 픽스처로 요약 충돌 변경 회귀를 실행한다 지식회귀",
}


def file_sha256(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def capture_record(record_id: str, project_id: str, content: str, source_ref: str, occurred_at: str) -> Json:
    return {
        "id": record_id,
        "kind": "capture",
        "data": {
            "project_id": project_id,
            "content": content,
            "content_digest": "sha256:" + hashlib.sha256(content.encode()).hexdigest(),
            "media_type": "text/plain",
            "source_kind": "file",
            "source_ref": source_ref,
            "occurred_at": occurred_at,
        },
    }


def artifact_record(record_id: str, path: Path, digest: str, note: str) -> Json:
    return {
        "id": record_id,
        "kind": "artifact",
        "data": {
            "uri": path.resolve().as_uri(),
            "digest": digest,
            "digest_alg": "sha256",
            "size_bytes": path.stat().st_size,
            "media_type": "application/json",
            "included_in_export": False,
            "note": note,
        },
    }


def observation_record(record_id: str, project_id: str, target: str, result: Json, event_at: str,
                       artifact_id: str, capture_id: str, actor: str = "tool:lifecycle") -> Json:
    return {
        "id": record_id,
        "kind": "observation",
        "data": {
            "project_id": project_id,
            "target_revision_id": target,
            "metric": result["metric"],
            "value": result["value"],
            "unit": result["unit"],
            "status": "observed",
            "occurred_at": event_at,
            "method": result["method"],
            "environment": result["environment"],
            "artifact_ids": [artifact_id],
            "capture_id": capture_id,
            "actor": actor,
            "note": f"workload_status={result['status']}; {result['note']}",
        },
    }


def goal_record(record_id: str, project_id: str, root: str, path: list[str], target: str,
                criterion: str, metric: str, comparator: str, threshold: float | int, unit: str) -> Json:
    return {
        "id": record_id,
        "kind": "goal",
        "data": {
            "project_id": project_id,
            "scope": {"root_revision_id": root, "slot_path": path, "target_revision_id": target},
            "statement": f"제한된 기술 프로토타입에서 {metric} 측정값을 재현한다",
            "criteria": [{
                "criterion_id": criterion,
                "kind": "quantitative",
                "statement": f"실행 관측 {metric} 확인",
                "metric": metric,
                "comparator": comparator,
                "threshold": threshold,
                "unit": unit,
                "required": True,
            }],
            "origin": "official",
            "actor": "human:lifecycle",
        },
    }


def baseline_record(record_id: str, goal_id: str, criterion: str, event_at: str) -> Json:
    return {
        "id": record_id,
        "kind": "baseline",
        "data": {
            "goal_id": goal_id,
            "criterion_ids": [criterion],
            "constraints": ["결정적 로컬 픽스처", "외부 네트워크 사용 안 함"],
            "effective_from": event_at,
            "supersedes": None,
            "actor": "human:lifecycle",
            "reason": "실행 전 채택 기준 고정",
        },
    }


def assessment_record(record_id: str, root: str, path: list[str], target: str, baseline: str,
                      criterion: str, observation: str, result: Json, cutoff_seq: int,
                      cutoff_at: str, status: str) -> Json:
    return {
        "id": record_id,
        "kind": "assessment",
        "data": {
            "root_revision_id": root,
            "slot_path": path,
            "target_revision_id": target,
            "baseline_id": baseline,
            "evidence_cutoff_seq": cutoff_seq,
            "evidence_cutoff_at": cutoff_at,
            "evaluator": "human:lifecycle",
            "rubric_version": "lifecycle-r1",
            "origin": "official",
            "status": status,
            "criteria_results": [{
                "criterion_id": criterion,
                "status": status,
                "observed_value": result["value"],
                "note": "실행 파일과 해시를 확인함",
            }],
            "evidence_observation_ids": [observation],
            "note": "실제 로컬 워크로드 결과에 대한 평가",
        },
    }


def rubric_status(value: float | int, comparator: str, threshold: float | int) -> str:
    tests = {
        "gte": value >= threshold, "gt": value > threshold,
        "lte": value <= threshold, "lt": value < threshold,
        "eq": value == threshold, "ne": value != threshold,
    }
    return "met" if tests[comparator] else "unmet"


def root_change(project_id: str, revision_id: str, before: str, after: str, reason: str) -> Json:
    return {
        "project_id": project_id,
        "stage": "working",
        "after_revision_id": revision_id,
        "reason": reason,
        "meaningful": True,
        "decision": {"before": before, "after": after, "rationale": reason},
    }


def publish(project_id: str, root: str, label: str, event_at: str) -> Json:
    return {"project_id": project_id, "root_revision_id": root, "label": label,
            "published_at": event_at, "notes": "lifecycle checkpoint publication"}


def iso_shift(value: str, milliseconds: int) -> str:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00")) + timedelta(milliseconds=milliseconds)
    return parsed.isoformat(timespec="milliseconds")


def iso_utc(value: str) -> str:
    parsed = datetime.fromisoformat(value.replace("Z", "+00:00")).astimezone(timezone.utc)
    return parsed.isoformat(timespec="milliseconds").replace("+00:00", "Z")


def event_times() -> list[str]:
    offsets = (timezone(timedelta(hours=9)), timezone.utc, timezone(timedelta(hours=-4)))
    out = []
    base = datetime(2026, 8, 1, 9, 7, 11, tzinfo=offsets[0])
    for index in range(30):
        local = (base.astimezone(timezone.utc) + timedelta(days=index, hours=(index * 7) % 19,
                                                           minutes=(index * 13) % 47)).astimezone(offsets[index % 3])
        out.append(local.isoformat(timespec="milliseconds"))
    return out


def tree_leaves(tree: Json | None) -> dict[str, str]:
    found: dict[str, str] = {}
    if not tree:
        return found
    stack = [tree]
    while stack:
        node = stack.pop()
        children = node.get("children", [])
        if not children and node.get("slot_path"):
            found["/".join(node["slot_path"])] = node["revision_id"]
        stack.extend(children)
    return found


def evidence_ids(detail: Json, key: str) -> set[str]:
    rows = detail.get("evidence", {}).get(key, [])
    return {row.get("id") for row in rows if isinstance(row, dict) and isinstance(row.get("id"), str)}


@dataclass
class Publication:
    step: int
    root: str
    effective_at: str


class Lifecycle:
    def __init__(self, api: Api, output_dir: Path, prefix: str) -> None:
        self.api = api
        self.output_dir = output_dir
        self.prefix = prefix
        self.project = self.ident("project")
        self.times = event_times()
        self.steps: list[Json] = []
        self.checkpoints: list[Json] = []
        self.publications: list[Publication] = []
        self.record_seq: dict[str, int] = {}
        self.current_root = ""
        self.current_leaves: dict[str, str] = {}
        self.workloads: dict[str, Json] = {}
        self.work_evidence: list[Json] = []
        self.search_cases: list[Json] = []
        self.similarity_links: list[Json] = []
        self.lineage_cases: list[Json] = []
        self.source_cases: list[Json] = []
        self.v1_entities: dict[str, str] = {}
        self.v1_revisions: dict[str, str] = {}
        self.v2_entities: dict[str, str] = {}
        self.v2_revisions: dict[str, str] = {}
        self.latest_entities: dict[str, str] = {}
        self.latest_revisions: dict[str, str] = {}
        self.v1_root = ""
        self.v2_root = ""
        self.v3_root = ""
        self.last_recorded_at: str | None = None

    def ident(self, suffix: str) -> str:
        return f"{self.prefix}_{suffix}"

    def state_seq(self) -> int:
        return int(self.api.get("/api/state")["seq"])

    def ensure_clock_tick(self) -> None:
        if self.last_recorded_at is not None:
            time.sleep(0.004)

    def apply_step(self, number: int, name: str, body: Json, event_at: str,
                   expected_root: str | None = None) -> Json:
        require(number == len(self.steps) + 1, f"step order mismatch at {number}")
        self.ensure_clock_tick()
        response = self.api.post("/api/packages/apply", body)
        receipt = response["receipt"]
        recorded_at = receipt["applied_at"]
        if self.last_recorded_at is not None:
            require(recorded_at > self.last_recorded_at,
                    f"step {number}: server recorded_at did not strictly advance")
        self.last_recorded_at = recorded_at
        for record_id in receipt.get("records_written", []):
            self.record_seq[record_id] = receipt["seq"]
        if expected_root is not None:
            self.current_root = expected_root
        snap = self.api.get("/api/snapshot", project_id=self.project, stage="working",
                            known_seq=receipt["seq"], max_nodes=5000, depth=32)
        require(snap.get("root_revision_id") == self.current_root,
                f"step {number}: working root mismatch")
        leaves = tree_leaves(snap.get("tree"))
        require(leaves == self.current_leaves, f"step {number}: snapshot leaves mismatch")
        checkpoint = {
            "step": number, "name": name, "event_at": event_at,
            "recorded_at": recorded_at, "seq": receipt["seq"],
            "working_root": self.current_root, "leaves": leaves,
            "record_ids": receipt.get("records_written", []),
        }
        self.steps.append({**checkpoint, "idempotency_key": body["idempotency_key"]})
        self.checkpoints.append(checkpoint)
        return response

    def build_version(self, version: str, texts: dict[str, str], event_at: str,
                      plan_path: Path, previous_root: str | None = None,
                      reuse: frozenset[str] = frozenset()) -> tuple[list[Json], str, dict[str, str], dict[str, str]]:
        records: list[Json] = []
        plan_text = plan_path.read_text("utf-8")
        plan_capture_id = self.ident(f"cap_plan_{version}")
        plan_source = {**inferred_source(), "capture_id": plan_capture_id}
        records.append(capture_record(plan_capture_id, self.project, plan_text, str(plan_path), event_at))
        entities: dict[str, str] = {}
        revisions: dict[str, str] = {}
        schema_slots: list[Json] = []
        for role in ROLES:
            schema_entity = self.ident(f"ent_{version}_{role}_schema")
            schema_revision = self.ident(f"rev_{version}_{role}_schema")
            core_entity = self.ident(f"ent_{version}_{role}_core")
            core_revision = self.ident(f"rev_{version}_{role}_core")
            prior_schema = self.ident(f"ent_v1_{role}_schema") if version == "v2" else self.ident(f"ent_v2_{role}_schema")
            prior_core = self.ident(f"ent_v1_{role}_core") if version == "v2" else self.ident(f"ent_v2_{role}_core")
            records.append(entity_record(schema_entity, self.project, "schema", f"{ROLE_KO[role]} {version}",
                                         tags=[role, version], derived_from=[prior_schema] if previous_root else None,
                                         lineage_kind="semantic_edit" if previous_root else "none"))
            records.append(entity_record(core_entity, self.project, "core", f"{ROLE_KO[role]} 실행 묶음 {version}",
                                         tags=[role], derived_from=[prior_core] if previous_root else None,
                                         lineage_kind="semantic_edit" if previous_root else "none"))
            atom_slots: list[Json] = []
            for n in range(1, 4):
                work_id = f"{role}_{n}"
                if work_id in reuse:
                    ent = self.latest_entities[work_id]
                    rev = self.latest_revisions[work_id]
                else:
                    ent = self.ident(f"ent_{version}_{work_id}")
                    rev = self.ident(f"rev_{version}_{work_id}")
                    derived = [self.latest_entities[work_id]] if previous_root else None
                    records.append(entity_record(ent, self.project, "idea", f"{ROLE_KO[role]} {n} {version}",
                                                 tags=[role, work_id, version], derived_from=derived,
                                                 lineage_kind="semantic_edit" if derived else "none"))
                    records.append(revision_record(rev, ent, texts[work_id],
                                                   change_kind="semantic" if derived else "initial",
                                                   source=plan_source, tags=[role, work_id, version]))
                entities[work_id] = ent
                revisions[work_id] = rev
                atom_slots.append(slot(work_id, rev, role))
            records.append(revision_record(core_revision, core_entity, f"{ROLE_KO[role]} 원자 실행 계획 {version}",
                                           slots=atom_slots,
                                           change_kind="semantic" if previous_root else "initial",
                                           source=plan_source, tags=[role, version]))
            records.append(revision_record(schema_revision, schema_entity, f"{ROLE_KO[role]} 스키마 {version}",
                                           slots=[slot("workflow", core_revision, role)],
                                           change_kind="semantic" if previous_root else "initial",
                                           source=plan_source, tags=[role, version]))
            schema_slots.append(slot(role, schema_revision, role))
        root = self.ident(f"rev_root_{version}")
        records.append(revision_record(root, self.project,
                                       f"{version} 전체 계획 " + ("고객지원분류" if version == "v1" else "운영장애분석" if version == "v2" else "사내지식검토 생애주기전환v3"),
                                       slots=schema_slots,
                                       change_kind="composition" if previous_root else "initial",
                                       previous_revision_id=previous_root,
                                       source=plan_source, tags=[version, "full-plan"]))
        return records, root, entities, revisions

    def initial_v1(self, plan_dir: Path) -> None:
        event_at = self.times[0]
        records, root, ents, revs = self.build_version("v1", V1_TEXT, event_at, plan_dir / "plan_v1.md")
        records.insert(0, project_record(self.project, "AI 지원 제품 생애주기 30단계"))
        self.current_leaves = {f"{role}/workflow/{role}_{n}": revs[f"{role}_{n}"]
                               for role in ROLES for n in range(1, 4)}
        body = package(self.ident("step01"), "고객지원 분류 제품 최초 계획", expected_head(self.project, None), records,
                       root_change=root_change(self.project, root, "계획 없음", "고객지원 분류", "여섯 역할의 최초 계획 구성"),
                       publish=publish(self.project, root, "고객지원 v1", event_at))
        body["expected_heads"].extend(expected_head(self.project, None, "official"))
        self.v1_root = root
        self.latest_entities, self.latest_revisions = ents.copy(), revs.copy()
        self.v1_entities, self.v1_revisions = ents, revs
        self.apply_step(1, "initial customer-support plan", body, event_at, root)
        self.source_cases.append({"step": 1, "revision_id": root,
                                  "capture_id": self.ident("cap_plan_v1")})
        self.publications.append(Publication(1, root, event_at))

    def pivot_v2(self, plan_dir: Path) -> None:
        event_at = self.times[1]
        records, root, ents, revs = self.build_version("v2", V2_TEXT, event_at, plan_dir / "plan_v2.md",
                                                       self.current_root, REUSED_V2)
        self.current_leaves = {f"{role}/workflow/{role}_{n}": revs[f"{role}_{n}"]
                               for role in ROLES for n in range(1, 4)}
        body = package(self.ident("step02"), "고객지원에서 운영 장애 분석으로 전환",
                       expected_head(self.project, self.current_root), records,
                       root_change=root_change(self.project, root, "고객지원 분류", "운영 장애 분석",
                                               "사용자와 입력 및 산출물이 바뀐 중대한 제품 전환"),
                       publish=publish(self.project, root, "운영 장애 v2", event_at))
        body["expected_heads"].extend(expected_head(self.project, self.v1_root, "official"))
        self.v2_root = root
        self.v2_entities, self.v2_revisions = ents, revs
        self.latest_entities, self.latest_revisions = ents.copy(), revs.copy()
        self.apply_step(2, "major pivot to incident analysis", body, event_at, root)
        self.source_cases.append({"step": 2, "revision_id": root,
                                  "capture_id": self.ident("cap_plan_v2")})
        self.publications.append(Publication(2, root, event_at))
        for work_id in REUSED_V2:
            require(revs[work_id] == self.v1_revisions[work_id], f"{work_id}: exact reuse lost")
        for work_id in set(WORK_IDS) - set(REUSED_V2):
            require(ents[work_id] != self.v1_entities[work_id], f"{work_id}: derived identity was reused")

    def record_workload(self, offset: int, work_id: str, result: Json) -> None:
        step = offset + 3
        event_at = self.times[step - 1]
        path = [work_id.split("_")[0], "workflow", work_id]
        target = self.v2_revisions[work_id]
        artifact_path = Path(result["artifact_path"])
        require(artifact_path.is_file(), f"{work_id}: workload artifact is missing")
        require(file_sha256(artifact_path) == result["artifact_sha256"],
                f"{work_id}: workload artifact digest mismatch")
        capture_id = self.ident(f"cap_{work_id}")
        artifact_id = self.ident(f"art_{work_id}")
        observation_id = self.ident(f"obs_{work_id}")
        goal_id = self.ident(f"goal_{work_id}")
        baseline_id = self.ident(f"base_{work_id}")
        assessment_id = self.ident(f"asm_{work_id}")
        criterion = f"criterion_{work_id}"
        comparator, threshold = RUBRICS[work_id]
        status = rubric_status(result["value"], comparator, threshold)
        next_seq = self.state_seq() + 1
        raw = artifact_path.read_text("utf-8")
        records = [
            capture_record(capture_id, self.project, raw, str(artifact_path), event_at),
            artifact_record(artifact_id, artifact_path, result["artifact_sha256"], f"{work_id} executable output"),
            observation_record(observation_id, self.project, target, result, event_at, artifact_id, capture_id),
            goal_record(goal_id, self.project, self.v2_root, path, target, criterion,
                        result["metric"], comparator, threshold, result["unit"]),
            baseline_record(baseline_id, goal_id, criterion, event_at),
            assessment_record(assessment_id, self.v2_root, path, target, baseline_id, criterion,
                              observation_id, result, next_seq, event_at, status),
        ]
        body = package(self.ident(f"step{step:02d}"), f"{work_id} 실제 워크로드 근거 기록",
                       expected_head(self.project, self.current_root), records)
        self.apply_step(step, f"workload {work_id}", body, event_at)
        self.work_evidence.append({
            "work_id": work_id, "step": step, "path": path, "target_revision_id": target,
            "capture_id": capture_id, "artifact_id": artifact_id, "observation_id": observation_id,
            "goal_id": goal_id, "baseline_id": baseline_id, "assessment_id": assessment_id,
            "event_at": event_at, "rubric": {"comparator": comparator, "threshold": threshold,
                                               "status": status},
            "result": {**result, "artifact_path": os.path.relpath(artifact_path, self.output_dir)},
        })

    def partial_change(self, role_index: int, role: str) -> None:
        step = 21 + role_index
        event_at = self.times[step - 1]
        work_id = f"{role}_1"
        path = [role, "workflow", work_id]
        old_leaves = self.current_leaves.copy()
        old_entity = self.latest_entities[work_id]
        old_revision = self.latest_revisions[work_id]
        new_entity = self.ident(f"ent_partial_{role}")
        new_revision = self.ident(f"rev_partial_{role}")
        capture_id = self.ident(f"cap_partial_{role}")
        new_body = V2_TEXT[work_id] + f" {role}세부변경{step} 검토 경계를 강화한다"
        request_body = {
            "protocol_version": 1,
            "idempotency_key": self.ident(f"step{step:02d}"),
            "actor": "human:lifecycle",
            "reason": f"{role} 세부 설계 변경",
            "project_id": self.project,
            "stage": "working",
            "expected_root_revision_id": self.current_root,
            "slot_path": path,
            "replacement": {
                "mode": "new_revision",
                "new_revision": {
                    "id": new_revision,
                    "data": revision_record(new_revision, new_entity, new_body,
                                            change_kind="semantic",
                                            source={**inferred_source(), "capture_id": capture_id},
                                            tags=[role, "partial"])["data"],
                },
                "new_records": [
                    entity_record(new_entity, self.project, "idea", f"{ROLE_KO[role]} 세부 변경",
                                  tags=[role, "partial"], derived_from=[old_entity],
                                  lineage_kind="semantic_edit"),
                    capture_record(capture_id, self.project,
                                   f"{role} 세부 변경 결정: {new_body}", "lifecycle-partial", event_at),
                ],
                "target_revision_id": None,
            },
            "root_change_reason": f"{role} 한 위치의 세부 의미 변경",
            "decision": {"before": old_revision, "after": new_revision,
                         "rationale": "다른 역할과 과거 스냅샷을 고정한 채 한 위치만 바꾼다"},
        }
        self.ensure_clock_tick()
        response = self.api.post("/api/occurrences/replace", request_body)
        receipt = response["receipt"]
        recorded_at = receipt["applied_at"]
        require(self.last_recorded_at is None or recorded_at > self.last_recorded_at,
                f"step {step}: server recorded_at did not strictly advance")
        self.last_recorded_at = recorded_at
        new_root = response["new_root_revision_id"]
        for record_id in receipt.get("records_written", []):
            self.record_seq[record_id] = receipt["seq"]
        self.current_root = new_root
        self.latest_entities[work_id] = new_entity
        self.latest_revisions[work_id] = new_revision
        self.current_leaves[path[0] + "/workflow/" + work_id] = new_revision
        snap = self.api.get("/api/snapshot", project_id=self.project, stage="working",
                            known_seq=receipt["seq"], max_nodes=5000, depth=32)
        leaves = tree_leaves(snap["tree"])
        require(leaves == self.current_leaves, f"step {step}: partial edit leaf map mismatch")
        for leaf_path, revision in old_leaves.items():
            if leaf_path != "/".join(path):
                require(leaves[leaf_path] == revision,
                        f"step {step}: unrelated occurrence {leaf_path} changed")
        goals = self.api.get("/api/goals", project_id=self.project, stage="working",
                             known_seq=receipt["seq"], max_nodes=5000)
        old_assessment = self.ident(f"asm_{work_id}")
        require(any(row.get("assessment_id") == old_assessment for row in goals.get("stale_assessments", [])),
                f"step {step}: prior scoped assessment was not marked stale")
        lineage = self.api.get(f"/api/records/{new_revision}", include="lineage",
                               known_seq=receipt["seq"])
        derived_ids = {row.get("id") for row in lineage.get("lineage", {}).get("derived_from", [])}
        require(old_entity in derived_ids, f"step {step}: semantic lineage to {old_entity} missing")
        self.lineage_cases.append({"step": step, "revision_id": new_revision,
                                   "entity_id": new_entity, "derived_from": old_entity})
        self.source_cases.append({"step": step, "revision_id": new_revision,
                                  "capture_id": capture_id})
        checkpoint = {"step": step, "name": f"partial semantic change {role}", "event_at": event_at,
                      "recorded_at": recorded_at, "seq": receipt["seq"], "working_root": new_root,
                      "leaves": leaves, "record_ids": receipt.get("records_written", []),
                      "changed_path": path, "old_revision": old_revision, "new_revision": new_revision}
        self.steps.append({**checkpoint, "idempotency_key": request_body["idempotency_key"]})
        self.checkpoints.append(checkpoint)

    def similarity_step(self) -> None:
        step = 27
        event_at = self.times[step - 1]
        records: list[Json] = [capture_record(self.ident("cap_similarity"), self.project,
                                               "역할별 한국어 어휘 검색과 유사 연결 점검", "lifecycle-search", event_at)]
        pairs = (("fe_1", "fe_3", 0.61), ("be_1", "be_3", 0.57),
                 ("ml_1", "dl_1", 0.44), ("infra_1", "llm_1", -0.20))
        for index, (left, right, score) in enumerate(pairs):
            link_id = self.ident(f"link_sim_{index}")
            records.append({
                "id": link_id, "kind": "link", "data": {
                    "link_type": "similar", "from_id": self.latest_revisions[left],
                    "to_id": self.latest_revisions[right], "note": "수동 결정적 픽스처 유사도",
                    "actor": "human:lifecycle", "impact": None,
                    "similarity": {"score": score, "method": "lifecycle curated relation fixture"},
                },
            })
            self.similarity_links.append({"link_id": link_id,
                                          "from_revision_id": self.latest_revisions[left],
                                          "to_revision_id": self.latest_revisions[right],
                                          "score": score})
        body = package(self.ident("step27"), "유사 관계와 검색 검증 자료 기록",
                       expected_head(self.project, self.current_root), records)
        self.apply_step(step, "similarity and retrieval", body, event_at)
        queries = {
            "fe": "장애 경보 로그 타임라인", "be": "중복 경보 이벤트 수집",
            "infra": "서비스 오류 예산 신호", "ml": "사건 군집 정확도 중복률",
            "dl": "로그 트레이스 검색 품질", "llm": "근거 장애 가설 구조화",
        }
        recalls = []
        for role, query_text in queries.items():
            expected = self.latest_revisions[f"{role}_1"]
            request_body = {"project_id": self.project, "query": query_text, "stage": "working",
                            "known_seq": self.checkpoints[-1]["seq"], "known_at": None,
                            "effective_at": event_at, "roles": [role], "tags": [],
                            "lanes": ["official"], "limit": 5, "vector": None}
            actual = self.api.post("/api/search", request_body)
            top = [row["revision_id"] for row in actual.get("results", [])]
            recall = 1.0 if expected in top else 0.0
            require(recall == 1.0, f"step 27: {role} expected result missing from top 5")
            wrong = dict(request_body)
            wrong["roles"] = [next(r for r in ROLES if r != role)]
            negative = self.api.post("/api/search", wrong)
            require(expected not in [row["revision_id"] for row in negative.get("results", [])],
                    f"step 27: role mismatch leaked {expected}")
            historical = dict(request_body)
            historical["known_seq"] = self.checkpoints[19]["seq"]
            old = self.api.post("/api/search", historical)
            old_ids = [row["revision_id"] for row in old.get("results", [])]
            require(self.v2_revisions[f"{role}_1"] in old_ids,
                    f"step 27: historical {role} revision not retrievable")
            require(expected == self.v2_revisions[f"{role}_1"] or expected not in old_ids,
                    f"step 27: future {role} revision leaked into historical search")
            case = {"role": role, "query": query_text, "expected_top_ids": [expected],
                    "actual_top_ids": top, "recall_at_5": recall,
                    "negative_role": wrong["roles"][0],
                    "excluded_different_role_ids": [self.latest_revisions[f"{wrong['roles'][0]}_1"]],
                    "historical_expected": self.v2_revisions[f"{role}_1"],
                    "search_known_seq": self.checkpoints[-1]["seq"],
                    "historical_known_seq": self.checkpoints[19]["seq"]}
            self.search_cases.append(case)
            recalls.append(recall)
        for link in self.similarity_links:
            detail = self.api.get(f"/api/records/{link['from_revision_id']}", include="links",
                                  known_seq=self.checkpoints[-1]["seq"])
            matches = [row for row in detail.get("links", {}).get("outgoing", [])
                       if row.get("link_id") == link["link_id"]]
            require(len(matches) == 1, f"step 27: similarity link {link['link_id']} missing")
            require(matches[0].get("to_id") == link["to_revision_id"] and
                    matches[0].get("data", {}).get("similarity", {}).get("score") == link["score"],
                    f"step 27: similarity link {link['link_id']} payload changed")
        self.steps[-1]["retrieval_recall_at_5"] = sum(recalls) / len(recalls)
        self.steps[-1]["semantic_embedding_quality_proven"] = False

    def late_observation_step(self) -> None:
        step = 28
        event_at = "2026-08-25T04:15:00-04:00"
        work_id = "be_2"
        evidence = next(row for row in self.work_evidence if row["work_id"] == work_id)
        target = evidence["target_revision_id"]
        artifact_path = self.output_dir / "workloads" / "late_be_2.json"
        late_payload = {"kind": "late-arriving", "target": target, "occurred_at": event_at,
                        "measured": 1, "note": "과거 발생 후 늦게 수집"}
        artifact_path.write_text(json.dumps(late_payload, ensure_ascii=False, sort_keys=True), "utf-8")
        digest = file_sha256(artifact_path)
        original_result = evidence["result"]
        result = {"status": "passed", "metric": original_result["metric"], "value": 1,
                  "unit": original_result["unit"],
                  "method": "deterministic late-arrival fixture", "artifact_path": str(artifact_path),
                  "artifact_sha256": digest,
                  "environment": {"code_ref": file_sha256(Path(__file__)), "data_version": "late-v1",
                                  "model_version": "none", "config_digest": canonical_digest(late_payload),
                                  "runtime": f"python-{sys.version_info.major}.{sys.version_info.minor}"},
                  "note": "실제 발생 뒤 늦게 기록된 관측"}
        capture_id, artifact_id = self.ident("cap_late"), self.ident("art_late")
        observation_id, assessment_id = self.ident("obs_late"), self.ident("asm_late")
        next_seq = self.state_seq() + 1
        records = [
            capture_record(capture_id, self.project, artifact_path.read_text("utf-8"), str(artifact_path), event_at),
            artifact_record(artifact_id, artifact_path, digest, "late observation output"),
            observation_record(observation_id, self.project, target, result, event_at, artifact_id, capture_id),
            assessment_record(assessment_id, self.v2_root, evidence["path"], target,
                              evidence["baseline_id"], f"criterion_{work_id}", observation_id,
                              result, next_seq, iso_shift(event_at, 1), "met"),
        ]
        body = package(self.ident("step28"), "늦게 도착한 과거 관측 기록",
                       expected_head(self.project, self.current_root), records)
        self.apply_step(step, "late-arriving past observation", body, event_at)
        self.work_evidence.append({"work_id": "late_be_2", "step": step, "path": evidence["path"],
                                   "target_revision_id": target, "capture_id": capture_id,
                                   "artifact_id": artifact_id, "observation_id": observation_id,
                                   "goal_id": evidence["goal_id"], "baseline_id": evidence["baseline_id"],
                                   "assessment_id": assessment_id, "event_at": event_at,
                                   "rubric": {"comparator": RUBRICS[work_id][0],
                                              "threshold": RUBRICS[work_id][1], "status": "met"},
                                   "result": {**result,
                                              "artifact_path": os.path.relpath(artifact_path, self.output_dir)}})

    def pivot_v3(self, plan_dir: Path) -> None:
        step = 29
        event_at = self.times[step - 1]
        records, root, ents, revs = self.build_version("v3", V3_TEXT, event_at, plan_dir / "plan_v3.md",
                                                       self.current_root, REUSED_V3)
        path = ["be", "workflow", "be_3"]
        target = revs["be_3"]
        goal_id, baseline_id = self.ident("goal_v3_source"), self.ident("base_v3_source")
        criterion = "criterion_source_trace"
        records.extend([
            goal_record(goal_id, self.project, root, path, target, criterion,
                        "source_trace_coverage", "gte", 1, "fraction"),
            baseline_record(baseline_id, goal_id, criterion, event_at),
        ])
        self.current_leaves = {f"{role}/workflow/{role}_{n}": revs[f"{role}_{n}"]
                               for role in ROLES for n in range(1, 4)}
        body = package(self.ident("step29"), "운영 장애에서 사내 지식 검토로 전체 계획 전환",
                       expected_head(self.project, self.current_root), records,
                       root_change=root_change(self.project, root, "운영 장애 분석", "사내 지식 검토",
                                               "입력 자료와 사용자 및 품질 기준이 근본적으로 변경됨"),
                       publish=publish(self.project, root, "사내 지식 검토 v3", event_at))
        body["expected_heads"].extend(expected_head(self.project, self.v2_root, "official"))
        self.v3_root = root
        self.latest_entities, self.latest_revisions = ents.copy(), revs.copy()
        self.apply_step(step, "full pivot to internal knowledge review", body, event_at, root)
        self.source_cases.append({"step": step, "revision_id": root,
                                  "capture_id": self.ident("cap_plan_v3")})
        self.publications.append(Publication(step, root, event_at))
        for work_id in REUSED_V3:
            require(revs[work_id] in {self.v1_revisions[work_id], self.v2_revisions[work_id]},
                    f"{work_id}: v3 explicit reuse lost")

    def final_marker(self) -> None:
        step = 30
        event_at = self.times[step - 1]
        content = "30개 체크포인트 전체 재생 검증을 시작한다"
        records = [capture_record(self.ident("cap_final_replay"), self.project, content,
                                  "lifecycle-final", event_at)]
        body = package(self.ident("step30"), "최종 체크포인트 재생 표식",
                       expected_head(self.project, self.current_root), records)
        self.apply_step(step, "final all-checkpoint replay", body, event_at)

    def execute(self, plan_dir: Path) -> None:
        workload_dir = self.output_dir / "workloads"
        self.workloads = run_workloads(workload_dir)
        require(set(self.workloads) == set(WORK_IDS), "workload mapping must contain exactly 18 role slots")
        for work_id in WORK_IDS:
            result = self.workloads[work_id]
            role = work_id.rsplit("_", 1)[0]
            require(result.get("role") == role, f"{work_id}: workload role mismatch")
            require(result.get("status") in {"passed", "observed_failure"},
                    f"{work_id}: workload did not produce an admissible measured result")
            require(isinstance(result.get("value"), (int, float)) and not isinstance(result.get("value"), bool),
                    f"{work_id}: quantitative workload value required")
        self.initial_v1(plan_dir)
        self.pivot_v2(plan_dir)
        for offset, work_id in enumerate(WORK_IDS):
            self.record_workload(offset, work_id, self.workloads[work_id])
        for role_index, role in enumerate(ROLES):
            self.partial_change(role_index, role)
        self.similarity_step()
        self.late_observation_step()
        self.pivot_v3(plan_dir)
        self.final_marker()
        require(len(self.checkpoints) == 30, "exactly 30 lifecycle checkpoints required")

    def report(self) -> Json:
        return {
            "format": "idea_db.lifecycle-report", "format_version": 1,
            "status": "pending-verification", "prefix": self.prefix, "project_id": self.project,
            "generated_at": datetime.now(timezone.utc).isoformat(timespec="milliseconds"),
            "steps": self.steps, "checkpoints": self.checkpoints,
            "publications": [p.__dict__ for p in self.publications],
            "work_evidence": self.work_evidence, "search_cases": self.search_cases,
            "similarity_links": self.similarity_links, "lineage_cases": self.lineage_cases,
            "source_cases": self.source_cases,
            "record_seq": self.record_seq,
            "limitations": [
                "합성 소규모 실행으로 팀 규모 성능을 증명하지 않는다.",
                "역할별 Goal은 측정된 기술 프로토타입 지표 하나만 평가하며 제품 기능 완료를 뜻하지 않는다.",
                "한국어 어휘 Recall@5는 기능 점검이며 의미 임베딩 품질을 증명하지 않는다.",
                "LLM 역할은 결정적 어댑터 픽스처이며 실제 LLM 추론을 실행하지 않는다.",
            ],
        }


class Verifier:
    def __init__(self, api: Api, report: Json) -> None:
        self.api = api
        self.report = report
        self.project = report["project_id"]
        self.checkpoints = report["checkpoints"]
        self.record_seq = {key: int(value) for key, value in report.get("record_seq", {}).items()}
        self.failures: list[str] = []

    def check(self, pattern: str, condition: Any, message: str) -> None:
        if not condition:
            self.failures.append(f"[{pattern}] {message}")

    def verify_checkpoints(self) -> None:
        final_seq = self.checkpoints[-1]["seq"]
        publications = sorted(self.report["publications"], key=lambda row: iso_utc(row["effective_at"]))
        for cp in self.checkpoints:
            for selector in ({"known_seq": cp["seq"]}, {"known_at": cp["recorded_at"]}):
                try:
                    snap = self.api.get("/api/snapshot", project_id=self.project, stage="working",
                                        max_nodes=5000, depth=32, **selector)
                    self.check("historical_head", snap.get("root_revision_id") == cp["working_root"],
                               f"step {cp['step']} {selector}: root {snap.get('root_revision_id')}")
                    self.check("historical_structure", tree_leaves(snap.get("tree")) == cp["leaves"],
                               f"step {cp['step']} {selector}: leaf structure changed")
                    for node in snap.get("nodes", []):
                        self.check("future_snapshot", int(node.get("seq", final_seq + 1)) <= cp["seq"],
                                   f"step {cp['step']}: future node {node.get('revision_id')} leaked")
                except Exception as exc:  # collect independent temporal failures
                    self.failures.append(f"[checkpoint_request] step {cp['step']} {selector}: {exc}")

            eligible = [p for p in publications
                        if p["step"] <= cp["step"] and iso_utc(p["effective_at"]) <= iso_utc(cp["event_at"])]
            expected_official = eligible[-1]["root"] if eligible else None
            try:
                official = self.api.get("/api/snapshot", project_id=self.project, stage="official",
                                        known_seq=cp["seq"], effective_at=cp["event_at"],
                                        max_nodes=5000, depth=32)
                self.check("publication_effective", official.get("root_revision_id") == expected_official,
                           f"step {cp['step']}: effective official head mismatch")
            except Exception as exc:
                self.failures.append(f"[publication_effective] step {cp['step']}: {exc}")

            query = {"project_id": self.project, "query": "생애주기전환v3", "stage": "working",
                     "known_seq": cp["seq"], "known_at": None, "effective_at": cp["event_at"],
                     "roles": [], "tags": [], "lanes": ["official"], "limit": 20, "vector": None}
            try:
                found = self.api.post("/api/search", query).get("results", [])
                if cp["step"] < 29:
                    self.check("future_search", not found,
                               f"step {cp['step']}: v3 future text leaked into search")
                for row in found:
                    seq = self.record_seq.get(row["revision_id"])
                    self.check("future_search", seq is None or seq <= cp["seq"],
                               f"step {cp['step']}: future search record {row['revision_id']} leaked")
            except Exception as exc:
                self.failures.append(f"[future_search] step {cp['step']}: {exc}")

    def verify_publication_boundaries(self) -> None:
        final_seq = self.checkpoints[-1]["seq"]
        pubs = sorted(self.report["publications"], key=lambda row: iso_utc(row["effective_at"]))
        for index, pub in enumerate(pubs):
            prior = pubs[index - 1]["root"] if index else None
            probes = ((iso_shift(pub["effective_at"], -1), prior, "before"),
                      (pub["effective_at"], pub["root"], "at"),
                      (iso_shift(pub["effective_at"], 1), pub["root"], "after"),
                      (iso_utc(pub["effective_at"]), pub["root"], "timezone-equivalent"))
            for effective, expected, label in probes:
                try:
                    snap = self.api.get("/api/snapshot", project_id=self.project, stage="official",
                                        known_seq=final_seq, effective_at=effective, max_nodes=5000, depth=32)
                    self.check("publication_boundary", snap.get("root_revision_id") == expected,
                               f"publication step {pub['step']} {label}: expected {expected}, got {snap.get('root_revision_id')}")
                except Exception as exc:
                    self.failures.append(f"[publication_boundary] step {pub['step']} {label}: {exc}")

    def verify_search(self) -> None:
        for case in self.report.get("search_cases", []):
            body = {"project_id": self.project, "query": case["query"], "stage": "working",
                    "known_seq": case["search_known_seq"], "known_at": None,
                    "effective_at": self.checkpoints[26]["event_at"],
                    "roles": [case["role"]], "tags": [], "lanes": ["official"], "limit": 5, "vector": None}
            try:
                actual = self.api.post("/api/search", body)
                ids = [row["revision_id"] for row in actual.get("results", [])]
                expected = case["expected_top_ids"]
                recall = sum(item in ids for item in expected) / len(expected)
                self.check("search_recall", recall >= case["recall_at_5"],
                           f"{case['role']} Recall@5 {recall} below {case['recall_at_5']}; actual={ids}")
                wrong = dict(body)
                wrong["roles"] = [case["negative_role"]]
                wrong_ids = [row["revision_id"] for row in self.api.post("/api/search", wrong).get("results", [])]
                self.check("search_role", not set(expected) & set(wrong_ids),
                           f"{case['role']} expected IDs leaked through role {case['negative_role']}")
                self.check("search_role_exclusion",
                           not set(case["excluded_different_role_ids"]) & set(ids),
                           f"{case['role']} different-role IDs entered positive results")
                historical = dict(body)
                historical["known_seq"] = case["historical_known_seq"]
                old_ids = [row["revision_id"] for row in self.api.post("/api/search", historical).get("results", [])]
                self.check("historical_search", case["historical_expected"] in old_ids,
                           f"{case['role']} historical expected revision missing")
            except Exception as exc:
                self.failures.append(f"[search_request] {case['role']}: {exc}")

        for link in self.report.get("similarity_links", []):
            try:
                detail = self.api.get(f"/api/records/{link['from_revision_id']}", include="links",
                                      known_seq=self.checkpoints[26]["seq"])
                rows = [row for row in detail.get("links", {}).get("outgoing", [])
                        if row.get("link_id") == link["link_id"]]
                self.check("similarity_link", len(rows) == 1 and
                           rows[0].get("to_id") == link["to_revision_id"] and
                           rows[0].get("data", {}).get("similarity", {}).get("score") == link["score"],
                           f"similarity link {link['link_id']} missing or changed")
            except Exception as exc:
                self.failures.append(f"[similarity_link] {link['link_id']}: {exc}")

        for lineage in self.report.get("lineage_cases", []):
            try:
                detail = self.api.get(f"/api/records/{lineage['revision_id']}", include="lineage",
                                      known_seq=self.checkpoints[lineage["step"] - 1]["seq"])
                ids = {row.get("id") for row in detail.get("lineage", {}).get("derived_from", [])}
                self.check("semantic_lineage", lineage["derived_from"] in ids,
                           f"revision {lineage['revision_id']} lost derived_from lineage")
            except Exception as exc:
                self.failures.append(f"[semantic_lineage] {lineage['revision_id']}: {exc}")

    def verify_evidence(self) -> None:
        for item in self.report.get("work_evidence", []):
            cp = self.checkpoints[item["step"] - 1]
            prior_cp = self.checkpoints[item["step"] - 2] if item["step"] > 1 else None
            before_effective = iso_shift(item["event_at"], -1)
            after_effective = iso_shift(item["event_at"], 1)
            target = item["target_revision_id"]
            try:
                if prior_cp is not None:
                    prior = self.api.get(f"/api/records/{target}", include="evidence",
                                         known_seq=prior_cp["seq"], effective_at=after_effective)
                    self.check("future_knowledge_observation",
                               item["observation_id"] not in evidence_ids(prior, "observations"),
                               f"{item['work_id']}: future observation leaked before recording")
                    self.check("future_knowledge_assessment",
                               item["assessment_id"] not in evidence_ids(prior, "assessments"),
                               f"{item['work_id']}: future assessment leaked before recording")
                before = self.api.get(f"/api/records/{target}", include="evidence", known_at=cp["recorded_at"],
                                      effective_at=before_effective)
                self.check("future_observation", item["observation_id"] not in evidence_ids(before, "observations"),
                           f"{item['work_id']}: observation visible before occurred_at")
                self.check("future_assessment", item["assessment_id"] not in evidence_ids(before, "assessments"),
                           f"{item['work_id']}: assessment relying on future evidence is visible")
                after = self.api.get(f"/api/records/{target}", include="evidence", known_at=cp["recorded_at"],
                                     effective_at=after_effective)
                self.check("evidence_visibility", item["observation_id"] in evidence_ids(after, "observations"),
                           f"{item['work_id']}: observation missing after occurred_at")
                self.check("assessment_visibility", item["assessment_id"] in evidence_ids(after, "assessments"),
                           f"{item['work_id']}: assessment missing after evidence cutoff")
                observation = self.api.get(f"/api/records/{item['observation_id']}",
                                           known_at=cp["recorded_at"])["record"]
                observed = observation.get("data", {})
                self.check("observation_integrity",
                           observed.get("value") == item["result"]["value"] and
                           observed.get("environment") == item["result"]["environment"] and
                           item["artifact_id"] in observed.get("artifact_ids", []),
                           f"{item['work_id']}: observed value/environment/artifact changed")
                artifact = self.api.get(f"/api/records/{item['artifact_id']}",
                                        known_at=cp["recorded_at"])["record"]
                self.check("artifact_integrity",
                           artifact.get("data", {}).get("digest") == item["result"]["artifact_sha256"] and
                           artifact.get("data", {}).get("included_in_export") is False,
                           f"{item['work_id']}: artifact digest/manifest flag changed")
                goals_before = self.api.get("/api/goals", project_id=self.project,
                                            root_revision_id=self.report["checkpoints"][1]["working_root"],
                                            known_at=cp["recorded_at"], effective_at=before_effective,
                                            max_nodes=5000)
                goal = next((g for g in goals_before.get("goals", []) if g.get("goal_id") == item["goal_id"]), None)
                if goal is not None:
                    baselines = [b for b in goal.get("baselines", []) if b.get("baseline_id") == item["baseline_id"]]
                    if baselines:
                        official = baselines[0].get("official_assessment")
                        self.check("future_goal", official is None or
                                   official.get("assessment_id") != item["assessment_id"],
                                   f"{item['work_id']}: future-evidence assessment is official")
                goals_after = self.api.get("/api/goals", project_id=self.project,
                                           root_revision_id=self.report["checkpoints"][1]["working_root"],
                                           known_at=cp["recorded_at"], effective_at=after_effective,
                                           max_nodes=5000)
                goal_after = next((g for g in goals_after.get("goals", [])
                                   if g.get("goal_id") == item["goal_id"]), None)
                self.check("goal_presence", goal_after is not None,
                           f"{item['work_id']}: scoped goal missing at its checkpoint")
                if goal_after is not None:
                    baseline_after = next((b for b in goal_after.get("baselines", [])
                                           if b.get("baseline_id") == item["baseline_id"]), None)
                    expected_status = item["rubric"]["status"]
                    official_after = baseline_after.get("official_assessment") if baseline_after else None
                    self.check("assessment_status",
                               official_after is not None and
                               official_after.get("assessment_id") == item["assessment_id"] and
                               official_after.get("status") == expected_status,
                               f"{item['work_id']}: official assessment/status mismatch")
                    self.check("gate_status", goal_after.get("gate_status") == expected_status,
                               f"{item['work_id']}: gate {goal_after.get('gate_status')} != {expected_status}")
            except Exception as exc:
                self.failures.append(f"[evidence_request] {item['work_id']}: {exc}")

        late = next((row for row in self.report.get("work_evidence", []) if row["work_id"] == "late_be_2"), None)
        if late:
            prior = self.checkpoints[26]
            try:
                detail = self.api.get(f"/api/records/{late['target_revision_id']}", include="evidence",
                                      known_seq=prior["seq"], effective_at=iso_shift(late["event_at"], 1))
                self.check("late_knowledge", late["observation_id"] not in evidence_ids(detail, "observations"),
                           "late observation leaked before its recording sequence")
            except Exception as exc:
                self.failures.append(f"[late_knowledge] {exc}")

    def verify_sources(self) -> None:
        for item in self.report.get("source_cases", []):
            try:
                detail = self.api.get(f"/api/records/{item['revision_id']}",
                                      known_seq=self.checkpoints[item["step"] - 1]["seq"])
                source = detail.get("record", {}).get("data", {}).get("source", {})
                self.check("source_capture", source.get("capture_id") == item["capture_id"] and
                           source.get("claim_mode") == "inferred" and source.get("source_anchor") is None,
                           f"revision {item['revision_id']} lost inferred plan capture source")
                capture = self.api.get(f"/api/records/{item['capture_id']}",
                                       known_seq=self.checkpoints[item["step"] - 1]["seq"])
                self.check("source_capture", capture.get("record", {}).get("kind") == "capture",
                           f"source capture {item['capture_id']} does not resolve")
            except Exception as exc:
                self.failures.append(f"[source_capture] {item['revision_id']}: {exc}")

    def run(self) -> None:
        self.verify_checkpoints()
        self.verify_publication_boundaries()
        self.verify_search()
        self.verify_evidence()
        self.verify_sources()
        if self.failures:
            grouped: dict[str, int] = {}
            for failure in self.failures:
                pattern = failure.split("]", 1)[0] + "]"
                grouped[pattern] = grouped.get(pattern, 0) + 1
            raise AcceptanceFailure("lifecycle verification failures by pattern " +
                                    json.dumps(grouped, ensure_ascii=False) + "\n" + "\n".join(self.failures))


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, sort_keys=True, indent=2) + "\n", "utf-8")


def write_markdown(path: Path, report: Json) -> None:
    rows = ["# idea_db 30단계 lifecycle 검증", "", f"- 상태: `{report['status']}`",
            f"- Project: `{report['project_id']}`", f"- 단계: {len(report['steps'])}/30", "",
            "| 단계 | 이름 | 발생 시각 | 서버 기록 시각 | seq |", "|---:|---|---|---|---:|"]
    rows.extend(f"| {s['step']} | {s['name']} | {s['event_at']} | {s['recorded_at']} | {s['seq']} |"
                for s in report["steps"])
    rows.extend(["", "## 실행 결과와 고정 기준", "",
                 "작은 기술 실험의 평가다. 제품 전체 목표 달성으로 해석하지 않는다.", "",
                 "| 실행 | 지표 | 측정값 | 고정 기준 | 평가 |",
                 "|---|---|---:|---|---|"])
    for item in report.get("work_evidence", []):
        result, rubric = item["result"], item["rubric"]
        rows.append(f"| {item['work_id']} | {result['metric']} | {result['value']} | "
                    f"{rubric['comparator']} {rubric['threshold']} | {rubric['status']} |")
    rows.extend(["", "## 검색 점검", ""])
    for case in report.get("search_cases", []):
        rows.append(f"- `{case['role']}` `{case['query']}`: Recall@5={case['recall_at_5']:.2f}, "
                    f"expected={case['expected_top_ids']}, actual={case['actual_top_ids']}")
    rows.extend(["", "의미 임베딩 품질은 이 검증으로 입증하지 않는다.", "", "## 한계", ""])
    rows.extend(f"- {item}" for item in report["limitations"])
    rows.extend(["", "## 실행 아티팩트", ""])
    rows.extend(f"- `{item['work_id']}`: [{item['result']['artifact_path']}]({item['result']['artifact_path']}) "
                f"`{item['result']['artifact_sha256']}`"
                for item in report.get("work_evidence", []))
    path.write_text("\n".join(rows) + "\n", "utf-8")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--base-url", default="http://127.0.0.1:8080")
    parser.add_argument("--token", default=os.environ.get("IDEA_DB_TOKEN"))
    parser.add_argument("--output-dir", type=Path,
                        help="artifact/report directory (required for a writing run)")
    parser.add_argument("--verify-report", type=Path,
                        help="read-only replay of a prior report against a restarted/restored DB")
    parser.add_argument("--timeout", type=float, default=20.0)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv or sys.argv[1:])
    api = Api(args.base_url, args.token, args.timeout)
    if args.output_dir is None and args.verify_report is None:
        raise SystemExit("--output-dir is required unless --verify-report is used")
    output_dir = args.output_dir or args.verify_report.resolve().parent
    if args.verify_report is None:
        if output_dir.exists() and any(output_dir.iterdir()):
            raise SystemExit(f"refusing to overwrite nonempty output directory: {output_dir}")
        output_dir.mkdir(parents=True, exist_ok=True)
    lifecycle: Lifecycle | None = None
    try:
        if args.verify_report:
            report = json.loads(args.verify_report.read_text("utf-8"))
            require(len(report.get("checkpoints", [])) == 30, "verify report must contain all 30 checkpoints")
            Verifier(api, report).run()
            print(json.dumps({"status": "all_30_passed", "mode": "read_only_verify",
                              "project_id": report["project_id"]}, ensure_ascii=False))
            return 0

        prefix = "lc" + uuid.uuid4().hex[:12]
        lifecycle = Lifecycle(api, output_dir, prefix)
        plan_dir = Path(__file__).resolve().parents[1] / "examples" / "lifecycle_30"
        lifecycle.execute(plan_dir)
        report = lifecycle.report()
        Verifier(api, report).run()
        report["status"] = "all_30_passed"
        export = api.get("/api/export", include_receipts="true")
        report["full_export_digest"] = export.get("digest")
        report["full_export_seq"] = export.get("content", {}).get("seq")
        write_json(output_dir / "export.json", export)
        write_json(output_dir / "checkpoint.json", {
            "format": "idea_db.lifecycle-checkpoints", "project_id": lifecycle.project,
            "checkpoints": lifecycle.checkpoints, "publications": report["publications"],
        })
        write_json(output_dir / "report.json", report)
        write_markdown(output_dir / "report.md", report)
        print(json.dumps({"status": report["status"], "project_id": lifecycle.project,
                          "report": str((output_dir / "report.json").resolve())}, ensure_ascii=False))
        return 0
    except Exception as exc:
        if args.verify_report is not None:
            failure = {"status": "failed", "mode": "read_only_verify",
                       "error_type": type(exc).__name__, "error": str(exc)}
            print(json.dumps(failure, ensure_ascii=False), file=sys.stderr)
            return 1
        failure = lifecycle.report() if lifecycle is not None else {}
        failure.update({"status": "failed", "error_type": type(exc).__name__, "error": str(exc)})
        write_json(output_dir / "report.json", failure)
        if failure.get("steps"):
            write_markdown(output_dir / "report.md", failure)
        print(json.dumps(failure, ensure_ascii=False), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
