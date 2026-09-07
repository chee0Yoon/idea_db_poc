const assert = require("node:assert/strict");
const model = require("../static/graph-model.js");

function rec(id, kind, data, seq = 1, at = "2026-09-07T00:00:00Z") { return { id, kind, data, seq, recorded_at: at }; }
const records = [
  rec("p", "project", { title: "P" }),
  rec("es", "entity", { entity_kind: "schema", project_id: "p", title: "Business" }),
  rec("ec", "entity", { entity_kind: "core", project_id: "p", title: "Core" }),
  rec("ei", "entity", { entity_kind: "idea", project_id: "other", title: "Shared" }),
  rec("r0", "revision", { entity_id: "p", body: "old", slots: [], previous_revision_id: null }, 2),
  rec("rs", "revision", { entity_id: "es", body: "schema", slots: [{ slot_id: "a", revision_id: "rc", roles: [] }, { slot_id: "shared", revision_id: "ri", roles: ["be"] }] }, 3),
  rec("rc", "revision", { entity_id: "ec", body: "core", slots: [{ slot_id: "shared", revision_id: "ri", roles: ["fe"] }] }, 4),
  rec("ri", "revision", { entity_id: "ei", body: "idea", slots: [] }, 5, "2026-09-07T00:00:01Z"),
  rec("r1", "revision", { entity_id: "p", body: "new", slots: [{ slot_id: "biz", revision_id: "rs", roles: [] }], previous_revision_id: "r0" }, 6),
  rec("rx", "revision", { entity_id: "x", body: "excluded", slots: [] }, 7),
  rec("ln", "link", { link_type: "similar", from_id: "ri", to_id: "rc" }, 8)
];
const graph = model.build(records, "p");
assert.equal(graph.nodes.filter((n) => n.id === "ri").length, 1);
assert.deepEqual(graph.nodes.find((n) => n.id === "ri").usages.map((u) => u.slot_path), [["biz", "a", "shared"], ["biz", "shared"]]);
assert.equal(graph.nodes.find((n) => n.id === "ri").x, Date.parse("2026-09-07T00:00:01Z"));
assert(graph.edges.some((e) => e.source === "r1" && e.target === "r0" && e.type === "version"));
assert(graph.edges.some((e) => e.type === "similar"));
assert(!graph.nodes.some((n) => n.id === "rx"));
assert.equal(graph.nodes.find((n) => n.id === "ri").y, 2);
assert.equal(graph.truncation.truncated, false);

const cyclic = [
  rec("pc", "project", { title: "cycle" }),
  rec("ecyc", "entity", { entity_kind: "schema", project_id: "pc", title: "cycle" }),
  rec("rpc", "revision", { entity_id: "pc", body: "root", slots: [{ slot_id: "s", revision_id: "rsc", roles: [] }] }),
  rec("rsc", "revision", { entity_id: "ecyc", body: "schema", slots: [{ slot_id: "loop", revision_id: "rsc", roles: [] }] })
];
assert(model.build(cyclic, "pc").truncation.cycles > 0);

const many = [rec("big", "project", { title: "big" })];
for (let i = 0; i < 5001; i++) many.push(rec(`root_${i}`, "revision", { entity_id: "big", body: String(i), slots: [] }, i));
const bounded = model.build(many, "big");
assert.equal(bounded.nodes.length, 5000);
assert.equal(bounded.truncation.truncated, true);
assert.equal(bounded.truncation.omitted_nodes, 1);

const diamond = [rec("pd", "project", { title: "diamond" }), rec("ed", "entity", { entity_kind: "schema", project_id: "pd", title: "D" })];
for (let i = 0; i <= 20; i++) {
  diamond.push(rec(`d${i}`, "revision", { entity_id: i === 0 ? "pd" : "ed", body: String(i), slots: i === 20 ? [] : [
    { slot_id: "a", revision_id: `d${i + 1}`, roles: [] }, { slot_id: "b", revision_id: `d${i + 1}`, roles: [] }
  ] }, i));
}
const diamondGraph = model.build(diamond, "pd");
assert.equal(diamondGraph.truncation.occurrence_limited, true);
assert.equal(diamondGraph.truncation.occurrences, 20000);

const versionedSchema = records.concat(rec("rs2", "revision", { entity_id: "es", body: "schema v2", slots: [] }, 9));
const versionedGraph = model.build(versionedSchema, "p");
assert.equal(versionedGraph.layers.filter((layer) => layer.id === "es").length, 1);
console.log("graph-model tests passed");
