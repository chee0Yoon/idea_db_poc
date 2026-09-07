// Actual main UI acceptance: hierarchical estimates and selectable revision history.
const {chromium}=require('playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
 const [base,project,authoredPath,output]=process.argv.slice(2);
 if(!base||!project||!authoredPath||!output||fs.existsSync(output))throw Error('base, project, authored evaluation, unused output directory required');
 fs.mkdirSync(output,{recursive:true});
 const authored=JSON.parse(fs.readFileSync(authoredPath));
 const browser=await chromium.launch({headless:true,executablePath:process.env.PLAYWRIGHT_CHROMIUM_PATH||undefined});
 const page=await browser.newPage({viewport:{width:1720,height:1100}}),checks=[],errors=[],writes=[];
 page.on('pageerror',e=>errors.push(e.message));page.on('request',r=>{if(r.method()!=='GET'&&!(r.method()==='POST'&&new URL(r.url()).pathname==='/api/search'))writes.push(r.url());});
 try{
  const exported=await(await page.request.get(`${base}/api/export?project_id=${project}&include_receipts=false`)).json();
  const records=new Map(exported.content.records.map(r=>[r.id,r]));
  const root=authored.entries.find(e=>e.level==='project'),schemas=authored.entries.filter(e=>e.level==='schema');
  const select=async entry=>{await page.locator(`#tree .tree-item[data-record-id="${entry.scope.target_revision_id}"]`).click();await page.waitForFunction(id=>document.querySelector('#record-meta').textContent.includes(id),entry.scope.target_revision_id);};
  await page.goto(`${base}/?project=${project}`,{waitUntil:'networkidle'});
  await page.waitForFunction(id=>document.querySelector('#record-meta').textContent.includes(id),root.scope.target_revision_id);
  assert.match(await page.locator('#project-goal-progress .goal-progress-percent').innerText(),/20%/);
  assert.ok((await page.locator('#project-goal-progress').innerText()).includes(root.statement));
  assert.equal(await page.locator('.project-schema-row .schema-progress-percent').count(),6);
  assert.ok(!(await page.locator('body').innerText()).includes(project.replace(/_project$/,'')));
  checks.push('first screen shows Project expected goal and 20% estimate plus six own Schema estimates without application hashes');
  await page.screenshot({path:path.join(output,'hierarchy-overview.png')});
  let checked=0;
  for(const entry of authored.entries){
   await select(entry);
   const scope=page.locator(`#goal-state .goal-progress[data-goal-id="${entry.goal_id}"]`);
   await scope.waitFor();
   const expected=entry.results.reduce((sum,r)=>sum+r.progress_estimate.percent,0)/entry.results.length;
   assert.match(await scope.locator('.goal-progress-percent').innerText(),new RegExp(`${Math.round(expected)}%`));
   assert.equal(await scope.locator('.criterion-progress').count(),entry.results.length);
   assert.equal(await page.locator('#goal-state .goal-progress').count(),1,'descendant estimate masqueraded as own goal');
   for(const result of entry.results){
    const criterion=scope.locator(`.criterion-progress[data-criterion-id="${result.criterion_id}"]`);
    assert.ok((await criterion.locator('.progress-rationale').innerText()).includes(result.progress_estimate.rationale));
    assert.match(await criterion.locator('.criterion-progress-percent').innerText(),new RegExp(`${result.progress_estimate.percent}%`));
    assert.equal(await criterion.locator('.progress-evidence').count(),result.progress_estimate.evidence_record_ids.length);checked++;
   }
   if(entry.level==='core'&&entry.role==='be'){
    assert.match(await scope.innerText(),/不一致|불일치/);
    await page.screenshot({path:path.join(output,'core-goal-gap.png')});
   }
  }
  assert.equal(checked,39);checks.push('all 13 Project/Schema/Core own goals and 39 cited criterion estimates match authored review; BE mismatch stays 0 and own mean 13%');
  const core=authored.entries.find(e=>e.level==='core'&&e.role==='be');await select(core);
  const evidence=page.locator(`#goal-state .goal-progress[data-goal-id="${core.goal_id}"] .progress-evidence`).first();await evidence.click();
  await page.waitForFunction(id=>document.querySelector('#record-meta').textContent.includes(id),core.results[0].progress_estimate.evidence_record_ids[0]);
  assert.match(await page.locator('#record-content').innerText(),/사내 지식/);checks.push('estimate evidence link opens the actual current planning source');
  await select(root);
  const history=page.locator('#version-compare-to');
  const rootVersions=exported.content.records.filter(r=>r.kind==='revision'&&r.data.entity_id===project);
  const v1=rootVersions.find(r=>r.data.body.startsWith('v1 ')),v2=rootVersions.find(r=>r.data.body.startsWith('v2 '));assert.ok(v1&&v2);
  await history.selectOption(root.scope.target_revision_id);await page.locator('#version-compare-from').selectOption(v1.id);
  assert.ok((await page.locator('.diff-copy').innerText()).includes(v1.data.body));assert.ok((await page.locator('.diff-copy').innerText()).includes(records.get(root.scope.target_revision_id).data.body));
  await history.selectOption(v2.id);await page.locator('#version-compare-from').selectOption(v1.id);
  assert.ok((await page.locator('.diff-copy').innerText()).includes(v2.data.body));
  await page.locator('.version-local-history summary').click();
  assert.ok(await page.locator('.version-local-history .version-activity').count()>0);
  assert.match(await page.locator('.version-local-history').innerText(),/변경 이유/);
  checks.push('v1→v3 and v1→v2 selectable comparison retains actual lineage and per-version direction/evidence history');
  await page.screenshot({path:path.join(output,'version-range-history.png')});
  const idea=exported.content.records.find(r=>r.kind==='revision'&&r.id.endsWith('_rev_v3_fe_1'));
  await select({scope:{target_revision_id:idea.id}});
  assert.ok(await page.locator('#version-compare-to option').count()>=3,'Idea lineage lost earlier versions');
  await page.locator('#version-compare-from').selectOption(await page.locator('#version-compare-from option').nth(1).getAttribute('value'));
  assert.ok(await page.locator('.diff-copy').isVisible());checks.push('atomic Idea also supports older lineage selection and comparison');
  await page.locator('#timeline .timeline-button').filter({hasText:'escaped_card_count'}).first().click();
  await page.waitForFunction(()=>document.querySelector('#tree-summary').textContent.includes('선택한 이력'));
  const observation=exported.content.records.find(r=>r.kind==='observation'&&r.data.metric==='escaped_card_count');
  await select({scope:{target_revision_id:observation.data.target_revision_id}});
  await page.waitForFunction(()=>document.querySelector('#goal-state').textContent.includes('실제 측정 3'));
  assert.match(await page.locator('#goal-state .official-completion-rate').innerText(),/100%/);
  assert.ok(!(await page.locator('#goal-state').innerText()).includes('AI 예상 달성률 약 20%'));
  checks.push('historical actual 3 versus target 2 shows 100% criterion satisfaction without future AI estimates');
  await page.locator('#latest-button').click();await page.waitForFunction(id=>document.querySelector('#record-meta').textContent.includes(id),root.scope.target_revision_id);
  await page.waitForFunction(()=>document.querySelector('#project-goal-progress').textContent.includes('AI 예상 달성률 약 20%'));
  await page.setViewportSize({width:1280,height:900});await page.evaluate(()=>{window.scrollTo(0,0);document.querySelector('.record-pane').scrollTop=0;document.querySelector('.insights').scrollTop=0;});
  assert.ok(await page.evaluate(()=>document.documentElement.scrollWidth<=innerWidth+1));
  await page.screenshot({path:path.join(output,'hierarchy-laptop.png')});checks.push('latest restores current hierarchy estimates; laptop has no horizontal overflow');
  assert.deepEqual(errors,[]);assert.deepEqual(writes,[]);checks.push('zero JavaScript errors and dashboard writes');
  fs.writeFileSync(path.join(output,'report.json'),JSON.stringify({status:'passed',checks,errors,writes},null,2));
 }catch(error){await page.screenshot({path:path.join(output,'failure.png')}).catch(()=>{});fs.writeFileSync(path.join(output,'report.json'),JSON.stringify({status:'failed',error:String(error),checks,errors,writes},null,2));throw error;}finally{await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
