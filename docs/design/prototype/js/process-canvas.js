/* SpanBay Interactive Prototype — Unified ProcessView Canvas Engine
 * Shared by Flows (HTTP Request DAG) and Agent Workflow Monitor Modal.
 * Supports LR / TB / Serpentine Wrap, Center / Top align, Swimlanes,
 * Multi-exit Condition branches, Parallel Fan-out / Join, Layered DataFlow arcs,
 * Hoverable edge '+' step insertion, and Drag-to-reposition + 1-click Reset. */
window.PD = window.PD || {};

window.PD.ProcessView = (() => {
  const SNAP = 8;
  const DRAG_THRESHOLD = 4;
  const CARD_W = 228;
  const CARD_H = 84;
  const COND_W = 224;
  const COND_H = 88;
  const JOIN_W = 188;
  const JOIN_H = 66;
  const COL_GAP = 64;
  const ROW_GAP = 34;
  const PAD_X = 36;
  const PAD_Y = 30;
  const LANE_HEADER_W = 112;
  const LANE_PAD_Y = 18;
  const LANE_GAP = 14;

  const esc = (s) =>
    String(s ?? "")
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");

  function nodeSize(node) {
    if (node.kind === "condition") return { w: COND_W, h: COND_H };
    if (node.kind === "join") return { w: JOIN_W, h: JOIN_H };
    return { w: CARD_W, h: CARD_H };
  }

  function statusTone(status) {
    if (status === "ok") return "ok";
    if (status === "bad") return "bad";
    if (status === "warn") return "warn";
    if (status === "run") return "run";
    if (status === "skipped") return "skipped";
    return "idle";
  }

  function statusLabel(status) {
    const map = {
      ok: "通过",
      bad: "失败",
      warn: "待审批",
      run: "运行中",
      skipped: "已跳过",
      idle: "等待",
    };
    return map[status] || status;
  }

  // Assign topological ranks (Longest-path layering) using Control edges only.
  function computeRanks(nodes, edges) {
    const byId = new Map(nodes.map((n, i) => [n.id, i]));
    const indeg = new Array(nodes.length).fill(0);
    const out = nodes.map(() => []);
    for (const e of edges) {
      if (e.layer === "data") continue;
      const u = byId.get(e.from);
      const v = byId.get(e.to);
      if (u === undefined || v === undefined || u === v) continue;
      out[u].push(v);
      indeg[v]++;
    }
    const rank = new Array(nodes.length).fill(0);
    const q = [];
    for (let i = 0; i < nodes.length; i++) if (indeg[i] === 0) q.push(i);
    let head = 0;
    while (head < q.length) {
      const u = q[head++];
      for (const v of out[u]) {
        if (rank[v] < rank[u] + 1) rank[v] = rank[u] + 1;
        indeg[v]--;
        if (indeg[v] === 0) q.push(v);
      }
    }
    return rank;
  }

  function computeLayout(model, ui) {
    const { nodes, edges, lanes = [] } = model;
    const pinned = ui.pinned || {};
    const dir = ui.direction || "lr";
    const align = ui.align || "center";
    const showLanes = Boolean(ui.swimlanes && lanes.length > 0 && dir !== "serpentine");
    const ranks = computeRanks(nodes, edges);
    const maxRank = ranks.reduce((m, r) => Math.max(m, r), 0);

    const pos = new Map();
    const laneBands = [];
    let canvasW = 760;
    let canvasH = 320;

    if (showLanes && dir === "lr") {
      // Group nodes by lane, then column = rank
      const laneOrder = lanes.map((l) => l.id);
      const colWidths = new Array(maxRank + 1).fill(CARD_W);
      nodes.forEach((n, i) => {
        const sz = nodeSize(n);
        colWidths[ranks[i]] = Math.max(colWidths[ranks[i]], sz.w);
      });
      const colX = [];
      let cx = PAD_X + LANE_HEADER_W + 18;
      for (let r = 0; r <= maxRank; r++) {
        colX.push(cx);
        cx += colWidths[r] + COL_GAP;
      }
      let curY = PAD_Y;
      for (const lane of lanes) {
        const inLane = nodes
          .map((n, i) => ({ n, i, r: ranks[i] }))
          .filter((x) => (x.n.lane || laneOrder[0]) === lane.id);
        const byCol = new Map();
        for (const item of inLane) {
          if (!byCol.has(item.r)) byCol.set(item.r, []);
          byCol.get(item.r).push(item);
        }
        let maxStack = 1;
        for (const arr of byCol.values()) maxStack = Math.max(maxStack, arr.length);
        const bandH = Math.max(118, maxStack * (CARD_H + 18) - 18 + LANE_PAD_Y * 2);
        laneBands.push({ id: lane.id, label: lane.label, x: PAD_X, y: curY, w: Math.max(760, cx - PAD_X), h: bandH });
        for (const [r, arr] of byCol.entries()) {
          const totalH = arr.reduce((s, it) => s + nodeSize(it.n).h, 0) + Math.max(0, arr.length - 1) * 18;
          let ny = align === "center" ? curY + (bandH - totalH) / 2 : curY + LANE_PAD_Y;
          for (const it of arr) {
            const sz = nodeSize(it.n);
            const nx = colX[r] + (colWidths[r] - sz.w) / 2;
            pos.set(it.n.id, { x: Math.round(nx), y: Math.round(ny), w: sz.w, h: sz.h });
            ny += sz.h + 18;
          }
        }
        curY += bandH + LANE_GAP;
      }
      canvasW = Math.max(820, cx + PAD_X);
      canvasH = Math.max(320, curY + PAD_Y);
    } else if (dir === "serpentine") {
      // 4-per-row snake wrap (even rows L->R, odd rows R->L)
      const perRow = 4;
      const cellW = CARD_W + 58;
      const cellH = CARD_H + 54;
      const ordered = [...nodes].sort((a, b) => (a.order ?? 0) - (b.order ?? 0));
      ordered.forEach((n, idx) => {
        const row = Math.floor(idx / perRow);
        const colInRow = idx % perRow;
        const col = row % 2 === 0 ? colInRow : perRow - 1 - colInRow;
        const sz = nodeSize(n);
        const x = PAD_X + col * cellW + (CARD_W - sz.w) / 2;
        const y = PAD_Y + row * cellH + (CARD_H - sz.h) / 2;
        pos.set(n.id, { x: Math.round(x), y: Math.round(y), w: sz.w, h: sz.h });
      });
      const rows = Math.max(1, Math.ceil(ordered.length / perRow));
      canvasW = Math.max(820, PAD_X * 2 + Math.min(perRow, ordered.length) * cellW);
      canvasH = Math.max(280, PAD_Y * 2 + rows * cellH);
    } else if (dir === "tb") {
      // Top-to-Bottom layered layout (optional vertical swimlanes)
      const layers = Array.from({ length: maxRank + 1 }, () => []);
      nodes.forEach((n, i) => layers[ranks[i]].push(n));
      if (showLanes) {
        const laneW = CARD_W + 54;
        lanes.forEach((lane, li) => {
          laneBands.push({
            id: lane.id,
            label: lane.label,
            x: PAD_X + li * (laneW + 14),
            y: PAD_Y,
            w: laneW,
            h: Math.max(320, (maxRank + 1) * (CARD_H + 52) + 56),
            vertical: true,
          });
        });
        const laneIdx = new Map(lanes.map((l, i) => [l.id, i]));
        let curY = PAD_Y + 42;
        for (let r = 0; r <= maxRank; r++) {
          let rowH = CARD_H;
          for (const n of layers[r]) {
            const sz = nodeSize(n);
            const li = laneIdx.get(n.lane) ?? 0;
            const band = laneBands[li];
            const x = band.x + (band.w - sz.w) / 2;
            pos.set(n.id, { x: Math.round(x), y: Math.round(curY), w: sz.w, h: sz.h });
            rowH = Math.max(rowH, sz.h);
          }
          curY += rowH + 52;
        }
        canvasW = Math.max(820, PAD_X * 2 + lanes.length * (laneW + 14));
        canvasH = Math.max(340, curY + PAD_Y);
      } else {
        const maxLayerW = layers.reduce(
          (m, arr) => Math.max(m, arr.reduce((s, n) => s + nodeSize(n).w, 0) + Math.max(0, arr.length - 1) * 34),
          CARD_W
        );
        const totalW = Math.max(760, maxLayerW + PAD_X * 2);
        let curY = PAD_Y;
        for (let r = 0; r <= maxRank; r++) {
          const arr = layers[r];
          const wSum = arr.reduce((s, n) => s + nodeSize(n).w, 0) + Math.max(0, arr.length - 1) * 34;
          let nx = align === "center" ? (totalW - wSum) / 2 : PAD_X;
          let rowH = CARD_H;
          for (const n of arr) {
            const sz = nodeSize(n);
            pos.set(n.id, { x: Math.round(nx), y: Math.round(curY), w: sz.w, h: sz.h });
            nx += sz.w + 34;
            rowH = Math.max(rowH, sz.h);
          }
          curY += rowH + 54;
        }
        canvasW = totalW;
        canvasH = Math.max(340, curY + PAD_Y);
      }
    } else {
      // LR without swimlanes
      const layers = Array.from({ length: maxRank + 1 }, () => []);
      nodes.forEach((n, i) => layers[ranks[i]].push(n));
      const colWidths = layers.map((arr) => arr.reduce((m, n) => Math.max(m, nodeSize(n).w), CARD_W));
      const maxLayerH = layers.reduce(
        (m, arr) => Math.max(m, arr.reduce((s, n) => s + nodeSize(n).h, 0) + Math.max(0, arr.length - 1) * ROW_GAP),
        CARD_H
      );
      const totalH = Math.max(320, maxLayerH + PAD_Y * 2);
      let cx = PAD_X;
      for (let r = 0; r <= maxRank; r++) {
        const arr = layers[r];
        const hSum = arr.reduce((s, n) => s + nodeSize(n).h, 0) + Math.max(0, arr.length - 1) * ROW_GAP;
        let ny = align === "center" ? (totalH - hSum) / 2 : PAD_Y;
        for (const n of arr) {
          const sz = nodeSize(n);
          const nx = cx + (colWidths[r] - sz.w) / 2;
          pos.set(n.id, { x: Math.round(nx), y: Math.round(ny), w: sz.w, h: sz.h });
          ny += sz.h + ROW_GAP;
        }
        cx += colWidths[r] + COL_GAP;
      }
      canvasW = Math.max(820, cx + PAD_X);
      canvasH = totalH;
    }

    // Overlay user-pinned positions (hybrid manual drag)
    for (const n of nodes) {
      const p = pinned[n.id];
      if (p) {
        const cur = pos.get(n.id) || { x: PAD_X, y: PAD_Y, ...nodeSize(n) };
        pos.set(n.id, { ...cur, x: p.x, y: p.y, pinned: true });
        canvasW = Math.max(canvasW, p.x + cur.w + PAD_X);
        canvasH = Math.max(canvasH, p.y + cur.h + PAD_Y);
      }
    }
    for (const band of laneBands) {
      if (!band.vertical) band.w = Math.max(band.w, canvasW - PAD_X * 2);
      else band.h = Math.max(band.h, canvasH - PAD_Y * 2);
    }

    return { pos, laneBands, canvasW, canvasH, horizontalPorts: dir !== "tb" };
  }

  function edgePath(a, b, horizontal, isData) {
    let x1, y1, x2, y2, d, mx, my;
    if (isData) {
      // Gentle arched top data-flow arc so it doesn't collide with control edges
      x1 = a.x + a.w * 0.5;
      y1 = a.y;
      x2 = b.x + b.w * 0.5;
      y2 = b.y;
      const lift = Math.min(92, Math.max(34, Math.abs(x2 - x1) * 0.16));
      const cy = Math.min(y1, y2) - lift;
      d = `M ${x1} ${y1} C ${x1} ${cy}, ${x2} ${cy}, ${x2} ${y2}`;
      mx = (x1 + x2) / 2;
      my = cy + lift * 0.25;
      return { d, x1, y1, x2, y2, mx, my };
    }

    if (horizontal) {
      const sameRowForward = b.x >= a.x + a.w - 16;
      const sameRowBackward = a.x >= b.x + b.w - 16 && Math.abs(a.y - b.y) < 40;
      if (sameRowForward) {
        x1 = a.x + a.w;
        y1 = a.y + a.h / 2;
        x2 = b.x;
        y2 = b.y + b.h / 2;
        const dx = Math.max(30, (x2 - x1) * 0.5);
        d = `M ${x1} ${y1} C ${x1 + dx} ${y1}, ${x2 - dx} ${y2}, ${x2} ${y2}`;
        mx = (x1 + x2) / 2;
        my = (y1 + y2) / 2;
      } else if (sameRowBackward) {
        // Serpentine odd row: right-to-left
        x1 = a.x;
        y1 = a.y + a.h / 2;
        x2 = b.x + b.w;
        y2 = b.y + b.h / 2;
        const dx = Math.max(30, (x1 - x2) * 0.5);
        d = `M ${x1} ${y1} C ${x1 - dx} ${y1}, ${x2 + dx} ${y2}, ${x2} ${y2}`;
        mx = (x1 + x2) / 2;
        my = (y1 + y2) / 2;
      } else {
        // Vertical drop between serpentine rows or steep step
        x1 = a.x + a.w / 2;
        y1 = a.y + a.h;
        x2 = b.x + b.w / 2;
        y2 = b.y;
        const dy = Math.max(24, Math.abs(y2 - y1) * 0.5);
        d = `M ${x1} ${y1} C ${x1} ${y1 + dy}, ${x2} ${y2 - dy}, ${x2} ${y2}`;
        mx = (x1 + x2) / 2;
        my = (y1 + y2) / 2;
      }
    } else {
      x1 = a.x + a.w / 2;
      y1 = a.y + a.h;
      x2 = b.x + b.w / 2;
      y2 = b.y;
      const dy = Math.max(28, Math.abs(y2 - y1) * 0.5);
      d = `M ${x1} ${y1} C ${x1} ${y1 + dy}, ${x2} ${y2 - dy}, ${x2} ${y2}`;
      mx = (x1 + x2) / 2;
      my = (y1 + y2) / 2;
    }
    return { d, x1, y1, x2, y2, mx, my };
  }

  function renderCard(node, rect, selectedId, mode) {
    const tone = statusTone(node.status);
    const sel = node.id === selectedId ? " selected" : "";
    const pin = rect.pinned ? " pinned" : "";
    const kindCls = node.kind === "condition" ? " pv-card-cond" : node.kind === "join" ? " pv-card-join" : "";
    const style = `left:${rect.x}px;top:${rect.y}px;width:${rect.w}px;height:${rect.h}px;`;

    let headBadge = "";
    if (node.kind === "condition") {
      headBadge = `<span class="pv-kind-tag pv-kind-cond">◇ 条件分支</span>`;
    } else if (node.kind === "join") {
      headBadge = `<span class="pv-kind-tag pv-kind-join">⑃ 并行汇合</span>`;
    } else if (node.method) {
      headBadge = `<span class="m-${node.method.toLowerCase()} fs-2xs">${esc(node.method)}</span>`;
    } else if (node.roleTag) {
      headBadge = `<span class="pv-kind-tag">${esc(node.roleTag)}</span>`;
    }

    const pinHtml = rect.pinned
      ? `<button type="button" class="pv-pin-btn" data-pv-unpin="${esc(node.id)}" title="取消固定坐标，恢复自动排版">📌</button>`
      : "";

    const footerPills = (node.pills || [])
      .map((p) => `<span class="pv-pill ${p.tone ? "tone-" + p.tone : ""}">${esc(p.text)}</span>`)
      .join("");

    return `
      <div class="pv-card tone-${tone}${kindCls}${sel}${pin}" data-pv-node="${esc(node.id)}" style="${style}" tabindex="0" role="button" aria-label="${esc(node.title)}">
        <div class="pv-card-top">
          <span class="dot dot-${tone === "skipped" ? "idle" : tone}"></span>
          ${headBadge}
          <span class="pv-card-title truncate">${esc(node.title)}</span>
          ${pinHtml}
          <span class="mono fs-2xs text-muted pv-card-metric">${esc(node.metric || "")}</span>
        </div>
        <div class="pv-card-sub truncate">${esc(node.subtitle || "")}</div>
        ${footerPills ? `<div class="pv-card-pills">${footerPills}</div>` : ""}
      </div>`;
  }

  function renderGraph(host, model, ui, opts = {}) {
    const layout = computeLayout(model, ui);
    const zoom = ui.zoom || 1;
    const selectedId = ui.selectedId;
    const showDataFlow = ui.dataFlow !== false;
    const editable = Boolean(opts.editable);
    const markersPrefix = `pv-arr-${opts.instanceId || "g"}`;

    const svgDefs = `
      <defs>
        <marker id="${markersPrefix}-ok" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="var(--ok)" /></marker>
        <marker id="${markersPrefix}-warn" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="var(--warn)" /></marker>
        <marker id="${markersPrefix}-bad" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="var(--bad)" /></marker>
        <marker id="${markersPrefix}-run" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="var(--accent-mark)" /></marker>
        <marker id="${markersPrefix}-idle" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="var(--border-control)" /></marker>
        <marker id="${markersPrefix}-data" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="6" markerHeight="6" orient="auto-start-reverse"><path d="M 0 1.5 L 9 5 L 0 8.5 z" fill="var(--accent-mark)" /></marker>
      </defs>`;

    let svgPaths = "";
    let edgeOverlays = "";

    for (const e of model.edges) {
      const a = layout.pos.get(e.from);
      const b = layout.pos.get(e.to);
      if (!a || !b) continue;
      const isData = e.layer === "data";
      if (isData && !showDataFlow) continue;
      const focused = selectedId && (e.from === selectedId || e.to === selectedId);
      if (isData && selectedId && !focused && !ui.showAllDataFlow) continue;

      const geo = edgePath(a, b, layout.horizontalPorts, isData);
      const tone = isData ? "data" : statusTone(e.tone || "idle");
      const activeCls = isData ? (focused ? " pv-edge-data-focus" : " pv-edge-data") : ` pv-edge-${tone}`;
      const dimCls = !isData && e.tone === "skipped" ? " pv-edge-skipped" : "";
      const markerKey = isData ? "data" : tone === "skipped" ? "idle" : tone;

      svgPaths += `<path d="${geo.d}" class="pv-edge${activeCls}${dimCls}" marker-end="url(#${markersPrefix}-${markerKey})" />`;
      if (!isData) {
        svgPaths += `<circle cx="${geo.x1}" cy="${geo.y1}" r="3" class="pv-port pv-port-${tone}" />`;
      }

      if (e.label) {
        const lblCls = isData ? "pv-edge-lbl pv-edge-lbl-data" : `pv-edge-lbl tone-${tone}`;
        edgeOverlays += `<span class="${lblCls}" style="left:${Math.round(geo.mx)}px;top:${Math.round(geo.my - (isData ? 4 : 11))}px">${esc(e.label)}</span>`;
      }
      if (editable && !isData && e.tone !== "skipped") {
        edgeOverlays += `<button type="button" class="pv-edge-plus" data-pv-insert-from="${esc(e.from)}" data-pv-insert-to="${esc(e.to)}" style="left:${Math.round(geo.mx)}px;top:${Math.round(geo.my + (e.label ? 10 : 0))}px" title="在此连线中间插入步骤">+</button>`;
      }
    }

    const lanesHtml = layout.laneBands
      .map(
        (b) => `
      <div class="pv-lane${b.vertical ? " vertical" : ""}" data-pv-lane="${esc(b.id)}" style="left:${b.x}px;top:${b.y}px;width:${b.w}px;height:${b.h}px">
        <span class="pv-lane-lbl">${esc(b.label)}</span>
      </div>`
      )
      .join("");

    const cardsHtml = model.nodes
      .map((n) => {
        const r = layout.pos.get(n.id);
        return r ? renderCard(n, r, selectedId, opts.mode) : "";
      })
      .join("");

    const guideLayer = `<div class="pv-guides" data-pv-guides></div>`;

    host.innerHTML = `
      <div class="pv-viewport" data-pv-viewport>
        <div class="pv-stage" data-pv-stage style="width:${Math.round(layout.canvasW * zoom)}px;height:${Math.round(layout.canvasH * zoom)}px">
          <div class="pv-stage-inner" style="width:${layout.canvasW}px;height:${layout.canvasH}px;transform:scale(${zoom});transform-origin:0 0">
            ${lanesHtml}
            <svg class="pv-svg" width="${layout.canvasW}" height="${layout.canvasH}" viewBox="0 0 ${layout.canvasW} ${layout.canvasH}" aria-hidden="true">
              ${svgDefs}
              ${svgPaths}
            </svg>
            ${guideLayer}
            ${edgeOverlays}
            ${cardsHtml}
          </div>
        </div>
      </div>`;

    bindDragAndClick(host, model, ui, layout, opts);
  }

  function bindDragAndClick(host, model, ui, layout, opts) {
    const stageInner = host.querySelector(".pv-stage-inner");
    const viewport = host.querySelector("[data-pv-viewport]");
    if (!stageInner || !viewport) return;

    let dragging = null;

    const clearGuides = () => {
      const g = host.querySelector("[data-pv-guides]");
      if (g) g.innerHTML = "";
    };

    const showGuides = (nx, ny, rect, dragId) => {
      const g = host.querySelector("[data-pv-guides]");
      if (!g) return;
      const lines = [];
      for (const [id, r] of layout.pos.entries()) {
        if (id === dragId) continue;
        if (Math.abs(r.x - nx) <= 4) lines.push(`<div class="pv-guide-v" style="left:${r.x}px"></div>`);
        if (Math.abs(r.y - ny) <= 4) lines.push(`<div class="pv-guide-h" style="top:${r.y}px"></div>`);
        const cy1 = r.y + r.h / 2;
        const cy2 = ny + rect.h / 2;
        if (Math.abs(cy1 - cy2) <= 4) lines.push(`<div class="pv-guide-h" style="top:${Math.round(cy1)}px"></div>`);
      }
      g.innerHTML = lines.slice(0, 4).join("");
    };

    stageInner.addEventListener("pointerdown", (e) => {
      if (e.target.closest("[data-pv-unpin], [data-pv-insert-from]")) return;
      const card = e.target.closest("[data-pv-node]");
      if (!card) return;
      const id = card.dataset.pvNode;
      const rect = layout.pos.get(id);
      if (!rect) return;
      card.setPointerCapture?.(e.pointerId);
      dragging = {
        id,
        card,
        startX: e.clientX,
        startY: e.clientY,
        origX: rect.x,
        origY: rect.y,
        rect,
        moved: false,
      };
    });

    stageInner.addEventListener("pointermove", (e) => {
      if (!dragging) return;
      const zoom = ui.zoom || 1;
      const dx = (e.clientX - dragging.startX) / zoom;
      const dy = (e.clientY - dragging.startY) / zoom;
      if (!dragging.moved && Math.hypot(dx, dy) < DRAG_THRESHOLD) return;
      dragging.moved = true;
      dragging.card.classList.add("is-dragging");
      const nx = Math.max(12, Math.round((dragging.origX + dx) / SNAP) * SNAP);
      const ny = Math.max(12, Math.round((dragging.origY + dy) / SNAP) * SNAP);
      dragging.lastX = nx;
      dragging.lastY = ny;
      dragging.card.style.left = `${nx}px`;
      dragging.card.style.top = `${ny}px`;
      showGuides(nx, ny, dragging.rect, dragging.id);
    });

    const finishDrag = (e) => {
      if (!dragging) return;
      const d = dragging;
      dragging = null;
      d.card.classList.remove("is-dragging");
      clearGuides();

      if (d.moved && d.lastX !== undefined) {
        ui.pinned = ui.pinned || {};
        ui.pinned[d.id] = { x: d.lastX, y: d.lastY };
        // Cross-lane drop reassigns node.lane
        if (ui.swimlanes && layout.laneBands.length > 0) {
          const cx = d.lastX + d.rect.w / 2;
          const cy = d.lastY + d.rect.h / 2;
          const hit = layout.laneBands.find((b) => cx >= b.x && cx <= b.x + b.w && cy >= b.y && cy <= b.y + b.h);
          const node = model.nodes.find((n) => n.id === d.id);
          if (hit && node && node.lane !== hit.id) {
            node.lane = hit.id;
            opts.onLaneChange?.(node, hit);
          }
        }
        opts.onMutate?.();
      } else {
        opts.onSelect?.(d.id);
      }
    };

    stageInner.addEventListener("pointerup", finishDrag);
    stageInner.addEventListener("pointercancel", finishDrag);

    viewport.addEventListener("click", (e) => {
      const unpin = e.target.closest("[data-pv-unpin]");
      if (unpin) {
        e.stopPropagation();
        if (ui.pinned) delete ui.pinned[unpin.dataset.pvUnpin];
        opts.onMutate?.();
        return;
      }
      const ins = e.target.closest("[data-pv-insert-from]");
      if (ins) {
        e.stopPropagation();
        opts.onInsertOnEdge?.(ins.dataset.pvInsertFrom, ins.dataset.pvInsertTo);
        return;
      }
      if (!e.target.closest("[data-pv-node]")) {
        opts.onBlankClick?.();
      }
    });
  }

  function renderSteps(host, model, ui, opts = {}) {
    const ranks = computeRanks(model.nodes, model.edges);
    const maxRank = ranks.reduce((m, r) => Math.max(m, r), 0);
    const groups = Array.from({ length: maxRank + 1 }, () => []);
    model.nodes.forEach((n, i) => groups[ranks[i]].push(n));

    const html = groups
      .map((arr, stageIdx) => {
        if (!arr.length) return "";
        const parallel = arr.length > 1 ? ` · 并行 ${arr.length} 路` : "";
        const items = arr
          .map((n) => {
            const tone = statusTone(n.status);
            const sel = n.id === ui.selectedId ? " selected" : "";
            const badge =
              n.kind === "condition"
                ? `<span class="pv-kind-tag pv-kind-cond">◇ 条件</span>`
                : n.kind === "join"
                  ? `<span class="pv-kind-tag pv-kind-join">⑃ 汇合</span>`
                  : n.method
                    ? `<span class="m-${n.method.toLowerCase()} fs-2xs" style="width:38px">${esc(n.method)}</span>`
                    : `<span class="pv-kind-tag">${esc(n.roleTag || "步骤")}</span>`;
            const laneName = model.lanes?.find((l) => l.id === n.lane)?.label;
            return `
              <button type="button" class="pv-step-row tone-${tone}${sel}" data-pv-step-pick="${esc(n.id)}">
                <span class="dot dot-${tone === "skipped" ? "idle" : tone}"></span>
                ${badge}
                <div class="pv-step-main">
                  <div class="row-center" style="gap:6px">
                    <b class="truncate">${esc(n.title)}</b>
                    ${laneName ? `<span class="tag">${esc(laneName)}</span>` : ""}
                  </div>
                  <span class="mono fs-2xs text-muted truncate">${esc(n.subtitle || "")}</span>
                </div>
                <span class="tag ${tone === "ok" ? "tag-ok" : tone === "bad" ? "tag-bad" : tone === "warn" ? "tag-warn" : tone === "run" ? "tag-accent" : ""}">${esc(n.metric || statusLabel(n.status))}</span>
              </button>`;
          })
          .join("");
        return `
          <div class="pv-stage-group">
            <div class="pv-stage-group-head"><span>阶段 ${stageIdx + 1}${parallel}</span><span class="text-faint">${arr.length} 个节点</span></div>
            <div class="pv-stage-group-list">${items}</div>
          </div>`;
      })
      .join("");

    host.innerHTML = `<div class="pv-steps-wrap">${html}</div>`;
    host.querySelectorAll("[data-pv-step-pick]").forEach((btn) => {
      btn.addEventListener("click", () => opts.onSelect?.(btn.dataset.pvStepPick));
    });
  }

  function renderTimeline(host, model, ui, opts = {}) {
    const totalMs = Math.max(100, model.totalMs || 1420);
    const legendHtml = (model.phaseLegend || [])
      .map((l) => `<span class="pv-legend-item"><i style="background:${l.color}"></i>${esc(l.label)}</span>`)
      .join("");

    const rows = model.nodes
      .map((n) => {
        const tone = statusTone(n.status);
        const sel = n.id === ui.selectedId ? " selected" : "";
        const startPct = Math.min(96, ((n.startMs || 0) / totalMs) * 100);
        const durPct = Math.max(1.5, ((n.durationMs || 18) / totalMs) * 100);
        let barHtml = "";
        if (n.status === "skipped") {
          barHtml = `<div class="pv-wf-skip" style="left:${startPct.toFixed(1)}%;width:6%">已跳过</div>`;
        } else if (n.phases && n.phases.length) {
          const sum = n.phases.reduce((s, p) => s + p.ms, 0) || 1;
          const segs = n.phases
            .map((p) => `<i style="width:${((p.ms / sum) * 100).toFixed(1)}%;background:${p.color}" title="${esc(p.label)}: ${p.ms} ms"></i>`)
            .join("");
          barHtml = `<div class="pv-wf-seg-bar" style="left:${startPct.toFixed(1)}%;width:${Math.min(100 - startPct, durPct).toFixed(1)}%">${segs}</div>`;
        } else {
          barHtml = `<div class="pv-wf-solid tone-${tone}" style="left:${startPct.toFixed(1)}%;width:${Math.min(100 - startPct, durPct).toFixed(1)}%"></div>`;
        }
        return `
          <button type="button" class="pv-wf-row${sel}" data-pv-wf-pick="${esc(n.id)}">
            <span class="row-center" style="gap:6px;min-width:0"><span class="dot dot-${tone === "skipped" ? "idle" : tone}"></span><span class="truncate">${esc(n.title)}</span></span>
            <div class="pv-wf-track">${barHtml}</div>
            <span class="mono fs-2xs text-right ${tone === "warn" ? "text-warn" : tone === "bad" ? "text-bad" : "text-muted"}">${esc(n.metric || `${n.durationMs || 0} ms`)}</span>
          </button>`;
      })
      .join("");

    host.innerHTML = `
      <div class="pv-timeline-wrap">
        <div class="card-panel">
          <div class="card-panel-head">
            <span>分段瀑布时间轴</span>
            <div class="row-center" style="gap:10px">${legendHtml}<span class="mono fs-2xs text-muted">0 ms — ${totalMs} ms</span></div>
          </div>
          <div class="pv-wf-list">${rows}</div>
        </div>
      </div>`;

    host.querySelectorAll("[data-pv-wf-pick]").forEach((btn) => {
      btn.addEventListener("click", () => opts.onSelect?.(btn.dataset.pvWfPick));
    });
  }

  return { renderGraph, renderSteps, renderTimeline, computeLayout };
})();
