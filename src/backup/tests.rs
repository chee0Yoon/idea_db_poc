//! Restore rejection rules, exercised without a database.
//!
//! Each test builds a document that would restore state the live write path
//! would never have produced, and asserts the replay refuses it.

use super::*;

const T1: &str = "2026-09-07T00:00:01.000Z";
const T2: &str = "2026-09-07T00:00:02.000Z";
const T3: &str = "2026-09-07T00:00:03.000Z";

fn rec(id: &str, seq: i64, at: &str, kind: RecordKind, data: Value) -> Value {
    json!({"id": id, "kind": kind.as_str(), "seq": seq, "recorded_at": at, "data": data})
}

fn project(id: &str, seq: i64, at: &str) -> Value {
    rec(
        id,
        seq,
        at,
        RecordKind::Project,
        json!({"title": id, "description": ""}),
    )
}

fn entity(id: &str, seq: i64, at: &str, project: &str, kind: &str) -> Value {
    rec(
        id,
        seq,
        at,
        RecordKind::Entity,
        json!({"entity_kind": kind, "project_id": project, "title": id,
               "tags": [], "derived_from": [], "lineage_kind": "none"}),
    )
}

fn revision(id: &str, seq: i64, at: &str, entity_id: &str, slots: Value) -> Value {
    rec(
        id,
        seq,
        at,
        RecordKind::Revision,
        json!({"entity_id": entity_id, "body": id, "tags": [], "slots": slots,
               "change_kind": "initial", "correction_of": null, "correction_reason": null,
               "previous_revision_id": null,
               "source": {"origin": "human", "capture_id": null, "model": null,
                          "skill": null, "claim_mode": "inferred", "source_anchor": null}}),
    )
}

fn head_change(id: &str, seq: i64, at: &str, project: &str, before: Value, after: &str) -> Value {
    rec(
        id,
        seq,
        at,
        RecordKind::HeadChange,
        json!({"project_id": project, "stage": "working", "before_revision_id": before,
               "after_revision_id": after, "reason": "restore fixture", "actor": "human:test",
               "meaningful": true,
               "decision": {"before": "none", "after": "root", "rationale": "fixture"}}),
    )
}

fn publication(id: &str, seq: i64, at: &str, project: &str, root: &str, published: &str) -> Value {
    rec(
        id,
        seq,
        at,
        RecordKind::Publication,
        json!({"project_id": project, "root_revision_id": root, "label": "v1",
               "published_at": published, "notes": "", "actor": "human:test"}),
    )
}

/// Two commits: commit 1 builds a project root, commit 2 moves the head and
/// publishes. This is the shape the live path actually produces.
fn base_records() -> Vec<Value> {
    vec![
        project("proj_a", 1, T1),
        entity("ent_schema", 1, T1, "proj_a", "schema"),
        revision("rev_schema", 1, T1, "ent_schema", json!([])),
        revision(
            "rev_root",
            1,
            T1,
            "proj_a",
            json!([{"slot_id": "s", "revision_id": "rev_schema", "roles": []}]),
        ),
        head_change("hc_1", 2, T2, "proj_a", Value::Null, "rev_root"),
        publication("pub_1", 2, T2, "proj_a", "rev_root", "2026-09-01T00:00:00Z"),
    ]
}

fn content(records: Vec<Value>, receipts: Vec<Value>) -> Value {
    json!({
        "seq": records.iter().filter_map(|r| r["seq"].as_i64()).max().unwrap_or(0),
        "records": records,
        "heads": [],
        "receipts": receipts,
        "manifest": {},
    })
}

fn replay(records: &[Value]) -> ApiResult<BTreeMap<String, HeadState>> {
    let content = content(records.to_vec(), Vec::new());
    let parsed = parse_records(&content)?;
    let final_seq = content["seq"].as_i64().unwrap();
    revalidate(&parsed, final_seq)
}

fn expect_replay_rejects(records: &[Value], code: &str) {
    match replay(records) {
        Ok(_) => panic!("expected {code}, but the replay accepted the document"),
        Err(e) => {
            let codes: Vec<String> = e
                .details
                .as_ref()
                .and_then(|d| d.get("issues"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|i| i.get("code").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                codes.iter().any(|c| c == code),
                "expected {code}, got {codes:?}: {}",
                e.message
            );
        }
    }
}

fn expect_receipts_reject(records: &[Value], receipts: Vec<Value>, code: &str) {
    let content = content(records.to_vec(), receipts);
    let parsed = parse_records(&content).expect("records parse");
    let final_seq = content["seq"].as_i64().unwrap();
    match check_receipts(&content, &parsed, final_seq) {
        Ok(_) => panic!("expected {code}, but the receipts were accepted"),
        Err(e) => assert_eq!(e.code, "validation_failed", "{}", e.message),
    }
}

// ---------------------------------------------------------------- baseline

#[test]
fn a_well_formed_document_replays() {
    let heads = replay(&base_records()).expect("a document the live path could have produced");
    assert_eq!(
        heads.get("proj_a").and_then(|h| h.working_head.clone()),
        Some("rev_root".to_string())
    );
    assert_eq!(
        heads.get("proj_a").and_then(|h| h.official_head.clone()),
        Some("rev_root".to_string())
    );
}

// ---------------------------------------------------------------- timestamps

#[test]
fn one_commit_carries_one_record_time() {
    let mut records = base_records();
    records[1]["recorded_at"] = json!(T3);
    expect_replay_rejects(&records, "invalid_field");
}

#[test]
fn a_record_time_must_be_the_canonical_server_form() {
    let mut records = base_records();
    // Parseable, correct instant, wrong shape: the server never writes this.
    for r in records.iter_mut().filter(|r| r["seq"] == 1) {
        r["recorded_at"] = json!("2026-09-07T09:00:01+09:00");
    }
    expect_replay_rejects(&records, "invalid_field");

    let mut naive = base_records();
    for r in naive.iter_mut().filter(|r| r["seq"] == 1) {
        r["recorded_at"] = json!("2026-09-07T00:00:01");
    }
    expect_replay_rejects(&naive, "timezone_required");
}

// ---------------------------------------------------------------- head events

#[test]
fn a_head_change_must_start_from_the_head_that_existed() {
    let mut records = base_records();
    // Claims it moved from a root that was never the head.
    records[4] = head_change("hc_1", 2, T2, "proj_a", json!("rev_schema"), "rev_root");
    expect_replay_rejects(&records, "invalid_head_revision");
}

#[test]
fn a_second_head_change_must_follow_the_first() {
    let mut next_root = revision(
        "rev_root_2",
        3,
        T3,
        "proj_a",
        json!([{"slot_id": "s", "revision_id": "rev_schema", "roles": []}]),
    );
    next_root["data"]["change_kind"] = json!("composition");
    next_root["data"]["previous_revision_id"] = json!("rev_root");

    // A move that continues from the current head is accepted...
    let mut good = base_records();
    good.push(next_root.clone());
    good.push(head_change(
        "hc_2",
        3,
        T3,
        "proj_a",
        json!("rev_root"),
        "rev_root_2",
    ));
    replay(&good).expect("a head change that continues from the current head");

    // ...but one that pretends the project had no head is not.
    let mut bad = base_records();
    bad.push(next_root);
    bad.push(head_change(
        "hc_2",
        3,
        T3,
        "proj_a",
        Value::Null,
        "rev_root_2",
    ));
    expect_replay_rejects(&bad, "invalid_head_revision");
}

#[test]
fn one_commit_moves_a_head_at_most_once() {
    let mut records = base_records();
    records.push(head_change(
        "hc_dup",
        2,
        T2,
        "proj_a",
        json!("rev_root"),
        "rev_schema",
    ));
    expect_replay_rejects(&records, "invalid_field");
}

#[test]
fn head_events_must_carry_their_reason_actor_and_decision() {
    for field in ["reason", "actor"] {
        let mut records = base_records();
        records[4]["data"][field] = json!("   ");
        expect_replay_rejects(
            &records,
            if field == "reason" {
                "root_change_requires_decision"
            } else {
                "invalid_field"
            },
        );
    }
    let mut records = base_records();
    records[4]["data"]["decision"]["rationale"] = json!("");
    expect_replay_rejects(&records, "root_change_requires_decision");

    let mut unlabelled = base_records();
    unlabelled[5]["data"]["label"] = json!("");
    expect_replay_rejects(&unlabelled, "invalid_field");

    let mut naive = base_records();
    naive[5]["data"]["published_at"] = json!("2026-09-01T00:00:00");
    expect_replay_rejects(&naive, "timezone_required");
}

#[test]
fn a_head_must_point_at_a_revision_of_its_own_project() {
    let mut records = base_records();
    records.insert(1, project("proj_b", 1, T1));
    // hc_1 now sits at index 5 and claims proj_b moved to proj_a's root.
    let last = records.len() - 2;
    records[last] = head_change("hc_1", 2, T2, "proj_b", Value::Null, "rev_root");
    expect_replay_rejects(&records, "invalid_head_revision");
}

// ---------------------------------------------------------------- plan drift

#[test]
fn a_record_the_validator_would_refuse_is_refused_on_restore() {
    let mut records = base_records();
    // A project root may only contain schemas. Keep commit order: the new
    // records belong to commit 1, alongside the root they change.
    records.insert(3, entity("ent_idea", 1, T1, "proj_a", "idea"));
    records.insert(4, revision("rev_idea", 1, T1, "ent_idea", json!([])));
    let root = records
        .iter_mut()
        .find(|r| r["id"] == "rev_root")
        .expect("fixture has a root");
    root["data"]["slots"] = json!([{"slot_id": "s", "revision_id": "rev_idea", "roles": []}]);
    expect_replay_rejects(&records, "invalid_containment");
}

#[test]
fn a_composition_may_not_claim_a_predecessor_the_validator_rejects() {
    let mut records = base_records();
    records.push(revision("rev_root_2", 3, T3, "proj_a", json!([])));
    records[6]["data"]["change_kind"] = json!("composition");
    records[6]["data"]["previous_revision_id"] = json!("rev_schema");
    expect_replay_rejects(&records, "previous_revision_mismatch");
}

// ---------------------------------------------------------------- receipts

fn receipt(key: &str, seq: i64, written: Value, heads: Value, publication: Value) -> Value {
    json!({
        "idempotency_key": key,
        "request_digest": "sha256:aa",
        "seq": seq,
        "recorded_at": T2,
        "response": {"receipt": {
            "idempotency_key": key,
            "request_digest": "sha256:aa",
            "seq": seq,
            "replay": false,
            "records_written": written,
            "heads": heads,
            "publication_id": publication,
        }},
    })
}

fn good_receipt() -> Value {
    receipt(
        "pkg-1",
        1,
        json!(["proj_a", "ent_schema", "rev_schema", "rev_root"]),
        json!([]),
        Value::Null,
    )
}

#[test]
fn a_receipt_that_describes_the_restored_commit_is_accepted() {
    let records = base_records();
    let content = content(records.clone(), vec![good_receipt()]);
    let parsed = parse_records(&content).expect("records parse");
    check_receipts(&content, &parsed, 2).expect("a receipt matching its commit");
}

#[test]
fn a_receipt_cannot_claim_records_the_document_does_not_restore() {
    expect_receipts_reject(
        &base_records(),
        vec![receipt(
            "pkg-1",
            1,
            json!(["proj_a", "ghost_record"]),
            json!([]),
            Value::Null,
        )],
        "invalid_field",
    );
    // Nor can it under-claim: a package writes every record at its sequence.
    expect_receipts_reject(
        &base_records(),
        vec![receipt(
            "pkg-1",
            1,
            json!(["proj_a"]),
            json!([]),
            Value::Null,
        )],
        "invalid_field",
    );
}

#[test]
fn a_receipt_cannot_claim_a_head_move_no_event_backs() {
    expect_receipts_reject(
        &base_records(),
        vec![receipt(
            "pkg-2",
            2,
            json!(["hc_1", "pub_1"]),
            json!([{"project_id": "proj_a", "stage": "working", "revision_id": "rev_schema", "seq": 2}]),
            Value::Null,
        )],
        "invalid_field",
    );
}

#[test]
fn a_receipt_cannot_name_a_publication_its_commit_did_not_write() {
    expect_receipts_reject(
        &base_records(),
        vec![receipt(
            "pkg-2",
            2,
            json!(["hc_1", "pub_1"]),
            json!([]),
            json!("pub_ghost"),
        )],
        "invalid_field",
    );
}

#[test]
fn a_receipt_body_must_agree_with_its_own_envelope() {
    let mut forged = good_receipt();
    forged["response"]["receipt"]["request_digest"] = json!("sha256:bb");
    expect_receipts_reject(&base_records(), vec![forged], "invalid_field");

    let mut renamed = good_receipt();
    renamed["response"]["receipt"]["idempotency_key"] = json!("pkg-other");
    expect_receipts_reject(&base_records(), vec![renamed], "invalid_field");

    let mut resequenced = good_receipt();
    resequenced["response"]["receipt"]["seq"] = json!(2);
    expect_receipts_reject(&base_records(), vec![resequenced], "invalid_field");
}

#[test]
fn a_receipt_for_an_empty_commit_is_refused() {
    expect_receipts_reject(
        &base_records(),
        vec![
            receipt("pkg-9", 2, json!(["hc_1", "pub_1"]), json!([]), Value::Null)
                .as_object_mut()
                .map(|m| {
                    m.insert("seq".into(), json!(9));
                    m["response"]["receipt"]["seq"] = json!(9);
                    Value::Object(m.clone())
                })
                .unwrap(),
        ],
        "invalid_field",
    );
}

// ---------------------------------------------------------------- scoping

#[test]
fn scoped_exports_drop_receipts_that_reach_outside_the_closure() {
    let closure: BTreeSet<String> = ["proj_a", "ent_schema", "rev_schema", "rev_root"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    assert!(receipt_fits_scope(&good_receipt(), &closure));

    let outside = receipt(
        "pkg-x",
        1,
        json!(["proj_a", "rev_other_project"]),
        json!([]),
        Value::Null,
    );
    assert!(
        !receipt_fits_scope(&outside, &closure),
        "a receipt naming another project's record must not ride along"
    );

    let head_outside = receipt(
        "pkg-y",
        1,
        json!(["proj_a"]),
        json!([{"project_id": "proj_b", "stage": "working", "revision_id": "rev_root", "seq": 1}]),
        Value::Null,
    );
    assert!(!receipt_fits_scope(&head_outside, &closure));

    let publication_outside = receipt(
        "pkg-z",
        1,
        json!(["proj_a"]),
        json!([]),
        json!("pub_elsewhere"),
    );
    assert!(!receipt_fits_scope(&publication_outside, &closure));

    assert!(
        !receipt_fits_scope(&json!({"idempotency_key": "pkg-w"}), &closure),
        "a receipt with no stored response cannot be shown to fit"
    );
}

#[test]
fn a_project_cannot_restore_a_schema_as_its_head() {
    let mut records = base_records();
    records[4]["data"]["after_revision_id"] = json!("rev_schema");
    expect_replay_rejects(&records, "invalid_head_revision");
}

#[test]
fn live_compatible_empty_before_after_with_rationale_can_restore() {
    let mut records = base_records();
    records[4]["data"]["decision"]["before"] = json!("");
    records[4]["data"]["decision"]["after"] = json!("");
    replay(&records).expect("live writes require rationale, but allow empty before and after text");
}
