(() => {
  'use strict';

  const ENDPOINTS = {
    state: '/api/state', snapshot: '/api/snapshot', record: (id) => `/api/records/${encodeURIComponent(id)}`,
    captures: '/api/captures', packageValidate: '/api/packages/validate', packageApply: '/api/packages/apply',
    occurrenceReplace: '/api/occurrences/replace', goals: '/api/goals', export: '/api/export'
  };
  const $ = (id) => document.getElementById(id);
  const state = { projectId: '', snapshot: null, selected: null, validatedPackageText: null };
  const maxTreeNodes = 500;
  const asArray = (value) => Array.isArray(value) ? value : [];
  const text = (value, fallback = '—') => value === null || value === undefined || value === '' ? fallback : String(value);
  const iso = (value) => { if (!value) return null; const date = new Date(value); return Number.isNaN(date.valueOf()) ? null : date.toISOString(); };
  const scope = () => ({ project_id: state.projectId || undefined, stage: $('stream-select').value, known_at: iso($('known-at').value), effective_at: iso($('effective-at').value) });
  const query = (params) => { const q = new URLSearchParams(); Object.entries(params).forEach(([key, value]) => { if (value !== undefined && value !== null && value !== '') q.set(key, String(value)); }); return q.size ? `?${q}` : ''; };
  const token = () => sessionStorage.getItem('idea_db.token') || '';
  const makeId = (prefix) => `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2, 10)}`;
  const clear = (node) => node.replaceChildren();
  const dialog = (id) => $(id).showModal();
  const errorMessage = (error) => error instanceof Error ? error.message : String(error);

  function notify(message, type = '') {
    const node = document.createElement('div');
    node.className = `notice ${type}`;
    node.textContent = message;
    $('status-region').append(node);
    setTimeout(() => node.remove(), 6500);
  }

  async function api(url, options = {}) {
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
    button.textContent = label;
    button.addEventListener('click', () => selectRecord(entry.id || entry.revision_id));
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
      if (recordId) li.append(recordButton({ id: recordId }, text(item.title || item.statement || item.body_preview || recordId)));
      else li.textContent = text(item.title || item.statement || item);
      list.append(li);
    });
  }
  function snapshotNodeMap() {
    return new Map(asArray(state.snapshot?.nodes).map((node) => [node.revision_id, node]));
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
        if (rendered >= maxTreeNodes) return;
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
        item.textContent = text(details.title, occurrence.revision_id);
        item.addEventListener('click', () => selectRecord(occurrence.revision_id, occurrence));
        row.append(item);
        const kind = document.createElement('span');
        kind.className = 'tree-kind';
        kind.textContent = text(details.entity_kind, '');
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
    if (state.snapshot.truncated || rendered >= maxTreeNodes) {
      const p = document.createElement('p');
      p.className = 'tree-empty';
      p.textContent = `표시는 ${maxTreeNodes}개 사용 위치로 제한되었습니다. 검색으로 범위를 좁히세요.`;
      container.append(p);
    }
  }

  function renderRecord(payload, selectedOccurrence) {
    const record = payload.record;
    const data = record.data || {};
    const entity = payload.entity?.data || {};
    const occurrence = selectedOccurrence || payload.occurrences?.[0] || null;
    state.selected = { record, occurrence, entityKind: entity.entity_kind || (data.entity_id === state.projectId ? 'project' : null) };
    $('record-empty').hidden = true;
    $('record-detail').hidden = false;
    const childButton = $('child-button');
    const canAddChild = state.selected.entityKind === 'project' || state.selected.entityKind === 'schema' || state.selected.entityKind === 'core';
    childButton.hidden = !canAddChild;
    childButton.disabled = !canAddChild;
    const snapshotNode = snapshotNodeMap().get(record.id);
    $('record-kind').textContent = text(record.kind, 'record').toUpperCase();
    $('record-title').textContent = text(entity.title || snapshotNode?.title, record.id);
    $('record-meta').textContent = `${record.id} · seq ${text(record.seq)} · ${text(record.recorded_at)}`;
    const badgeHolder = $('record-badges');
    clear(badgeHolder);
    [entity.entity_kind, data.change_kind, data.source?.claim_mode, data.source?.origin].filter(Boolean).forEach((value) => {
      const badge = document.createElement('span');
      badge.className = `badge ${value === 'official' ? 'official' : value === 'ai' || value === 'inferred' ? 'proposed' : ''}`;
      badge.textContent = String(value);
      badgeHolder.append(badge);
    });
    $('record-content').textContent = text(data.body, '이 기록에는 본문이 없습니다.');
    const facts = $('occurrence-details');
    clear(facts);
    [
      ['역할', occurrence?.roles?.join(', ')],
      ['slot path', occurrence?.slot_path?.join(' / ')],
      ['root revision', occurrence?.root_revision_id || state.snapshot?.root_revision_id],
      ['pinned revision', occurrence?.revision_id || record.id]
    ].forEach(([name, value]) => {
      const dt = document.createElement('dt'); dt.textContent = name;
      const dd = document.createElement('dd'); dd.textContent = text(value);
      facts.append(dt, dd);
    });
    const source = data.source ? [{ title: `${data.source.origin} · ${data.source.claim_mode}${data.source.capture_id ? ` · ${data.source.capture_id}` : ''}`, id: data.source.capture_id }] : [];
    renderList('source-list', source, '연결된 출처가 없습니다.');
    renderList('uses-list', asArray(payload.occurrences).map((item) => ({ id: item.revision_id, title: `${item.slot_path.join(' / ') || '(root)'} · ${(item.roles || []).join(', ') || '역할 없음'}` })), '현재 스냅샷의 사용 위치가 없습니다.');
    const lineage = payload.lineage || {};
    renderList('lineage-list', [...asArray(lineage.derived_from), ...asArray(lineage.derives), ...asArray(lineage.corrections)], '계보가 없습니다.');
    const evidence = payload.evidence || {};
    renderList('criteria-list', [...asArray(evidence.assessments), ...asArray(evidence.observations)].map((item) => ({ id: item.id, title: item.data?.status || item.id })), '평가나 관측 근거가 없습니다.');
  }

  async function selectRecord(revisionId, occurrence = null) {
    try {
      document.querySelectorAll('.tree-item[aria-current="true"]').forEach((node) => node.removeAttribute('aria-current'));
      document.querySelector(`.tree-item[data-record-id="${CSS.escape(String(revisionId))}"]`)?.setAttribute('aria-current', 'true');
      const payload = await api(`${ENDPOINTS.record(revisionId)}${query({ include: 'links,lineage,evidence,occurrences', stage: $('stream-select').value, known_at: scope().known_at, effective_at: scope().effective_at })}`);
      renderRecord(payload, occurrence);
    } catch (error) {
      notify(`기록을 불러오지 못했습니다: ${errorMessage(error)}`, 'error');
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
        return;
      }
      projects.forEach((project) => select.add(new Option(project.title, project.project_id)));
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

  async function loadProject() {
    state.projectId = $('project-select').value;
    state.selected = null;
    $('record-detail').hidden = true;
    $('record-empty').hidden = false;
    if (!state.projectId) {
      state.snapshot = null;
      renderTree();
      return;
    }
    try {
      state.snapshot = await api(`${ENDPOINTS.snapshot}${query({ ...scope(), max_nodes: maxTreeNodes, depth: 32 })}`);
      renderTree();
      const selected = state.snapshot.selected_by || {};
      $('tree-summary').textContent = state.snapshot.root_revision_id ? `revision ${state.snapshot.root_revision_id} · known seq ${selected.known_seq}` : '아직 작업 head가 없습니다.';
      await Promise.all([loadGoals(), loadTimeline()]);
    } catch (error) {
      state.snapshot = null;
      renderTree();
      $('tree-summary').textContent = `구성을 불러오지 못했습니다: ${errorMessage(error)}`;
      notify(errorMessage(error), 'error');
    }
  }

  function appendSearchResult(holder, entry, lane) {
    const button = document.createElement('button');
    button.type = 'button';
    button.className = `result ${lane}`;
    const title = document.createElement('span');
    title.className = 'result-title';
    title.textContent = text(entry.title, entry.revision_id || entry.candidate_id);
    const meta = document.createElement('span');
    meta.className = 'result-meta';
    meta.textContent = lane === 'related' ? `관련 문맥 · ${text(entry.reason)}` : lane === 'pending' ? `후보 · ${text(entry.origin)}` : `직접 역할 일치 · ${(entry.occurrences?.flatMap((item) => item.roles || []).join(', ') || '역할 없음')}`;
    button.append(title, meta);
    const id = entry.revision_id || entry.candidate_id;
    if (id) button.addEventListener('click', () => selectRecord(id, entry.occurrences?.[0]));
    holder.append(button);
  }

  async function search(event) {
    event?.preventDefault();
    const queryText = $('search-query').value.trim();
    const roles = $('role-query').value.split(',').map((value) => value.trim()).filter(Boolean);
    if (!queryText && !roles.length) {
      clear($('search-results'));
      $('search-state').textContent = '검색어 또는 역할을 입력하세요.';
      return;
    }
    $('search-state').textContent = '검색 중…';
    try {
      const payload = await api(ENDPOINTS.search, { method: 'POST', body: JSON.stringify({ ...scope(), query: queryText, roles, tags: [], lanes: ['official', 'candidate'], limit: 50, vector: null }) });
      const holder = $('search-results');
      clear(holder);
      const related = $('related-context').checked ? asArray(payload.related) : [];
      const candidates = asArray(payload.candidates);
      asArray(payload.results).forEach((entry) => appendSearchResult(holder, entry, 'direct'));
      related.forEach((entry) => appendSearchResult(holder, entry, 'related'));
      candidates.forEach((entry) => appendSearchResult(holder, entry, 'pending'));
      const vector = payload.vector_status || {};
      $('search-state').textContent = `직접 ${asArray(payload.results).length} · 관련 ${related.length} · 후보 ${candidates.length} · ${vector.used ? '벡터 사용' : '어휘 검색'}`;
    } catch (error) {
      $('search-state').textContent = `검색 실패: ${errorMessage(error)}`;
      notify(errorMessage(error), 'error');
    }
  }

  async function loadGoals() {
    const holder = $('goal-state');
    clear(holder);
    if (!state.snapshot?.root_revision_id) {
      holder.className = 'goal-state muted compact';
      holder.textContent = 'head가 생기면 목표와 기준을 표시합니다.';
      return;
    }
    try {
      const payload = await api(`${ENDPOINTS.goals}${query({ ...scope(), root_revision_id: state.snapshot.root_revision_id, max_nodes: maxTreeNodes })}`);
      const goals = asArray(payload.goals);
      if (!goals.length) {
        holder.className = 'goal-state muted compact';
        holder.textContent = `목표가 없습니다. 범위 내 목표 누락 ${asArray(payload.missing_goal_occurrences).length}건`;
        return;
      }
      holder.className = 'goal-state';
      goals.forEach((goal) => {
        const item = document.createElement('div');
        item.className = 'goal-item';
        const title = document.createElement('span');
        title.className = 'goal-title';
        title.textContent = goal.statement;
        const meta = document.createElement('span');
        meta.className = 'goal-meta';
        meta.textContent = `공식 gate: ${goal.gate_status} · 필수 기준 ${asArray(goal.required_criteria_status).map((criterion) => `${criterion.criterion_id}: ${criterion.status}`).join(', ') || '없음'}`;
        item.append(title, meta);
        holder.append(item);
      });
      const proposals = asArray(payload.proposed_goals);
      if (proposals.length) {
        const proposal = document.createElement('div');
        proposal.className = 'goal-item';
        proposal.textContent = `AI 제안 목표 ${proposals.length}건 (공식 gate와 별도)`;
        holder.append(proposal);
      }
    } catch (error) {
      holder.className = 'goal-state muted compact';
      holder.textContent = `평가를 불러오지 못했습니다: ${errorMessage(error)}`;
    }
  }

  async function loadTimeline() {
    const holder = $('timeline');
    clear(holder);
    if (!state.projectId) return;
    try {
      const payload = await api(`${ENDPOINTS.state}${query({ project_id: state.projectId, kinds: 'head_change,publication', max_seq: state.snapshot?.selected_by?.known_seq, limit: 100 })}`);
      const records = asArray(payload.records).sort((a, b) => b.seq - a.seq);
      if (!records.length) {
        const li = document.createElement('li');
        li.className = 'muted';
        li.textContent = '표시할 방향 변경이나 공식 기록이 없습니다.';
        holder.append(li);
        return;
      }
      records.forEach((record) => {
        const li = document.createElement('li');
        const time = document.createElement('time');
        time.textContent = record.recorded_at;
        const body = document.createElement('span');
        const data = record.data || {};
        body.textContent = record.kind === 'publication' ? `공식 반영 · ${text(data.label, data.root_revision_id)}` : `${text(data.before_revision_id)} → ${text(data.after_revision_id)} · ${text(data.reason)}`;
        li.append(time, body);
        holder.append(li);
      });
    } catch (error) {
      const li = document.createElement('li');
      li.className = 'muted';
      li.textContent = `이력을 불러오지 못했습니다: ${errorMessage(error)}`;
      holder.append(li);
    }
  }

  function showPackageResult(value, isError = false) {
    const node = $('package-result');
    node.hidden = false;
    node.classList.toggle('error', isError);
    node.textContent = typeof value === 'string' ? value : JSON.stringify(value, null, 2);
  }
  async function validatePackage() {
    const raw = $('package-json').value;
    try { JSON.parse(raw); } catch (error) { showPackageResult(`JSON 형식 오류: ${errorMessage(error)}`, true); return false; }
    try {
      const result = await api(ENDPOINTS.packageValidate, { method: 'POST', body: raw });
      state.validatedPackageText = result.valid ? raw : null;
      showPackageResult(result, !result.valid);
      $('package-apply').disabled = !result.valid;
      return result.valid;
    } catch (error) {
      state.validatedPackageText = null;
      $('package-apply').disabled = true;
      showPackageResult(`검증 요청 실패: ${errorMessage(error)}`, true);
      return false;
    }
  }

  async function submitProject(event) {
    event.preventDefault();
    const form = new FormData(event.currentTarget);
    const id = String(form.get('id'));
    const rootRevisionId = `rev_${id}`;
    const request = {
      protocol_version: 1,
      idempotency_key: makeId('pkg'),
      actor: form.get('actor'),
      reason: '프로젝트 최초 등록',
      expected_heads: [{ project_id: id, stage: 'working', revision_id: null }],
      records: [
        { id, kind: 'project', data: { title: form.get('title'), description: form.get('description') || '' } },
        { id: rootRevisionId, kind: 'revision', data: { entity_id: id, body: form.get('title'), tags: [], slots: [], change_kind: 'initial', correction_of: null, correction_reason: null, previous_revision_id: null, source: { origin: 'human', capture_id: null, model: null, skill: null, claim_mode: 'inferred', source_anchor: null } } }
      ],
      root_change: { project_id: id, stage: 'working', after_revision_id: rootRevisionId, reason: '프로젝트 최초 구성', meaningful: true, decision: { before: '루트 없음', after: form.get('title'), rationale: '프로젝트 작업 공간을 시작한다' } },
      publish: null
    };
    try {
      const validation = await api(ENDPOINTS.packageValidate, { method: 'POST', body: JSON.stringify(request) });
      if (!validation.valid) {
        $('project-dialog').close();
        $('package-json').value = JSON.stringify(request, null, 2);
        showPackageResult(validation, true);
        dialog('package-dialog');
        return;
      }
      await api(ENDPOINTS.packageApply, { method: 'POST', body: JSON.stringify(request) });
      $('project-dialog').close();
      notify('프로젝트를 만들었습니다.', 'success');
      state.projectId = id;
      await loadProjects();
    } catch (error) {
      notify(`프로젝트 생성 실패: ${errorMessage(error)}`, 'error');
    }
  }

  async function submitCapture(event) {
    event.preventDefault();
    if (!state.projectId) { notify('먼저 프로젝트를 만드세요.', 'error'); return; }
    const formElement = event.currentTarget;
    const form = new FormData(formElement);
    const request = { project_id: state.projectId, content: form.get('content'), media_type: 'text/plain', source_kind: form.get('source_kind'), source_ref: form.get('source_ref') || null, occurred_at: iso(form.get('occurred_at')), content_digest: null };
    try {
      const result = await api(ENDPOINTS.captures, { method: 'POST', body: JSON.stringify(request) });
      $('capture-dialog').close();
      formElement.reset();
      notify(result.replay ? `기존 캡처 결과를 다시 받았습니다: ${result.capture.id}` : `원문을 저장했습니다: ${result.capture.id}`, 'success');
    } catch (error) {
      notify(`원문 저장 실패: ${errorMessage(error)}`, 'error');
    }
  }

  async function submitPackage(event) {
    event.preventDefault();
    const raw = $('package-json').value;
    if (!state.validatedPackageText || state.validatedPackageText !== raw) { await validatePackage(); return; }
    try {
      const result = await api(ENDPOINTS.packageApply, { method: 'POST', body: raw });
      $('package-dialog').close();
      notify(result.receipt?.replay ? '기존 적용 결과를 다시 받았습니다.' : '검증된 패키지를 적용했습니다.', 'success');
      await loadProjects();
    } catch (error) {
      showPackageResult(`적용 실패: ${errorMessage(error)}`, true);
    }
  }

  function decisionFrom(form) {
    return { before: String(form.get('decision_before')).trim(), after: String(form.get('decision_after')).trim(), rationale: String(form.get('decision_rationale')).trim() };
  }
  async function submitReplace(event) {
    event.preventDefault();
    const occurrence = state.selected?.occurrence;
    if (!occurrence || !state.snapshot?.root_revision_id) { notify('트리에서 특정 사용 위치를 선택하세요.', 'error'); return; }
    if ($('stream-select').value !== 'working') { notify('선택 위치 교체는 작업 head에서만 할 수 있습니다.', 'error'); return; }
    const form = new FormData(event.currentTarget);
    const request = {
      protocol_version: 1, idempotency_key: form.get('idempotency_key'), actor: form.get('actor'), reason: form.get('reason'),
      project_id: state.projectId, stage: 'working', expected_root_revision_id: state.snapshot.root_revision_id, slot_path: occurrence.slot_path,
      replacement: { mode: 'existing_revision', new_revision: null, new_records: [], target_revision_id: form.get('target_revision_id') },
      root_change_reason: form.get('root_change_reason'), decision: decisionFrom(form)
    };
    try {
      const result = await api(ENDPOINTS.occurrenceReplace, { method: 'POST', body: JSON.stringify(request) });
      $('replace-dialog').close();
      notify(`선택 위치를 교체했습니다. 새 root: ${result.new_root_revision_id}`, 'success');
      await loadProject();
    } catch (error) {
      notify(`교체 실패: ${errorMessage(error)}`, 'error');
    }
  }

  function allowedChildKinds() {
    const parent = state.selected?.entityKind;
    if (parent === 'project') return ['schema'];
    if (parent === 'schema' || parent === 'core') return ['core', 'idea'];
    return [];
  }
  function cloneSource(source) {
    return source || { origin: 'human', capture_id: null, model: null, skill: null, claim_mode: 'inferred', source_anchor: null };
  }
  function childSlots(data, form, childRevisionId) {
    return [...asArray(data.slots), { slot_id: form.get('slot_id'), revision_id: childRevisionId, roles: String(form.get('roles')).split(',').map((value) => value.trim()).filter(Boolean) }];
  }
  function childRecords(form, parentRecord) {
    const entityId = makeId('ent');
    const childRevisionId = makeId('rev');
    const parentRevisionId = makeId('rev');
    const extracted = form.get('claim_mode') === 'extracted';
    const captureId = String(form.get('capture_id') || '').trim();
    const start = Number(form.get('anchor_start'));
    const end = Number(form.get('anchor_end'));
    if (extracted && (!captureId || !Number.isInteger(start) || !Number.isInteger(end) || start < 0 || end <= start)) throw new Error('원문 인용에는 캡처 ID와 올바른 시작·끝 위치가 필요합니다.');
    const source = extracted ? { origin: 'human', capture_id: captureId, model: null, skill: null, claim_mode: 'extracted', source_anchor: { capture_id: captureId, start, end } } : { origin: 'human', capture_id: null, model: null, skill: null, claim_mode: 'inferred', source_anchor: null };
    const data = parentRecord.data || {};
    return {
      entityId, childRevisionId, parentRevisionId,
      entity: { id: entityId, kind: 'entity', data: { entity_kind: form.get('entity_kind'), project_id: state.projectId, title: form.get('title'), tags: [], derived_from: [], lineage_kind: 'none' } },
      child: { id: childRevisionId, kind: 'revision', data: { entity_id: entityId, body: form.get('body'), tags: [], slots: [], change_kind: 'initial', correction_of: null, correction_reason: null, previous_revision_id: null, source } },
      parent: { id: parentRevisionId, data: { entity_id: data.entity_id, body: data.body, tags: asArray(data.tags), slots: childSlots(data, form, childRevisionId), change_kind: 'composition', correction_of: null, correction_reason: null, previous_revision_id: parentRecord.id, source: cloneSource(data.source) } }
    };
  }
  async function submitChild(event) {
    event.preventDefault();
    const selected = state.selected;
    const occurrence = selected?.occurrence;
    if (!selected?.record || !occurrence || !state.snapshot?.root_revision_id) { notify('트리에서 구성 위치를 선택하세요.', 'error'); return; }
    const formElement = event.currentTarget;
    const form = new FormData(formElement);
    const decision = decisionFrom(form);
    const reason = form.get('reason');
    const idempotencyKey = makeId('child');
    try {
      const records = childRecords(form, selected.record);
      if (occurrence.slot_path.length === 0) {
        const request = {
          protocol_version: 1, idempotency_key: idempotencyKey, actor: form.get('actor'), reason,
          expected_heads: [{ project_id: state.projectId, stage: 'working', revision_id: state.snapshot.root_revision_id }],
          records: [records.entity, records.child, { id: records.parentRevisionId, kind: 'revision', data: records.parent.data }],
          root_change: { project_id: state.projectId, stage: 'working', after_revision_id: records.parentRevisionId, reason, meaningful: true, decision }, publish: null
        };
        const validation = await api(ENDPOINTS.packageValidate, { method: 'POST', body: JSON.stringify(request) });
        if (!validation.valid) { $('child-dialog').close(); $('package-json').value = JSON.stringify(request, null, 2); showPackageResult(validation, true); dialog('package-dialog'); return; }
        await api(ENDPOINTS.packageApply, { method: 'POST', body: JSON.stringify(request) });
      } else {
        const request = {
          protocol_version: 1, idempotency_key: idempotencyKey, actor: form.get('actor'), reason, project_id: state.projectId, stage: 'working', expected_root_revision_id: state.snapshot.root_revision_id, slot_path: occurrence.slot_path,
          replacement: { mode: 'new_revision', new_revision: records.parent, new_records: [records.entity, records.child], target_revision_id: null }, root_change_reason: reason, decision
        };
        await api(ENDPOINTS.occurrenceReplace, { method: 'POST', body: JSON.stringify(request) });
      }
      $('child-dialog').close();
      formElement.reset();
      notify('하위 항목을 추가했습니다.', 'success');
      await loadProject();
    } catch (error) {
      notify(`하위 항목 추가 실패: ${errorMessage(error)}`, 'error');
    }
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

  function openGoalPackage() {
    dialog('package-dialog');
    showPackageResult('목표와 기준은 버전형 패키지에 goal / baseline / assessment 레코드로 넣어 검증 후 적용합니다. AI 제안(origin: ai_proposed)과 공식(origin: official)은 서버가 분리해 표시합니다.');
    $('package-json').focus();
  }
  function bind() {
    $('refresh-button').addEventListener('click', loadProjects);
    $('project-select').addEventListener('change', loadProject);
    ['stream-select', 'known-at', 'effective-at'].forEach((id) => $(id).addEventListener('change', loadProject));
    $('search-form').addEventListener('submit', search);
    $('token-button').addEventListener('click', () => { $('token-input').value = token(); dialog('token-dialog'); });
    $('token-save').addEventListener('click', () => { const value = $('token-input').value.trim(); if (value) sessionStorage.setItem('idea_db.token', value); else sessionStorage.removeItem('idea_db.token'); });
    $('token-clear').addEventListener('click', () => { sessionStorage.removeItem('idea_db.token'); $('token-input').value = ''; notify('이 탭의 접속 토큰을 지웠습니다.', 'success'); });
    ['new-project-button', 'empty-project-button'].forEach((id) => $(id).addEventListener('click', () => dialog('project-dialog')));
    ['capture-button', 'empty-capture-button'].forEach((id) => $(id).addEventListener('click', () => dialog('capture-dialog')));
    $('export-button').addEventListener('click', exportProject);
    $('goal-button').addEventListener('click', openGoalPackage);
    $('child-button').addEventListener('click', () => {
      const kinds = allowedChildKinds();
      if (!kinds.length) { notify('이 항목 아래에는 하위 구성을 추가할 수 없습니다.', 'error'); return; }
      const selector = $('child-kind');
      clear(selector);
      kinds.forEach((kind) => selector.add(new Option(kind, kind)));
      const occurrence = state.selected?.occurrence;
      $('child-context').textContent = `${state.selected.entityKind} · ${occurrence?.slot_path?.join(' / ') || '(project root)'} 아래에 추가합니다.`;
      $('child-dialog').showModal();
    });
    $('replace-button').addEventListener('click', () => {
      const occurrence = state.selected?.occurrence;
      $('replace-context').textContent = occurrence ? `root: ${state.snapshot?.root_revision_id} · path: ${occurrence.slot_path.join(' / ') || '(root)'}` : '트리에서 특정 사용 위치를 선택하면 그 경로만 교체합니다.';
      dialog('replace-dialog');
    });
    $('package-validate').addEventListener('click', validatePackage);
    $('project-form').addEventListener('submit', submitProject);
    $('capture-form').addEventListener('submit', submitCapture);
    $('package-form').addEventListener('submit', submitPackage);
    $('replace-form').addEventListener('submit', submitReplace);
    $('child-form').addEventListener('submit', submitChild);
    document.querySelectorAll('[data-close-dialog]').forEach((button) => button.addEventListener('click', () => button.closest('dialog').close()));
    document.addEventListener('keydown', (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key === 'k') { event.preventDefault(); $('search-query').focus(); }
      if ((event.metaKey || event.ctrlKey) && event.key === 'Enter') { event.preventDefault(); dialog('package-dialog'); }
    });
  }
  bind();
  loadProjects();
})();
