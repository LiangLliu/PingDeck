/* PingDeck Prototype — Unified Overlay Engine (Dialogs, Wizards, Drawers, Popovers, Toasts) */
window.PingDeckOverlays = (() => {
  const byId = (id) => document.getElementById(id);
  const dialogRoot = () => byId("dialog-root");
  const drawerRoot = () => byId("drawer-root");
  const popRoot = () => byId("popover-root");
  const toastRoot = () => byId("toast-root");

  const wizardState = { sniffStep: 1, connectTool: "claude", snippetLang: "curl", importTab: "import" };

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

    const t = e.target.closest("[data-modal], [data-drawer], [data-popover], [data-action], [data-dialog-done], [data-dialog-undo], [data-drawer-done], [data-pop-toast], [data-pick-method], [data-pick-env], [data-pick-perm], [data-pick-model], [data-pick-ai-proto], [data-pick-range], [data-pick-which], [data-pick-proxy], [data-sniff-step], [data-sniff-preset], [data-connect-tool], [data-snippet-lang], [data-import-tab], [data-toast]");
    if (!t) {
      if (!e.target.closest(".pop-box")) closeAllPopovers();
      return;
    }

    if (t.dataset.popover) { e.stopPropagation(); openPopover(t.dataset.popover, t); return; }
    if (t.dataset.modal) {
      if (t.dataset.modal === "add-provider") wizardState.sniffStep = 1;
      if (t.dataset.modal === "import-export") wizardState.importTab = "import";
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
