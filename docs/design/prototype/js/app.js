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
    module: "requests", reqMode: "http", provView: "gateway",
    envName: "Staging", envTab: "vars", usageView: "overview", usageScope: "全部服务商", setPage: "appearance",
    palette: "slate", mode: "light", density: "comfortable", rail: false, frameless: false,
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
        dynEl.innerHTML = `<p class="set-lede">没有标注的设置立即生效，并随同步文件夹同步。只有例外才标出来。</p>${info.html()}`;
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
    if (byId("prov-gateway-view")) byId("prov-gateway-view").hidden = view !== "gateway";
    if (byId("prov-relay-view")) byId("prov-relay-view").hidden = view !== "relay";
    document.querySelectorAll("[data-prov-view]").forEach((el) => {
      const on = clickedEl ? el === clickedEl : (view === "gateway" ? el.dataset.provView === "gateway" : el.dataset.provPrimary === "relay");
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

  function selectFlowStep(stepId, stepEl) {
    document.querySelectorAll("[data-flow-step]").forEach((s) => s.classList.toggle("selected", s === stepEl));
    const s = FLOW_STEPS[stepId];
    if (!s || !byId("flow-insp-inputs") || !byId("flow-insp-step")) return;
    byId("flow-insp-inputs").hidden = true;
    byId("flow-insp-step").hidden = false;
    if (byId("flow-insp-title")) byId("flow-insp-title").textContent = `${s.name} · 第 ${s.n} 步`;
    if (byId("flow-step-src")) byId("flow-step-src").innerHTML = `<span class="m-${s.method.toLowerCase()}">${s.method}</span> <code class="text-ink1">${s.path}</code>`;
    if (byId("flow-step-extract")) byId("flow-step-extract").textContent = s.extract;
    if (byId("flow-step-assert")) byId("flow-step-assert").textContent = s.assert;
    if (byId("flow-step-retry")) byId("flow-step-retry").textContent = s.retry;
    if (byId("flow-step-status")) byId("flow-step-status").textContent = s.status;
    if (byId("flow-step-body")) byId("flow-step-body").textContent = s.body;
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

  document.addEventListener("input", (e) => {
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

    const flowStep = e.target.closest("[data-flow-step]");
    if (flowStep) { selectFlowStep(flowStep.dataset.flowStep, flowStep); return; }

    const t = e.target.closest("button, tr[data-usage-view], [data-nav-to], [data-req-mode], [data-prov-view], [data-env-tab], [data-env-pick], [data-usage-view], [data-usage-scope], [data-tab-jump], [data-tab], [data-mock-server], [data-set-page], [data-set-palette], [data-set-mode], [data-set-density], [data-action]");
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

    switch (t.dataset.action) {
      case "toggle-rail": toggleRail(); break;
      case "open-cmd": toggleCmd(true); break;
      case "close-cmd": toggleCmd(false); break;
      case "toggle-console": togglePanel("console-drawer"); break;
      case "toggle-chat-details": togglePanel("chat-details-panel"); break;
      case "toggle-flow-inspector": togglePanel("flow-inspector"); break;
      case "show-flow-inputs":
        document.querySelectorAll("[data-flow-step]").forEach((s) => s.classList.remove("selected"));
        if (byId("flow-insp-inputs")) byId("flow-insp-inputs").hidden = false;
        if (byId("flow-insp-step")) byId("flow-insp-step").hidden = true;
        if (byId("flow-insp-title")) byId("flow-insp-title").textContent = "流程输入与会话";
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
    }
  });

  cmdModal?.addEventListener("click", (e) => { if (e.target === cmdModal) toggleCmd(false); });

  const MODULE_ORDER = ["requests", "mock", "flows", "history", "environments", "chat", "providers", "usage"];
  document.addEventListener("keydown", (e) => {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "k") { e.preventDefault(); toggleCmd(); return; }
    if (mod && e.key.toLowerCase() === "b") { e.preventDefault(); toggleRail(); return; }
    if (mod && e.key.toLowerCase() === "j") { e.preventDefault(); if (state.module === "requests") togglePanel("console-drawer"); return; }
    if (mod && e.key === ",") { e.preventDefault(); setModule("settings"); return; }
    if (mod && /^[1-8]$/.test(e.key)) { e.preventDefault(); setModule(MODULE_ORDER[Number(e.key) - 1]); return; }
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

  if (q.get("tab")) { const [g, tb] = q.get("tab").split(":"); if (g && tb) setSubTab(g, tb); }
  if (q.get("console") === "1") togglePanel("console-drawer", true);
  if (q.get("details") === "1") togglePanel("chat-details-panel", true);
  if (q.get("modal")) ov()?.openDialog(q.get("modal"));
  if (q.get("drawer")) ov()?.openDrawer(q.get("drawer"));
})();
