const assert = require('node:assert/strict');
const {revisionHistory,revisionActivity} = require('../static/version-history.js');
const r=(id,kind,seq,data)=>({id,kind,seq,data});
const records=[r('e1','entity',1,{}),r('e2','entity',4,{derived_from:['e1']}),r('e3','entity',6,{derived_from:['e2']}),
 r('v1','revision',2,{entity_id:'e1',source:{capture_id:'source'}}),r('v1b','revision',3,{entity_id:'e1',previous_revision_id:'v1'}),
 r('v2','revision',5,{entity_id:'e2'}),r('v3','revision',7,{entity_id:'e3'}),r('future','revision',99,{entity_id:'e2'}),
 r('unrelated','revision',2,{entity_id:'else',body:'v1 same name'}),r('source','capture',1,{}),
 r('obs','observation',8,{target_revision_id:'v2'}),r('otherobs','observation',8,{target_revision_id:'unrelated'}),
 r('later','observation',10,{target_revision_id:'v2'})];
const history=revisionHistory(records,'v3',8);
assert.deepEqual(history.revisions.map(r=>r.id),['v1','v1b','v2','v3']);
assert.equal(history.edges.filter(e=>e.to==='v2').length,2,'ambiguous derivation retained, not arbitrary predecessor');
assert.deepEqual(revisionHistory(records,'v2',5).revisions.map(r=>r.id),['v1','v1b','v2']);
assert.equal(revisionHistory(records,'v3',6).revisions.length,0,'future selected revision hidden');
assert.deepEqual(revisionActivity(records,'v2',8).map(r=>r.id),['obs']);
assert.deepEqual(revisionActivity(records,'v1',8).map(r=>r.id),['source']);
assert.equal(revisionHistory([r('cycle','revision',2,{entity_id:'e',previous_revision_id:'cycle'})],'cycle').revisions.length,1);
console.log('version-history tests passed');
