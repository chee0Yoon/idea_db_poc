//! Neo4j persistence: schema bootstrap, the transactional write lock, loading a
//! consistent `Context`, and turning a validated `Plan` into Cypher.
//!
//! Two decisions here are load-bearing and deliberately conservative:
//!
//! * **The write lock is in the database, not in the process.** Every request
//!   that reads state it will act on takes a write dependency on a singleton
//!   node *before* reading, so compare-and-set holds across clients and across
//!   API replicas. `docs/api.md` §2.6.
//! * **Heads are derived from records, never stored as an independent
//!   pointer.** A head is whatever the `head_change` / `publication` event
//!   stream says it is, so a head can never drift away from the events that
//!   justify it, and an import cannot smuggle in a forged head.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};

use crate::error::{ApiError, ApiResult};
use crate::limits;
use crate::model::{LinkType, RecordData, RecordKind, Stage, StoredRecord};
use crate::neo4j::{stmt, Neo4j, QueryResult, Statement, Tx};
use crate::util;
use crate::validate::{Context, HeadState};

const REC_COLS: &str =
    "n.id AS id, n.kind AS kind, n.seq AS seq, n.recorded_at AS recorded_at, n.data AS data";

/// Schema objects created once at startup. `IF NOT EXISTS` makes this safe to
/// run on every boot and on a restored volume.
const SCHEMA: &[&str] = &[
    "CREATE CONSTRAINT idea_db_record_id IF NOT EXISTS FOR (n:Record) REQUIRE n.id IS UNIQUE",
    "CREATE CONSTRAINT idea_db_receipt_key IF NOT EXISTS FOR (n:Receipt) REQUIRE n.idempotency_key IS UNIQUE",
    "CREATE CONSTRAINT idea_db_meta_id IF NOT EXISTS FOR (n:IdeaDbMeta) REQUIRE n.id IS UNIQUE",
    "CREATE CONSTRAINT idea_db_upload_id IF NOT EXISTS FOR (n:IdeaDbUpload) REQUIRE n.upload_id IS UNIQUE",
    "CREATE INDEX idea_db_record_kind IF NOT EXISTS FOR (n:Record) ON (n.kind)",
    "CREATE INDEX idea_db_record_seq IF NOT EXISTS FOR (n:Record) ON (n.seq)",
    "CREATE INDEX idea_db_record_project IF NOT EXISTS FOR (n:Record) ON (n.project_id)",
    "CREATE INDEX idea_db_record_entity IF NOT EXISTS FOR (n:Record) ON (n.entity_id)",
];

pub async fn bootstrap(neo: &Neo4j) -> ApiResult<()> {
    for cypher in SCHEMA {
        neo.run(&[stmt(*cypher, json!({}))]).await?;
    }
    neo.run(&[stmt(
        "MERGE (m:IdeaDbMeta {id:'global'}) ON CREATE SET m.seq = 0, m.lock_version = 0",
        json!({}),
    )])
    .await?;
    Ok(())
}

/// Readiness asks the database a real question every time. A flag latched at
/// startup would keep reporting ready after the database went away.
pub async fn ping(neo: &Neo4j) -> ApiResult<i64> {
    let results = neo
        .run(&[stmt(
            "MATCH (m:IdeaDbMeta {id:'global'}) RETURN m.seq AS seq",
            json!({}),
        )])
        .await?;
    results
        .first()
        .and_then(|r| r.col(0, "seq"))
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::storage("singleton meta node is missing; run bootstrap"))
}

/// Take the global write dependency and return the current sequence number.
///
/// The `SET` locks the singleton for the rest of the transaction, so everything
/// read afterwards stays valid until commit or rollback. Writing before reading
/// is the whole point: a read-then-write order would let two transactions both
/// observe the same head and both commit.
pub async fn lock(tx: &mut Tx) -> ApiResult<i64> {
    let result = tx
        .run_one(stmt(
            "MATCH (m:IdeaDbMeta {id:'global'}) \
             SET m.lock_version = m.lock_version + 1 \
             RETURN m.seq AS seq",
            json!({}),
        ))
        .await?;
    result
        .col(0, "seq")
        .and_then(Value::as_i64)
        .ok_or_else(|| ApiError::storage("singleton meta node is missing; run bootstrap"))
}

// ---------------------------------------------------------------- reading

fn row_to_record(r: &QueryResult, i: usize) -> ApiResult<StoredRecord> {
    let id = r
        .col(i, "id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let kind_text = r.col(i, "kind").and_then(Value::as_str).unwrap_or_default();
    let kind = RecordKind::parse_str(kind_text)
        .ok_or_else(|| ApiError::storage(format!("record {id} has unknown kind '{kind_text}'")))?;
    let seq = r.col(i, "seq").and_then(Value::as_i64).unwrap_or_default();
    let recorded_at = r
        .col(i, "recorded_at")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let raw = r.col(i, "data").and_then(Value::as_str).unwrap_or("{}");
    let value: Value = serde_json::from_str(raw)
        .map_err(|e| ApiError::storage(format!("record {id} has unreadable payload: {e}")))?;
    let data = RecordData::parse(kind, value)
        .map_err(|e| ApiError::storage(format!("record {id} no longer matches its schema: {e}")))?;
    Ok(StoredRecord {
        id,
        seq,
        recorded_at,
        data,
    })
}

fn rows_to_records(result: &QueryResult) -> ApiResult<Vec<StoredRecord>> {
    (0..result.rows.len())
        .map(|i| row_to_record(result, i))
        .collect()
}

/// Load every record in the namespace, in commit order.
///
/// The MVP keeps the whole graph inside `MAX_PREFETCH_RECORDS` and reads it in
/// one statement so that records, heads and `seq` are all from the same
/// transaction. Several separate queries under read-committed would not be
/// repeatable, and a snapshot stitched from inconsistent reads is worse than a
/// slow one. Exceeding the bound is reported, never silently truncated.
async fn load_records(tx: &mut Tx) -> ApiResult<Vec<StoredRecord>> {
    let cap = limits::MAX_PREFETCH_RECORDS;
    let result = tx
        .run_one(stmt(
            format!("MATCH (n:Record) RETURN {REC_COLS} ORDER BY n.seq, n.id LIMIT $limit"),
            json!({ "limit": cap as i64 + 1 }),
        ))
        .await?;
    if result.rows.len() > cap {
        return Err(ApiError::context_too_large(result.rows.len(), cap));
    }
    rows_to_records(&result)
}

/// Working and official heads, derived from the event records themselves.
pub fn derive_heads(records: &BTreeMap<String, StoredRecord>) -> BTreeMap<String, HeadState> {
    let mut heads: BTreeMap<String, HeadState> = BTreeMap::new();
    for rec in records.values() {
        if let RecordData::Project(_) = rec.data {
            heads.entry(rec.id.clone()).or_default();
        }
    }
    // Working head: the latest head_change wins.
    for rec in records.values() {
        let Some(hc) = rec.as_head_change() else {
            continue;
        };
        if hc.stage != Stage::Working {
            continue;
        }
        let entry = heads.entry(hc.project_id.clone()).or_default();
        if rec.seq >= entry.working_head_seq {
            entry.working_head = Some(hc.after_revision_id.clone());
            entry.working_head_seq = rec.seq;
        }
    }
    // Official head: chronological publication precedence, broken
    // deterministically by (published_at, seq, id).
    let mut best: BTreeMap<String, (String, i64, String)> = BTreeMap::new();
    for rec in records.values() {
        let Some(pb) = rec.as_publication() else {
            continue;
        };
        let key = (
            util::parse_rfc3339(&pb.published_at)
                .map(util::to_utc_key)
                .unwrap_or_default(),
            rec.seq,
            rec.id.clone(),
        );
        let slot = best.entry(pb.project_id.clone());
        match slot {
            std::collections::btree_map::Entry::Vacant(v) => {
                v.insert(key);
            }
            std::collections::btree_map::Entry::Occupied(mut o) => {
                if key > *o.get() {
                    o.insert(key);
                }
            }
        }
    }
    for (project_id, (_, _, publication_id)) in best {
        if let Some(pb) = records
            .get(&publication_id)
            .and_then(|r| r.as_publication())
        {
            let entry = heads.entry(project_id).or_default();
            entry.official_head = Some(pb.root_revision_id.clone());
            entry.official_publication_id = Some(publication_id);
        }
    }
    heads
}

fn build_context(records: Vec<StoredRecord>, seq: i64) -> Context {
    let mut ctx = Context {
        seq,
        ..Context::default()
    };
    for rec in records {
        if let Some(rev) = rec.as_revision() {
            ctx.entity_revisions
                .entry(rev.entity_id.clone())
                .or_default()
                .push((rec.seq, rec.id.clone()));
        }
        if let RecordData::Promotion(p) = &rec.data {
            ctx.promoted_candidates.insert(p.candidate_id.clone());
        }
        ctx.records.insert(rec.id.clone(), rec);
    }
    for revisions in ctx.entity_revisions.values_mut() {
        revisions.sort();
    }
    ctx.heads = derive_heads(&ctx.records);
    ctx
}

/// Consistent state for a read-only request.
///
/// Reads take the same lock as writes and then roll back. That serialises reads
/// against in-flight writes, which is the cost of getting a repeatable view out
/// of several statements without a snapshot isolation level. It is a real
/// throughput limit for this release, not a measured-safe design.
pub async fn read_context(neo: &Neo4j) -> ApiResult<Context> {
    let mut tx = neo.begin().await?;
    let loaded = async {
        let seq = lock(&mut tx).await?;
        let records = load_records(&mut tx).await?;
        Ok::<Context, ApiError>(build_context(records, seq))
    }
    .await;
    tx.rollback().await;
    loaded
}

/// Consistent state inside a write transaction that is already locked.
pub async fn write_context(tx: &mut Tx, seq: i64) -> ApiResult<Context> {
    let records = load_records(tx).await?;
    Ok(build_context(records, seq))
}

/// Export needs records and receipts from the same consistent read.
pub async fn read_context_with_receipts(neo: &Neo4j) -> ApiResult<(Context, Vec<Value>)> {
    let mut tx = neo.begin().await?;
    let loaded = async {
        let seq = lock(&mut tx).await?;
        let records = load_records(&mut tx).await?;
        let receipts = load_receipts(&mut tx).await?;
        Ok::<(Context, Vec<Value>), ApiError>((build_context(records, seq), receipts))
    }
    .await;
    tx.rollback().await;
    loaded
}

async fn load_receipts(tx: &mut Tx) -> ApiResult<Vec<Value>> {
    let result = tx
        .run_one(stmt(
            "MATCH (r:Receipt) RETURN r.idempotency_key AS key, r.request_digest AS digest, \
             r.seq AS seq, r.recorded_at AS recorded_at, r.response AS response ORDER BY r.seq, r.idempotency_key",
            json!({}),
        ))
        .await?;
    let mut out = Vec::with_capacity(result.rows.len());
    for i in 0..result.rows.len() {
        let raw = result
            .col(i, "response")
            .and_then(Value::as_str)
            .unwrap_or("{}");
        let response: Value = serde_json::from_str(raw)
            .map_err(|e| ApiError::storage(format!("receipt has an unreadable response: {e}")))?;
        out.push(json!({
            "idempotency_key": result.col(i, "key").cloned().unwrap_or(Value::Null),
            "request_digest": result.col(i, "digest").cloned().unwrap_or(Value::Null),
            "seq": result.col(i, "seq").cloned().unwrap_or(Value::Null),
            "recorded_at": result.col(i, "recorded_at").cloned().unwrap_or(Value::Null),
            "response": response,
        }));
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct StoredReceipt {
    pub request_digest: String,
    pub response: Value,
}

pub async fn find_receipt(tx: &mut Tx, key: &str) -> ApiResult<Option<StoredReceipt>> {
    let result = tx
        .run_one(stmt(
            "MATCH (r:Receipt {idempotency_key: $key}) \
             RETURN r.request_digest AS digest, r.response AS response",
            json!({ "key": key }),
        ))
        .await?;
    if result.rows.is_empty() {
        return Ok(None);
    }
    let request_digest = result
        .col(0, "digest")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let raw = result
        .col(0, "response")
        .and_then(Value::as_str)
        .unwrap_or("{}");
    let response = serde_json::from_str(raw)
        .map_err(|e| ApiError::storage(format!("receipt {key} has an unreadable response: {e}")))?;
    Ok(Some(StoredReceipt {
        request_digest,
        response,
    }))
}

pub async fn count_records(tx: &mut Tx) -> ApiResult<i64> {
    let result = tx
        .run_one(stmt("MATCH (n:Record) RETURN count(n) AS total", json!({})))
        .await?;
    Ok(result.col(0, "total").and_then(Value::as_i64).unwrap_or(0))
}

// ---------------------------------------------------------------- writing

fn node_row(rec: &StoredRecord) -> Value {
    let mut props: Map<String, Value> = rec.scalars();
    props.insert("id".into(), json!(rec.id));
    props.insert("kind".into(), json!(rec.kind().as_str()));
    props.insert("seq".into(), json!(rec.seq));
    props.insert("recorded_at".into(), json!(rec.recorded_at));
    props.insert(
        "data".into(),
        json!(util::canonical_json(&rec.data_value())),
    );
    Value::Object(props)
}

fn rel(from: &str, to: &str, props: Value) -> Value {
    json!({ "from": from, "to": to, "props": props })
}

/// Typed relationships projected from record payloads. The relationship type is
/// always one of a fixed set of literals; no part of it comes from a request.
fn rel_batches(records: &[StoredRecord]) -> Vec<(&'static str, Vec<Value>)> {
    let mut batches: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    let mut add = |ty: &'static str, row: Value| batches.entry(ty).or_default().push(row);

    for rec in records {
        match &rec.data {
            RecordData::Entity(d) => {
                add("IN_PROJECT", rel(&rec.id, &d.project_id, json!({})));
                for parent in &d.derived_from {
                    add("DERIVED_FROM", rel(&rec.id, parent, json!({})));
                }
            }
            RecordData::Revision(d) => {
                add("REVISION_OF", rel(&rec.id, &d.entity_id, json!({})));
                for (ord, slot) in d.slots.iter().enumerate() {
                    add(
                        "CONTAINS",
                        rel(
                            &rec.id,
                            &slot.revision_id,
                            json!({"slot_id": slot.slot_id, "roles": slot.roles, "ord": ord as i64}),
                        ),
                    );
                }
                if let Some(prev) = &d.correction_of {
                    add("CORRECTION_OF", rel(&rec.id, prev, json!({})));
                }
            }
            RecordData::Candidate(d) => {
                if let Some(cap) = &d.capture_id {
                    add("FROM_CAPTURE", rel(&rec.id, cap, json!({})));
                }
            }
            RecordData::Promotion(d) => {
                add("PROMOTES", rel(&rec.id, &d.candidate_id, json!({})));
                add("PRODUCED", rel(&rec.id, &d.entity_id, json!({})));
                add("PRODUCED", rel(&rec.id, &d.revision_id, json!({})));
            }
            RecordData::Observation(d) => {
                if let Some(target) = &d.target_revision_id {
                    add("OBSERVES", rel(&rec.id, target, json!({})));
                }
                for artifact in &d.artifact_ids {
                    add("ARTIFACT", rel(&rec.id, artifact, json!({})));
                }
            }
            RecordData::Goal(d) => {
                add("IN_PROJECT", rel(&rec.id, &d.project_id, json!({})));
                add(
                    "SCOPED_TO",
                    rel(&rec.id, &d.scope.target_revision_id, json!({})),
                );
            }
            RecordData::Baseline(d) => {
                add("FOR_GOAL", rel(&rec.id, &d.goal_id, json!({})));
                if let Some(prev) = &d.supersedes {
                    add("SUPERSEDES", rel(&rec.id, prev, json!({})));
                }
            }
            RecordData::Assessment(d) => {
                add("AGAINST_BASELINE", rel(&rec.id, &d.baseline_id, json!({})));
                add("EVALUATES", rel(&rec.id, &d.target_revision_id, json!({})));
                for obs in &d.evidence_observation_ids {
                    add("EVIDENCE", rel(&rec.id, obs, json!({})));
                }
                for result in &d.criteria_results {
                    if let Some(estimate) = &result.progress_estimate {
                        for evidence in &estimate.evidence_record_ids {
                            add(
                                "EVIDENCE",
                                rel(
                                    &rec.id,
                                    evidence,
                                    json!({"criterion_id": result.criterion_id, "evidence_kind": "progress_estimate"}),
                                ),
                            );
                        }
                    }
                }
            }
            RecordData::Link(d) => {
                add(
                    link_rel(d.link_type),
                    rel(&d.from_id, &d.to_id, json!({"link_id": rec.id})),
                );
            }
            RecordData::Embedding(d) => {
                add("EMBEDS", rel(&rec.id, &d.revision_id, json!({})));
            }
            RecordData::HeadChange(d) => {
                add("IN_PROJECT", rel(&rec.id, &d.project_id, json!({})));
                add("MOVES_TO", rel(&rec.id, &d.after_revision_id, json!({})));
            }
            RecordData::Publication(d) => {
                add("IN_PROJECT", rel(&rec.id, &d.project_id, json!({})));
                add("PUBLISHES", rel(&rec.id, &d.root_revision_id, json!({})));
            }
            RecordData::Project(_) | RecordData::Capture(_) | RecordData::Artifact(_) => {}
        }
    }
    batches.into_iter().collect()
}

fn link_rel(link_type: LinkType) -> &'static str {
    // Matching on the enum keeps the Cypher literal out of request data.
    match link_type {
        LinkType::Derived => "DERIVED",
        LinkType::Applies => "APPLIES",
        LinkType::Implements => "IMPLEMENTS",
        LinkType::Depends => "DEPENDS",
        LinkType::Similar => "SIMILAR",
        LinkType::Support => "SUPPORTS",
        LinkType::Contradict => "CONTRADICTS",
        LinkType::Impact => "IMPACTS",
    }
}

/// Statements that persist a validated batch. Run inside the locked
/// transaction; the caller commits them all or rolls the whole thing back.
pub fn write_statements(
    records: &[StoredRecord],
    seq: i64,
    receipt: Option<(&str, &str, &Value)>,
) -> Vec<Statement> {
    let mut out = Vec::new();

    let mut by_kind: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for rec in records {
        by_kind
            .entry(rec.kind().label())
            .or_default()
            .push(node_row(rec));
    }
    for (label, rows) in by_kind {
        // `label` is a compile-time constant chosen by RecordKind, never input.
        out.push(stmt(
            format!("UNWIND $rows AS row CREATE (n:Record:{label}) SET n = row"),
            json!({ "rows": rows }),
        ));
    }

    for (rel_type, rows) in rel_batches(records) {
        // `rel_type` is a literal from the whitelist above, never request text.
        out.push(stmt(
            format!(
                "UNWIND $rows AS row \
                 MATCH (a:Record {{id: row.from}}) MATCH (b:Record {{id: row.to}}) \
                 CREATE (a)-[r:{rel_type}]->(b) SET r = row.props \
                 RETURN count(r) AS created"
            ),
            json!({ "rows": rows }),
        ));
    }

    out.push(stmt(
        "MATCH (m:IdeaDbMeta {id:'global'}) SET m.seq = $seq",
        json!({ "seq": seq }),
    ));

    if let Some((key, digest, response)) = receipt {
        out.push(stmt(
            "CREATE (r:Receipt {idempotency_key: $key, request_digest: $digest, \
             seq: $seq, recorded_at: $recorded_at, response: $response})",
            json!({
                "key": key,
                "digest": digest,
                "seq": seq,
                "recorded_at": util::now_utc_millis(),
                "response": util::canonical_json(response),
            }),
        ));
    }
    out
}

/// Restore statements. Records keep their original `id`, `seq` and
/// `recorded_at`; nothing is re-stamped. Receipts are restored verbatim so a
/// client that retries an old idempotency key still gets its original answer.
pub fn import_statements(
    records: &[StoredRecord],
    final_seq: i64,
    receipts: &[Value],
) -> Vec<Statement> {
    let mut out = write_statements(records, final_seq, None);
    for row in receipts {
        out.push(stmt(
            "CREATE (r:Receipt {idempotency_key: $key, request_digest: $digest, \
             seq: $seq, recorded_at: $recorded_at, response: $response})",
            json!({
                "key": row.get("idempotency_key"),
                "digest": row.get("request_digest"),
                "seq": row.get("seq"),
                "recorded_at": row.get("recorded_at"),
                "response": util::canonical_json(row.get("response").unwrap_or(&Value::Null)),
            }),
        ));
    }
    out
}

/// Every relationship batch must attach exactly as many edges as it declared.
/// A short count means an endpoint vanished between validation and write, and
/// the caller rolls back rather than committing a partial graph.
pub fn verify_relationship_counts(
    records: &[StoredRecord],
    results: &[QueryResult],
) -> ApiResult<()> {
    let batches = rel_batches(records);
    // Node creation statements come first, one per distinct label.
    let mut labels: BTreeSet<&'static str> = BTreeSet::new();
    for rec in records {
        labels.insert(rec.kind().label());
    }
    let offset = labels.len();
    for (i, (rel_type, rows)) in batches.iter().enumerate() {
        let created = results
            .get(offset + i)
            .and_then(|r| r.col(0, "created"))
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        if created != rows.len() as i64 {
            return Err(ApiError::storage(format!(
                "relationship batch {rel_type} attached {created} of {} edges",
                rows.len()
            )));
        }
    }
    Ok(())
}
