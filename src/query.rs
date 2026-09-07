//! Pure API read projections over one transaction-consistent Context.
use crate::{
    error::{ApiError, ApiResult},
    graph::{self, Limits},
    limits,
    model::*,
    util,
    validate::Context,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use unicode_normalization::UnicodeNormalization;

pub type Params = BTreeMap<String, String>;
fn bad(s: impl Into<String>) -> ApiError {
    ApiError::bad_request(s)
}
fn check(params: &Params, keys: &[&str]) -> ApiResult<()> {
    if let Some(k) = params.keys().find(|k| !keys.contains(&k.as_str())) {
        Err(bad(format!("unknown query parameter '{k}'")))
    } else {
        Ok(())
    }
}
fn num(params: &Params, key: &str, default: usize, max: usize) -> ApiResult<usize> {
    match params.get(key) {
        None => Ok(default),
        Some(v) => v
            .parse::<usize>()
            .ok()
            .filter(|x| *x > 0 && *x <= max)
            .ok_or_else(|| bad(format!("{key} must be 1..={max}"))),
    }
}
fn stage(params: &Params) -> ApiResult<Stage> {
    match params.get("stage").map(String::as_str).unwrap_or("working") {
        "working" => Ok(Stage::Working),
        "official" => Ok(Stage::Official),
        _ => Err(bad("stage must be working or official")),
    }
}
fn time(v: &str, name: &str) -> ApiResult<String> {
    util::parse_rfc3339(v)
        .map(util::to_utc_key)
        .ok_or_else(|| bad(format!("{name} must be RFC 3339 with offset")))
}

struct View {
    ctx: Context,
    known: i64,
    known_at: Option<String>,
    effective: Option<String>,
}
fn view(ctx: &Context, p: &Params) -> ApiResult<View> {
    if p.contains_key("known_seq") && p.contains_key("known_at") {
        return Err(bad("known_seq and known_at are mutually exclusive"));
    }
    let known_at = p.get("known_at").map(|v| time(v, "known_at")).transpose()?;
    let effective = p
        .get("effective_at")
        .map(|v| time(v, "effective_at"))
        .transpose()?;
    let known = match p.get("known_seq") {
        Some(v) => v
            .parse::<i64>()
            .ok()
            .filter(|n| *n >= 0 && *n <= ctx.seq)
            .ok_or_else(|| bad("known_seq must be 0..=current sequence"))?,
        None => known_at.as_ref().map_or(ctx.seq, |cut| {
            ctx.records
                .values()
                .filter(|r| time(&r.recorded_at, "recorded_at").is_ok_and(|x| x <= *cut))
                .map(|r| r.seq)
                .max()
                .unwrap_or(0)
        }),
    };
    Ok(View {
        ctx: Context {
            records: ctx
                .records
                .iter()
                .filter(|(_, r)| r.seq <= known)
                .map(|(id, r)| (id.clone(), r.clone()))
                .collect(),
            seq: known,
            ..Default::default()
        },
        known,
        known_at,
        effective,
    })
}
fn project_of(r: &StoredRecord, rs: &BTreeMap<String, StoredRecord>) -> Option<String> {
    match &r.data {
        RecordData::Project(_) => Some(r.id.clone()),
        RecordData::Entity(d) => Some(d.project_id.clone()),
        RecordData::Revision(d) => rs.get(&d.entity_id).and_then(|x| project_of(x, rs)),
        RecordData::Capture(d) => d.project_id.clone(),
        RecordData::Candidate(d) => Some(d.project_id.clone()),
        RecordData::Observation(d) => Some(d.project_id.clone()),
        RecordData::Goal(d) => Some(d.project_id.clone()),
        RecordData::HeadChange(d) => Some(d.project_id.clone()),
        RecordData::Publication(d) => Some(d.project_id.clone()),
        _ => None,
    }
}
fn belongs_to_project(
    r: &StoredRecord,
    project: &str,
    rs: &BTreeMap<String, StoredRecord>,
) -> bool {
    if let Some(link) = r.as_link() {
        return [link.from_id.as_str(), link.to_id.as_str()]
            .into_iter()
            .any(|id| {
                rs.get(id)
                    .and_then(|endpoint| project_of(endpoint, rs))
                    .as_deref()
                    == Some(project)
            });
    }
    project_of(r, rs).as_deref() == Some(project)
}
fn root(v: &View, project: &str, stage: Stage) -> Option<(String, Value)> {
    if stage == Stage::Working {
        v.ctx
            .records
            .values()
            .filter_map(|r| r.as_head_change().map(|d| (r, d)))
            .filter(|(_, d)| d.project_id == project && d.stage == Stage::Working)
            .max_by_key(|(r, _)| r.seq)
            .map(|(r, d)| (d.after_revision_id.clone(), json!({"head_change_id":r.id})))
    } else {
        v.ctx
            .records
            .values()
            .filter_map(|r| r.as_publication().map(|d| (r, d)))
            .filter(|(_, d)| d.project_id == project)
            .filter(|(_, d)| {
                v.effective
                    .as_ref()
                    .is_none_or(|x| time(&d.published_at, "published_at").is_ok_and(|t| t <= *x))
            })
            .max_by(|(a, ad), (b, bd)| {
                (
                    time(&ad.published_at, "published_at").unwrap_or_default(),
                    a.seq,
                    &a.id,
                )
                    .cmp(&(
                        time(&bd.published_at, "published_at").unwrap_or_default(),
                        b.seq,
                        &b.id,
                    ))
            })
            .map(|(r, d)| (d.root_revision_id.clone(), json!({"publication_id":r.id})))
    }
}
fn valid_root(v: &View, project: &str, id: &str) -> bool {
    v.ctx
        .records
        .get(id)
        .and_then(StoredRecord::as_revision)
        .is_some_and(|d| d.entity_id == project)
}
fn selected(v: &View, p: &Params, s: Stage, project: &str) -> ApiResult<(Option<String>, Value)> {
    let pair = p
        .get("root_revision_id")
        .map(|x| (x.clone(), json!({})))
        .or_else(|| root(v, project, s));
    let mut meta = json!({"known_seq":v.known,"known_at":v.known_at,"effective_at":v.effective,"head_change_id":null,"publication_id":null});
    match pair {
        None => Ok((None, meta)),
        Some((id, extra)) => {
            if !valid_root(v, project, &id) {
                return Err(bad(
                    "root_revision_id must be this project's project-root revision",
                ));
            }
            for (k, x) in extra.as_object().unwrap() {
                meta[k] = x.clone()
            }
            Ok((Some(id), meta))
        }
    }
}
fn owner(rs: &BTreeMap<String, StoredRecord>, r: &StoredRecord) -> (String, String, String) {
    let d = r.as_revision().unwrap();
    match rs.get(&d.entity_id) {
        Some(x) => match &x.data {
            RecordData::Project(p) => (x.id.clone(), "project".into(), p.title.clone()),
            RecordData::Entity(e) => (
                x.id.clone(),
                NodeKind::from_entity(e.entity_kind).as_str().into(),
                e.title.clone(),
            ),
            _ => (d.entity_id.clone(), "unknown".into(), d.entity_id.clone()),
        },
        None => (d.entity_id.clone(), "unknown".into(), d.entity_id.clone()),
    }
}
fn short(s: &str, n: usize) -> String {
    let mut c = s.chars();
    let x: String = c.by_ref().take(n).collect();
    if c.next().is_some() {
        format!("{x}…")
    } else {
        x
    }
}
fn record_json(r: &StoredRecord) -> Value {
    r.to_json()
}

pub fn state(ctx: &Context, p: &Params) -> ApiResult<Value> {
    check(p, &["project_id", "kinds", "limit", "offset", "max_seq"])?;
    let mut q = p.clone();
    if let Some(x) = p.get("max_seq") {
        q.insert("known_seq".into(), x.clone());
    };
    let v = view(ctx, &q)?;
    let limit = num(
        p,
        "limit",
        limits::DEFAULT_LIST_LIMIT,
        limits::MAX_LIST_LIMIT,
    )?;
    let offset = p
        .get("offset")
        .map(|x| x.parse().map_err(|_| bad("offset must be non-negative")))
        .transpose()?
        .unwrap_or(0);
    let kinds: Option<Vec<RecordKind>> = p
        .get("kinds")
        .map(|x| {
            x.split(',')
                .map(|k| {
                    RecordKind::parse_str(k)
                        .ok_or_else(|| bad(format!("unknown record kind '{k}'")))
                })
                .collect()
        })
        .transpose()?;
    let project = p.get("project_id");
    let mut rows: Vec<_> = v
        .ctx
        .records
        .values()
        .filter(|r| {
            project.is_none_or(|x| belongs_to_project(r, x, &v.ctx.records))
                && kinds.as_ref().is_none_or(|x| x.contains(&r.kind()))
        })
        .collect();
    rows.sort_by_key(|r| (r.seq, r.id.clone()));
    let total = rows.len();
    let projects = v
        .ctx
        .records
        .values()
        .filter(|record| record.kind() == RecordKind::Project)
        .map(|project_record| {
            let title = match &project_record.data {
                RecordData::Project(project) => project.title.clone(),
                _ => String::new(),
            };
            let mut counts = serde_json::Map::new();
            for kind in [
                RecordKind::Entity,
                RecordKind::Revision,
                RecordKind::Capture,
                RecordKind::Candidate,
                RecordKind::Goal,
                RecordKind::Baseline,
                RecordKind::Assessment,
                RecordKind::Observation,
                RecordKind::Link,
                RecordKind::Artifact,
                RecordKind::Embedding,
                RecordKind::Promotion,
                RecordKind::HeadChange,
                RecordKind::Publication,
            ] {
                let count = v
                    .ctx
                    .records
                    .values()
                    .filter(|record| {
                        record.kind() == kind
                            && belongs_to_project(record, &project_record.id, &v.ctx.records)
                    })
                    .count();
                counts.insert(kind.as_str().into(), json!(count));
            }
            let working_head = v
                .ctx
                .records
                .values()
                .filter_map(|record| {
                    record.as_head_change().filter(|change| {
                        change.project_id == project_record.id && change.stage == Stage::Working
                    })
                    .map(|change| (record.seq, change.after_revision_id.clone()))
                })
                .max();
            let official_publication = v
                .ctx
                .records
                .values()
                .filter_map(|record| {
                    record.as_publication().filter(|publication| {
                        publication.project_id == project_record.id
                    })
                    .map(|publication| {
                        (
                            time(&publication.published_at, "published_at").unwrap_or_default(),
                            record.seq,
                            record.id.clone(),
                            publication.root_revision_id.clone(),
                        )
                    })
                })
                .max();
            json!({
                "project_id": project_record.id,
                "title": title,
                "working_head": working_head.as_ref().map(|head| &head.1),
                "working_head_seq": working_head.as_ref().map(|head| head.0).unwrap_or(0),
                "official_head": official_publication.as_ref().map(|publication| &publication.3),
                "official_publication_id": official_publication.as_ref().map(|publication| &publication.2),
                "publication_count": v.ctx.records.values().filter(|record| {
                    record.as_publication().is_some_and(|publication| {
                        publication.project_id == project_record.id
                    })
                }).count(),
                "counts": counts,
            })
        })
        .collect::<Vec<_>>();
    Ok(
        json!({"seq":v.known,"protocol_version":PROTOCOL_VERSION,"projects":projects,"records":rows.into_iter().skip(offset).take(limit).map(record_json).collect::<Vec<_>>(),"total_matched":total,"truncated":offset+limit<total}),
    )
}
pub fn captures(ctx: &Context, p: &Params) -> ApiResult<Value> {
    check(p, &["project_id", "limit", "offset", "include_content"])?;
    let project = p
        .get("project_id")
        .ok_or_else(|| bad("project_id is required"))?;
    let limit = num(
        p,
        "limit",
        limits::DEFAULT_CAPTURE_LIST_LIMIT,
        limits::MAX_CAPTURE_LIST_LIMIT,
    )?;
    let offset = p
        .get("offset")
        .map(|x| x.parse().map_err(|_| bad("offset must be non-negative")))
        .transpose()?
        .unwrap_or(0);
    let include = match p
        .get("include_content")
        .map(String::as_str)
        .unwrap_or("false")
    {
        "true" => true,
        "false" => false,
        _ => return Err(bad("include_content must be true or false")),
    };
    let mut rows: Vec<_> = ctx
        .records
        .values()
        .filter(|r| {
            r.as_capture()
                .is_some_and(|d| d.project_id.as_deref() == Some(project))
        })
        .collect();
    rows.sort_by_key(|r| (r.seq, r.id.clone()));
    let total = rows.len();
    let captures=rows.into_iter().skip(offset).take(limit).map(|r|{let d=r.as_capture().unwrap();json!({"id":r.id,"seq":r.seq,"recorded_at":r.recorded_at,"occurred_at":d.occurred_at,"source_kind":d.source_kind,"content_digest":d.content_digest,"content_length":d.content.chars().count(),"content_preview":short(&d.content,400),"content":if include{json!(d.content)}else{Value::Null},"candidate_count":ctx.records.values().filter(|x|matches!(&x.data,RecordData::Candidate(c) if c.capture_id.as_deref()==Some(&r.id))).count()})}).collect::<Vec<_>>();
    Ok(json!({"captures":captures,"total_matched":total,"truncated":offset+limit<total}))
}
pub fn snapshot(ctx: &Context, p: &Params) -> ApiResult<Value> {
    check(
        p,
        &[
            "project_id",
            "stage",
            "known_seq",
            "known_at",
            "effective_at",
            "max_nodes",
            "depth",
            "root_revision_id",
        ],
    )?;
    let project = p
        .get("project_id")
        .ok_or_else(|| bad("project_id is required"))?;
    let v = view(ctx, p)?;
    let s = stage(p)?;
    let max = num(
        p,
        "max_nodes",
        limits::DEFAULT_SNAPSHOT_NODES,
        limits::MAX_SNAPSHOT_NODES,
    )?;
    let depth = num(p, "depth", graph::MAX_DEPTH, graph::MAX_DEPTH)?;
    let (root_id, meta) = selected(&v, p, s, project)?;
    let Some(root_id) = root_id else {
        return Ok(
            json!({"project_id":project,"stage":s.as_str(),"selected_by":meta,"root_revision_id":null,"tree":null,"nodes":[],"node_count":0,"revision_count":0,"max_depth_reached":0,"truncated":false}),
        );
    };
    let (tree, node_count, truncated, deep) = graph::tree_json(
        &v.ctx,
        &root_id,
        Limits {
            max_nodes: max,
            max_depth: depth,
        },
    );
    let walk = graph::walk(
        &v.ctx,
        &root_id,
        Limits {
            max_nodes: max,
            max_depth: depth,
        },
    );
    let nodes=walk.revisions.iter().filter_map(|id|v.ctx.records.get(id)).map(|r|{let(e,k,t)=owner(&v.ctx.records,r);let d=r.as_revision().unwrap();json!({"revision_id":r.id,"entity_id":e,"entity_kind":k,"title":t,"body":d.body,"tags":d.tags,"change_kind":d.change_kind,"seq":r.seq,"recorded_at":r.recorded_at,"occurrence_count":walk.occurrences.iter().filter(|o|o.revision_id==r.id).count()})}).collect::<Vec<_>>();
    Ok(
        json!({"project_id":project,"stage":s.as_str(),"selected_by":meta,"root_revision_id":root_id,"tree":tree,"nodes":nodes,"node_count":node_count,"revision_count":walk.revisions.len(),"max_depth_reached":deep,"truncated":truncated||walk.truncated}),
    )
}
fn all_occurrences(v: &View, s: Stage, wanted: &str, project: Option<&str>) -> Vec<Value> {
    let projects: Vec<_> = project.map(|x| vec![x.to_string()]).unwrap_or_else(|| {
        v.ctx
            .records
            .values()
            .filter(|r| r.kind() == RecordKind::Project)
            .map(|r| r.id.clone())
            .collect()
    });
    let mut out = Vec::new();
    for p in projects {
        if let Some((root, _)) = root(v, &p, s) {
            if !valid_root(v, &p, &root) {
                continue;
            }
            for o in graph::walk(&v.ctx, &root, Limits::default()).occurrences {
                if o.revision_id == wanted
                    || v.ctx
                        .records
                        .get(&o.revision_id)
                        .and_then(StoredRecord::as_revision)
                        .is_some_and(|d| d.entity_id == wanted)
                {
                    out.push(json!({"root_revision_id":root,"slot_path":o.slot_path,"revision_id":o.revision_id,"roles":o.roles,"parent_revision_id":o.parent_revision_id,"depth":o.depth,"stage":s.as_str()}))
                }
            }
        }
    }
    out
}

/// A judgment cannot be projected before the observation window it evaluates.
/// Knowledge time has already filtered records in `view`; effective time must
/// also filter conclusions, otherwise a hidden observation leaks via its gate.
fn assessment_visible(v: &View, assessment: &AssessmentData) -> bool {
    v.effective.as_ref().is_none_or(|cut| {
        time(&assessment.evidence_cutoff_at, "evidence_cutoff_at").is_ok_and(|at| at <= *cut)
            && v.ctx
                .records
                .get(&assessment.baseline_id)
                .and_then(StoredRecord::as_baseline)
                .is_some_and(|baseline| {
                    time(&baseline.effective_from, "effective_from").is_ok_and(|at| at <= *cut)
                })
    })
}

pub fn record(ctx: &Context, id: &str, p: &Params) -> ApiResult<Value> {
    check(
        p,
        &["include", "stage", "known_seq", "known_at", "effective_at"],
    )?;
    let v = view(ctx, p)?;
    let r =
        v.ctx.records.get(id).ok_or_else(|| {
            ApiError::not_found(format!("record {id} not found at selected time"))
        })?;
    let s = stage(p)?;
    let parts: BTreeSet<_> = p
        .get("include")
        .map(String::as_str)
        .unwrap_or("links,lineage,evidence,occurrences")
        .split(',')
        .map(str::to_string)
        .collect();
    if parts
        .iter()
        .any(|x| !["links", "lineage", "evidence", "occurrences"].contains(&x.as_str()))
    {
        return Err(bad("include has an unknown section"));
    }
    let entity = r
        .as_revision()
        .and_then(|d| v.ctx.records.get(&d.entity_id))
        .filter(|x| x.kind() == RecordKind::Entity);
    let revisions=entity.map(|e|v.ctx.records.values().filter(|x|x.as_revision().is_some_and(|d|d.entity_id==e.id)).map(|x|{let d=x.as_revision().unwrap();json!({"id":x.id,"seq":x.seq,"change_kind":d.change_kind,"correction_of":d.correction_of,"body_preview":short(&d.body,400)})}).collect::<Vec<_>>()).unwrap_or_default();
    let lineage = if parts.contains("lineage") {
        let source=entity.and_then(StoredRecord::as_entity).map(|e|e.derived_from.iter().filter_map(|id|v.ctx.records.get(id)).map(|x|json!({"id":x.id,"title":x.as_entity().map(|e|e.title.clone()),"lineage_kind":x.as_entity().map(|e|e.lineage_kind)})).collect::<Vec<_>>()).unwrap_or_default();
        let eid = entity.map(|x| x.id.clone());
        let derives=eid.as_ref().map(|id|v.ctx.records.values().filter(|x|x.as_entity().is_some_and(|e|e.derived_from.contains(id))).map(|x|json!({"id":x.id,"title":x.as_entity().map(|e|e.title.clone()),"lineage_kind":x.as_entity().map(|e|e.lineage_kind)})).collect::<Vec<_>>()).unwrap_or_default();
        let corrections = v
            .ctx
            .records
            .values()
            .filter(|x| {
                x.as_revision()
                    .is_some_and(|d| d.correction_of.as_deref() == Some(id))
            })
            .map(record_json)
            .collect::<Vec<_>>();
        json!({"derived_from":source,"derives":derives,"corrections":corrections})
    } else {
        json!({"derived_from":[],"derives":[],"corrections":[]})
    };
    let links = if parts.contains("links") {
        let outgoing=v.ctx.records.values().filter_map(|x|x.as_link().filter(|d|d.from_id==id).map(|d|json!({"link_id":x.id,"link_type":d.link_type,"to_id":d.to_id,"data":x.data_value()}))).collect::<Vec<_>>();
        let incoming=v.ctx.records.values().filter_map(|x|x.as_link().filter(|d|d.to_id==id).map(|d|json!({"link_id":x.id,"link_type":d.link_type,"from_id":d.from_id,"data":x.data_value()}))).collect::<Vec<_>>();
        json!({"outgoing":outgoing,"incoming":incoming})
    } else {
        json!({"outgoing":[],"incoming":[]})
    };
    let evidence = if parts.contains("evidence") {
        let obs = v
            .ctx
            .records
            .values()
            .filter(|x| {
                x.as_observation().is_some_and(|d| {
                    d.target_revision_id.as_deref() == Some(id)
                        && v.effective.as_ref().is_none_or(|cut| {
                            time(&d.occurred_at, "occurred_at").is_ok_and(|x| x <= *cut)
                        })
                })
            })
            .map(record_json)
            .collect::<Vec<_>>();
        let asm = v
            .ctx
            .records
            .values()
            .filter(|x| {
                x.as_assessment()
                    .is_some_and(|d| d.target_revision_id == id && assessment_visible(&v, d))
            })
            .map(record_json)
            .collect::<Vec<_>>();
        json!({"observations":obs,"assessments":asm})
    } else {
        json!({"observations":[],"assessments":[]})
    };
    Ok(
        json!({"record":record_json(r),"entity":entity.map(record_json),"revisions_of_entity":revisions,"lineage":lineage,"links":links,"evidence":evidence,"occurrences":if parts.contains("occurrences"){all_occurrences(&v,s,id,None)}else{vec![]},"truncated":false}),
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Search {
    project_id: String,
    query: String,
    #[serde(default)]
    stage: Option<Stage>,
    #[serde(default)]
    known_seq: Option<i64>,
    #[serde(default)]
    known_at: Option<String>,
    #[serde(default)]
    effective_at: Option<String>,
    #[serde(default)]
    roles: Vec<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    lanes: Vec<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    vector: Option<Vector>,
    #[serde(default)]
    scope: SearchScope,
    #[serde(default)]
    root_revision_id: Option<String>,
    #[serde(default = "default_search_nodes")]
    max_nodes: usize,
    #[serde(default = "default_search_kinds")]
    kinds: Vec<RecordKind>,
}
#[derive(Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SearchScope {
    #[default]
    Snapshot,
    ProjectHistory,
}
fn default_search_nodes() -> usize {
    limits::MAX_SNAPSHOT_NODES
}
fn default_search_kinds() -> Vec<RecordKind> {
    vec![RecordKind::Revision]
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    model: String,
    dim: usize,
    values: Vec<f64>,
}
fn is_hangul(c: char) -> bool {
    matches!(c, '\u{1100}'..='\u{11ff}' | '\u{3130}'..='\u{318f}' | '\u{ac00}'..='\u{d7af}')
}
fn strip_particle(word: &str) -> String {
    // Only remove common case/topic particles when a useful two-character stem remains.
    const PARTICLES: [&str; 18] = [
        "으로", "에서", "에게", "까지", "부터", "처럼", "보다", "하고", "이며", "이고", "은", "는",
        "이", "가", "을", "를", "의", "에",
    ];
    for particle in PARTICLES {
        if let Some(stem) = word.strip_suffix(particle) {
            if stem.chars().count() >= 2 && stem.chars().all(|c| c.is_alphanumeric()) {
                return stem.to_string();
            }
        }
    }
    word.to_string()
}
fn words(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in s.nfc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            word.push(c);
        } else if !word.is_empty() {
            out.push(strip_particle(&std::mem::take(&mut word)));
        }
    }
    if !word.is_empty() {
        out.push(strip_particle(&word));
    }
    out
}
#[derive(Default)]
struct LexicalFeatures {
    words: HashMap<String, usize>,
    grams: HashSet<String>,
}
fn lexical_features(s: &str) -> LexicalFeatures {
    let mut out = LexicalFeatures::default();
    for word in words(s) {
        *out.words.entry(word.clone()).or_default() += 1;
        let chars: Vec<char> = word.chars().collect();
        if chars.iter().all(|c| is_hangul(*c)) {
            for width in [2, 3] {
                for gram in chars.windows(width) {
                    out.grams.insert(gram.iter().collect());
                }
            }
        }
    }
    out
}
fn lex(q: &LexicalFeatures, s: &str) -> f64 {
    let haystack = lexical_features(s);
    let exact: usize = q
        .words
        .iter()
        .map(|(word, count)| haystack.words.get(word).copied().unwrap_or(0).min(*count))
        .sum();
    let grams = q.grams.intersection(&haystack.grams).count();
    let covered_words = q
        .words
        .keys()
        .filter(|query_word| {
            haystack.words.keys().any(|candidate_word| {
                candidate_word == *query_word || candidate_word.contains(query_word.as_str())
            })
        })
        .count();
    // One short/generic fragment is too weak. A normalized word or two independent
    // Hangul n-grams are required before a row participates in ranking.
    if (q.words.len() >= 2 && covered_words < 2)
        || (q.words.len() < 2 && exact == 0 && covered_words == 0 && grams < 2)
    {
        0.0
    } else {
        exact as f64 * 3.0 + grams as f64
    }
}
fn canonical_role(role: &str) -> String {
    match role
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], "")
        .as_str()
    {
        "fe" | "frontend" => "frontend".into(),
        "be" | "backend" => "backend".into(),
        "infra" | "infrastructure" => "infrastructure".into(),
        other => other.to_string(),
    }
}
fn roles_match(requested: &HashSet<String>, occurrence: &[String]) -> bool {
    occurrence
        .iter()
        .map(|role| canonical_role(role))
        .any(|role| requested.contains(&role))
}
fn cosine(a: &[f64], b: &[f64]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut aa, mut bb) = (0., 0., 0.);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        aa += x * x;
        bb += y * y
    }
    if aa == 0. || bb == 0. {
        None
    } else {
        Some(dot / (aa.sqrt() * bb.sqrt()))
    }
}
pub fn search(ctx: &Context, body: Value) -> ApiResult<Value> {
    let q: Search = serde_json::from_value(body).map_err(|e| bad(e.to_string()))?;
    let mut p = Params::new();
    p.insert("project_id".into(), q.project_id.clone());
    if let Some(x) = q.stage {
        p.insert("stage".into(), x.as_str().into());
    }
    if let Some(x) = q.known_seq {
        p.insert("known_seq".into(), x.to_string());
    }
    if let Some(x) = q.known_at {
        p.insert("known_at".into(), x);
    }
    if let Some(x) = q.effective_at {
        p.insert("effective_at".into(), x);
    }
    if let Some(x) = &q.root_revision_id {
        p.insert("root_revision_id".into(), x.clone());
    }
    let v = view(ctx, &p)?;
    let s = stage(&p)?;
    let limit = q.limit.unwrap_or(limits::DEFAULT_SEARCH_LIMIT);
    if limit == 0 || limit > limits::MAX_SEARCH_LIMIT {
        return Err(bad("limit must be 1..=200"));
    }
    if q.max_nodes == 0 || q.max_nodes > limits::MAX_SNAPSHOT_NODES {
        return Err(bad("max_nodes must be 1..=5000"));
    }
    let allowed_kinds = [
        RecordKind::Revision,
        RecordKind::Capture,
        RecordKind::Observation,
        RecordKind::Goal,
        RecordKind::Assessment,
        RecordKind::Artifact,
    ];
    if q.kinds.is_empty() || q.kinds.iter().any(|kind| !allowed_kinds.contains(kind)) {
        return Err(bad(
            "kinds must contain revision, capture, observation, goal, assessment, or artifact",
        ));
    }
    if q.lanes
        .iter()
        .any(|lane| lane != "official" && lane != "candidate")
    {
        return Err(bad("lanes must contain official and/or candidate"));
    }
    let official_lane = q.lanes.is_empty() || q.lanes.iter().any(|lane| lane == "official");
    let candidate_lane = q.lanes.is_empty() || q.lanes.iter().any(|lane| lane == "candidate");
    let (selected_root, meta) = selected(&v, &p, s, &q.project_id)?;

    #[derive(Clone)]
    struct SearchOccurrence {
        root_revision_id: String,
        occurrence: graph::Occurrence,
    }
    let mut eligible = Vec::new();
    let mut eligible_set = HashSet::new();
    let mut occurrences: HashMap<String, Vec<SearchOccurrence>> = HashMap::new();
    let mut truncated = false;
    let mut add_root = |root_id: &str| {
        let walked = graph::walk(
            &v.ctx,
            root_id,
            Limits {
                max_nodes: q.max_nodes,
                max_depth: graph::MAX_DEPTH,
            },
        );
        truncated |= walked.truncated;
        for occurrence in walked.occurrences {
            if !eligible_set.contains(&occurrence.revision_id) && eligible_set.len() >= q.max_nodes
            {
                truncated = true;
                continue;
            }
            if eligible_set.insert(occurrence.revision_id.clone()) {
                eligible.push(occurrence.revision_id.clone());
            }
            occurrences
                .entry(occurrence.revision_id.clone())
                .or_default()
                .push(SearchOccurrence {
                    root_revision_id: root_id.to_string(),
                    occurrence,
                });
        }
    };
    if official_lane {
        match q.scope {
            SearchScope::Snapshot => {
                if let Some(root_id) = &selected_root {
                    add_root(root_id);
                }
            }
            SearchScope::ProjectHistory => {
                let mut roots = if s == Stage::Official {
                    v.ctx
                        .records
                        .values()
                        .filter_map(|record| {
                            record
                                .as_publication()
                                .filter(|publication| {
                                    publication.project_id == q.project_id
                                        && v.effective.as_ref().is_none_or(|cut| {
                                            time(&publication.published_at, "published_at")
                                                .is_ok_and(|at| at <= *cut)
                                        })
                                })
                                .map(|publication| {
                                    (record.seq, publication.root_revision_id.clone())
                                })
                        })
                        .collect::<Vec<_>>()
                } else {
                    v.ctx
                        .records
                        .values()
                        .filter(|record| {
                            record
                                .as_revision()
                                .is_some_and(|revision| revision.entity_id == q.project_id)
                        })
                        .map(|record| (record.seq, record.id.clone()))
                        .collect::<Vec<_>>()
                };
                roots.sort();
                for (_, root_id) in roots {
                    add_root(&root_id);
                }
                // Project-owned orphan/imported revisions are searchable in history,
                // but have no role occurrence until a project root composes them.
                let mut owned = if s == Stage::Working {
                    v.ctx
                        .records
                        .values()
                        .filter(|record| {
                            record.as_revision().is_some_and(|revision| {
                                v.ctx
                                    .records
                                    .get(&revision.entity_id)
                                    .and_then(StoredRecord::as_entity)
                                    .is_some_and(|entity| entity.project_id == q.project_id)
                            })
                        })
                        .map(|record| (record.seq, record.id.clone()))
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                };
                owned.sort();
                for (_, revision_id) in owned {
                    if eligible_set.insert(revision_id.clone()) {
                        if eligible.len() >= q.max_nodes {
                            eligible_set.remove(&revision_id);
                            truncated = true;
                            break;
                        }
                        eligible.push(revision_id);
                    }
                }
            }
        }
    }
    let query_features = lexical_features(&q.query);
    let requested_roles: HashSet<String> =
        q.roles.iter().map(|role| canonical_role(role)).collect();
    let mut embeddings: HashMap<(String, String, usize), &StoredRecord> = HashMap::new();
    for r in v.ctx.records.values() {
        if let Some(e) = r.as_embedding() {
            if e.dim == e.values.len() && e.values.iter().all(|value| value.is_finite()) {
                let key = (e.revision_id.clone(), e.model.clone(), e.dim);
                let replace = embeddings
                    .get(&key)
                    .is_none_or(|old| (r.seq, &r.id) > (old.seq, &old.id));
                if replace {
                    embeddings.insert(key, r);
                }
            }
        }
    }
    if let Some(vector) = &q.vector {
        if vector.dim == 0
            || vector.dim > limits::MAX_EMBEDDING_DIM
            || vector.dim != vector.values.len()
        {
            return Err(bad("vector dim must be 1..=4096 and equal values length"));
        }
    }
    let vector = q.vector.as_ref();
    struct RevisionHit {
        id: String,
        lexical: f64,
        vector: Option<f64>,
        occurrences: Vec<SearchOccurrence>,
    }
    let mut hits = Vec::new();
    let mut role_mismatches = Vec::new();
    if q.kinds.contains(&RecordKind::Revision) {
        for id in &eligible {
            let r = &v.ctx.records[id];
            let d = r.as_revision().unwrap();
            if !q.tags.iter().all(|x| d.tags.contains(x)) {
                continue;
            }
            let occ = occurrences.get(id).cloned().unwrap_or_default();
            let (_, _, title) = owner(&v.ctx.records, r);
            let vs = vector.and_then(|x| {
                embeddings
                    .get(&(id.clone(), x.model.clone(), x.dim))
                    .and_then(|record| record.as_embedding())
                    .and_then(|embedding| cosine(&x.values, &embedding.values))
            });
            let lexical = lex(
                &query_features,
                &format!("{} {} {}", title, d.body, d.tags.join(" ")),
            );
            if lexical > 0.0 || vs.is_some() || query_features.words.is_empty() && vector.is_none()
            {
                if !requested_roles.is_empty()
                    && !occ
                        .iter()
                        .any(|item| roles_match(&requested_roles, &item.occurrence.roles))
                {
                    role_mismatches.push((id.clone(), occ));
                } else {
                    hits.push(RevisionHit {
                        id: id.clone(),
                        lexical,
                        vector: vs,
                        occurrences: occ,
                    });
                }
            }
        }
    }
    let mut lexical = hits
        .iter()
        .filter(|candidate| candidate.lexical > 0.0)
        .collect::<Vec<_>>();
    lexical.sort_by(|a, b| {
        b.lexical
            .partial_cmp(&a.lexical)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    let lexical_rank: HashMap<_, _> = lexical
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.id.clone(), index + 1))
        .collect();
    let mut vr = hits
        .iter()
        .filter_map(|hit| hit.vector.map(|score| (hit.id.clone(), score)))
        .collect::<Vec<_>>();
    vr.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let vpos: HashMap<_, _> = vr
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.clone(), i + 1))
        .collect();
    hits.sort_by(|a, b| {
        let score = |candidate: &RevisionHit| {
            lexical_rank
                .get(&candidate.id)
                .map_or(0.0, |rank| 1.0 / (60.0 + *rank as f64))
                + vpos
                    .get(&candidate.id)
                    .map_or(0.0, |rank| 1.0 / (60.0 + *rank as f64))
        };
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.id.cmp(&b.id))
    });
    let results = hits
        .iter()
        .take(limit)
        .map(|hit| {
            let id = &hit.id;
            let record = &v.ctx.records[id];
            let revision = record.as_revision().unwrap();
            let (entity_id, entity_kind, title) = owner(&v.ctx.records, record);
            let lexical_rank = lexical_rank.get(id).copied();
            let vector_rank = vpos.get(id).copied();
            let score = lexical_rank.map_or(0.0, |rank| 1.0 / (60.0 + rank as f64))
                + vector_rank.map_or(0.0, |rank| 1.0 / (60.0 + rank as f64));
            json!({
                "revision_id": id,
                "entity_id": entity_id,
                "entity_kind": entity_kind,
                "title": title,
                "snippet": short(&revision.body, 400),
                "tags": revision.tags,
                "occurrences": hit.occurrences.iter().map(|item| json!({
                    "root_revision_id": item.root_revision_id,
                    "slot_path": item.occurrence.slot_path,
                    "roles": item.occurrence.roles,
                })).collect::<Vec<_>>(),
                "lexical_rank": lexical_rank,
                "vector_rank": vector_rank,
                "score": score,
                "claim_mode": revision.source.claim_mode,
                "origin": revision.source.origin,
            })
        })
        .collect::<Vec<_>>();
    let direct_ids: HashSet<String> = results
        .iter()
        .filter_map(|row| row["revision_id"].as_str().map(str::to_string))
        .collect();
    let direct_entities: HashSet<String> = results
        .iter()
        .filter_map(|row| row["entity_id"].as_str().map(str::to_string))
        .collect();
    let revisions_for_endpoint = |endpoint: &str| -> Vec<String> {
        match v.ctx.records.get(endpoint) {
            Some(record)
                if record.kind() == RecordKind::Revision && eligible_set.contains(endpoint) =>
            {
                vec![endpoint.to_string()]
            }
            Some(record) if record.kind() == RecordKind::Entity => eligible
                .iter()
                .filter(|id| {
                    v.ctx
                        .records
                        .get(*id)
                        .and_then(StoredRecord::as_revision)
                        .is_some_and(|revision| revision.entity_id == record.id)
                })
                .cloned()
                .collect(),
            _ => vec![],
        }
    };
    let mut related = Vec::new();
    let mut related_seen: HashSet<String> = HashSet::new();
    let mut relation_records = v
        .ctx
        .records
        .values()
        .filter(|record| record.as_link().is_some())
        .collect::<Vec<_>>();
    relation_records.sort_by_key(|record| {
        let link = record.as_link().unwrap();
        let priority = match (
            link.link_type,
            link.similarity.as_ref().map(|value| value.score),
        ) {
            (LinkType::Contradict, _) => 0,
            (LinkType::Similar, Some(score)) if score < 0.0 => 1,
            (LinkType::Similar, Some(score)) if score > 0.0 => 2,
            _ => 3,
        };
        (priority, record.seq, record.id.clone())
    });
    for record in relation_records {
        let Some(link) = record.as_link() else {
            continue;
        };
        if !matches!(
            link.link_type,
            LinkType::Applies
                | LinkType::Implements
                | LinkType::Depends
                | LinkType::Derived
                | LinkType::Similar
                | LinkType::Contradict
        ) {
            continue;
        }
        let from_direct =
            direct_ids.contains(&link.from_id) || direct_entities.contains(&link.from_id);
        let to_direct = direct_ids.contains(&link.to_id) || direct_entities.contains(&link.to_id);
        if !from_direct && !to_direct {
            continue;
        }
        let similarity_score = link.similarity.as_ref().map(|similarity| similarity.score);
        if link.link_type == LinkType::Similar && similarity_score == Some(0.0) {
            continue;
        }
        let other = if from_direct {
            &link.to_id
        } else {
            &link.from_id
        };
        for revision_id in revisions_for_endpoint(other) {
            if direct_ids.contains(&revision_id) {
                continue;
            }
            let occurrence = occurrences.get(&revision_id).cloned().unwrap_or_default();
            if !matches!(link.link_type, LinkType::Similar | LinkType::Contradict)
                && !requested_roles.is_empty()
                && occurrence
                    .iter()
                    .any(|item| roles_match(&requested_roles, &item.occurrence.roles))
            {
                continue;
            }
            let reason = match (link.link_type, similarity_score) {
                (LinkType::Similar, Some(score)) if score < 0.0 => "dissimilarity",
                (LinkType::Similar, _) => "similarity",
                (LinkType::Contradict, _) => "contradiction",
                _ => "role_mismatch",
            };
            if related_seen.insert(revision_id.clone()) {
                let target = &v.ctx.records[&revision_id];
                let (entity_id, _, title) = owner(&v.ctx.records, target);
                related.push(json!({"revision_id":revision_id,"record_id":revision_id,"kind":"revision","entity_id":entity_id,"title":title,"reason":reason,"counterevidence":matches!(reason,"dissimilarity"|"contradiction"),"link_type":link.link_type,"similarity_score":similarity_score,"roles":occurrence.iter().flat_map(|item|item.occurrence.roles.clone()).collect::<BTreeSet<_>>() }));
            }
        }
        if link.link_type == LinkType::Contradict && revisions_for_endpoint(other).is_empty() {
            let Some(other_record) = v.ctx.records.get(other) else {
                continue;
            };
            let scoped = match &other_record.data {
                RecordData::Capture(capture) => {
                    capture
                        .project_id
                        .as_deref()
                        .is_none_or(|project| project == q.project_id)
                        && capture.occurred_at.as_ref().is_none_or(|at| {
                            v.effective.as_ref().is_none_or(|cut| {
                                time(at, "occurred_at").is_ok_and(|value| value <= *cut)
                            })
                        })
                }
                RecordData::Observation(observation) => {
                    observation.project_id == q.project_id
                        && v.effective.as_ref().is_none_or(|cut| {
                            time(&observation.occurred_at, "occurred_at")
                                .is_ok_and(|value| value <= *cut)
                        })
                }
                RecordData::Goal(goal) => goal.project_id == q.project_id,
                RecordData::Assessment(assessment) => {
                    assessment_visible(&v, assessment)
                        && v.ctx
                            .records
                            .get(&assessment.baseline_id)
                            .and_then(StoredRecord::as_baseline)
                            .and_then(|baseline| v.ctx.records.get(&baseline.goal_id))
                            .and_then(StoredRecord::as_goal)
                            .is_some_and(|goal| goal.project_id == q.project_id)
                }
                _ => false,
            };
            if scoped && related_seen.insert(other.clone()) {
                let (title, snippet) = match &other_record.data {
                    RecordData::Capture(value) => (
                        value.source_ref.clone().unwrap_or_else(|| "capture".into()),
                        short(&value.content, 400),
                    ),
                    RecordData::Observation(value) => (
                        value.metric.clone().unwrap_or_else(|| "observation".into()),
                        short(&value.note, 400),
                    ),
                    RecordData::Goal(value) => {
                        (value.statement.clone(), short(&value.statement, 400))
                    }
                    RecordData::Assessment(value) => (
                        format!("{} assessment", value.evaluator),
                        short(&value.note, 400),
                    ),
                    _ => (other.clone(), String::new()),
                };
                related.push(json!({"record_id":other_record.id,"kind":other_record.kind().as_str(),"title":title,"snippet":snippet,"reason":"contradiction","counterevidence":true,"link_type":link.link_type}));
            }
        }
    }
    for (id, occ) in role_mismatches {
        if related_seen.insert(id.clone()) {
            let record = &v.ctx.records[&id];
            let (entity_id, _, title) = owner(&v.ctx.records, record);
            let lineage_reaches_requested_role = v
                .ctx
                .records
                .get(&entity_id)
                .and_then(StoredRecord::as_entity)
                .is_some_and(|entity| {
                    entity.derived_from.iter().any(|source| {
                        revisions_for_endpoint(source)
                            .iter()
                            .any(|source_revision| {
                                occurrences.get(source_revision).is_some_and(|uses| {
                                    uses.iter().any(|item| {
                                        roles_match(&requested_roles, &item.occurrence.roles)
                                    })
                                })
                            })
                    })
                });
            related.push(json!({"revision_id":id,"record_id":id,"kind":"revision","entity_id":entity_id,"title":title,"reason":"role_mismatch","counterevidence":false,"roles":occ.iter().flat_map(|item|item.occurrence.roles.clone()).collect::<BTreeSet<_>>(),"link_type":if lineage_reaches_requested_role { Some("derived") } else { None } }));
        }
    }
    if q.scope == SearchScope::ProjectHistory {
        for entity_id in &direct_entities {
            if let Some(entity) = v
                .ctx
                .records
                .get(entity_id)
                .and_then(StoredRecord::as_entity)
            {
                for source in &entity.derived_from {
                    for revision_id in revisions_for_endpoint(source) {
                        if direct_ids.contains(&revision_id)
                            || !related_seen.insert(revision_id.clone())
                        {
                            continue;
                        }
                        let target = &v.ctx.records[&revision_id];
                        let (source_entity_id, _, title) = owner(&v.ctx.records, target);
                        related.push(json!({"revision_id":revision_id,"entity_id":source_entity_id,"title":title,"reason":"derived_from","link_type":"derived"}));
                    }
                }
            }
        }
    }
    let candidates = if candidate_lane && requested_roles.is_empty() {
        let mut lane = v
            .ctx
            .records
            .values()
            .filter_map(|r| match &r.data {
                RecordData::Candidate(c) if c.project_id == q.project_id => {
                    let score = lex(&query_features, &format!("{} {}", c.title, c.body));
                    if query_features.words.is_empty() || score > 0.0 {
                        Some((r, c, score))
                    } else {
                        None
                    }
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        lane.sort_by(|left, right| {
            right
                .2
                .partial_cmp(&left.2)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(left.0.id.cmp(&right.0.id))
        });
        lane.into_iter()
            .take(limit)
            .enumerate()
            .map(|(index, (record, candidate, score))| {
                json!({
                    "candidate_id": record.id,
                    "title": candidate.title,
                    "body_preview": short(&candidate.body, 400),
                    "origin": candidate.origin,
                    "model": candidate.model,
                    "status": candidate.status,
                    "lexical_rank": index + 1,
                    "score": score,
                })
            })
            .collect::<Vec<_>>()
    } else {
        vec![]
    };
    let matching = eligible
        .iter()
        .filter(|id| {
            vector
                .and_then(|x| embeddings.get(&(id.to_string(), x.model.clone(), x.dim)))
                .is_some()
        })
        .count();
    let eligible_ideas = eligible
        .iter()
        .filter(|id| {
            let Some(record) = v.ctx.records.get(id.as_str()) else {
                return false;
            };
            let (_, entity_kind, _) = owner(&v.ctx.records, record);
            entity_kind == "idea"
                && (requested_roles.is_empty()
                    || occurrences.get(id.as_str()).is_some_and(|uses| {
                        uses.iter()
                            .any(|item| roles_match(&requested_roles, &item.occurrence.roles))
                    }))
        })
        .collect::<Vec<_>>();
    let matching_ideas = eligible_ideas
        .iter()
        .filter(|id| {
            vector
                .and_then(|x| embeddings.get(&(id.to_string(), x.model.clone(), x.dim)))
                .is_some()
        })
        .count();
    let target_roles = |target: &str| occurrences.get(target).cloned().unwrap_or_default();
    let observation_in_scope = |observation: &ObservationData| {
        observation.project_id == q.project_id
            && observation
                .target_revision_id
                .as_ref()
                .is_none_or(|id| eligible_set.contains(id))
            && v.effective.as_ref().is_none_or(|cut| {
                time(&observation.occurred_at, "occurred_at").is_ok_and(|at| at <= *cut)
            })
    };
    let mut records = Vec::new();
    if official_lane {
        for record in v.ctx.records.values() {
            if record.kind() == RecordKind::Revision || !q.kinds.contains(&record.kind()) {
                continue;
            }
            let (eligible_record, associated_target, title, snippet, search_text) = match &record
                .data
            {
                RecordData::Capture(capture) => {
                    let linked_target = eligible
                        .iter()
                        .find(|id| {
                            v.ctx
                                .records
                                .get(id.as_str())
                                .and_then(StoredRecord::as_revision)
                                .is_some_and(|revision| {
                                    (revision.source.capture_id.as_deref() == Some(&record.id)
                                        || revision
                                            .source
                                            .source_anchor
                                            .as_ref()
                                            .is_some_and(|anchor| anchor.capture_id == record.id))
                                        && (requested_roles.is_empty()
                                            || occurrences.get(id.as_str()).is_some_and(|uses| {
                                                uses.iter().any(|item| {
                                                    roles_match(
                                                        &requested_roles,
                                                        &item.occurrence.roles,
                                                    )
                                                })
                                            }))
                                })
                        })
                        .cloned()
                        .or_else(|| {
                            v.ctx.records.values().find_map(|candidate| {
                                candidate
                                    .as_observation()
                                    .filter(|observation| {
                                        observation.capture_id.as_deref() == Some(&record.id)
                                            && observation_in_scope(observation)
                                            && (requested_roles.is_empty()
                                                || observation
                                                    .target_revision_id
                                                    .as_ref()
                                                    .is_some_and(|target| {
                                                        occurrences.get(target).is_some_and(
                                                            |uses| {
                                                                uses.iter().any(|item| {
                                                                    roles_match(
                                                                        &requested_roles,
                                                                        &item.occurrence.roles,
                                                                    )
                                                                })
                                                            },
                                                        )
                                                    }))
                                    })
                                    .and_then(|observation| observation.target_revision_id.clone())
                            })
                        });
                    let effective = capture.occurred_at.as_ref().is_none_or(|at| {
                        v.effective.as_ref().is_none_or(|cut| {
                            time(at, "occurred_at").is_ok_and(|value| value <= *cut)
                        })
                    });
                    let title = capture
                        .source_ref
                        .clone()
                        .unwrap_or_else(|| "capture".into());
                    (
                        effective
                            && (capture.project_id.as_deref() == Some(&q.project_id)
                                || linked_target.is_some()),
                        linked_target,
                        title.clone(),
                        short(&capture.content, 400),
                        format!("{title} {}", capture.content),
                    )
                }
                RecordData::Observation(observation) => {
                    let title = observation
                        .metric
                        .clone()
                        .unwrap_or_else(|| "observation".into());
                    (
                        observation_in_scope(observation),
                        observation.target_revision_id.clone(),
                        title.clone(),
                        short(&observation.note, 400),
                        format!(
                            "{title} {} {} {:?} {} {} {}",
                            observation.note,
                            observation.method.as_deref().unwrap_or(""),
                            observation.status,
                            observation
                                .value
                                .as_ref()
                                .map(Value::to_string)
                                .unwrap_or_default(),
                            observation.unit.as_deref().unwrap_or(""),
                            serde_json::to_string(&observation.environment).unwrap_or_default(),
                        ),
                    )
                }
                RecordData::Goal(goal) => (
                    goal.project_id == q.project_id
                        && eligible_set.contains(&goal.scope.target_revision_id),
                    Some(goal.scope.target_revision_id.clone()),
                    goal.statement.clone(),
                    short(&goal.statement, 400),
                    format!(
                        "{} {}",
                        goal.statement,
                        serde_json::to_string(&goal.criteria).unwrap_or_default()
                    ),
                ),
                RecordData::Assessment(assessment) => {
                    let belongs = v
                        .ctx
                        .records
                        .get(&assessment.baseline_id)
                        .and_then(StoredRecord::as_baseline)
                        .and_then(|baseline| v.ctx.records.get(&baseline.goal_id))
                        .and_then(StoredRecord::as_goal)
                        .is_some_and(|goal| goal.project_id == q.project_id);
                    (
                        belongs
                            && eligible_set.contains(&assessment.target_revision_id)
                            && assessment_visible(&v, assessment),
                        Some(assessment.target_revision_id.clone()),
                        format!("{} assessment", assessment.evaluator),
                        short(&assessment.note, 400),
                        format!(
                            "{} {} {:?}",
                            assessment.evaluator, assessment.note, assessment.status
                        ),
                    )
                }
                RecordData::Artifact(artifact) => {
                    let target = v.ctx.records.values().find_map(|candidate| {
                        candidate
                            .as_observation()
                            .filter(|observation| {
                                observation.artifact_ids.contains(&record.id)
                                    && observation_in_scope(observation)
                                    && (requested_roles.is_empty()
                                        || observation.target_revision_id.as_ref().is_some_and(
                                            |target| {
                                                occurrences.get(target).is_some_and(|uses| {
                                                    uses.iter().any(|item| {
                                                        roles_match(
                                                            &requested_roles,
                                                            &item.occurrence.roles,
                                                        )
                                                    })
                                                })
                                            },
                                        ))
                            })
                            .and_then(|observation| observation.target_revision_id.clone())
                    });
                    (
                        target.is_some(),
                        target,
                        artifact.uri.clone(),
                        short(&artifact.note, 400),
                        format!("{} {} {}", artifact.uri, artifact.note, artifact.digest),
                    )
                }
                _ => continue,
            };
            if !eligible_record {
                continue;
            }
            let associated_occurrences = associated_target
                .as_deref()
                .map(&target_roles)
                .unwrap_or_default();
            if !requested_roles.is_empty()
                && !associated_occurrences
                    .iter()
                    .any(|item| roles_match(&requested_roles, &item.occurrence.roles))
            {
                continue;
            }
            let score = lex(&query_features, &search_text);
            if !query_features.words.is_empty() && score == 0.0 {
                continue;
            }
            records.push((record, title, snippet, score, associated_occurrences));
        }
    }
    records.sort_by(|left, right| {
        right
            .3
            .partial_cmp(&left.3)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(left.0.id.cmp(&right.0.id))
    });
    let records = records.into_iter().take(limit).enumerate().map(|(index, (record, title, snippet, score, occ))| json!({
        "record_id":record.id,"kind":record.kind().as_str(),"title":title,"snippet":snippet,"lexical_rank":index+1,"score":score,
        "occurrences":occ.iter().map(|item|json!({"root_revision_id":item.root_revision_id,"slot_path":item.occurrence.slot_path,"roles":item.occurrence.roles})).collect::<Vec<_>>()
    })).collect::<Vec<_>>();
    Ok(json!({
        "mode": if matching > 0 { "lexical+vector" } else { "lexical" },
        "stage": s.as_str(),
        "selected_by": meta,
        "results": results,
        "records": records,
        "related": related,
        "candidates": candidates,
        "vector_status": {
            "requested": q.vector.is_some(),
            "model": q.vector.as_ref().map(|vector| &vector.model),
            "dim": q.vector.as_ref().map(|vector| vector.dim),
            "eligible_revisions": eligible.len(),
            "with_matching_embedding": matching,
            "eligible_idea_revisions": eligible_ideas.len(),
            "ideas_with_matching_embedding": matching_ideas,
            "used": matching > 0,
        },
        "eligible_revisions": eligible.len(),
        "scope": match q.scope { SearchScope::Snapshot => "snapshot", SearchScope::ProjectHistory => "project_history" },
        "truncated": truncated,
    }))
}
pub fn occurrences(ctx: &Context, p: &Params) -> ApiResult<Value> {
    check(
        p,
        &[
            "project_id",
            "entity_id",
            "revision_id",
            "stage",
            "known_seq",
            "known_at",
            "max_nodes",
        ],
    )?;
    let project = p
        .get("project_id")
        .ok_or_else(|| bad("project_id is required"))?;
    let ent = p.get("entity_id");
    let rev = p.get("revision_id");
    if ent.is_some() == rev.is_some() {
        return Err(bad("exactly one of entity_id or revision_id is required"));
    }
    let v = view(ctx, p)?;
    let s = stage(p)?;
    let max = num(
        p,
        "max_nodes",
        limits::DEFAULT_SNAPSHOT_NODES,
        limits::MAX_SNAPSHOT_NODES,
    )?;
    let (root, meta) = selected(&v, p, s, project)?;
    let Some(root) = root else {
        return Ok(
            json!({"selected_by":meta,"occurrences":[],"occurrence_count":0,"truncated":false}),
        );
    };
    let wanted = ent.or(rev).unwrap();
    let walk = graph::walk(
        &v.ctx,
        &root,
        Limits {
            max_nodes: max,
            max_depth: graph::MAX_DEPTH,
        },
    );
    let rows=walk.occurrences.iter().filter(|o|o.revision_id==*wanted||ent.is_some()&&v.ctx.records.get(&o.revision_id).and_then(StoredRecord::as_revision).is_some_and(|d|d.entity_id==*wanted)).map(|o|json!({"slot_path":o.slot_path,"revision_id":o.revision_id,"roles":o.roles,"parent_revision_id":o.parent_revision_id,"depth":o.depth})).collect::<Vec<_>>();
    Ok(
        json!({"selected_by":meta,"occurrences":rows,"occurrence_count":rows.len(),"truncated":walk.truncated}),
    )
}
fn required_criteria_status(
    goal: &GoalData,
    baseline: &BaselineData,
    assessment: &AssessmentData,
) -> Vec<Value> {
    goal.criteria
        .iter()
        .filter(|criterion| {
            criterion.required && baseline.criterion_ids.contains(&criterion.criterion_id)
        })
        .map(|criterion| {
            let status = assessment
                .criteria_results
                .iter()
                .find(|result| result.criterion_id == criterion.criterion_id)
                .map(|result| result.status);
            json!({"criterion_id":criterion.criterion_id,"status":status,"required":true})
        })
        .collect()
}

fn unassessed_required_criteria(goal: &GoalData, baseline: &BaselineData) -> Vec<Value> {
    goal.criteria
        .iter()
        .filter(|criterion| {
            criterion.required && baseline.criterion_ids.contains(&criterion.criterion_id)
        })
        .map(|criterion| {
            json!({"criterion_id":criterion.criterion_id,"status":null,"required":true})
        })
        .collect()
}

fn gate_status(results: &[Value]) -> &'static str {
    if results.is_empty() || results.iter().any(|result| result["status"].is_null()) {
        return "unknown";
    }
    let statuses: Vec<&str> = results
        .iter()
        .filter_map(|result| result["status"].as_str())
        .collect();
    if statuses.contains(&"unmet") {
        "unmet"
    } else if statuses.contains(&"disputed") {
        "disputed"
    } else if statuses.iter().all(|status| *status == "met") {
        "met"
    } else {
        "unknown"
    }
}

pub fn goals(ctx: &Context, p: &Params) -> ApiResult<Value> {
    check(
        p,
        &[
            "project_id",
            "stage",
            "known_seq",
            "known_at",
            "effective_at",
            "root_revision_id",
            "max_nodes",
        ],
    )?;
    let project = p
        .get("project_id")
        .ok_or_else(|| bad("project_id is required"))?;
    let v = view(ctx, p)?;
    let s = stage(p)?;
    let max = num(
        p,
        "max_nodes",
        limits::DEFAULT_SNAPSHOT_NODES,
        limits::MAX_SNAPSHOT_NODES,
    )?;
    let (root, _meta) = selected(&v, p, s, project)?;
    let Some(root) = root else {
        return Ok(
            json!({"scope":{"project_id":project,"stage":s.as_str(),"known_seq":v.known,"root_revision_id":null},"goals":[],"stale_assessments":[],"missing_goal_occurrences":[],"proposed_goals":[],"note":"gate_status uses required criteria only","truncated":false}),
        );
    };
    let walk = graph::walk(
        &v.ctx,
        &root,
        Limits {
            max_nodes: max,
            max_depth: graph::MAX_DEPTH,
        },
    );
    let mut rows = Vec::new();
    let mut proposed = Vec::new();
    let mut stale = Vec::new();
    for gr in v.ctx.records.values().filter(|r| r.as_goal().is_some()) {
        let g = gr.as_goal().unwrap();
        if g.project_id != *project {
            continue;
        }
        let is_proposed = g.origin == ProposalOrigin::AiProposed;
        let exact = g.scope.root_revision_id == root
            && graph::resolve_path(&v.ctx, &g.scope.root_revision_id, &g.scope.slot_path)
                .is_ok_and(|x| x.id == g.scope.target_revision_id);
        let mut baseline_records = v
            .ctx
            .records
            .values()
            .filter_map(|record| {
                let baseline = record.as_baseline()?;
                let effective_from =
                    time(&baseline.effective_from, "baseline effective_from").ok()?;
                (baseline.goal_id == gr.id
                    && v.effective
                        .as_ref()
                        .is_none_or(|cut| effective_from <= *cut))
                .then_some((record, baseline, effective_from))
            })
            .collect::<Vec<_>>();
        baseline_records.sort_by(|left, right| {
            (left.2.as_str(), left.0.seq, left.0.id.as_str()).cmp(&(
                right.2.as_str(),
                right.0.seq,
                right.0.id.as_str(),
            ))
        });

        let mut bases = Vec::new();
        let mut current_gate = None;
        for (baseline_record, baseline, effective_from) in baseline_records {
            let mut assessments = v
                .ctx
                .records
                .values()
                .filter(|record| {
                    record
                        .as_assessment()
                        .is_some_and(|assessment| assessment.baseline_id == baseline_record.id)
                })
                .collect::<Vec<_>>();
            assessments.sort_by_key(|record| (record.seq, record.id.clone()));

            let mut official = None;
            let mut proposed_assessments = Vec::new();
            for assessment_record in assessments {
                let assessment = assessment_record.as_assessment().unwrap();
                if !assessment_visible(&v, assessment) {
                    continue;
                }
                let valid = assessment.root_revision_id == root
                    && assessment.slot_path == g.scope.slot_path
                    && assessment.target_revision_id == g.scope.target_revision_id
                    && graph::resolve_path(
                        &v.ctx,
                        &assessment.root_revision_id,
                        &assessment.slot_path,
                    )
                    .is_ok_and(|target| target.id == assessment.target_revision_id);
                if !valid {
                    let current_target = graph::resolve_path(&v.ctx, &root, &assessment.slot_path)
                        .ok()
                        .map(|target| target.id.clone());
                    let applicability = match current_target.as_deref() {
                        None => "path_removed",
                        Some(target) if target == assessment.target_revision_id => {
                            "unchanged_target"
                        }
                        Some(_) => "target_changed",
                    };
                    stale.push(json!({
                        "assessment_id": assessment_record.id,
                        "goal_id": gr.id,
                        "baseline_id": baseline_record.id,
                        "reason": if assessment.root_revision_id != root {
                            "root_not_in_snapshot"
                        } else {
                            applicability
                        },
                        "applicability": applicability,
                        "original_root": assessment.root_revision_id,
                        "current_root": root,
                        "original_target_revision_id": assessment.target_revision_id,
                        "current_target_revision_id": current_target,
                        "requires_review": true,
                        "status": assessment.status,
                        "recorded_at": assessment_record.recorded_at,
                    }));
                    continue;
                }
                let required = required_criteria_status(g, baseline, assessment);
                let projection = json!({
                    "assessment_id": assessment_record.id,
                    "origin": assessment.origin,
                    "status": assessment.status,
                    "gate_status": gate_status(&required),
                    "evaluator": assessment.evaluator,
                    "rubric_version": assessment.rubric_version,
                    "recorded_at": assessment_record.recorded_at,
                    "evidence_cutoff_seq": assessment.evidence_cutoff_seq,
                    "scope_match": "exact",
                    "stale": false,
                    "criteria_results": assessment.criteria_results,
                    "required_criteria_status": required,
                });
                if assessment.origin == ProposalOrigin::Official {
                    official = Some(projection);
                } else {
                    proposed_assessments.push(projection);
                }
            }

            let successor = v
                .ctx
                .records
                .values()
                .filter_map(|record| {
                    let candidate = record.as_baseline()?;
                    let candidate_effective =
                        time(&candidate.effective_from, "baseline effective_from").ok()?;
                    (candidate.supersedes.as_deref() == Some(&baseline_record.id)
                        && v.effective
                            .as_ref()
                            .is_none_or(|cut| candidate_effective <= *cut))
                    .then_some((candidate_effective, record.seq, record.id.clone()))
                })
                .max();
            if successor.is_none() {
                let selected_assessment = if is_proposed {
                    proposed_assessments.last()
                } else {
                    official.as_ref()
                };
                let (gate, required, status) = selected_assessment.map_or_else(
                    || {
                        (
                            "unknown".to_string(),
                            unassessed_required_criteria(g, baseline),
                            Value::Null,
                        )
                    },
                    |assessment| {
                        (
                            assessment["gate_status"]
                                .as_str()
                                .unwrap_or("unknown")
                                .to_string(),
                            assessment["required_criteria_status"]
                                .as_array()
                                .cloned()
                                .unwrap_or_default(),
                            assessment["status"].clone(),
                        )
                    },
                );
                current_gate = Some((
                    effective_from,
                    baseline_record.seq,
                    baseline_record.id.clone(),
                    gate,
                    required,
                    status,
                ));
            }
            bases.push(json!({
                "baseline_id": baseline_record.id,
                "seq": baseline_record.seq,
                "recorded_at": baseline_record.recorded_at,
                "effective_from": baseline.effective_from,
                "supersedes": baseline.supersedes,
                "criterion_ids": baseline.criterion_ids,
                "official_assessment": official,
                "superseded_by": successor.map(|(_, _, id)| id),
                "proposed_assessments": proposed_assessments,
            }));
        }
        let (gate, required, status) = if exact {
            current_gate
                .map(|(_, _, _, gate, required, status)| (gate, required, status))
                .unwrap_or_else(|| ("unknown".to_string(), Vec::new(), Value::Null))
        } else {
            ("unknown".to_string(), Vec::new(), Value::Null)
        };
        let projection = json!({"goal_id":gr.id,"statement":g.statement,"origin":g.origin,"scope":{"root_revision_id":g.scope.root_revision_id,"slot_path":g.scope.slot_path,"target_revision_id":g.scope.target_revision_id,"in_current_snapshot":exact},"origin_project_id":g.project_id,"criteria":g.criteria,"baselines":bases,"gate_status":gate,"status":status,"required_criteria_status":required });
        if is_proposed {
            proposed.push(projection);
        } else {
            rows.push(projection);
        }
    }
    let missing=walk.occurrences.iter().filter(|o|!rows.iter().any(|g|g["scope"]["in_current_snapshot"]==true&&g["scope"]["slot_path"]==json!(o.slot_path)&&g["scope"]["target_revision_id"]==json!(o.revision_id))).filter_map(|o|v.ctx.records.get(&o.revision_id).map(|r|{let(e,k,_)=owner(&v.ctx.records,r);json!({"slot_path":o.slot_path,"revision_id":o.revision_id,"entity_id":e,"entity_kind":k})})).collect::<Vec<_>>();
    Ok(
        json!({"scope":{"project_id":project,"stage":s.as_str(),"known_seq":v.known,"root_revision_id":root},"goals":rows,"stale_assessments":stale,"missing_goal_occurrences":missing,"proposed_goals":proposed,"note":"gate_status uses only required criteria; no atom-count progress","truncated":walk.truncated}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::fixtures;
    use serde_json::json;

    fn related_context() -> Context {
        let mut records = BTreeMap::new();
        let mut add = |record: StoredRecord| {
            records.insert(record.id.clone(), record);
        };
        add(fixtures::project("proj_q", 1));
        add(fixtures::entity("ent_schema", 2, "schema", "proj_q"));
        add(fixtures::record(
            "ent_be",
            3,
            RecordKind::Entity,
            json!({
                "entity_kind":"idea",
                "project_id":"proj_q",
                "title":"ent_be",
                "derived_from":["ent_fe"],
                "lineage_kind":"semantic_edit"
            }),
        ));
        add(fixtures::entity("ent_fe", 4, "idea", "proj_q"));
        add(fixtures::revision(
            "rev_be",
            5,
            "ent_be",
            "백엔드 정책",
            json!([]),
        ));
        add(fixtures::revision(
            "rev_fe",
            6,
            "ent_fe",
            "프론트 구현",
            json!([]),
        ));
        add(fixtures::revision(
            "rev_schema",
            7,
            "ent_schema",
            "스키마",
            json!([
                {"slot_id":"be","revision_id":"rev_be","roles":["be"]},
                {"slot_id":"fe","revision_id":"rev_fe","roles":["fe"]}
            ]),
        ));
        add(fixtures::revision(
            "rev_root",
            8,
            "proj_q",
            "프로젝트",
            json!([
                {"slot_id":"schema","revision_id":"rev_schema","roles":[]}
            ]),
        ));
        add(fixtures::record(
            "lnk_related",
            9,
            RecordKind::Link,
            json!({
                "link_type":"applies","from_id":"ent_be","to_id":"ent_fe","note":"","actor":"human:test","impact":null,"similarity":null
            }),
        ));
        add(fixtures::record(
            "hc_q",
            10,
            RecordKind::HeadChange,
            json!({
                "project_id":"proj_q","stage":"working","before_revision_id":null,"after_revision_id":"rev_root","reason":"root","actor":"human:test","meaningful":true,"decision":{"before":"","after":"","rationale":"test"}
            }),
        ));
        Context {
            records,
            seq: 10,
            ..Context::default()
        }
    }
    #[test]
    fn historical_view_hides_future() {
        let c = Context {
            records: fixtures::deep_diamond(),
            seq: 100,
            ..Context::default()
        };
        let mut p = Params::new();
        p.insert("known_seq".into(), "3".into());
        assert!(!view(&c, &p).unwrap().ctx.records.contains_key("rev_l5"));
    }

    #[test]
    fn role_mismatched_linked_entity_is_related() {
        let found = search(&related_context(), json!({
            "project_id":"proj_q","query":"정책","roles":["fe"],"tags":[],"lanes":["official"],"limit":20,"vector":null
        })).unwrap();
        assert!(found["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["revision_id"] != "rev_be"));
        assert!(found["related"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["revision_id"] == "rev_be"));
    }

    #[test]
    fn state_counts_links_by_endpoint_project() {
        let found = state(&related_context(), &Params::new()).unwrap();
        assert_eq!(found["projects"][0]["counts"]["link"], json!(1));
    }

    #[test]
    fn derived_lineage_is_related_without_a_link_record() {
        let mut context = related_context();
        context.records.remove("lnk_related");
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"정책","roles":["fe"],
                "lanes":["official"],"limit":20
            }),
        )
        .unwrap();
        assert!(found["related"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["revision_id"] == "rev_be" && row["link_type"] == "derived"));
    }

    #[test]
    fn vector_rank_reranks_before_limit() {
        let mut context = related_context();
        context.records.insert(
            "rev_fe".into(),
            fixtures::revision("rev_fe", 6, "ent_fe", "정책 프론트 구현", json!([])),
        );
        context.records.insert(
            "embedding_fe".into(),
            fixtures::record(
                "embedding_fe",
                11,
                RecordKind::Embedding,
                json!({
                    "revision_id":"rev_fe","model":"test-vector","dim":2,
                    "values":[1.0,0.0],"normalized":true
                }),
            ),
        );
        context.seq = 11;
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"정책","limit":1,
                "vector":{"model":"test-vector","dim":2,"values":[1.0,0.0]}
            }),
        )
        .unwrap();
        assert_eq!(found["results"][0]["revision_id"], "rev_fe");
        assert_eq!(found["results"][0]["vector_rank"], 1);
    }

    #[test]
    fn lexical_search_without_a_match_has_no_results() {
        let found = search(
            &related_context(),
            json!({"project_id":"proj_q","query":"not-present","limit":20}),
        )
        .unwrap();
        assert!(found["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn hangul_search_normalizes_particles_and_spacing_without_one_fragment_hits() {
        assert!(lex(&lexical_features("경보를"), "경보 처리") > 0.0);
        assert!(lex(&lexical_features("경보"), "화재경보 라우팅") > 0.0);
        assert!(lex(&lexical_features("경보"), "경보시스템 라우팅") > 0.0);
        assert!(lex(&lexical_features("경보"), "경보 시스템") > 0.0);
        assert!(lex(&lexical_features("관제 타임라인"), "관제타임라인 재구성") > 0.0);
        assert_eq!(lex(&lexical_features("실시간 관제"), "실시간 보고서"), 0.0);
    }

    #[test]
    fn role_aliases_are_contextual_and_data_is_not_infrastructure() {
        let frontend = search(
            &related_context(),
            json!({"project_id":"proj_q","query":"프론트","roles":["FE"]}),
        )
        .unwrap();
        assert_eq!(frontend["results"][0]["revision_id"], "rev_fe");
        let backend = search(
            &related_context(),
            json!({"project_id":"proj_q","query":"정책","roles":["backend"]}),
        )
        .unwrap();
        assert_eq!(backend["results"][0]["revision_id"], "rev_be");
        let data = search(
            &related_context(),
            json!({"project_id":"proj_q","query":"정책","roles":["data"]}),
        )
        .unwrap();
        assert!(data["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn candidate_only_search_does_not_require_a_head() {
        let mut context = Context::default();
        context
            .records
            .insert("project".into(), fixtures::project("project", 1));
        context.records.insert(
            "candidate".into(),
            fixtures::record(
                "candidate",
                2,
                RecordKind::Candidate,
                json!({
                    "project_id":"project","proposed_kind":"idea","title":"경보 후보",
                    "body":"관제 경보","origin":"human","claim_mode":"inferred","status":"pending"
                }),
            ),
        );
        context.seq = 2;
        let found = search(
            &context,
            json!({"project_id":"project","query":"경보를","lanes":["candidate"]}),
        )
        .unwrap();
        assert_eq!(found["candidates"][0]["candidate_id"], "candidate");
        assert!(found["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn history_includes_local_orphans_but_not_cross_project_or_roleless_orphans() {
        let mut context = related_context();
        context.records.insert(
            "ent_orphan".into(),
            fixtures::entity("ent_orphan", 11, "idea", "proj_q"),
        );
        context.records.insert(
            "rev_orphan".into(),
            fixtures::revision("rev_orphan", 12, "ent_orphan", "고립 경보", json!([])),
        );
        context
            .records
            .insert("other".into(), fixtures::project("other", 13));
        context.records.insert(
            "ent_cross".into(),
            fixtures::entity("ent_cross", 14, "idea", "other"),
        );
        context.records.insert(
            "rev_cross".into(),
            fixtures::revision("rev_cross", 15, "ent_cross", "고립 경보", json!([])),
        );
        context.seq = 15;
        let found = search(
            &context,
            json!({"project_id":"proj_q","query":"고립 경보","scope":"project_history"}),
        )
        .unwrap();
        let ids = found["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["revision_id"].as_str())
            .collect::<Vec<_>>();
        assert!(ids.contains(&"rev_orphan"));
        assert!(!ids.contains(&"rev_cross"));
        let role_filtered = search(
            &context,
            json!({"project_id":"proj_q","query":"고립 경보","scope":"project_history","roles":["be"]}),
        )
        .unwrap();
        assert!(role_filtered["results"].as_array().unwrap().is_empty());
        let past = search(
            &context,
            json!({"project_id":"proj_q","query":"고립 경보","scope":"project_history","known_seq":11}),
        )
        .unwrap();
        assert!(past["results"].as_array().unwrap().is_empty());
    }

    #[test]
    fn official_history_contains_only_publication_roots_at_the_effective_cutoff() {
        let mut context = related_context();
        context.records.insert(
            "ent_future".into(),
            fixtures::entity("ent_future", 11, "idea", "proj_q"),
        );
        context.records.insert(
            "rev_future".into(),
            fixtures::revision("rev_future", 12, "ent_future", "미래 경보", json!([])),
        );
        context.records.insert(
            "rev_schema_future".into(),
            fixtures::revision(
                "rev_schema_future",
                13,
                "ent_schema",
                "미래 스키마",
                json!([
                    {"slot_id":"future","revision_id":"rev_future","roles":["be"]}
                ]),
            ),
        );
        context.records.insert(
            "rev_root_future".into(),
            fixtures::revision(
                "rev_root_future",
                14,
                "proj_q",
                "미래 루트",
                json!([
                    {"slot_id":"schema","revision_id":"rev_schema_future","roles":[]}
                ]),
            ),
        );
        for (id, seq, root, published_at) in [
            ("publication_old", 15, "rev_root", "2026-08-01T00:00:00Z"),
            (
                "publication_future",
                16,
                "rev_root_future",
                "2026-09-01T00:00:00Z",
            ),
        ] {
            context.records.insert(
                id.into(),
                fixtures::record(
                    id,
                    seq,
                    RecordKind::Publication,
                    json!({
                        "project_id":"proj_q","root_revision_id":root,"label":"release",
                        "published_at":published_at,"actor":"human:test"
                    }),
                ),
            );
        }
        context.seq = 16;
        let before = search(&context, json!({
            "project_id":"proj_q","stage":"official","scope":"project_history","query":"미래 경보",
            "effective_at":"2026-08-31T23:59:59Z"
        })).unwrap();
        assert!(before["results"].as_array().unwrap().is_empty());
        let after = search(&context, json!({
            "project_id":"proj_q","stage":"official","scope":"project_history","query":"미래 경보",
            "effective_at":"2026-09-01T00:00:00Z"
        })).unwrap();
        assert_eq!(after["results"][0]["revision_id"], "rev_future");
    }

    #[test]
    fn default_search_walk_is_not_truncated_at_five_hundred_nodes() {
        let mut context = related_context();
        let mut slots = Vec::new();
        for index in 0..520 {
            let entity_id = format!("wide_entity_{index}");
            let revision_id = format!("wide_revision_{index}");
            context.records.insert(
                entity_id.clone(),
                fixtures::entity(&entity_id, 20 + index as i64 * 2, "idea", "proj_q"),
            );
            let body = if index == 519 {
                "마지막 탐색표식"
            } else {
                "일반 항목"
            };
            context.records.insert(
                revision_id.clone(),
                fixtures::revision(
                    &revision_id,
                    21 + index as i64 * 2,
                    &entity_id,
                    body,
                    json!([]),
                ),
            );
            slots.push(
                json!({"slot_id":format!("slot_{index}"),"revision_id":revision_id,"roles":["be"]}),
            );
        }
        context.records.insert(
            "rev_schema".into(),
            fixtures::revision(
                "rev_schema",
                7,
                "ent_schema",
                "넓은 스키마",
                Value::Array(slots),
            ),
        );
        context.seq = 1100;
        let found = search(
            &context,
            json!({"project_id":"proj_q","query":"마지막 탐색표식"}),
        )
        .unwrap();
        assert_eq!(found["results"][0]["revision_id"], "wide_revision_519");
        assert_eq!(found["truncated"], false);
        assert!(found["eligible_revisions"].as_u64().unwrap() > 500);
    }

    #[test]
    fn embeddings_are_selected_by_revision_model_dim_and_latest_sequence() {
        let mut context = related_context();
        context.records.insert(
            "embedding_a_old".into(),
            fixtures::record(
                "embedding_a_old",
                11,
                RecordKind::Embedding,
                json!({"revision_id":"rev_fe","model":"a","dim":2,"values":[1.0,0.0]}),
            ),
        );
        context.records.insert(
            "embedding_b".into(),
            fixtures::record(
                "embedding_b",
                12,
                RecordKind::Embedding,
                json!({"revision_id":"rev_fe","model":"b","dim":2,"values":[1.0,0.0]}),
            ),
        );
        context.records.insert(
            "embedding_a_new".into(),
            fixtures::record(
                "embedding_a_new",
                13,
                RecordKind::Embedding,
                json!({"revision_id":"rev_fe","model":"a","dim":2,"values":[0.0,1.0]}),
            ),
        );
        context.records.insert(
            "embedding_be".into(),
            fixtures::record(
                "embedding_be",
                14,
                RecordKind::Embedding,
                json!({"revision_id":"rev_be","model":"a","dim":2,"values":[1.0,0.0]}),
            ),
        );
        context.seq = 14;
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"","limit":1,
                "vector":{"model":"a","dim":2,"values":[0.0,1.0]}
            }),
        )
        .unwrap();
        assert_eq!(found["results"][0]["revision_id"], "rev_fe");
        assert_eq!(found["vector_status"]["with_matching_embedding"], 2);
        assert_eq!(found["vector_status"]["eligible_idea_revisions"], 2);
        assert_eq!(found["vector_status"]["ideas_with_matching_embedding"], 2);

        let role_scoped = search(
            &context,
            json!({
                "project_id":"proj_q","query":"","roles":["frontend"],
                "vector":{"model":"a","dim":2,"values":[0.0,1.0]}
            }),
        )
        .unwrap();
        assert_eq!(role_scoped["vector_status"]["eligible_idea_revisions"], 1);
        assert_eq!(
            role_scoped["vector_status"]["ideas_with_matching_embedding"],
            1
        );
    }

    #[test]
    fn nonrevision_results_obey_effective_time_and_occurrence_roles() {
        let mut context = related_context();
        context.records.insert(
            "observation".into(),
            fixtures::record(
                "observation",
                11,
                RecordKind::Observation,
                json!({
                    "project_id":"proj_q","target_revision_id":"rev_be","metric":"경보 처리",
                    "value":true,"status":"observed","occurred_at":"2026-08-02T09:00:00+09:00",
                    "method":"실행","actor":"human:test","note":"경보 관측 결과"
                }),
            ),
        );
        context.seq = 11;
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"경보를","kinds":["observation"],"roles":["backend"],
                "effective_at":"2026-08-02T00:00:00Z"
            }),
        )
        .unwrap();
        assert_eq!(found["records"][0]["record_id"], "observation");
        let wrong_role = search(
            &context,
            json!({
                "project_id":"proj_q","query":"경보","kinds":["observation"],"roles":["frontend"]
            }),
        )
        .unwrap();
        assert!(wrong_role["records"].as_array().unwrap().is_empty());
        let before = search(
            &context,
            json!({
                "project_id":"proj_q","query":"경보","kinds":["observation"],
                "effective_at":"2026-08-01T23:59:59.999Z"
            }),
        )
        .unwrap();
        assert!(before["records"].as_array().unwrap().is_empty());
    }

    #[test]
    fn all_supported_nonrevision_kinds_are_project_and_role_scoped() {
        let mut context = related_context();
        for (id, seq, kind, data) in [
            (
                "artifact_search",
                11,
                RecordKind::Artifact,
                json!({"uri":"file:///tmp/result.json","digest":"abcd","note":"경보 결과 파일"}),
            ),
            (
                "capture_search",
                12,
                RecordKind::Capture,
                json!({"project_id":"proj_q","content":"경보 원문","source_kind":"note","occurred_at":"2026-08-01T00:00:00Z"}),
            ),
            (
                "observation_search",
                13,
                RecordKind::Observation,
                json!({
                    "project_id":"proj_q","target_revision_id":"rev_be","metric":"경보 처리","value":42.5,"unit":"ms",
                    "status":"observed","occurred_at":"2026-08-02T00:00:00Z","method":"synthetic-run","actor":"human:test",
                    "environment":{"runtime":"cuda"},
                    "artifact_ids":["artifact_search"],"capture_id":"capture_search","note":"경보 관측"
                }),
            ),
            (
                "goal_search",
                14,
                RecordKind::Goal,
                json!({
                    "project_id":"proj_q","scope":{"root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be"},
                    "statement":"경보 처리 목표","criteria":[{"criterion_id":"ok","kind":"qualitative","statement":"통과","required":true}],
                    "origin":"official","actor":"human:test"
                }),
            ),
            (
                "baseline_search",
                15,
                RecordKind::Baseline,
                json!({"goal_id":"goal_search","criterion_ids":["ok"],"effective_from":"2026-08-01T00:00:00Z","actor":"human:test","reason":"adopt"}),
            ),
            (
                "assessment_search",
                16,
                RecordKind::Assessment,
                json!({
                    "root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be","baseline_id":"baseline_search",
                    "evidence_cutoff_seq":13,"evidence_cutoff_at":"2026-08-02T00:00:00Z","evaluator":"경보 평가자","rubric_version":"v1",
                    "origin":"official","status":"met","criteria_results":[{"criterion_id":"ok","status":"met"}],"evidence_observation_ids":["observation_search"]
                }),
            ),
        ] {
            context
                .records
                .insert(id.into(), fixtures::record(id, seq, kind, data));
        }
        context.seq = 16;
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"","roles":["BE"],
                "kinds":["capture","observation","goal","assessment","artifact"]
            }),
        )
        .unwrap();
        let kinds = found["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|row| row["kind"].as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            kinds,
            BTreeSet::from(["capture", "observation", "goal", "assessment", "artifact"])
        );
        assert!(found["results"].as_array().unwrap().is_empty());
        for query in ["42.5", "observed", "cuda", "synthetic-run"] {
            let typed = search(
                &context,
                json!({
                    "project_id":"proj_q","query":query,"roles":["BE"],"kinds":["observation"]
                }),
            )
            .unwrap();
            assert_eq!(
                typed["records"][0]["record_id"], "observation_search",
                "query={query}"
            );
        }
    }

    #[test]
    fn positive_similarity_expands_only_from_a_direct_hit() {
        let mut context = related_context();
        context.records.insert(
            "similar".into(),
            fixtures::record(
                "similar",
                11,
                RecordKind::Link,
                json!({
                    "link_type":"similar","from_id":"rev_fe","to_id":"rev_be","note":"",
                    "actor":"human:test","similarity":{"score":0.87,"method":"test"}
                }),
            ),
        );
        context.seq = 11;
        let found = search(&context, json!({"project_id":"proj_q","query":"프론트"})).unwrap();
        assert!(found["related"].as_array().unwrap().iter().any(|row| {
            row["revision_id"] == "rev_be"
                && row["reason"] == "similarity"
                && row["similarity_score"] == json!(0.87)
        }));
        let absent = search(&context, json!({"project_id":"proj_q","query":"없는 질의"})).unwrap();
        assert!(absent["related"].as_array().unwrap().is_empty());
    }

    #[test]
    fn similarity_keeps_same_role_neighbors_and_deduplicates_by_returned_id() {
        let mut context = related_context();
        context.records.insert(
            "ent_peer".into(),
            fixtures::entity("ent_peer", 11, "idea", "proj_q"),
        );
        context.records.insert(
            "rev_peer".into(),
            fixtures::revision("rev_peer", 12, "ent_peer", "백엔드 이웃", json!([])),
        );
        context.records.insert(
            "rev_schema".into(),
            fixtures::revision(
                "rev_schema",
                7,
                "ent_schema",
                "스키마",
                json!([
                    {"slot_id":"be","revision_id":"rev_be","roles":["be"]},
                    {"slot_id":"peer","revision_id":"rev_peer","roles":["backend"]},
                    {"slot_id":"fe","revision_id":"rev_fe","roles":["fe"]}
                ]),
            ),
        );
        for (id, seq, score) in [("similar_one", 13, 0.8), ("similar_two", 14, 0.7)] {
            context.records.insert(id.into(), fixtures::record(id, seq, RecordKind::Link, json!({
                "link_type":"similar","from_id":"rev_be","to_id":"rev_peer","actor":"human:test",
                "similarity":{"score":score,"method":"test"}
            })));
        }
        context.seq = 14;
        let found = search(
            &context,
            json!({"project_id":"proj_q","query":"정책","roles":["BE"]}),
        )
        .unwrap();
        let peer = found["related"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["revision_id"] == "rev_peer")
            .collect::<Vec<_>>();
        assert_eq!(peer.len(), 1);
        assert_eq!(peer[0]["reason"], "similarity");
    }

    #[test]
    fn negative_similarity_and_contradiction_are_counterevidence_not_positive_similarity() {
        let mut context = related_context();
        context.records.insert(
            "negative_similarity".into(),
            fixtures::record(
                "negative_similarity",
                11,
                RecordKind::Link,
                json!({
                    "link_type":"similar","from_id":"rev_fe","to_id":"rev_be","actor":"human:test",
                    "similarity":{"score":-0.4,"method":"test"}
                }),
            ),
        );
        context.records.insert(
            "counter_observation".into(),
            fixtures::record("counter_observation", 12, RecordKind::Observation, json!({
                "project_id":"proj_q","target_revision_id":"rev_fe","metric":"반증","value":false,
                "status":"negative","occurred_at":"2026-08-01T00:00:00Z","method":"test","actor":"human:test"
            })),
        );
        context.records.insert(
            "contradiction".into(),
            fixtures::record("contradiction", 13, RecordKind::Link, json!({
                "link_type":"contradict","from_id":"counter_observation","to_id":"rev_fe","actor":"human:test"
            })),
        );
        context.seq = 13;
        let found = search(&context, json!({"project_id":"proj_q","query":"프론트"})).unwrap();
        assert!(found["related"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["revision_id"] == "rev_be"
                && row["reason"] == "dissimilarity"
                && row["counterevidence"] == true));
        assert!(found["related"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["record_id"] == "counter_observation"
                && row["reason"] == "contradiction"
                && row["counterevidence"] == true));
    }

    #[test]
    fn candidate_lane_filters_to_project_and_query() {
        let mut context = related_context();
        context.records.insert(
            "candidate_local".into(),
            fixtures::record(
                "candidate_local",
                11,
                RecordKind::Candidate,
                json!({
                    "project_id":"proj_q","proposed_kind":"idea","title":"찾을 후보",
                    "body":"정책 후보","origin":"human","claim_mode":"inferred","status":"pending"
                }),
            ),
        );
        context.records.insert(
            "candidate_other".into(),
            fixtures::record(
                "candidate_other",
                12,
                RecordKind::Candidate,
                json!({
                    "project_id":"other","proposed_kind":"idea","title":"찾을 후보",
                    "body":"정책 후보","origin":"human","claim_mode":"inferred","status":"pending"
                }),
            ),
        );
        context.seq = 12;
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"찾을","lanes":["candidate"],"limit":20
            }),
        )
        .unwrap();
        assert_eq!(found["candidates"].as_array().unwrap().len(), 1);
        assert_eq!(found["candidates"][0]["candidate_id"], "candidate_local");
    }

    #[test]
    fn unrelated_links_do_not_create_related_results() {
        let mut context = related_context();
        context.records.insert(
            "ent_other".into(),
            fixtures::entity("ent_other", 11, "idea", "proj_q"),
        );
        context.records.insert(
            "rev_other".into(),
            fixtures::revision("rev_other", 12, "ent_other", "무관한 항목", json!([])),
        );
        context.records.insert(
            "rev_schema".into(),
            fixtures::revision(
                "rev_schema",
                7,
                "ent_schema",
                "스키마",
                json!([
                    {"slot_id":"be","revision_id":"rev_be","roles":["be"]},
                    {"slot_id":"fe","revision_id":"rev_fe","roles":["fe"]},
                    {"slot_id":"other","revision_id":"rev_other","roles":["be"]}
                ]),
            ),
        );
        context.records.insert(
            "lnk_unrelated".into(),
            fixtures::record(
                "lnk_unrelated",
                13,
                RecordKind::Link,
                json!({
                    "link_type":"depends","from_id":"ent_fe","to_id":"ent_other",
                    "note":"","actor":"human:test","impact":null,"similarity":null
                }),
            ),
        );
        context.seq = 13;
        let found = search(
            &context,
            json!({
                "project_id":"proj_q","query":"정책","roles":["fe"],"limit":20
            }),
        )
        .unwrap();
        assert!(found["related"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["revision_id"] != "rev_other"));
    }

    #[test]
    fn goals_choose_latest_effective_unsuperseded_baseline_not_id_order() {
        let mut context = related_context();
        context.records.insert(
            "goal_q".into(),
            fixtures::record(
                "goal_q",
                11,
                RecordKind::Goal,
                json!({
                    "project_id":"proj_q",
                    "scope":{"root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be"},
                    "statement":"정책 목표",
                    "criteria":[{"criterion_id":"required","kind":"qualitative","statement":"충족","required":true}],
                    "origin":"official","actor":"human:test"
                }),
            ),
        );
        for (id, seq, effective_from, supersedes) in [
            ("baseline_z_old", 12, "2026-01-01T00:00:00.000Z", None),
            (
                "baseline_a_new",
                14,
                "2026-02-01T00:00:00.000Z",
                Some("baseline_z_old"),
            ),
            (
                "baseline_0_future",
                16,
                "2026-03-01T00:00:00.000Z",
                Some("baseline_a_new"),
            ),
        ] {
            context.records.insert(
                id.into(),
                fixtures::record(
                    id,
                    seq,
                    RecordKind::Baseline,
                    json!({
                        "goal_id":"goal_q","criterion_ids":["required"],"constraints":[],
                        "effective_from":effective_from,"supersedes":supersedes,
                        "actor":"human:test","reason":"revision"
                    }),
                ),
            );
        }
        for (id, seq, baseline_id, status) in [
            ("assessment_old", 13, "baseline_z_old", "unmet"),
            ("assessment_new", 15, "baseline_a_new", "met"),
            ("assessment_future", 17, "baseline_0_future", "unmet"),
        ] {
            context.records.insert(
                id.into(),
                fixtures::record(
                    id,
                    seq,
                    RecordKind::Assessment,
                    json!({
                        "root_revision_id":"rev_root","slot_path":["schema","be"],
                        "target_revision_id":"rev_be","baseline_id":baseline_id,
                        "evidence_cutoff_seq":10,"evidence_cutoff_at":"2026-01-01T00:00:00.000Z",
                        "evaluator":"human:test","rubric_version":"v1","origin":"official",
                        "status":status,
                        "criteria_results":[{"criterion_id":"required","status":status,"note":""}]
                    }),
                ),
            );
        }
        context.seq = 17;

        let mut january = Params::new();
        january.insert("project_id".into(), "proj_q".into());
        january.insert("effective_at".into(), "2026-01-15T00:00:00.000Z".into());
        assert_eq!(
            goals(&context, &january).unwrap()["goals"][0]["gate_status"],
            "unmet"
        );

        let mut february = Params::new();
        february.insert("project_id".into(), "proj_q".into());
        february.insert("effective_at".into(), "2026-02-15T00:00:00.000Z".into());
        let found = goals(&context, &february).unwrap();
        assert_eq!(found["goals"][0]["gate_status"], "met");
        assert_eq!(found["goals"][0]["baselines"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn newer_active_unassessed_baseline_cannot_inherit_an_older_met_gate() {
        let mut context = related_context();
        context.records.insert(
            "goal_active".into(),
            fixtures::record(
                "goal_active",
                11,
                RecordKind::Goal,
                json!({
                    "project_id":"proj_q",
                    "scope":{"root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be"},
                    "statement":"활성 기준선 선택",
                    "criteria":[{"criterion_id":"required","kind":"qualitative","statement":"충족","required":true}],
                    "origin":"official","actor":"human:test"
                }),
            ),
        );
        for (id, seq, effective_from) in [
            ("baseline_old_met", 12, "2026-01-01T00:00:00Z"),
            ("baseline_new_unassessed", 14, "2026-02-01T00:00:00Z"),
        ] {
            context.records.insert(
                id.into(),
                fixtures::record(
                    id,
                    seq,
                    RecordKind::Baseline,
                    json!({
                        "goal_id":"goal_active","criterion_ids":["required"],"constraints":[],
                        "effective_from":effective_from,"actor":"human:test","reason":"adopt"
                    }),
                ),
            );
        }
        context.records.insert(
            "assessment_old_met".into(),
            fixtures::record(
                "assessment_old_met",
                13,
                RecordKind::Assessment,
                json!({
                    "root_revision_id":"rev_root","slot_path":["schema","be"],
                    "target_revision_id":"rev_be","baseline_id":"baseline_old_met",
                    "evidence_cutoff_seq":10,"evidence_cutoff_at":"2026-01-01T00:00:00Z",
                    "evaluator":"human:test","rubric_version":"v1","origin":"official",
                    "status":"met","criteria_results":[{"criterion_id":"required","status":"met"}]
                }),
            ),
        );
        context.seq = 14;

        let found = goals(
            &context,
            &Params::from([("project_id".into(), "proj_q".into())]),
        )
        .unwrap();
        let goal = &found["goals"][0];
        assert_eq!(goal["gate_status"], "unknown");
        assert!(goal["status"].is_null());
        assert_eq!(
            goal["required_criteria_status"][0]["criterion_id"],
            "required"
        );
        assert!(goal["required_criteria_status"][0]["status"].is_null());
        assert_eq!(
            goal["baselines"][1]["baseline_id"],
            "baseline_new_unassessed"
        );
        assert_eq!(goal["baselines"][1]["seq"], 14);
        assert_eq!(
            goal["baselines"][1]["recorded_at"],
            "2026-09-07T00:00:14.000Z"
        );
    }

    #[test]
    fn proposed_goal_keeps_scope_criteria_baselines_and_forecast_without_official_gate() {
        let mut context = related_context();
        context.records.insert(
            "goal_proposed".into(),
            fixtures::record(
                "goal_proposed",
                11,
                RecordKind::Goal,
                json!({
                    "project_id":"proj_q",
                    "scope":{"root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be"},
                    "statement":"장애 응답 시간을 계량화한다",
                    "criteria":[{
                        "criterion_id":"latency","kind":"quantitative","statement":"p95 2초 이하",
                        "metric":"response_p95_seconds","comparator":"lte","threshold":2.0,"unit":"seconds","required":true
                    }],
                    "origin":"ai_proposed","actor":"model:local"
                }),
            ),
        );
        context.records.insert(
            "baseline_proposed".into(),
            fixtures::record(
                "baseline_proposed",
                12,
                RecordKind::Baseline,
                json!({
                    "goal_id":"goal_proposed","criterion_ids":["latency"],"constraints":[],
                    "effective_from":"2026-01-01T00:00:00Z","actor":"model:local","reason":"forecast"
                }),
            ),
        );
        context.records.insert(
            "assessment_proposed".into(),
            fixtures::record(
                "assessment_proposed",
                13,
                RecordKind::Assessment,
                json!({
                    "root_revision_id":"rev_root","slot_path":["schema","be"],
                    "target_revision_id":"rev_be","baseline_id":"baseline_proposed",
                    "evidence_cutoff_seq":10,"evidence_cutoff_at":"2026-01-01T00:00:00Z",
                    "evaluator":"model:local","rubric_version":"forecast-v1","origin":"ai_proposed",
                    "status":"met","criteria_results":[{
                        "criterion_id":"latency","status":"met","observed_value":1.5,"note":"forecast"
                    }]
                }),
            ),
        );
        context.seq = 13;

        let found = goals(
            &context,
            &Params::from([("project_id".into(), "proj_q".into())]),
        )
        .unwrap();
        assert!(found["goals"].as_array().unwrap().is_empty());
        let proposed = &found["proposed_goals"][0];
        assert_eq!(proposed["scope"]["root_revision_id"], "rev_root");
        assert_eq!(proposed["scope"]["slot_path"], json!(["schema", "be"]));
        assert_eq!(proposed["scope"]["target_revision_id"], "rev_be");
        assert_eq!(proposed["scope"]["in_current_snapshot"], true);
        assert_eq!(proposed["criteria"][0]["metric"], "response_p95_seconds");
        assert_eq!(proposed["gate_status"], "met");
        assert_eq!(proposed["status"], "met");
        assert!(proposed["baselines"][0]["official_assessment"].is_null());
        assert_eq!(
            proposed["baselines"][0]["proposed_assessments"][0]["assessment_id"],
            "assessment_proposed"
        );
        assert!(found["missing_goal_occurrences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["slot_path"] == json!(["schema", "be"])));
    }

    fn stale_goal_context(new_schema_slots: Value) -> Context {
        let mut context = related_context();
        context.records.insert(
            "goal_stale".into(),
            fixtures::record("goal_stale", 11, RecordKind::Goal, json!({
                "project_id":"proj_q","scope":{"root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be"},
                "statement":"정책 목표","criteria":[{"criterion_id":"ok","kind":"qualitative","statement":"통과","required":true}],
                "origin":"official","actor":"human:test"
            })),
        );
        context.records.insert(
            "baseline_stale".into(),
            fixtures::record("baseline_stale", 12, RecordKind::Baseline, json!({
                "goal_id":"goal_stale","criterion_ids":["ok"],"effective_from":"2026-01-01T00:00:00Z","actor":"human:test","reason":"adopt"
            })),
        );
        context.records.insert(
            "assessment_stale".into(),
            fixtures::record("assessment_stale", 13, RecordKind::Assessment, json!({
                "root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be","baseline_id":"baseline_stale",
                "evidence_cutoff_seq":10,"evidence_cutoff_at":"2026-01-01T00:00:00Z","evaluator":"human:test","rubric_version":"v1",
                "origin":"official","status":"met","criteria_results":[{"criterion_id":"ok","status":"met"}]
            })),
        );
        context.records.insert(
            "rev_schema_new".into(),
            fixtures::revision(
                "rev_schema_new",
                14,
                "ent_schema",
                "새 스키마",
                new_schema_slots,
            ),
        );
        context.records.insert(
            "rev_root_new".into(),
            fixtures::revision(
                "rev_root_new",
                15,
                "proj_q",
                "새 루트",
                json!([
                    {"slot_id":"schema","revision_id":"rev_schema_new","roles":[]}
                ]),
            ),
        );
        context.records.insert(
            "hc_new".into(),
            fixtures::record("hc_new", 16, RecordKind::HeadChange, json!({
                "project_id":"proj_q","stage":"working","before_revision_id":"rev_root","after_revision_id":"rev_root_new",
                "reason":"change","actor":"human:test","meaningful":true,"decision":{"before":"old","after":"new","rationale":"test"}
            })),
        );
        context.seq = 16;
        context
    }

    #[test]
    fn stale_assessment_reports_current_path_applicability_without_granting_gate() {
        for (slots, expected, current_target) in [
            (
                json!([{"slot_id":"be","revision_id":"rev_be","roles":["be"]}]),
                "unchanged_target",
                Some("rev_be"),
            ),
            (
                json!([{"slot_id":"be","revision_id":"rev_fe","roles":["be"]}]),
                "target_changed",
                Some("rev_fe"),
            ),
            (json!([]), "path_removed", None),
        ] {
            let context = stale_goal_context(slots);
            let found = goals(
                &context,
                &Params::from([("project_id".into(), "proj_q".into())]),
            )
            .unwrap();
            let stale = &found["stale_assessments"][0];
            assert_eq!(stale["applicability"], expected);
            assert_eq!(stale["original_root"], "rev_root");
            assert_eq!(stale["current_root"], "rev_root_new");
            assert_eq!(stale["current_target_revision_id"], json!(current_target));
            assert_eq!(stale["requires_review"], true);
            assert_eq!(found["goals"][0]["gate_status"], "unknown");
            assert!(found["goals"][0]["baselines"][0]["official_assessment"].is_null());
        }
    }

    #[test]
    fn old_root_goal_does_not_satisfy_a_current_occurrence_goal_requirement() {
        let context =
            stale_goal_context(json!([{"slot_id":"be","revision_id":"rev_be","roles":["be"]}]));
        let found = goals(
            &context,
            &Params::from([("project_id".into(), "proj_q".into())]),
        )
        .unwrap();
        assert_eq!(found["goals"][0]["scope"]["in_current_snapshot"], false);
        assert!(found["missing_goal_occurrences"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| {
                row["slot_path"] == json!(["schema", "be"]) && row["revision_id"] == "rev_be"
            }));
    }
    #[test]
    fn assessments_cannot_leak_future_evidence_into_an_earlier_effective_view() {
        let mut ctx = related_context();
        for (id, seq, kind, data) in [
            (
                "goal_time",
                11,
                RecordKind::Goal,
                json!({
                    "project_id":"proj_q", "scope":{"root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be"},
                    "statement":"Time bounded result", "criteria":[{"criterion_id":"ok","kind":"qualitative","statement":"Measured success","required":true}],
                    "origin":"official", "actor":"human:test"
                }),
            ),
            (
                "baseline_time",
                12,
                RecordKind::Baseline,
                json!({
                    "goal_id":"goal_time","criterion_ids":["ok"],"effective_from":"2026-08-01T00:00:00Z", "actor":"human:test","reason":"adopt"
                }),
            ),
            (
                "obs_time",
                13,
                RecordKind::Observation,
                json!({
                    "project_id":"proj_q","target_revision_id":"rev_be","metric":"passes","value":true,
                    "status":"observed","occurred_at":"2026-08-03T09:00:00+09:00","method":"executed test", "actor":"human:test"
                }),
            ),
            (
                "assessment_time",
                14,
                RecordKind::Assessment,
                json!({
                    "root_revision_id":"rev_root","slot_path":["schema","be"],"target_revision_id":"rev_be","baseline_id":"baseline_time",
                    "evidence_cutoff_seq":13,"evidence_cutoff_at":"2026-08-03T09:00:00+09:00","evaluator":"human:test","rubric_version":"v1",
                    "origin":"official","status":"met","criteria_results":[{"criterion_id":"ok","status":"met"}],"evidence_observation_ids":["obs_time"]
                }),
            ),
        ] {
            ctx.records
                .insert(id.into(), fixtures::record(id, seq, kind, data));
        }
        ctx.seq = 14;
        let before = Params::from([(
            "effective_at".into(),
            "2026-08-03T08:59:59.999+09:00".into(),
        )]);
        let detail = record(&ctx, "rev_be", &before).unwrap();
        assert!(detail["evidence"]["observations"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(
            detail["evidence"]["assessments"]
                .as_array()
                .unwrap()
                .is_empty(),
            "an assessment exposes future observations indirectly"
        );
        let mut scope = before;
        scope.insert("project_id".into(), "proj_q".into());
        assert_eq!(
            goals(&ctx, &scope).unwrap()["goals"][0]["gate_status"],
            "unknown"
        );
        scope.insert("effective_at".into(), "2026-08-03T00:00:00Z".into());
        assert_eq!(
            goals(&ctx, &scope).unwrap()["goals"][0]["gate_status"],
            "met"
        );
    }
}
