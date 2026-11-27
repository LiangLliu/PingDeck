/* PingDeck Prototype — Unified Overlay Engine (Dialogs, Wizards, Drawers, Popovers, Toasts) */
window.PingDeckOverlays = (() => {
  const byId = (id) => document.getElementById(id);
  const dialogRoot = () => byId("dialog-root");
  const drawerRoot = () => byId("drawer-root");
  const popRoot = () => byId("popover-root");
  const toastRoot = () => byId("toast-root");

  const wizardState = { sniffStep: 1, connectTool: "claude", snippetLang: "curl", importTab: "import" };
  const rmState = {
    runIdx: 0,
    view: "graph",
    direction: "lr",
    align: "center",
    swimlanes: true,
    zoom: 1,
    selectedId: "root",
    pinned: {},
  };

  function renderRunMonitor() {
    const runs = window.PD?.AGENT_WORKFLOWS || [];
    const model = runs[rmState.runIdx] || runs[0];
    const stage = byId("rm-stage-main");
    const detail = byId("rm-node-detail");
    if (!model || !stage || !detail) return;
    if (!model.nodes.some((n) => n.id === rmState.selectedId)) {
      rmState.selectedId = model.nodes[0]?.id || null;
    }

    const lbl = byId("rm-run-label");
    if (lbl) lbl.textContent = model.runLabel;
    const kpiBox = byId("rm-kpis");
    if (kpiBox) {
      kpiBox.innerHTML = model.kpis
        .map((k) => `<span class="tag ${k.tone === "text-warn" ? "tag-warn" : k.tone === "text-ok" ? "tag-ok" : ""}"><b>${k.label}</b> ${k.val}</span>`)
        .join("");
    }
    const root = byId("rm-canvas-root");
    root?.querySelectorAll("[data-rm-view]").forEach((b) => b.classList.toggle("active", b.dataset.rmView === rmState.view));
    root?.querySelectorAll("[data-rm-dir]").forEach((b) => b.classList.toggle("active", b.dataset.rmDir === rmState.direction));
    const lanesBtn = byId("rm-toggle-lanes");
    if (lanesBtn) lanesBtn.classList.toggle("tag-accent", rmState.swimlanes);
    const alignBtn = byId("rm-toggle-align");
    if (alignBtn) {
      alignBtn.classList.toggle("tag-accent", rmState.align === "center");
      alignBtn.textContent = rmState.align === "center" ? "居中" : "顶对齐";
    }
    const resetBtn = byId("rm-reset-layout");
    if (resetBtn) resetBtn.hidden = Object.keys(rmState.pinned).length === 0;
    const zoomLbl = byId("rm-zoom-lbl");
    if (zoomLbl) zoomLbl.textContent = `${Math.round(rmState.zoom * 100)}%`;

    const pv = window.PD?.ProcessView;
    const opts = {
      instanceId: "rm",
      editable: false,
      onSelect: (id) => { rmState.selectedId = id; renderRunMonitor(); },
      onBlankClick: () => { rmState.selectedId = null; renderRunMonitor(); },
      onMutate: () => renderRunMonitor(),
      onLaneChange: (node, band) => toast(`已将「${node.title}」移入泳道「${band.label}」`),
    };

    if (rmState.view === "changes") {
      stage.innerHTML = `
        <div class="pv-steps-wrap">
          <div class="card-panel">
            <div class="card-panel-head">
              <span><b>crates/pingdeck-core/src/llm/router.rs</b></span>
              <span class="mono"><span class="text-ok">+18</span> <span class="text-bad">-4</span></span>
            </div>
            <div class="diff-box" style="margin-top:0">
              <div class="diff-head"><span>@@ -212,7 +212,8 @@ impl Router</span><button type="button" class="btn btn-sm" data-toast="已从编辑前快照撤销 router.rs 改动">撤销此文件改动</button></div>
              <div class="diff-row">  214   match status {</div>
              <div class="diff-row del">- 215     429 | 500..=599 =&gt; breaker.trip(target, Duration::from_secs(60)),</div>
              <div class="diff-row add">+ 215     429 =&gt; honor_retry_after(&amp;headers, MAX_RETRY_WAIT),</div>
              <div class="diff-row add">+ 216     500..=599 =&gt; breaker.record_failure(target, FAILURES_BEFORE_TRIP),</div>
            </div>
          </div>
        </div>`;
    } else if (rmState.view === "steps") {
      pv?.renderSteps(stage, model, rmState, opts);
    } else if (rmState.view === "timeline") {
      pv?.renderTimeline(stage, model, rmState, opts);
    } else {
      pv?.renderGraph(stage, model, rmState, opts);
    }

    const selNode = model.nodes.find((n) => n.id === rmState.selectedId);
    detail.hidden = !selNode || rmState.view === "changes";
    if (selNode) {
      const toneCls = selNode.status === "warn" ? "tag-warn" : selNode.status === "bad" ? "tag-bad" : "tag-ok";
      const laneName = model.lanes?.find((l) => l.id === selNode.lane)?.label || "默认";
      detail.innerHTML = `
        <div class="pane-head">
          <span class="pane-title truncate" style="font-size:13px">${selNode.title}</span>
          <button type="button" class="btn-icon" data-rm-close-detail title="关闭详情">×</button>
        </div>
        <div class="pv-inspector-body">
          <div class="row-between"><span class="tag ${toneCls}">${selNode.metric || "已完成"}</span><span class="tag">泳道：${laneName}</span></div>
          <div class="card-panel">
            <div class="card-panel-head"><span>执行信息</span></div>
            <div class="fs-xs text-muted">模型：<code class="text-ink1">${selNode.model || "—"}</code></div>
            <div class="fs-xs text-muted">统计：<b class="mono text-ink1">${selNode.meta || selNode.metric || "—"}</b></div>
          </div>
          <div class="card-panel">
            <div class="card-panel-head"><span>节点摘要与日志</span></div>
            <div class="code-box fs-2xs" style="white-space:pre-wrap">${selNode.log || "无日志"}</div>
          </div>
          ${selNode.status === "warn" ? `<button type="button" class="btn btn-sm btn-primary" data-action="close-dialog" data-nav-to="chat">前往对话关卡区审批 →</button>` : ""}
        </div>`;
    }
  }

  function toast(msg, opts = {}) {
    const { tone = "ok", undo } = opts;
    const host = toastRoot();
    if (!host) return;
    const el = document.createElement("div");
    el.className = `toast-item ${tone}`;
    el.innerHTML = `<span class="dot dot-${tone === "bad" ? "bad" : tone === "warn" ? "warn" : "ok"}"></span><span class="toast-msg">${msg}</span>`;
    if (undo) {
      const btn = document.createElement("button");
      btn.className = "btn btn-sm";
      btn.textContent = "撤销";
      btn.onclick = () => { undo(); el.remove(); };
      el.appendChild(btn);
    }
    const x = document.createElement("button");
    x.className = "btn-icon";
    x.textContent = "×";
    x.onclick = () => el.remove();
    el.appendChild(x);
    host.appendChild(el);
    setTimeout(() => el.remove(), undo ? 6500 : 3200);
  }

  const closeAllPopovers = () => { const r = popRoot(); if (r) r.innerHTML = ""; };
  const closeDialog = () => { const r = dialogRoot(); if (r) r.innerHTML = ""; };
  const closeDrawer = () => { const r = drawerRoot(); if (r) r.innerHTML = ""; };

  function openPopover(id, anchorEl, ctx = {}) {
    closeAllPopovers();
    const root = popRoot();
    const render = window.PD?.POPOVER_TEMPLATES?.[id];
    if (!root || !anchorEl || !render) return;

    const rect = anchorEl.getBoundingClientRect();
    const box = document.createElement("div");
    box.className = "pop-box";
    box.innerHTML = render(ctx);
    root.appendChild(box);

    const bw = box.offsetWidth || 270;
    const bh = box.offsetHeight || 220;
    let left = Math.min(rect.left, window.innerWidth - bw - 12);
    let top = rect.bottom + 6;
    if (top + bh > window.innerHeight - 12) top = Math.max(12, rect.top - bh - 6);
    if (left < 12) left = 12;
    box.style.left = `${left}px`;
    box.style.top = `${top}px`;
  }

  function bindConfirmGate(checkboxId, buttonId) {
    const ack = byId(checkboxId);
    const btn = byId(buttonId);
    if (ack && btn) ack.onchange = () => { btn.disabled = !ack.checked; };
  }

  function openDialog(id) {
    closeAllPopovers();
    const cmd = byId("cmd-modal");
    if (cmd) cmd.hidden = true;
    const wire = byId("wire-popover");
    if (wire) wire.hidden = true;

    const spec = window.PD?.renderDialogSpec?.(id, wizardState);
    const root = dialogRoot();
    if (!spec || !root) return;

    const existing = root.querySelector(`.dialog-box[data-dialog-id="${id}"]`);
    const titleHtml = `${spec.tone === "danger" ? `<span class="dot dot-bad"></span>` : spec.tone === "warn" ? `<span class="dot dot-warn"></span>` : ""}${spec.title}`;
    if (existing) {
      existing.className = `dialog-box ${spec.size || ""} ${spec.tone || ""}`;
      const tEl = existing.querySelector(".dialog-title");
      const bEl = existing.querySelector(".dialog-body");
      const fEl = existing.querySelector(".dialog-foot");
      if (tEl) tEl.innerHTML = titleHtml;
      if (bEl) bEl.innerHTML = spec.body;
      if (fEl) fEl.innerHTML = spec.foot;
    } else {
      root.innerHTML = `
        <div class="dialog-scrim" data-scrim="dialog">
          <div class="dialog-box ${spec.size || ""} ${spec.tone || ""}" role="dialog" aria-modal="true" data-dialog-id="${id}">
            <div class="dialog-head">
              <span class="dialog-title">${titleHtml}</span>
              <button class="btn-icon" data-action="close-dialog" title="关闭 (Esc)">×</button>
            </div>
            <div class="dialog-body">${spec.body}</div>
            <div class="dialog-foot">${spec.foot}</div>
          </div>
        </div>`;
    }
    bindConfirmGate("full-access-ack", "full-access-confirm");
    bindConfirmGate("secret-export-ack", "secret-export-confirm");
    if (id === "run-monitor") renderRunMonitor();
  }

  function openDrawer(id) {
    closeAllPopovers();
    const wire = byId("wire-popover");
    if (wire) wire.hidden = true;
    const root = drawerRoot();
    const spec = window.PD?.DRAWER_SPECS?.[id];
    if (!root || !spec) return;

    root.innerHTML = `
      <div class="drawer-scrim" data-scrim="drawer">
        <div class="drawer-box ${spec.size || ""}" role="dialog" aria-label="${spec.title}">
          <div class="dialog-head">
            <div class="col-stack-sm" style="gap:2px;min-width:0">
              <span class="dialog-title">${spec.title}</span>
              ${spec.subtitle ? `<span class="fs-xs text-muted">${spec.subtitle}</span>` : ""}
            </div>
            <button class="btn-icon" data-action="close-drawer">×</button>
          </div>
          <div class="dialog-body">${spec.body}</div>
          ${spec.foot ? `<div class="dialog-foot">${spec.foot}</div>` : ""}
        </div>
      </div>`;
  }

  document.addEventListener("click", (e) => {
    const scrim = e.target.closest("[data-scrim]");
    if (scrim && e.target === scrim) {
      if (scrim.dataset.scrim === "dialog") closeDialog();
      if (scrim.dataset.scrim === "drawer") closeDrawer();
      return;
    }

    const t = e.target.closest("[data-modal], [data-drawer], [data-popover], [data-action], [data-dialog-done], [data-dialog-undo], [data-drawer-done], [data-pop-toast], [data-pick-method], [data-pick-env], [data-pick-perm], [data-pick-model], [data-pick-ai-proto], [data-pick-range], [data-pick-which], [data-pick-proxy], [data-pick-basis], [data-pick-curr], [data-pick-unlisted-prov], [data-pick-flow-run], [data-rm-view], [data-rm-dir], [data-rm-toggle], [data-rm-reset], [data-rm-zoom], [data-rm-run-prev], [data-rm-run-next], [data-rm-close-detail], [data-sniff-step], [data-sniff-preset], [data-connect-tool], [data-snippet-lang], [data-import-tab], [data-toast]");
    if (!t) {
      if (!e.target.closest(".pop-box")) closeAllPopovers();
      return;
    }

    if (t.dataset.popover) { e.stopPropagation(); openPopover(t.dataset.popover, t); return; }
    if (t.dataset.modal) {
      if (t.dataset.modal === "add-provider") wizardState.sniffStep = 1;
      if (t.dataset.modal === "import-export") wizardState.importTab = "import";
      if (t.dataset.modal === "run-monitor") {
        if (t.dataset.rmRunIdx !== undefined) rmState.runIdx = Number(t.dataset.rmRunIdx) || 0;
        rmState.view = t.dataset.rmInitialTab || "graph";
        rmState.pinned = {};
      }
      openDialog(t.dataset.modal);
      return;
    }
    if (t.dataset.drawer) { openDrawer(t.dataset.drawer); return; }
    if (t.dataset.sniffStep) { wizardState.sniffStep = Number(t.dataset.sniffStep); openDialog("add-provider"); return; }
    if (t.dataset.sniffPreset) {
      const inp = byId("sniff-base-url");
      if (inp) inp.value = t.dataset.sniffPreset;
      t.parentElement?.querySelectorAll("[data-sniff-preset]").forEach((b) => b.classList.toggle("tag-accent", b === t));
      return;
    }
    if (t.dataset.connectTool) { wizardState.connectTool = t.dataset.connectTool; openDialog("connect-client"); return; }
    if (t.dataset.snippetLang) { wizardState.snippetLang = t.dataset.snippetLang; openDialog("code-snippets"); return; }
    if (t.dataset.importTab) { wizardState.importTab = t.dataset.importTab; openDialog("import-export"); return; }

    if (t.dataset.dialogDone) {
      if (t.dataset.resolveGate) {
        const card = byId("chat-gate-card");
        const done = byId("chat-gate-resolved");
        if (card && done) { card.hidden = true; done.hidden = false; }
      }
      if (t.dataset.setFullAccess) {
        const chip = byId("chat-perm-chip");
        if (chip) { chip.className = "tag tag-warn"; chip.textContent = "▲ 完全权限 ▾"; }
      }
      closeDialog();
      toast(t.dataset.dialogDone, { tone: t.dataset.toastTone || "ok" });
      return;
    }
    if (t.dataset.dialogUndo) {
      closeDialog();
      toast(t.dataset.dialogUndo, { tone: "ok", undo: () => toast("已撤销刚才的重置操作") });
      return;
    }
    if (t.dataset.drawerDone) { closeDrawer(); toast(t.dataset.drawerDone); return; }
    if (t.dataset.popToast) { closeAllPopovers(); toast(t.dataset.popToast); return; }
    if (t.dataset.toast) { toast(t.dataset.toast, { tone: t.dataset.toastTone || "ok" }); return; }

    if (t.dataset.pickMethod) {
      const el = byId("req-method-label");
      if (el) { el.className = `m-${t.dataset.pickMethod.toLowerCase()}`; el.textContent = t.dataset.pickMethod; }
      closeAllPopovers();
      return;
    }
    if (t.dataset.pickEnv) {
      const el = byId("req-env-label");
      const dot = byId("req-env-dot");
      if (el) el.textContent = t.dataset.pickEnv;
      if (dot) dot.className = `dot dot-${t.dataset.envDot || "ok"}`;
      closeAllPopovers();
      toast(`已切换环境为 ${t.dataset.pickEnv}`);
      return;
    }
    if (t.dataset.pickPerm) {
      const chip = byId("chat-perm-chip");
      if (chip) {
        chip.className = t.dataset.permTone ? `tag tag-${t.dataset.permTone}` : "tag";
        chip.textContent = `${t.dataset.pickPerm} ▾`;
      }
      closeAllPopovers();
      return;
    }
    if (t.dataset.pickModel) {
      const el = byId("chat-model-label");
      if (el) el.textContent = `38% · ${t.dataset.pickModel}`;
      closeAllPopovers();
      toast(`已切换模型为 ${t.dataset.pickModel}`);
      return;
    }
    if (t.dataset.pickAiProto) {
      const el = byId("req-ai-proto-label");
      if (el) el.textContent = t.dataset.pickAiProto;
      closeAllPopovers();
      return;
    }
    if (t.dataset.pickRange) {
      const el = byId("usage-range-btn");
      if (el) el.textContent = `时间范围 ${t.dataset.pickRange} ▾`;
      closeAllPopovers();
      return;
    }
    if (t.dataset.pickProxy) {
      const btn = byId("tb-proxy-btn");
      const dot = byId("tb-proxy-dot");
      if (btn) btn.title = `全局代理：${t.dataset.pickProxy}（HTTP · 流程 · AI）`;
      if (dot) {
        dot.classList.remove("dot-ok", "dot-accent", "dot-idle");
        const tone = t.dataset.proxyTone;
        dot.classList.add(tone === "accent" ? "dot-accent" : tone === "idle" ? "dot-idle" : "dot-ok");
      }
      closeAllPopovers();
      toast(`已切换全局代理：${t.dataset.pickProxy}`);
      return;
    }
    if (t.dataset.pickWhich) {
      t.closest(".which-list")?.querySelectorAll(".which-opt").forEach((b) => b.classList.toggle("active", b === t));
      toast(`已关联 gemini-4-neon → ${t.dataset.pickWhich}（仅对 relay.acme.dev 生效）`);
      return;
    }
    if (t.dataset.pickUnlistedProv !== undefined) {
      const prov = t.dataset.pickUnlistedProv;
      if (window.PD?.catalogUi) window.PD.catalogUi.unlistedProv = prov;
      const btn = byId("cat-unlisted-prov-btn");
      if (btn) btn.textContent = `${prov || "全部服务商"} ▾`;
      window.PD?.filterUnlistedRows?.();
      closeAllPopovers();
      return;
    }
    if (t.dataset.pickBasis !== undefined) {
      const basis = t.dataset.pickBasis;
      const lbl = t.dataset.basisLabel || (basis || "跟随官方价");
      const btn = byId("prov-pricing-basis-btn");
      if (btn) { btn.dataset.curBasis = basis; btn.textContent = `${lbl} ▾`; }
      const cov = byId("prov-pricing-cov");
      if (cov) {
        cov.textContent = basis
          ? `${lbl} 目录价覆盖 32 / 34 个启用模型，其余按官方价`
          : "34 个启用模型 · 跟随官方目录价";
      }
      const sug = byId("prov-pricing-sug-banner");
      if (sug) sug.hidden = basis === "openrouter";
      const fl = byId("prov-pricing-flash");
      if (fl) { fl.textContent = "已更新目录基准价"; fl.hidden = false; }
      window.PD?.updateProviderPricingFormula?.();
      closeAllPopovers();
      toast(`已将目录基准价切换为「${lbl}」，生效价已重算`);
      return;
    }
    if (t.dataset.pickCurr !== undefined) {
      const curr = t.dataset.pickCurr;
      const lbl = t.dataset.currLabel || (curr || "沿用目录币种");
      const btn = byId("prov-pricing-curr-btn");
      if (btn) { btn.dataset.curCurr = curr; btn.textContent = `${lbl} ▾`; }
      window.PD?.updateProviderPricingFormula?.();
      closeAllPopovers();
      return;
    }
    if (t.dataset.pickFlowRun !== undefined) {
      window.PD?.flowUi?.selectRun?.(Number(t.dataset.pickFlowRun));
      closeAllPopovers();
      return;
    }
    if (t.dataset.rmView) { rmState.view = t.dataset.rmView; renderRunMonitor(); return; }
    if (t.dataset.rmDir) { rmState.direction = t.dataset.rmDir; renderRunMonitor(); return; }
    if (t.dataset.rmToggle === "lanes") { rmState.swimlanes = !rmState.swimlanes; renderRunMonitor(); return; }
    if (t.dataset.rmToggle === "align") { rmState.align = rmState.align === "center" ? "top" : "center"; renderRunMonitor(); return; }
    if (t.dataset.rmReset !== undefined) { rmState.pinned = {}; renderRunMonitor(); toast("已恢复自动排版"); return; }
    if (t.dataset.rmZoom) { rmState.zoom = Math.min(1.6, Math.max(0.55, +(rmState.zoom + Number(t.dataset.rmZoom)).toFixed(2))); renderRunMonitor(); return; }
    if (t.dataset.rmRunPrev !== undefined) {
      const n = (window.PD?.AGENT_WORKFLOWS || []).length || 1;
      rmState.runIdx = (rmState.runIdx + n - 1) % n;
      rmState.pinned = {};
      renderRunMonitor();
      return;
    }
    if (t.dataset.rmRunNext !== undefined) {
      const n = (window.PD?.AGENT_WORKFLOWS || []).length || 1;
      rmState.runIdx = (rmState.runIdx + 1) % n;
      rmState.pinned = {};
      renderRunMonitor();
      return;
    }
    if (t.dataset.rmCloseDetail !== undefined) { rmState.selectedId = null; renderRunMonitor(); return; }

    switch (t.dataset.action) {
      case "close-dialog": closeDialog(); break;
      case "close-drawer": closeDrawer(); break;
      case "run-mock-try": {
        const list = byId("mock-traffic-list");
        if (list) {
          const row = document.createElement("div");
          row.className = "mock-hit-row";
          row.dataset.drawer = "mock-hit-detail";
          row.innerHTML = `<span class="mono text-accent">刚刚</span><span class="m-post">POST</span><span class="mono">/refunds</span><span class="status-mark"><span class="dot dot-ok"></span><b class="text-ok">201</b><span class="dot dot-ok" style="margin-left:6px"></span><span>回调 200</span></span><span class="text-muted">试一下测试命中，响应 201</span><span class="mono text-right text-ink2">124 ms</span><div class="prog-track"><div class="prog-fill" style="width:70%"></div></div>`;
          list.prepend(row);
        }
        toast("已发送本地 Mock 测试，命中 201 Created + 回调 200");
        break;
      }
    }
  });

  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      if (dialogRoot()?.innerHTML) { closeDialog(); e.stopImmediatePropagation(); return; }
      if (drawerRoot()?.innerHTML) { closeDrawer(); e.stopImmediatePropagation(); return; }
      if (popRoot()?.innerHTML) { closeAllPopovers(); e.stopImmediatePropagation(); return; }
    }
  }, true);

  return { openDialog, openDrawer, openPopover, closeAllPopovers, closeDialog, closeDrawer, toast };
})();
