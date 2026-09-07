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
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Vector {
    model: String,
    dim: usize,
    values: Vec<f64>,
}
fn tokens(s: &str) -> Vec<String> {
    let cs: Vec<_> = s.to_lowercase().chars().collect();
    let mut out = Vec::new();
    let mut w = String::new();
    for c in &cs {
        if c.is_alphanumeric() {
            w.push(*c)
        } else if !w.is_empty() {
            out.push(std::mem::take(&mut w))
        }
    }
    if !w.is_empty() {
        out.push(w)
    }
    out.extend(
        cs.windows(2)
            .filter(|x| x.iter().all(|c| ('\u{3400}'..='\u{9fff}').contains(c)))
            .map(|x| x.iter().collect()),
    );
    out
}
fn lex(q: &[String], s: &str) -> f64 {
    let h = tokens(s);
    q.iter()
        .map(|x| h.iter().filter(|y| *y == x).count() as f64)
        .sum()
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
    let v = view(ctx, &p)?;
    let s = stage(&p)?;
    let limit = q.limit.unwrap_or(limits::DEFAULT_SEARCH_LIMIT);
    if limit == 0 || limit > limits::MAX_SEARCH_LIMIT {
        return Err(bad("limit must be 1..=200"));
    }
    let (root_id, meta) = selected(&v, &p, s, &q.project_id)?;
    let Some(root_id) = root_id else {
        return Ok(
            json!({"mode":"lexical","stage":s.as_str(),"selected_by":meta,"results":[],"related":[],"candidates":[],"vector_status":{"requested":q.vector.is_some(),"used":false},"eligible_revisions":0,"truncated":false}),
        );
    };
    let walk = graph::walk(&v.ctx, &root_id, Limits::default());
    let words = tokens(&q.query);
    let mut em: HashMap<String, &EmbeddingData> = HashMap::new();
    for r in v.ctx.records.values() {
        if let Some(e) = r.as_embedding() {
            em.insert(e.revision_id.clone(), e);
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
    let mut candidates = Vec::new();
    for id in &walk.revisions {
        let r = &v.ctx.records[id];
        let d = r.as_revision().unwrap();
        if !q.tags.iter().all(|x| d.tags.contains(x)) {
            continue;
        }
        let occ: Vec<_> = walk
            .occurrences
            .iter()
            .filter(|x| x.revision_id == *id)
            .collect();
        if !q.roles.is_empty()
            && !occ
                .iter()
                .any(|x| x.roles.iter().any(|z| q.roles.contains(z)))
        {
            continue;
        }
        let (_, _, title) = owner(&v.ctx.records, r);
        let vs = vector.and_then(|x| {
            em.get(id)
                .filter(|e| e.model == x.model && e.dim == x.dim)
                .and_then(|e| cosine(&x.values, &e.values))
        });
        let lexical = lex(
            &words,
            &format!("{} {} {}", title, d.body, d.tags.join(" ")),
        );
        if lexical > 0.0 || vs.is_some() || words.is_empty() && vector.is_none() {
            candidates.push((id.clone(), lexical, vs, occ));
        }
    }
    let mut lexical = candidates
        .iter()
        .filter(|candidate| candidate.1 > 0.0)
        .collect::<Vec<_>>();
    lexical.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let lexical_rank: HashMap<_, _> = lexical
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.0.clone(), index + 1))
        .collect();
    let mut vr = candidates
        .iter()
        .filter_map(|x| x.2.map(|s| (x.0.clone(), s)))
        .collect::<Vec<_>>();
    vr.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let vpos: HashMap<_, _> = vr
        .iter()
        .enumerate()
        .map(|(i, (id, _))| (id.clone(), i + 1))
        .collect();
    candidates.sort_by(|a, b| {
        let score = |candidate: &(String, f64, Option<f64>, Vec<&graph::Occurrence>)| {
            lexical_rank
                .get(&candidate.0)
                .map_or(0.0, |rank| 1.0 / (60.0 + *rank as f64))
                + vpos
                    .get(&candidate.0)
                    .map_or(0.0, |rank| 1.0 / (60.0 + *rank as f64))
        };
        score(b)
            .partial_cmp(&score(a))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    let results = candidates
        .iter()
        .take(limit)
        .map(|(id, _, _, occurrences)| {
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
                "occurrences": occurrences.iter().map(|occurrence| json!({
                    "slot_path": occurrence.slot_path,
                    "roles": occurrence.roles,
                })).collect::<Vec<_>>(),
                "lexical_rank": lexical_rank,
                "vector_rank": vector_rank,
                "score": score,
                "claim_mode": revision.source.claim_mode,
                "origin": revision.source.origin,
            })
        })
        .collect::<Vec<_>>();
    let ids: HashSet<_> = walk.revisions.iter().cloned().collect();
    let direct_entities: HashSet<&str> = results
        .iter()
        .filter_map(|row| row["entity_id"].as_str())
        .collect();
    let direct_revisions: HashSet<&str> = results
        .iter()
        .filter_map(|row| row["revision_id"].as_str())
        .collect();
    let related = if q.roles.is_empty() {
        vec![]
    } else {
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        let mut relation_edges = Vec::new();
        for record in v.ctx.records.values() {
            if let Some(link) = record.as_link().filter(|link| {
                matches!(
                    link.link_type,
                    LinkType::Applies
                        | LinkType::Implements
                        | LinkType::Depends
                        | LinkType::Derived
                )
            }) {
                relation_edges.push((link.from_id.clone(), link.to_id.clone(), link.link_type));
            }
            if let Some(entity) = record.as_entity() {
                for source_id in &entity.derived_from {
                    relation_edges.push((record.id.clone(), source_id.clone(), LinkType::Derived));
                }
            }
        }
        for (from_id, to_id, link_type) in relation_edges {
            let endpoint_matches_query = [&from_id, &to_id].iter().any(|endpoint| {
                let revision_ids: Vec<&String> = match v.ctx.records.get(*endpoint) {
                    Some(record) if record.kind() == RecordKind::Revision => vec![&record.id],
                    Some(record) if record.kind() == RecordKind::Entity => walk
                        .revisions
                        .iter()
                        .filter(|revision_id| {
                            v.ctx
                                .records
                                .get(*revision_id)
                                .and_then(StoredRecord::as_revision)
                                .is_some_and(|revision| revision.entity_id == record.id)
                        })
                        .collect(),
                    _ => vec![],
                };
                revision_ids.into_iter().any(|revision_id| {
                    let revision = &v.ctx.records[revision_id];
                    let data = revision.as_revision().unwrap();
                    let (_, _, title) = owner(&v.ctx.records, revision);
                    lex(
                        &words,
                        &format!("{} {} {}", title, data.body, data.tags.join(" ")),
                    ) > 0.0
                })
            });
            if !endpoint_matches_query
                && !direct_entities.contains(from_id.as_str())
                && !direct_entities.contains(to_id.as_str())
                && !direct_revisions.contains(from_id.as_str())
                && !direct_revisions.contains(to_id.as_str())
            {
                continue;
            }
            for endpoint in [&from_id, &to_id] {
                let revision_ids: Vec<String> = match v.ctx.records.get(endpoint) {
                    Some(record) if record.kind() == RecordKind::Revision => {
                        vec![record.id.clone()]
                    }
                    Some(record) if record.kind() == RecordKind::Entity => walk
                        .revisions
                        .iter()
                        .filter(|revision_id| {
                            v.ctx
                                .records
                                .get(*revision_id)
                                .and_then(StoredRecord::as_revision)
                                .is_some_and(|revision| revision.entity_id == record.id)
                        })
                        .cloned()
                        .collect(),
                    _ => vec![],
                };
                for revision_id in revision_ids {
                    if !ids.contains(&revision_id) || !seen.insert(revision_id.clone()) {
                        continue;
                    }
                    let occurrences: Vec<_> = walk
                        .occurrences
                        .iter()
                        .filter(|occurrence| occurrence.revision_id == revision_id)
                        .collect();
                    if occurrences.iter().any(|occurrence| {
                        occurrence.roles.iter().any(|role| q.roles.contains(role))
                    }) {
                        continue;
                    }
                    let revision = &v.ctx.records[&revision_id];
                    let (entity_id, _, title) = owner(&v.ctx.records, revision);
                    rows.push(json!({
                        "revision_id": revision_id,
                        "entity_id": entity_id,
                        "title": title,
                        "reason": "role_mismatch",
                        "roles": occurrences.first().map(|occurrence| occurrence.roles.clone()).unwrap_or_default(),
                        "link_type": link_type,
                    }));
                }
            }
        }
        rows
    };
    let candidates = if q.lanes.is_empty() || q.lanes.iter().any(|x| x == "candidate") {
        let mut lane = v
            .ctx
            .records
            .values()
            .filter_map(|r| match &r.data {
                RecordData::Candidate(c) if c.project_id == q.project_id => {
                    let score = lex(&words, &format!("{} {}", c.title, c.body));
                    if words.is_empty() || score > 0.0 {
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
    let matching = walk
        .revisions
        .iter()
        .filter(|id| {
            vector
                .and_then(|x| em.get(*id).filter(|e| e.model == x.model && e.dim == x.dim))
                .is_some()
        })
        .count();
    Ok(json!({
        "mode": if matching > 0 { "lexical+vector" } else { "lexical" },
        "stage": s.as_str(),
        "selected_by": meta,
        "results": results,
        "related": related,
        "candidates": candidates,
        "vector_status": {
            "requested": q.vector.is_some(),
            "model": q.vector.as_ref().map(|vector| &vector.model),
            "dim": q.vector.as_ref().map(|vector| vector.dim),
            "eligible_revisions": walk.revisions.len(),
            "with_matching_embedding": matching,
            "used": matching > 0,
        },
        "eligible_revisions": walk.revisions.len(),
        "truncated": walk.truncated,
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
        if g.origin == ProposalOrigin::AiProposed {
            proposed.push(json!({"goal_id":gr.id,"statement":g.statement,"origin":g.origin}));
            continue;
        }
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
                    stale.push(json!({
                        "assessment_id": assessment_record.id,
                        "goal_id": gr.id,
                        "baseline_id": baseline_record.id,
                        "reason": if assessment.root_revision_id != root {
                            "root_not_in_snapshot"
                        } else {
                            "target_changed"
                        },
                        "status": assessment.status,
                        "recorded_at": assessment_record.recorded_at,
                    }));
                    continue;
                }
                let required = required_criteria_status(g, baseline, assessment);
                let projection = json!({
                    "assessment_id": assessment_record.id,
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
                if let Some(assessment) = official.as_ref() {
                    current_gate = Some((
                        effective_from,
                        baseline_record.seq,
                        baseline_record.id.clone(),
                        assessment["gate_status"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_string(),
                        assessment["required_criteria_status"]
                            .as_array()
                            .cloned()
                            .unwrap_or_default(),
                    ));
                }
            }
            bases.push(json!({
                "baseline_id": baseline_record.id,
                "effective_from": baseline.effective_from,
                "supersedes": baseline.supersedes,
                "criterion_ids": baseline.criterion_ids,
                "official_assessment": official,
                "superseded_by": successor.map(|(_, _, id)| id),
                "proposed_assessments": proposed_assessments,
            }));
        }
        let (gate, required) = if exact {
            current_gate
                .map(|(_, _, _, gate, required)| (gate, required))
                .unwrap_or_else(|| ("unknown".to_string(), Vec::new()))
        } else {
            ("unknown".to_string(), Vec::new())
        };
        rows.push(json!({"goal_id":gr.id,"statement":g.statement,"origin":g.origin,"scope":{"slot_path":g.scope.slot_path,"target_revision_id":g.scope.target_revision_id,"in_current_snapshot":exact},"origin_project_id":g.project_id,"criteria":g.criteria,"baselines":bases,"gate_status":gate,"required_criteria_status":required }));
    }
    let missing=walk.occurrences.iter().filter(|o|!rows.iter().any(|g|g["scope"]["slot_path"]==json!(o.slot_path)&&g["scope"]["target_revision_id"]==json!(o.revision_id))).filter_map(|o|v.ctx.records.get(&o.revision_id).map(|r|{let(e,k,_)=owner(&v.ctx.records,r);json!({"slot_path":o.slot_path,"revision_id":o.revision_id,"entity_id":e,"entity_kind":k})})).collect::<Vec<_>>();
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
