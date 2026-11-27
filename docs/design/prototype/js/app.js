/* PingDeck Interactive Prototype — Core State, Event Delegation & URL Router */
(() => {
  const byId = (id) => document.getElementById(id);
  const root = document.documentElement;
  const appWin = byId("app-window");
  const stage = byId("stage");
  const protoBar = byId("proto-bar");
  const wirePop = byId("wire-popover");
  const cmdModal = byId("cmd-modal");
  const cmdInput = byId("cmd-input");

  const ov = () => window.PingDeckOverlays;
  const { SETTINGS_PAGES, MOCK_BRANCHES, SSE_FRAMES, FLOW_STEPS, ENV_EXTRA_VIEWS, buildSubtabs } = window.PD;

  const state = {
    module: "requests", reqMode: "http", provView: "relay",
    envName: "Staging", envTab: "vars", usageView: "overview", usageScope: "全部服务商", setPage: "appearance",
    palette: "slate", mode: "light", density: "comfortable", platform: "macos", rail: false, frameless: false,
  };

  const TAB_TARGETS = {
    "req-ed": "req-ed-body", "req-resp": "req-resp-body", "ai-ed": "ai-ed-body", "ai-resp": "ai-resp-body",
    "mock-main": "mock-tab-body", "hist-resp": "hist-resp-body", "chat-det": "chat-det-body",
    "gw-main": "gw-tab-body", "prov-detail": "prov-detail-body", "usage-dim": "usage-dim-body",
  };
  const DEFAULT_TAB_HTML = {};
  Object.entries(TAB_TARGETS).forEach(([g, id]) => { const el = byId(id); if (el) DEFAULT_TAB_HTML[g] = el.innerHTML; });
  const SUBTAB_CONTENT = buildSubtabs(DEFAULT_TAB_HTML);

  const ENV_META = {
    Local: { badge: "默认基准", cls: "tag", meta: "无变量覆盖 · 继承工作区全局变量" },
    Staging: { badge: "● 当前生效", cls: "tag tag-ok", meta: "10 个变量（2 个秘密）" },
    Production: { badge: "▲ 发送前确认", cls: "tag tag-warn", meta: "8 个变量（3 个秘密）· 写操作受保护" },
  };

  function setSubTab(group, tab) {
    const bar = document.querySelector(`[data-tab-group="${group}"]`);
    if (bar) bar.querySelectorAll("[data-tab]").forEach((b) => b.classList.toggle("active", b.dataset.tab === tab));
    const target = byId(TAB_TARGETS[group]);
    const render = SUBTAB_CONTENT[group]?.[tab];
    if (target && render) target.innerHTML = render();
  }

  let swapFrame = 0;
  function suppressTransitionsForSwap() {
    root.dataset.noTransition = "";
    if (swapFrame !== 0) cancelAnimationFrame(swapFrame);
    swapFrame = requestAnimationFrame(() => {
      swapFrame = requestAnimationFrame(() => {
        swapFrame = 0;
        delete root.dataset.noTransition;
      });
    });
  }

  function applyTheme() {
    const effective = state.mode === "system"
      ? (window.matchMedia?.("(prefers-color-scheme: dark)").matches ? "dark" : "light")
      : state.mode;
    const nextPalette = state.palette === "slate" ? undefined : state.palette;
    if (root.dataset.mode !== effective || root.dataset.palette !== nextPalette) {
      suppressTransitionsForSwap();
    }
    root.dataset.mode = effective;
    if (nextPalette === undefined) delete root.dataset.palette; else root.dataset.palette = nextPalette;
    if (state.density === "compact") root.dataset.density = "compact"; else delete root.dataset.density;

    document.querySelectorAll("[data-set-palette]").forEach((el) => el.classList.toggle("active", el.dataset.setPalette === state.palette));
    document.querySelectorAll("[data-set-mode]").forEach((el) => {
      el.classList.toggle("active", el.dataset.setMode === (el.closest(".proto-seg") ? effective : state.mode));
    });
    document.querySelectorAll("[data-set-density]").forEach((el) => el.classList.toggle("active", el.dataset.setDensity === state.density));
  }

  function setModule(id) {
    state.module = id;
    document.querySelectorAll(".module-view").forEach((v) => v.classList.toggle("active", v.dataset.module === id));
    document.querySelectorAll(".nav-item[data-nav-to]").forEach((btn) => btn.classList.toggle("active", btn.dataset.navTo === id));
    if (wirePop) wirePop.hidden = true;
    ov()?.closeAllPopovers();
  }

  function setSettingsPage(pageId) {
    const def = SETTINGS_PAGES[pageId] ? pageId : "appearance";
    state.setPage = def;
    const info = SETTINGS_PAGES[def];
    const appEl = byId("set-page-appearance");
    const dynEl = byId("set-page-dynamic");
    const crumbGroup = byId("set-crumb-group");
    if (crumbGroup) {
      const same = info.group === info.title;
      crumbGroup.hidden = same;
      crumbGroup.textContent = same ? "" : `${info.group} ›`;
    }
    if (byId("set-crumb-page")) byId("set-crumb-page").textContent = info.title;
    if (byId("set-changed-btn")) byId("set-changed-btn").hidden = !info.changed;
    document.querySelectorAll("#set-nav-list [data-set-page]").forEach((b) => b.classList.toggle("selected", b.dataset.setPage === def));
    if (appEl) appEl.hidden = def !== "appearance";
    if (dynEl) {
      dynEl.hidden = def === "appearance";
      if (def !== "appearance") {
        const sub = info.sub || "更改即时自动保存；仅标注「仅这台电脑」的项目不会随同步文件夹导出。";
        const hero = def === "about" ? "" : `<div class="set-page-hero"><h2 class="set-page-title">${info.title}</h2><p class="set-page-sub">${sub}</p></div>`;
        dynEl.innerHTML = `${hero}${info.html()}`;
      }
    }
  }

  function setReqMode(mode) {
    state.reqMode = mode;
    if (byId("req-http-view")) byId("req-http-view").hidden = mode !== "http";
    if (byId("req-ai-view")) byId("req-ai-view").hidden = mode !== "ai";
    document.querySelectorAll("[data-req-mode]").forEach((el) => {
      const on = el.dataset.reqPrimary === mode;
      el.classList.toggle("active", on);
      el.classList.toggle("selected", on);
    });
    if (wirePop) wirePop.hidden = true;
  }

  function setProvView(view, clickedEl) {
    state.provView = view;
    if (byId("prov-relay-view")) byId("prov-relay-view").hidden = false;
    document.querySelectorAll("[data-prov-view]").forEach((el) => {
      const on = clickedEl ? el === clickedEl : el.dataset.provPrimary === "relay";
      el.classList.toggle("selected", on);
    });
  }

  function setEnvPick(name) {
    state.envName = ENV_META[name] ? name : "Staging";
    const info = ENV_META[state.envName];
    if (byId("env-work-title")) byId("env-work-title").textContent = state.envName;
    if (byId("env-work-badge")) { byId("env-work-badge").className = info.cls; byId("env-work-badge").textContent = info.badge; }
    if (byId("env-work-meta")) byId("env-work-meta").textContent = info.meta;
    setEnvTab(state.envName === "Production" ? "protect" : "vars");
  }

  function setEnvTab(tab) {
    state.envTab = tab;
    if (byId("env-vars-view")) byId("env-vars-view").hidden = tab !== "vars";
    if (byId("env-compare-view")) byId("env-compare-view").hidden = tab !== "compare";
    const extraEl = byId("env-extra-view");
    if (extraEl) {
      const render = ENV_EXTRA_VIEWS[tab];
      extraEl.hidden = !render;
      if (render) extraEl.innerHTML = render();
    }
    document.querySelectorAll(".work-actions [data-env-tab]").forEach((el) => el.classList.toggle("active", el.dataset.envTab === tab));
  }

  function setUsageView(view, titleText) {
    state.usageView = view;
    if (byId("usage-overview-view")) byId("usage-overview-view").hidden = view !== "overview";
    if (byId("usage-calls-view")) byId("usage-calls-view").hidden = view !== "calls";
    if (byId("usage-title")) byId("usage-title").textContent = titleText || state.usageScope;
    document.querySelectorAll("#usage-view-tabs [data-usage-view]").forEach((b) => b.classList.toggle("active", b.dataset.usageView === view));
  }

  function setUsageScope(scope, clickedRow) {
    state.usageScope = scope || "全部服务商";
    if (byId("usage-title")) byId("usage-title").textContent = state.usageScope;
    document.querySelectorAll('[data-module="usage"] .pane [data-usage-scope]').forEach((r) => {
      r.classList.toggle("selected", clickedRow ? r === clickedRow : r.dataset.usageScope === state.usageScope);
    });
  }

  function selectMockBranch(code, btnEl) {
    const spec = MOCK_BRANCHES[code];
    if (!spec) return;
    document.querySelectorAll("[data-mock-branch]").forEach((b) => b.classList.toggle("selected", b === btnEl));
    if (byId("mock-branch-title")) byId("mock-branch-title").textContent = spec.title;
    if (byId("mock-branch-status")) byId("mock-branch-status").value = spec.status;
    if (byId("mock-branch-delay")) byId("mock-branch-delay").value = spec.delay;
    if (byId("mock-branch-expr")) byId("mock-branch-expr").innerHTML = spec.ruleExpr;
    if (byId("mock-branch-headers")) byId("mock-branch-headers").textContent = spec.headers;
    if (byId("mock-branch-bucket")) byId("mock-branch-bucket").innerHTML = spec.bucketAction;
    if (byId("mock-branch-body")) byId("mock-branch-body").innerHTML = spec.body;
  }

  // ── Unified ProcessView Flows Controller ──────────────────────────────────
  const flowUi = {
    presetId: "ecommerce",
    model: window.PD.clonePreset?.("ecommerce"),
    runIdx: 0,
    running: false,
    view: "graph",
    direction: "lr",
    align: "center",
    swimlanes: true,
    dataFlow: true,
    zoom: 1,
    selectedId: "route",
    inspTab: "topology",
    pinned: {},
    seq: 1,
    getRuns() { return this.model?.runs || []; },
    getRunIndex() { return this.runIdx; },
    selectRun(idx) {
      const runs = this.getRuns();
      if (!runs.length) return;
      this.runIdx = ((idx % runs.length) + runs.length) % runs.length;
      renderFlowsWorkbench();
      ov()?.toast(`已切换回放：${runs[this.runIdx].label}`);
    },
  };
  window.PD.flowUi = flowUi;

  function loadFlowPreset(presetId) {
    flowUi.presetId = presetId;
    flowUi.model = window.PD.clonePreset(presetId);
    flowUi.runIdx = 0;
    flowUi.running = false;
    flowUi.pinned = {};
    flowUi.zoom = 1;
    const dl = flowUi.model.defaultLayout || {};
    flowUi.direction = dl.direction || "lr";
    flowUi.align = dl.align || "center";
    flowUi.swimlanes = Boolean(dl.swimlanes);
    flowUi.dataFlow = dl.dataFlow !== false;
    const failed = flowUi.model.nodes.find((n) => n.status === "bad");
    const cond = flowUi.model.nodes.find((n) => n.kind === "condition");
    flowUi.selectedId = (failed || cond || flowUi.model.nodes[0])?.id || null;
    flowUi.inspTab = failed ? "result" : cond ? "topology" : "config";
    renderFlowsWorkbench();
  }

  function renderFlowInspector() {
    const aside = byId("flow-inspector");
    const body = byId("flow-insp-body");
    const titleEl = byId("flow-insp-title");
    if (!aside || !body || !flowUi.model) return;
    const node = flowUi.model.nodes.find((n) => n.id === flowUi.selectedId);
    aside.hidden = !node;
    if (!node) return;

    if (titleEl) titleEl.textContent = node.title;
    byId("flow-insp-tabs")?.querySelectorAll("[data-flow-insp-tab]").forEach((b) => {
      b.classList.toggle("active", b.dataset.flowInspTab === flowUi.inspTab);
    });

    const laneName = flowUi.model.lanes?.find((l) => l.id === node.lane)?.label || "默认泳道";
    if (flowUi.inspTab === "topology") {
      const deps = (node.dependsOn || [])
        .map((id) => flowUi.model.nodes.find((x) => x.id === id)?.title || id);
      const branchesHtml = node.kind === "condition" && node.branches?.length
        ? `<div class="card-panel">
            <div class="card-panel-head"><span>条件分支出口（按顺序求值）</span><span class="tag tag-accent">${node.branches.length} 条</span></div>
            ${node.branches.map((br) => `
              <div class="list-row" style="background:${br.matched ? "var(--ok-soft)" : "var(--bg-surface)"};border-color:${br.matched ? "var(--ok-border)" : "var(--border-subtle)"}">
                <div class="list-row-col">
                  <span class="list-row-title">${br.label}</span>
                  <span class="mono fs-2xs text-muted">${br.expr} → ${br.targets.join(", ")}</span>
                </div>
                <span class="tag ${br.matched ? "tag-ok" : ""}">${br.matched ? "命中" : "跳过"}</span>
              </div>`).join("")}
          </div>`
        : "";
      const laneChips = (flowUi.model.lanes || [])
        .map((l) => `<button type="button" class="tag${l.id === node.lane ? " tag-accent" : ""}" data-flow-set-lane="${l.id}">${l.label}</button>`)
        .join("");
      body.innerHTML = `
        <div class="card-panel">
          <div class="card-panel-head"><span>节点类型与泳道归属</span><span class="tag">${node.kind === "condition" ? "条件分支" : node.kind === "join" ? "并行汇合" : "HTTP 请求"}</span></div>
          <div class="row-wrap">${laneChips}</div>
          <span class="fs-2xs text-muted">提示：也可直接在画布中跨泳道拖拽卡片自动改归属。</span>
        </div>
        <div class="card-panel">
          <div class="card-panel-head"><span>上游依赖 (depends_on)</span><span class="tag">${deps.length} 个</span></div>
          <div class="row-wrap">${deps.length ? deps.map((d) => `<span class="tag">${d}</span>`).join("") : `<span class="text-muted">入口节点（无上游依赖）</span>`}</div>
        </div>
        ${branchesHtml}`;
    } else if (flowUi.inspTab === "result") {
      const toneCls = node.status === "ok" ? "tag-ok" : node.status === "bad" ? "tag-bad" : node.status === "skipped" ? "" : "tag-accent";
      body.innerHTML = `
        <div class="card-panel">
          <div class="card-panel-head"><span>执行状态与耗时</span><span class="tag ${toneCls}">${node.metric || node.status}</span></div>
          <div class="fs-xs text-muted">起始偏移：<b class="mono text-ink1">+${node.startMs || 0} ms</b> · 持续 <b class="mono text-ink1">${node.durationMs || 0} ms</b></div>
          ${node.assert ? `<div class="fs-2xs ${node.status === "bad" ? "text-bad" : "text-ink2"}">断言：${node.assert}</div>` : ""}
        </div>
        <div class="card-panel">
          <div class="card-panel-head"><span>响应快照（脱敏预览）</span><button type="button" class="text-accent" data-nav-to="history">查看历史 ↗</button></div>
          <div class="code-box fs-2xs" style="white-space:pre-wrap">${node.body || "// 控制节点（无 HTTP 响应体）"}</div>
        </div>`;
    } else {
      body.innerHTML = `
        <div class="card-panel">
          <div class="card-panel-head"><span>请求与归属</span><span class="tag">${laneName}</span></div>
          <div>${node.method ? `<span class="m-${node.method.toLowerCase()}">${node.method}</span> ` : ""}<code class="text-ink1">${node.subtitle || "—"}</code></div>
          ${node.retry ? `<div class="fs-2xs text-muted">重试与超时：${node.retry}</div>` : ""}
        </div>
        <div class="card-panel">
          <div class="card-panel-head"><span>提取变量 (Extract)</span><span class="tag">JSONPath</span></div>
          <div class="mono fs-2xs text-ink2">${node.extract || "—（无提取变量）"}</div>
        </div>
        <div class="card-panel">
          <div class="card-panel-head"><span>步骤断言</span></div>
          <div class="fs-2xs text-ink2">${node.assert || "—"}</div>
        </div>`;
    }
  }

  function renderFlowsWorkbench() {
    const model = flowUi.model;
    const host = byId("flow-stage-main");
    if (!model || !host || !window.PD.ProcessView) return;

    const titleEl = byId("flow-work-title");
    if (titleEl) titleEl.textContent = model.name;
    const stEl = byId("flow-work-status");
    if (stEl) {
      stEl.textContent = flowUi.running ? "↻ 运行中…" : model.statusText;
      stEl.className = `tag ${flowUi.running ? "tag-accent" : model.statusTone === "bad" ? "tag-bad" : "tag-ok"}`;
    }
    const runBtn = byId("flow-run-btn");
    if (runBtn) {
      runBtn.textContent = flowUi.running ? "■ 停止流程" : "▶ 运行流程";
      runBtn.className = `btn btn-sm ${flowUi.running ? "btn-danger" : "btn-primary"}`;
    }
    const runs = flowUi.getRuns();
    const curRun = runs[flowUi.runIdx];
    const histLbl = byId("flow-hist-label");
    if (histLbl && curRun) histLbl.textContent = `${curRun.label} ▾`;

    byId("flow-view-seg")?.querySelectorAll("[data-flow-view]").forEach((b) => b.classList.toggle("active", b.dataset.flowView === flowUi.view));
    byId("flow-dir-seg")?.querySelectorAll("[data-flow-dir]").forEach((b) => b.classList.toggle("active", b.dataset.flowDir === flowUi.direction));
    byId("flow-toggle-lanes")?.classList.toggle("tag-accent", flowUi.swimlanes);
    const alignBtn = byId("flow-toggle-align");
    if (alignBtn) {
      alignBtn.classList.toggle("tag-accent", flowUi.align === "center");
      alignBtn.textContent = flowUi.align === "center" ? "居中" : "顶对齐";
    }
    byId("flow-toggle-data")?.classList.toggle("tag-accent", flowUi.dataFlow);
    const resetBtn = byId("flow-reset-layout");
    if (resetBtn) resetBtn.hidden = Object.keys(flowUi.pinned).length === 0;
    const zoomLbl = byId("flow-zoom-lbl");
    if (zoomLbl) zoomLbl.textContent = `${Math.round(flowUi.zoom * 100)}%`;

    const opts = {
      instanceId: "flow",
      editable: true,
      onSelect: (id) => { flowUi.selectedId = id; renderFlowsWorkbench(); },
      onBlankClick: () => { flowUi.selectedId = null; renderFlowsWorkbench(); },
      onMutate: () => renderFlowsWorkbench(),
      onLaneChange: (node, band) => ov()?.toast(`已将「${node.title}」移动到泳道「${band.label}」`),
      onInsertOnEdge: (fromId, toId) => {
        const idx = flowUi.seq++;
        const newId = `inserted_${idx}`;
        const fromNode = model.nodes.find((n) => n.id === fromId);
        const toNode = model.nodes.find((n) => n.id === toId);
        model.nodes.push({
          id: newId, order: model.nodes.length, kind: "request", lane: fromNode?.lane || model.lanes[0]?.id,
          method: "GET", title: `校验节点 #${idx}`, subtitle: "/api/check/state", metric: "等待", status: "idle",
          startMs: (fromNode?.startMs || 0) + (fromNode?.durationMs || 80), durationMs: 60,
          pills: [{ text: "在线插入", tone: "accent" }], extract: "—", assert: "status == 200",
          retry: "不重试", dependsOn: [fromId], body: "// 新增内联检查步骤",
        });
        const edgeIdx = model.edges.findIndex((e) => e.layer !== "data" && e.from === fromId && e.to === toId);
        if (edgeIdx >= 0) {
          const old = model.edges[edgeIdx];
          model.edges.splice(edgeIdx, 1,
            { from: fromId, to: newId, layer: "control", tone: "idle", label: old.label },
            { from: newId, to: toId, layer: "control", tone: "idle" }
          );
        }
        if (toNode && Array.isArray(toNode.dependsOn)) {
          toNode.dependsOn = toNode.dependsOn.map((d) => (d === fromId ? newId : d));
        }
        flowUi.selectedId = newId;
        flowUi.inspTab = "config";
        renderFlowsWorkbench();
        ov()?.toast(`已在「${fromNode?.title || fromId} → ${toNode?.title || toId}」中间插入步骤`);
      },
    };

    if (flowUi.view === "steps") window.PD.ProcessView.renderSteps(host, model, flowUi, opts);
    else if (flowUi.view === "timeline") window.PD.ProcessView.renderTimeline(host, model, flowUi, opts);
    else window.PD.ProcessView.renderGraph(host, model, flowUi, opts);
    renderFlowInspector();
  }

  const toggleRail = () => {
    state.rail = !state.rail;
    appWin.classList.toggle("rail", state.rail);
    const btn = document.querySelector('[data-action="toggle-rail"]');
    if (btn) btn.setAttribute("aria-expanded", String(!state.rail));
  };
  const togglePanel = (id, force) => { const el = byId(id); if (el) el.hidden = force !== undefined ? !force : !el.hidden; };

  function filterCmdItems(q) {
    const term = q.trim().toLowerCase();
    let first = true;
    document.querySelectorAll("#cmd-list .palette-item").forEach((item) => {
      const hit = !term || item.textContent.toLowerCase().includes(term);
      item.hidden = !hit;
      item.classList.toggle("active", hit && first);
      if (hit) first = false;
    });
  }

  function toggleCmd(open) {
    const show = open ?? cmdModal.hidden;
    cmdModal.hidden = !show;
    if (show && cmdInput) { cmdInput.value = ""; filterCmdItems(""); setTimeout(() => cmdInput.focus(), 10); }
  }

  if (cmdInput) {
    cmdInput.addEventListener("input", () => filterCmdItems(cmdInput.value));
    cmdInput.addEventListener("keydown", (e) => {
      if (e.key === "Enter") {
        const hit = document.querySelector("#cmd-list .palette-item:not([hidden])");
        if (hit) { e.preventDefault(); hit.click(); }
      }
    });
  }

  byId("set-search-input")?.addEventListener("input", (e) => {
    const q = e.target.value.trim().toLowerCase();
    document.querySelectorAll("#set-nav-list [data-set-page]").forEach((btn) => {
      const id = btn.dataset.setPage;
      const info = SETTINGS_PAGES[id];
      btn.hidden = Boolean(q && !id.includes(q) && !info.title.toLowerCase().includes(q) && !info.group.toLowerCase().includes(q));
    });
  });

  const catalogUi = { unlistedProv: "", unlistedStatus: "all", savedMult: "0.5", savedCurr: "CNY" };

  function filterUnlistedRows() {
    const q = (byId("cat-unlisted-search")?.value || "").trim().toLowerCase();
    let shown = 0;
    document.querySelectorAll("#unlisted-rows tr").forEach((tr) => {
      const okProv = !catalogUi.unlistedProv || tr.dataset.prov === catalogUi.unlistedProv;
      const okStatus = catalogUi.unlistedStatus === "all"
        || (catalogUi.unlistedStatus === "unpriced" && tr.dataset.priced === "0")
        || (catalogUi.unlistedStatus === "priced" && tr.dataset.priced === "1");
      const okQ = !q || tr.textContent.toLowerCase().includes(q);
      const vis = okProv && okStatus && okQ;
      tr.hidden = !vis;
      if (vis) shown++;
    });
    const pg = document.querySelector('[data-pg-bar="cat-unlisted"] .pg-summary');
    if (pg) pg.textContent = shown ? `显示第 1–${shown} 条，共 ${shown} 条` : "共 0 条";
  }

  function updateProviderPricingFormula() {
    const basisBtn = byId("prov-pricing-basis-btn");
    const currBtn = byId("prov-pricing-curr-btn");
    const multEl = byId("prov-relay-mult");
    const formulaEl = byId("prov-pricing-formula");
    if (!basisBtn || !currBtn || !multEl || !formulaEl) return;
    const basis = basisBtn.dataset.curBasis || "";
    const basisName = basis ? basisBtn.textContent.replace(/\s*▾$/, "") : "";
    const curr = currBtn.dataset.curCurr || "";
    const multRaw = multEl.value.trim();
    const multNum = multRaw === "" ? (curr ? 1 : null) : Number(multRaw);
    const multValid = multNum !== null && Number.isFinite(multNum) && multNum > 0 && multNum <= 1000;
    const currLabel = curr || "目录原币种";
    if (basisName && multValid) {
      formulaEl.textContent = `计费规则：${basisName} 目录价 × ${multNum}（按 ${currLabel} 结算）· 单模型单独定价优先`;
    } else if (!basisName && multValid) {
      formulaEl.textContent = `计费规则：官方目录价 × ${multNum}（按 ${currLabel} 结算）· 单模型单独定价优先`;
    } else if (basisName) {
      formulaEl.textContent = `计费规则：${basisName} 目录价 · 单模型单独定价优先`;
    } else {
      formulaEl.textContent = "计费规则：官方目录价 · 单模型单独定价优先";
    }
    const dirty = multRaw !== catalogUi.savedMult || curr !== catalogUi.savedCurr;
    if (byId("prov-relay-apply")) byId("prov-relay-apply").hidden = !dirty;
    if (byId("prov-relay-reset")) byId("prov-relay-reset").hidden = !dirty;
  }

  window.PD.catalogUi = catalogUi;
  window.PD.filterUnlistedRows = filterUnlistedRows;
  window.PD.updateProviderPricingFormula = updateProviderPricingFormula;

  document.addEventListener("input", (e) => {
    if (e.target?.dataset?.filterUnlisted) { filterUnlistedRows(); return; }
    if (e.target?.id === "prov-relay-mult") { updateProviderPricingFormula(); return; }
    const sel = e.target?.dataset?.filterRows;
    if (sel) {
      const q = e.target.value.trim().toLowerCase();
      document.querySelectorAll(sel).forEach((el) => { el.hidden = Boolean(q && !el.textContent.toLowerCase().includes(q)); });
      return;
    }
    if (e.target?.id === "resp-jsonpath-input") {
      const q = e.target.value.trim().toLowerCase();
      let count = 0;
      document.querySelectorAll("#req-resp-code .code-line").forEach((ln) => {
        const show = !q || q.startsWith("$") || ln.textContent.toLowerCase().includes(q);
        ln.hidden = !show;
        if (show) count++;
      });
      const badge = byId("resp-filter-count");
      if (badge) badge.textContent = q.startsWith("$.data[0]") ? "命中 1 项 (inv_01J9X3K2P8)" : q.startsWith("$.next_cursor") ? `"inv_01J9X0M4R2"` : `${count} 行`;
    }
  });

  [["slider-ui-fs", "val-ui-fs", "sample-ui-row"], ["slider-mono-fs", "val-mono-fs", "sample-code-box"]].forEach(([sId, vId, tId]) => {
    byId(sId)?.addEventListener("input", (e) => {
      const v = e.target.value;
      if (byId(vId)) byId(vId).textContent = `${v} px`;
      if (byId(tId)) byId(tId).style.fontSize = `${v}px`;
    });
  });

  // ── Main Click Delegation ─────────────────────────────────────────────────
  document.addEventListener("click", (e) => {
    const chk = e.target.closest(".kv-table .check-box");
    if (chk) {
      const on = chk.classList.toggle("checked");
      chk.textContent = on ? "✓" : "";
      chk.closest("tr")?.classList.toggle("kv-off", !on);
      return;
    }

    const pill = e.target.closest("[data-pill-pick]");
    if (pill) {
      pill.closest("[data-pill-group]")?.querySelectorAll("[data-pill-pick]").forEach((p) => p.classList.toggle("tag-accent", p === pill));
      return;
    }

    const folderToggle = e.target.closest("[data-toggle-folder]");
    if (folderToggle && !e.target.closest("button")) {
      const fid = folderToggle.dataset.toggleFolder;
      const body = document.querySelector(`[data-folder-body="${fid}"]`);
      if (body) {
        body.hidden = !body.hidden;
        folderToggle.innerHTML = folderToggle.innerHTML.replace(body.hidden ? "▾" : "▸", body.hidden ? "▸" : "▾");
      }
      return;
    }

    const listRow = e.target.closest("[data-select-list] .list-row");
    if (listRow) listRow.closest("[data-select-list]").querySelectorAll(".list-row").forEach((r) => r.classList.toggle("selected", r === listRow));

    const frameChip = e.target.closest("[data-sse-frame]");
    if (frameChip) {
      document.querySelectorAll("[data-sse-frame]").forEach((c) => c.classList.toggle("active", c === frameChip));
      const f = SSE_FRAMES[frameChip.dataset.sseFrame];
      if (f && byId("ai-frame-inspector")) byId("ai-frame-inspector").innerHTML = `<span class="text-accent fw-600">${f.meta}</span><code class="truncate">${f.data}</code>`;
      return;
    }

    const jpChip = e.target.closest("[data-jsonpath-pick]");
    if (jpChip) {
      const inp = byId("resp-jsonpath-input");
      if (inp) { inp.value = jpChip.dataset.jsonpathPick; inp.dispatchEvent(new Event("input", { bubbles: true })); }
      return;
    }

    const mockBranch = e.target.closest("[data-mock-branch]");
    if (mockBranch) { selectMockBranch(mockBranch.dataset.mockBranch, mockBranch); return; }

    const flowPreset = e.target.closest("[data-flow-preset]");
    if (flowPreset) { loadFlowPreset(flowPreset.dataset.flowPreset); return; }

    const flowView = e.target.closest("[data-flow-view]");
    if (flowView) { flowUi.view = flowView.dataset.flowView; renderFlowsWorkbench(); return; }

    const flowDir = e.target.closest("[data-flow-dir]");
    if (flowDir) { flowUi.direction = flowDir.dataset.flowDir; renderFlowsWorkbench(); return; }

    const flowZoom = e.target.closest("[data-flow-zoom]");
    if (flowZoom) {
      flowUi.zoom = Math.min(1.6, Math.max(0.55, +(flowUi.zoom + Number(flowZoom.dataset.flowZoom)).toFixed(2)));
      renderFlowsWorkbench();
      return;
    }

    const flowInspTab = e.target.closest("[data-flow-insp-tab]");
    if (flowInspTab) { flowUi.inspTab = flowInspTab.dataset.flowInspTab; renderFlowInspector(); return; }

    const flowSetLane = e.target.closest("[data-flow-set-lane]");
    if (flowSetLane && flowUi.model) {
      const n = flowUi.model.nodes.find((x) => x.id === flowUi.selectedId);
      if (n) { n.lane = flowSetLane.dataset.flowSetLane; renderFlowsWorkbench(); }
      return;
    }

    const flowAdd = e.target.closest("[data-flow-add]");
    if (flowAdd && flowUi.model) {
      const kind = flowAdd.dataset.flowAdd;
      const idx = flowUi.seq++;
      const newId = `node_${idx}`;
      const tail = flowUi.model.nodes[flowUi.model.nodes.length - 1];
      const base = {
        id: newId, order: flowUi.model.nodes.length, lane: tail?.lane || flowUi.model.lanes[0]?.id,
        status: "idle", startMs: (tail?.startMs || 0) + 120, durationMs: 60, dependsOn: tail ? [tail.id] : [],
      };
      if (kind === "condition") {
        flowUi.model.nodes.push({ ...base, kind: "condition", title: `条件路由 #${idx}`, subtitle: "待配置分支规则", metric: "路由", branches: [{ label: "默认分支", expr: "true", targets: [], matched: false }] });
      } else if (kind === "join") {
        flowUi.model.nodes.push({ ...base, kind: "join", title: `并发汇合 #${idx}`, subtitle: "等待上游并行节点", metric: "屏障" });
      } else {
        flowUi.model.nodes.push({ ...base, kind: "request", method: kind === "saved" ? "GET" : "POST", title: kind === "saved" ? `引用请求 #${idx}` : `内联步骤 #${idx}`, subtitle: kind === "saved" ? "/api/orders/{{order_id}}" : "/api/custom/step", metric: "等待", extract: "—", assert: "status == 200", retry: "不重试", body: "// 尚未运行" });
      }
      if (tail) flowUi.model.edges.push({ from: tail.id, to: newId, layer: "control", tone: "idle" });
      flowUi.selectedId = newId;
      flowUi.inspTab = kind === "condition" || kind === "join" ? "topology" : "config";
      renderFlowsWorkbench();
      ov()?.toast(`已添加节点「${flowUi.model.nodes[flowUi.model.nodes.length - 1].title}」`);
      return;
    }

    const catTab = e.target.closest("[data-cat-tab]");
    if (catTab) {
      const which = catTab.dataset.catTab;
      catTab.parentElement?.querySelectorAll("[data-cat-tab]").forEach((b) => b.classList.toggle("active", b === catTab));
      if (byId("cat-pane-catalog")) byId("cat-pane-catalog").hidden = which !== "catalog";
      if (byId("cat-pane-unlisted")) byId("cat-pane-unlisted").hidden = which !== "unlisted";
      const fn = byId("cat-tab-footnote");
      if (fn) {
        fn.textContent = which === "unlisted"
          ? "显示已启用但尚未匹配到目录的模型（本地免费服务商自动排除）；指认目录模型后将自动归入「目录价格」。"
          : "价格按每百万 token 计（不跨币种换算）；点击任意行编辑默认价、查看分发商报价或设置服务商覆盖。";
      }
      return;
    }

    const unlStatus = e.target.closest("[data-unlisted-status]");
    if (unlStatus) {
      catalogUi.unlistedStatus = unlStatus.dataset.unlistedStatus || "all";
      unlStatus.closest("[data-unlisted-status-group]")?.querySelectorAll("[data-unlisted-status]").forEach((b) => b.classList.toggle("tag-accent", b === unlStatus));
      filterUnlistedRows();
      return;
    }

    const pgCtrl = e.target.closest("[data-pg-btn], [data-pg-size]");
    if (pgCtrl) {
      const bar = pgCtrl.closest("[data-pg-bar]");
      if (bar) {
        const total = Number(bar.dataset.pgTotal || 0);
        let size = Number(bar.dataset.pgSize || 50);
        let page = Number(bar.dataset.pgPage || 1);
        if (pgCtrl.dataset.pgSize) {
          size = Number(pgCtrl.dataset.pgSize);
          page = 1;
          bar.querySelectorAll("[data-pg-size]").forEach((b) => b.classList.toggle("active", b === pgCtrl));
        } else {
          const pages = Math.max(1, Math.ceil(total / size));
          const act = pgCtrl.dataset.pgBtn;
          if (act === "first") page = 1;
          else if (act === "prev") page = Math.max(1, page - 1);
          else if (act === "next") page = Math.min(pages, page + 1);
          else if (act === "last") page = pages;
        }
        const pages = Math.max(1, Math.ceil(total / size));
        page = Math.min(page, pages);
        bar.dataset.pgSize = String(size);
        bar.dataset.pgPage = String(page);
        const start = total ? (page - 1) * size + 1 : 0;
        const end = Math.min(total, page * size);
        const sumEl = bar.querySelector(".pg-summary");
        if (sumEl) sumEl.textContent = `显示第 ${start}–${end} 条，共 ${total.toLocaleString()} 条`;
        const numEl = bar.querySelector(".pg-page-num");
        if (numEl) numEl.textContent = `${page} / ${pages}`;
        const f = bar.querySelector('[data-pg-btn="first"]');
        const pr = bar.querySelector('[data-pg-btn="prev"]');
        const nx = bar.querySelector('[data-pg-btn="next"]');
        const ls = bar.querySelector('[data-pg-btn="last"]');
        if (f) f.disabled = page <= 1;
        if (pr) pr.disabled = page <= 1;
        if (nx) nx.disabled = page >= pages;
        if (ls) ls.disabled = page >= pages;
      }
      return;
    }

    const t = e.target.closest("button, tr[data-usage-view], [data-nav-to], [data-req-mode], [data-prov-view], [data-env-tab], [data-env-pick], [data-usage-view], [data-usage-scope], [data-tab-jump], [data-tab], [data-mock-server], [data-set-page], [data-set-palette], [data-set-mode], [data-set-density], [data-set-platform], [data-action]");
    if (!t) { if (wirePop && !wirePop.hidden && !e.target.closest("#wire-popover")) wirePop.hidden = true; return; }

    if (t.dataset.navTo) {
      setModule(t.dataset.navTo);
      if (t.dataset.subView === "ai" || t.dataset.subView === "http") setReqMode(t.dataset.subView);
      if (t.dataset.subView === "relay" || t.dataset.subView === "gateway") setProvView(t.dataset.subView);
      if (t.dataset.setPage) setSettingsPage(t.dataset.setPage);
      if (t.dataset.tabJump) { const [g, tb] = t.dataset.tabJump.split(":"); if (g && tb) setSubTab(g, tb); }
      if (!cmdModal.hidden) toggleCmd(false);
      ov()?.closeDrawer();
      return;
    }
    if (t.dataset.tabJump) { const [g, tb] = t.dataset.tabJump.split(":"); if (g && tb) setSubTab(g, tb); return; }
    if (t.dataset.tab) { const g = t.closest("[data-tab-group]")?.dataset.tabGroup; if (g) setSubTab(g, t.dataset.tab); return; }

    if (t.dataset.mockServer) {
      document.querySelectorAll("[data-mock-server]").forEach((s) => s.classList.toggle("active", s === t));
      if (byId("mock-srv-title")) byId("mock-srv-title").textContent = t.dataset.mockServer;
      const urlEl = byId("mock-srv-url");
      if (urlEl) { const u = `http://127.0.0.1:${t.dataset.mockPort}/v1`; urlEl.textContent = u; urlEl.dataset.toast = `已复制 Mock 地址 ${u}`; }
      ov()?.toast(`已切换至 Mock 服务「${t.dataset.mockServer} (:${t.dataset.mockPort})」`);
      return;
    }

    if (t.dataset.setPage) { setSettingsPage(t.dataset.setPage); return; }
    if (t.dataset.reqMode) { setReqMode(t.dataset.reqMode); return; }
    if (t.dataset.provView) { setProvView(t.dataset.provView, t); return; }
    if (t.dataset.envPick) { setEnvPick(t.dataset.envPick); return; }
    if (t.dataset.envTab) { setEnvTab(t.dataset.envTab); return; }
    if (t.dataset.usageScope) { setUsageScope(t.dataset.usageScope, t); return; }
    if (t.dataset.usageView) {
      const label = t.tagName === "TR"
        ? `${state.usageScope} · ${t.querySelector("td")?.textContent?.trim() || ""}`
        : undefined;
      setUsageView(t.dataset.usageView, label);
      return;
    }
    if (t.dataset.setPalette) { state.palette = t.dataset.setPalette; applyTheme(); return; }
    if (t.dataset.setMode) { state.mode = t.dataset.setMode; applyTheme(); return; }
    if (t.dataset.setDensity) { state.density = t.dataset.setDensity; applyTheme(); return; }
    if (t.dataset.setPlatform) {
      state.platform = t.dataset.setPlatform;
      if (appWin) appWin.dataset.platform = state.platform;
      document.querySelectorAll("[data-set-platform]").forEach((el) => el.classList.toggle("active", el.dataset.setPlatform === state.platform));
      return;
    }

    switch (t.dataset.action) {
      case "toggle-rail": toggleRail(); break;
      case "toggle-mode-quick":
        state.mode = root.dataset.mode === "dark" ? "light" : "dark";
        applyTheme();
        break;
      case "open-cmd": toggleCmd(true); break;
      case "close-cmd": toggleCmd(false); break;
      case "toggle-console": togglePanel("console-drawer"); break;
      case "close-flow-inspector": flowUi.selectedId = null; renderFlowsWorkbench(); break;
      case "flow-toggle-lanes": flowUi.swimlanes = !flowUi.swimlanes; renderFlowsWorkbench(); break;
      case "flow-toggle-align": flowUi.align = flowUi.align === "center" ? "top" : "center"; renderFlowsWorkbench(); break;
      case "flow-toggle-data": flowUi.dataFlow = !flowUi.dataFlow; renderFlowsWorkbench(); break;
      case "flow-reset-layout": flowUi.pinned = {}; renderFlowsWorkbench(); ov()?.toast("已重置为自动排版"); break;
      case "flow-hist-prev": flowUi.selectRun(flowUi.runIdx - 1); break;
      case "flow-hist-next": flowUi.selectRun(flowUi.runIdx + 1); break;
      case "toggle-flow-run":
        flowUi.running = !flowUi.running;
        renderFlowsWorkbench();
        ov()?.toast(flowUi.running ? `正在运行流程「${flowUi.model?.name}」（并行波次自动调度）…` : `已停止流程「${flowUi.model?.name}」`, { tone: flowUi.running ? "ok" : "warn" });
        break;
      case "add-env-var": {
        const kEl = byId("new-env-key"); const vEl = byId("new-env-val"); const tbody = byId("env-vars-tbody");
        const k = kEl?.value.trim() || "webhook_secret"; const v = vEl?.value.trim() || "whsec_stg_89a1f";
        if (tbody) {
          const tr = document.createElement("tr");
          tr.innerHTML = `<td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">${k}</td><td><span class="tag">文本</span></td><td class="mono">${v}</td>`;
          tbody.insertBefore(tr, tbody.lastElementChild);
        }
        if (kEl) kEl.value = ""; if (vEl) vEl.value = "";
        ov()?.toast(`已向 ${state.envName} 环境添加变量「${k}」`);
        break;
      }
      case "toggle-wire": {
        if (!wirePop) break;
        const show = wirePop.hidden;
        wirePop.hidden = !show;
        if (show) {
          const r = t.getBoundingClientRect();
          wirePop.style.left = `${Math.min(Math.max(12, r.left), window.innerWidth - 332)}px`;
          wirePop.style.right = "auto";
          wirePop.style.top = `${Math.min(r.bottom + 6, window.innerHeight - 170)}px`;
        }
        break;
      }
      case "send-http": {
        const ms = 148 + Math.floor(Math.random() * 56);
        if (byId("req-resp-ms")) byId("req-resp-ms").textContent = `${ms} ms`;
        if (byId("req-resp-age")) byId("req-resp-age").textContent = "刚刚";
        const body = document.querySelector("#console-drawer .console-body");
        if (body) {
          const line = document.createElement("div");
          line.className = "console-row";
          line.innerHTML = `<span class="text-faint">${new Date().toTimeString().slice(0, 8)}</span><span class="tag tag-ok" style="height:17px">HTTP</span><span>GET https://billing.staging.acme.dev/invoices?status=open&amp;limit=25 → 200 OK (${ms} ms)</span>`;
          body.prepend(line);
        }
        ov()?.toast(`请求成功 · 200 OK (${ms} ms · 4.2 KB)`);
        break;
      }
      case "add-ai-msg": {
        const list = byId("ai-ed-body");
        if (list) {
          const card = document.createElement("div");
          card.className = "ai-msg-card";
          card.innerHTML = `<span class="tag tag-accent">user ▾</span><span class="mono" style="font-size:12.5px">Follow-up message…</span><span class="text-faint">⋮</span>`;
          list.insertBefore(card, list.lastElementChild);
        }
        ov()?.toast("已添加一条对话消息");
        break;
      }
      case "toggle-web-pill": {
        const on = t.classList.toggle("tag-accent");
        t.textContent = on ? "联网 ×" : "联网（已关）";
        ov()?.toast(on ? "已开启智能体联网搜索" : "已关闭智能体联网搜索");
        break;
      }
      case "chat-jump-bottom": {
        const scroller = byId("chat-thread-scroll");
        if (scroller) scroller.scrollTo({ top: scroller.scrollHeight, behavior: "smooth" });
        const btn = byId("chat-jump-btn");
        if (btn) btn.hidden = true;
        break;
      }
      case "send-chat": {
        const inp = byId("chat-composer-input");
        const val = inp?.value.trim();
        if (inp) inp.value = "";
        ov()?.toast(val ? `已加入待发队列：「${val.slice(0, 24)}${val.length > 24 ? "…" : ""}」` : "已将后续消息加入待发队列");
        break;
      }
      case "toggle-frameless":
        state.frameless = !state.frameless;
        stage.classList.toggle("frameless", state.frameless);
        protoBar.hidden = state.frameless;
        break;
      case "toggle-switch":
        t.classList.toggle("on");
        t.setAttribute("aria-pressed", String(t.classList.contains("on")));
        break;
      case "seg-pick":
        t.parentElement?.querySelectorAll("button").forEach((b) => b.classList.toggle("active", b === t));
        break;
      case "approve-gate":
      case "deny-gate": {
        const approve = t.dataset.action === "approve-gate";
        const card = byId("chat-gate-card"); const done = byId("chat-gate-resolved"); const txt = byId("chat-gate-resolved-text");
        if (txt) txt.textContent = approve ? "✓ 已批准执行命令，智能体继续运行中…" : "✕ 已拒绝本次命令执行，智能体已等待你的进一步指令";
        if (done) {
          done.style.background = approve ? "var(--ok-soft)" : "var(--warn-soft)";
          done.style.borderColor = approve ? "var(--ok-border)" : "var(--warn-border)";
          done.style.color = approve ? "var(--ok)" : "var(--warn)";
        }
        if (card && done) { card.hidden = true; done.hidden = false; }
        ov()?.toast(approve ? "已批准执行 cargo clippy，继续运行中…" : "已拒绝执行命令", { tone: approve ? "ok" : "warn" });
        break;
      }
      case "reset-gate":
        if (byId("chat-gate-card")) byId("chat-gate-card").hidden = false;
        if (byId("chat-gate-resolved")) byId("chat-gate-resolved").hidden = true;
        break;
      case "toggle-reason": {
        const body = byId("reason-body");
        const lbl = byId("reason-toggle-lbl");
        if (body) {
          body.hidden = !body.hidden;
          if (lbl) lbl.textContent = body.hidden ? "展开 ▸" : "收起 ▾";
        }
        break;
      }
      case "apply-relay-pricing": {
        catalogUi.savedMult = (byId("prov-relay-mult")?.value || "").trim();
        catalogUi.savedCurr = byId("prov-pricing-curr-btn")?.dataset.curCurr || "";
        updateProviderPricingFormula();
        const fl = byId("prov-pricing-flash");
        if (fl) { fl.textContent = "已保存倍率与币种"; fl.hidden = false; }
        ov()?.toast("已保存服务商中转倍率与币种，模型列表生效价已同步更新");
        break;
      }
      case "reset-relay-pricing": {
        if (byId("prov-relay-mult")) byId("prov-relay-mult").value = catalogUi.savedMult;
        const cb = byId("prov-pricing-curr-btn");
        if (cb) { cb.dataset.curCurr = catalogUi.savedCurr; cb.textContent = `${catalogUi.savedCurr || "沿用目录币种"} ▾`; }
        updateProviderPricingFormula();
        break;
      }
    }
  });

  cmdModal?.addEventListener("click", (e) => { if (e.target === cmdModal) toggleCmd(false); });
  byId("chat-thread-scroll")?.addEventListener("scroll", (e) => {
    const el = e.currentTarget;
    const jump = byId("chat-jump-btn");
    if (!jump) return;
    const dist = el.scrollHeight - el.scrollTop - el.clientHeight;
    jump.hidden = dist <= 40;
  });

  const MODULE_ORDER = ["requests", "mock", "flows", "history", "environments", "chat", "runs", "providers", "gateway", "usage"];
  document.addEventListener("keydown", (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "k") { e.preventDefault(); toggleCmd(); return; }
    if (mod && e.key.toLowerCase() === "b") { e.preventDefault(); toggleRail(); return; }
    if (mod && e.key.toLowerCase() === "j") { e.preventDefault(); if (state.module === "requests") togglePanel("console-drawer"); return; }
    if (mod && e.key === ",") { e.preventDefault(); setModule("settings"); return; }
    if (mod && /^[1-9]$/.test(e.key)) { e.preventDefault(); setModule(MODULE_ORDER[Number(e.key) - 1]); return; }
    if (e.key === "Escape") {
      if (!cmdModal.hidden) { toggleCmd(false); return; }
      if (wirePop && !wirePop.hidden) { wirePop.hidden = true; return; }
      if (state.frameless) { state.frameless = false; stage.classList.remove("frameless"); protoBar.hidden = false; }
    }
  });

  // ── URL Query Params for Direct Preview & Automated Visual QA ─────────────
  const q = new URLSearchParams(location.search);
  ["palette", "mode", "density", "module", "req", "prov", "env", "usage", "set"].forEach((k) => {
    const v = q.get(k);
    if (!v) return;
    if (k === "req") state.reqMode = v;
    else if (k === "prov") state.provView = v;
    else if (k === "env") state.envTab = v;
    else if (k === "usage") state.usageView = v;
    else if (k === "set") state.setPage = v;
    else state[k] = v;
  });
  if (q.get("rail") === "1") { state.rail = true; appWin.classList.add("rail"); }
  if (q.get("frameless") === "1") { state.frameless = true; stage.classList.add("frameless"); protoBar.hidden = true; }

  applyTheme();
  setModule(state.module);
  setReqMode(state.reqMode);
  setProvView(state.provView);
  setEnvTab(state.envTab);
  setUsageView(state.usageView);
  setSettingsPage(state.setPage);
  renderFlowsWorkbench();

  if (q.get("tab")) { const [g, tb] = q.get("tab").split(":"); if (g && tb) setSubTab(g, tb); }
  if (q.get("console") === "1") togglePanel("console-drawer", true);
  if (q.get("details") === "1") togglePanel("chat-details-panel", true);
  if (q.get("modal")) ov()?.openDialog(q.get("modal"));
  if (q.get("drawer")) ov()?.openDrawer(q.get("drawer"));
})();
