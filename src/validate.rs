//! Deterministic domain validation. Pure over a pre-fetched `Context`, so every
//! rule in `docs/api.md` §6 is unit-testable without a database.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::Deserialize;

use crate::error::Issue;
use crate::graph::{self, Lookup, Overlay, PathError};
use crate::limits;
use crate::model::*;
use crate::util;

// ---------------------------------------------------------------- request

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedHead {
    pub project_id: String,
    pub stage: Stage,
    #[serde(default)]
    pub revision_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootChange {
    pub project_id: String,
    pub stage: Stage,
    pub after_revision_id: String,
    pub reason: String,
    pub meaningful: bool,
    pub decision: Decision,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublishRequest {
    pub project_id: String,
    pub root_revision_id: String,
    pub label: String,
    pub published_at: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Package {
    pub protocol_version: u32,
    pub idempotency_key: String,
    pub actor: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub expected_heads: Vec<ExpectedHead>,
    #[serde(default)]
    pub records: Vec<NewRecord>,
    #[serde(default)]
    pub root_change: Option<RootChange>,
    #[serde(default)]
    pub publish: Option<PublishRequest>,
}

// ---------------------------------------------------------------- context

#[derive(Debug, Clone, Default)]
pub struct HeadState {
    pub working_head: Option<String>,
    pub working_head_seq: i64,
    pub official_head: Option<String>,
    pub official_publication_id: Option<String>,
}

/// Everything validation is allowed to know about existing state. Filled by
/// `store::prefetch` inside the write transaction.
#[derive(Debug, Default)]
pub struct Context {
    pub records: BTreeMap<String, StoredRecord>,
    pub heads: BTreeMap<String, HeadState>,
    /// entity id -> its existing revisions, ascending by `(seq, id)`.
    pub entity_revisions: BTreeMap<String, Vec<(i64, String)>>,
    pub promoted_candidates: BTreeSet<String>,
    pub seq: i64,
}

impl Lookup for Context {
    fn get(&self, id: &str) -> Option<&StoredRecord> {
        self.records.get(id)
    }
}

#[derive(Debug, Clone)]
pub struct HeadUpdate {
    pub project_id: String,
    pub stage: Stage,
    pub revision_id: String,
    pub publication_id: Option<String>,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub records: Vec<StoredRecord>,
    pub head_updates: Vec<HeadUpdate>,
    pub warnings: Vec<Issue>,
    pub publication_id: Option<String>,
    /// `previous_revision_id` the server inferred for `composition` revisions.
    pub resolved_previous: BTreeMap<String, String>,
}

// ---------------------------------------------------------------- entry

/// Validate a package against existing state. `seq` is the sequence number the
/// whole batch will carry; `now` is the server record time.
pub fn validate_package(
    pkg: &Package,
    ctx: &Context,
    seq: i64,
    now: &str,
) -> Result<Plan, Vec<Issue>> {
    let mut issues = Vec::new();
    let mut plan = Plan::default();

    if pkg.records.len() > limits::MAX_PACKAGE_RECORDS {
        issues.push(Issue::new(
            "records",
            "limit_exceeded",
            format!(
                "{} records exceeds the {} per package limit",
                pkg.records.len(),
                limits::MAX_PACKAGE_RECORDS
            ),
        ));
        return Err(issues);
    }
    if pkg.idempotency_key.trim().is_empty() || pkg.idempotency_key.len() > 200 {
        issues.push(Issue::new(
            "idempotency_key",
            "invalid_field",
            "idempotency_key must be 1..200 characters",
        ));
    }
    if pkg.actor.trim().is_empty() || pkg.actor.len() > 200 {
        issues.push(Issue::new(
            "actor",
            "invalid_field",
            "actor must be 1..200 characters",
        ));
    }

    // --- shape: ids, kinds, payload schema --------------------------------
    let mut new_records: Vec<StoredRecord> = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();
    for (i, raw) in pkg.records.iter().enumerate() {
        let path = format!("records[{i}]");
        if !is_valid_id(&raw.id) {
            issues.push(Issue::new(
                format!("{path}.id"),
                "bad_id_format",
                format!("'{}' does not match the documented id format", raw.id),
            ));
            continue;
        }
        if !seen_ids.insert(raw.id.clone()) {
            issues.push(Issue::new(
                format!("{path}.id"),
                "duplicate_id",
                format!("{} appears twice in this package", raw.id),
            ));
            continue;
        }
        if ctx.records.contains_key(&raw.id) {
            issues.push(Issue::new(
                format!("{path}.id"),
                "duplicate_id",
                format!("{} already exists and records are immutable", raw.id),
            ));
            continue;
        }
        if raw.kind.is_server_only() {
            issues.push(Issue::new(
                format!("{path}.kind"),
                "forbidden_kind",
                format!(
                    "{} is written by the server, not submitted",
                    raw.kind.as_str()
                ),
            ));
            continue;
        }
        match RecordData::parse(raw.kind, raw.data.clone()) {
            Ok(data) => new_records.push(StoredRecord {
                id: raw.id.clone(),
                seq,
                recorded_at: now.to_string(),
                data,
            }),
            Err(msg) => issues.push(Issue::new(format!("{path}.data"), "bad_request", msg)),
        }
    }
    if !issues.is_empty() {
        return Err(issues);
    }

    // --- semantics --------------------------------------------------------
    let lk = Overlay::new(ctx, new_records.iter());
    let index: HashMap<&str, usize> = new_records
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();

    for (i, rec) in new_records.iter().enumerate() {
        let path = format!("records[{i}]");
        validate_record(
            rec,
            &path,
            &lk,
            ctx,
            &index,
            &new_records,
            &mut issues,
            &mut plan,
        );
    }

    detect_cycles(&new_records, &mut issues);
    detect_history_cycles(ctx, &new_records, &mut issues);
    warn_missing_goals(&new_records, &mut plan);
    let head_records = validate_root_change(pkg, ctx, &lk, seq, now, &mut issues, &mut plan);
    drop(lk);
    for rec in &mut new_records {
        if let (RecordData::Revision(revision), Some(previous)) =
            (&mut rec.data, plan.resolved_previous.get(&rec.id))
        {
            revision.previous_revision_id = Some(previous.clone());
        }
    }
    new_records.extend(head_records);

    if !issues.is_empty() {
        return Err(issues);
    }
    plan.records = new_records;
    Ok(plan)
}

// ---------------------------------------------------------------- helpers

fn need<'a>(
    lk: &'a dyn Lookup,
    issues: &mut Vec<Issue>,
    path: &str,
    id: &str,
    kinds: &[RecordKind],
) -> Option<&'a StoredRecord> {
    match lk.get(id) {
        None => {
            issues.push(Issue::new(
                path,
                "unknown_reference",
                format!("{id} not found"),
            ));
            None
        }
        Some(r) if kinds.contains(&r.kind()) => Some(r),
        Some(r) => {
            let want: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
            issues.push(Issue::new(
                path,
                "wrong_reference_kind",
                format!(
                    "{id} is a '{}' record, expected {}",
                    r.kind().as_str(),
                    want.join("|")
                ),
            ));
            None
        }
    }
}

fn check_text(issues: &mut Vec<Issue>, path: &str, value: &str, min: usize, max: usize) {
    let len = value.chars().count();
    if len < min || len > max {
        issues.push(Issue::new(
            path,
            if len > max {
                "limit_exceeded"
            } else {
                "invalid_field"
            },
            format!("length {len} is outside {min}..{max}"),
        ));
    }
}

fn check_slugs(issues: &mut Vec<Issue>, path: &str, values: &[String], max_count: usize) {
    if values.len() > max_count {
        issues.push(Issue::new(
            path,
            "limit_exceeded",
            format!("{} entries exceeds {max_count}", values.len()),
        ));
    }
    for (i, v) in values.iter().enumerate() {
        if !is_valid_slug(v) {
            issues.push(Issue::new(
                format!("{path}[{i}]"),
                "invalid_field",
                format!("'{v}' is not a valid slug"),
            ));
        }
    }
}

fn check_timestamp(issues: &mut Vec<Issue>, path: &str, value: &str) -> Option<String> {
    match util::parse_rfc3339(value) {
        Some(ts) => Some(util::to_utc_key(ts)),
        None => {
            issues.push(Issue::new(
                path,
                "timezone_required",
                format!("'{value}' is not RFC 3339 with an explicit offset"),
            ));
            None
        }
    }
}

/// The anchored slice must be exactly the claimed text. A paraphrase is
/// `inferred`, not `extracted`. `docs/api.md` §3.3.
fn check_anchor(
    lk: &dyn Lookup,
    issues: &mut Vec<Issue>,
    path: &str,
    claim_mode: ClaimMode,
    anchor: Option<&SourceAnchor>,
    capture_id: Option<&str>,
    body: &str,
) {
    if let Some(id) = capture_id {
        need(
            lk,
            issues,
            &format!("{path}.capture_id"),
            id,
            &[RecordKind::Capture],
        );
    }
    match (claim_mode, anchor) {
        (ClaimMode::Extracted, None) => issues.push(Issue::new(
            format!("{path}.source_anchor"),
            "missing_source_anchor",
            "extracted claims must anchor to a capture",
        )),
        (ClaimMode::Inferred, Some(_)) => issues.push(Issue::new(
            format!("{path}.source_anchor"),
            "inferred_must_not_anchor",
            "inferred claims must not carry a source anchor",
        )),
        (ClaimMode::Inferred, None) => {}
        (ClaimMode::Extracted, Some(a)) => {
            if let Some(declared) = capture_id {
                if declared != a.capture_id {
                    issues.push(Issue::new(
                        format!("{path}.capture_id"),
                        "source_capture_mismatch",
                        format!("{declared} differs from anchor capture {}", a.capture_id),
                    ));
                }
            }
            let anchor_path = format!("{path}.source_anchor");
            let Some(cap) = need(
                lk,
                issues,
                &anchor_path,
                &a.capture_id,
                &[RecordKind::Capture],
            ) else {
                return;
            };
            let content: Vec<char> = cap
                .as_capture()
                .map(|c| c.content.chars().collect())
                .unwrap_or_default();
            if a.start >= a.end || a.end > content.len() {
                issues.push(Issue::new(
                    anchor_path,
                    "source_anchor_out_of_range",
                    format!(
                        "[{}, {}) is not inside a capture of {} characters",
                        a.start,
                        a.end,
                        content.len()
                    ),
                ));
                return;
            }
            let slice: String = content[a.start..a.end].iter().collect();
            if slice != body {
                issues.push(Issue::new(
                    anchor_path,
                    "source_anchor_text_mismatch",
                    "anchored text differs from the claim; use claim_mode 'inferred' for a paraphrase",
                ));
            }
        }
    }
}

fn check_origin_attribution(
    issues: &mut Vec<Issue>,
    path: &str,
    origin: Origin,
    model: Option<&String>,
    skill: Option<&String>,
) {
    if origin == Origin::Ai
        && model.is_none_or(|s| s.trim().is_empty())
        && skill.is_none_or(|s| s.trim().is_empty())
    {
        issues.push(Issue::new(
            format!("{path}.model"),
            "invalid_field",
            "ai-origin records must name a model or a skill",
        ));
    }
}

// ---------------------------------------------------------------- per kind

#[allow(clippy::too_many_arguments)]
fn validate_record(
    rec: &StoredRecord,
    path: &str,
    lk: &Overlay,
    ctx: &Context,
    index: &HashMap<&str, usize>,
    new_records: &[StoredRecord],
    issues: &mut Vec<Issue>,
    plan: &mut Plan,
) {
    let d = format!("{path}.data");
    match &rec.data {
        RecordData::Project(p) => {
            check_text(issues, &format!("{d}.title"), &p.title, 1, 200);
            check_text(issues, &format!("{d}.description"), &p.description, 0, 4000);
        }

        RecordData::Entity(e) => {
            need(
                lk,
                issues,
                &format!("{d}.project_id"),
                &e.project_id,
                &[RecordKind::Project],
            );
            check_text(issues, &format!("{d}.title"), &e.title, 1, 200);
            check_slugs(issues, &format!("{d}.tags"), &e.tags, limits::MAX_TAGS);
            if e.derived_from.len() > 16 {
                issues.push(Issue::new(
                    format!("{d}.derived_from"),
                    "limit_exceeded",
                    "at most 16 lineage parents",
                ));
            }
            for (i, parent) in e.derived_from.iter().enumerate() {
                need(
                    lk,
                    issues,
                    &format!("{d}.derived_from[{i}]"),
                    parent,
                    &[RecordKind::Entity],
                );
            }
            let declared = e.lineage_kind != LineageKind::None;
            if declared != !e.derived_from.is_empty() {
                issues.push(Issue::new(
                    format!("{d}.lineage_kind"),
                    "invalid_field",
                    "lineage_kind and derived_from must both be set or both be empty",
                ));
            }
        }

        RecordData::Revision(r) => validate_revision(rec, r, &d, lk, ctx, index, issues, plan),

        RecordData::Capture(c) => {
            if let Some(p) = &c.project_id {
                need(
                    lk,
                    issues,
                    &format!("{d}.project_id"),
                    p,
                    &[RecordKind::Project],
                );
            }
            check_text(
                issues,
                &format!("{d}.content"),
                &c.content,
                1,
                limits::MAX_CAPTURE_CHARS,
            );
            check_content_digest(issues, &d, c);
            if let Some(at) = &c.occurred_at {
                check_timestamp(issues, &format!("{d}.occurred_at"), at);
            }
        }

        RecordData::Candidate(c) => {
            need(
                lk,
                issues,
                &format!("{d}.project_id"),
                &c.project_id,
                &[RecordKind::Project],
            );
            if let Some(cap) = &c.capture_id {
                need(
                    lk,
                    issues,
                    &format!("{d}.capture_id"),
                    cap,
                    &[RecordKind::Capture],
                );
            }
            check_text(issues, &format!("{d}.title"), &c.title, 1, 200);
            check_text(
                issues,
                &format!("{d}.body"),
                &c.body,
                1,
                limits::MAX_BODY_CHARS,
            );
            check_origin_attribution(issues, &d, c.origin, c.model.as_ref(), c.skill.as_ref());
            check_anchor(
                lk,
                issues,
                &d,
                c.claim_mode,
                c.source_anchor.as_ref(),
                c.capture_id.as_deref(),
                &c.body,
            );
        }

        RecordData::Promotion(p) => {
            let cand = need(
                lk,
                issues,
                &format!("{d}.candidate_id"),
                &p.candidate_id,
                &[RecordKind::Candidate],
            );
            if cand.is_some() && ctx.promoted_candidates.contains(&p.candidate_id) {
                issues.push(Issue::new(
                    format!("{d}.candidate_id"),
                    "promotion_already_exists",
                    format!("candidate {} was already promoted", p.candidate_id),
                ));
            }
            if new_records
                .iter()
                .filter(|r| {
                    matches!(&r.data, RecordData::Promotion(o) if o.candidate_id == p.candidate_id)
                })
                .count()
                > 1
            {
                issues.push(Issue::new(
                    format!("{d}.candidate_id"),
                    "promotion_already_exists",
                    "two promotions for the same candidate in one package",
                ));
            }
            need(
                lk,
                issues,
                &format!("{d}.entity_id"),
                &p.entity_id,
                &[RecordKind::Entity],
            );
            if let Some(rev) = need(
                lk,
                issues,
                &format!("{d}.revision_id"),
                &p.revision_id,
                &[RecordKind::Revision],
            ) {
                if rev.as_revision().map(|r| r.entity_id.as_str()) != Some(p.entity_id.as_str()) {
                    issues.push(Issue::new(
                        format!("{d}.revision_id"),
                        "wrong_reference_kind",
                        "promoted revision does not belong to the promoted entity",
                    ));
                }
            }
            check_text(issues, &format!("{d}.reason"), &p.reason, 1, 500);
        }

        RecordData::Artifact(a) => {
            check_text(issues, &format!("{d}.uri"), &a.uri, 1, 2000);
            check_text(issues, &format!("{d}.digest"), &a.digest, 1, 200);
            if a.digest_alg != "sha256"
                || !a.digest.strip_prefix("sha256:").is_some_and(|v| {
                    v.len() == 64
                        && v.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                issues.push(Issue::new(
                    format!("{d}.digest"),
                    "invalid_field",
                    "artifact digest must be sha256: followed by 64 lowercase hex digits",
                ));
            }
            if a.included_in_export {
                issues.push(Issue::new(
                    format!("{d}.included_in_export"),
                    "artifact_bytes_unsupported",
                    "this release stores no artifact bytes; external references are always unresolved",
                ));
            }
        }

        RecordData::Observation(o) => {
            need(
                lk,
                issues,
                &format!("{d}.project_id"),
                &o.project_id,
                &[RecordKind::Project],
            );
            if let Some(t) = &o.target_revision_id {
                need(
                    lk,
                    issues,
                    &format!("{d}.target_revision_id"),
                    t,
                    &[RecordKind::Revision],
                );
            }
            check_timestamp(issues, &format!("{d}.occurred_at"), &o.occurred_at);
            match o.status {
                ObservationStatus::Observed => {
                    if o.value
                        .as_ref()
                        .is_none_or(|v| !v.is_number() && !v.is_string() && !v.is_boolean())
                        || o.metric.as_ref().is_none_or(|m| m.trim().is_empty())
                    {
                        issues.push(Issue::new(
                            format!("{d}.value"),
                            "invalid_field",
                            "an observation needs a metric and number, text, or boolean value",
                        ));
                    }
                }
                _ => {
                    if o.value.is_some() {
                        issues.push(Issue::new(
                            format!("{d}.value"),
                            "invalid_field",
                            "only status 'observed' may carry a value",
                        ));
                    }
                }
            }
            for (i, art) in o.artifact_ids.iter().enumerate() {
                need(
                    lk,
                    issues,
                    &format!("{d}.artifact_ids[{i}]"),
                    art,
                    &[RecordKind::Artifact],
                );
            }
            if let Some(cap) = &o.capture_id {
                need(
                    lk,
                    issues,
                    &format!("{d}.capture_id"),
                    cap,
                    &[RecordKind::Capture],
                );
            }
        }

        RecordData::Goal(g) => {
            need(
                lk,
                issues,
                &format!("{d}.project_id"),
                &g.project_id,
                &[RecordKind::Project],
            );
            check_text(issues, &format!("{d}.statement"), &g.statement, 1, 2000);
            check_scope(
                lk,
                issues,
                &format!("{d}.scope"),
                &g.scope,
                "goal_scope_mismatch",
            );
            if let Some(root) = lk.get(&g.scope.root_revision_id) {
                if root.as_revision().map(|r| r.entity_id.as_str()) != Some(g.project_id.as_str()) {
                    issues.push(Issue::new(
                        format!("{d}.scope.root_revision_id"),
                        "goal_project_mismatch",
                        "goal scope must start at a revision of its own Project",
                    ));
                }
            }
            check_criteria(issues, &format!("{d}.criteria"), &g.criteria);
        }

        RecordData::Baseline(b) => validate_baseline(b, &d, lk, issues),

        RecordData::Assessment(a) => validate_assessment(a, &d, lk, issues),

        RecordData::Link(l) => validate_link(l, &d, lk, issues),

        RecordData::Embedding(e) => {
            need(
                lk,
                issues,
                &format!("{d}.revision_id"),
                &e.revision_id,
                &[RecordKind::Revision],
            );
            if e.dim != e.values.len() {
                issues.push(Issue::new(
                    format!("{d}.dim"),
                    "embedding_dim_mismatch",
                    format!("dim {} but {} values", e.dim, e.values.len()),
                ));
            }
            if e.dim == 0 || e.dim > limits::MAX_EMBEDDING_DIM {
                issues.push(Issue::new(
                    format!("{d}.dim"),
                    "limit_exceeded",
                    format!("dim must be 1..{}", limits::MAX_EMBEDDING_DIM),
                ));
            }
            check_text(issues, &format!("{d}.model"), &e.model, 1, 200);
        }

        RecordData::HeadChange(_) | RecordData::Publication(_) => {}
    }
}

fn check_content_digest(issues: &mut Vec<Issue>, d: &str, c: &CaptureData) {
    if let Some(declared) = &c.content_digest {
        let actual = util::digest_text(&c.content);
        if declared != &actual {
            issues.push(Issue::new(
                format!("{d}.content_digest"),
                "content_digest_mismatch",
                format!("declared {declared} but content hashes to {actual}"),
            ));
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_revision(
    rec: &StoredRecord,
    r: &RevisionData,
    d: &str,
    lk: &Overlay,
    ctx: &Context,
    index: &HashMap<&str, usize>,
    issues: &mut Vec<Issue>,
    plan: &mut Plan,
) {
    check_text(
        issues,
        &format!("{d}.body"),
        &r.body,
        1,
        limits::MAX_BODY_CHARS,
    );
    check_slugs(issues, &format!("{d}.tags"), &r.tags, limits::MAX_TAGS);

    let Some(owner) = need(
        lk,
        issues,
        &format!("{d}.entity_id"),
        &r.entity_id,
        &[RecordKind::Entity, RecordKind::Project],
    ) else {
        return;
    };
    let kind = match &owner.data {
        RecordData::Project(_) => NodeKind::Project,
        RecordData::Entity(e) => NodeKind::from_entity(e.entity_kind),
        _ => return,
    };

    // --- slots ---
    if r.slots.len() > limits::MAX_SLOTS {
        issues.push(Issue::new(
            format!("{d}.slots"),
            "limit_exceeded",
            format!("{} slots exceeds {}", r.slots.len(), limits::MAX_SLOTS),
        ));
    }
    let mut slot_ids = HashSet::new();
    for (i, slot) in r.slots.iter().enumerate() {
        let sp = format!("{d}.slots[{i}]");
        if !is_valid_slug(&slot.slot_id) {
            issues.push(Issue::new(
                format!("{sp}.slot_id"),
                "invalid_field",
                format!("'{}' is not a valid slug", slot.slot_id),
            ));
        }
        if !slot_ids.insert(slot.slot_id.clone()) {
            issues.push(Issue::new(
                format!("{sp}.slot_id"),
                "duplicate_slot_id",
                format!("slot '{}' appears twice", slot.slot_id),
            ));
        }
        check_slugs(
            issues,
            &format!("{sp}.roles"),
            &slot.roles,
            limits::MAX_ROLES,
        );
        if slot.revision_id == rec.id {
            issues.push(Issue::new(
                format!("{sp}.revision_id"),
                "self_reference",
                "a revision cannot contain itself",
            ));
            continue;
        }
        let Some(child) = need(
            lk,
            issues,
            &format!("{sp}.revision_id"),
            &slot.revision_id,
            &[RecordKind::Revision],
        ) else {
            continue;
        };
        let Some(child_kind) = graph::node_kind(lk, child) else {
            issues.push(Issue::new(
                format!("{sp}.revision_id"),
                "unknown_reference",
                format!("owner of revision {} not found", child.id),
            ));
            continue;
        };
        if !kind.may_contain(child_kind) {
            let code = if kind == NodeKind::Idea {
                "atom_cannot_contain"
            } else {
                "invalid_containment"
            };
            issues.push(Issue::new(
                format!("{sp}.revision_id"),
                code,
                format!(
                    "a '{}' revision may not contain a '{}' revision",
                    kind.as_str(),
                    child_kind.as_str()
                ),
            ));
            continue;
        }
        // A Schema stays inside its origin project; Core/Idea may be reused
        // across projects by the same trusted owner. `docs/api.md` §3.2.
        if child_kind == NodeKind::Schema {
            let child_origin = graph::origin_project(lk, child);
            if child_origin.as_deref() != Some(owner.id.as_str()) {
                issues.push(Issue::new(
                    format!("{sp}.revision_id"),
                    "schema_cross_project_composition",
                    format!(
                        "schema revision {} belongs to project {:?} and cannot be composed under project {}",
                        child.id, child_origin, owner.id
                    ),
                ));
            }
        }
    }

    // --- change kind ---
    let existing: &[(i64, String)] = ctx
        .entity_revisions
        .get(&r.entity_id)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let starts_in_batch = lk
        .added
        .values()
        .filter(|item| {
            item.as_revision().is_some_and(|v| {
                v.entity_id == r.entity_id
                    && matches!(v.change_kind, ChangeKind::Initial | ChangeKind::Semantic)
            })
        })
        .count();
    if starts_in_batch > 1 {
        issues.push(Issue::new(
            format!("{d}.change_kind"),
            "initial_already_exists",
            "an entity can have only one first revision, including within a batch",
        ));
    }
    match r.change_kind {
        ChangeKind::Initial => {
            if !existing.is_empty() {
                issues.push(Issue::new(
                    format!("{d}.change_kind"),
                    "initial_already_exists",
                    format!(
                        "entity {} already has {} revision(s)",
                        r.entity_id,
                        existing.len()
                    ),
                ));
            }
            if r.correction_of.is_some() || r.previous_revision_id.is_some() {
                issues.push(Issue::new(
                    format!("{d}.correction_of"),
                    "invalid_field",
                    "an initial revision has no predecessor",
                ));
            }
        }
        ChangeKind::Composition => {
            if kind == NodeKind::Idea {
                issues.push(Issue::new(
                    format!("{d}.change_kind"),
                    "composition_not_allowed_on_idea",
                    "an idea has no composition; use 'correction' or a new derived entity",
                ));
            }
            if r.correction_of.is_some() {
                issues.push(Issue::new(
                    format!("{d}.correction_of"),
                    "invalid_field",
                    "correction_of applies to change_kind 'correction' only",
                ));
            }
            match &r.previous_revision_id {
                Some(prev_id) => {
                    if let Some(prev) = need(
                        lk,
                        issues,
                        &format!("{d}.previous_revision_id"),
                        prev_id,
                        &[RecordKind::Revision],
                    ) {
                        if prev.as_revision().map(|p| p.entity_id.as_str())
                            != Some(r.entity_id.as_str())
                        {
                            issues.push(Issue::new(
                                format!("{d}.previous_revision_id"),
                                "previous_revision_mismatch",
                                format!("{prev_id} is a revision of a different entity"),
                            ));
                        }
                    }
                }
                None => match existing.last() {
                    Some((_, latest)) => {
                        plan.resolved_previous
                            .insert(rec.id.clone(), latest.clone());
                    }
                    None => issues.push(Issue::new(
                        format!("{d}.previous_revision_id"),
                        "previous_revision_mismatch",
                        format!(
                            "entity {} has no previous revision; use change_kind 'initial'",
                            r.entity_id
                        ),
                    )),
                },
            }
        }
        ChangeKind::Correction => {
            if r.previous_revision_id.is_some() {
                issues.push(Issue::new(
                    format!("{d}.previous_revision_id"),
                    "invalid_field",
                    "a correction uses correction_of only",
                ));
            }
            let Some(prev_id) = &r.correction_of else {
                issues.push(Issue::new(
                    format!("{d}.correction_of"),
                    "correction_requires_reason",
                    "a correction must name the exact revision it corrects",
                ));
                return;
            };
            match &r.correction_reason {
                Some(reason) if !reason.trim().is_empty() => {
                    check_text(issues, &format!("{d}.correction_reason"), reason, 1, 500)
                }
                _ => issues.push(Issue::new(
                    format!("{d}.correction_reason"),
                    "correction_requires_reason",
                    "a correction must carry an explicit reason",
                )),
            }
            if let Some(prev) = need(
                lk,
                issues,
                &format!("{d}.correction_of"),
                prev_id,
                &[RecordKind::Revision],
            ) {
                let prev_rev = prev.as_revision().expect("revision kind checked");
                if prev_rev.entity_id != r.entity_id {
                    issues.push(Issue::new(
                        format!("{d}.correction_of"),
                        "correction_entity_mismatch",
                        "a correction stays inside the same entity",
                    ));
                } else if util::numeric_tokens(&prev_rev.body) != util::numeric_tokens(&r.body) {
                    issues.push(Issue::new(
                        format!("{d}.body"),
                        "semantic_edit_required",
                        "the numbers changed, so this is a semantic edit: create a new derived entity",
                    ));
                } else {
                    plan.warnings.push(Issue::new(
                        format!("{d}.body"),
                        "correction_needs_human_review",
                        "the numeric guard passed; it does not prove the meaning is unchanged",
                    ));
                }
            }
        }
        ChangeKind::Semantic => {
            let created_here = index.contains_key(r.entity_id.as_str());
            let lineage_ok = owner
                .as_entity()
                .is_some_and(|e| !e.derived_from.is_empty());
            if !created_here || !lineage_ok {
                issues.push(Issue::new(
                    format!("{d}.change_kind"),
                    "semantic_requires_new_entity",
                    "a semantic edit creates a new entity in this package with derived_from set",
                ));
            }
            if r.correction_of.is_some() || r.previous_revision_id.is_some() {
                issues.push(Issue::new(
                    format!("{d}.correction_of"),
                    "invalid_field",
                    "a semantic revision starts a new entity and has no predecessor",
                ));
            }
        }
    }

    check_origin_attribution(
        issues,
        &format!("{d}.source"),
        r.source.origin,
        r.source.model.as_ref(),
        r.source.skill.as_ref(),
    );
    check_anchor(
        lk,
        issues,
        &format!("{d}.source"),
        r.source.claim_mode,
        r.source.source_anchor.as_ref(),
        r.source.capture_id.as_deref(),
        &r.body,
    );
}

fn check_scope(
    lk: &dyn Lookup,
    issues: &mut Vec<Issue>,
    path: &str,
    scope: &Scope,
    mismatch_code: &'static str,
) {
    if scope.slot_path.len() > graph::MAX_DEPTH {
        issues.push(Issue::new(
            format!("{path}.slot_path"),
            "limit_exceeded",
            format!("slot path deeper than {}", graph::MAX_DEPTH),
        ));
        return;
    }
    match graph::resolve_path(lk, &scope.root_revision_id, &scope.slot_path) {
        Ok(found) => {
            if found.id != scope.target_revision_id {
                issues.push(Issue::new(
                    format!("{path}.target_revision_id"),
                    mismatch_code,
                    format!(
                        "path {} under {} resolves to {}, not {}",
                        graph::render_path(&scope.slot_path),
                        scope.root_revision_id,
                        found.id,
                        scope.target_revision_id
                    ),
                ));
            }
        }
        Err(e) => issues.push(Issue::new(
            format!("{path}.slot_path"),
            match e {
                PathError::RootMissing | PathError::RootNotRevision => "unknown_reference",
                _ => "slot_path_not_found",
            },
            e.message(),
        )),
    }
}

fn check_criteria(issues: &mut Vec<Issue>, path: &str, criteria: &[Criterion]) {
    if criteria.is_empty() || criteria.len() > 32 {
        issues.push(Issue::new(
            path,
            "invalid_field",
            "a goal needs 1..32 criteria",
        ));
    }
    let mut ids = HashSet::new();
    for (i, c) in criteria.iter().enumerate() {
        let cp = format!("{path}[{i}]");
        if !is_valid_slug(&c.criterion_id) {
            issues.push(Issue::new(
                format!("{cp}.criterion_id"),
                "invalid_field",
                format!("'{}' is not a valid slug", c.criterion_id),
            ));
        }
        if !ids.insert(c.criterion_id.clone()) {
            issues.push(Issue::new(
                format!("{cp}.criterion_id"),
                "invalid_field",
                format!("criterion '{}' appears twice", c.criterion_id),
            ));
        }
        check_text(issues, &format!("{cp}.statement"), &c.statement, 1, 1000);
        let quantified = c.metric.is_some() && c.comparator.is_some() && c.threshold.is_some();
        match c.kind {
            CriterionKind::Quantitative if !quantified => issues.push(Issue::new(
                cp,
                "invalid_field",
                "a quantitative criterion needs metric, comparator and threshold",
            )),
            CriterionKind::Qualitative
                if c.metric.is_some() || c.comparator.is_some() || c.threshold.is_some() =>
            {
                issues.push(Issue::new(
                    cp,
                    "invalid_field",
                    "a qualitative criterion must leave metric, comparator and threshold null",
                ))
            }
            _ => {}
        }
    }
}

fn validate_baseline(b: &BaselineData, d: &str, lk: &Overlay, issues: &mut Vec<Issue>) {
    check_text(issues, &format!("{d}.reason"), &b.reason, 1, 500);
    check_timestamp(issues, &format!("{d}.effective_from"), &b.effective_from);
    let Some(goal_rec) = need(
        lk,
        issues,
        &format!("{d}.goal_id"),
        &b.goal_id,
        &[RecordKind::Goal],
    ) else {
        return;
    };
    let goal = goal_rec.as_goal().expect("goal kind checked");
    let known: HashSet<&str> = goal
        .criteria
        .iter()
        .map(|c| c.criterion_id.as_str())
        .collect();
    for (i, cid) in b.criterion_ids.iter().enumerate() {
        if !known.contains(cid.as_str()) {
            issues.push(Issue::new(
                format!("{d}.criterion_ids[{i}]"),
                "unknown_criterion",
                format!("goal {} has no criterion '{cid}'", b.goal_id),
            ));
        }
    }
    let chosen: HashSet<&str> = b.criterion_ids.iter().map(|s| s.as_str()).collect();
    for c in goal.criteria.iter().filter(|c| c.required) {
        if !chosen.contains(c.criterion_id.as_str()) {
            issues.push(Issue::new(
                format!("{d}.criterion_ids"),
                "baseline_missing_required_criterion",
                format!("required criterion '{}' is missing", c.criterion_id),
            ));
        }
    }
    // A threshold change needs a new goal, so `supersedes` may cross goals as
    // long as the occurrence scope is identical. `docs/api.md` §3.10.
    if let Some(prev_id) = &b.supersedes {
        if let Some(prev) = need(
            lk,
            issues,
            &format!("{d}.supersedes"),
            prev_id,
            &[RecordKind::Baseline],
        ) {
            let prev_goal_id = prev
                .as_baseline()
                .map(|p| p.goal_id.clone())
                .unwrap_or_default();
            match lk.get(&prev_goal_id).and_then(|g| g.as_goal()) {
                Some(prev_goal) if prev_goal.scope == goal.scope => {}
                Some(prev_goal) => issues.push(Issue::new(
                    format!("{d}.supersedes"),
                    "baseline_supersede_scope_mismatch",
                    format!(
                        "superseded goal {} is scoped to {}{} but this goal is scoped to {}{}",
                        prev_goal_id,
                        prev_goal.scope.root_revision_id,
                        graph::render_path(&prev_goal.scope.slot_path),
                        goal.scope.root_revision_id,
                        graph::render_path(&goal.scope.slot_path)
                    ),
                )),
                None => issues.push(Issue::new(
                    format!("{d}.supersedes"),
                    "unknown_reference",
                    format!("goal {prev_goal_id} of the superseded baseline not found"),
                )),
            }
        }
    }
}

fn comparator_accepts(comparator: Comparator, observed: f64, threshold: f64) -> bool {
    match comparator {
        Comparator::Lt => observed < threshold,
        Comparator::Lte => observed <= threshold,
        Comparator::Gt => observed > threshold,
        Comparator::Gte => observed >= threshold,
        Comparator::Eq => observed == threshold,
        Comparator::Ne => observed != threshold,
    }
}

fn observation_supports_quantitative_met(
    observation: &ObservationData,
    criterion: &Criterion,
    observed_value: f64,
    goal: &GoalData,
    assessment: &AssessmentData,
) -> bool {
    observation.status == ObservationStatus::Observed
        && observation.project_id == goal.project_id
        && observation.target_revision_id.as_deref() == Some(assessment.target_revision_id.as_str())
        && observation.metric.as_deref() == criterion.metric.as_deref()
        && observation.value.as_ref().and_then(|value| value.as_f64()) == Some(observed_value)
}

fn validate_assessment(a: &AssessmentData, d: &str, lk: &Overlay, issues: &mut Vec<Issue>) {
    let scope = Scope {
        root_revision_id: a.root_revision_id.clone(),
        slot_path: a.slot_path.clone(),
        target_revision_id: a.target_revision_id.clone(),
    };
    check_scope(lk, issues, d, &scope, "assessment_scope_mismatch");
    check_text(issues, &format!("{d}.evaluator"), &a.evaluator, 1, 200);
    check_text(
        issues,
        &format!("{d}.rubric_version"),
        &a.rubric_version,
        1,
        100,
    );
    let cutoff_at = check_timestamp(
        issues,
        &format!("{d}.evidence_cutoff_at"),
        &a.evidence_cutoff_at,
    );

    let Some(baseline_rec) = need(
        lk,
        issues,
        &format!("{d}.baseline_id"),
        &a.baseline_id,
        &[RecordKind::Baseline],
    ) else {
        return;
    };
    let baseline = baseline_rec.as_baseline().expect("baseline kind checked");
    let Some(goal_rec) = lk
        .get(&baseline.goal_id)
        .filter(|g| g.kind() == RecordKind::Goal)
    else {
        issues.push(Issue::new(
            format!("{d}.baseline_id"),
            "unknown_reference",
            format!("goal {} of the baseline not found", baseline.goal_id),
        ));
        return;
    };
    let goal = goal_rec.as_goal().expect("goal kind checked");
    if goal.scope != scope {
        issues.push(Issue::new(
            format!("{d}.baseline_id"),
            "baseline_scope_mismatch",
            format!(
                "baseline goal is scoped to {}{} -> {}, the assessment claims {}{} -> {}",
                goal.scope.root_revision_id,
                graph::render_path(&goal.scope.slot_path),
                goal.scope.target_revision_id,
                scope.root_revision_id,
                graph::render_path(&scope.slot_path),
                scope.target_revision_id
            ),
        ));
    }
    if a.origin == ProposalOrigin::Official && goal.origin == ProposalOrigin::AiProposed {
        issues.push(Issue::new(
            format!("{d}.origin"),
            "official_assessment_from_proposed_goal",
            "an ai-proposed goal cannot back an official assessment",
        ));
    }

    let allowed: HashSet<&str> = baseline.criterion_ids.iter().map(|s| s.as_str()).collect();
    let mut covered = HashSet::new();
    for (i, res) in a.criteria_results.iter().enumerate() {
        if !allowed.contains(res.criterion_id.as_str()) {
            issues.push(Issue::new(
                format!("{d}.criteria_results[{i}].criterion_id"),
                "unknown_criterion",
                format!(
                    "baseline {} does not adopt '{}'",
                    a.baseline_id, res.criterion_id
                ),
            ));
        }
        if !covered.insert(res.criterion_id.as_str()) {
            issues.push(Issue::new(
                format!("{d}.criteria_results[{i}].criterion_id"),
                "duplicate_criterion_result",
                "a criterion can have only one result per assessment",
            ));
        }

        let Some(estimate) = &res.progress_estimate else {
            continue;
        };
        let estimate_path = format!("{d}.criteria_results[{i}].progress_estimate");
        if a.origin != ProposalOrigin::AiProposed {
            issues.push(Issue::new(
                estimate_path.clone(),
                "official_progress_estimate",
                "progress estimates are allowed only on ai_proposed assessments",
            ));
        }
        if !estimate.percent.is_finite() || !(0.0..=100.0).contains(&estimate.percent) {
            issues.push(Issue::new(
                format!("{estimate_path}.percent"),
                "invalid_progress_estimate",
                "progress estimate percent must be finite and between 0 and 100",
            ));
        }
        if a.rubric_version == "goal-progress-milestones-v1"
            && ![0.0, 20.0, 40.0, 60.0, 80.0, 100.0].contains(&estimate.percent)
        {
            issues.push(Issue::new(
                format!("{estimate_path}.percent"),
                "invalid_progress_milestone",
                "goal-progress-milestones-v1 permits only 0, 20, 40, 60, 80, or 100",
            ));
        }
        check_text(
            issues,
            &format!("{estimate_path}.rationale"),
            &estimate.rationale,
            1,
            2000,
        );
        if estimate.rationale.trim().is_empty() {
            issues.push(Issue::new(
                format!("{estimate_path}.rationale"),
                "invalid_progress_estimate",
                "progress estimate rationale must contain non-whitespace text",
            ));
        }
        if estimate.evidence_record_ids.is_empty() {
            issues.push(Issue::new(
                format!("{estimate_path}.evidence_record_ids"),
                "progress_estimate_missing_evidence",
                "progress estimates need at least one evidence record",
            ));
        }
        if estimate.evidence_record_ids.len() > 64 {
            issues.push(Issue::new(
                format!("{estimate_path}.evidence_record_ids"),
                "limit_exceeded",
                "progress estimate evidence exceeds 64 records",
            ));
        }
        let mut evidence_ids = HashSet::new();
        for (j, evidence_id) in estimate.evidence_record_ids.iter().enumerate() {
            let evidence_path = format!("{estimate_path}.evidence_record_ids[{j}]");
            if !evidence_ids.insert(evidence_id.as_str()) {
                issues.push(Issue::new(
                    evidence_path.clone(),
                    "duplicate_evidence_record",
                    "a progress estimate may cite an evidence record only once",
                ));
            }
            let Some(evidence) = need(
                lk,
                issues,
                &evidence_path,
                evidence_id,
                &[
                    RecordKind::Capture,
                    RecordKind::Revision,
                    RecordKind::Observation,
                ],
            ) else {
                continue;
            };
            let belongs = match &evidence.data {
                RecordData::Capture(capture) => {
                    capture.project_id.as_deref() == Some(goal.project_id.as_str())
                }
                RecordData::Revision(_) => {
                    graph::origin_project(lk, evidence).as_deref() == Some(goal.project_id.as_str())
                }
                RecordData::Observation(observation) => observation.project_id == goal.project_id,
                _ => false,
            };
            if !belongs {
                issues.push(Issue::new(
                    evidence_path.clone(),
                    "progress_evidence_project_mismatch",
                    "progress estimate evidence must belong to the assessed Project",
                ));
            }
            if evidence.seq > a.evidence_cutoff_seq {
                issues.push(Issue::new(
                    evidence_path.clone(),
                    "evidence_after_cutoff",
                    format!(
                        "evidence was recorded at seq {}, past cutoff {}",
                        evidence.seq, a.evidence_cutoff_seq
                    ),
                ));
            }
            if let (Some(cutoff), Some(recorded)) =
                (&cutoff_at, util::parse_rfc3339(&evidence.recorded_at))
            {
                if &util::to_utc_key(recorded) > cutoff {
                    issues.push(Issue::new(
                        evidence_path.clone(),
                        "evidence_after_cutoff",
                        format!(
                            "evidence was recorded at {}, past cutoff {}",
                            evidence.recorded_at, a.evidence_cutoff_at
                        ),
                    ));
                }
            }
            if let (Some(cutoff), Some(observation)) = (&cutoff_at, evidence.as_observation()) {
                if let Some(occurred) = util::parse_rfc3339(&observation.occurred_at) {
                    if &util::to_utc_key(occurred) > cutoff {
                        issues.push(Issue::new(
                            evidence_path,
                            "evidence_after_cutoff",
                            format!(
                                "observation occurred at {}, past cutoff {}",
                                observation.occurred_at, a.evidence_cutoff_at
                            ),
                        ));
                    }
                }
            }
        }
    }
    for c in goal.criteria.iter().filter(|c| c.required) {
        if allowed.contains(c.criterion_id.as_str()) && !covered.contains(c.criterion_id.as_str()) {
            issues.push(Issue::new(
                format!("{d}.criteria_results"),
                "assessment_missing_required_criterion",
                format!("required criterion '{}' has no result", c.criterion_id),
            ));
        }
    }

    if a.status == JudgeStatus::Met {
        let required: Vec<_> = goal
            .criteria
            .iter()
            .filter(|c| c.required && allowed.contains(c.criterion_id.as_str()))
            .collect();
        if required.is_empty()
            || required.iter().any(|c| {
                !a.criteria_results
                    .iter()
                    .any(|r| r.criterion_id == c.criterion_id && r.status == JudgeStatus::Met)
            })
        {
            issues.push(Issue::new(
                format!("{d}.status"),
                "assessment_status_mismatch",
                "met requires at least one required criterion and all required criteria met",
            ));
        }
        if a.evidence_observation_ids.is_empty() && a.origin == ProposalOrigin::Official {
            issues.push(Issue::new(
                format!("{d}.evidence_observation_ids"),
                "assessment_missing_evidence",
                "official met assessments need scoped observations",
            ));
        }
        if a.origin == ProposalOrigin::Official && !a.evidence_observation_ids.is_empty() {
            let evidence = a
                .evidence_observation_ids
                .iter()
                .filter_map(|id| lk.get(id).and_then(StoredRecord::as_observation))
                .collect::<Vec<_>>();
            if !evidence.iter().any(|observation| {
                observation.status == ObservationStatus::Observed
                    && observation.project_id == goal.project_id
                    && observation.target_revision_id.as_deref()
                        == Some(a.target_revision_id.as_str())
            }) {
                issues.push(Issue::new(
                    format!("{d}.evidence_observation_ids"),
                    "assessment_missing_evidence",
                    "official met needs at least one observed, scoped evidence value; failed, invalid, not_observed, and negative records are context only",
                ));
            }
            for criterion in required
                .iter()
                .copied()
                .filter(|criterion| criterion.kind == CriterionKind::Quantitative)
            {
                let Some(result) = a.criteria_results.iter().find(|result| {
                    result.criterion_id == criterion.criterion_id
                        && result.status == JudgeStatus::Met
                }) else {
                    continue;
                };
                let Some(observed_value) = result.observed_value else {
                    issues.push(Issue::new(
                        format!("{d}.criteria_results"),
                        "assessment_status_mismatch",
                        format!(
                            "quantitative met criterion '{}' needs observed_value",
                            criterion.criterion_id
                        ),
                    ));
                    continue;
                };
                if criterion.comparator.zip(criterion.threshold).is_none_or(
                    |(comparator, threshold)| {
                        !comparator_accepts(comparator, observed_value, threshold)
                    },
                ) {
                    issues.push(Issue::new(
                        format!("{d}.criteria_results"),
                        "assessment_status_mismatch",
                        format!(
                            "observed_value {observed_value} contradicts the comparator for met criterion '{}'",
                            criterion.criterion_id
                        ),
                    ));
                }
                if !evidence.iter().any(|observation| {
                    observation_supports_quantitative_met(
                        observation,
                        criterion,
                        observed_value,
                        goal,
                        a,
                    )
                }) {
                    issues.push(Issue::new(
                        format!("{d}.evidence_observation_ids"),
                        "assessment_missing_evidence",
                        format!(
                            "met criterion '{}' needs an observed evidence value with the same metric and value",
                            criterion.criterion_id
                        ),
                    ));
                }
            }
        }
    }

    for (i, obs_id) in a.evidence_observation_ids.iter().enumerate() {
        let op = format!("{d}.evidence_observation_ids[{i}]");
        let Some(obs) = need(lk, issues, &op, obs_id, &[RecordKind::Observation]) else {
            continue;
        };
        if let Some(o) = obs.as_observation() {
            if o.project_id != goal.project_id
                || o.target_revision_id.as_deref() != Some(a.target_revision_id.as_str())
            {
                issues.push(Issue::new(op.clone(), "evidence_scope_mismatch", "evidence must belong to the assessed Project and exact target revision; indirect reuse needs a new applicability observation"));
            }
        }
        if obs.seq > a.evidence_cutoff_seq {
            issues.push(Issue::new(
                op.clone(),
                "evidence_after_cutoff",
                format!(
                    "observation was recorded at seq {}, past cutoff {}",
                    obs.seq, a.evidence_cutoff_seq
                ),
            ));
        }
        if let (Some(cutoff), Some(o)) = (&cutoff_at, obs.as_observation()) {
            if let Some(occurred) = util::parse_rfc3339(&o.occurred_at) {
                if &util::to_utc_key(occurred) > cutoff {
                    issues.push(Issue::new(
                        op,
                        "evidence_after_cutoff",
                        format!(
                            "observation occurred at {}, past cutoff {}",
                            o.occurred_at, a.evidence_cutoff_at
                        ),
                    ));
                }
            }
        }
    }
}

fn validate_link(l: &LinkData, d: &str, lk: &Overlay, issues: &mut Vec<Issue>) {
    use RecordKind as K;
    let (from_kinds, to_kinds): (&[K], &[K]) = match l.link_type {
        LinkType::Derived | LinkType::Applies | LinkType::Implements | LinkType::Depends => {
            (&[K::Entity], &[K::Entity])
        }
        LinkType::Similar => (&[K::Revision], &[K::Revision]),
        LinkType::Support | LinkType::Contradict => (
            &[K::Observation, K::Capture, K::Revision],
            &[K::Revision, K::Assessment, K::Goal],
        ),
        LinkType::Impact => (&[K::Entity, K::Revision], &[K::Goal]),
    };
    for (field, id, kinds) in [
        ("from_id", &l.from_id, from_kinds),
        ("to_id", &l.to_id, to_kinds),
    ] {
        match lk.get(id) {
            None => issues.push(Issue::new(
                format!("{d}.{field}"),
                "unknown_reference",
                format!("{id} not found"),
            )),
            Some(r) if !kinds.contains(&r.kind()) => {
                let want: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
                issues.push(Issue::new(
                    format!("{d}.{field}"),
                    "invalid_link_endpoints",
                    format!(
                        "link_type '{}' does not accept a '{}' record here, expected {}",
                        l.link_type.rel_type().to_lowercase(),
                        r.kind().as_str(),
                        want.join("|")
                    ),
                ));
            }
            Some(_) => {}
        }
    }

    match (l.link_type, &l.impact) {
        (LinkType::Impact, None) => issues.push(Issue::new(
            format!("{d}.impact"),
            "invalid_field",
            "an impact link must carry the impact claim",
        )),
        (LinkType::Impact, Some(im)) => {
            check_text(issues, &format!("{d}.impact.scope"), &im.scope, 1, 500);
            match im.evidence_kind {
                EvidenceKind::Observed => {
                    if im.observation_ids.is_empty() {
                        issues.push(Issue::new(
                            format!("{d}.impact.observation_ids"),
                            "impact_evidence_mismatch",
                            "an observed impact must cite at least one observation",
                        ));
                    }
                    for (i, obs) in im.observation_ids.iter().enumerate() {
                        need(
                            lk,
                            issues,
                            &format!("{d}.impact.observation_ids[{i}]"),
                            obs,
                            &[RecordKind::Observation],
                        );
                    }
                }
                EvidenceKind::Estimated => {
                    if !im.observation_ids.is_empty() {
                        issues.push(Issue::new(
                            format!("{d}.impact.observation_ids"),
                            "impact_evidence_mismatch",
                            "an estimated impact must not cite observations; use evidence_kind 'observed'",
                        ));
                    }
                    if im.method.as_ref().is_none_or(|m| m.trim().is_empty()) {
                        issues.push(Issue::new(
                            format!("{d}.impact.method"),
                            "impact_evidence_mismatch",
                            "an estimated impact must state how it was estimated",
                        ));
                    }
                }
            }
        }
        (_, Some(_)) => issues.push(Issue::new(
            format!("{d}.impact"),
            "invalid_field",
            "only an impact link carries an impact claim",
        )),
        (_, None) => {}
    }

    match (l.link_type, &l.similarity) {
        (LinkType::Similar, None) => issues.push(Issue::new(
            format!("{d}.similarity"),
            "invalid_field",
            "a similarity link must state its score and method",
        )),
        (LinkType::Similar, Some(s)) => {
            if !(-1.0..=1.0).contains(&s.score) {
                issues.push(Issue::new(
                    format!("{d}.similarity.score"),
                    "invalid_field",
                    "score must be within [-1, 1]",
                ));
            }
            check_text(issues, &format!("{d}.similarity.method"), &s.method, 1, 200);
        }
        (_, Some(_)) => issues.push(Issue::new(
            format!("{d}.similarity"),
            "invalid_field",
            "only a similarity link carries a similarity score",
        )),
        (_, None) => {}
    }
}

/// Existing records are immutable and cannot reference ids created now, so a
/// cycle can only exist among the package's own revisions.
fn detect_cycles(new_records: &[StoredRecord], issues: &mut Vec<Issue>) {
    let ids: HashMap<&str, usize> = new_records
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id.as_str(), i))
        .collect();
    let mut state = vec![0u8; new_records.len()]; // 0 unvisited, 1 on stack, 2 done
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for start in 0..new_records.len() {
        if state[start] != 0 || new_records[start].as_revision().is_none() {
            continue;
        }
        stack.push((start, 0));
        state[start] = 1;
        while let Some((node, child_ix)) = stack.pop() {
            let slots = new_records[node]
                .as_revision()
                .map(|r| r.slots.as_slice())
                .unwrap_or(&[]);
            if child_ix >= slots.len() {
                state[node] = 2;
                continue;
            }
            stack.push((node, child_ix + 1));
            let Some(&next) = ids.get(slots[child_ix].revision_id.as_str()) else {
                continue; // points at an existing (already acyclic) revision
            };
            match state[next] {
                1 => {
                    issues.push(Issue::new(
                        format!("records[{next}].data.slots"),
                        "cycle_detected",
                        format!(
                            "{} and {} form a containment cycle",
                            new_records[node].id, new_records[next].id
                        ),
                    ));
                    state[next] = 2;
                }
                0 => {
                    state[next] = 1;
                    stack.push((next, 0));
                }
                _ => {}
            }
        }
    }
}

/// Chronological lineage and predecessor links cannot loop, including links
/// added between already-existing entities. They are separate from containment.
fn detect_history_cycles(ctx: &Context, added: &[StoredRecord], issues: &mut Vec<Issue>) {
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for rec in ctx.records.values().chain(added.iter()) {
        match &rec.data {
            RecordData::Entity(e) => {
                edges
                    .entry(&rec.id)
                    .or_default()
                    .extend(e.derived_from.iter().map(String::as_str));
            }
            RecordData::Revision(r) => {
                edges.entry(&rec.id).or_default().extend(
                    r.correction_of
                        .iter()
                        .chain(r.previous_revision_id.iter())
                        .map(String::as_str),
                );
            }
            RecordData::Baseline(b) => {
                edges
                    .entry(&rec.id)
                    .or_default()
                    .extend(b.supersedes.iter().map(String::as_str));
            }
            RecordData::Link(l) if l.link_type == LinkType::Derived => {
                edges.entry(&l.from_id).or_default().push(&l.to_id);
            }
            _ => {}
        }
    }
    let mut done = HashSet::new();
    let mut visiting = HashSet::new();
    for start in edges.keys() {
        let mut stack = vec![(*start, false)];
        while let Some((id, leaving)) = stack.pop() {
            if leaving {
                visiting.remove(id);
                done.insert(id);
            } else if !done.contains(id) {
                if !visiting.insert(id) {
                    issues.push(Issue::new(
                        "records",
                        "history_cycle_detected",
                        format!("lineage or predecessor cycle at {id}"),
                    ));
                    return;
                }
                stack.push((id, true));
                if let Some(parents) = edges.get(id) {
                    stack.extend(parents.iter().rev().map(|p| (*p, false)));
                }
            }
        }
    }
}

/// Returns the server-written `head_change` / `publication` records.
#[allow(clippy::too_many_arguments)]
fn validate_root_change(
    pkg: &Package,
    ctx: &Context,
    lk: &Overlay,
    seq: i64,
    now: &str,
    issues: &mut Vec<Issue>,
    plan: &mut Plan,
) -> Vec<StoredRecord> {
    let mut new_records: Vec<StoredRecord> = Vec::new();
    if let Some(rc) = &pkg.root_change {
        let path = "root_change";
        if rc.stage != Stage::Working {
            issues.push(Issue::new(
                format!("{path}.stage"),
                "invalid_field",
                "root_change moves the working head; use 'publish' for the official stream",
            ));
        }
        // Every root change records why. No heuristic decides that a change was
        // too small to explain. `docs/api.md` §5.8.
        if !rc.meaningful || rc.reason.trim().is_empty() || rc.decision.rationale.trim().is_empty()
        {
            issues.push(Issue::new(
                format!("{path}.decision"),
                "root_change_requires_decision",
                "every root change needs meaningful=true, a reason and a decision rationale",
            ));
        }
        check_text(issues, &format!("{path}.reason"), &rc.reason, 0, 500);
        check_text(
            issues,
            &format!("{path}.decision.rationale"),
            &rc.decision.rationale,
            0,
            1000,
        );
        need(
            lk,
            issues,
            &format!("{path}.project_id"),
            &rc.project_id,
            &[RecordKind::Project],
        );
        check_head_revision(
            lk,
            issues,
            &format!("{path}.after_revision_id"),
            &rc.project_id,
            &rc.after_revision_id,
        );
        require_expected_head(pkg, issues, path, &rc.project_id, Stage::Working);

        let before = ctx
            .heads
            .get(&rc.project_id)
            .and_then(|h| h.working_head.clone());
        let id = util::derived_id("hc", &format!("{}:{}", pkg.idempotency_key, rc.project_id));
        new_records.push(StoredRecord {
            id: id.clone(),
            seq,
            recorded_at: now.to_string(),
            data: RecordData::HeadChange(HeadChangeData {
                project_id: rc.project_id.clone(),
                stage: Stage::Working,
                before_revision_id: before,
                after_revision_id: rc.after_revision_id.clone(),
                reason: rc.reason.clone(),
                actor: pkg.actor.clone(),
                meaningful: true,
                decision: rc.decision.clone(),
            }),
        });
        plan.head_updates.push(HeadUpdate {
            project_id: rc.project_id.clone(),
            stage: Stage::Working,
            revision_id: rc.after_revision_id.clone(),
            publication_id: None,
        });
    }

    if let Some(pb) = &pkg.publish {
        let path = "publish";
        need(
            lk,
            issues,
            &format!("{path}.project_id"),
            &pb.project_id,
            &[RecordKind::Project],
        );
        check_head_revision(
            lk,
            issues,
            &format!("{path}.root_revision_id"),
            &pb.project_id,
            &pb.root_revision_id,
        );
        check_text(issues, &format!("{path}.label"), &pb.label, 1, 100);
        check_timestamp(issues, &format!("{path}.published_at"), &pb.published_at);
        require_expected_head(pkg, issues, path, &pb.project_id, Stage::Official);

        let id = util::derived_id("pub", &format!("{}:{}", pkg.idempotency_key, pb.project_id));
        new_records.push(StoredRecord {
            id: id.clone(),
            seq,
            recorded_at: now.to_string(),
            data: RecordData::Publication(PublicationData {
                project_id: pb.project_id.clone(),
                root_revision_id: pb.root_revision_id.clone(),
                label: pb.label.clone(),
                published_at: pb.published_at.clone(),
                notes: pb.notes.clone(),
                actor: pkg.actor.clone(),
            }),
        });
        plan.publication_id = Some(id.clone());
        plan.head_updates.push(HeadUpdate {
            project_id: pb.project_id.clone(),
            stage: Stage::Official,
            revision_id: pb.root_revision_id.clone(),
            publication_id: Some(id),
        });
    }
    new_records
}

fn require_expected_head(
    pkg: &Package,
    issues: &mut Vec<Issue>,
    path: &str,
    project_id: &str,
    stage: Stage,
) {
    let present = pkg
        .expected_heads
        .iter()
        .any(|h| h.project_id == project_id && h.stage == stage);
    if !present {
        issues.push(Issue::new(
            format!("{path}.project_id"),
            "missing_expected_head",
            format!(
                "expected_heads must include ({project_id}, {})",
                stage.as_str()
            ),
        ));
    }
}

/// The head is the Project's own composition revision, containing its Schemas.
fn check_head_revision(
    lk: &dyn Lookup,
    issues: &mut Vec<Issue>,
    path: &str,
    project_id: &str,
    revision_id: &str,
) {
    let Some(rev) = need(lk, issues, path, revision_id, &[RecordKind::Revision]) else {
        return;
    };
    if rev
        .as_revision()
        .is_none_or(|data| data.entity_id != project_id)
    {
        issues.push(Issue::new(
            path,
            "invalid_head_revision",
            format!("{revision_id} must be a revision of Project {project_id} itself"),
        ));
    }
}

/// A goal is optional; the dashboard reports the gap. Warn at write time so the
/// author sees it without being blocked.
fn warn_missing_goals(new_records: &[StoredRecord], plan: &mut Plan) {
    let goal_targets: HashSet<&str> = new_records
        .iter()
        .filter_map(|r| r.as_goal())
        .map(|g| g.scope.target_revision_id.as_str())
        .collect();
    for (i, rec) in new_records.iter().enumerate() {
        let Some(rev) = rec.as_revision() else {
            continue;
        };
        let structural = new_records.iter().any(|e| {
            e.id == rev.entity_id
                && e.as_entity()
                    .is_some_and(|d| matches!(d.entity_kind, EntityKind::Core | EntityKind::Schema))
        });
        if structural && !goal_targets.contains(rec.id.as_str()) {
            plan.warnings.push(Issue::new(
                format!("records[{i}]"),
                "missing_goal",
                format!(
                    "new structural revision {} has no goal in this package",
                    rec.id
                ),
            ));
        }
    }
}

#[cfg(test)]
mod tests;
