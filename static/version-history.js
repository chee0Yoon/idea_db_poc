(function (root) {
  'use strict';
  const list = value => Array.isArray(value) ? value : [];
  // Only explicit predecessor/derivation edges establish a lineage. Titles never do.
  function revisionHistory(records, revisionId, knownSeq = Infinity) {
    const eligible = list(records).filter(r => Number(r.seq) <= knownSeq);
    const byId = new Map(eligible.map(r => [r.id, r]));
    const revisions = eligible.filter(r => r.kind === 'revision');
    const selected = byId.get(revisionId);
    if (selected?.kind !== 'revision') return { revisions: [], edges: [], truncated: false };
    const adjacent = new Map(revisions.map(r => [r.id, []])), allEdges = [];
    for (const current of revisions) {
      let predecessors = [];
      if (current.data.previous_revision_id) {
        const prior = byId.get(current.data.previous_revision_id);
        if (prior?.kind === 'revision' && Number(prior.seq) <= Number(current.seq)) predecessors = [prior];
      } else {
        const sources = new Set(list(byId.get(current.data.entity_id)?.data?.derived_from));
        predecessors = revisions.filter(r => sources.has(r.data.entity_id) && Number(r.seq) <= Number(current.seq));
      }
      for (const prior of predecessors) {
        allEdges.push({ from: prior.id, to: current.id, type: current.data.previous_revision_id ? 'previous_revision' : 'derived_entity' });
        adjacent.get(current.id).push(prior.id); adjacent.get(prior.id).push(current.id);
      }
    }
    const seen = new Set(), pending = [selected.id], limit = 500;
    while (pending.length && seen.size < limit) {
      const id = pending.pop();
      if (seen.has(id)) continue;
      seen.add(id);
      for (const next of adjacent.get(id) || []) if (!seen.has(next)) pending.push(next);
    }
    const edges = allEdges.filter(e => seen.has(e.from) && seen.has(e.to));
    return {
      revisions: [...seen].map(id => byId.get(id)).sort((a,b) => Number(a.seq)-Number(b.seq) || a.id.localeCompare(b.id)),
      edges: edges.filter(e => seen.has(e.from) && seen.has(e.to)), truncated: pending.length > 0
    };
  }
  function revisionActivity(records, revisionId, knownSeq = Infinity, effectiveAt = null) {
    const eligible = list(records).filter(r => Number(r.seq) <= knownSeq);
    const byId = new Map(eligible.map(r => [r.id, r]));
    const revision = byId.get(revisionId);
    if (revision?.kind !== 'revision') return [];
    const sourceId = revision.data.source?.capture_id;
    const cutoff = effectiveAt == null ? Infinity : Date.parse(effectiveAt);
    const before = timestamp => Number.isFinite(Date.parse(timestamp)) && Date.parse(timestamp) <= cutoff;
    const visible = record => {
      if (effectiveAt == null) return true;
      if (!Number.isFinite(cutoff)) return false;
      if (record.kind === 'observation') return before(record.data.occurred_at);
      if (record.kind === 'assessment') return before(record.data.evidence_cutoff_at) && before(byId.get(record.data.baseline_id)?.data?.effective_from);
      return true;
    };
    return eligible.filter(r => {
      const d = r.data || {};
      if (!visible(r)) return false;
      return r.id === sourceId
        || (r.kind === 'observation' && d.target_revision_id === revisionId)
        || (r.kind === 'assessment' && d.target_revision_id === revisionId)
        || (r.kind === 'goal' && d.scope?.target_revision_id === revisionId)
        || (r.kind === 'head_change' && d.after_revision_id === revisionId)
        || (r.kind === 'link' && [d.from_id,d.to_id].includes(revisionId));
    }).sort((a,b) => Number(a.seq)-Number(b.seq) || a.id.localeCompare(b.id));
  }
  const api = {revisionHistory, revisionActivity};
  root.IdeaVersionHistory = api;
  if (typeof module !== 'undefined' && module.exports) module.exports = api;
})(typeof window !== 'undefined' ? window : globalThis);
