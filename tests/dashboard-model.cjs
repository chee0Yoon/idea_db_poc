const assert = require("node:assert/strict");
const model = require("../static/dashboard-model.js");
const r = (id, kind, data, seq = 1) => ({ id, kind, data, seq });
const records = [
  r("p", "project", { title: "P" }), r("es", "entity", { entity_kind: "schema", title: "S", derived_from: ["oldes"] }),
  r("oldes", "entity", { entity_kind: "schema", title: "old" }),
  r("es2", "entity", { entity_kind: "schema", title: "S2", derived_from: ["oldes2"] }),
  r("oldes2", "entity", { entity_kind: "schema", title: "old2" }),
  r("ri0", "revision", { entity_id: "ei", body: "30초", slots: [{ slot_id: "worker", revision_id: "old", roles: ["be"] }], change_kind: "initial" }, 1),
  r("ri1", "revision", { entity_id: "ei", body: "2분", slots: [{ slot_id: "worker", revision_id: "new", roles: ["fe"] }, { slot_id: "audit", revision_id: "audit", roles: ["infra"] }], change_kind: "composition", previous_revision_id: "ri0" }, 2),
  r("rs", "revision", { entity_id: "es", body: "S", slots: [{ slot_id: "leaf", revision_id: "ri1", roles: ["be"] }] }, 3),
  r("rs2", "revision", { entity_id: "es2", body: "S2", slots: [] }, 3),
  r("root", "revision", { entity_id: "p", body: "root", slots: [{ slot_id: "biz", revision_id: "rs", roles: [] }, { slot_id: "other", revision_id: "rs2", roles: [] }] }, 4),
  r("oldrs", "revision", { entity_id: "oldes", body: "old", slots: [] }, 1),
  r("oldrs2", "revision", { entity_id: "oldes2", body: "old2", slots: [] }, 1),
  r("oldroot", "revision", { entity_id: "p", body: "oldroot", slots: [{ slot_id: "biz", revision_id: "oldrs", roles: [] }, { slot_id: "other", revision_id: "oldrs2", roles: [] }] }, 1),
  r("g_old", "goal", { scope: { root_revision_id: "oldroot", slot_path: ["biz"], target_revision_id: "oldrs" } }, 2),
  r("g_other", "goal", { scope: { root_revision_id: "oldroot", slot_path: ["other"], target_revision_id: "oldrs2" } }, 2),
  r("g_future", "goal", { scope: { root_revision_id: "oldroot", slot_path: ["biz"], target_revision_id: "oldrs" } }, 99),
  r("hc_future", "head_change", { after_revision_id: "ri1", reason: "future reason" }, 50)
];
const diff = model.revisionDiff(records, "ri1");
assert.equal(diff.status, "ready"); assert.equal(diff.before.body, "30초"); assert.equal(diff.after.body, "2분");
assert.equal(diff.slots.changed[0].before.roles[0], "be"); assert.equal(diff.slots.added[0].slot_id, "audit");
assert.equal(diff.reason, null);
assert.equal(model.revisionDiff(records, "ri1", "missing").status, "unset");
const snapshot = { root_revision_id: "root", selected_by: { known_seq: 10 } };
const payload = { scope: { root_revision_id: "root", known_seq: 10 }, goals: [{ goal_id: "g", scope: { slot_path: ["biz", "leaf"], in_current_snapshot: true }, criteria: [{ criterion_id: "latency", kind: "quantitative", statement: "latency", comparator: "lte", threshold: 30, unit: "seconds", required: true }, { criterion_id: "retired", kind: "quantitative", threshold: 1, required: true }], required_criteria_status: [{ criterion_id: "latency", status: "unknown" }, { criterion_id: "retired", status: "met" }], baselines: [{ baseline_id: "older", effective_from: "2026-01-01T09:00:00+09:00", seq: 1, criterion_ids: ["retired"], superseded_by: null }, { baseline_id: "newer", effective_from: "2026-01-01T00:00:00Z", seq: 2, criterion_ids: ["latency"], superseded_by: null, official_assessment: null }] }, { goal_id: "superseded", scope: { slot_path: ["biz"], in_current_snapshot: true }, criteria: [{ criterion_id: "old", kind: "quantitative", threshold: 1, required: true }], required_criteria_status: [{ criterion_id: "old", status: "met" }], baselines: [{ baseline_id: "dead", criterion_ids: ["old"], superseded_by: "replacement" }] }], proposed_goals: [{ goal_id: "ai", scope: { slot_path: ["biz"], in_current_snapshot: true }, criteria: [{ criterion_id: "review", kind: "qualitative", required: true }], required_criteria_status: [] }] };
const schemas = model.schemaGoals(records, snapshot, payload);
assert.equal(schemas[0].official.descendant, 1); assert.equal(schemas[0].official.required.unknown, 1);
assert.equal(schemas[0].official.numeric[0].observed_value, null); assert.equal(schemas[0].ai_proposed.own, 1);
assert.equal(schemas[0].official.numeric[0].criterion_id, "latency");
assert.equal(schemas[0].official.required.total, 1);
assert.equal(schemas[0].official.own, 0);
assert.equal(schemas[0].ai_proposed.required.unknown, 1);
assert.equal(schemas[0].historical_references[0].goal_id, "g_old");
assert.deepEqual(schemas[0].historical_references.map((goal) => goal.goal_id), ["g_old"]);
assert.deepEqual(schemas[1].historical_references.map((goal) => goal.goal_id), ["g_other"]);
// A progress percentage exists only when every adopted required criterion has a valid AI estimate.
const progressPayload = { scope: { root_revision_id: "root" }, goals: [{ goal_id: "own", statement: "own goal", scope: { slot_path: ["biz"], in_current_snapshot: true }, criteria: [{ criterion_id: "a", required: true, statement: "A" }, { criterion_id: "b", required: true, statement: "B" }], baselines: [{ criterion_ids: ["a", "b"], superseded_by: null, proposed_assessments: [{ assessment_id: "ai-own", origin: "ai_proposed", criteria_results: [{ criterion_id: "a", progress_estimate: { percent: 0, rationale: "not started", evidence_record_ids: ["g_old"] } }, { criterion_id: "b", progress_estimate: { percent: 40, rationale: "designed", evidence_record_ids: [] } }] }] }] }, { goal_id: "child", statement: "child goal", scope: { slot_path: ["biz", "leaf"], in_current_snapshot: true }, criteria: [{ criterion_id: "c", required: true }], baselines: [{ criterion_ids: ["c"], superseded_by: null, proposed_assessments: [{ origin: "ai_proposed", criteria_results: [{ criterion_id: "c", progress_estimate: { percent: 20, rationale: "plan", evidence_record_ids: [] } }] }] }] }, { goal_id: "unknown", scope: { slot_path: ["other"], in_current_snapshot: true }, criteria: [{ criterion_id: "d", required: true }], baselines: [{ criterion_ids: ["d"], superseded_by: null, proposed_assessments: [{ origin: "ai_proposed", criteria_results: [{ criterion_id: "d", progress_estimate: { rationale: "unknown", evidence_record_ids: [] } }] }] }] }] };
const ownOccurrence = model.occurrenceGoals(records, snapshot, progressPayload, ["biz"]);
assert.equal(ownOccurrence.official.own, 1); assert.equal(ownOccurrence.official.descendant, 1);
assert.equal(ownOccurrence.official.progress_goals[0].percent, 20); // zero is an estimate, not unknown
assert.equal(ownOccurrence.official.progress_goals[0].criteria[0].percent, 0);
assert.equal(model.occurrenceGoals(records, snapshot, progressPayload, ["other"]).official.progress_goals[0].percent, null);
assert.equal(model.occurrenceGoals(records, snapshot, progressPayload, []).official.progress_goals.length, 3);
// A new active baseline does not inherit an older AI estimate, and null is not 0%.
const rebased = JSON.parse(JSON.stringify(progressPayload));
rebased.goals[0].baselines = [
  { criterion_ids: ["a", "b"], superseded_by: "new", proposed_assessments: rebased.goals[0].baselines[0].proposed_assessments },
  { criterion_ids: ["a", "b"], superseded_by: null, proposed_assessments: [{ origin: "ai_proposed", criteria_results: [{ criterion_id: "a", progress_estimate: { percent: null } }, { criterion_id: "b", progress_estimate: { percent: 20 } }] }] }
];
assert.equal(model.occurrenceGoals(records, snapshot, rebased, ["biz"]).official.progress_goals[0].percent, null);
console.log("dashboard-model tests passed");
