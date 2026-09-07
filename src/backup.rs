//! Versioned export and import.
//!
//! Import is the strict direction: the document is only a proposal. Its digest
//! is recomputed, every record is re-validated by the same validator that
//! guards live writes, and the heads are re-derived from the record events
//! instead of being copied out of the document. A backup can therefore restore
//! state, but it cannot introduce state the running server would have refused.

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult, Issue};
use crate::limits;
use crate::model::{NewRecord, RecordData, RecordKind, Stage, StoredRecord};
use crate::neo4j::Neo4j;
use crate::store;
use crate::util;
use crate::validate::{self, Context, HeadState, Package};

pub const FORMAT: &str = "idea_db.export";
pub const FORMAT_VERSION: u32 = 1;

// ---------------------------------------------------------------- export

pub async fn export(
    neo: &Neo4j,
    project_id: Option<String>,
    include_receipts: bool,
) -> ApiResult<Value> {
    let (ctx, receipts) = store::read_context_with_receipts(neo).await?;

    let selected = match &project_id {
        None => ctx.records.keys().cloned().collect::<BTreeSet<String>>(),
        Some(p) => project_closure(&ctx, p)?,
    };
    if selected.len() > limits::MAX_EXPORT_RECORDS {
        return Err(ApiError::validation(vec![Issue::new(
            "project_id",
            "export_scope_unsupported",
            format!(
                "the reference closure holds {} records, above the {} export bound",
                selected.len(),
                limits::MAX_EXPORT_RECORDS
            ),
        )]));
    }

    let mut records: Vec<&StoredRecord> = selected
        .iter()
        .filter_map(|id| ctx.records.get(id))
        .collect();
    records.sort_by(|a, b| (a.seq, &a.id).cmp(&(b.seq, &b.id)));

    let heads: Vec<Value> = ctx
        .heads
        .iter()
        .filter(|(id, _)| selected.contains(*id))
        .filter(|(id, _)| project_id.as_deref().is_none_or(|p| p == id.as_str()))
        .map(|(id, h)| head_json(id, h))
        .collect();

    let total_receipts = receipts.len();
    let receipt_rows: Vec<Value> = if include_receipts {
        receipts
            .into_iter()
            .filter(|r| receipt_fits_scope(r, &selected))
            .collect()
    } else {
        Vec::new()
    };
    let omitted_receipts = if include_receipts {
        total_receipts - receipt_rows.len()
    } else {
        total_receipts
    };

    // Every external artifact is unresolved in this release: no bytes are
    // stored, so no export can honestly claim to be a complete backup.
    let external: Vec<&StoredRecord> = records
        .iter()
        .copied()
        .filter(|r| r.kind() == RecordKind::Artifact)
        .collect();
    let manifest = json!({
        "external_artifacts": external.iter().map(|r| {
            let RecordData::Artifact(a) = &r.data else { unreachable!("filtered to artifacts") };
            json!({
                "artifact_id": r.id,
                "uri": a.uri,
                "digest": a.digest,
                "included_in_export": false,
            })
        }).collect::<Vec<_>>(),
        "unresolved_external_artifacts": external.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        "complete_backup": external.is_empty() && omitted_receipts == 0,
        "receipts_included": receipt_rows.len(),
        "receipts_omitted": omitted_receipts,
    });

    let content = json!({
        "seq": ctx.seq,
        "records": records.iter().map(|r| r.to_json()).collect::<Vec<_>>(),
        "heads": heads,
        "receipts": receipt_rows,
        "manifest": manifest,
    });

    Ok(json!({
        "format": FORMAT,
        "format_version": FORMAT_VERSION,
        "protocol_version": crate::model::PROTOCOL_VERSION,
        "exported_at": util::now_utc_millis(),
        "scope": { "project_id": project_id, "closure_included": true },
        "digest_alg": "sha256",
        "digest": util::digest_json(&content),
        "content": content,
    }))
}

/// A receipt belongs in a scoped export only when everything it describes is in
/// the export. A receipt whose `records_written` reach outside the closure would
/// leak unrelated ids and, on restore, promise a replay for records the document
/// does not contain.
fn receipt_fits_scope(row: &Value, selected: &BTreeSet<String>) -> bool {
    let Some(receipt) = row.get("response").and_then(|r| r.get("receipt")) else {
        return false;
    };
    let written_ok = receipt
        .get("records_written")
        .and_then(Value::as_array)
        .is_some_and(|ids| {
            !ids.is_empty()
                && ids
                    .iter()
                    .all(|v| v.as_str().is_some_and(|id| selected.contains(id)))
        });
    let heads_ok = receipt
        .get("heads")
        .and_then(Value::as_array)
        .map(|hs| {
            hs.iter().all(|h| {
                let project = h
                    .get("project_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let revision = h
                    .get("revision_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                selected.contains(project) && selected.contains(revision)
            })
        })
        .unwrap_or(true);
    let publication_ok = match receipt.get("publication_id").and_then(Value::as_str) {
        Some(id) => selected.contains(id),
        None => true,
    };
    written_ok && heads_ok && publication_ok
}

fn head_json(project_id: &str, head: &HeadState) -> Value {
    json!({
        "project_id": project_id,
        "working_head": head.working_head,
        "working_head_seq": head.working_head_seq,
        "official_head": head.official_head,
        "official_publication_id": head.official_publication_id,
    })
}

/// Records belonging to a project, plus everything they point at.
///
/// Cross-project reuse means a plain filter would export a graph with dangling
/// references, so the closure pulls in reused Core/Idea records and the origin
/// `project` records that give them their attribution.
fn project_closure(ctx: &Context, project_id: &str) -> ApiResult<BTreeSet<String>> {
    if !ctx
        .records
        .get(project_id)
        .is_some_and(|r| r.kind() == RecordKind::Project)
    {
        return Err(ApiError::not_found(format!(
            "project {project_id} not found"
        )));
    }
    let mut selected: BTreeSet<String> = BTreeSet::new();
    selected.insert(project_id.to_string());
    for rec in ctx.records.values() {
        if owns(rec, project_id) {
            selected.insert(rec.id.clone());
        }
    }
    loop {
        let mut added = false;
        // Forward references: composition children, captures, artifacts,
        // reused entities from other projects and their origin projects.
        let forward: Vec<String> = selected
            .iter()
            .filter_map(|id| ctx.records.get(id))
            .flat_map(|r| r.refs())
            .collect();
        for id in forward {
            if ctx.records.contains_key(&id) && selected.insert(id) {
                added = true;
            }
        }
        // Records that hang off a selected record rather than pointing at the
        // project themselves.
        for rec in ctx.records.values() {
            if selected.contains(&rec.id) {
                continue;
            }
            let attach = match &rec.data {
                // Every revision of a selected identity belongs to the export,
                // including superseded ones that nothing points at any more.
                RecordData::Revision(d) => selected.contains(&d.entity_id),
                RecordData::Baseline(d) => selected.contains(&d.goal_id),
                RecordData::Assessment(d) => selected.contains(&d.baseline_id),
                RecordData::Embedding(d) => selected.contains(&d.revision_id),
                RecordData::Promotion(d) => selected.contains(&d.candidate_id),
                RecordData::Link(d) => selected.contains(&d.from_id) && selected.contains(&d.to_id),
                _ => false,
            };
            if attach {
                selected.insert(rec.id.clone());
                added = true;
            }
        }
        if !added {
            return Ok(selected);
        }
    }
}

/// Direct project ownership, before any closure expansion.
fn owns(rec: &StoredRecord, project_id: &str) -> bool {
    match &rec.data {
        RecordData::Entity(d) => d.project_id == project_id,
        RecordData::Candidate(d) => d.project_id == project_id,
        RecordData::Observation(d) => d.project_id == project_id,
        RecordData::Goal(d) => d.project_id == project_id,
        RecordData::HeadChange(d) => d.project_id == project_id,
        RecordData::Publication(d) => d.project_id == project_id,
        RecordData::Capture(d) => d.project_id.as_deref() == Some(project_id),
        _ => false,
    }
}

// ---------------------------------------------------------------- import

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportRequest {
    document: Value,
}

pub async fn import(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let req: ImportRequest =
        serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let doc = req.document;

    if doc.get("format").and_then(Value::as_str) != Some(FORMAT) {
        return Err(ApiError::bad_request("document is not an idea_db export"));
    }
    if doc.get("format_version").and_then(Value::as_u64) != Some(FORMAT_VERSION as u64) {
        return Err(ApiError::bad_request(format!(
            "unsupported export format_version, expected {FORMAT_VERSION}"
        )));
    }
    let content = doc
        .get("content")
        .ok_or_else(|| ApiError::bad_request("document has no content"))?;
    let declared = doc
        .get("digest")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("document has no digest"))?;
    let actual = util::digest_json(content);
    if declared != actual {
        return Err(ApiError::digest_mismatch(declared, &actual));
    }

    let records = parse_records(content)?;
    if records.len() > limits::MAX_EXPORT_RECORDS {
        return Err(ApiError::context_too_large(
            records.len(),
            limits::MAX_EXPORT_RECORDS,
        ));
    }
    let final_seq = content
        .get("seq")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::bad_request("document content has no seq"))?;

    // Re-validate the whole document before touching the database.
    let rebuilt = revalidate(&records, final_seq)?;
    check_declared_heads(content, &rebuilt)?;
    let receipts = check_receipts(content, &records, final_seq)?;

    let mut tx = neo.begin().await?;
    let outcome = async {
        store::lock(&mut tx).await?;
        if store::count_records(&mut tx).await? > 0 {
            return Err(ApiError::import_not_empty());
        }
        let statements = store::import_statements(&records, final_seq, &receipts);
        // Write and check inside the transaction. Verifying after commit could
        // only report a half-restored graph, never undo one.
        let results = tx.run(&statements).await?;
        store::verify_relationship_counts(&records, &results)
    }
    .await;
    match outcome {
        Ok(()) => {
            tx.commit(&[]).await?;
        }
        Err(e) => {
            tx.rollback().await;
            return Err(e);
        }
    }

    let external: Vec<String> = records
        .iter()
        .filter(|r| r.kind() == RecordKind::Artifact)
        .map(|r| r.id.clone())
        .collect();
    Ok(json!({
        "imported": {
            "records": records.len(),
            "heads": rebuilt.len(),
            "receipts": receipts.len(),
            "seq": final_seq,
        },
        "digest": actual,
        "complete_backup": external.is_empty()
            && content["manifest"]["complete_backup"].as_bool().unwrap_or(false),
        "unresolved_external_artifacts": external,
    }))
}

fn parse_records(content: &Value) -> ApiResult<Vec<StoredRecord>> {
    let rows = content
        .get("records")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("document content has no records array"))?;
    let mut out = Vec::with_capacity(rows.len());
    let mut last_seq = 0i64;
    for (i, row) in rows.iter().enumerate() {
        let path = format!("content.records[{i}]");
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request(format!("{path}.id is missing")))?;
        let kind_text = row
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request(format!("{path}.kind is missing")))?;
        let kind = RecordKind::parse_str(kind_text).ok_or_else(|| {
            ApiError::bad_request(format!("{path}.kind '{kind_text}' is unknown"))
        })?;
        let seq = row
            .get("seq")
            .and_then(Value::as_i64)
            .ok_or_else(|| ApiError::bad_request(format!("{path}.seq is missing")))?;
        if seq < last_seq {
            return Err(ApiError::validation(vec![Issue::new(
                path,
                "invalid_field",
                "records are not in commit order",
            )]));
        }
        last_seq = seq;
        let recorded_at = row
            .get("recorded_at")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request(format!("{path}.recorded_at is missing")))?;
        let data = RecordData::parse(kind, row.get("data").cloned().unwrap_or(Value::Null))
            .map_err(|e| ApiError::bad_request(format!("{path}.data: {e}")))?;
        out.push(StoredRecord {
            id: id.to_string(),
            seq,
            recorded_at: recorded_at.to_string(),
            data,
        });
    }
    Ok(out)
}

fn issue(path: impl Into<String>, code: &'static str, message: impl Into<String>) -> ApiError {
    ApiError::validation(vec![Issue::new(path, code, message)])
}

/// A commit's server timestamp, which every record in that commit shares.
///
/// The live path stamps one `recorded_at` per commit and writes it in canonical
/// UTC millisecond form. A document whose records disagree inside one `seq`, or
/// whose timestamp is merely parseable rather than canonical, did not come from
/// a commit this server would have made.
fn group_recorded_at(seq: i64, group: &[&StoredRecord]) -> ApiResult<String> {
    let first = group.first().ok_or_else(|| {
        issue(
            format!("content.records[seq={seq}]"),
            "invalid_field",
            "empty commit",
        )
    })?;
    let stamp = first.recorded_at.clone();
    let canonical = util::parse_rfc3339(&stamp)
        .map(util::to_utc_key)
        .ok_or_else(|| {
            issue(
                format!("content.records[{}].recorded_at", first.id),
                "timezone_required",
                format!("'{stamp}' is not RFC 3339 with an explicit offset"),
            )
        })?;
    if canonical != stamp {
        return Err(issue(
            format!("content.records[{}].recorded_at", first.id),
            "invalid_field",
            format!("'{stamp}' is not the canonical server form '{canonical}'"),
        ));
    }
    for rec in group.iter().skip(1) {
        if rec.recorded_at != stamp {
            return Err(issue(
                format!("content.records[{}].recorded_at", rec.id),
                "invalid_field",
                format!(
                    "commit {seq} carries two record times, '{stamp}' and '{}'",
                    rec.recorded_at
                ),
            ));
        }
    }
    Ok(stamp)
}

/// Running head state during a replay, used to check that each head event was
/// a legal move from the head that existed when it was recorded.
#[derive(Default)]
struct Replay {
    heads: BTreeMap<String, HeadState>,
    /// One head event per project, stage and commit.
    events: BTreeSet<(String, &'static str, i64)>,
}

/// Replay the document one commit at a time through the live validator.
///
/// Each `seq` group is submitted as the package it once was, so imports are
/// held to exactly the invariants a live write is held to, and the plan the
/// validator produces is compared against the document byte for byte. Head
/// events are checked separately because a package may not submit them
/// directly, but they are held to the same rules the live head path applies.
fn revalidate(records: &[StoredRecord], final_seq: i64) -> ApiResult<BTreeMap<String, HeadState>> {
    let mut ctx = Context::default();
    let mut replay = Replay::default();
    let mut groups: BTreeMap<i64, Vec<&StoredRecord>> = BTreeMap::new();
    for rec in records {
        if rec.seq < 1 || rec.seq > final_seq {
            return Err(issue(
                format!("content.records[{}]", rec.id),
                "invalid_field",
                format!("seq {} is outside 1..{final_seq}", rec.seq),
            ));
        }
        groups.entry(rec.seq).or_default().push(rec);
    }

    for (seq, group) in groups {
        let recorded_at = group_recorded_at(seq, &group)?;
        let mut client_records = Vec::new();
        let mut head_events = Vec::new();
        for rec in &group {
            if rec.kind().is_server_only() {
                head_events.push(*rec);
            } else {
                client_records.push(NewRecord {
                    id: rec.id.clone(),
                    kind: rec.kind(),
                    data: rec.data_value(),
                });
            }
        }
        let expected_ids: BTreeSet<String> = client_records.iter().map(|r| r.id.clone()).collect();
        let pkg = Package {
            protocol_version: crate::model::PROTOCOL_VERSION,
            idempotency_key: format!("import:{seq}"),
            actor: "import".to_string(),
            reason: "restore".to_string(),
            expected_heads: Vec::new(),
            records: client_records,
            root_change: None,
            publish: None,
        };
        ctx.seq = seq - 1;
        let plan = validate::validate_package(&pkg, &ctx, seq, &recorded_at)
            .map_err(ApiError::validation)?;

        // The plan is the normalised truth. Anything the document says that the
        // validator would not have produced is a difference, not a detail.
        let planned_ids: BTreeSet<String> = plan.records.iter().map(|r| r.id.clone()).collect();
        if planned_ids != expected_ids {
            return Err(issue(
                format!("content.records[seq={seq}]"),
                "invalid_field",
                "the validated plan does not match the records this commit claims",
            ));
        }
        for planned in &plan.records {
            let original = group
                .iter()
                .find(|r| r.id == planned.id)
                .expect("planned ids were just checked against the group");
            if util::canonical_json(&planned.data_value())
                != util::canonical_json(&original.data_value())
            {
                return Err(issue(
                    format!("content.records[{}].data", planned.id),
                    "invalid_field",
                    "record payload differs from the validated plan",
                ));
            }
            if planned.seq != original.seq || planned.recorded_at != original.recorded_at {
                return Err(issue(
                    format!("content.records[{}]", planned.id),
                    "invalid_field",
                    "record stamp differs from the validated plan",
                ));
            }
            insert(&mut ctx, (*original).clone());
        }
        // A `composition` revision that omits its predecessor is resolved by the
        // validator; the document must not have recorded a different one.
        for (id, inferred) in &plan.resolved_previous {
            let stored_previous = ctx
                .records
                .get(id)
                .and_then(|r| r.as_revision())
                .and_then(|r| r.previous_revision_id.clone());
            if stored_previous.is_some_and(|p| &p != inferred) {
                return Err(issue(
                    format!("content.records[{id}].data.previous_revision_id"),
                    "previous_revision_mismatch",
                    format!("the validator resolves this predecessor to {inferred}"),
                ));
            }
        }
        for rec in head_events {
            check_head_event(&ctx, &mut replay, rec)?;
            insert(&mut ctx, rec.clone());
        }
    }

    if ctx.records.len() != records.len() {
        return Err(issue(
            "content.records",
            "duplicate_id",
            "the document repeats an application id",
        ));
    }
    Ok(store::derive_heads(&ctx.records))
}

fn insert(ctx: &mut Context, rec: StoredRecord) {
    if let Some(rev) = rec.as_revision() {
        let slot = ctx
            .entity_revisions
            .entry(rev.entity_id.clone())
            .or_default();
        slot.push((rec.seq, rec.id.clone()));
        slot.sort();
    }
    if let RecordData::Promotion(p) = &rec.data {
        ctx.promoted_candidates.insert(p.candidate_id.clone());
    }
    ctx.seq = ctx.seq.max(rec.seq);
    ctx.records.insert(rec.id.clone(), rec);
}

/// Head events carry the authority to move a head, so they get the same checks
/// the live path applies before it writes one, plus the continuity checks a
/// live sequence gets for free: a head change must start from the head that
/// actually existed, and one commit moves a given head at most once.
fn check_head_event(ctx: &Context, replay: &mut Replay, rec: &StoredRecord) -> ApiResult<()> {
    let path = format!("content.records[{}]", rec.id);
    let fail = |code: &'static str, message: String| issue(path.clone(), code, message);

    let (project_id, revision_id, stage) = match &rec.data {
        RecordData::HeadChange(d) => {
            if d.stage != Stage::Working {
                return Err(fail(
                    "invalid_field",
                    "head_change records the working stage only".to_string(),
                ));
            }
            if !d.meaningful || d.reason.trim().is_empty() || d.decision.rationale.trim().is_empty()
            {
                return Err(fail(
                    "root_change_requires_decision",
                    "an imported head change must carry its reason and before/after decision"
                        .to_string(),
                ));
            }
            if d.actor.trim().is_empty() {
                return Err(fail(
                    "invalid_field",
                    "an imported head change must name its actor".to_string(),
                ));
            }
            // The move must start where the head actually was at this point in
            // the replay. Otherwise a document could graft an unrelated history.
            let current = replay
                .heads
                .get(&d.project_id)
                .and_then(|h| h.working_head.clone());
            if current != d.before_revision_id {
                return Err(fail(
                    "invalid_head_revision",
                    format!(
                        "claims it moved from {:?} but the working head was {:?} at seq {}",
                        d.before_revision_id, current, rec.seq
                    ),
                ));
            }
            (&d.project_id, &d.after_revision_id, Stage::Working)
        }
        RecordData::Publication(d) => {
            let Some(published) = util::parse_rfc3339(&d.published_at) else {
                return Err(fail(
                    "timezone_required",
                    format!("published_at '{}' has no explicit offset", d.published_at),
                ));
            };
            if d.label.trim().is_empty() || d.actor.trim().is_empty() {
                return Err(fail(
                    "invalid_field",
                    "an imported publication must carry a label and an actor".to_string(),
                ));
            }
            let _ = published;
            (&d.project_id, &d.root_revision_id, Stage::Official)
        }
        _ => return Ok(()),
    };

    if !replay
        .events
        .insert((project_id.clone(), stage.as_str(), rec.seq))
    {
        return Err(fail(
            "invalid_field",
            format!(
                "commit {} moves the {} head of {project_id} more than once",
                rec.seq,
                stage.as_str()
            ),
        ));
    }
    if !ctx
        .records
        .get(project_id)
        .is_some_and(|r| r.kind() == RecordKind::Project)
    {
        return Err(fail(
            "unknown_reference",
            format!("project {project_id} not found"),
        ));
    }
    let revision = ctx
        .records
        .get(revision_id)
        .filter(|r| r.kind() == RecordKind::Revision)
        .ok_or_else(|| {
            fail(
                "unknown_reference",
                format!("revision {revision_id} not found"),
            )
        })?;
    if revision
        .as_revision()
        .is_none_or(|data| &data.entity_id != project_id)
    {
        return Err(fail(
            "invalid_head_revision",
            format!("{revision_id} must be a revision of Project {project_id} itself"),
        ));
    }

    if stage == Stage::Working {
        let entry = replay.heads.entry(project_id.clone()).or_default();
        entry.working_head = Some(revision_id.clone());
        entry.working_head_seq = rec.seq;
    }
    Ok(())
}

/// The document may state heads, but the records decide them.
fn check_declared_heads(content: &Value, derived: &BTreeMap<String, HeadState>) -> ApiResult<()> {
    let Some(rows) = content.get("heads").and_then(Value::as_array) else {
        return Ok(());
    };
    for (i, row) in rows.iter().enumerate() {
        let path = format!("content.heads[{i}]");
        let project_id = row
            .get("project_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(actual) = derived.get(project_id) else {
            return Err(ApiError::validation(vec![Issue::new(
                path,
                "unknown_reference",
                format!("project {project_id} is not in the document"),
            )]));
        };
        let claimed_working = row.get("working_head").and_then(Value::as_str);
        let claimed_official = row.get("official_head").and_then(Value::as_str);
        if claimed_working != actual.working_head.as_deref()
            || claimed_official != actual.official_head.as_deref()
        {
            return Err(ApiError::validation(vec![Issue::new(
                path,
                "invalid_head_revision",
                format!(
                    "declared heads ({claimed_working:?}, {claimed_official:?}) do not match the \
                     head events in the document ({:?}, {:?})",
                    actual.working_head, actual.official_head
                ),
            )]));
        }
    }
    Ok(())
}

/// Check every receipt against the records that were actually restored.
///
/// **What this can and cannot prove.** A receipt stores `request_digest`, a hash
/// of a request body the document does not carry, so nothing here can show the
/// digest belongs to a real request; a forged pair of digest and response stays
/// internally consistent. What is checked is that the receipt describes the
/// restored graph: its key, digest and sequence agree with the response it
/// carries, and the records, heads and publication that response claims are
/// exactly the ones at that commit. A receipt that would replay a success the
/// database cannot show is therefore rejected, which is the property that
/// matters — a replay hands back a receipt for work that really happened.
fn check_receipts(
    content: &Value,
    records: &[StoredRecord],
    final_seq: i64,
) -> ApiResult<Vec<Value>> {
    let Some(rows) = content.get("receipts").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut by_seq: BTreeMap<i64, BTreeSet<&str>> = BTreeMap::new();
    for rec in records {
        by_seq.entry(rec.seq).or_default().insert(rec.id.as_str());
    }
    let mut seen: BTreeSet<&str> = BTreeSet::new();

    for (i, row) in rows.iter().enumerate() {
        let path = format!("content.receipts[{i}]");
        let bad = |message: String| issue(path.clone(), "invalid_field", message);

        let key = row
            .get("idempotency_key")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request(format!("{path}.idempotency_key is missing")))?;
        if !seen.insert(key) {
            return Err(issue(
                path.clone(),
                "duplicate_id",
                format!("receipt key {key} appears twice"),
            ));
        }
        let digest = row
            .get("request_digest")
            .and_then(Value::as_str)
            .ok_or_else(|| ApiError::bad_request(format!("{path}.request_digest is missing")))?;
        let seq = row.get("seq").and_then(Value::as_i64).unwrap_or(-1);
        if seq < 1 || seq > final_seq {
            return Err(bad(format!("receipt seq {seq} is outside 1..{final_seq}")));
        }

        let receipt = row
            .get("response")
            .and_then(|r| r.get("receipt"))
            .ok_or_else(|| bad("stored response carries no receipt".to_string()))?;
        if receipt.get("idempotency_key").and_then(Value::as_str) != Some(key) {
            return Err(bad(
                "stored response names a different idempotency key".into()
            ));
        }
        if receipt.get("request_digest").and_then(Value::as_str) != Some(digest) {
            return Err(bad(
                "stored response carries a different request digest".into()
            ));
        }
        if receipt.get("seq").and_then(Value::as_i64) != Some(seq) {
            return Err(bad(
                "stored response was recorded at a different sequence".into()
            ));
        }

        // A receipt exists only for a package commit, and a package commit
        // writes every record at its sequence.
        let Some(actual) = by_seq.get(&seq) else {
            return Err(bad(format!("no restored record belongs to commit {seq}")));
        };
        let claimed: BTreeSet<&str> = receipt
            .get("records_written")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        if &claimed != actual {
            return Err(bad(format!(
                "claims {} records for commit {seq} but the document restores {}",
                claimed.len(),
                actual.len()
            )));
        }

        for head in receipt
            .get("heads")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let project_id = head
                .get("project_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let stage = head
                .get("stage")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let revision_id = head
                .get("revision_id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let backed = records.iter().any(|r| match &r.data {
                RecordData::HeadChange(d) => {
                    r.seq == seq
                        && stage == "working"
                        && d.project_id == project_id
                        && d.after_revision_id == revision_id
                }
                RecordData::Publication(d) => {
                    r.seq == seq
                        && stage == "official"
                        && d.project_id == project_id
                        && d.root_revision_id == revision_id
                }
                _ => false,
            });
            if !backed {
                return Err(bad(format!(
                    "claims the {stage} head of {project_id} moved to {revision_id}, \
                     but no head event at commit {seq} says so"
                )));
            }
        }

        if let Some(publication_id) = receipt.get("publication_id").and_then(Value::as_str) {
            let backed = records.iter().any(|r| {
                r.id == publication_id
                    && r.seq == seq
                    && matches!(r.data, RecordData::Publication(_))
            });
            if !backed {
                return Err(bad(format!(
                    "names publication {publication_id}, which commit {seq} did not write"
                )));
            }
        }
    }
    Ok(rows.clone())
}

#[cfg(test)]
mod tests;
