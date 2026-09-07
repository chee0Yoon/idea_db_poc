// Read-only browser verification of the new summary surface and view toggles.
const {chromium}=require('playwright');
const fs=require('node:fs'),path=require('node:path'),assert=require('node:assert/strict');
(async()=>{
  const [base,fixturePath,out]=process.argv.slice(2);
  if(!base||!fixturePath||!out||fs.existsSync(out))throw Error('base, summary-ingest report, unused output directory required');
  fs.mkdirSync(out,{recursive:true});
  const fixture=JSON.parse(fs.readFileSync(fixturePath)),ids=fixture.fixture_ids;
  const browser=await chromium.launch({headless:true,executablePath:process.env.PLAYWRIGHT_CHROMIUM_PATH||undefined});
  const page=await browser.newPage({viewport:{width:1600,height:1100}}),errors=[],writes=[];
  page.on('pageerror',e=>errors.push(e.message));
  page.on('request',r=>{if(r.method()!=='GET'&&!(r.method()==='POST'&&new URL(r.url()).pathname==='/api/search'))writes.push(r.url());});
  try {
    await page.goto(`${base}/?project=${fixture.project_id}`,{waitUntil:'networkidle'});
    const select=async id=>{
      await page.locator(`#tree .tree-item[data-record-id="${id}"]`).first().click();
      await page.waitForFunction(id=>document.querySelector('#record-meta').textContent.includes(id),id);
    };
    await select(ids.summary_revision);
    assert.equal(await page.locator('#record-summary-section').isVisible(),true);
    assert.match(await page.locator('#record-summary-section').innerText(),/AI 검색 요약 · 추론/);
    assert.match(await page.locator('#record-summary').innerText(),/야간 배포/);
    assert.match(await page.locator('#record-content').innerText(),/그 경우에는/);
    await page.screenshot({path:path.join(out,'summary-and-body-2d.png'),fullPage:true});
    await select(ids.plain_revision);
    assert.equal(await page.locator('#record-summary-section').isVisible(),false);
    await select(ids.summary_revision);
    await page.locator('#view-3d').click();
    await page.waitForFunction(()=>document.querySelector('#view-3d').getAttribute('aria-pressed')==='true');
    await page.locator('canvas').first().waitFor({state:'visible'});
    await page.screenshot({path:path.join(out,'summary-graph-3d.png'),fullPage:true});
    await page.locator('#view-2d').click();
    await page.waitForFunction(()=>document.querySelector('#view-2d').getAttribute('aria-pressed')==='true');
    assert.deepEqual(errors,[]);assert.deepEqual(writes,[]);
    fs.writeFileSync(path.join(out,'report.json'),JSON.stringify({status:'passed',checks:['AI summary and body separated','legacy record hides summary without stale text','2D and 3D toggles and canvas visible','no browser exceptions or domain writes'],errors,writes},null,2),{flag:'wx'});
  } catch(error) {
    await page.screenshot({path:path.join(out,'failure.png'),fullPage:true});
    fs.writeFileSync(path.join(out,'failure.json'),JSON.stringify({error:String(error),errors,writes}),{flag:'wx'});throw error;
  } finally {await browser.close();}
})().catch(e=>{console.error(e);process.exitCode=1;});
