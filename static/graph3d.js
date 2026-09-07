(() => {
  'use strict';

  const colors = {
    project: '#26231f', schema: '#315f5c', core: '#73589b', idea: '#97611c',
    revision: '#315f5c', default: '#6f685d'
  };
  const clamp = (value, min, max) => Math.min(max, Math.max(min, value));

  function makeRenderer(canvas, options = {}) {
    const context = canvas.getContext('2d');
    const view = { yaw: -0.68, pitch: 0.38, zoom: 1, graph: null, selected: null, projected: [] };
    let dragging = null;
    let observer = null;

    function size() {
      const rect = canvas.getBoundingClientRect();
      const ratio = window.devicePixelRatio || 1;
      const width = Math.max(1, Math.round(rect.width * ratio));
      const height = Math.max(1, Math.round(rect.height * ratio));
      if (canvas.width !== width || canvas.height !== height) {
        canvas.width = width;
        canvas.height = height;
      }
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
      return { width: rect.width, height: rect.height };
    }

    function bounds(graph) {
      const source = graph?.bounds || {};
      const range = (name) => {
        const value = source[name];
        if (Array.isArray(value) && value.length === 2 && Number.isFinite(Number(value[0])) && Number.isFinite(Number(value[1]))) return [Number(value[0]), Number(value[1])];
        const values = (graph?.nodes || []).map((node) => Number(node[name])).filter(Number.isFinite);
        return values.length ? [Math.min(...values), Math.max(...values)] : [0, 1];
      };
      return { x: range('x'), y: range('y'), z: range('z') };
    }

    function unit(value, pair) {
      const span = pair[1] - pair[0];
      return span ? ((Number(value) - pair[0]) / span - 0.5) * 2 : 0;
    }

    function project(point, width, height) {
      const cosY = Math.cos(view.yaw);
      const sinY = Math.sin(view.yaw);
      const cosX = Math.cos(view.pitch);
      const sinX = Math.sin(view.pitch);
      const x1 = point.x * cosY - point.z * sinY;
      const z1 = point.x * sinY + point.z * cosY;
      const y1 = point.y * cosX - z1 * sinX;
      const z2 = point.y * sinX + z1 * cosX;
      const perspective = 2.9 / (2.9 - z2);
      // The normalized axis endpoints are ±1.16.  This default keeps both
      // endpoints and all actual nodes inside the canvas with a margin after
      // perspective projection; wheel zoom can intentionally move closer.
      const scale = Math.min(width, height) * 0.18 * view.zoom;
      return { x: width / 2 + x1 * scale * perspective, y: height / 2 - y1 * scale * perspective, z: z2, scale: perspective };
    }

    function sourcePoint(node, graphBounds) {
      return { x: unit(node.x, graphBounds.x), y: -unit(node.y, graphBounds.y), z: unit(node.z, graphBounds.z) };
    }

    function label(value, x, y, color = '#716b61') {
      context.fillStyle = color;
      context.font = '12px ui-rounded, system-ui, sans-serif';
      context.fillText(value, x, y);
    }

    function drawAxes(width, height) {
      const origin = project({ x: -1.16, y: 1.16, z: -1.16 }, width, height);
      const axes = [
        [{ x: 1.15, y: 1.16, z: -1.16 }, '#9e3d32', 'X 기록 시각'],
        [{ x: -1.16, y: -1.15, z: -1.16 }, '#315f5c', 'Y 깊이 (아래로 증가)'],
        [{ x: -1.16, y: 1.16, z: 1.15 }, '#73589b', 'Z 스키마 층']
      ];
      axes.forEach(([point, color, title]) => {
        const end = project(point, width, height);
        context.strokeStyle = color;
        context.lineWidth = 1.5;
        context.beginPath();
        context.moveTo(origin.x, origin.y);
        context.lineTo(end.x, end.y);
        context.stroke();
        label(title, end.x + 5, end.y - 4, color);
      });
    }

    function collisions(nodes, graphBounds) {
      const groups = new Map();
      nodes.forEach((node) => {
        const point = sourcePoint(node, graphBounds);
        const key = `${node.x}|${node.y}|${node.z}`;
        const group = groups.get(key) || [];
        group.push({ node, point });
        groups.set(key, group);
      });
      return [...groups.values()];
    }

    function render() {
      const { width, height } = size();
      context.clearRect(0, 0, width, height);
      context.fillStyle = '#fcfaf4';
      context.fillRect(0, 0, width, height);
      drawAxes(width, height);
      const graph = view.graph;
      if (!graph || !Array.isArray(graph.nodes) || !graph.nodes.length) {
        context.fillStyle = '#716b61';
        context.font = '15px ui-rounded, system-ui, sans-serif';
        context.fillText('표시할 실제 이력 노드가 없습니다.', 20, 32);
        view.projected = [];
        return;
      }
      const graphBounds = bounds(graph);
      const nodeById = new Map(graph.nodes.map((node) => [node.id, node]));
      context.lineWidth = 1;
      const edgeStyle = {
        composition: { color: 'rgba(104, 95, 81, .40)', dash: [] },
        version: { color: '#315f5c', dash: [5, 3] },
        derived: { color: '#73589b', dash: [2, 3] },
        similar: { color: '#97611c', dash: [6, 2] },
        contradict: { color: '#9e3d32', dash: [7, 2, 2, 2] }
      };
      const usedEdgeTypes = new Set();
      (graph.edges || []).forEach((edge) => {
        const source = nodeById.get(edge.source);
        const target = nodeById.get(edge.target);
        if (!source || !target) return;
        const a = project(sourcePoint(source, graphBounds), width, height);
        const b = project(sourcePoint(target, graphBounds), width, height);
        const style = edgeStyle[edge.type] || edgeStyle.composition;
        usedEdgeTypes.add(edge.type || 'composition');
        context.strokeStyle = style.color;
        context.setLineDash(style.dash);
        context.beginPath();
        context.moveTo(a.x, a.y);
        context.lineTo(b.x, b.y);
        context.stroke();
      });
      context.setLineDash([]);
      const projected = [];
      collisions(graph.nodes, graphBounds).forEach((group) => {
        group.forEach((entry, index) => {
          // Identical recorded time/depth/layer remain truthful; only their visual
          // marker is fanned out so each historical node can be selected.
          const angle = group.length > 1 ? (Math.PI * 2 * index) / group.length : 0;
          const radius = group.length > 1 ? Math.min(.09, .028 * group.length) : 0;
          const visual = { ...entry.point, x: entry.point.x + Math.cos(angle) * radius, y: entry.point.y + Math.sin(angle) * radius };
          projected.push({ node: entry.node, groupSize: group.length, groupIndex: index, ...project(visual, width, height) });
        });
      });
      view.projected = projected.sort((a, b) => a.z - b.z);
      view.projected.forEach((entry) => {
        const selected = view.selected === entry.node.id;
        const radius = (selected ? 8 : 5.5) * entry.scale;
        context.fillStyle = colors[entry.node.kind] || colors.default;
        context.globalAlpha = clamp(.48 + (entry.z + 1) * .24, .48, 1);
        context.beginPath();
        context.arc(entry.x, entry.y, radius, 0, Math.PI * 2);
        context.fill();
        context.globalAlpha = 1;
        if (selected) {
          context.strokeStyle = '#26231f';
          context.lineWidth = 2;
          context.beginPath();
          context.arc(entry.x, entry.y, radius + 3, 0, Math.PI * 2);
          context.stroke();
          label(String(entry.node.title || entry.node.revision_id || entry.node.id), entry.x + radius + 7, entry.y - radius - 4, '#26231f');
        }
      });
      view.projected.filter((entry) => entry.groupSize > 1 && entry.groupIndex === 0).forEach((entry) => {
        context.fillStyle = '#26231f';
        context.beginPath();
        context.arc(entry.x + 11, entry.y - 11, 8, 0, Math.PI * 2);
        context.fill();
        context.fillStyle = '#fff';
        context.font = '10px ui-rounded, system-ui, sans-serif';
        context.fillText(String(entry.groupSize), entry.x + 8, entry.y - 8);
      });
      const layers = graph.layers || [];
      if (layers.length) {
        context.fillStyle = '#716b61';
        context.font = '11px ui-rounded, system-ui, sans-serif';
        context.fillText(`스키마 층 ${layers.length}개 · 전체 이름은 층 선택 메뉴에서 확인`, 14, height - 14);
      }
      let legendY = 18;
      [...usedEdgeTypes].sort().forEach((type) => {
        const style = edgeStyle[type] || edgeStyle.composition;
        context.strokeStyle = style.color;
        context.setLineDash(style.dash);
        context.beginPath();
        context.moveTo(14, legendY - 3);
        context.lineTo(34, legendY - 3);
        context.stroke();
        context.setLineDash([]);
        label(type, 40, legendY + 1, style.color);
        legendY += 16;
      });
    }

    function chooseAt(event) {
      const rect = canvas.getBoundingClientRect();
      const x = event.clientX - rect.left;
      const y = event.clientY - rect.top;
      const hit = [...view.projected].reverse().find((entry) => Math.hypot(entry.x - x, entry.y - y) <= 14 * entry.scale);
      if (!hit) return;
      view.selected = hit.node.id;
      render();
      options.onSelect?.(hit.node);
    }

    canvas.addEventListener('pointerdown', (event) => {
      canvas.setPointerCapture?.(event.pointerId);
      dragging = { x: event.clientX, y: event.clientY, moved: false };
    });
    canvas.addEventListener('pointermove', (event) => {
      if (!dragging) return;
      const dx = event.clientX - dragging.x;
      const dy = event.clientY - dragging.y;
      if (Math.abs(dx) + Math.abs(dy) > 3) dragging.moved = true;
      view.yaw += dx * .009;
      view.pitch = clamp(view.pitch + dy * .009, -1.3, 1.3);
      dragging.x = event.clientX;
      dragging.y = event.clientY;
      render();
    });
    canvas.addEventListener('pointerup', (event) => {
      const moved = dragging?.moved;
      dragging = null;
      if (!moved) chooseAt(event);
    });
    canvas.addEventListener('wheel', (event) => {
      event.preventDefault();
      view.zoom = clamp(view.zoom * (event.deltaY < 0 ? 1.12 : .89), .45, 3.2);
      render();
    }, { passive: false });
    canvas.addEventListener('keydown', (event) => {
      if (event.key === 'ArrowLeft') view.yaw -= .12;
      else if (event.key === 'ArrowRight') view.yaw += .12;
      else if (event.key === 'ArrowUp') view.pitch = clamp(view.pitch - .12, -1.3, 1.3);
      else if (event.key === 'ArrowDown') view.pitch = clamp(view.pitch + .12, -1.3, 1.3);
      else if (event.key === '+' || event.key === '=') view.zoom = clamp(view.zoom * 1.12, .45, 3.2);
      else if (event.key === '-') view.zoom = clamp(view.zoom * .89, .45, 3.2);
      else return;
      event.preventDefault();
      render();
    });
    if (window.ResizeObserver) {
      observer = new ResizeObserver(render);
      observer.observe(canvas);
    } else window.addEventListener('resize', render);

    return {
      setGraph(graph) { view.graph = graph; view.selected = null; render(); },
      setSelected(id) { view.selected = id; render(); },
      reset() { view.yaw = -0.68; view.pitch = .38; view.zoom = 1; render(); },
      render,
      destroy() { observer?.disconnect(); }
    };
  }

  window.IdeaGraph3D = { create: makeRenderer };
})();
