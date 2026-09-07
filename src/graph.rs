//! Pure composition logic over a map of records: slot-path resolution and
//! bounded occurrence traversal. No database access, so it is unit-testable.

use std::collections::{BTreeMap, HashSet};

use serde_json::{json, Value};

use crate::model::{NodeKind, RecordData, RecordKind, StoredRecord};

pub const MAX_DEPTH: usize = 32;

pub trait Lookup {
    fn get(&self, id: &str) -> Option<&StoredRecord>;
}

impl Lookup for BTreeMap<String, StoredRecord> {
    fn get(&self, id: &str) -> Option<&StoredRecord> {
        BTreeMap::get(self, id)
    }
}

/// Existing records plus the records a package is about to write.
pub struct Overlay<'a> {
    pub base: &'a dyn Lookup,
    pub added: BTreeMap<String, &'a StoredRecord>,
}

impl<'a> Overlay<'a> {
    pub fn new(base: &'a dyn Lookup, added: impl Iterator<Item = &'a StoredRecord>) -> Self {
        Overlay {
            base,
            added: added.map(|r| (r.id.clone(), r)).collect(),
        }
    }
}

impl Lookup for Overlay<'_> {
    fn get(&self, id: &str) -> Option<&StoredRecord> {
        match self.added.get(id) {
            Some(r) => Some(r),
            None => self.base.get(id),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PathError {
    RootMissing,
    RootNotRevision,
    SlotMissing {
        at: Vec<String>,
        slot_id: String,
    },
    ChildMissing {
        at: Vec<String>,
        revision_id: String,
    },
    TooDeep,
}

impl PathError {
    pub fn message(&self) -> String {
        match self {
            PathError::RootMissing => "root revision not found".to_string(),
            PathError::RootNotRevision => "root id is not a revision".to_string(),
            PathError::SlotMissing { at, slot_id } => {
                format!("slot '{slot_id}' not found at path {}", render_path(at))
            }
            PathError::ChildMissing { at, revision_id } => {
                format!(
                    "revision {revision_id} not found at path {}",
                    render_path(at)
                )
            }
            PathError::TooDeep => format!("slot path deeper than {MAX_DEPTH}"),
        }
    }
}

pub fn render_path(path: &[String]) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", path.join("/"))
    }
}

/// Containment kind of a revision. `project` records used as `entity_id` give a
/// project-root revision. `docs/api.md` §3.3.
pub fn node_kind(lk: &dyn Lookup, revision: &StoredRecord) -> Option<NodeKind> {
    let rev = revision.as_revision()?;
    let owner = lk.get(&rev.entity_id)?;
    match &owner.data {
        RecordData::Project(_) => Some(NodeKind::Project),
        RecordData::Entity(e) => Some(NodeKind::from_entity(e.entity_kind)),
        _ => None,
    }
}

/// Origin project of a revision: the entity's origin project, or the project
/// itself for a project-root revision.
pub fn origin_project(lk: &dyn Lookup, revision: &StoredRecord) -> Option<String> {
    let rev = revision.as_revision()?;
    let owner = lk.get(&rev.entity_id)?;
    match &owner.data {
        RecordData::Project(_) => Some(owner.id.clone()),
        RecordData::Entity(e) => Some(e.project_id.clone()),
        _ => None,
    }
}

/// Follow `slot_path` from `root_id` and return the revision it pins.
pub fn resolve_path<'a>(
    lk: &'a dyn Lookup,
    root_id: &str,
    slot_path: &[String],
) -> Result<&'a StoredRecord, PathError> {
    if slot_path.len() > MAX_DEPTH {
        return Err(PathError::TooDeep);
    }
    let mut current = lk.get(root_id).ok_or(PathError::RootMissing)?;
    if current.kind() != RecordKind::Revision {
        return Err(PathError::RootNotRevision);
    }
    let mut walked: Vec<String> = Vec::new();
    for slot_id in slot_path {
        let rev = current.as_revision().ok_or(PathError::RootNotRevision)?;
        let slot = rev
            .slots
            .iter()
            .find(|s| &s.slot_id == slot_id)
            .ok_or_else(|| PathError::SlotMissing {
                at: walked.clone(),
                slot_id: slot_id.clone(),
            })?;
        walked.push(slot_id.clone());
        current = lk
            .get(&slot.revision_id)
            .filter(|r| r.kind() == RecordKind::Revision)
            .ok_or_else(|| PathError::ChildMissing {
                at: walked.clone(),
                revision_id: slot.revision_id.clone(),
            })?;
    }
    Ok(current)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub slot_path: Vec<String>,
    pub revision_id: String,
    pub roles: Vec<String>,
    pub parent_revision_id: Option<String>,
    pub depth: usize,
}

#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_nodes: usize,
    pub max_depth: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_nodes: 500,
            max_depth: MAX_DEPTH,
        }
    }
}

#[derive(Debug, Default)]
pub struct Walk {
    /// Every use site, in depth-first order. A shared revision appears once per
    /// use site; that is what makes an occurrence addressable.
    pub occurrences: Vec<Occurrence>,
    /// Distinct revision ids in first-visit order.
    pub revisions: Vec<String>,
    pub truncated: bool,
    pub max_depth_reached: usize,
    /// Referenced ids that are absent from the lookup.
    pub missing: Vec<String>,
}

/// Depth-first traversal of the pinned composition under `root_id`.
pub fn walk(lk: &dyn Lookup, root_id: &str, limits: Limits) -> Walk {
    let mut out = Walk::default();
    let Some(root) = lk.get(root_id).filter(|r| r.kind() == RecordKind::Revision) else {
        out.missing.push(root_id.to_string());
        return out;
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut on_path: Vec<String> = Vec::new();
    visit(
        lk,
        root,
        Vec::new(),
        Vec::new(),
        None,
        0,
        limits,
        &mut out,
        &mut seen,
        &mut on_path,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn visit(
    lk: &dyn Lookup,
    node: &StoredRecord,
    slot_path: Vec<String>,
    roles: Vec<String>,
    parent: Option<String>,
    depth: usize,
    limits: Limits,
    out: &mut Walk,
    seen: &mut HashSet<String>,
    on_path: &mut Vec<String>,
) {
    if out.occurrences.len() >= limits.max_nodes {
        out.truncated = true;
        return;
    }
    out.max_depth_reached = out.max_depth_reached.max(depth);
    out.occurrences.push(Occurrence {
        slot_path: slot_path.clone(),
        revision_id: node.id.clone(),
        roles,
        parent_revision_id: parent,
        depth,
    });
    if seen.insert(node.id.clone()) {
        out.revisions.push(node.id.clone());
    }
    if depth >= limits.max_depth {
        out.truncated |= node.as_revision().is_some_and(|r| !r.slots.is_empty());
        return;
    }
    // Defensive: stored data is validated acyclic, but an imported or
    // hand-edited graph must not loop forever here.
    if on_path.iter().any(|id| id == &node.id) {
        return;
    }
    on_path.push(node.id.clone());
    if let Some(rev) = node.as_revision() {
        for slot in &rev.slots {
            let mut child_path = slot_path.clone();
            child_path.push(slot.slot_id.clone());
            match lk.get(&slot.revision_id) {
                Some(child) if child.kind() == RecordKind::Revision => visit(
                    lk,
                    child,
                    child_path,
                    slot.roles.clone(),
                    Some(node.id.clone()),
                    depth + 1,
                    limits,
                    out,
                    seen,
                    on_path,
                ),
                _ => out.missing.push(slot.revision_id.clone()),
            }
        }
    }
    on_path.pop();
}

/// Nested JSON tree for `GET /api/snapshot`.
pub fn tree_json(lk: &dyn Lookup, root_id: &str, limits: Limits) -> (Value, usize, bool, usize) {
    let mut budget = limits.max_nodes;
    let mut truncated = false;
    let mut deepest = 0usize;
    let mut counted = 0usize;
    let value = build_tree(
        lk,
        root_id,
        None,
        Vec::new(),
        Vec::new(),
        0,
        limits,
        &mut budget,
        &mut truncated,
        &mut deepest,
        &mut counted,
    );
    (value, counted, truncated, deepest)
}

#[allow(clippy::too_many_arguments)]
fn build_tree(
    lk: &dyn Lookup,
    revision_id: &str,
    slot_id: Option<String>,
    slot_path: Vec<String>,
    roles: Vec<String>,
    depth: usize,
    limits: Limits,
    budget: &mut usize,
    truncated: &mut bool,
    deepest: &mut usize,
    counted: &mut usize,
) -> Value {
    if *budget == 0 {
        *truncated = true;
        return json!({
            "revision_id": revision_id, "slot_id": slot_id, "slot_path": slot_path,
            "roles": roles, "depth": depth, "children": [], "elided": true
        });
    }
    *budget -= 1;
    *counted += 1;
    *deepest = (*deepest).max(depth);
    let node = lk
        .get(revision_id)
        .filter(|r| r.kind() == RecordKind::Revision);
    let mut children = Vec::new();
    let mut elided = false;
    match node {
        None => elided = true,
        Some(node) => {
            if depth >= limits.max_depth {
                if node.as_revision().is_some_and(|r| !r.slots.is_empty()) {
                    elided = true;
                    *truncated = true;
                }
            } else if let Some(rev) = node.as_revision() {
                for slot in &rev.slots {
                    if *budget == 0 {
                        elided = true;
                        *truncated = true;
                        break;
                    }
                    let mut child_path = slot_path.clone();
                    child_path.push(slot.slot_id.clone());
                    children.push(build_tree(
                        lk,
                        &slot.revision_id,
                        Some(slot.slot_id.clone()),
                        child_path,
                        slot.roles.clone(),
                        depth + 1,
                        limits,
                        budget,
                        truncated,
                        deepest,
                        counted,
                    ));
                }
            }
        }
    }
    let mut obj = json!({
        "revision_id": revision_id,
        "slot_id": slot_id,
        "slot_path": slot_path,
        "roles": roles,
        "depth": depth,
        "children": children,
    });
    if elided {
        obj["elided"] = json!(true);
    }
    obj
}

#[cfg(test)]
pub mod fixtures {
    use super::*;
    use crate::model::*;
    use serde_json::json;

    pub fn record(id: &str, seq: i64, kind: RecordKind, data: Value) -> StoredRecord {
        StoredRecord {
            id: id.to_string(),
            seq,
            recorded_at: format!("2026-09-07T00:00:{:02}.000Z", seq.min(59)),
            data: RecordData::parse(kind, data).expect("fixture payload"),
        }
    }

    pub fn project(id: &str, seq: i64) -> StoredRecord {
        record(id, seq, RecordKind::Project, json!({"title": id}))
    }

    pub fn entity(id: &str, seq: i64, kind: &str, project: &str) -> StoredRecord {
        record(
            id,
            seq,
            RecordKind::Entity,
            json!({"entity_kind": kind, "project_id": project, "title": id}),
        )
    }

    pub fn revision(id: &str, seq: i64, entity_id: &str, body: &str, slots: Value) -> StoredRecord {
        record(
            id,
            seq,
            RecordKind::Revision,
            json!({
                "entity_id": entity_id,
                "body": body,
                "slots": slots,
                "change_kind": "initial",
                "source": {"origin": "human", "claim_mode": "inferred"}
            }),
        )
    }

    /// Six levels deep with the same atom reused through two paths (diamond).
    pub fn deep_diamond() -> BTreeMap<String, StoredRecord> {
        let mut m = BTreeMap::new();
        let mut push = |r: StoredRecord| {
            m.insert(r.id.clone(), r);
        };
        push(project("proj_login", 1));
        push(entity("ent_lockout", 2, "idea", "proj_login"));
        push(revision(
            "rev_lockout_1",
            3,
            "ent_lockout",
            "5회 실패 시 30분 잠금",
            json!([]),
        ));
        for (i, name) in ["l5", "l4", "l3", "l2"].iter().enumerate() {
            push(entity(
                &format!("ent_{name}"),
                10 + i as i64,
                "core",
                "proj_login",
            ));
        }
        push(revision(
            "rev_l5",
            20,
            "ent_l5",
            "레벨5",
            json!([{"slot_id": "policy", "revision_id": "rev_lockout_1", "roles": ["be"]}]),
        ));
        push(revision(
            "rev_l4",
            21,
            "ent_l4",
            "레벨4",
            json!([{"slot_id": "inner", "revision_id": "rev_l5", "roles": []}]),
        ));
        push(revision(
            "rev_l3",
            22,
            "ent_l3",
            "레벨3",
            json!([{"slot_id": "session", "revision_id": "rev_l4", "roles": []}]),
        ));
        push(entity("ent_auth", 30, "schema", "proj_login"));
        push(revision(
            "rev_auth",
            31,
            "ent_auth",
            "인증 스키마",
            json!([
                {"slot_id": "session", "revision_id": "rev_l3", "roles": ["be"]},
                {"slot_id": "direct_policy", "revision_id": "rev_lockout_1", "roles": ["fe"]}
            ]),
        ));
        push(entity("ent_billing", 40, "schema", "proj_login"));
        push(revision(
            "rev_billing",
            41,
            "ent_billing",
            "결제 스키마",
            json!([{"slot_id": "policy", "revision_id": "rev_lockout_1", "roles": ["be"]}]),
        ));
        push(revision(
            "rev_root",
            50,
            "proj_login",
            "로그인 프로젝트 루트",
            json!([
                {"slot_id": "auth", "revision_id": "rev_auth", "roles": []},
                {"slot_id": "billing", "revision_id": "rev_billing", "roles": []}
            ]),
        ));
        m
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    #[test]
    fn resolves_a_six_level_path_to_the_shared_atom() {
        let g = deep_diamond();
        let path: Vec<String> = ["auth", "session", "session", "inner", "policy"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let found = resolve_path(&g, "rev_root", &path).expect("path resolves");
        assert_eq!(found.id, "rev_lockout_1");
        assert_eq!(path.len(), 5, "root + 5 slots = 6 levels");
    }

    #[test]
    fn missing_slot_names_the_position() {
        let g = deep_diamond();
        let path = vec!["auth".to_string(), "nope".to_string()];
        let err = resolve_path(&g, "rev_root", &path).unwrap_err();
        assert_eq!(
            err,
            PathError::SlotMissing {
                at: vec!["auth".to_string()],
                slot_id: "nope".to_string()
            }
        );
    }

    #[test]
    fn diamond_reuse_yields_distinct_occurrences_of_one_revision() {
        let g = deep_diamond();
        let w = walk(&g, "rev_root", Limits::default());
        let uses: Vec<&Occurrence> = w
            .occurrences
            .iter()
            .filter(|o| o.revision_id == "rev_lockout_1")
            .collect();
        assert_eq!(uses.len(), 3, "one deep use plus two direct uses");
        assert_eq!(
            w.revisions
                .iter()
                .filter(|id| *id == "rev_lockout_1")
                .count(),
            1
        );
        assert!(w.missing.is_empty());
        assert!(!w.truncated);
        // Root at depth 0 plus five slots: six levels.
        assert_eq!(w.max_depth_reached, 5);
    }

    #[test]
    fn walk_respects_the_node_budget() {
        let g = deep_diamond();
        let w = walk(
            &g,
            "rev_root",
            Limits {
                max_nodes: 4,
                max_depth: MAX_DEPTH,
            },
        );
        assert!(w.truncated);
        assert_eq!(w.occurrences.len(), 4);
    }

    #[test]
    fn node_kind_reads_project_root_revisions() {
        let g = deep_diamond();
        assert_eq!(
            node_kind(&g, g.get("rev_root").unwrap()),
            Some(NodeKind::Project)
        );
        assert_eq!(
            node_kind(&g, g.get("rev_auth").unwrap()),
            Some(NodeKind::Schema)
        );
        assert_eq!(
            node_kind(&g, g.get("rev_lockout_1").unwrap()),
            Some(NodeKind::Idea)
        );
    }

    #[test]
    fn actual_tree_output_never_exceeds_the_budget() {
        fn count(v: &Value) -> usize {
            1 + v["children"]
                .as_array()
                .unwrap()
                .iter()
                .map(count)
                .sum::<usize>()
        }
        let g = deep_diamond();
        let (tree, reported, truncated, _) = tree_json(
            &g,
            "rev_root",
            Limits {
                max_nodes: 3,
                max_depth: MAX_DEPTH,
            },
        );
        assert!(truncated);
        assert_eq!(reported, 3);
        assert_eq!(count(&tree), reported);
    }
}
