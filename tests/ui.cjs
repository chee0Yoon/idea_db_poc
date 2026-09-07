// Read-only browser acceptance. Requires Playwright; no mutations or model calls.
const { chromium } = require('playwright');
const fs = require('node:fs');
const path = require('node:path');
const assert = require('node:assert/strict');

(async () => {
  const base = process.argv[2] || 'http://127.0.0.1:18080';
  const output = process.argv[3];
  if (!output) throw new Error('Provide an unused screenshot/report output directory');
  if (fs.existsSync(output)) throw new Error('Refusing to overwrite browser evidence');
  fs.mkdirSync(output, { recursive: true });
  const browser = await chromium.launch({ headless: true, executablePath: process.env.PLAYWRIGHT_CHROMIUM_PATH || undefined });
  const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
  const errors = [], writes = [];
  page.on('pageerror', e => errors.push(e.message));
  page.on('request', r => {
    if (r.method() !== 'GET' && !(r.method() === 'POST' && new URL(r.url()).pathname === '/api/search')) writes.push(r.url());
  });
  try {
    await page.goto(base, { waitUntil: 'networkidle' });
    await page.getByRole('button', { name: '로그인 차단 규칙', exact: true }).click();
    await page.locator('#record-content').filter({ hasText: '로그인 실패는 5회까지 허용하지 않는다.' }).waitFor();
    assert.match(await page.locator('#occurrence-details').innerText(), /be/);
    assert.match(await page.locator('#occurrence-details').innerText(), /code point|코드포인트|codepoint|Unicode/i);
    await page.screenshot({ path: path.join(output, 'idea-detail.png'), fullPage: true });
    await page.locator('#source-list button').first().click();
    await page.locator('#record-kind').filter({ hasText: 'CAPTURE' }).waitFor();
    assert.match(await page.locator('#record-content').innerText(), /로그인/);
    await page.locator('#search-query').fill('로그인');
    await page.getByRole('button', { name: '검색', exact: true }).click();
    await page.waitForFunction(() => document.querySelector('#search-state').textContent.includes('어휘 검색'));
    assert.ok(await page.locator('#search-results button').count() > 0);
    await page.screenshot({ path: path.join(output, 'capture-search.png'), fullPage: true });
    await page.locator('#timeline button').first().click();
    await page.waitForFunction(() => /known seq 2/.test(document.querySelector('#tree-summary').textContent));
    assert.equal(await page.locator('[name="search-kind"]:checked').count(), 6);
    assert.equal(await page.locator('#max-nodes').inputValue(), '5000');
    assert.match(await page.locator('.brand').innerText(), /읽기 전용/);
    assert.deepEqual(writes, []);
    assert.deepEqual(errors, []);
    fs.writeFileSync(path.join(output, 'report.json'), JSON.stringify({ status: 'passed', checks: ['recursive detail and exact role', 'Unicode anchor facts', 'capture body display', 'all-kind search', 'historical root and sequence navigation', 'read-only requests', 'no browser exceptions'], writes, errors }, null, 2));
  } finally { await browser.close(); }
})().catch(e => { console.error(e); process.exitCode = 1; });
