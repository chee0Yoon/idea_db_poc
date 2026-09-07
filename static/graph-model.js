(function (root) {
  "use strict";

  const MAX_NODES = 5000;
  const MAX_DEPTH = 32;
  const MAX_OCCURRENCES = 20000;

  function build(records, projectId) {
    const all = Array.isArray(records) ? records : [];
    const entities = new Map(all.filter((r) => r.kind === "entity").map((r) => [r.id, r]));
    const revisions = new Map(all.filter((r) => r.kind === "revision").map((r) => [r.id, r]));
    const owned = new Set();
    for (const [id, rev] of revisions) {
      const entity = entities.get(rev.data && rev.data.entity_id);
      if (rev.data && rev.data.entity_id === projectId || (entity && entity.data.project_id === projectId)) owned.add(id);
    }

    const roots = [...owned].filter((id) => revisions.get(id).data.entity_id === projectId).sort();
    const usages = new Map();
    const depth = new Map();
    const layerKeys = new Map();
    let cycleCount = 0;
    let depthLimited = 0;
    let occurrenceCount = 0;
    let occurrenceLimited = false;

    function visit(id, rootId, path, roles, d, layerKey, ancestors) {
      if (!revisions.has(id)) return;
      if (occurrenceCount >= MAX_OCCURRENCES) { occurrenceLimited = true; return; }
      occurrenceCount++;
      owned.add(id); // explicitly composed cross-project Core/Idea
      const key = rootId + "\0" + path.join("/");
      const list = usages.get(id) || [];
      if (!list.some((u) => u._key === key)) list.push({ root_revision_id: rootId, slot_path: path.slice(), roles: roles.slice(), _key: key });
      usages.set(id, list);
      depth.set(id, Math.min(depth.has(id) ? depth.get(id) : Infinity, d));
      if (layerKey != null) {
        const keys = layerKeys.get(id) || new Set(); keys.add(layerKey); layerKeys.set(id, keys);
      }
      if (ancestors.has(id)) { cycleCount++; return; }
      if (d >= MAX_DEPTH) { depthLimited++; return; }
      const next = new Set(ancestors); next.add(id);
      const slots = Array.isArray(revisions.get(id).data.slots) ? revisions.get(id).data.slots : [];
      for (const slot of slots) {
        const child = revisions.get(slot.revision_id);
        const childLayer = d === 0 ? (child && child.data.entity_id) : layerKey;
        visit(slot.revision_id, rootId, path.concat(String(slot.slot_id)), Array.isArray(slot.roles) ? slot.roles : [], d + 1, childLayer, next);
        if (occurrenceLimited) break;
      }
    }
    for (const id of roots) visit(id, id, [], [], 0, null, new Set());

    const schemaIds = new Set();
    for (const rootId of roots) {
      for (const slot of revisions.get(rootId).data.slots || []) {
        const child = revisions.get(slot.revision_id);
        if (child) schemaIds.add(child.data.entity_id);
      }
    }
    const schemaOrder = [...schemaIds].sort((a, b) => {
      const ea = entities.get(a)?.data.title || a;
      const eb = entities.get(b)?.data.title || b;
      return ea.localeCompare(eb) || a.localeCompare(b);
    });
    const layerIndex = new Map(schemaOrder.map((id, i) => [id, i]));
    const titleCounts = new Map();
    for (const id of schemaOrder) {
      const title = entities.get(id)?.data.title || id;
      titleCounts.set(title, (titleCounts.get(title) || 0) + 1);
    }
    const layers = schemaOrder.map((id, index) => {
      const title = entities.get(id)?.data.title || id;
      return { id, index, label: titleCounts.get(title) > 1 ? `${title} (${id})` : title };
    });
    const unassignedIndex = layers.length;
    layers.push({ id: "unassigned", index: unassignedIndex, label: "미배치" });

    const selectedIds = [...owned].sort((a, b) => {
      const ra = revisions.get(a), rb = revisions.get(b);
      return (ra.seq || 0) - (rb.seq || 0) || a.localeCompare(b);
    });
    const truncated = selectedIds.length > MAX_NODES;
    const kept = new Set(selectedIds.slice(0, MAX_NODES));
    const nodes = [...kept].map((id) => {
      const rev = revisions.get(id), entity = entities.get(rev.data.entity_id);
      const keys = [...(layerKeys.get(id) || [])].map((k) => layerIndex.get(k)).filter(Number.isInteger);
      const layer = keys.length ? Math.min(...keys) : unassignedIndex;
      return {
        id, revision_id: id,
        title: (entity && entity.data.title) || rev.data.body || id,
        kind: rev.data.entity_id === projectId ? "project" : ((entity && entity.data.entity_kind) || "revision"),
        recorded_at: rev.recorded_at || null, seq: rev.seq == null ? null : rev.seq,
        x: Date.parse(rev.recorded_at) || 0, y: depth.has(id) ? depth.get(id) : -1,
        z: layer, layer,
        usages: (usages.get(id) || []).map(({ _key, ...u }) => u)
      };
    });
    const edgeMap = new Map();
    function edge(source, target, type) {
      if (kept.has(source) && kept.has(target)) edgeMap.set(source + "\0" + target + "\0" + type, { source, target, type });
    }
    for (const id of kept) {
      const rev = revisions.get(id);
      for (const slot of rev.data.slots || []) edge(id, slot.revision_id, "composition");
      if (rev.data.previous_revision_id) edge(id, rev.data.previous_revision_id, "version");
      if (rev.data.correction_of) edge(id, rev.data.correction_of, "version");
    }
    for (const rec of all.filter((r) => r.kind === "link")) {
      if (["similar", "contradict"].includes(rec.data.link_type)) edge(rec.data.from_id, rec.data.to_id, rec.data.link_type);
    }
    const revsByEntity = new Map();
    for (const id of kept) {
      const eid = revisions.get(id).data.entity_id, list = revsByEntity.get(eid) || []; list.push(id); revsByEntity.set(eid, list);
    }
    for (const id of kept) {
      const entity = entities.get(revisions.get(id).data.entity_id);
      for (const priorEntity of (entity && entity.data.derived_from) || []) {
        const prior = revsByEntity.get(priorEntity) || [];
        if ((revsByEntity.get(entity.id) || []).length === 1 && prior.length === 1) edge(id, prior[0], "derived");
      }
    }
    const xs = nodes.map((n) => n.x), ys = nodes.map((n) => n.y), zs = nodes.map((n) => n.z);
    return {
      nodes, edges: [...edgeMap.values()], layers,
      bounds: nodes.length ? { x: [Math.min(...xs), Math.max(...xs)], y: [Math.min(...ys), Math.max(...ys)], z: [Math.min(...zs), Math.max(...zs)] } : { x: [0, 0], y: [0, 0], z: [0, 0] },
      truncation: { truncated: truncated || occurrenceLimited, max_nodes: MAX_NODES, omitted_nodes: Math.max(0, selectedIds.length - MAX_NODES), max_depth: MAX_DEPTH, depth_limited: depthLimited, cycles: cycleCount, max_occurrences: MAX_OCCURRENCES, occurrences: occurrenceCount, occurrence_limited: occurrenceLimited }
    };
  }

  const api = { build };
  root.IdeaGraphModel = api;
  if (typeof module !== "undefined" && module.exports) module.exports = api;
})(typeof globalThis !== "undefined" ? globalThis : window);
