(() => {
  'use strict';

  const ENDPOINTS = {
    state: '/api/state', snapshot: '/api/snapshot', search: '/api/search', record: (id) => `/api/records/${encodeURIComponent(id)}`,
    goals: '/api/goals', export: '/api/export'
  };
  const $ = (id) => document.getElementById(id);
  const state = { projectId: '', snapshot: null, selected: null, knownSeq: null, rootRevisionId: null };
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
    button.textContent = label;
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
      if (recordId) li.append(recordButton({ id: recordId }, text(item.title || item.statement || item.body_preview || recordId)));
      else li.textContent = text(item.title || item.statement || item);
      list.append(li);
    });
  }
  function occurrenceLabel(occurrence) {
    const root = text(occurrence?.root_revision_id, 'root 미제공');
    const path = asArray(occurrence?.slot_path).join(' / ') || '(root)';
    const roles = asArray(occurrence?.roles).join(', ') || '역할 없음';
    return `root ${root} · ${path} · ${roles}`;
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
    try {
      const payload = await api(`${ENDPOINTS.record(recordId)}${query({ include: 'links,lineage,evidence,occurrences', ...recordScope() })}`);
      if (payload.record?.kind === 'entity') {
        const current = asArray(state.snapshot?.nodes).find((node) => node.entity_id === recordId);
        const fallback = asArray(payload.revisions_of_entity).sort((a, b) => b.seq - a.seq)[0];
        if (current?.revision_id || fallback?.id) return selectRecord(current?.revision_id || fallback.id);
      }
      renderRecord(payload, null);
    } catch (error) {
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
        item.textContent = text(details.title, occurrence.revision_id);
        item.addEventListener('click', () => selectRecord(
          occurrence.revision_id,
          occurrence.root_revision_id ? occurrence : { ...occurrence, root_revision_id: state.snapshot.root_revision_id },
        ));
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
    const isRevision = record.kind === 'revision';
    state.selected = { record, occurrence, entityKind: entity.entity_kind || (data.entity_id === state.projectId ? 'project' : null) };
    $('record-empty').hidden = true;
    $('record-detail').hidden = false;
    const snapshotNode = snapshotNodeMap().get(record.id);
    $('record-kind').textContent = text(record.kind, 'record').toUpperCase();
    $('record-title').textContent = text(entity.title || data.title || data.metric || data.uri || snapshotNode?.title, record.id);
    $('record-meta').textContent = `${record.id} · seq ${text(record.seq)} · ${text(record.recorded_at)}`;
    const badgeHolder = $('record-badges');
    clear(badgeHolder);
    [entity.entity_kind, data.change_kind, data.source?.claim_mode, data.source?.origin].filter(Boolean).forEach((value) => {
      const badge = document.createElement('span');
      badge.className = `badge ${value === 'official' ? 'official' : value === 'ai' || value === 'inferred' ? 'proposed' : ''}`;
      badge.textContent = String(value);
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
    appendFact(facts, 'kind', record.kind);
    appendFact(facts, '역할', occurrence?.roles?.join(', '));
    appendFact(facts, 'slot path', occurrence?.slot_path?.join(' / '));
    appendFact(facts, 'root revision', occurrence?.root_revision_id);
    appendFact(facts, 'pinned revision', occurrence?.revision_id || (isRevision ? record.id : null));
    if (record.kind === 'capture') {
      appendFact(facts, '발생 시각', data.occurred_at);
      appendFact(facts, '출처', data.source_kind);
      appendFact(facts, 'digest', data.content_digest);
    } else if (record.kind === 'observation') {
      appendFact(facts, 'metric', data.metric);
      appendFact(facts, 'value', data.value);
      appendFact(facts, 'status', data.status);
      appendFact(facts, '발생 시각', data.occurred_at);
      appendFact(facts, '환경', data.environment);
      appendFact(facts, 'artifacts', data.artifact_ids?.join(', '));
    } else if (record.kind === 'goal') {
      appendFact(facts, 'scope root', data.scope?.root_revision_id);
      appendFact(facts, 'scope path', data.scope?.slot_path?.join(' / '));
      appendFact(facts, 'criteria', data.criteria);
    } else if (record.kind === 'assessment') {
      appendFact(facts, 'baseline', data.baseline_id);
      appendFact(facts, 'status', data.status);
      appendFact(facts, 'evidence cutoff', data.evidence_cutoff_at);
      appendFact(facts, 'evidence observations', data.evidence_observation_ids?.join(', '));
    } else if (record.kind === 'artifact') {
      appendFact(facts, 'digest', data.digest);
      appendFact(facts, 'media type', data.media_type);
      appendFact(facts, 'size', data.size_bytes);
    } else if (record.kind === 'promotion') {
      appendFact(facts, 'candidate', data.candidate_id);
      appendFact(facts, 'entity', data.entity_id);
      appendFact(facts, 'revision', data.revision_id);
    } else if (record.kind === 'candidate') {
      appendFact(facts, 'status', data.status);
      appendFact(facts, 'origin', data.origin);
      appendFact(facts, 'capture', data.capture_id);
    }
    const anchor = record.kind === 'candidate' ? data.source_anchor : data.source?.source_anchor;
    if (anchor) {
      appendFact(facts, 'source anchor start (code point)', anchor.start);
      appendFact(facts, 'source anchor end (code point)', anchor.end);
    }
    renderFacts(facts);
    const source = [];
    if (data.source?.capture_id) source.push({ title: `${data.source.origin} · ${data.source.claim_mode} · ${data.source.capture_id}`, id: data.source.capture_id });
    if (record.kind === 'candidate' && data.capture_id && data.capture_id !== data.source?.capture_id) {
      source.push({ title: `candidate capture · ${data.capture_id}`, id: data.capture_id });
    }
    renderList('source-list', source, '연결된 출처가 없습니다.');
    renderOccurrences('uses-list', asArray(payload.occurrences), '선택한 기록에 사용 위치가 없습니다.');
    const lineage = payload.lineage || {};
    renderList('lineage-list', [...asArray(lineage.derived_from), ...asArray(lineage.derives), ...asArray(lineage.corrections)], '계보가 없습니다.');
    const evidence = payload.evidence || {};
    renderList('criteria-list', [...asArray(evidence.assessments), ...asArray(evidence.observations)].map((item) => ({ id: item.id, title: item.data?.status || item.id })), '평가나 관측 근거가 없습니다.');
  }

  async function selectRecord(revisionId, occurrence = null) {
    try {
      document.querySelectorAll('.tree-item[aria-current="true"]').forEach((node) => node.removeAttribute('aria-current'));
      document.querySelector(`.tree-item[data-record-id="${CSS.escape(String(revisionId))}"]`)?.setAttribute('aria-current', 'true');
      const payload = await api(`${ENDPOINTS.record(revisionId)}${query({ include: 'links,lineage,evidence,occurrences', ...recordScope() })}`);
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
        $('tree-summary').textContent = '이 DB는 비어 있습니다. export 파일은 자동으로 불러오지 않습니다. 빈 DB에서만 “export 가져오기”를 사용할 수 있습니다.';
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
      state.snapshot = await api(`${ENDPOINTS.snapshot}${query({ ...scope(), max_nodes: maxTreeNodes(), depth: 32 })}`);
      renderTree();
      const selected = state.snapshot.selected_by || {};
      const source = state.snapshot.selected_by?.publication_id ? `official publication ${state.snapshot.selected_by.publication_id}` : `working head ${state.snapshot.selected_by?.head_change_id || '없음'}`;
      const lowerLimitNotice = maxTreeNodes() < 5000 ? ` · 낮은 표시 한도 ${maxTreeNodes()}개 선택됨` : '';
      $('tree-summary').textContent = state.snapshot.root_revision_id ? `${source} · revision ${state.snapshot.root_revision_id} · server known seq ${selected.known_seq}${lowerLimitNotice}${state.snapshot.truncated ? ` · ${maxTreeNodes()}개에서 잘림` : ''}` : '선택한 서버 기록 시점에는 head가 없습니다.';
      await Promise.all([loadGoals(), loadTimeline()]);
      if (state.snapshot.root_revision_id) {
        await selectRecord(state.snapshot.root_revision_id, { root_revision_id: state.snapshot.root_revision_id, revision_id: state.snapshot.root_revision_id, slot_path: [], roles: [] });
      }
    } catch (error) {
      state.snapshot = null;
      renderTree();
      $('tree-summary').textContent = `구성을 불러오지 못했습니다: ${errorMessage(error)}`;
      notify(errorMessage(error), 'error');
    }
  }

  function appendSearchResult(holder, entry, lane, selectedRoot = null) {
    const card = document.createElement('article');
    card.className = `result ${lane}`;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'result-record';
    const title = document.createElement('span');
    title.className = 'result-title';
    title.textContent = text(entry.title, entry.revision_id || entry.candidate_id);
    const meta = document.createElement('span');
    meta.className = 'result-meta';
    meta.textContent = lane === 'related' ? `관련 문맥 · ${text(entry.reason)}` : lane === 'pending' ? `후보 · ${text(entry.origin)}` : '직접 일치';
    button.append(title, meta);
    const id = entry.revision_id || entry.record_id || entry.candidate_id;
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

  async function loadGoals() {
    const holder = $('goal-state');
    clear(holder);
    if (!state.snapshot?.root_revision_id) {
      holder.className = 'goal-state muted compact';
      holder.textContent = 'head가 생기면 목표와 기준을 표시합니다.';
      return;
    }
    try {
      const payload = await api(`${ENDPOINTS.goals}${query({ ...scope(), root_revision_id: state.snapshot.root_revision_id, max_nodes: maxTreeNodes() })}`);
      const goals = asArray(payload.goals);
      const stale = asArray(payload.stale_assessments);
      if (!goals.length && !stale.length) {
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
        meta.textContent = `${goal.scope?.in_current_snapshot ? '현재 범위' : '이전 범위'} · 공식 gate: ${goal.gate_status} · 필수 기준 ${asArray(goal.required_criteria_status).map((criterion) => `${criterion.criterion_id}: ${criterion.status}`).join(', ') || '없음'}`;
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
      if (stale.length) {
        const heading = document.createElement('p');
        heading.className = 'muted compact';
        heading.textContent = `이전 범위 평가 ${stale.length}건: 현재 gate로 자동 승계하지 않습니다.`;
        holder.append(heading);
        stale.forEach((assessment) => {
          const item = document.createElement('div');
          item.className = 'goal-item stale';
          const reason = text(assessment.reason, 'scope changed');
          const applicability = text(assessment.applicability, 'requires review');
          item.textContent = `${assessment.assessment_id} · ${reason} · ${applicability}`;
          if (assessment.original_root) {
            const oldRoot = document.createElement('button');
            oldRoot.type = 'button';
            oldRoot.className = 'inline-button';
            oldRoot.textContent = '이전 root 열기';
            oldRoot.addEventListener('click', () => openHistoricalRoot(assessment.original_root, assessment.recorded_at));
            item.append(document.createTextNode(' '), oldRoot);
          }

          holder.append(item);
        });
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
        time.textContent = `서버 기록 ${record.recorded_at}`;
        const body = document.createElement('button');
        body.type = 'button';
        body.className = 'timeline-button';
        const data = record.data || {};
        body.textContent = record.kind === 'publication' ? `공식 반영 · ${text(data.label, data.root_revision_id)}` : `${text(data.before_revision_id)} → ${text(data.after_revision_id)} · ${text(data.reason)}`;
        const root = data.root_revision_id || data.after_revision_id;
        if (root) body.addEventListener('click', () => openHistoricalRoot(root, record.seq));
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

  async function openHistoricalRoot(rootRevisionId, known) {
    state.rootRevisionId = rootRevisionId;
    state.knownSeq = Number.isFinite(Number(known)) ? Number(known) : null;
    $('known-at').value = '';
    notify(`revision ${rootRevisionId} · server known seq ${state.knownSeq ?? '현재'} 범위를 열었습니다.`, 'success');
    await loadProject();
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
    $('refresh-button').addEventListener('click', loadProjects);
    $('project-select').addEventListener('change', loadProject);
    $('stream-select').addEventListener('change', () => { state.rootRevisionId = null; state.knownSeq = null; loadProject(); });
    $('known-at').addEventListener('change', () => { state.rootRevisionId = null; state.knownSeq = null; loadProject(); });
    $('effective-at').addEventListener('change', loadProject);
    $('max-nodes').addEventListener('change', loadProject);
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
  loadProjects();
})();
