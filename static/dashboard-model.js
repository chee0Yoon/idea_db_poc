(function (root) {
  "use strict";

  const list = (value) => Array.isArray(value) ? value : [];
  const same = (a, b) => JSON.stringify(a) === JSON.stringify(b);

  function revisionDiff(records, revisionId, compareId) {
    const all = list(records), byId = new Map(all.map((r) => [r.id, r]));
    const after = byId.get(revisionId);
    if (!after || after.kind !== "revision") return { status: "unset", revision_id: revisionId, compare_revision_id: null, candidates: [], before: null, after: null, slots: { added: [], removed: [], changed: [] }, metadata_changes: [], reason: null };
    let candidates = [];
    if (compareId) candidates = [compareId];
    else if (after.data.previous_revision_id) candidates = [after.data.previous_revision_id];
    else {
      const entity = byId.get(after.data.entity_id);
      for (const priorEntity of list(entity?.data?.derived_from)) {
        candidates.push(...all.filter((r) => r.kind === "revision" && r.data?.entity_id === priorEntity && Number(r.seq) < Number(after.seq)).map((r) => r.id));
      }
      candidates = [...new Set(candidates)];
    }
    const before = candidates.length === 1 ? byId.get(candidates[0]) : null;
    const base = before && before.kind === "revision" ? before : null;
    const oldSlots = new Map(list(base?.data?.slots).map((s) => [s.slot_id, s]));
    const newSlots = new Map(list(after.data.slots).map((s) => [s.slot_id, s]));
    const added = [], removed = [], changed = [];
    for (const [id, slot] of newSlots) {
      if (!oldSlots.has(id)) added.push(slot);
      else if (!same(oldSlots.get(id), slot)) changed.push({ slot_id: id, before: oldSlots.get(id), after: slot });
    }
    for (const [id, slot] of oldSlots) if (!newSlots.has(id)) removed.push(slot);
    const metadataChanges = [];
    if (base) for (const key of ["tags", "change_kind", "correction_of", "source"]) {
      if (!same(base.data[key] ?? null, after.data[key] ?? null)) metadataChanges.push({ field: key, before: base.data[key] ?? null, after: after.data[key] ?? null });
    }
    return {
      status: compareId && !base ? "unset" : (!compareId && candidates.length > 1 ? "ambiguous" : (base ? "ready" : "unset")),
      revision_id: revisionId, compare_revision_id: base?.id || null, candidates,
      before: base ? { id: base.id, body: base.data.body } : null,
      after: { id: after.id, body: after.data.body }, slots: { added, removed, changed },
      metadata_changes: metadataChanges,
      reason: after.data.correction_reason || all
        .filter((record) => record.kind === "head_change" && record.data?.after_revision_id === after.id && Number(record.seq) <= Number(after.seq))
        .sort((a, b) => Number(b.seq) - Number(a.seq))[0]?.data?.reason || null
    };
  }

  function pathStarts(path, prefix) { return prefix.length <= path.length && prefix.every((part, i) => path[i] === part); }
  function statusCounts() { return { met: 0, unmet: 0, unknown: 0, disputed: 0, recheck: 0, total: 0 }; }
  function emptyLane() { return { own: 0, descendant: 0, required: statusCounts(), own_required: statusCounts(), numeric: [], own_numeric: [], progress_goals: [] }; }
  function latestAssessment(items) { return list(items).at(-1) || null; }
  function assessmentProvenance(assessment) {
    if (!assessment) return null;
    return { assessment_id: assessment.assessment_id || assessment.id || null, evaluator: assessment.evaluator || null,
      rubric_version: assessment.rubric_version || null, recorded_at: assessment.recorded_at || null,
      evidence_cutoff_seq: assessment.evidence_cutoff_seq ?? null, evidence_cutoff_at: assessment.evidence_cutoff_at || null,
      note: assessment.note || null };
  }
  function progressGoal(goal, requiredCriteria, estimateAssessment, own) {
    const results = new Map(list(estimateAssessment?.criteria_results).map((result) => [result.criterion_id, result]));
    const rows = requiredCriteria.map((criterion) => {
      const result = results.get(criterion.criterion_id);
      const estimate = result?.progress_estimate;
      const percent = typeof estimate?.percent === 'number' && Number.isFinite(estimate.percent) ? estimate.percent : null;
      return { criterion_id: criterion.criterion_id, statement: criterion.statement || '', percent,
        rationale: estimate?.rationale || '', evidence_record_ids: list(estimate?.evidence_record_ids), valid: percent !== null && percent >= 0 && percent <= 100 };
    });
    const complete = rows.length > 0 && rows.every((row) => row.valid);
    return { goal_id: goal.goal_id || goal.id || null, statement: goal.statement || '', own: !!own,
      required_count: rows.length, estimated_count: rows.filter((row) => row.valid).length,
      percent: complete ? rows.reduce((sum, row) => sum + row.percent, 0) / rows.length : null,
      criteria: rows, assessment: assessmentProvenance(estimateAssessment) };
  }
  function addGoal(lane, goal, own, proposed) {
    const baselines = list(goal.baselines);
    const activeBaselines = baselines.filter((item) => item.superseded_by == null);
    if (baselines.length && !activeBaselines.length) return false;
    lane[own ? "own" : "descendant"]++;
    const baseline = activeBaselines.sort((a, b) => {
      const time = (Date.parse(a.effective_from) || 0) - (Date.parse(b.effective_from) || 0);
      return time || Number(a.seq || 0) - Number(b.seq || 0) || String(a.baseline_id || "").localeCompare(String(b.baseline_id || ""));
    }).at(-1) || null;
    const adopted = baseline && Array.isArray(baseline.criterion_ids) ? new Set(baseline.criterion_ids) : null;
    const eligibleCriteria = list(goal.criteria).filter((criterion) => !adopted || adopted.has(criterion.criterion_id));
    const declaredRequired = eligibleCriteria.filter((criterion) => criterion.required);
    const reportedRequired = list(goal.required_criteria_status).filter((criterion) => !adopted || adopted.has(criterion.criterion_id));
    const required = reportedRequired.length
      ? reportedRequired
      : declaredRequired.map((criterion) => ({ criterion_id: criterion.criterion_id, status: "unknown" }));
    for (const criterion of required) {
      const status = ["met", "unmet", "disputed", "recheck"].includes(criterion.status) ? criterion.status : "unknown";
      lane.required[status]++; lane.required.total++;
      if (own) { lane.own_required[status]++; lane.own_required.total++; }
    }
    const criteria = eligibleCriteria;
    const assessment = proposed
      ? (goal.proposed_assessment || list(baseline?.proposed_assessments).at(-1) || null)
      : (goal.official_assessment || baseline?.official_assessment || null);
    const assessed = new Map(list(assessment?.criteria_results).map((r) => [r.criterion_id, r]));
    for (const criterion of criteria.filter((c) => c.kind === "quantitative" || c.threshold != null)) {
      const result = assessed.get(criterion.criterion_id);
      const numeric = { goal_id: goal.goal_id || goal.id, criterion_id: criterion.criterion_id, statement: criterion.statement || "", comparator: criterion.comparator ?? null, threshold: criterion.threshold ?? null, unit: criterion.unit ?? null, observed_value: result?.observed_value ?? null, status: result?.status || "unknown" };
      lane.numeric.push(numeric); if (own) lane.own_numeric.push(numeric);
    }
    // Completion estimates are always AI proposals, including proposals attached to official goals.
    const estimateAssessment = goal.proposed_assessment || latestAssessment(baseline?.proposed_assessments);
    if (estimateAssessment || own) lane.progress_goals.push(progressGoal(goal, declaredRequired, estimateAssessment, own));
    return true;
  }

  function occurrenceGoals(records, snapshot, goalsPayload, slotPath = []) {
    const root = snapshot?.root_revision_id, payloadRoot = goalsPayload?.scope?.root_revision_id;
    const result = { slot_path: list(slotPath), official: emptyLane(), ai_proposed: emptyLane() };
    for (const goal of list(goalsPayload?.goals)) {
      const scope = goal.scope || {}, path = list(scope.slot_path);
      if ((scope.root_revision_id || payloadRoot) === root && scope.in_current_snapshot === true && pathStarts(path, result.slot_path)) addGoal(result.official, goal, path.length === result.slot_path.length, false);
    }
    for (const goal of list(goalsPayload?.proposed_goals)) {
      const scope = goal.scope || {}, path = list(scope.slot_path);
      if ((scope.root_revision_id || payloadRoot) === root && scope.in_current_snapshot !== false && pathStarts(path, result.slot_path)) addGoal(result.ai_proposed, goal, path.length === result.slot_path.length, true);
    }
    return result;
  }

  function schemaGoals(records, snapshot, goalsPayload) {
    const all = list(records), byId = new Map(all.map((r) => [r.id, r]));
    const root = snapshot?.root_revision_id;
    const cutoff = Number(goalsPayload?.scope?.known_seq ?? snapshot?.selected_by?.known_seq);
    const rootNode = byId.get(root);
    return list(rootNode?.data?.slots).map((slot) => {
      const rev = byId.get(slot.revision_id), entity = byId.get(rev?.data?.entity_id);
      const prefix = [String(slot.slot_id)];
      const lanes = occurrenceGoals(all, snapshot, goalsPayload, prefix);
      const result = { schema_revision_id: slot.revision_id, schema_entity_id: rev?.data?.entity_id || null, title: entity?.data?.title || slot.slot_id, slot_path: prefix, official: lanes.official, ai_proposed: lanes.ai_proposed, historical_references: [] };
      const lineage = new Set([result.schema_entity_id, ...list(entity?.data?.derived_from)]);
      for (const record of all.filter((r) => r.kind === "goal")) {
        const scope = record.data?.scope || {};
        if (scope.root_revision_id === root) continue;
        if (Number.isFinite(cutoff) && Number(record.seq) > cutoff) continue;
        const historicalRoot = byId.get(scope.root_revision_id);
        const historicalPath = list(scope.slot_path);
        if (!historicalPath.length) continue;
        const firstSlot = list(historicalRoot?.data?.slots).find((s) => s.slot_id === historicalPath[0]);
        const historicalEntity = byId.get(firstSlot?.revision_id)?.data?.entity_id;
        if (historicalEntity && lineage.has(historicalEntity)) result.historical_references.push({ goal_id: record.id, root_revision_id: scope.root_revision_id, slot_path: historicalPath, schema_entity_id: historicalEntity });
      }
      return result;
    });
  }

  const api = { revisionDiff, occurrenceGoals, schemaGoals };
  root.IdeaDashboardModel = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis !== "undefined" ? globalThis : window);
