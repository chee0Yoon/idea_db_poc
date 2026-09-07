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
    const seen = new Set(), edges = [], pending = [selected], limit = 500;
    while (pending.length && seen.size < limit) {
      const current = pending.pop();
      if (seen.has(current.id)) continue;
      seen.add(current.id);
      let predecessors = [];
      if (current.data.previous_revision_id) {
        const prior = byId.get(current.data.previous_revision_id);
        if (prior?.kind === 'revision' && Number(prior.seq) < Number(current.seq)) predecessors = [prior];
      } else {
        const sources = new Set(list(byId.get(current.data.entity_id)?.data?.derived_from));
        predecessors = revisions.filter(r => sources.has(r.data.entity_id) && Number(r.seq) < Number(current.seq));
      }
      for (const prior of predecessors) {
        edges.push({ from: prior.id, to: current.id, type: current.data.previous_revision_id ? 'previous_revision' : 'derived_entity' });
        if (!seen.has(prior.id)) pending.push(prior);
      }
    }
    return {
      revisions: [...seen].map(id => byId.get(id)).sort((a,b) => Number(a.seq)-Number(b.seq) || a.id.localeCompare(b.id)),
      edges: edges.filter(e => seen.has(e.from) && seen.has(e.to)), truncated: pending.length > 0
    };
  }
  function revisionActivity(records, revisionId, knownSeq = Infinity) {
    const eligible = list(records).filter(r => Number(r.seq) <= knownSeq);
    const byId = new Map(eligible.map(r => [r.id, r]));
    const revision = byId.get(revisionId);
    if (revision?.kind !== 'revision') return [];
    const sourceId = revision.data.source?.capture_id;
    return eligible.filter(r => {
      const d = r.data || {};
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
