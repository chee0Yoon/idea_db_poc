//! Record kinds and their payloads. Every payload denies unknown fields so a
//! typo fails at the trust boundary. See `docs/api.md` §3.

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::util;

pub const PROTOCOL_VERSION: u32 = 1;

// ---------------------------------------------------------------- enums

macro_rules! snake_enum {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}

snake_enum!(RecordKind {
    Project,
    Entity,
    Revision,
    Capture,
    Candidate,
    Promotion,
    Artifact,
    Observation,
    Goal,
    Baseline,
    Assessment,
    Link,
    Embedding,
    HeadChange,
    Publication,
});

impl RecordKind {
    /// Neo4j label for the kind-specific label placed next to `:Record`.
    pub fn label(self) -> &'static str {
        match self {
            RecordKind::Project => "Project",
            RecordKind::Entity => "Entity",
            RecordKind::Revision => "Revision",
            RecordKind::Capture => "Capture",
            RecordKind::Candidate => "Candidate",
            RecordKind::Promotion => "Promotion",
            RecordKind::Artifact => "Artifact",
            RecordKind::Observation => "Observation",
            RecordKind::Goal => "Goal",
            RecordKind::Baseline => "Baseline",
            RecordKind::Assessment => "Assessment",
            RecordKind::Link => "Link",
            RecordKind::Embedding => "Embedding",
            RecordKind::HeadChange => "HeadChange",
            RecordKind::Publication => "Publication",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RecordKind::Project => "project",
            RecordKind::Entity => "entity",
            RecordKind::Revision => "revision",
            RecordKind::Capture => "capture",
            RecordKind::Candidate => "candidate",
            RecordKind::Promotion => "promotion",
            RecordKind::Artifact => "artifact",
            RecordKind::Observation => "observation",
            RecordKind::Goal => "goal",
            RecordKind::Baseline => "baseline",
            RecordKind::Assessment => "assessment",
            RecordKind::Link => "link",
            RecordKind::Embedding => "embedding",
            RecordKind::HeadChange => "head_change",
            RecordKind::Publication => "publication",
        }
    }

    pub fn parse_str(text: &str) -> Option<Self> {
        serde_json::from_value(Value::String(text.to_string())).ok()
    }

    /// Kinds the server writes itself; clients may not submit them in a package.
    pub fn is_server_only(self) -> bool {
        matches!(self, RecordKind::HeadChange | RecordKind::Publication)
    }
}

snake_enum!(EntityKind { Schema, Core, Idea });
snake_enum!(LineageKind {
    None,
    SemanticEdit,
    Split,
    Merge
});
snake_enum!(ChangeKind {
    Initial,
    Composition,
    Correction,
    Semantic
});
snake_enum!(Origin { Human, Ai, Import });
snake_enum!(ClaimMode {
    Extracted,
    Inferred
});
snake_enum!(SourceKind {
    Paste,
    File,
    Url,
    Note,
    Tool
});
snake_enum!(ProposedKind {
    Idea,
    Core,
    Schema,
    Goal,
    Link
});
snake_enum!(CandidateStatus { Pending });
snake_enum!(ObservationStatus {
    Observed,
    Failed,
    Invalid,
    NotObserved,
    Negative
});
snake_enum!(CriterionKind {
    Quantitative,
    Qualitative
});
snake_enum!(Comparator {
    Lt,
    Lte,
    Gt,
    Gte,
    Eq,
    Ne
});
snake_enum!(ProposalOrigin {
    Official,
    AiProposed
});
snake_enum!(JudgeStatus {
    Met,
    Unmet,
    Unknown,
    Disputed,
    Recheck
});
snake_enum!(LinkType {
    Derived,
    Applies,
    Implements,
    Depends,
    Similar,
    Support,
    Contradict,
    Impact,
});
snake_enum!(ImpactDirection {
    Increase,
    Decrease,
    Neutral,
    Unknown
});
snake_enum!(EvidenceKind {
    Estimated,
    Observed
});
snake_enum!(Stage { Working, Official });

impl LinkType {
    /// Whitelisted relationship type. Never built from client text.
    pub fn rel_type(self) -> &'static str {
        match self {
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
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Working => "working",
            Stage::Official => "official",
        }
    }
}

/// Containment kind of a revision: the entity kind, plus `Project` for a
/// project-root revision. `docs/api.md` §3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    Project,
    Schema,
    Core,
    Idea,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Project => "project",
            NodeKind::Schema => "schema",
            NodeKind::Core => "core",
            NodeKind::Idea => "idea",
        }
    }

    pub fn from_entity(kind: EntityKind) -> Self {
        match kind {
            EntityKind::Schema => NodeKind::Schema,
            EntityKind::Core => NodeKind::Core,
            EntityKind::Idea => NodeKind::Idea,
        }
    }

    /// `docs/api.md` §3.3 containment table.
    pub fn may_contain(self, child: NodeKind) -> bool {
        match self {
            NodeKind::Project => child == NodeKind::Schema,
            NodeKind::Schema | NodeKind::Core => {
                matches!(child, NodeKind::Core | NodeKind::Idea)
            }
            NodeKind::Idea => false,
        }
    }
}

// ---------------------------------------------------------------- payloads

fn default_media_type() -> String {
    "text/plain".to_string()
}

fn default_digest_alg() -> String {
    "sha256".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectData {
    pub title: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntityData {
    pub entity_kind: EntityKind,
    /// Origin project. Core/Idea may still be reused by other projects.
    pub project_id: String,
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub derived_from: Vec<String>,
    #[serde(default = "lineage_none")]
    pub lineage_kind: LineageKind,
}

fn lineage_none() -> LineageKind {
    LineageKind::None
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Slot {
    pub slot_id: String,
    pub revision_id: String,
    #[serde(default)]
    pub roles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAnchor {
    pub capture_id: String,
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub origin: Origin,
    #[serde(default)]
    pub capture_id: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub skill: Option<String>,
    pub claim_mode: ClaimMode,
    #[serde(default)]
    pub source_anchor: Option<SourceAnchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevisionData {
    /// An `entity` record, or a `project` record for a project-root revision.
    pub entity_id: String,
    pub body: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub slots: Vec<Slot>,
    pub change_kind: ChangeKind,
    #[serde(default)]
    pub correction_of: Option<String>,
    #[serde(default)]
    pub correction_reason: Option<String>,
    #[serde(default)]
    pub previous_revision_id: Option<String>,
    pub source: Source,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureData {
    #[serde(default)]
    pub project_id: Option<String>,
    pub content: String,
    #[serde(default)]
    pub content_digest: Option<String>,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    pub source_kind: SourceKind,
    #[serde(default)]
    pub source_ref: Option<String>,
    #[serde(default)]
    pub occurred_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateData {
    pub project_id: String,
    #[serde(default)]
    pub capture_id: Option<String>,
    pub proposed_kind: ProposedKind,
    pub title: String,
    pub body: String,
    pub origin: Origin,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub skill: Option<String>,
    pub claim_mode: ClaimMode,
    #[serde(default)]
    pub source_anchor: Option<SourceAnchor>,
    pub status: CandidateStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromotionData {
    pub candidate_id: String,
    pub entity_id: String,
    pub revision_id: String,
    pub actor: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactData {
    pub uri: String,
    pub digest: String,
    #[serde(default = "default_digest_alg")]
    pub digest_alg: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub media_type: Option<String>,
    /// Must be false: this release stores no artifact bytes.
    #[serde(default)]
    pub included_in_export: bool,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    #[serde(default)]
    pub code_ref: Option<String>,
    #[serde(default)]
    pub data_version: Option<String>,
    #[serde(default)]
    pub model_version: Option<String>,
    #[serde(default)]
    pub config_digest: Option<String>,
    #[serde(default)]
    pub runtime: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationData {
    pub project_id: String,
    #[serde(default)]
    pub target_revision_id: Option<String>,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default)]
    pub unit: Option<String>,
    pub status: ObservationStatus,
    pub occurred_at: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub environment: Environment,
    #[serde(default)]
    pub artifact_ids: Vec<String>,
    #[serde(default)]
    pub capture_id: Option<String>,
    pub actor: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub root_revision_id: String,
    #[serde(default)]
    pub slot_path: Vec<String>,
    pub target_revision_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Criterion {
    pub criterion_id: String,
    pub kind: CriterionKind,
    pub statement: String,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub comparator: Option<Comparator>,
    #[serde(default)]
    pub threshold: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoalData {
    pub project_id: String,
    pub scope: Scope,
    pub statement: String,
    pub criteria: Vec<Criterion>,
    pub origin: ProposalOrigin,
    pub actor: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaselineData {
    pub goal_id: String,
    pub criterion_ids: Vec<String>,
    #[serde(default)]
    pub constraints: Vec<String>,
    pub effective_from: String,
    /// May point at a baseline of a *different* goal with an identical scope.
    #[serde(default)]
    pub supersedes: Option<String>,
    pub actor: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CriterionResult {
    pub criterion_id: String,
    pub status: JudgeStatus,
    #[serde(default)]
    pub observed_value: Option<f64>,
    #[serde(default)]
    pub note: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_estimate: Option<ProgressEstimate>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProgressEstimate {
    pub percent: f64,
    pub rationale: String,
    pub evidence_record_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssessmentData {
    pub root_revision_id: String,
    #[serde(default)]
    pub slot_path: Vec<String>,
    pub target_revision_id: String,
    pub baseline_id: String,
    pub evidence_cutoff_seq: i64,
    pub evidence_cutoff_at: String,
    pub evaluator: String,
    pub rubric_version: String,
    pub origin: ProposalOrigin,
    pub status: JudgeStatus,
    pub criteria_results: Vec<CriterionResult>,
    #[serde(default)]
    pub evidence_observation_ids: Vec<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Impact {
    pub scope: String,
    pub direction: ImpactDirection,
    #[serde(default)]
    pub metric: Option<String>,
    #[serde(default)]
    pub magnitude: Option<f64>,
    #[serde(default)]
    pub unit: Option<String>,
    pub evidence_kind: EvidenceKind,
    #[serde(default)]
    pub observation_ids: Vec<String>,
    #[serde(default)]
    pub method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Similarity {
    pub score: f64,
    pub method: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkData {
    pub link_type: LinkType,
    pub from_id: String,
    pub to_id: String,
    #[serde(default)]
    pub note: String,
    pub actor: String,
    #[serde(default)]
    pub impact: Option<Impact>,
    #[serde(default)]
    pub similarity: Option<Similarity>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingData {
    pub revision_id: String,
    pub model: String,
    pub dim: usize,
    pub values: Vec<f64>,
    #[serde(default)]
    pub normalized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub before: String,
    pub after: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HeadChangeData {
    pub project_id: String,
    pub stage: Stage,
    #[serde(default)]
    pub before_revision_id: Option<String>,
    pub after_revision_id: String,
    pub reason: String,
    pub actor: String,
    pub meaningful: bool,
    pub decision: Decision,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationData {
    pub project_id: String,
    pub root_revision_id: String,
    pub label: String,
    pub published_at: String,
    #[serde(default)]
    pub notes: String,
    pub actor: String,
}

// ---------------------------------------------------------------- record

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum RecordData {
    Project(ProjectData),
    Entity(EntityData),
    Revision(RevisionData),
    Capture(CaptureData),
    Candidate(CandidateData),
    Promotion(PromotionData),
    Artifact(ArtifactData),
    Observation(ObservationData),
    Goal(GoalData),
    Baseline(BaselineData),
    Assessment(AssessmentData),
    Link(LinkData),
    Embedding(EmbeddingData),
    HeadChange(HeadChangeData),
    Publication(PublicationData),
}

impl RecordData {
    /// Parse a `data` object against the schema selected by `kind`. Unknown
    /// fields fail here, at the trust boundary.
    pub fn parse(kind: RecordKind, value: Value) -> Result<Self, String> {
        fn conv<T>(v: Value) -> Result<T, String>
        where
            T: for<'de> Deserialize<'de>,
        {
            serde_json::from_value(v).map_err(|e| e.to_string())
        }
        Ok(match kind {
            RecordKind::Project => RecordData::Project(conv(value)?),
            RecordKind::Entity => RecordData::Entity(conv(value)?),
            RecordKind::Revision => RecordData::Revision(conv(value)?),
            RecordKind::Capture => RecordData::Capture(conv(value)?),
            RecordKind::Candidate => RecordData::Candidate(conv(value)?),
            RecordKind::Promotion => RecordData::Promotion(conv(value)?),
            RecordKind::Artifact => RecordData::Artifact(conv(value)?),
            RecordKind::Observation => RecordData::Observation(conv(value)?),
            RecordKind::Goal => RecordData::Goal(conv(value)?),
            RecordKind::Baseline => RecordData::Baseline(conv(value)?),
            RecordKind::Assessment => RecordData::Assessment(conv(value)?),
            RecordKind::Link => RecordData::Link(conv(value)?),
            RecordKind::Embedding => RecordData::Embedding(conv(value)?),
            RecordKind::HeadChange => RecordData::HeadChange(conv(value)?),
            RecordKind::Publication => RecordData::Publication(conv(value)?),
        })
    }

    pub fn kind(&self) -> RecordKind {
        match self {
            RecordData::Project(_) => RecordKind::Project,
            RecordData::Entity(_) => RecordKind::Entity,
            RecordData::Revision(_) => RecordKind::Revision,
            RecordData::Capture(_) => RecordKind::Capture,
            RecordData::Candidate(_) => RecordKind::Candidate,
            RecordData::Promotion(_) => RecordKind::Promotion,
            RecordData::Artifact(_) => RecordKind::Artifact,
            RecordData::Observation(_) => RecordKind::Observation,
            RecordData::Goal(_) => RecordKind::Goal,
            RecordData::Baseline(_) => RecordKind::Baseline,
            RecordData::Assessment(_) => RecordKind::Assessment,
            RecordData::Link(_) => RecordKind::Link,
            RecordData::Embedding(_) => RecordKind::Embedding,
            RecordData::HeadChange(_) => RecordKind::HeadChange,
            RecordData::Publication(_) => RecordKind::Publication,
        }
    }
}

/// A record as stored: client-supplied identity and payload, server-supplied
/// `seq` and `recorded_at`.
#[derive(Debug, Clone)]
pub struct StoredRecord {
    pub id: String,
    pub seq: i64,
    pub recorded_at: String,
    pub data: RecordData,
}

impl StoredRecord {
    pub fn kind(&self) -> RecordKind {
        self.data.kind()
    }

    pub fn as_revision(&self) -> Option<&RevisionData> {
        match &self.data {
            RecordData::Revision(r) => Some(r),
            _ => None,
        }
    }

    pub fn as_entity(&self) -> Option<&EntityData> {
        match &self.data {
            RecordData::Entity(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_capture(&self) -> Option<&CaptureData> {
        match &self.data {
            RecordData::Capture(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_goal(&self) -> Option<&GoalData> {
        match &self.data {
            RecordData::Goal(g) => Some(g),
            _ => None,
        }
    }

    pub fn as_baseline(&self) -> Option<&BaselineData> {
        match &self.data {
            RecordData::Baseline(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_assessment(&self) -> Option<&AssessmentData> {
        match &self.data {
            RecordData::Assessment(a) => Some(a),
            _ => None,
        }
    }

    pub fn as_observation(&self) -> Option<&ObservationData> {
        match &self.data {
            RecordData::Observation(o) => Some(o),
            _ => None,
        }
    }

    pub fn as_embedding(&self) -> Option<&EmbeddingData> {
        match &self.data {
            RecordData::Embedding(e) => Some(e),
            _ => None,
        }
    }

    pub fn as_link(&self) -> Option<&LinkData> {
        match &self.data {
            RecordData::Link(l) => Some(l),
            _ => None,
        }
    }

    pub fn as_publication(&self) -> Option<&PublicationData> {
        match &self.data {
            RecordData::Publication(p) => Some(p),
            _ => None,
        }
    }

    pub fn as_head_change(&self) -> Option<&HeadChangeData> {
        match &self.data {
            RecordData::HeadChange(h) => Some(h),
            _ => None,
        }
    }

    /// Every record id this record points at. Drives the prefetch closure so
    /// validation sees the whole referenced subgraph.
    pub fn refs(&self) -> Vec<String> {
        fn opt(out: &mut Vec<String>, o: &Option<String>) {
            if let Some(v) = o {
                out.push(v.clone());
            }
        }
        let mut out: Vec<String> = Vec::new();
        match &self.data {
            RecordData::Project(_) | RecordData::Artifact(_) => {}
            RecordData::Entity(d) => {
                out.push(d.project_id.clone());
                out.extend(d.derived_from.iter().cloned());
            }
            RecordData::Revision(d) => {
                out.push(d.entity_id.clone());
                out.extend(d.slots.iter().map(|s| s.revision_id.clone()));
                opt(&mut out, &d.correction_of);
                opt(&mut out, &d.previous_revision_id);
                opt(&mut out, &d.source.capture_id);
                if let Some(a) = &d.source.source_anchor {
                    out.push(a.capture_id.clone());
                }
            }
            RecordData::Capture(d) => opt(&mut out, &d.project_id),
            RecordData::Candidate(d) => {
                out.push(d.project_id.clone());
                opt(&mut out, &d.capture_id);
                if let Some(a) = &d.source_anchor {
                    out.push(a.capture_id.clone());
                }
            }
            RecordData::Promotion(d) => {
                out.push(d.candidate_id.clone());
                out.push(d.entity_id.clone());
                out.push(d.revision_id.clone());
            }
            RecordData::Observation(d) => {
                out.push(d.project_id.clone());
                opt(&mut out, &d.target_revision_id);
                opt(&mut out, &d.capture_id);
                out.extend(d.artifact_ids.iter().cloned());
            }
            RecordData::Goal(d) => {
                out.push(d.project_id.clone());
                out.push(d.scope.root_revision_id.clone());
                out.push(d.scope.target_revision_id.clone());
            }
            RecordData::Baseline(d) => {
                out.push(d.goal_id.clone());
                opt(&mut out, &d.supersedes);
            }
            RecordData::Assessment(d) => {
                out.push(d.root_revision_id.clone());
                out.push(d.target_revision_id.clone());
                out.push(d.baseline_id.clone());
                out.extend(d.evidence_observation_ids.iter().cloned());
                for result in &d.criteria_results {
                    if let Some(estimate) = &result.progress_estimate {
                        out.extend(estimate.evidence_record_ids.iter().cloned());
                    }
                }
            }
            RecordData::Link(d) => {
                out.push(d.from_id.clone());
                out.push(d.to_id.clone());
                if let Some(i) = &d.impact {
                    out.extend(i.observation_ids.iter().cloned());
                }
            }
            RecordData::Embedding(d) => out.push(d.revision_id.clone()),
            RecordData::HeadChange(d) => {
                out.push(d.project_id.clone());
                out.push(d.after_revision_id.clone());
                opt(&mut out, &d.before_revision_id);
            }
            RecordData::Publication(d) => {
                out.push(d.project_id.clone());
                out.push(d.root_revision_id.clone());
            }
        }
        out
    }

    pub fn data_value(&self) -> Value {
        serde_json::to_value(&self.data).unwrap_or(Value::Null)
    }

    /// Wire form used by every read endpoint.
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "kind": self.kind().as_str(),
            "seq": self.seq,
            "recorded_at": self.recorded_at,
            "data": self.data_value(),
        })
    }

    /// Scalar properties duplicated next to the JSON payload so Cypher can
    /// filter without parsing `data`.
    pub fn scalars(&self) -> Map<String, Value> {
        let mut m = Map::new();
        let mut put = |k: &str, v: Value| {
            m.insert(k.to_string(), v);
        };
        match &self.data {
            RecordData::Project(d) => {
                put("project_id", json!(self.id));
                put("title", json!(d.title));
                put("text", json!(format!("{}\n{}", d.title, d.description)));
            }
            RecordData::Entity(d) => {
                put("project_id", json!(d.project_id));
                put("entity_kind", json!(d.entity_kind));
                put("title", json!(d.title));
                put("tags", json!(d.tags));
                put("text", json!(d.title));
            }
            RecordData::Revision(d) => {
                put("entity_id", json!(d.entity_id));
                put("tags", json!(d.tags));
                put("text", json!(d.body));
                put("change_kind", json!(d.change_kind));
                put("claim_mode", json!(d.source.claim_mode));
                put("origin", json!(d.source.origin));
            }
            RecordData::Capture(d) => {
                put("project_id", json!(d.project_id));
                put("text", json!(d.content));
                put("content_digest", json!(d.content_digest));
                put("occurred_at_utc", utc_key(d.occurred_at.as_deref()));
            }
            RecordData::Candidate(d) => {
                put("project_id", json!(d.project_id));
                put("title", json!(d.title));
                put("text", json!(d.body));
                put("status", json!(d.status));
                put("origin", json!(d.origin));
                put("claim_mode", json!(d.claim_mode));
                put("capture_id", json!(d.capture_id));
            }
            RecordData::Promotion(d) => {
                put("entity_id", json!(d.entity_id));
                put("revision_id", json!(d.revision_id));
                put("candidate_id", json!(d.candidate_id));
            }
            RecordData::Artifact(d) => {
                put("text", json!(d.uri));
            }
            RecordData::Observation(d) => {
                put("project_id", json!(d.project_id));
                put("revision_id", json!(d.target_revision_id));
                put("status", json!(d.status));
                put("occurred_at_utc", utc_key(Some(&d.occurred_at)));
                put("text", json!(d.note));
            }
            RecordData::Goal(d) => {
                put("project_id", json!(d.project_id));
                put("revision_id", json!(d.scope.target_revision_id));
                put("origin", json!(d.origin));
                put("text", json!(d.statement));
            }
            RecordData::Baseline(d) => {
                put("goal_id", json!(d.goal_id));
                put("text", json!(d.reason));
            }
            RecordData::Assessment(d) => {
                put("revision_id", json!(d.target_revision_id));
                put("baseline_id", json!(d.baseline_id));
                put("status", json!(d.status));
                put("origin", json!(d.origin));
                put("text", json!(d.note));
            }
            RecordData::Link(d) => {
                put("link_type", json!(d.link_type));
                put("from_id", json!(d.from_id));
                put("to_id", json!(d.to_id));
                put("text", json!(d.note));
            }
            RecordData::Embedding(d) => {
                put("revision_id", json!(d.revision_id));
                put("model", json!(d.model));
                put("dim", json!(d.dim));
            }
            RecordData::HeadChange(d) => {
                put("project_id", json!(d.project_id));
                put("stage", json!(d.stage));
                put("revision_id", json!(d.after_revision_id));
                put("text", json!(d.reason));
            }
            RecordData::Publication(d) => {
                put("project_id", json!(d.project_id));
                put("revision_id", json!(d.root_revision_id));
                put("title", json!(d.label));
                put("occurred_at_utc", utc_key(Some(&d.published_at)));
                put("text", json!(d.notes));
            }
        }
        m
    }
}

fn utc_key(raw: Option<&str>) -> Value {
    match raw.and_then(util::parse_rfc3339) {
        Some(ts) => json!(util::to_utc_key(ts)),
        None => Value::Null,
    }
}

/// A client-submitted record before the server assigns `seq`/`recorded_at`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewRecord {
    pub id: String,
    pub kind: RecordKind,
    pub data: Value,
}

/// `docs/api.md` §2.3.
pub fn is_valid_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    if bytes.len() < 3 || bytes.len() > 128 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() {
        return false;
    }
    bytes.iter().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b':' | b'-')
    })
}

/// Slugs used for tags, roles and slot ids.
pub fn is_valid_slug(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    if !(bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit()) {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_fields_are_rejected() {
        let err = RecordData::parse(
            RecordKind::Project,
            json!({"title": "x", "descriptio": "typo"}),
        )
        .unwrap_err();
        assert!(err.contains("descriptio"), "{err}");
    }

    #[test]
    fn containment_table_matches_contract() {
        assert!(NodeKind::Project.may_contain(NodeKind::Schema));
        assert!(!NodeKind::Project.may_contain(NodeKind::Core));
        assert!(NodeKind::Schema.may_contain(NodeKind::Idea));
        assert!(NodeKind::Schema.may_contain(NodeKind::Core));
        assert!(!NodeKind::Schema.may_contain(NodeKind::Schema));
        assert!(NodeKind::Core.may_contain(NodeKind::Core));
        assert!(!NodeKind::Core.may_contain(NodeKind::Schema));
        assert!(!NodeKind::Core.may_contain(NodeKind::Project));
        assert!(!NodeKind::Idea.may_contain(NodeKind::Idea));
    }

    #[test]
    fn absent_progress_estimate_is_omitted_from_serialized_criterion_result() {
        let result: CriterionResult = serde_json::from_value(json!({
            "criterion_id": "done", "status": "unknown", "note": ""
        }))
        .unwrap();
        let value = serde_json::to_value(result).unwrap();
        assert!(!value.as_object().unwrap().contains_key("progress_estimate"));
    }

    #[test]
    fn progress_evidence_participates_in_the_record_reference_closure() {
        let data = RecordData::parse(
            RecordKind::Assessment,
            json!({
                "root_revision_id":"rev_root","slot_path":[],"target_revision_id":"rev_target",
                "baseline_id":"baseline_1","evidence_cutoff_seq":10,
                "evidence_cutoff_at":"2026-01-01T00:00:00Z","evaluator":"model:local",
                "rubric_version":"goal-progress-milestones-v1","origin":"ai_proposed",
                "status":"unknown","criteria_results":[{
                    "criterion_id":"planned","status":"unknown",
                    "progress_estimate":{"percent":20.0,"rationale":"plan exists","evidence_record_ids":["rev_plan"]}
                }]
            }),
        )
        .unwrap();
        let record = StoredRecord {
            id: "assessment_1".into(),
            seq: 11,
            recorded_at: "2026-01-01T00:00:00.000Z".into(),
            data,
        };
        assert!(record.refs().contains(&"rev_plan".to_string()));
    }

    #[test]
    fn id_and_slug_formats() {
        assert!(is_valid_id("rev_login.policy:1"));
        assert!(!is_valid_id("Rev_login"));
        assert!(!is_valid_id("ab"));
        assert!(!is_valid_id("1rev"));
        assert!(is_valid_slug("be"));
        assert!(is_valid_slug("2fa-check"));
        assert!(!is_valid_slug("BE"));
        assert!(!is_valid_slug(""));
    }

    #[test]
    fn head_change_and_publication_are_server_only() {
        assert!(RecordKind::HeadChange.is_server_only());
        assert!(RecordKind::Publication.is_server_only());
        assert!(!RecordKind::Revision.is_server_only());
    }
}
