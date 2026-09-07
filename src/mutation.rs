//! Write paths: package validate/apply, occurrence replace and capture intake.
//!
//! All of them follow the same order, which is what makes concurrent editing
//! safe: open a transaction, take the database write lock, read the state we
//! are about to compare against, check idempotency *before* the head check,
//! validate in Rust, then write and commit — or roll the whole thing back.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::Deserialize;
use serde_json::{json, Value};

use crate::error::{ApiError, ApiResult, Issue};
use crate::graph::{self, Limits, PathError};
use crate::limits;
use crate::model::{
    CaptureData, ChangeKind, Decision, NewRecord, RecordKind, SourceKind, Stage, StoredRecord,
    PROTOCOL_VERSION,
};
use crate::neo4j::Neo4j;
use crate::store;
use crate::util;
use crate::validate::{self, Context, ExpectedHead, Package, Plan, RootChange};

/// Distinguishes captures created in the same millisecond with identical text.
/// A capture is an event, so two identical pastes are two records.
static CAPTURE_NONCE: AtomicU64 = AtomicU64::new(0);

fn parse_body<T: for<'de> Deserialize<'de>>(body: Value) -> ApiResult<T> {
    serde_json::from_value(body).map_err(|e| ApiError::bad_request(e.to_string()))
}

fn check_protocol(version: u32) -> ApiResult<()> {
    if version != PROTOCOL_VERSION {
        return Err(ApiError::unsupported_protocol_version(version));
    }
    Ok(())
}

pub(crate) fn check_not_noop(pkg: &Package) -> ApiResult<()> {
    if pkg.records.is_empty() && pkg.root_change.is_none() && pkg.publish.is_none() {
        Err(ApiError::validation(vec![Issue::new(
            "records",
            "empty_commit",
            "a package must write a record, move the working head, or publish a root",
        )]))
    } else {
        Ok(())
    }
}

/// Refuse a commit that would push the namespace past the size every read is
/// bounded by.
///
/// `store::load_records` caps a consistent read at `MAX_PREFETCH_RECORDS`, so a
/// write that crosses that line would leave a database nothing can read back —
/// no snapshot, no search, not even an export to recover from. The check runs
/// under the transaction lock against the same context the write validated
/// against, and counts the whole plan, including the `head_change` and
/// `publication` records the server adds itself.
fn check_capacity(existing: usize, incoming: usize) -> ApiResult<()> {
    let total = existing + incoming;
    if total > limits::MAX_PREFETCH_RECORDS {
        return Err(ApiError::validation(vec![Issue::new(
            "records",
            "limit_exceeded",
            format!(
                "this commit would store {total} records ({existing} existing + {incoming} new), \
                 past the {} the namespace can be read back at",
                limits::MAX_PREFETCH_RECORDS
            ),
        )]));
    }
    Ok(())
}

/// Compare-and-set against the heads read under the lock.
fn check_heads(expected: &[ExpectedHead], ctx: &Context) -> ApiResult<()> {
    for want in expected {
        let head = ctx.heads.get(&want.project_id);
        let actual = match want.stage {
            Stage::Working => head.and_then(|h| h.working_head.clone()),
            Stage::Official => head.and_then(|h| h.official_head.clone()),
        };
        if actual != want.revision_id {
            return Err(ApiError::head_conflict(json!({
                "project_id": want.project_id,
                "stage": want.stage.as_str(),
                "expected": want.revision_id,
                "actual": actual,
            })));
        }
    }
    Ok(())
}

fn head_json(plan: &Plan, seq: i64) -> Value {
    json!(plan
        .head_updates
        .iter()
        .map(|h| json!({
            "project_id": h.project_id,
            "stage": h.stage.as_str(),
            "revision_id": h.revision_id,
            "seq": seq,
        }))
        .collect::<Vec<_>>())
}

fn receipt_json(
    pkg: &Package,
    plan: &Plan,
    seq: i64,
    applied_at: &str,
    request_digest: &str,
) -> Value {
    json!({
        "idempotency_key": pkg.idempotency_key,
        "request_digest": request_digest,
        "applied_at": applied_at,
        "seq": seq,
        "replay": false,
        "records_written": plan.records.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
        "heads": head_json(plan, seq),
        "publication_id": plan.publication_id,
        "resolved_previous": plan.resolved_previous,
        "warnings": plan.warnings,
    })
}

/// Mark a stored response as a replay without changing anything else about it.
fn as_replay(mut stored: Value) -> Value {
    if let Some(receipt) = stored.get_mut("receipt") {
        receipt["replay"] = json!(true);
    }
    stored
}

// ---------------------------------------------------------------- packages

pub async fn validate_only(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let pkg: Package = parse_body(body)?;
    check_protocol(pkg.protocol_version)?;

    // Reads take the same lock and roll back, so the dry run sees exactly the
    // state a real apply would see.
    let ctx = store::read_context(neo).await?;
    if let Err(error) = check_not_noop(&pkg) {
        return Ok(
            json!({"valid":false,"errors":error.details.as_ref().map(|d|d["issues"].clone()).unwrap_or(json!([])),"warnings":[],"derived":{"records_to_write":0,"next_seq":ctx.seq+1,"context_records_loaded":ctx.records.len()}}),
        );
    }
    let next_seq = ctx.seq + 1;
    let head_issue = check_heads(&pkg.expected_heads, &ctx).err();

    Ok(
        match validate::validate_package(&pkg, &ctx, next_seq, &util::now_utc_millis()) {
            Ok(plan) => {
                let capacity_issue = check_capacity(ctx.records.len(), plan.records.len()).err();
                let errors: Vec<Value> = head_issue
                    .iter()
                    .map(|e| {
                        json!({
                            "path": "expected_heads", "code": e.code, "message": e.message
                        })
                    })
                    .chain(capacity_issue.iter().map(|e| {
                        json!({
                            "path": "records", "code": e.code, "message": e.message
                        })
                    }))
                    .collect();
                json!({
                    "valid": errors.is_empty(),
                    "errors": errors,
                    "warnings": plan.warnings,
                    "derived": {
                        "records_to_write": plan.records.len(),
                        "next_seq": next_seq,
                        "context_records_loaded": ctx.records.len(),
                        "namespace_capacity": limits::MAX_PREFETCH_RECORDS,
                        "resolved_previous": plan.resolved_previous,
                        "head_updates": head_json(&plan, next_seq),
                    },
                })
            }
            Err(issues) => json!({
                "valid": false,
                "errors": issues,
                "warnings": [],
                "derived": {
                    "records_to_write": 0,
                    "next_seq": next_seq,
                    "context_records_loaded": ctx.records.len(),
                },
            }),
        },
    )
}

pub async fn apply(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let request_digest = util::digest_json(&body);
    let pkg: Package = parse_body(body)?;
    check_protocol(pkg.protocol_version)?;
    commit_package(neo, pkg, &request_digest, None, |_, _| Ok(json!({}))).await
}

/// Apply a package only while its exact prepared upload remains live.
pub async fn apply_staged(neo: &Neo4j, body: Value, upload_id: &str) -> ApiResult<Value> {
    let request_digest = util::digest_json(&body);
    let pkg: Package = parse_body(body)?;
    check_protocol(pkg.protocol_version)?;
    commit_package(neo, pkg, &request_digest, Some(upload_id), |_, _| {
        Ok(json!({}))
    })
    .await
}

/// The one place that turns a validated package into committed records.
///
/// `decorate` adds endpoint-specific fields (occurrence mapping, for instance)
/// to the response *before* it is stored in the receipt, so a replay returns
/// exactly what the first call returned.
async fn commit_package<F>(
    neo: &Neo4j,
    pkg: Package,
    request_digest: &str,
    staged_upload_id: Option<&str>,
    decorate: F,
) -> ApiResult<Value>
where
    F: FnOnce(&Plan, &Context) -> ApiResult<Value>,
{
    let mut tx = neo.begin().await?;
    let outcome = commit_inner(&mut tx, &pkg, request_digest, staged_upload_id, decorate).await;
    match outcome {
        // The writes already ran and were checked inside the transaction, so
        // committing has nothing left to add.
        Ok(Committed::Written(response)) => {
            tx.commit(&[]).await?;
            Ok(response)
        }
        Ok(Committed::Replay(response)) => {
            tx.rollback().await;
            Ok(response)
        }
        Err(e) => {
            tx.rollback().await;
            Err(e)
        }
    }
}

enum Committed {
    Replay(Value),
    Written(Value),
}

async fn commit_inner<F>(
    tx: &mut crate::neo4j::Tx,
    pkg: &Package,
    request_digest: &str,
    staged_upload_id: Option<&str>,
    decorate: F,
) -> ApiResult<Committed>
where
    F: FnOnce(&Plan, &Context) -> ApiResult<Value>,
{
    let current_seq = store::lock(tx).await?;

    // The store lock makes this check and the graph writes one serialized
    // operation. A restore or discard cannot invalidate the handle between
    // this check and the marker below.
    if let Some(upload_id) = staged_upload_id {
        let staged = tx
            .run_one(crate::neo4j::stmt(
                "MATCH (s:IdeaDbUpload {upload_id:$upload_id}) \
                 RETURN s.prepared_digest AS prepared_digest, \
                 s.invalidated_at AS invalidated_at, s.withdrawn_at AS withdrawn_at, \
                 s.applied_seq AS applied_seq LIMIT 1",
                json!({"upload_id":upload_id}),
            ))
            .await?;
        if staged.rows.is_empty() {
            return Err(ApiError::not_found(format!(
                "staged upload {upload_id} not found"
            )));
        }
        if !staged.col(0, "invalidated_at").is_none_or(Value::is_null) {
            return Err(ApiError::validation(vec![Issue::new(
                "upload_id",
                "staging_invalidated",
                format!("staged upload {upload_id} was invalidated by a domain restore; prepare again with a new idempotency key"),
            )]));
        }
        if !staged.col(0, "withdrawn_at").is_none_or(Value::is_null) {
            return Err(ApiError::validation(vec![Issue::new(
                "upload_id",
                "staging_withdrawn",
                format!("staged upload {upload_id} was withdrawn; prepare a new upload"),
            )]));
        }
        if staged.col(0, "prepared_digest").and_then(Value::as_str) != Some(request_digest) {
            return Err(ApiError::bad_request(
                "prepared_digest mismatch; apply only the reviewed upload handle",
            ));
        }
    }

    // Idempotency is checked before the head comparison on purpose: an
    // interrupted client retrying the exact same request must get its original
    // receipt back, not a conflict caused by its own earlier success.
    if let Some(existing) = store::find_receipt(tx, &pkg.idempotency_key).await? {
        if existing.request_digest == request_digest {
            return Ok(Committed::Replay(as_replay(existing.response)));
        }
        return Err(ApiError::idempotency_conflict(&pkg.idempotency_key));
    }
    check_not_noop(pkg)?;

    let ctx = store::write_context(tx, current_seq).await?;
    check_heads(&pkg.expected_heads, &ctx)?;

    let seq = current_seq + 1;
    let applied_at = util::now_utc_millis();
    let plan =
        validate::validate_package(pkg, &ctx, seq, &applied_at).map_err(ApiError::validation)?;
    check_capacity(ctx.records.len(), plan.records.len())?;

    let mut response = json!({
        "receipt": receipt_json(pkg, &plan, seq, &applied_at, request_digest)
    });
    let extra = decorate(&plan, &ctx)?;
    if let Some(fields) = extra.as_object() {
        for (k, v) in fields {
            response[k.clone()] = v.clone();
        }
    }

    let statements = store::write_statements(
        &plan.records,
        seq,
        Some((&pkg.idempotency_key, request_digest, &response)),
    );
    // Run and check the writes while the transaction is still open. Verifying
    // after commit could only report a partial graph, never undo one.
    let results = tx.run(&statements).await?;
    store::verify_relationship_counts(&plan.records, &results)?;
    // The staged packet becomes applied in the SAME transaction as its receipt
    // and records. A process death cannot strand a committed upload as pending.
    if let Some(upload_id) = staged_upload_id {
        let marked = tx
            .run_one(crate::neo4j::stmt(
                "MATCH (s:IdeaDbUpload {upload_id:$upload_id}) \
                 WHERE s.prepared_digest=$digest AND s.invalidated_at IS NULL \
                 AND s.withdrawn_at IS NULL AND s.applied_seq IS NULL \
                 SET s.applied_seq=$seq RETURN count(s) AS marked",
                json!({"upload_id":upload_id,"digest":request_digest,"seq":seq}),
            ))
            .await?;
        if marked.col(0, "marked").and_then(Value::as_i64) != Some(1) {
            return Err(ApiError::storage(format!(
                "staged upload {upload_id} could not be marked applied"
            )));
        }
    }
    Ok(Committed::Written(response))
}

// ---------------------------------------------------------------- occurrences

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ReplaceMode {
    NewRevision,
    ExistingRevision,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddedRevision {
    id: String,
    data: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Replacement {
    mode: ReplaceMode,
    #[serde(default)]
    new_revision: Option<EmbeddedRevision>,
    #[serde(default)]
    new_records: Vec<NewRecord>,
    #[serde(default)]
    target_revision_id: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReplaceRequest {
    protocol_version: u32,
    idempotency_key: String,
    actor: String,
    #[serde(default)]
    reason: String,
    project_id: String,
    stage: Stage,
    expected_root_revision_id: String,
    slot_path: Vec<String>,
    replacement: Replacement,
    root_change_reason: String,
    decision: Decision,
}

/// Edit one use site: copy only the ancestors on the selected path and leave
/// every other occurrence pinned to the revision it already names.
pub async fn replace_occurrence(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let request_digest = util::digest_json(&body);
    let req: ReplaceRequest = parse_body(body)?;
    check_protocol(req.protocol_version)?;
    validate_replace_request(&req)?;

    // The plan is built inside the transaction, from state read under the lock.
    let mut tx = neo.begin().await?;
    let prepared = prepare_replacement(&mut tx, &req).await;
    let (pkg, mapping, old_chain) = match prepared {
        Ok(v) => v,
        Err(e) => {
            tx.rollback().await;
            // A replayed request must still get its stored receipt, so fall
            // through to the shared path rather than failing here.
            if e.code == "replay_pending" {
                return replay_or_fail(neo, &req.idempotency_key, &request_digest).await;
            }
            return Err(e);
        }
    };
    tx.rollback().await;

    let new_root = pkg
        .root_change
        .as_ref()
        .map(|rc| rc.after_revision_id.clone())
        .unwrap_or_default();
    commit_package(neo, pkg, &request_digest, None, move |plan, ctx| {
        Ok(json!({
            "new_root_revision_id": new_root,
            "mapping": mapping,
            "pinned_unchanged": pinned_unchanged(plan, ctx, &new_root, &old_chain),
        }))
    })
    .await
}

/// Build and validate an occurrence replacement package without committing it.
/// The transaction is rolled back before any package or metadata is returned.
pub async fn preview_replacement(neo: &Neo4j, body: Value) -> ApiResult<Value> {
    let req: ReplaceRequest = parse_body(body)?;
    check_protocol(req.protocol_version)?;
    validate_replace_request(&req)?;
    let mut tx = neo.begin().await?;
    let outcome = async {
        let (pkg, mapping, old_chain) = prepare_replacement(&mut tx, &req).await?;
        let seq = store::lock(&mut tx).await?;
        let ctx = store::write_context(&mut tx, seq).await?;
        let plan = validate::validate_package(&pkg, &ctx, seq + 1, &util::now_utc_millis())
            .map_err(ApiError::validation)?;
        check_capacity(ctx.records.len(), plan.records.len())?;
        let new_root = pkg
            .root_change
            .as_ref()
            .map(|c| c.after_revision_id.clone())
            .unwrap_or_default();
        Ok(json!({
            "package": package_value(&pkg),
            "replacement_result": {
                "new_root_revision_id": new_root,
                "mapping": mapping,
                "pinned_unchanged": pinned_unchanged(&plan, &ctx, &new_root, &old_chain),
            }
        }))
    }
    .await;
    tx.rollback().await;
    outcome
}

fn validate_replace_request(req: &ReplaceRequest) -> ApiResult<()> {
    if req.stage != Stage::Working
        || req.slot_path.is_empty()
        || req.slot_path.len() > graph::MAX_DEPTH
    {
        let (path, code, message) = if req.stage != Stage::Working {
            (
                "stage",
                "invalid_field",
                "an occurrence replacement moves the working head".to_string(),
            )
        } else if req.slot_path.is_empty() {
            (
                "slot_path",
                "invalid_field",
                "replacing the root itself is a package, not an occurrence edit".to_string(),
            )
        } else {
            (
                "slot_path",
                "limit_exceeded",
                format!("slot path deeper than {}", graph::MAX_DEPTH),
            )
        };
        return Err(ApiError::validation(vec![Issue::new(path, code, message)]));
    }
    match req.replacement.mode {
        ReplaceMode::NewRevision if req.replacement.new_revision.is_none() => {
            Err(ApiError::validation(vec![Issue::new(
                "replacement.new_revision",
                "invalid_field",
                "mode 'new_revision' must carry the new revision",
            )]))
        }
        ReplaceMode::ExistingRevision if req.replacement.target_revision_id.is_none() => {
            Err(ApiError::validation(vec![Issue::new(
                "replacement.target_revision_id",
                "invalid_field",
                "mode 'existing_revision' must name the target revision",
            )]))
        }
        _ => Ok(()),
    }
}

fn package_value(pkg: &Package) -> Value {
    json!({
        "protocol_version": pkg.protocol_version, "idempotency_key": pkg.idempotency_key,
        "actor": pkg.actor, "reason": pkg.reason,
        "expected_heads": pkg.expected_heads.iter().map(|h| json!({"project_id":h.project_id,"stage":h.stage,"revision_id":h.revision_id})).collect::<Vec<_>>(),
        "records": pkg.records.iter().map(|r| json!({"id":r.id,"kind":r.kind,"data":r.data})).collect::<Vec<_>>(),
        "root_change": pkg.root_change.as_ref().map(|c| json!({"project_id":c.project_id,"stage":c.stage,"after_revision_id":c.after_revision_id,"reason":c.reason,"meaningful":c.meaningful,"decision":c.decision})),
        "publish": pkg.publish.as_ref().map(|p| json!({"project_id":p.project_id,"root_revision_id":p.root_revision_id,"label":p.label,"published_at":p.published_at,"notes":p.notes})),
    })
}

/// A replace whose key was already used: hand back the stored receipt.
async fn replay_or_fail(neo: &Neo4j, key: &str, digest: &str) -> ApiResult<Value> {
    let mut tx = neo.begin().await?;
    let found = async {
        store::lock(&mut tx).await?;
        store::find_receipt(&mut tx, key).await
    }
    .await;
    tx.rollback().await;
    match found? {
        Some(existing) if existing.request_digest == digest => Ok(as_replay(existing.response)),
        Some(_) => Err(ApiError::idempotency_conflict(key)),
        None => Err(ApiError::storage("receipt disappeared between reads")),
    }
}

type Prepared = (Package, Vec<Value>, Vec<String>);

async fn prepare_replacement(
    tx: &mut crate::neo4j::Tx,
    req: &ReplaceRequest,
) -> ApiResult<Prepared> {
    let current_seq = store::lock(tx).await?;
    if store::find_receipt(tx, &req.idempotency_key)
        .await?
        .is_some()
    {
        return Err(ApiError {
            status: axum::http::StatusCode::CONFLICT,
            code: "replay_pending",
            message: "handled by the shared replay path".into(),
            details: None,
        });
    }
    let ctx = store::write_context(tx, current_seq).await?;

    let expected = vec![ExpectedHead {
        project_id: req.project_id.clone(),
        stage: Stage::Working,
        revision_id: Some(req.expected_root_revision_id.clone()),
    }];
    check_heads(&expected, &ctx)?;

    // Walk the path once, keeping every ancestor we will have to copy.
    let mut chain: Vec<StoredRecord> = Vec::with_capacity(req.slot_path.len() + 1);
    let root = ctx
        .records
        .get(&req.expected_root_revision_id)
        .filter(|r| r.kind() == RecordKind::Revision)
        .ok_or_else(|| {
            ApiError::validation(vec![Issue::new(
                "expected_root_revision_id",
                "unknown_reference",
                format!("{} is not a revision", req.expected_root_revision_id),
            )])
        })?;
    chain.push(root.clone());
    for (depth, slot_id) in req.slot_path.iter().enumerate() {
        let prefix = &req.slot_path[..depth];
        let current = chain.last().expect("chain always has the root");
        let slot = current
            .as_revision()
            .and_then(|r| r.slots.iter().find(|s| &s.slot_id == slot_id))
            .ok_or_else(|| {
                ApiError::validation(vec![Issue::new(
                    "slot_path",
                    "slot_path_not_found",
                    PathError::SlotMissing {
                        at: prefix.to_vec(),
                        slot_id: slot_id.clone(),
                    }
                    .message(),
                )])
            })?;
        let child = ctx
            .records
            .get(&slot.revision_id)
            .filter(|r| r.kind() == RecordKind::Revision)
            .ok_or_else(|| {
                ApiError::validation(vec![Issue::new(
                    "slot_path",
                    "slot_path_not_found",
                    format!("revision {} is missing", slot.revision_id),
                )])
            })?;
        chain.push(child.clone());
    }

    // The leaf is replaced outright; only its ancestors are copied.
    let leaf_old = chain.pop().expect("path has at least one slot");
    let (leaf_new_id, mut records) = match req.replacement.mode {
        ReplaceMode::NewRevision => {
            let embedded = req
                .replacement
                .new_revision
                .as_ref()
                .expect("checked above");
            let mut records = req.replacement.new_records.clone();
            records.push(NewRecord {
                id: embedded.id.clone(),
                kind: RecordKind::Revision,
                data: embedded.data.clone(),
            });
            (embedded.id.clone(), records)
        }
        ReplaceMode::ExistingRevision => {
            let target = req
                .replacement
                .target_revision_id
                .clone()
                .expect("checked above");
            if !ctx
                .records
                .get(&target)
                .is_some_and(|r| r.kind() == RecordKind::Revision)
            {
                return Err(ApiError::validation(vec![Issue::new(
                    "replacement.target_revision_id",
                    "unknown_reference",
                    format!("{target} is not an existing revision"),
                )]));
            }
            (target, req.replacement.new_records.clone())
        }
    };

    let mut mapping = vec![json!({
        "slot_path": req.slot_path,
        "old_revision_id": leaf_old.id,
        "new_revision_id": leaf_new_id,
    })];
    let mut old_chain: Vec<String> = vec![leaf_old.id.clone()];

    // Copy ancestors bottom-up. Each copy keeps every slot it had and only
    // repoints the one slot on the edited path, which is what leaves peer
    // occurrences and every historical root untouched.
    let mut child_id = leaf_new_id;
    for depth in (0..req.slot_path.len()).rev() {
        let ancestor = &chain[depth];
        let new_id = util::derived_id("rev", &format!("{}:{}", req.idempotency_key, ancestor.id));
        let mut data = ancestor.data_value();
        let slot_id = &req.slot_path[depth];
        if let Some(slots) = data.get_mut("slots").and_then(Value::as_array_mut) {
            for slot in slots.iter_mut() {
                if slot.get("slot_id").and_then(Value::as_str) == Some(slot_id.as_str()) {
                    slot["revision_id"] = json!(child_id);
                }
            }
        }
        data["change_kind"] = json!(ChangeKind::Composition);
        data["previous_revision_id"] = json!(ancestor.id);
        data["correction_of"] = Value::Null;
        data["correction_reason"] = Value::Null;
        records.push(NewRecord {
            id: new_id.clone(),
            kind: RecordKind::Revision,
            data,
        });
        mapping.push(json!({
            "slot_path": req.slot_path[..depth],
            "old_revision_id": ancestor.id,
            "new_revision_id": new_id,
        }));
        old_chain.push(ancestor.id.clone());
        child_id = new_id;
    }
    mapping.reverse();

    let pkg = Package {
        protocol_version: PROTOCOL_VERSION,
        idempotency_key: req.idempotency_key.clone(),
        actor: req.actor.clone(),
        reason: req.reason.clone(),
        expected_heads: expected,
        records,
        root_change: Some(RootChange {
            project_id: req.project_id.clone(),
            stage: Stage::Working,
            after_revision_id: child_id,
            reason: req.root_change_reason.clone(),
            meaningful: true,
            decision: req.decision.clone(),
        }),
        publish: None,
    };
    Ok((pkg, mapping, old_chain))
}

/// Use sites in the new root that still point at a revision we replaced
/// elsewhere. This is the evidence that a shared atom kept its other homes.
fn pinned_unchanged(plan: &Plan, ctx: &Context, new_root: &str, old_chain: &[String]) -> Value {
    // The new root exists only in the plan at this point, so the walk needs the
    // about-to-be-written revisions layered over stored state.
    let overlay = graph::Overlay::new(ctx, plan.records.iter());
    let walk = graph::walk(
        &overlay,
        new_root,
        Limits {
            max_nodes: limits::MAX_SNAPSHOT_NODES,
            max_depth: graph::MAX_DEPTH,
        },
    );
    json!(walk
        .occurrences
        .iter()
        .filter(|o| old_chain.iter().any(|id| id == &o.revision_id))
        .map(|o| json!({"slot_path": o.slot_path, "revision_id": o.revision_id, "roles": o.roles}))
        .collect::<Vec<_>>())
}

// ---------------------------------------------------------------- captures

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaptureRequest {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    project_id: Option<String>,
    content: String,
    #[serde(default = "text_plain")]
    media_type: String,
    source_kind: SourceKind,
    #[serde(default)]
    source_ref: Option<String>,
    #[serde(default)]
    occurred_at: Option<String>,
    #[serde(default)]
    content_digest: Option<String>,
}

fn text_plain() -> String {
    "text/plain".to_string()
}

/// Captures are stored in their own transaction so that raw source survives
/// every downstream proposal that later fails validation.
pub async fn create_capture(neo: &Neo4j, body: Value) -> ApiResult<(bool, Value)> {
    let req: CaptureRequest = parse_body(body)?;
    let digest = util::digest_text(&req.content);
    let data = CaptureData {
        project_id: req.project_id.clone(),
        content: req.content.clone(),
        content_digest: Some(digest.clone()),
        media_type: req.media_type.clone(),
        source_kind: req.source_kind,
        source_ref: req.source_ref.clone(),
        occurred_at: req.occurred_at.clone(),
    };
    if let Some(declared) = &req.content_digest {
        if declared != &digest {
            return Err(ApiError::validation(vec![Issue::new(
                "content_digest",
                "content_digest_mismatch",
                format!("declared {declared} but content hashes to {digest}"),
            )]));
        }
    }
    let payload = serde_json::to_value(&data).map_err(|e| ApiError::bad_request(e.to_string()))?;

    let mut tx = neo.begin().await?;
    let outcome = capture_inner(&mut tx, &req, payload, &digest).await;
    match outcome {
        Ok(CaptureOutcome::Replay(value)) => {
            tx.rollback().await;
            Ok((false, value))
        }
        Ok(CaptureOutcome::Created(response)) => {
            tx.commit(&[]).await?;
            Ok((true, response))
        }
        Err(e) => {
            tx.rollback().await;
            Err(e)
        }
    }
}

enum CaptureOutcome {
    Replay(Value),
    Created(Value),
}

async fn capture_inner(
    tx: &mut crate::neo4j::Tx,
    req: &CaptureRequest,
    payload: Value,
    digest: &str,
) -> ApiResult<CaptureOutcome> {
    let current_seq = store::lock(tx).await?;
    let ctx = store::write_context(tx, current_seq).await?;

    // An explicit id is the idempotency key. No id means a new event, even when
    // the text is byte-identical to an earlier capture.
    let id = match &req.id {
        Some(id) => {
            if let Some(existing) = ctx.records.get(id) {
                let same = existing
                    .as_capture()
                    .map(|c| serde_json::to_value(c).ok() == Some(payload.clone()))
                    .unwrap_or(false);
                return if same {
                    Ok(CaptureOutcome::Replay(json!({
                        "capture": existing.to_json(),
                        "content_digest": digest,
                        "replay": true,
                    })))
                } else {
                    Err(ApiError::idempotency_conflict(id))
                };
            }
            id.clone()
        }
        None => {
            let nonce = CAPTURE_NONCE.fetch_add(1, Ordering::Relaxed);
            util::derived_id(
                "cap",
                &format!(
                    "{}:{}:{}:{nonce}",
                    util::now_utc_millis(),
                    current_seq,
                    digest
                ),
            )
        }
    };

    let seq = current_seq + 1;
    let now = util::now_utc_millis();
    let pkg = Package {
        protocol_version: PROTOCOL_VERSION,
        idempotency_key: format!("capture:{id}"),
        actor: "capture".to_string(),
        reason: "raw capture".to_string(),
        expected_heads: Vec::new(),
        records: vec![NewRecord {
            id: id.clone(),
            kind: RecordKind::Capture,
            data: payload,
        }],
        root_change: None,
        publish: None,
    };
    let plan = validate::validate_package(&pkg, &ctx, seq, &now).map_err(ApiError::validation)?;
    check_capacity(ctx.records.len(), plan.records.len())?;
    let stored = plan.records.first().cloned().expect("one capture record");
    let response = json!({
        "capture": stored.to_json(),
        "content_digest": digest,
        "replay": false,
    });
    let statements = store::write_statements(&plan.records, seq, None);
    let results = tx.run(&statements).await?;
    store::verify_relationship_counts(&plan.records, &results)?;
    Ok(CaptureOutcome::Created(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::fixtures::deep_diamond;

    const CAP: usize = limits::MAX_PREFETCH_RECORDS;

    fn context() -> Context {
        let mut ctx = Context::default();
        for rec in deep_diamond().into_values() {
            if let Some(rev) = rec.as_revision() {
                ctx.entity_revisions
                    .entry(rev.entity_id.clone())
                    .or_default()
                    .push((rec.seq, rec.id.clone()));
            }
            ctx.seq = ctx.seq.max(rec.seq);
            ctx.records.insert(rec.id.clone(), rec);
        }
        for revisions in ctx.entity_revisions.values_mut() {
            revisions.sort();
        }
        ctx
    }

    fn code_of(err: &ApiError) -> String {
        err.details
            .as_ref()
            .and_then(|d| d.get("issues"))
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|i| i.get("code"))
            .and_then(Value::as_str)
            .unwrap_or(err.code)
            .to_string()
    }

    #[test]
    fn capacity_is_checked_at_the_boundary_not_past_it() {
        check_capacity(CAP - 1, 1).expect("filling the namespace exactly is allowed");
        check_capacity(CAP, 0).expect("a commit that writes nothing new stays inside the bound");
        let err = check_capacity(CAP - 1, 2).expect_err("one record past the bound is refused");
        assert_eq!(code_of(&err), "limit_exceeded");
        assert_eq!(err.code, "validation_failed");
        assert!(
            err.message.contains(&CAP.to_string()),
            "the error should name the bound: {}",
            err.message
        );
    }

    #[test]
    fn empty_apply_is_rejected_but_server_head_events_are_meaningful() {
        let empty: Package = serde_json::from_value(json!({
            "protocol_version":1,"idempotency_key":"empty","actor":"human:test","records":[]
        }))
        .unwrap();
        assert_eq!(
            code_of(&check_not_noop(&empty).unwrap_err()),
            "empty_commit"
        );

        let mut head_only = empty;
        head_only.root_change = serde_json::from_value(json!({
            "project_id":"proj_login","stage":"working","after_revision_id":"rev_root",
            "reason":"adopt","meaningful":true,
            "decision":{"before":"none","after":"root","rationale":"adopt"}
        }))
        .unwrap();
        check_not_noop(&head_only).expect("a head change writes a server record");

        head_only.root_change = None;
        head_only.publish = serde_json::from_value(json!({
            "project_id":"proj_login","root_revision_id":"rev_root","label":"v1",
            "published_at":"2026-09-07T00:00:00Z"
        }))
        .unwrap();
        check_not_noop(&head_only).expect("a publication writes a server record");
    }

    #[test]
    fn a_full_namespace_still_refuses_one_more_record() {
        // The failure this guards against is a namespace that can be written
        // but never read back, so the check must bite before the write.
        assert!(check_capacity(CAP, 1).is_err());
        assert!(check_capacity(CAP + 1, 0).is_err());
    }

    /// The server writes `head_change` and `publication` records of its own, so
    /// counting only the client's records would let a commit slip past the bound.
    #[test]
    fn the_capacity_count_includes_server_written_head_records() {
        let ctx = context();
        let pkg: Package = serde_json::from_value(json!({
            "protocol_version": 1,
            "idempotency_key": "capacity-probe",
            "actor": "human:test",
            "reason": "move and publish the root",
            "expected_heads": [
                {"project_id": "proj_login", "stage": "working", "revision_id": null},
                {"project_id": "proj_login", "stage": "official", "revision_id": null}
            ],
            "records": [],
            "root_change": {
                "project_id": "proj_login", "stage": "working",
                "after_revision_id": "rev_root", "reason": "initial",
                "meaningful": true,
                "decision": {"before": "none", "after": "root", "rationale": "fixture"}
            },
            "publish": {
                "project_id": "proj_login", "root_revision_id": "rev_root",
                "label": "v1", "published_at": "2026-09-01T00:00:00Z", "notes": ""
            }
        }))
        .expect("package shape");

        let plan = validate::validate_package(&pkg, &ctx, ctx.seq + 1, "2026-09-07T00:00:00.000Z")
            .expect("a documented root change and publication");
        assert_eq!(
            plan.records.len(),
            2,
            "the client sent no records; both come from the server"
        );

        // With one slot left, counting only the (empty) client list would wave
        // this through; counting the plan sees two records and refuses.
        check_capacity(CAP - 1, pkg.records.len())
            .expect("client-only count sees nothing to store");
        assert!(
            check_capacity(CAP - 1, plan.records.len()).is_err(),
            "counting the plan catches the two server records"
        );
    }

    /// A replay never reaches the capacity check: it returns the stored receipt
    /// before a plan is built, so a full namespace can still answer retries.
    #[test]
    fn a_replay_is_answered_without_planning_new_records() {
        let stored = json!({"receipt": {"idempotency_key": "k", "seq": 3, "replay": false}});
        let replayed = as_replay(stored.clone());
        assert_eq!(replayed["receipt"]["replay"], json!(true));
        assert_eq!(replayed["receipt"]["seq"], stored["receipt"]["seq"]);
        assert_eq!(
            replayed["receipt"]["idempotency_key"],
            stored["receipt"]["idempotency_key"]
        );
    }
}
