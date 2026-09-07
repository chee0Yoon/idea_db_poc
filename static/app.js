(() => {
  'use strict';

  const ENDPOINTS = {
    state: '/api/state', snapshot: '/api/snapshot', search: '/api/search', record: (id) => `/api/records/${encodeURIComponent(id)}`,
    goals: '/api/goals', export: '/api/export'
  };
  const $ = (id) => document.getElementById(id);
  const route = new URLSearchParams(window.location.search);
  const state = {
    projectId: route.get('project') || '', snapshot: null, selected: null, knownSeq: null, rootRevisionId: null,
    view: route.get('view') === '3d' ? '3d' : '2d', exportProjectId: null, exportRecords: null, graph: null, goalPayload: null, contextGeneration: 0, recordGeneration: 0
  };
  let graphRenderer = null;
  const maxTreeNodes = () => Number($('max-nodes')?.value || 5000);
  const asArray = (value) => Array.isArray(value) ? value : [];
  const text = (value, fallback = '—') => value === null || value === undefined || value === '' ? fallback : String(value);
  const iso = (value) => { if (!value) return null; const date = new Date(value); return Number.isNaN(date.valueOf()) ? null : date.toISOString(); };
  const scope = () => ({ project_id: state.projectId || undefined, stage: $('stream-select').value, known_seq: state.knownSeq, known_at: state.knownSeq === null ? iso($('known-at').value) : undefined, effective_at: iso($('effective-at').value), root_revision_id: state.rootRevisionId || undefined });
  const recordScope = () => {
    const current = scope();
    return { stage: current.stage, known_seq: current.known_seq, known_at: current.known_at, effective_at: current.effective_at };
  };
  const query = (params) => { const q = new URLSearchParams(); Object.entries(params).forEach(([key, value]) => { if (value !== undefined && value !== null && value !== '') q.set(key, String(value)); }); return q.size ? `?${q}` : ''; };
  const token = () => sessionStorage.getItem('idea_db.token') || '';
  const makeId = (prefix) => `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
  const clear = (node) => node.replaceChildren();
  const dialog = (id) => $(id).showModal();
  const errorMessage = (error) => error instanceof Error ? error.message : String(error);
  const formatValue = (value) => value === null || value === undefined ? '—' : typeof value === 'object' ? JSON.stringify(value) : String(value);
  const kindLabel = (kind) => ({ project: '프로젝트', entity: '항목', schema: '스키마', core: '핵심 항목', idea: '아이디어', revision: '버전', capture: '원문', observation: '관측', goal: '목표', assessment: '평가', artifact: '산출물', candidate: '후보', promotion: '승격', publication: '공식 반영', head_change: '구성 변경', link: '연결' }[kind] || '기록');

  function formatTime(value) {
    const date = value ? new Date(value) : null;
    return !date || Number.isNaN(date.valueOf()) ? '시각 미기록' : date.toLocaleString('ko-KR', { dateStyle: 'medium', timeStyle: 'medium' });
  }

  function recordMap() {
    return new Map(asArray(state.exportRecords).map((record) => [record.id, record]));
  }

  function titleForRecord(record, fallback = '') {
    if (!record) return fallback || '연결된 기록';
    const data = record.data || {};
    if (record.kind === 'revision') {
      const entity = recordMap().get(data.entity_id);
      return text(entity?.data?.title || data.title || data.body, fallback || '버전');
    }
    if (record.kind === 'capture') return captureTimelineTitle(record) || '구조화된 원문';
    return text(data.title || data.statement || data.metric || data.label || data.uri || data.source_kind, fallback || kindLabel(record.kind));
  }

  function titleForId(id, fallback = '') {
    if (!id) return fallback || '연결된 기록';
    const snapshotNode = snapshotNodeMap().get(id);
    return text(snapshotNode?.title || titleForRecord(recordMap().get(id), ''), fallback || '연결된 기록');
  }

  function recordTime(record) {
    return formatTime(record?.recorded_at || record?.data?.occurred_at);
  }

  function timestampSuffix(record) {
    return record?.recorded_at || record?.data?.occurred_at ? ` · ${recordTime(record)}` : '';
  }

  function humanEntryTitle(value, id, fallback = '연결된 기록') {
    const candidate = value === null || value === undefined ? '' : String(value).trim();
    return candidate && candidate !== String(id || '') ? candidate : titleForId(id, fallback);
  }

  function updateRoute() {
    const url = new URL(window.location.href);
    if (state.projectId) url.searchParams.set('project', state.projectId); else url.searchParams.delete('project');
    if (state.view === '3d') url.searchParams.set('view', '3d'); else url.searchParams.delete('view');
    window.history.replaceState(null, '', url);
  }

  function notify(message, type = '') {
    const node = document.createElement('div');
    node.className = `notice ${type}`;
    node.textContent = message;
    $('status-region').append(node);
    setTimeout(() => node.remove(), 6500);
  }

  async function api(url, options = {}) {
    const method = String(options.method || 'GET').toUpperCase();
    if (method !== 'GET' && !(method === 'POST' && url === ENDPOINTS.search)) {
      throw new Error('이 대시보드는 읽기 전용입니다. 변경은 로컬 AI MCP에서 실행하세요.');
    }
    const headers = new Headers(options.headers || {});
    headers.set('Accept', 'application/json');
    if (options.body !== undefined) headers.set('Content-Type', 'application/json');
    if (token()) headers.set('Authorization', `Bearer ${token()}`);
    let response;
    try { response = await fetch(url, { ...options, headers }); }
    catch (error) { throw new Error(`API에 연결하지 못했습니다: ${errorMessage(error)}`); }
    const raw = await response.text();
    let body = null;
    try { body = raw ? JSON.parse(raw) : null; } catch (_) { body = raw; }
    if (!response.ok) {
      const detail = body && typeof body === 'object' ? (body.message || body.code || body.error) : raw;
      throw new Error(`${response.status} ${detail || response.statusText}`);
    }
    return body;
  }

  function recordButton(entry, label) {
    const button = document.createElement('button');
    button.type = 'button';
    button.textContent = label || titleForId(entry.id || entry.revision_id);
    button.addEventListener('click', () => openLinkedRecord(entry.id || entry.revision_id));
    return button;
  }
  function renderList(id, entries, emptyText) {
    const list = $(id);
    clear(list);
    if (!entries.length) {
      const li = document.createElement('li');
      li.className = 'muted';
      li.textContent = emptyText;
      list.append(li);
      return;
    }
    entries.forEach((entry) => {
      const li = document.createElement('li');
      const item = typeof entry === 'object' ? entry : { title: entry };
      const recordId = item.id || item.revision_id || item.to_id || item.from_id;
      if (recordId) {
        const linked = recordMap().get(recordId);
        li.append(recordButton({ id: recordId }, `${humanEntryTitle(item.title || item.statement || item.body_preview, recordId)}${timestampSuffix(item.recorded_at ? item : linked)}`));
      }
      else li.textContent = text(item.title || item.statement || item);
      list.append(li);
    });
  }
  function occurrenceLabel(occurrence) {
    const root = titleForId(occurrence?.root_revision_id, '루트 구성');
    const path = asArray(occurrence?.slot_path).length ? `${asArray(occurrence.slot_path).length}단계 하위 위치` : '루트';
    const roles = asArray(occurrence?.roles).join(', ') || '역할 없음';
    return `${root} · ${path} · ${roles}`;
  }
  function occurrenceButton(revisionId, occurrence, className = 'occurrence-button', knownRoot = null) {
    const contextualOccurrence = occurrence?.root_revision_id || !knownRoot
      ? occurrence
      : { ...occurrence, root_revision_id: knownRoot };
    const button = document.createElement('button');
    button.type = 'button';
    button.className = className;
    button.textContent = occurrenceLabel(contextualOccurrence);
    button.addEventListener('click', () => selectRecord(revisionId, contextualOccurrence));
    return button;
  }
  function renderOccurrences(id, occurrences, emptyText) {
    const list = $(id);
    clear(list);
    if (!occurrences.length) {
      const li = document.createElement('li');
      li.className = 'muted';
      li.textContent = emptyText;
      list.append(li);
      return;
    }
    occurrences.forEach((occurrence) => {
      const li = document.createElement('li');
      li.append(occurrenceButton(occurrence.revision_id, occurrence));
      list.append(li);
    });
  }
  function snapshotNodeMap() {
    return new Map(asArray(state.snapshot?.nodes).map((node) => [node.revision_id, node]));
  }

  function appendFact(rows, name, value) {
    if (value === undefined || value === null || value === '') return;
    rows.push([name, formatValue(value)]);
  }

  function renderFacts(rows) {
    const facts = $('occurrence-details');
    clear(facts);
    rows.forEach(([name, value]) => {
      const dt = document.createElement('dt');
      const dd = document.createElement('dd');
      dt.textContent = name;
      dd.textContent = value;
      facts.append(dt, dd);
    });
  }

  async function openLinkedRecord(recordId) {
    if (!recordId) return;
    const generation = state.contextGeneration;
    const requestGeneration = ++state.recordGeneration;
    try {
      const payload = await api(`${ENDPOINTS.record(recordId)}${query({ include: 'links,lineage,evidence,occurrences', ...recordScope() })}`);
      if (generation !== state.contextGeneration || requestGeneration !== state.recordGeneration) return;
      if (payload.record?.kind === 'entity') {
        const current = asArray(state.snapshot?.nodes).find((node) => node.entity_id === recordId);
        const fallback = asArray(payload.revisions_of_entity).sort((a, b) => b.seq - a.seq)[0];
        if (current?.revision_id || fallback?.id) return selectRecord(current?.revision_id || fallback.id);
      }
      renderRecord(payload, null);
      revealSelectedDetail();
    } catch (error) {
      if (generation !== state.contextGeneration || requestGeneration !== state.recordGeneration) return;
      notify(`연결된 기록을 열지 못했습니다: ${errorMessage(error)}`, 'error');
    }
  }

  function renderTree() {
    const container = $('tree');
    clear(container);
    const root = state.snapshot?.tree;
    if (!root) {
      const p = document.createElement('p');
      p.className = 'tree-empty';
      p.textContent = '이 스냅샷에는 아직 작업 head가 없습니다.';
      container.append(p);
      return;
    }
    let rendered = 0;
    const nodes = snapshotNodeMap();
    function branch(occurrences) {
      const ul = document.createElement('ul');
      asArray(occurrences).forEach((occurrence) => {
        if (rendered >= maxTreeNodes()) return;
        rendered += 1;
        const li = document.createElement('li');
        const children = asArray(occurrence.children);
        const details = nodes.get(occurrence.revision_id) || {};
        const row = document.createElement('div');
        row.className = 'tree-row';
        const toggle = document.createElement('button');
        toggle.type = 'button';
        toggle.className = 'icon-button tree-toggle';
        toggle.textContent = children.length ? '⌄' : '·';
        toggle.setAttribute('aria-label', children.length ? '하위 항목 접기' : '하위 항목 없음');
        row.append(toggle);
        const item = document.createElement('button');
        item.type = 'button';
        item.className = 'tree-item';
        item.dataset.recordId = occurrence.revision_id;
        item.textContent = text(details.title, titleForId(occurrence.revision_id, '구성 항목'));
        item.addEventListener('click', () => selectRecord(
          occurrence.revision_id,
          occurrence.root_revision_id ? occurrence : { ...occurrence, root_revision_id: state.snapshot.root_revision_id },
        ));
        row.append(item);
        const kind = document.createElement('span');
        kind.className = 'tree-kind';
        kind.textContent = kindLabel(details.entity_kind);
        item.title = `${text(details.title, '항목')} · 서버 기록 ${recordTime(details)}`;
        row.append(kind);
        li.append(row);
        if (children.length) {
          const childList = branch(children);
          toggle.addEventListener('click', () => {
            childList.hidden = !childList.hidden;
            toggle.textContent = childList.hidden ? '›' : '⌄';
            toggle.setAttribute('aria-label', childList.hidden ? '하위 항목 펼치기' : '하위 항목 접기');
          });
          li.append(childList);
        }
        ul.append(li);
      });
      return ul;
    }
    container.append(branch([root]));
    if (state.snapshot.truncated || rendered >= maxTreeNodes()) {
      const p = document.createElement('p');
      p.className = 'tree-empty';
      p.textContent = `표시는 ${maxTreeNodes()}개 사용 위치로 제한되었습니다. 한도를 늘리거나 검색 범위를 좁히세요.`;
      container.append(p);
    }
  }

  function renderRecord(payload, selectedOccurrence) {
    const record = payload.record;
    const data = record.data || {};
    const entity = payload.entity?.data || {};
    const occurrence = selectedOccurrence || null;
    state.selected = { record, occurrence, entityKind: entity.entity_kind || (data.entity_id === state.projectId ? 'project' : null) };
    $('record-empty').hidden = true;
    $('record-detail').hidden = false;
    const snapshotNode = snapshotNodeMap().get(record.id);
    $('record-kind').textContent = kindLabel(record.kind);
    $('record-title').textContent = text(entity.title || titleForRecord(record, '') || snapshotNode?.title, '제목 없는 기록');
    $('current-selection').textContent = `현재 선택: ${$('record-title').textContent} · ${kindLabel(entity.entity_kind || record.kind)}${occurrence?.roles?.length ? ` · ${occurrence.roles.join(', ')}` : ''}`;
    $('record-time').textContent = `서버 기록 ${recordTime(record)}`;
    $('record-raw').textContent = JSON.stringify(record, null, 2);
    $('record-meta').textContent = `record_id: ${record.id} · seq: ${text(record.seq)} · recorded_at: ${text(record.recorded_at)}${occurrence ? ` · root_revision_id: ${text(occurrence.root_revision_id)} · slot_path: ${asArray(occurrence.slot_path).join('/')}` : ''}`;
    const badgeHolder = $('record-badges');
    clear(badgeHolder);
    [entity.entity_kind, data.change_kind, data.source?.claim_mode, data.source?.origin].filter(Boolean).forEach((value) => {
      const badge = document.createElement('span');
      badge.className = `badge ${value === 'official' ? 'official' : value === 'ai' || value === 'inferred' ? 'proposed' : ''}`;
      badge.textContent = ({project:'프로젝트',schema:'스키마',core:'핵심 묶음',idea:'아이디어',composition:'구성 변경',semantic:'의미 변경',initial:'최초 버전',correction:'교정',inferred:'추론',extracted:'원문 추출',human:'사람 입력',ai:'AI 입력',official:'공식'}[value] || String(value));
      badgeHolder.append(badge);
    });
    const typedContent = {
      revision: data.body,
      capture: data.content,
      observation: `${text(data.metric)}: ${formatValue(data.value)}`,
      goal: data.statement,
      assessment: data.note,
      artifact: data.uri,
      candidate: data.body || data.title,
      promotion: data.reason,
    };
    $('record-content').textContent = text(typedContent[record.kind] || data.note || data.description, '표시할 본문이 없습니다.');
    const facts = [];
    appendFact(facts, '종류', kindLabel(record.kind));
    appendFact(facts, '역할', occurrence?.roles?.join(', '));
    appendFact(facts, '사용 위치', occurrence?.slot_path?.length ? `${occurrence.slot_path.length}단계 하위 위치` : occurrence ? '루트' : null);
    if (record.kind === 'capture') {
      appendFact(facts, '발생 시각', data.occurred_at);
      appendFact(facts, '출처', data.source_kind);
    } else if (record.kind === 'observation') {
      appendFact(facts, 'metric', data.metric);
      appendFact(facts, 'value', data.value);
      appendFact(facts, 'status', data.status);
      appendFact(facts, '발생 시각', data.occurred_at);
      appendFact(facts, '실행 환경', data.environment?.runtime);
      appendFact(facts, '모델 버전', data.environment?.model_version);
    } else if (record.kind === 'goal') {
      appendFact(facts, '판정 기준', asArray(data.criteria).map(criterion => `${criterion.statement}${criterion.threshold !== null && criterion.threshold !== undefined ? ` · ${comparatorLabel(criterion.comparator)} ${metricValue(criterion.threshold, criterion.unit)}` : ''}`).join(' / '));
    } else if (record.kind === 'assessment') {
      appendFact(facts, 'status', data.status);
      appendFact(facts, 'evidence cutoff', data.evidence_cutoff_at);
    } else if (record.kind === 'artifact') {
      appendFact(facts, 'media type', data.media_type);
      appendFact(facts, 'size', data.size_bytes);
    } else if (record.kind === 'promotion') {
    } else if (record.kind === 'candidate') {
      appendFact(facts, 'status', data.status);
      appendFact(facts, 'origin', data.origin);
    }
    const anchor = record.kind === 'candidate' ? data.source_anchor : data.source?.source_anchor;
    if (anchor) {
      appendFact(facts, 'source anchor start (code point)', anchor.start);
      appendFact(facts, 'source anchor end (code point)', anchor.end);
    }
    renderFacts(facts);
    const source = [];
    if (data.source?.capture_id) source.push({ title: `${text(data.source.origin, '출처')} · ${text(data.source.claim_mode, '주장')} · ${titleForId(data.source.capture_id, '원문')}`, id: data.source.capture_id });
    if (record.kind === 'candidate' && data.capture_id && data.capture_id !== data.source?.capture_id) {
      source.push({ title: `후보 원문 · ${titleForId(data.capture_id, '원문')}`, id: data.capture_id });
    }
    renderList('source-list', source, '연결된 출처가 없습니다.');
    renderOccurrences('uses-list', asArray(payload.occurrences), '선택한 기록에 사용 위치가 없습니다.');
    const lineage = payload.lineage || {};
    renderList('lineage-list', [...asArray(lineage.derived_from), ...asArray(lineage.derives), ...asArray(lineage.corrections)], '계보가 없습니다.');
    const evidence = payload.evidence || {};
    renderList('criteria-list', [...asArray(evidence.assessments), ...asArray(evidence.observations)].map((item) => ({ id: item.id, title: `${kindLabel(item.kind)} · ${text(item.data?.status || item.data?.metric, '기록')} · ${recordTime(item)}` })), '평가나 관측 근거가 없습니다.');
    renderVersionDiff(record);
    renderSchemaGoals(state.goalPayload);
  }

  function slotDescription(slot) {
    const revisionTitle = titleForId(slot?.revision_id, '연결된 항목');
    const roles = asArray(slot?.roles).join(', ');
    return roles ? `${revisionTitle} · 역할 ${roles}` : revisionTitle;
  }

  function appendDiffSlotList(holder, heading, slots) {
    if (!asArray(slots).length) return;
    const block = document.createElement('div');
    block.className = 'diff-slots';
    const title = document.createElement('h4');
    title.textContent = heading;
    const list = document.createElement('ul');
    asArray(slots).forEach((slot) => {
      const item = document.createElement('li');
      if (slot?.before || slot?.after) item.textContent = `${slotDescription(slot.before)} → ${slotDescription(slot.after)}`;
      else item.textContent = slotDescription(slot);
      list.append(item);
    });
    block.append(title, list);
    holder.append(block);
  }

  function versionLabel(record) {
    return `${text(record?.data?.body, titleForId(record?.id, '버전')).split('\n')[0].slice(0, 72)} · ${recordTime(record)}`;
  }

  function appendVersionHistory(holder, selected, history, cutoff, compareId, afterId) {
    const controls = document.createElement('div'); controls.className = 'version-controls';
    const beforeLabel = document.createElement('label'); beforeLabel.textContent = '비교 시작 버전';
    const before = document.createElement('select'); before.id = 'version-compare-from';
    const endLabel = document.createElement('label'); endLabel.textContent = '비교 끝 버전';
    const after = document.createElement('select'); after.id = 'version-compare-to';
    const target = history.revisions.find(r => r.id === afterId) || selected;
    const prior = history.revisions.filter(r => r.id !== target.id);
    const placeholder = document.createElement('option'); placeholder.value = ''; placeholder.textContent = prior.length ? '계보에서 비교할 버전 선택' : '최초 버전'; before.append(placeholder);
    for (const record of prior) { const option = document.createElement('option'); option.value = record.id; option.textContent = versionLabel(record); before.append(option); }
    for (const record of history.revisions) { const option = document.createElement('option'); option.value = record.id; option.textContent = versionLabel(record); after.append(option); }
    before.value = prior.some(r => r.id === compareId) ? compareId : ''; after.value = target.id;
    before.addEventListener('change', () => renderVersionDiff(selected, before.value || null, after.value));
    after.addEventListener('change', () => renderVersionDiff(selected, null, after.value));
    beforeLabel.append(before); endLabel.append(after); controls.append(beforeLabel, endLabel); holder.append(controls);
    const chain = document.createElement('div'); chain.className = 'version-chain'; chain.setAttribute('aria-label', '이 항목의 버전 계보');
    history.revisions.forEach((revision, index) => {
      const button = document.createElement('button'); button.type = 'button'; button.className = 'inline-button';
      button.textContent = `${index + 1}. ${text(revision.data.body, titleForId(revision.id, '버전')).split('\n')[0].slice(0, 44)}`;
      button.setAttribute('aria-pressed', String(revision.id === target.id));
      button.addEventListener('click', () => renderVersionDiff(selected, null, revision.id)); chain.append(button);
    }); holder.append(chain);
    const detail = document.createElement('details'); detail.className = 'version-local-history';
    const heading = document.createElement('summary'); heading.textContent = `이 항목의 방향과 기록 · ${history.revisions.length}개 버전${history.truncated ? ' · 계보 탐색 한도 도달' : ''}`; detail.append(heading);
    const timeline = document.createElement('ol');
    for (const revision of history.revisions) {
      const item = document.createElement('li');
      const heading = document.createElement('strong'); heading.textContent = versionLabel(revision); item.append(heading);
      const delta = window.IdeaDashboardModel.revisionDiff(state.exportRecords, revision.id);
      const reason = document.createElement('p'); reason.textContent = delta.reason ? `방향 변경 이유: ${delta.reason}` : '별도로 기록된 변경 이유 없음'; item.append(reason);
      const activity = window.IdeaVersionHistory.revisionActivity(state.exportRecords, revision.id, cutoff, iso($('effective-at').value));
      if (!activity.length) { const empty = document.createElement('p'); empty.textContent = '연결된 원문·관측·평가 기록 없음'; item.append(empty); }
      for (const record of activity) {
        const button = document.createElement('button'); button.type = 'button'; button.className = 'inline-button version-activity';
        button.textContent = `${kindLabel(record.kind)} · ${titleForRecord(record, '연결된 기록')} · ${recordTime(record)}`;
        button.addEventListener('click', () => openLinkedRecord(record.id)); item.append(button);
      }
      timeline.append(item);
    }
    detail.append(timeline); holder.append(detail);
  }

  function renderVersionDiff(record, compareId = null, afterId = null) {
    const section = $('version-diff');
    const holder = $('version-diff-content');
    clear(holder);
    if (record.kind !== 'revision' || !window.IdeaDashboardModel || !Array.isArray(state.exportRecords)) {
      section.hidden = true;
      return;
    }
    const cutoffValue = state.goalPayload?.scope?.known_seq ?? state.snapshot?.selected_by?.known_seq;
    const cutoff = cutoffValue == null ? Infinity : Number(cutoffValue);
    const history = window.IdeaVersionHistory?.revisionHistory(state.exportRecords, record.id, cutoff);
    const target = history?.revisions.find(revision => revision.id === afterId) || record;
    const diff = window.IdeaDashboardModel.revisionDiff(state.exportRecords, target.id, compareId || undefined);
    section.hidden = false;
    if (history?.revisions.length) appendVersionHistory(holder, record, history, cutoff, diff.compare_revision_id, target.id);
    const summary = document.createElement('p');
    summary.className = 'diff-status';
    if (diff.status === 'ready') summary.textContent = `${titleForId(diff.before.id, '선택한 시작 버전')} → ${titleForId(diff.after.id, '선택한 끝 버전')} 비교.${diff.reason ? ` 끝 버전의 변경 이유: ${diff.reason}` : ''}`;
    else if (diff.status === 'ambiguous') summary.textContent = '이전 버전 후보가 여럿입니다. 실제 계보를 선택해야 비교를 표시합니다.';
    else summary.textContent = '비교할 이전 버전이 확정되지 않았습니다.';
    holder.append(summary);
    if (diff.status === 'ambiguous' && asArray(diff.candidates).length) {
      const candidates = document.createElement('div');
      candidates.className = 'diff-candidates';
      asArray(diff.candidates).forEach((candidate) => {
        const id = typeof candidate === 'string' ? candidate : candidate?.revision_id || candidate?.id;
        if (!id) return;
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'occurrence-button';
        button.textContent = `${titleForId(id, '이전 버전')} · ${recordTime(recordMap().get(id))} 비교`;
        button.addEventListener('click', () => renderVersionDiff(record, id, target.id));
        candidates.append(button);
      });
      holder.append(candidates);
      return;
    }
    if (diff.status !== 'ready') return;
    const copy = document.createElement('div');
    copy.className = 'diff-copy';
    const beforeChars = Array.from(diff.before?.body || '');
    const afterChars = Array.from(diff.after?.body || '');
    let prefix = 0, suffix = 0;
    while (prefix < beforeChars.length && prefix < afterChars.length && beforeChars[prefix] === afterChars[prefix]) prefix++;
    while (suffix < beforeChars.length - prefix && suffix < afterChars.length - prefix && beforeChars.at(-1 - suffix) === afterChars.at(-1 - suffix)) suffix++;
    [['− 비교 시작 본문', beforeChars, 'del'], ['+ 비교 끝 본문', afterChars, 'ins']].forEach(([label, chars, tag]) => {
      const block = document.createElement('div');
      const heading = document.createElement('h4');
      heading.textContent = label;
      const paragraph = document.createElement('p');
      paragraph.append(document.createTextNode(chars.slice(0, prefix).join('')));
      const difference = chars.slice(prefix, chars.length - suffix).join('');
      if (difference) {
        const highlight = document.createElement(tag);
        highlight.textContent = difference;
        paragraph.append(highlight);
      }
      if (suffix) paragraph.append(document.createTextNode(chars.slice(-suffix).join('')));
      if (!chars.length) paragraph.textContent = '본문 없음';
      block.append(heading, paragraph);
      copy.append(block);
    });
    holder.append(copy);
    appendDiffSlotList(holder, '추가된 사용 위치', diff.slots?.added);
    appendDiffSlotList(holder, '제거된 사용 위치', diff.slots?.removed);
    appendDiffSlotList(holder, '변경된 역할 또는 연결', diff.slots?.changed);
    if (asArray(diff.metadata_changes).length) {
      const changes = document.createElement('p');
      changes.className = 'diff-status';
      changes.textContent = `변경된 정보: ${asArray(diff.metadata_changes).map((change) => ({tags:'태그',source:'출처',change_kind:'변경 유형',correction_of:'교정 대상'}[change.field] || change.field)).join(', ')}`;
      holder.append(changes);
    }
  }

  async function selectRecord(revisionId, occurrence = null, reveal = true) {
    const generation = state.contextGeneration;
    const requestGeneration = ++state.recordGeneration;
    try {
      const payload = await api(`${ENDPOINTS.record(revisionId)}${query({ include: 'links,lineage,evidence,occurrences', ...recordScope() })}`);
      if (generation !== state.contextGeneration || requestGeneration !== state.recordGeneration) return;
      document.querySelectorAll('.tree-item[aria-current="true"]').forEach((node) => node.removeAttribute('aria-current'));
      document.querySelector(`.tree-item[data-record-id="${CSS.escape(String(revisionId))}"]`)?.setAttribute('aria-current', 'true');
      renderRecord(payload, occurrence);
      if (reveal) revealSelectedDetail();
    } catch (error) {
      if (generation !== state.contextGeneration || requestGeneration !== state.recordGeneration) return;
      notify(`기록을 불러오지 못했습니다: ${errorMessage(error)}`, 'error');
    }
  }

  function setView(view, reveal = true) {
    state.view = view === '3d' ? '3d' : '2d';
    $('view-2d').setAttribute('aria-pressed', String(state.view === '2d'));
    $('view-3d').setAttribute('aria-pressed', String(state.view === '3d'));
    $('graph-workbench').hidden = state.view !== '3d';
    updateRoute();
    if (state.view === '3d' && state.projectId) {
      loadGraph();
      if (reveal) $('graph-workbench').scrollIntoView({ block: 'start', behavior: 'smooth' });
    } else if (reveal) {
      revealSelectedDetail();
    }
  }

  function revealSelectedDetail() {
    if ($('record-detail').hidden) return;
    requestAnimationFrame(() => $('record-detail').scrollIntoView({ block: 'start', behavior: 'smooth' }));
  }

  function graphTime(value) {
    return formatTime(value);
  }

  function useHistoricalScope() {
    $('stream-select').value = 'working';
    $('effective-at').value = '';
  }

  function updateLatestButton() {
    const historical = Boolean(state.rootRevisionId) || state.knownSeq !== null || Boolean($('known-at').value) || Boolean($('effective-at').value) || $('stream-select').value !== 'working';
    $('latest-button').hidden = !historical;
  }

  function openLatest() {
    state.rootRevisionId = null;
    state.knownSeq = null;
    $('stream-select').value = 'working';
    $('known-at').value = '';
    $('effective-at').value = '';
    loadProject();
  }

  function captureTimelineTitle(capture) {
    const content = typeof capture?.data?.content === 'string' ? capture.data.content.trim() : '';
    const heading = content.match(/^\s{0,3}#{1,6}\s+(.+?)\s*#*\s*$/m);
    if (heading?.[1]) return heading[1].trim();
    if (content && !/^[{[]/.test(content)) return content.split('\n').find((line) => line.trim())?.trim().slice(0, 90) || null;
    return null;
  }

  function observationTimelineTitle(observation, recordsById) {
    const data = observation?.data || {};
    const revision = recordsById.get(data.target_revision_id);
    const target = recordsById.get(revision?.data?.entity_id)?.data?.title;
    const metric = text(data.metric, '관측');
    const status = data.status ? ` · ${data.status}` : '';
    return `${target || '대상 미지정'} · ${metric}${status}`;
  }

  async function loadProjectExport(generation = state.contextGeneration) {
    const requestedProject = state.projectId;
    if (!requestedProject) return [];
    if (generation !== state.contextGeneration) return [];
    if (state.exportProjectId === requestedProject && Array.isArray(state.exportRecords)) return state.exportRecords;
    const payload = await api(`${ENDPOINTS.export}${query({ project_id: requestedProject, include_receipts: false })}`);
    const records = asArray(payload?.content?.records);
    if (generation !== state.contextGeneration || state.projectId !== requestedProject) return [];
    state.exportProjectId = requestedProject;
    state.exportRecords = records;
    return records;
  }

  function graphNodeList(graph) {
    const holder = $('graph-node-list');
    clear(holder);
    const nodes = asArray(graph?.nodes);
    if (!nodes.length) {
      const item = document.createElement('li');
      item.className = 'muted';
      item.textContent = '프로젝트 이력에서 표시할 revision이 없습니다.';
      holder.append(item);
      return;
    }
    nodes.forEach((node) => {
      const item = document.createElement('li');
      item.className = 'graph-node-row';
      const button = document.createElement('button');
      button.type = 'button';
      button.textContent = text(node.title, '제목 없는 항목');
      button.addEventListener('click', () => selectGraphNode(node));
      const meta = document.createElement('span');
      meta.className = 'graph-node-meta';
      meta.textContent = `${kindLabel(node.kind)} · ${graphTime(node.recorded_at)} · 깊이 ${text(node.y)} · 층 ${text(node.layer)} · 사용 위치 ${asArray(node.usages).length}`;
      item.append(button, meta);
      holder.append(item);
    });
  }

  function filteredGraph() {
    const graph = state.graph;
    if (!graph) return null;
    const layer = $('graph-layer-filter').value;
    if (layer === '') return graph;
    const nodes = asArray(graph.nodes).filter((node) => String(node.layer) === layer);
    const ids = new Set(nodes.map((node) => node.id));
    return { ...graph, nodes, edges: asArray(graph.edges).filter((edge) => ids.has(edge.source) && ids.has(edge.target)) };
  }

  function renderGraph() {
    const graph = filteredGraph();
    if (!graphRenderer || !graph) return;
    graphRenderer.setGraph(graph);
    graphNodeList(graph);
    const all = state.graph;
    const times = asArray(graph.nodes).map((node) => node.recorded_at).filter(Boolean).sort();
    const overlaps = new Map();
    asArray(graph.nodes).forEach((node) => {
      const key = `${node.x}|${node.y}|${node.z}`;
      overlaps.set(key, (overlaps.get(key) || 0) + 1);
    });
    const collisionGroups = [...overlaps.values()].filter((count) => count > 1).length;
    const truncation = all.truncation || {};
    const limits = [];
    if (truncation.truncated || Number(truncation.omitted_nodes) > 0) limits.push(`노드 한도 ${truncation.max_nodes || 5000}: ${truncation.omitted_nodes || 0}개 제외`);
    if (truncation.occurrence_limited) limits.push(`사용 위치 탐색 ${truncation.max_occurrences || 20000}회 한도에서 중단`);
    if (truncation.depth_limited) limits.push(`깊이 한도 ${truncation.max_depth || 32}: ${truncation.depth_limited}개 중단`);
    if (truncation.cycles) limits.push(`순환 ${truncation.cycles}개 감지`);
    $('graph-summary').textContent = `프로젝트 전체 이력 · 표시 노드 ${asArray(graph.nodes).length}/${asArray(all.nodes).length} · edge ${asArray(graph.edges).length}/${asArray(all.edges).length}${times.length ? ` · 기록 ${graphTime(times[0])} — ${graphTime(times[times.length - 1])}` : ''}${collisionGroups ? ` · 같은 축 좌표 ${collisionGroups}묶음은 선택 가능하도록 시각적으로 펼침` : ''}${limits.length ? ` · ${limits.join(' · ')}` : ''}`;
  }

  function selectGraphNode(node) {
    graphRenderer?.setSelected(node.id);
    const selection = $('graph-selection');
    clear(selection);
    const title = document.createElement('span');
    title.textContent = `${text(node.title, '제목 없는 항목')} · 서버 기록 ${graphTime(node.recorded_at)}`;
    selection.append(title);
    const usages = asArray(node.usages);
    if (usages.length === 1 && node.revision_id) {
      openGraphOccurrence(node, usages[0]);
      return;
    }
    if (usages.length > 1 && node.revision_id) {
      const picker = document.createElement('div');
      picker.className = 'occurrence-picker';
      const label = document.createElement('span');
      label.className = 'result-meta';
      label.textContent = '공유 revision입니다. 사용 위치를 명시적으로 선택하세요.';
      picker.append(label);
      usages.forEach((usage) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'occurrence-button';
        button.textContent = occurrenceLabel({ ...usage, revision_id: node.revision_id });
        button.addEventListener('click', () => openGraphOccurrence(node, usage));
        picker.append(button);
      });
      selection.append(picker);
      return;
    }
    if (node.revision_id) {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'inline-button';
      button.textContent = '기록 상세 열기';
      button.addEventListener('click', () => openGraphRecord(node));
      selection.append(document.createTextNode(' · '), button);
    }
  }

  async function openGraphRecord(node) {
    const requestedProject = state.projectId;
    useHistoricalScope();
    state.rootRevisionId = null;
    state.knownSeq = Number.isFinite(Number(node.seq)) ? Number(node.seq) : null;
    $('known-at').value = '';
    const generation = await loadProject();
    if (generation !== state.contextGeneration || state.projectId !== requestedProject) return;
    await selectRecord(node.revision_id);
    revealSelectedDetail();
  }

  async function openGraphOccurrence(node, usage) {
    const requestedProject = state.projectId;
    useHistoricalScope();
    const rootNode = asArray(state.graph?.nodes).find((candidate) => candidate.revision_id === usage.root_revision_id);
    state.rootRevisionId = usage.root_revision_id || null;
    state.knownSeq = Math.max(Number(node.seq) || 0, Number(rootNode?.seq) || 0) || null;
    $('known-at').value = '';
    const generation = await loadProject();
    if (generation !== state.contextGeneration || state.projectId !== requestedProject) return;
    await selectRecord(node.revision_id, { ...usage, revision_id: node.revision_id });
    revealSelectedDetail();
  }

  async function loadGraph(generation = state.contextGeneration) {
    $('graph-workbench').hidden = false;
    if (!window.IdeaGraphModel || !window.IdeaGraph3D) {
      $('graph-summary').textContent = '3D 그래프 모듈을 불러오지 못했습니다.';
      return;
    }
    try {
      const requestedProject = state.projectId;
      const records = await loadProjectExport(generation);
      if (generation !== state.contextGeneration || state.projectId !== requestedProject) return;
      state.graph = window.IdeaGraphModel.build(records, requestedProject);
      if (!graphRenderer) graphRenderer = window.IdeaGraph3D.create($('graph-canvas'), { onSelect: selectGraphNode });
      const select = $('graph-layer-filter');
      const prior = select.value;
      clear(select);
      select.add(new Option('모든 층', ''));
      asArray(state.graph.layers).forEach((layer) => select.add(new Option(`${layer.index} · ${layer.label}`, String(layer.index))));
      select.value = [...select.options].some((option) => option.value === prior) ? prior : '';
      renderGraph();
    } catch (error) {
      if (generation !== state.contextGeneration) return;
      $('graph-summary').textContent = `프로젝트 이력 export를 불러오지 못했습니다: ${errorMessage(error)}`;
      clear($('graph-node-list'));
      notify(errorMessage(error), 'error');
    }
  }

  async function loadProjects() {
    const select = $('project-select');
    select.disabled = true;
    try {
      const data = await api(`${ENDPOINTS.state}${query({ limit: 200 })}`);
      const projects = asArray(data.projects);
      clear(select);
      if (!projects.length) {
        select.add(new Option('프로젝트가 없습니다', ''));
        state.projectId = '';
        state.snapshot = null;
        renderTree();
        $('tree-summary').textContent = '이 DB는 비어 있습니다. export 파일은 자동으로 불러오지 않습니다. 프로젝트와 원문 입력은 로컬 AI MCP에서 실행하세요.';
        return;
      }
      projects.forEach((project) => {
        const suffix = /(?:30\s*(?:step|단계)|lifecycle|라이프사이클)/i.test(text(project.title, '')) ? ' · 라이프사이클 검증' : '';
        select.add(new Option(`${project.title}${suffix}`, project.project_id));
      });
      if (!projects.some((project) => project.project_id === state.projectId)) state.projectId = projects[0].project_id;
      select.value = state.projectId;
      await loadProject();
    } catch (error) {
      clear(select);
      select.add(new Option('프로젝트를 불러오지 못함', ''));
      state.projectId = '';
      state.snapshot = null;
      renderTree();
      $('tree-summary').textContent = `API 오류: ${errorMessage(error)}`;
      notify(errorMessage(error), 'error');
    } finally {
      select.disabled = false;
    }
  }

  async function loadProject(options = {}) {
    const nextProjectId = $('project-select').value;
    const changingProject = state.projectId !== nextProjectId;
    const generation = ++state.contextGeneration;
    state.recordGeneration += 1;
    const initialRecordGeneration = state.recordGeneration;
    if (changingProject) {
      state.rootRevisionId = null;
      state.knownSeq = null;
      $('known-at').value = '';
      $('effective-at').value = '';
    }
    state.projectId = nextProjectId;
    state.goalPayload = null;
    updateLatestButton();
    if (state.exportProjectId !== state.projectId) {
      state.exportProjectId = null;
      state.exportRecords = null;
      state.graph = null;
    }
    updateRoute();
    state.selected = null;
    state.snapshot = null;
    clear($('tree'));
    clear($('project-schema-summary'));
    clear($('goal-state'));
    $('current-selection').textContent = '선택한 구성을 불러오는 중입니다.';
    $('record-detail').hidden = true;
    $('record-empty').hidden = false;
    if (!state.projectId) {
      state.snapshot = null;
      renderTree();
      return generation;
    }
    try {
      const exportPromise = loadProjectExport(generation);
      const snapshot = await api(`${ENDPOINTS.snapshot}${query({ ...scope(), max_nodes: maxTreeNodes(), depth: 32 })}`);
      if (generation !== state.contextGeneration) return null;
      state.snapshot = snapshot;
      renderTree();
      const selected = state.snapshot.selected_by || {};
      const rootTitle = text(snapshotNodeMap().get(state.snapshot.root_revision_id)?.title, '제목 없는 최신 구성');
      const friendlySource = state.rootRevisionId ? '선택한 이력' : (state.snapshot.selected_by?.publication_id ? '공식 구성' : '작업 중 구성');
      const lowerLimitNotice = maxTreeNodes() < 5000 ? ` · 낮은 표시 한도 ${maxTreeNodes()}개 선택됨` : '';
      const scopeTime = selected.known_at ? ` · 조회 시점 ${formatTime(selected.known_at)}` : '';
      $('tree-summary').textContent = state.snapshot.root_revision_id ? `${friendlySource} · ${rootTitle}${scopeTime}${lowerLimitNotice}${state.snapshot.truncated ? ` · ${maxTreeNodes()}개에서 잘림` : ''}` : '선택한 서버 기록 시점에는 구성이 없습니다.';
      $('current-selection').textContent = state.snapshot.root_revision_id ? `현재 선택: ${friendlySource} · ${rootTitle}` : '현재 선택: 이 시점에는 구성이 없습니다.';
      await Promise.all([loadGoals(generation), exportPromise]);
      if (generation !== state.contextGeneration) return null;
      renderSchemaGoals(state.goalPayload);
      loadTimeline(generation);
      if (state.view === '3d') await loadGraph(generation);
      if (state.snapshot.root_revision_id && options.autoSelect !== false && state.recordGeneration === initialRecordGeneration) {
        await selectRecord(state.snapshot.root_revision_id, { root_revision_id: state.snapshot.root_revision_id, revision_id: state.snapshot.root_revision_id, slot_path: [], roles: [] }, false);
      }
      return generation === state.contextGeneration ? generation : null;
    } catch (error) {
      if (generation !== state.contextGeneration) return null;
      state.snapshot = null;
      renderTree();
      $('tree-summary').textContent = `구성을 불러오지 못했습니다: ${errorMessage(error)}`;
      notify(errorMessage(error), 'error');
      return null;
    }
  }

  function appendSearchResult(holder, entry, lane, selectedRoot = null) {
    const card = document.createElement('article');
    card.className = `result ${lane}`;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'result-record';
    const id = entry.revision_id || entry.record_id || entry.candidate_id;
    const title = document.createElement('span');
    title.className = 'result-title';
    title.textContent = humanEntryTitle(entry.title, id, kindLabel(entry.kind));
    const meta = document.createElement('span');
    meta.className = 'result-meta';
    meta.textContent = `${lane === 'related' ? `관련 문맥 · ${text(entry.reason)}` : lane === 'pending' ? `후보 · ${text(entry.origin)}` : '직접 일치'}${timestampSuffix(entry)}`;
    button.append(title, meta);
    if (id) button.addEventListener('click', () => selectRecord(id));
    card.append(button);
    const occurrences = asArray(entry.occurrences);
    if (id && occurrences.length) {
      const picker = document.createElement('div');
      picker.className = 'occurrence-picker';
      const label = document.createElement('span');
      label.className = 'result-meta';
      label.textContent = '사용 위치 선택';
      picker.append(label);
      occurrences.forEach((occurrence) => picker.append(occurrenceButton(id, occurrence, 'occurrence-button', selectedRoot)));
      card.append(picker);
    }
    holder.append(card);
  }

  async function search(event) {
    event?.preventDefault();
    const queryText = $('search-query').value.trim();
    const roles = $('role-query').value.split(',').map((value) => value.trim()).filter(Boolean);
    const kinds = Array.from(document.querySelectorAll('input[name="search-kind"]:checked')).map((input) => input.value);
    if (!queryText && !roles.length) {
      clear($('search-results'));
      $('search-state').textContent = '검색어 또는 역할을 입력하세요.';
      return;
    }
    $('search-state').textContent = '검색 중…';
    try {
      const payload = await api(ENDPOINTS.search, { method: 'POST', body: JSON.stringify({ ...scope(), scope: $('search-scope').value, query: queryText, roles, tags: [], kinds, lanes: ['official', 'candidate'], limit: 50, max_nodes: maxTreeNodes(), vector: null }) });
      const holder = $('search-results');
      clear(holder);
      const related = $('related-context').checked ? asArray(payload.related) : [];
      const candidates = asArray(payload.candidates);
      const selectedRoot = payload.selected_by?.root_revision_id || null;
      asArray(payload.results).forEach((entry) => appendSearchResult(holder, entry, 'direct', selectedRoot));
      asArray(payload.records).forEach((entry) => appendSearchResult(holder, entry, 'direct', selectedRoot));
      related.forEach((entry) => appendSearchResult(holder, entry, 'related', selectedRoot));
      candidates.forEach((entry) => appendSearchResult(holder, entry, 'pending', selectedRoot));
      const vector = payload.vector_status || {};
      $('search-state').textContent = `revision ${asArray(payload.results).length} · 기록 ${asArray(payload.records).length} · 관련 ${related.length} · 후보 ${candidates.length} · ${vector.used ? '벡터 사용' : '어휘 검색'}${payload.truncated ? ` · ${maxTreeNodes()}개 범위에서 잘림` : ''}`;
    } catch (error) {
      $('search-state').textContent = `검색 실패: ${errorMessage(error)}`;
      notify(errorMessage(error), 'error');
    }
  }

  function laneHasCurrentData(lane) {
    return Number(lane?.own) + Number(lane?.descendant) + Number(lane?.required?.total) + asArray(lane?.numeric).length > 0;
  }

  const comparatorLabel = (value) => ({ lte: '≤', lt: '<', gte: '≥', gt: '>', eq: '=', ne: '≠' }[String(value || '').toLowerCase()] || text(value, '기준'));
  const goalStatusLabel = (value) => ({ met: '충족', unmet: '미충족', unknown: '미측정', disputed: '이견', recheck: '재검토' }[String(value || '').toLowerCase()] || '미측정');

  function metricValue(value, unit) {
    const ratio = String(unit || '').toLowerCase().includes('ratio') || String(unit || '').includes('비율');
    if (ratio && typeof value === 'number') return `${(value * 100).toLocaleString('ko-KR', { maximumFractionDigits: 2 })}%`;
    return formatValue(value);
  }

  function scopedGoalStatements(schema, payload, proposed = false, ownOnly = false) {
    const root = state.snapshot?.root_revision_id;
    const payloadRoot = payload?.scope?.root_revision_id;
    return asArray(payload?.[proposed ? 'proposed_goals' : 'goals'])
      .filter((goal) => {
        if (asArray(goal.baselines).length && asArray(goal.baselines).every((baseline) => baseline.superseded_by != null)) return false;
        const scope = goal.scope || {};
        const path = asArray(scope.slot_path);
        return (scope.root_revision_id || payloadRoot) === root
          && (proposed ? scope.in_current_snapshot !== false : scope.in_current_snapshot === true)
          && path[0] === schema.slot_path?.[0] && (!ownOnly || (path.length === schema.slot_path.length && path.every((part, index) => part === schema.slot_path[index])));
      })
      .map((goal) => text(goal.statement, '목표 문장 없음'));
  }

  function appendProgressGoal(holder, progress, compact = false) {
    const block = document.createElement('section');
    block.className = 'goal-progress';
    if (progress.goal_id) block.dataset.goalId = progress.goal_id;
    block.dataset.origin = 'ai_proposed';
    const statement = document.createElement('p');
    statement.className = 'goal-progress-statement';
    statement.textContent = `목표: ${text(progress.statement, '목표 문장 없음')}`;
    const headline = document.createElement('p');
    headline.className = 'goal-progress-headline goal-progress-percent';
    headline.textContent = progress.percent === null ? 'AI 예상 달성률: 산정 불가' : `AI 예상 달성률 약 ${Math.round(progress.percent)}%`;
    block.append(statement, headline);
    const coverage = document.createElement('p');
    coverage.className = 'goal-progress-meta';
    coverage.textContent = `필수 기준 추정 ${progress.estimated_count}/${progress.required_count}개${progress.percent === null ? ' · 모든 필수 기준의 유효한 추정이 있어야 계산합니다.' : ' · 필수 기준을 같은 비중으로 평균한 현재 계획·구현·검증 이정표입니다.'}`;
    block.append(coverage);
    if (progress.percent !== null) {
      const meter = document.createElement('div');
      meter.className = 'goal-progress-meter'; meter.setAttribute('role', 'progressbar');
      meter.setAttribute('aria-label', `${text(progress.statement, '목표')} AI 예상 달성률`); meter.setAttribute('aria-valuemin', '0'); meter.setAttribute('aria-valuemax', '100'); meter.setAttribute('aria-valuenow', String(progress.percent));
      const fill = document.createElement('span'); fill.style.width = `${progress.percent}%`; meter.append(fill); block.append(meter);
    }
    const list = document.createElement('ul'); list.className = 'goal-progress-criteria';
    asArray(progress.criteria).forEach((criterion) => {
      const item = document.createElement('li'); item.className = 'criterion-progress'; item.dataset.criterionId = criterion.criterion_id;
      const title = document.createElement('strong'); title.textContent = `${text(criterion.statement, criterion.criterion_id)} · ${criterion.valid ? `${Math.round(criterion.percent)}%` : '추정 없음'}`;
      title.className = 'criterion-progress-percent';
      item.append(title);
      if (criterion.rationale) { const rationale = document.createElement('span'); rationale.className = 'progress-rationale'; rationale.textContent = criterion.rationale; item.append(rationale); }
      asArray(criterion.evidence_record_ids).forEach((id) => { const evidence = document.createElement('button'); evidence.type = 'button'; evidence.className = 'inline-button progress-evidence'; evidence.textContent = `근거: ${titleForId(id, '근거 기록')}`; evidence.addEventListener('click', () => openLinkedRecord(id)); item.append(evidence); });
      list.append(item);
    });
    if (!compact && asArray(progress.criteria).length) block.append(list);
    const provenance = progress.assessment || {};
    const details = document.createElement('details'); details.className = 'goal-progress-details';
    const summary = document.createElement('summary'); summary.textContent = 'AI 예상 산정 기준과 근거 범위';
    const copy = document.createElement('p');
    const milestones = provenance.rubric_version === 'goal-progress-milestones-v1'
      ? '0 계획 없음, 20 원자 계획, 40 적용 가능한 상세 설계, 60 적용 가능한 구현, 80 범위 내 부분 검증, 100 기준 검증 완료.'
      : text(provenance.note, '저장된 AI 평가 설명이 없습니다.');
    copy.textContent = `Rubric ${text(provenance.rubric_version, '미기록')} · ${text(provenance.evaluator, '평가자 미기록')} · 근거기록기준 ${text(provenance.evidence_cutoff_seq, '—')} / ${text(provenance.evidence_cutoff_at, '—')}. ${milestones} 실제 목표 지표 측정과 공식 충족 판정은 별도입니다.`;
    if (compact && asArray(progress.criteria).length) { const criterionCopy = document.createElement('p'); criterionCopy.textContent = progress.criteria.map((criterion) => `${text(criterion.statement, criterion.criterion_id)} ${criterion.valid ? `${Math.round(criterion.percent)}%` : '추정 없음'}${criterion.rationale ? ` · ${criterion.rationale}` : ''}`).join(' / '); details.append(criterionCopy); }
    details.prepend(summary); details.append(copy); block.append(details); holder.append(block);
  }

  function appendGoalLane(holder, title, lane, proposed = false, statements = []) {
    const block = document.createElement('section');
    block.className = 'goal-lane';
    block.dataset.origin = proposed ? 'ai_proposed' : 'official';
    const heading = document.createElement('h4');
    heading.textContent = title;
    block.append(heading);
    if (!laneHasCurrentData(lane)) {
      const unset = document.createElement('p');
      unset.textContent = proposed ? 'AI 제안 기준이 아직 없습니다.' : '현재 공식 목표가 설정되지 않았습니다.';
      block.append(unset);
      holder.append(block);
      return;
    }
    if (statements.length) {
      const statement = document.createElement('p');
      statement.className = 'goal-statement';
      statement.textContent = `목표: ${statements.join(' · ')}`;
      block.append(statement);
    }
    // A schema card may have several direct goals. Show each estimate; never average goals.
    asArray(lane.progress_goals).filter((progress) => progress.own).forEach((progress) => appendProgressGoal(block, progress));
    const coverage = document.createElement('p');
    coverage.textContent = `직접 목표 ${Number(lane.own) || 0}건 · 하위 항목 목표 ${Number(lane.descendant) || 0}건`;
    block.append(coverage);
    const required = lane.own_required || lane.required || {};
    const criteria = document.createElement('p');
    criteria.textContent = `필수 기준 ${Number(required.total) || 0}개 · 충족 ${Number(required.met) || 0} · 미충족 ${Number(required.unmet) || 0} · 확인 필요 ${Number(required.unknown) || 0}${Number(required.disputed) ? ` · 이견 ${required.disputed}` : ''}${Number(required.recheck) ? ` · 재확인 ${required.recheck}` : ''}`;
    block.append(criteria);
    if (!proposed && Number(required.total)) {
      const rate = document.createElement('p'); rate.className = 'official-completion-rate';
      const evaluated = Number(required.met || 0) + Number(required.unmet || 0);
      rate.textContent = evaluated === Number(required.total)
        ? `실제 기준 충족률 ${Math.round(Number(required.met || 0) / Number(required.total) * 100)}% · 이 목표의 필수 기준 ${evaluated}개 판정 기준`
        : `실제 기준 충족률 미확정 · 필수 기준 판정 ${evaluated}/${required.total}개`;
      block.append(rate);
    }
    if (asArray(lane.own_numeric || lane.numeric).length) {
      const values = document.createElement('ul');
      values.className = 'goal-numeric';
      asArray(lane.own_numeric || lane.numeric).forEach((criterion) => {
        const item = document.createElement('li');
        const measured = criterion.observed_value === null || criterion.observed_value === undefined ? '측정 없음' : metricValue(criterion.observed_value, criterion.unit);
        const isRatio = String(criterion.unit || '').toLowerCase().includes('ratio') || String(criterion.unit || '').includes('비율');
        const threshold = criterion.threshold === null || criterion.threshold === undefined ? '기준 미설정' : `${comparatorLabel(criterion.comparator)} ${metricValue(criterion.threshold, criterion.unit)}${criterion.unit && !isRatio ? ` ${criterion.unit}` : ''}`;
        item.textContent = `${text(criterion.statement, '정량 기준')} · ${proposed ? '기준 지표 값' : '실제 측정'} ${measured} · 목표 기준 ${threshold} · ${goalStatusLabel(criterion.status)}`;
        values.append(item);
      });
      block.append(values);
    }
    holder.append(block);
  }

  function laneSummary(lane) {
    const required = lane?.own_required || lane?.required || {};
    const total = Number(required.total) || 0;
    if (!total) return '기준 미설정';
    return `${Number(required.met) || 0}/${total} · 미측정 ${Number(required.unknown) || 0}`;
  }

  async function selectSchema(schema) {
    if (!schema?.schema_revision_id || !state.snapshot?.root_revision_id) return;
    await selectRecord(schema.schema_revision_id, {
      root_revision_id: state.snapshot.root_revision_id,
      revision_id: schema.schema_revision_id,
      slot_path: asArray(schema.slot_path),
      roles: []
    });
    $('record-detail').scrollIntoView({ block: 'start', behavior: 'smooth' });
    $('goal-state').focus({ preventScroll: true });
  }

  function renderProjectSummary(schemas, payload) {
    const title = $('project-summary-title');
    const meta = $('project-summary-meta');
    const rows = $('project-schema-summary');
    const rootTitle = text(snapshotNodeMap().get(state.snapshot?.root_revision_id)?.title, '제목 없는 최신 구성');
    const cutoff = Number(payload?.scope?.known_seq);
    const records = asArray(state.exportRecords).filter((record) => !Number.isFinite(cutoff) || Number(record.seq) <= cutoff);
    const latest = records.slice().sort((a, b) => Number(b.seq) - Number(a.seq))[0];
    const changes = records.filter((record) => record.kind === 'head_change').length;
    title.textContent = rootTitle;
    const rootProgress = window.IdeaDashboardModel?.occurrenceGoals
      ? [...asArray(window.IdeaDashboardModel.occurrenceGoals(state.exportRecords, state.snapshot, payload, []).official.progress_goals), ...asArray(window.IdeaDashboardModel.occurrenceGoals(state.exportRecords, state.snapshot, payload, []).ai_proposed.progress_goals)].filter((progress) => progress.own)
      : [];
    meta.textContent = `${state.rootRevisionId || state.knownSeq !== null ? '선택한 시점 기록' : '최신 서버 기록'} ${recordTime(latest)} · 구성 변경 ${changes}건 · 스키마 ${schemas.length}개`;
    clear(rows);
    const projectGoalProgress = $('project-goal-progress');
    if (projectGoalProgress) { clear(projectGoalProgress); rootProgress.forEach((progress) => appendProgressGoal(projectGoalProgress, progress, true)); }
    schemas.forEach((schema) => {
      const button = document.createElement('button');
      button.type = 'button';
      button.className = 'project-schema-row';
      const heading = document.createElement('strong');
      heading.textContent = schema.title || '제목 없는 스키마';
      const goal = document.createElement('span');
      const directProgress = [...asArray(schema.official.progress_goals), ...asArray(schema.ai_proposed.progress_goals)].filter((progress) => progress.own);
      const officialStatements = scopedGoalStatements(schema, payload, false, true);
      const aiStatements = scopedGoalStatements(schema, payload, true, true);
      goal.textContent = directProgress.length ? directProgress.map((progress) => text(progress.statement, '목표')).join(' / ') : (officialStatements[0] || aiStatements[0] || '현재 목표가 설정되지 않았습니다.');
      const status = document.createElement('span');
      status.textContent = `${schema.official.own ? '공식 기준' : schema.official.descendant ? '하위 공식 기준' : '공식'} ${laneSummary(schema.official)} · AI ${laneSummary(schema.ai_proposed)}`;
      button.append(heading, goal, status);
      directProgress.forEach((progress) => { const percent = document.createElement('span'); percent.className = 'schema-progress-percent'; percent.textContent = progress.percent === null ? 'AI 예상 산정 불가' : `AI 예상 달성률 약 ${Math.round(progress.percent)}%`; button.append(percent); });
      button.addEventListener('click', () => selectSchema(schema));
      rows.append(button);
    });
  }

  function renderSchemaGoals(payload) {
    const holder = $('goal-state');
    if (!holder) return;
    clear(holder);
    const model = window.IdeaDashboardModel;
    if (!payload || !state.snapshot) {
      holder.className = 'goal-state muted compact';
      holder.textContent = '목표와 기준을 불러오는 중입니다.';
      return;
    }
    const schemas = model && Array.isArray(state.exportRecords) ? model.schemaGoals(state.exportRecords, state.snapshot, payload) : [];
    renderProjectSummary(schemas, payload);
    const selectedPath = asArray(state.selected?.occurrence?.slot_path);
    const selectedRevision = state.selected?.record?.id;
    // A selected Core (or Project root) gets only its exact own-goal card; its parent Schema is not a proxy.
    const visible = state.selected?.occurrence && model?.occurrenceGoals
      ? [{ ...model.occurrenceGoals(state.exportRecords, state.snapshot, payload, selectedPath), title: titleForId(selectedRevision, selectedPath.length ? '선택한 구성 항목' : '프로젝트 목표'), slot_path: selectedPath, historical_references: [] }]
      : schemas;
    const panelTitle = $('goal-panel-title');
    if (panelTitle) panelTitle.textContent = state.selected?.occurrence
      ? `${selectedPath.length ? '프로젝트 / ' : '프로젝트 / '}${titleForId(selectedRevision, selectedPath.length ? '선택한 구성 항목' : '프로젝트')} 목표와 달성률`
      : '스키마별 목표와 달성률';
    if (!visible.length) {
      holder.className = 'goal-state muted compact';
      holder.textContent = state.snapshot.root_revision_id ? '이 구성에는 표시할 스키마별 목표가 없습니다. 현재 공식 목표가 설정되지 않았을 수 있습니다.' : '이 시점에는 구성별 목표를 계산할 수 없습니다.';
      return;
    }
    holder.className = 'goal-state';
    visible.forEach((schema) => {
      const card = document.createElement('section');
      card.className = 'schema-goal';
      const heading = document.createElement('h3');
      heading.textContent = schema.title || '제목 없는 스키마';
      card.append(heading);
      appendGoalLane(card, '공식 목표와 실제 측정', schema.official, false, scopedGoalStatements(schema, payload, false, true));
      appendGoalLane(card, 'AI 제안과 예상 기준', schema.ai_proposed, true, scopedGoalStatements(schema, payload, true, true));
      if (asArray(schema.historical_references).length) {
        const historical = document.createElement('div');
        historical.className = 'goal-history';
        historical.textContent = `이전 범위 목표 ${schema.historical_references.length}건은 현재 충족률에 합산하지 않습니다.`;
        asArray(schema.historical_references).filter((reference, index, all) => all.findIndex(other => other.root_revision_id === reference.root_revision_id) === index).forEach((reference) => {
          if (!reference.root_revision_id) return;
          const button = document.createElement('button');
          button.type = 'button';
          button.className = 'inline-button';
          button.textContent = '이전 범위 열기';
          button.addEventListener('click', () => openHistoricalRoot(reference.root_revision_id));
          historical.append(document.createTextNode(' '), button);
        });
        card.append(historical);
      }
      holder.append(card);
    });
    const stale = asArray(payload.stale_assessments);
    if (stale.length) {
      const history = document.createElement('details');
      history.className = 'goal-item stale';
      const heading = document.createElement('summary');
      heading.className = 'goal-title';
      heading.textContent = `이전 범위 평가 ${stale.length}건`;
      const note = document.createElement('span');
      note.className = 'goal-meta';
      note.textContent = '현재 공식 충족률에는 포함하지 않으며, 범위와 근거를 다시 검토해야 합니다.';
      history.append(heading, note);
      stale.forEach((assessment) => {
        const item = document.createElement('p');
        item.className = 'goal-meta';
        item.textContent = `${assessment.reason === 'root_not_in_snapshot' ? '기획 버전 변경' : '평가 범위 변경'} · ${{target_changed:'대상 내용 변경',unchanged_target:'대상은 유지됨',path_removed:'사용 위치 제거'}[assessment.applicability] || '적용 여부 재검토'}`;
        if (assessment.original_root) {
          const button = document.createElement('button');
          button.type = 'button';
          button.className = 'inline-button';
          button.textContent = '이전 범위 열기';
          button.addEventListener('click', () => openHistoricalRoot(assessment.original_root, assessment.recorded_at));
          item.append(document.createTextNode(' '), button);
        }
        history.append(item);
      });
      holder.append(history);
    }
  }

  async function loadGoals(generation = state.contextGeneration) {
    const holder = $('goal-state');
    if (generation !== state.contextGeneration) return;
    clear(holder);
    holder.className = 'goal-state muted compact';
    holder.textContent = '목표와 기준을 불러오는 중입니다.';
    try {
      const payload = await api(`${ENDPOINTS.goals}${query({ ...scope(), root_revision_id: state.snapshot?.root_revision_id || undefined, max_nodes: maxTreeNodes() })}`);
      if (generation !== state.contextGeneration) return;
      state.goalPayload = payload;
      renderSchemaGoals(payload);
    } catch (error) {
      if (generation !== state.contextGeneration) return;
      state.goalPayload = null;
      holder.className = 'goal-state muted compact';
      holder.textContent = `평가를 불러오지 못했습니다: ${errorMessage(error)}`;
    }
  }

  async function loadTimeline(generation = state.contextGeneration) {
    const holder = $('timeline');
    if (generation !== state.contextGeneration) return;
    clear(holder);
    if (!state.projectId) return;
    try {
      const requestedProject = state.projectId;
      const records = await loadProjectExport(generation);
      if (generation !== state.contextGeneration || state.projectId !== requestedProject) return;
      const recordsById = new Map(records.map((record) => [record.id, record]));
      const groups = new Map();
      records.forEach((record) => {
        if (!Number.isFinite(Number(record.seq))) return;
        const group = groups.get(record.seq) || [];
        group.push(record);
        groups.set(record.seq, group);
      });
      const steps = [...groups.entries()].sort((a, b) => Number(b[0]) - Number(a[0]));
      if (!steps.length) {
        const li = document.createElement('li');
        li.className = 'muted';
        li.textContent = '프로젝트 export에 표시할 기록 단계가 없습니다.';
        holder.append(li);
        $('timeline-summary').textContent = '프로젝트 export에 표시할 기록 단계가 없습니다.';
        return;
      }
      const headChanges = records.filter((record) => record.kind === 'head_change' && record.data?.project_id === state.projectId)
        .sort((a, b) => Number(a.seq) - Number(b.seq));
      steps.forEach(([seq, stepRecords]) => {
        const li = document.createElement('li');
        const time = document.createElement('time');
        const capture = stepRecords.find((record) => record.kind === 'capture');
        const observation = stepRecords.find((record) => record.kind === 'observation');
        const eventTime = capture?.data?.occurred_at;
        time.textContent = `기록 단계 · 서버 기록 ${formatTime(stepRecords[0]?.recorded_at)}${eventTime ? ` · 원문 발생 ${formatTime(eventTime)}` : ''}`;
        const body = document.createElement('button');
        body.type = 'button';
        body.className = 'timeline-button';
        const kinds = [...new Set(stepRecords.map((record) => kindLabel(record.kind)))].join(', ');
        const captureTitle = captureTimelineTitle(capture);
        const observationTitle = observation ? observationTimelineTitle(observation, recordsById) : null;
        const title = captureTitle || observationTitle || (capture ? `원문 · ${text(capture.data?.source_kind, '기록')}` : null);
        body.textContent = title ? `${title} · ${stepRecords.length} 기록 (${kinds})` : `${stepRecords.length} 기록 (${kinds})`;
        const earlierHeads = headChanges.filter((record) => Number(record.seq) <= Number(seq));
        const head = earlierHeads[earlierHeads.length - 1];
        const root = head?.data?.after_revision_id || head?.data?.root_revision_id || null;
        if (root) body.addEventListener('click', () => openHistoricalRoot(root, seq));
        else body.disabled = true;
        li.append(time, body);
        const details = document.createElement('div');
        details.className = 'timeline-record-links';
        const priority = { capture: 0, observation: 1, assessment: 2, goal: 3, head_change: 4, publication: 5, revision: 6, link: 7, artifact: 8 };
        const previewRecords = stepRecords.filter((record) => record.kind !== 'embedding')
          .sort((a, b) => (priority[a.kind] ?? 50) - (priority[b.kind] ?? 50) || String(a.id).localeCompare(String(b.id)));
        previewRecords.slice(0, 8).forEach((record) => {
          const detail = document.createElement('button');
          detail.type = 'button';
          detail.className = 'inline-button';
          detail.textContent = `${kindLabel(record.kind)}: ${titleForRecord(record, '기록')} · ${recordTime(record)}`;
          detail.addEventListener('click', () => openTimelineRecord(record, root));
          details.append(detail);
        });
        if (previewRecords.length > 8) details.append(document.createTextNode(` 외 ${previewRecords.length - 8}건`));
        li.append(details);
        holder.append(li);
      });
      $('timeline-summary').textContent = `프로젝트 전체 ${steps.length}단계 · 현재 선택 ${state.rootRevisionId ? '선택한 이력' : '최신 구성'} · 각 단계를 누르면 당시 구성과 서버 기록을 엽니다.`;
    } catch (error) {
      if (generation !== state.contextGeneration) return;
      const li = document.createElement('li');
      li.className = 'muted';
      li.textContent = `이력을 불러오지 못했습니다: ${errorMessage(error)}`;
      holder.append(li);
      $('timeline-summary').textContent = '프로젝트 전체 이력을 불러오지 못했습니다.';
    }
  }

  async function openHistoricalRoot(rootRevisionId, known) {
    const requestedProject = state.projectId;
    useHistoricalScope();
    state.rootRevisionId = rootRevisionId;
    state.knownSeq = Number.isFinite(Number(known)) ? Number(known) : null;
    $('known-at').value = '';
    notify('선택한 이력 범위를 열었습니다.', 'success');
    const generation = await loadProject();
    if (generation !== state.contextGeneration || state.projectId !== requestedProject) return;
    if (state.selected?.record?.id === rootRevisionId) revealSelectedDetail();
  }

  async function openTimelineRecord(record, rootRevisionId) {
    const requestedProject = state.projectId;
    useHistoricalScope();
    state.rootRevisionId = rootRevisionId || null;
    state.knownSeq = Number.isFinite(Number(record.seq)) ? Number(record.seq) : null;
    $('known-at').value = '';
    const expectedRecordGeneration = state.recordGeneration + 1;
    const generation = await loadProject({ autoSelect: false });
    if (generation !== state.contextGeneration || state.projectId !== requestedProject) return;
    if (state.recordGeneration === expectedRecordGeneration) await openLinkedRecord(record.id);
  }

  async function exportProject() {
    try {
      const headers = token() ? { Authorization: `Bearer ${token()}` } : {};
      const response = await fetch(`${ENDPOINTS.export}${query({ project_id: state.projectId || undefined, include_receipts: true })}`, { headers });
      if (!response.ok) throw new Error(`${response.status} ${await response.text()}`);
      const blob = await response.blob();
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = `idea-db-${state.projectId || 'namespace'}-export.json`;
      document.body.append(link);
      link.click();
      link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
    } catch (error) {
      notify(`내보내기 실패: ${errorMessage(error)}`, 'error');
    }
  }

  function bind() {
    const resizeHeader = () => document.documentElement.style.setProperty('--topbar-height', `${Math.ceil(document.querySelector('.topbar').getBoundingClientRect().height)}px`);
    resizeHeader();
    new ResizeObserver(resizeHeader).observe(document.querySelector('.topbar'));
    $('refresh-button').addEventListener('click', () => { state.exportProjectId = null; state.exportRecords = null; state.graph = null; loadProjects(); });
    $('latest-button').addEventListener('click', openLatest);
    $('project-select').addEventListener('change', loadProject);
    $('stream-select').addEventListener('change', () => { state.rootRevisionId = null; state.knownSeq = null; loadProject(); });
    $('known-at').addEventListener('change', () => { state.rootRevisionId = null; state.knownSeq = null; loadProject(); });
    $('effective-at').addEventListener('change', loadProject);
    $('max-nodes').addEventListener('change', loadProject);
    $('view-2d').addEventListener('click', () => setView('2d'));
    $('view-3d').addEventListener('click', () => setView('3d'));
    $('graph-reset').addEventListener('click', () => graphRenderer?.reset());
    $('graph-layer-filter').addEventListener('change', renderGraph);
    $('search-form').addEventListener('submit', search);
    $('token-button').addEventListener('click', () => { $('token-input').value = token(); dialog('token-dialog'); });
    $('token-save').addEventListener('click', () => { const value = $('token-input').value.trim(); if (value) sessionStorage.setItem('idea_db.token', value); else sessionStorage.removeItem('idea_db.token'); });
    $('token-clear').addEventListener('click', () => { sessionStorage.removeItem('idea_db.token'); $('token-input').value = ''; notify('이 탭의 접속 토큰을 지웠습니다.', 'success'); });
    $('export-button').addEventListener('click', exportProject);
    document.addEventListener('keydown', (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key === 'k') { event.preventDefault(); $('search-query').focus(); }
    });
  }

  bind();
  setView(state.view, false);
  loadProjects();
})();
