//! Domain rules from `docs/api.md` §6, exercised without a database.

use super::*;
use crate::graph::fixtures::*;
use serde_json::{json, Value};

fn ctx(records: Vec<StoredRecord>) -> Context {
    let mut c = Context::default();
    for r in records {
        if let Some(rev) = r.as_revision() {
            c.entity_revisions
                .entry(rev.entity_id.clone())
                .or_default()
                .push((r.seq, r.id.clone()));
        }
        c.seq = c.seq.max(r.seq);
        c.records.insert(r.id.clone(), r);
    }
    for v in c.entity_revisions.values_mut() {
        v.sort();
    }
    c
}

fn login_ctx() -> Context {
    ctx(deep_diamond().into_values().collect())
}

fn package(records: Value) -> Package {
    serde_json::from_value(json!({
        "protocol_version": 1,
        "idempotency_key": "test-key",
        "actor": "human:test",
        "reason": "test",
        "records": records
    }))
    .expect("package shape")
}

fn run(pkg: &Package, ctx: &Context) -> Result<Plan, Vec<Issue>> {
    validate_package(pkg, ctx, ctx.seq + 1, "2026-09-07T00:00:00.000Z")
}

fn codes(issues: &[Issue]) -> Vec<&str> {
    issues.iter().map(|i| i.code).collect()
}

fn expect_code(pkg: &Package, ctx: &Context, code: &str) {
    match run(pkg, ctx) {
        Ok(_) => panic!("expected {code}, but validation passed"),
        Err(issues) => assert!(
            codes(&issues).contains(&code),
            "expected {code}, got {:?}",
            issues
        ),
    }
}

fn revision(id: &str, entity: &str, body: &str, slots: Value) -> Value {
    json!({"id": id, "kind": "revision", "data": {
        "entity_id": entity, "body": body, "slots": slots,
        "change_kind": "initial",
        "source": {"origin": "human", "claim_mode": "inferred"}
    }})
}

fn entity(id: &str, kind: &str, project: &str) -> Value {
    json!({"id": id, "kind": "entity", "data": {
        "entity_kind": kind, "project_id": project, "title": id
    }})
}

// ---------------------------------------------------------------- references

#[test]
fn unknown_and_mistyped_references_are_rejected() {
    let ctx = login_ctx();
    expect_code(
        &package(json!([entity("ent_new", "core", "proj_missing")])),
        &ctx,
        "unknown_reference",
    );
    // ent_lockout is an entity, not a project.
    expect_code(
        &package(json!([entity("ent_new", "core", "ent_lockout")])),
        &ctx,
        "wrong_reference_kind",
    );
}

#[test]
fn a_revision_cannot_contain_itself() {
    let ctx = login_ctx();
    let pkg = package(json!([
        entity("ent_new", "core", "proj_login"),
        revision(
            "rev_new",
            "ent_new",
            "self",
            json!([{"slot_id": "me", "revision_id": "rev_new"}])
        ),
    ]));
    expect_code(&pkg, &ctx, "self_reference");
}

#[test]
fn mutual_containment_is_a_cycle() {
    let ctx = login_ctx();
    let pkg = package(json!([
        entity("ent_a", "core", "proj_login"),
        entity("ent_b", "core", "proj_login"),
        revision(
            "rev_a",
            "ent_a",
            "a",
            json!([{"slot_id": "b", "revision_id": "rev_b"}])
        ),
        revision(
            "rev_b",
            "ent_b",
            "b",
            json!([{"slot_id": "a", "revision_id": "rev_a"}])
        ),
    ]));
    expect_code(&pkg, &ctx, "cycle_detected");
}

#[test]
fn ids_are_immutable_and_unique() {
    let ctx = login_ctx();
    expect_code(
        &package(json!([entity("ent_lockout", "idea", "proj_login")])),
        &ctx,
        "duplicate_id",
    );
    expect_code(
        &package(json!([entity("Ent_Upper", "idea", "proj_login")])),
        &ctx,
        "bad_id_format",
    );
}

#[test]
fn head_change_records_cannot_be_submitted_by_a_client() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "hc_forged", "kind": "head_change", "data": {
            "project_id": "proj_login", "stage": "working",
            "after_revision_id": "rev_root", "reason": "r", "actor": "a", "meaningful": true,
            "decision": {"before": "", "after": "", "rationale": "r"}
        }
    }]));
    expect_code(&pkg, &ctx, "forbidden_kind");
}

// ---------------------------------------------------------------- containment

#[test]
fn containment_rules_hold_for_every_level() {
    let ctx = login_ctx();
    // An idea is atomic.
    expect_code(
        &package(json!([
            entity("ent_atom", "idea", "proj_login"),
            revision(
                "rev_atom",
                "ent_atom",
                "atom",
                json!([{"slot_id": "x", "revision_id": "rev_lockout_1"}])
            ),
        ])),
        &ctx,
        "atom_cannot_contain",
    );
    // A core may not contain a schema.
    expect_code(
        &package(json!([
            entity("ent_wrap", "core", "proj_login"),
            revision(
                "rev_wrap",
                "ent_wrap",
                "wrap",
                json!([{"slot_id": "s", "revision_id": "rev_auth"}])
            ),
        ])),
        &ctx,
        "invalid_containment",
    );
    // A project root takes schemas only.
    expect_code(
        &package(json!([revision(
            "rev_root_2",
            "proj_login",
            "root",
            json!([{"slot_id": "policy", "revision_id": "rev_lockout_1"}])
        )])),
        &ctx,
        "invalid_containment",
    );
}

#[test]
fn a_project_root_may_hold_many_unrelated_schemas() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "rev_root_2", "kind": "revision", "data": {
            "entity_id": "proj_login",
            "body": "여러 스키마 루트",
            "slots": [
                {"slot_id": "auth", "revision_id": "rev_auth"},
                {"slot_id": "billing", "revision_id": "rev_billing"},
                {"slot_id": "auth_again", "revision_id": "rev_auth"}
            ],
            "change_kind": "composition",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }
    }]));
    let plan = run(&pkg, &ctx).expect("unrelated sibling schemas need no invented wrapper");
    assert_eq!(plan.records.len(), 1);
    assert_eq!(
        plan.resolved_previous.get("rev_root_2").map(String::as_str),
        Some("rev_root")
    );
}

// ---------------------------------------------------------------- reuse

#[test]
fn an_idea_may_be_reused_by_another_project() {
    let ctx = login_ctx();
    let pkg = package(json!([
        {"id": "proj_admin", "kind": "project", "data": {"title": "관리자 콘솔"}},
        entity("ent_admin_core", "core", "proj_admin"),
        revision(
            "rev_admin_core",
            "ent_admin_core",
            "관리자도 같은 잠금 정책을 쓴다",
            json!([{"slot_id": "policy", "revision_id": "rev_lockout_1", "roles": ["be"]}])
        ),
    ]));
    run(&pkg, &ctx).expect("cross-project reuse of an idea is allowed");
}

#[test]
fn a_schema_stays_inside_its_origin_project() {
    let ctx = login_ctx();
    let pkg = package(json!([
        {"id": "proj_admin", "kind": "project", "data": {"title": "관리자 콘솔"}},
        revision(
            "rev_admin_root",
            "proj_admin",
            "관리자 루트",
            json!([{"slot_id": "auth", "revision_id": "rev_auth"}])
        ),
    ]));
    expect_code(&pkg, &ctx, "schema_cross_project_composition");
}

// ---------------------------------------------------------------- edits

#[test]
fn a_wording_fix_is_a_correction() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "rev_lockout_1b", "kind": "revision", "data": {
            "entity_id": "ent_lockout",
            "body": "연속 5회 실패 시 30분 동안 잠근다",
            "change_kind": "correction",
            "correction_of": "rev_lockout_1",
            "correction_reason": "문장 다듬음",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }
    }]));
    let plan = run(&pkg, &ctx).expect("same numbers, cosmetic");
    assert!(
        codes(&plan.warnings).contains(&"correction_needs_human_review"),
        "the numeric guard must not be presented as proof of equivalence"
    );
}

#[test]
fn changing_a_number_cannot_be_filed_as_a_correction() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "rev_lockout_1b", "kind": "revision", "data": {
            "entity_id": "ent_lockout",
            "body": "5회 실패 시 10분 잠금",
            "change_kind": "correction",
            "correction_of": "rev_lockout_1",
            "correction_reason": "오타",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }
    }]));
    expect_code(&pkg, &ctx, "semantic_edit_required");
}

#[test]
fn a_correction_stays_inside_one_entity_and_needs_a_reason() {
    let ctx = login_ctx();
    expect_code(
        &package(json!([{
            "id": "rev_x", "kind": "revision", "data": {
                "entity_id": "ent_l5", "body": "레벨5", "change_kind": "correction",
                "correction_of": "rev_lockout_1", "correction_reason": "이유",
                "source": {"origin": "human", "claim_mode": "inferred"}
            }
        }])),
        &ctx,
        "correction_entity_mismatch",
    );
    expect_code(
        &package(json!([{
            "id": "rev_x", "kind": "revision", "data": {
                "entity_id": "ent_lockout", "body": "5회 실패 시 30분 잠금",
                "change_kind": "correction", "correction_of": "rev_lockout_1",
                "source": {"origin": "human", "claim_mode": "inferred"}
            }
        }])),
        &ctx,
        "correction_requires_reason",
    );
}

#[test]
fn a_semantic_edit_starts_a_new_entity_with_lineage() {
    let ctx = login_ctx();
    let good = package(json!([
        {"id": "ent_lockout_v2", "kind": "entity", "data": {
            "entity_kind": "idea", "project_id": "proj_login", "title": "10분 잠금",
            "derived_from": ["ent_lockout"], "lineage_kind": "semantic_edit"
        }},
        {"id": "rev_lockout_v2", "kind": "revision", "data": {
            "entity_id": "ent_lockout_v2", "body": "5회 실패 시 10분 잠금",
            "change_kind": "semantic",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }},
    ]));
    run(&good, &ctx).expect("new entity plus derived_from");

    let bad = package(json!([{
        "id": "rev_lockout_v2", "kind": "revision", "data": {
            "entity_id": "ent_lockout", "body": "5회 실패 시 10분 잠금",
            "change_kind": "semantic",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }
    }]));
    expect_code(&bad, &ctx, "semantic_requires_new_entity");
}

#[test]
fn composition_versions_structure_but_never_an_idea() {
    let ctx = login_ctx();
    let ok = package(json!([{
        "id": "rev_auth_2", "kind": "revision", "data": {
            "entity_id": "ent_auth", "body": "인증 스키마 v2",
            "slots": [{"slot_id": "session", "revision_id": "rev_l3"}],
            "change_kind": "composition",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }
    }]));
    let plan = run(&ok, &ctx).expect("schema composition is allowed");
    assert_eq!(
        plan.resolved_previous.get("rev_auth_2").map(String::as_str),
        Some("rev_auth"),
        "the server resolves the predecessor when the client omits it"
    );

    let bad = package(json!([{
        "id": "rev_lockout_2", "kind": "revision", "data": {
            "entity_id": "ent_lockout", "body": "다른 본문",
            "change_kind": "composition",
            "source": {"origin": "human", "claim_mode": "inferred"}
        }
    }]));
    expect_code(&bad, &ctx, "composition_not_allowed_on_idea");
}

#[test]
fn an_entity_gets_exactly_one_initial_revision() {
    let ctx = login_ctx();
    expect_code(
        &package(json!([revision(
            "rev_lockout_again",
            "ent_lockout",
            "다시",
            json!([])
        )])),
        &ctx,
        "initial_already_exists",
    );
}

// ---------------------------------------------------------------- provenance

#[test]
fn an_extracted_claim_must_match_its_anchor_exactly() {
    let mut ctx = login_ctx();
    let cap = record(
        "cap_meeting",
        60,
        RecordKind::Capture,
        json!({
            "project_id": "proj_login",
            "content": "회의 요약: 연속 5회 실패 시 30분 잠금. 이후 논의 계속.",
            "source_kind": "note"
        }),
    );
    ctx.records.insert(cap.id.clone(), cap);

    let content = "회의 요약: 연속 5회 실패 시 30분 잠금. 이후 논의 계속.";
    let quote = "연속 5회 실패 시 30분 잠금";
    let start = content
        .chars()
        .collect::<Vec<_>>()
        .windows(quote.chars().count())
        .position(|w| w.iter().collect::<String>() == quote)
        .expect("quote is present");

    let anchored = |body: &str, start: usize, end: usize| {
        package(json!([
            entity("ent_from_note", "idea", "proj_login"),
            {"id": "rev_from_note", "kind": "revision", "data": {
                "entity_id": "ent_from_note", "body": body, "change_kind": "initial",
                "source": {
                    "origin": "human", "capture_id": "cap_meeting", "claim_mode": "extracted",
                    "source_anchor": {"capture_id": "cap_meeting", "start": start, "end": end}
                }
            }}
        ]))
    };
    let end = start + quote.chars().count();
    run(&anchored(quote, start, end), &ctx).expect("an exact quote is an extracted claim");
    // A paraphrase over the same span is not an extracted claim.
    expect_code(
        &anchored("5회 실패하면 30분 잠긴다", start, end),
        &ctx,
        "source_anchor_text_mismatch",
    );
    expect_code(
        &anchored(quote, start, 9999),
        &ctx,
        "source_anchor_out_of_range",
    );
}

#[test]
fn an_inferred_claim_carries_no_anchor() {
    let ctx = login_ctx();
    let pkg = package(json!([
        entity("ent_guess", "idea", "proj_login"),
        {"id": "rev_guess", "kind": "revision", "data": {
            "entity_id": "ent_guess", "body": "추론한 내용", "change_kind": "initial",
            "source": {
                "origin": "ai", "model": "local-qwen3-8b", "claim_mode": "inferred",
                "source_anchor": {"capture_id": "cap_x", "start": 0, "end": 1}
            }
        }}
    ]));
    expect_code(&pkg, &ctx, "inferred_must_not_anchor");
}

#[test]
fn a_declared_capture_digest_must_be_true() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "cap_bad", "kind": "capture", "data": {
            "content": "원문", "source_kind": "note", "content_digest": "sha256:0000"
        }
    }]));
    expect_code(&pkg, &ctx, "content_digest_mismatch");
}

#[test]
fn artifact_bytes_are_not_storable_in_this_release() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "art_run", "kind": "artifact", "data": {
            "uri": "file:///runs/a.json", "digest": "sha256:ab", "included_in_export": true
        }
    }]));
    expect_code(&pkg, &ctx, "artifact_bytes_unsupported");
}

// ---------------------------------------------------------------- goals

fn goal_package(extra: Vec<Value>) -> Package {
    let mut records = vec![json!({
        "id": "goal_lockout", "kind": "goal", "data": {
            "project_id": "proj_login",
            "scope": {
                "root_revision_id": "rev_root",
                "slot_path": ["auth", "direct_policy"],
                "target_revision_id": "rev_lockout_1"
            },
            "statement": "탈취를 막으면서 정상 사용자를 막지 않는다",
            "criteria": [
                {"criterion_id": "c_takeover", "kind": "quantitative", "statement": "탈취율",
                 "metric": "takeover_rate", "comparator": "lte", "threshold": 0.001,
                 "unit": "ratio", "required": true}
            ],
            "origin": "official", "actor": "human:test"
        }
    })];
    records.extend(extra);
    package(Value::Array(records))
}

#[test]
fn a_goal_scope_must_resolve_to_its_target() {
    let ctx = login_ctx();
    let pkg = package(json!([{
        "id": "goal_bad", "kind": "goal", "data": {
            "project_id": "proj_login",
            "scope": {"root_revision_id": "rev_root", "slot_path": ["auth"], "target_revision_id": "rev_lockout_1"},
            "statement": "s",
            "criteria": [{"criterion_id": "c1", "kind": "qualitative", "statement": "q"}],
            "origin": "official", "actor": "human:test"
        }
    }]));
    expect_code(&pkg, &ctx, "goal_scope_mismatch");
}

#[test]
fn an_assessment_must_match_its_baseline_scope() {
    let ctx = login_ctx();
    let baseline = json!({
        "id": "bl_1", "kind": "baseline", "data": {
            "goal_id": "goal_lockout", "criterion_ids": ["c_takeover"],
            "effective_from": "2026-09-01T00:00:00+09:00",
            "actor": "human:test", "reason": "1차"
        }
    });
    let assessment = |path: Value, target: &str| {
        json!({"id": "asm_1", "kind": "assessment", "data": {
            "root_revision_id": "rev_root", "slot_path": path, "target_revision_id": target,
            "baseline_id": "bl_1", "evidence_cutoff_seq": 1000,
            "evidence_cutoff_at": "2026-09-10T00:00:00+09:00",
            "evaluator": "human:test", "rubric_version": "v1",
            "origin": "official", "status": "unknown",
            "criteria_results": [{"criterion_id": "c_takeover", "status": "unknown"}]
        }})
    };
    run(
        &goal_package(vec![
            baseline.clone(),
            assessment(json!(["auth", "direct_policy"]), "rev_lockout_1"),
        ]),
        &ctx,
    )
    .expect("matching scope");

    // Same atom, different use site: the baseline does not carry over.
    expect_code(
        &goal_package(vec![
            baseline,
            assessment(json!(["billing", "policy"]), "rev_lockout_1"),
        ]),
        &ctx,
        "baseline_scope_mismatch",
    );
}

#[test]
fn evidence_recorded_after_the_cutoff_is_refused() {
    let mut ctx = login_ctx();
    let obs = record(
        "obs_late",
        900,
        RecordKind::Observation,
        json!({
            "project_id": "proj_login", "metric": "takeover_rate", "value": 0.0004,
            "status": "observed", "occurred_at": "2026-09-20T00:00:00+09:00", "actor": "human:test"
        }),
    );
    ctx.records.insert(obs.id.clone(), obs);
    ctx.seq = 900;

    let pkg = goal_package(vec![
        json!({"id": "bl_1", "kind": "baseline", "data": {
            "goal_id": "goal_lockout", "criterion_ids": ["c_takeover"],
            "effective_from": "2026-09-01T00:00:00+09:00", "actor": "human:test", "reason": "1차"
        }}),
        json!({"id": "asm_1", "kind": "assessment", "data": {
            "root_revision_id": "rev_root", "slot_path": ["auth", "direct_policy"],
            "target_revision_id": "rev_lockout_1", "baseline_id": "bl_1",
            "evidence_cutoff_seq": 100, "evidence_cutoff_at": "2026-09-06T00:00:00+09:00",
            "evaluator": "human:test", "rubric_version": "v1", "origin": "official", "status": "met",
            "criteria_results": [{"criterion_id": "c_takeover", "status": "met"}],
            "evidence_observation_ids": ["obs_late"]
        }}),
    ]);
    expect_code(&pkg, &ctx, "evidence_after_cutoff");
}

#[test]
fn a_baseline_may_supersede_another_goal_at_the_same_scope() {
    let ctx = login_ctx();
    let stricter_goal = json!({
        "id": "goal_lockout_v2", "kind": "goal", "data": {
            "project_id": "proj_login",
            "scope": {"root_revision_id": "rev_root", "slot_path": ["auth", "direct_policy"],
                      "target_revision_id": "rev_lockout_1"},
            "statement": "더 엄격한 기준",
            "criteria": [{"criterion_id": "c_takeover", "kind": "quantitative", "statement": "탈취율",
                          "metric": "takeover_rate", "comparator": "lte", "threshold": 0.0001,
                          "unit": "ratio", "required": true}],
            "origin": "official", "actor": "human:test"
        }
    });
    let base = json!({"id": "bl_1", "kind": "baseline", "data": {
        "goal_id": "goal_lockout", "criterion_ids": ["c_takeover"],
        "effective_from": "2026-09-01T00:00:00+09:00", "actor": "human:test", "reason": "1차"
    }});
    let next = json!({"id": "bl_2", "kind": "baseline", "data": {
        "goal_id": "goal_lockout_v2", "criterion_ids": ["c_takeover"],
        "effective_from": "2026-10-01T00:00:00+09:00", "supersedes": "bl_1",
        "actor": "human:test", "reason": "기준 강화"
    }});
    run(&goal_package(vec![stricter_goal, base.clone(), next]), &ctx)
        .expect("a threshold change needs a new goal, and the baseline chain survives it");

    let elsewhere = json!({
        "id": "goal_other", "kind": "goal", "data": {
            "project_id": "proj_login",
            "scope": {"root_revision_id": "rev_root", "slot_path": ["billing", "policy"],
                      "target_revision_id": "rev_lockout_1"},
            "statement": "다른 사용 위치",
            "criteria": [{"criterion_id": "c_takeover", "kind": "qualitative", "statement": "q", "required": true}],
            "origin": "official", "actor": "human:test"
        }
    });
    let crossing = json!({"id": "bl_3", "kind": "baseline", "data": {
        "goal_id": "goal_other", "criterion_ids": ["c_takeover"],
        "effective_from": "2026-10-01T00:00:00+09:00", "supersedes": "bl_1",
        "actor": "human:test", "reason": "잘못된 승계"
    }});
    expect_code(
        &goal_package(vec![elsewhere, base, crossing]),
        &ctx,
        "baseline_supersede_scope_mismatch",
    );
}

#[test]
fn an_ai_proposed_goal_cannot_back_an_official_assessment() {
    let ctx = login_ctx();
    let pkg = package(json!([
        {"id": "goal_ai", "kind": "goal", "data": {
            "project_id": "proj_login",
            "scope": {"root_revision_id": "rev_root", "slot_path": ["auth", "direct_policy"],
                      "target_revision_id": "rev_lockout_1"},
            "statement": "모델이 제안한 목표",
            "criteria": [{"criterion_id": "c1", "kind": "qualitative", "statement": "q", "required": true}],
            "origin": "ai_proposed", "actor": "ai:local-qwen3-8b"
        }},
        {"id": "bl_ai", "kind": "baseline", "data": {
            "goal_id": "goal_ai", "criterion_ids": ["c1"],
            "effective_from": "2026-09-01T00:00:00+09:00", "actor": "human:test", "reason": "검토용"
        }},
        {"id": "asm_ai", "kind": "assessment", "data": {
            "root_revision_id": "rev_root", "slot_path": ["auth", "direct_policy"],
            "target_revision_id": "rev_lockout_1", "baseline_id": "bl_ai",
            "evidence_cutoff_seq": 1000, "evidence_cutoff_at": "2026-09-10T00:00:00+09:00",
            "evaluator": "human:test", "rubric_version": "v1", "origin": "official", "status": "met",
            "criteria_results": [{"criterion_id": "c1", "status": "met"}]
        }}
    ]));
    expect_code(&pkg, &ctx, "official_assessment_from_proposed_goal");
}

// ---------------------------------------------------------------- links

#[test]
fn link_endpoints_are_type_checked() {
    let ctx = login_ctx();
    // `applies` connects entities, not revisions.
    expect_code(
        &package(json!([{
            "id": "lnk_1", "kind": "link", "data": {
                "link_type": "applies", "from_id": "rev_auth", "to_id": "rev_lockout_1", "actor": "human:test"
            }
        }])),
        &ctx,
        "invalid_link_endpoints",
    );
}

#[test]
fn an_impact_claim_separates_estimate_from_observation() {
    let ctx = login_ctx();
    let goal = json!({"id": "goal_g", "kind": "goal", "data": {
        "project_id": "proj_login",
        "scope": {"root_revision_id": "rev_root", "slot_path": ["auth", "direct_policy"],
                  "target_revision_id": "rev_lockout_1"},
        "statement": "s",
        "criteria": [{"criterion_id": "c1", "kind": "qualitative", "statement": "q"}],
        "origin": "official", "actor": "human:test"
    }});
    let link = |evidence: &str, obs: Value, method: Value| {
        package(json!([goal.clone(), {
            "id": "lnk_i", "kind": "link", "data": {
                "link_type": "impact", "from_id": "ent_lockout", "to_id": "goal_g", "actor": "human:test",
                "impact": {"scope": "CS 문의", "direction": "decrease", "evidence_kind": evidence,
                           "observation_ids": obs, "method": method}
            }
        }]))
    };
    run(&link("estimated", json!([]), json!("과거 사례 비교")), &ctx)
        .expect("an estimate states its method");
    expect_code(
        &link("estimated", json!([]), Value::Null),
        &ctx,
        "impact_evidence_mismatch",
    );
    expect_code(
        &link("observed", json!([]), Value::Null),
        &ctx,
        "impact_evidence_mismatch",
    );
}

// ---------------------------------------------------------------- head

#[test]
fn every_root_change_carries_a_decision() {
    let ctx = login_ctx();
    let mut pkg = package(json!([]));
    pkg.expected_heads = serde_json::from_value(json!([
        {"project_id": "proj_login", "stage": "working", "revision_id": null}
    ]))
    .unwrap();
    pkg.root_change = serde_json::from_value(json!({
        "project_id": "proj_login", "stage": "working", "after_revision_id": "rev_root",
        "reason": "", "meaningful": false,
        "decision": {"before": "", "after": "", "rationale": ""}
    }))
    .unwrap();
    expect_code(&pkg, &ctx, "root_change_requires_decision");

    pkg.root_change = serde_json::from_value(json!({
        "project_id": "proj_login", "stage": "working", "after_revision_id": "rev_root",
        "reason": "최초 구성", "meaningful": true,
        "decision": {"before": "루트 없음", "after": "auth/billing", "rationale": "정책 재사용을 위해"}
    }))
    .unwrap();
    let plan = run(&pkg, &ctx).expect("a documented root change is accepted");
    assert_eq!(plan.head_updates.len(), 1);
    assert!(plan
        .records
        .iter()
        .any(|r| r.kind() == RecordKind::HeadChange));
}

#[test]
fn a_head_must_belong_to_its_project() {
    let mut ctx = login_ctx();
    let other = record(
        "proj_admin",
        70,
        RecordKind::Project,
        json!({"title": "관리자"}),
    );
    ctx.records.insert(other.id.clone(), other);

    let mut pkg = package(json!([]));
    pkg.expected_heads = serde_json::from_value(json!([
        {"project_id": "proj_admin", "stage": "working", "revision_id": null}
    ]))
    .unwrap();
    pkg.root_change = serde_json::from_value(json!({
        "project_id": "proj_admin", "stage": "working", "after_revision_id": "rev_root",
        "reason": "잘못된 루트", "meaningful": true,
        "decision": {"before": "", "after": "", "rationale": "이유"}
    }))
    .unwrap();
    expect_code(&pkg, &ctx, "invalid_head_revision");
}

#[test]
fn batch_first_revisions_and_chronological_cycles_cannot_bypass_immutability() {
    let c = login_ctx();
    expect_code(
        &package(json!([
            entity("ent_new", "idea", "proj_login"),
            revision("rev_new_a", "ent_new", "a", json!([])),
            revision("rev_new_b", "ent_new", "b", json!([]))
        ])),
        &c,
        "initial_already_exists",
    );
    expect_code(
        &package(json!([
            {"id":"ent_a","kind":"entity","data":{"entity_kind":"idea","project_id":"proj_login","title":"a","derived_from":["ent_b"],"lineage_kind":"semantic_edit"}},
            {"id":"ent_b","kind":"entity","data":{"entity_kind":"idea","project_id":"proj_login","title":"b","derived_from":["ent_a"],"lineage_kind":"semantic_edit"}}
        ])),
        &c,
        "history_cycle_detected",
    );
    let mut a = revision("rev_new_a", "ent_auth", "a", json!([]));
    a["data"]["change_kind"] = json!("composition");
    a["data"]["previous_revision_id"] = json!("rev_new_b");
    let mut b = a.clone();
    b["id"] = json!("rev_new_b");
    b["data"]["previous_revision_id"] = json!("rev_new_a");
    expect_code(&package(json!([a, b])), &c, "history_cycle_detected");
}

#[test]
fn goals_cannot_claim_a_different_project_than_their_scope() {
    let mut c = login_ctx();
    c.records
        .insert("proj_other".into(), project("proj_other", 1));
    let mut p = goal_package(vec![]);
    p.records[0].data["project_id"] = json!("proj_other");
    expect_code(&p, &c, "goal_project_mismatch");
}

fn judged_package() -> Package {
    goal_package(vec![
        json!({"id":"bl_checked","kind":"baseline","data":{"goal_id":"goal_lockout","criterion_ids":["c_takeover"],"effective_from":"2026-01-01T00:00:00Z","actor":"human:test","reason":"fixed"}}),
        json!({"id":"obs_checked","kind":"observation","data":{"project_id":"proj_login","target_revision_id":"rev_lockout_1","metric":"takeover_rate","value":0.0004,"status":"observed","occurred_at":"2026-01-01T00:00:00Z","actor":"human:test"}}),
        json!({"id":"asm_checked","kind":"assessment","data":{"root_revision_id":"rev_root","slot_path":["auth","direct_policy"],"target_revision_id":"rev_lockout_1","baseline_id":"bl_checked","evidence_cutoff_seq":1000,"evidence_cutoff_at":"2026-09-01T00:00:00Z","evaluator":"human:test","rubric_version":"1","origin":"official","status":"met","criteria_results":[{"criterion_id":"c_takeover","status":"met","observed_value":0.0004}],"evidence_observation_ids":["obs_checked"]}}),
    ])
}

#[test]
fn evidence_and_completion_must_agree_with_scope_and_required_criteria() {
    let c = login_ctx();
    let good = judged_package();
    run(&good, &c).expect("scoped observation supports official assessment");
    let mut wrong = good.clone();
    wrong.records[2].data["target_revision_id"] = json!("rev_auth");
    expect_code(&wrong, &c, "evidence_scope_mismatch");
    let mut duplicated = good.clone();
    let first = duplicated.records[3].data["criteria_results"][0].clone();
    duplicated.records[3].data["criteria_results"]
        .as_array_mut()
        .unwrap()
        .push(first);
    expect_code(&duplicated, &c, "duplicate_criterion_result");
    let mut conflicting = good.clone();
    conflicting.records[3].data["criteria_results"][0]["status"] = json!("unmet");
    expect_code(&conflicting, &c, "assessment_status_mismatch");
    let mut unsupported = good;
    unsupported.records[3].data["evidence_observation_ids"] = json!([]);
    expect_code(&unsupported, &c, "assessment_missing_evidence");
}

#[test]
fn qualitative_observations_do_not_require_invented_numeric_values() {
    let c = login_ctx();
    for value in [json!("인터뷰에서 사용자가 목적을 설명했다"), json!(true)] {
        let p = package(
            json!([{"id":"obs_words","kind":"observation","data":{"project_id":"proj_login","target_revision_id":"rev_lockout_1","metric":"interview_result","value":value,"status":"observed","occurred_at":"2026-01-01T00:00:00Z","actor":"human:test"}}]),
        );
        run(&p, &c).expect("qualitative scalar evidence");
    }
}

#[test]
fn project_head_must_be_the_projects_own_revision() {
    let ctx = login_ctx();
    let mut pkg = package(json!([]));
    pkg.expected_heads = serde_json::from_value(json!([
        {"project_id":"proj_login","stage":"working","revision_id":"rev_root"}
    ]))
    .unwrap();
    pkg.root_change = serde_json::from_value(json!({
        "project_id":"proj_login","stage":"working","after_revision_id":"rev_lockout_1",
        "reason":"attempt narrow head","meaningful":true,
        "decision":{"before":"root","after":"atom","rationale":"test"}
    }))
    .unwrap();
    expect_code(&pkg, &ctx, "invalid_head_revision");
}
