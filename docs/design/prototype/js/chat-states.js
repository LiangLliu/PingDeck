/* SpanBay Prototype — Agent Chat Redesign Controller & Interactions (.overhaul/chat-redesign-spec.md).
   Drives all 16 review states, right review panel (diff, per-hunk revert, line comments),
   editable queue bar, transcript view mode (Compact/Normal/Verbose), and keyboard shortcuts. */
(() => {
  const CS = window.PD.CS;
  const { t, banner, frag } = CS;
  const $ = (s, r = document) => r.querySelector(s);
  const ov = () => window.SpanBayOverlays;
  const mod = $('.module-view[data-module="chat"]');
  if (!mod) return;

  const el = {
    head: $(".chat-head", mod),
    list: $('[data-select-list="chat-threads"]', mod),
    search: $(".pane-search input", mod),
    scroller: $("#chat-thread-scroll"),
    col: $("#chat-thread-scroll .chat-col"),
    dock: $(".chat-dock", mod),
    composer: $("#chat-composer-input"),
    chips: $("#chat-composer-chips"),
    editBanner: $("#chat-edit-banner"),
    permBtn: $("#chat-perm-chip"),
    ctxSlot: $("#cs-ctx-slot"),
    modelBtn: $("#chat-model-label"),
    sendSlot: $("#chat-send-slot"),
    reviewPanel: $("#chat-review-panel"),
    jump: $("#chat-jump-btn"),
    bar: $("#cs-bar"),
    select: $("#cs-state-select"),
  };
  const defaultHeadHtml = el.head.innerHTML;
  const defaultListHtml = el.list.innerHTML;

  const dock = $("#cs-dock", el.dock) || (() => {
    const d = document.createElement("div");
    d.id = "cs-dock";
    d.className = "cs-dock";
    el.dock.insertBefore(d, $(".composer-card", el.dock));
    return d;
  })();

  let current = null;
  let variant = null;
  let timers = [];
  let revise = false;
  let earlierPages = 0;
  let viewMode = "normal"; // "compact" | "normal" | "verbose"

  // Interactive Review & Composer state
  let rvState = {
    open: false,
    selectedFile: 0,
    scope: "turn",
    diffMode: "net",
    revertedHunks: {},
    fileStates: ["pending", "pending"],
    commentingLine: null,
  };
  let queueItems = [
    { id: "q1", text: "顺便检查一下 retry_after 超过 u32::MAX 时的饱和边界处理" },
  ];
  let editingQueueIdx = null;
  let composerChips = [];

  const clearTimers = () => { timers.forEach(clearInterval); timers = []; };

  // ── Older turns for `history` state (auto-collapsed into 1-line "Worked for Ns") ─
  const OLDER_TURNS = [
    {
      id: "h1",
      q: "网关日志里 429 集中在哪个时段？是不是单客户端打满的？",
      fold: () => t("turn.fold_ro", { d: "3.2s", f: 2, s: 2 }),
      spine: () => frag.exploreGroup({ id: "exp-h1", files: 2, searches: 2 }),
      a: "11 月 26 日 14:00–15:00 占全天 429 的 70%，来自 Claude Code 与 Codex 两个本地客户端并发请求。",
      cost: "3.2s · 8.1k token · $0.007",
    },
    {
      id: "h2",
      q: "Retry-After 解析需要同时支持秒数和 HTTP-date 吗？",
      fold: () => t("turn.fold_ro", { d: "2.1s", f: 1, s: 1 }),
      spine: () => frag.exploreGroup({ id: "exp-h2", files: 1, searches: 1 }),
      a: "需要。RFC 9110 §10.2.3 允许 delta-seconds 或 HTTP-date 两种格式，上游中转站两种都会返回。",
      cost: "2.1s · 6.4k token · $0.005",
    },
    {
      id: "h3",
      q: "单次退避等待上限 15 秒是在哪里配置的？",
      fold: () => t("turn.fold_ro", { d: "1.8s", f: 2, s: 1 }),
      spine: () => frag.exploreGroup({ id: "exp-h3", files: 2, searches: 1 }),
      a: "在「设置 › AI › 网关」的 <code>ai.gateway.max_retry_wait_s</code>，默认值为 15 秒。",
      cost: "1.8s · 5.9k token · $0.005",
    },
    {
      id: "h4",
      q: "流式输出开始后如果遇到中断，还会切备用链吗？",
      fold: () => t("turn.fold_ro", { d: "1.5s", f: 1, s: 0 }),
      spine: () => frag.exploreGroup({ id: "exp-h4", files: 1, searches: 0 }),
      a: "不会。首个内容事件发出后即锁定当前目标，这是网关的流式提交点（PRD §4）。",
      cost: "1.5s · 4.2k token · $0.004",
    },
  ];
  const EARLIER_TURNS = [
    {
      id: "h0a",
      q: "先看看网关日志每条记录的字段结构",
      fold: () => t("turn.fold_ro", { d: "1.2s", f: 1, s: 0 }),
      spine: () => frag.exploreGroup({ id: "exp-h0a", files: 1, searches: 0 }),
      a: "每行一个 JSON：<code>ts</code>、<code>client_id</code>、<code>target</code>、<code>status</code>、<code>ttft_ms</code> 与 <code>retry_after</code>。",
      cost: "1.2s · 3.1k token · $0.003",
    },
    {
      id: "h0b",
      q: "当前启用了哪几个备用模型？",
      fold: () => t("turn.fold_ro", { d: "0.9s", f: 1, s: 1 }),
      spine: () => frag.exploreGroup({ id: "exp-h0b", files: 1, searches: 1 }),
      a: "主模型 <code>deepseek-v4-pro</code>，一级备用 <code>deepseek-v4-flash</code>，二级备用 <code>kimi-k2.6</code>。",
      cost: "0.9s · 2.8k token · $0.002",
    },
  ];

  const renderHistoryTurn = (item) => `
    <div class="turn-block">
      ${frag.userTurn(item.q, { id: `u-${item.id}` })}
      ${frag.asstTurn({
        id: item.id,
        dur: item.cost.split(" · ")[0],
        collapsed: true,
        foldSummary: item.fold(),
        spineHtml: item.spine(),
        mdHtml: `<div class="cs-md"><p>${item.a}</p></div>`,
        costTxt: item.cost,
      })}
    </div>`;

  // ── Error Banner helper ────────────────────────────────────────────────────
  const FAIL = {
    all_targets_failed: ["deepseek-v4-pro · 429 Too Many Requests（retry-after 2，重试 2 次）", "kimi-k2.6 · 30 秒内无首字节响应"],
    key_rejected: ["DeepSeek · 401 invalid_api_key（密钥 sk-…7f3c 已失效或轮换）"],
    refused_or_empty: ["deepseek-v4-pro · finish_reason=content_filter，返回内容为空"],
    stalled: ["deepseek-v4-pro · 首字节后 90 秒无后续字节，自动重试一次仍停滞"],
    mcp_needs_token: ["linear · 401 Unauthorized（请在设置 › MCP 中补充凭据）"],
    context_full: ["deepseek-v4-pro · 请求 131,204 token 超出 128,000 窗口上限"],
    offline: ["api.deepseek.com · DNS 解析失败（本机网络已断开）"],
    generic: ["store · database is locked（SQLite 繁忙，请稍后重试）"],
  };
  const errorBanner = (kind) => {
    const k = FAIL[kind] ? kind : "all_targets_failed";
    return `
      <div id="cs-err" class="col-stack-sm" style="margin-top:4px">
        ${banner("bad", `<b>${t(`fail.${k}`)}</b>`, `
          <button class="btn btn-sm" data-cs="toggle" data-cs-target="cs-err-detail">${t("common.details")}</button>
          <button class="btn btn-sm btn-primary" data-cs="retry">${t("common.retry")}</button>
          <button class="btn-icon" data-cs="dismiss-error" title="${t("common.close")}">×</button>`)}
        <div id="cs-err-detail" class="cs-detail bad" style="margin-left:0" hidden>
          ${FAIL[k].map((x) => `<span class="mono fs-xs">${x}</span>`).join("")}
          <span class="fs-2xs text-muted">技术原文与尝试链仅在此处展开；外层横幅只显示本地化结论。</span>
        </div>
      </div>`;
  };

  // ── 16 Redesigned States Catalogue ─────────────────────────────────────────
  const STATES = [
    // 1. Idle Welcome
    {
      id: "welcome",
      group: "1 核心对话流",
      title: "1. 空闲欢迎态（Idle Welcome）",
      ref: "spec §2.3 · §3",
      note: "展示当前绑定的工作区/仓库上下文与 3 张任务启动卡；点击卡片填入输入框",
      thread: () => CS.welcomeHtml(),
    },

    // 2. Running Turn (Live Exploring, Thinking, Working Timer, Stop, Editable Queue/Steer)
    {
      id: "running",
      group: "1 核心对话流",
      title: "2. 运行中回合：实时探索组、思考、计时与插队/可编辑队列",
      ref: "spec §3 · §8",
      note: "连续只读实时聚合为「正在探索…」；待发队列支持点「编辑 ✎」载入输入框原位修改，或点「立即插队 ⌘↵」注入当前轮",
      variants: [["streaming", "流式生成中 + 待发队列"], ["thinking", "刚发送（思考中…）"]],
      running: true,
      thread: (v) => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          ${v === "thinking"
            ? `<div class="cs-thinking">${CS.dot("run")}<span>${t("thread.thinking")}</span><span class="mono text-faint">· ${t("run.step", { steps: 1, s: 1 })}</span></div>`
            : `
              <div class="asst-turn" id="cs-running-turn">
                <div class="asst-turn-head">
                  <span class="asst-model">deepseek-v4-pro</span>
                  <span class="asst-effort">低推理</span>
                  <span class="mono text-faint">· ${t("run.step", { steps: 4, s: 12 })}</span>
                </div>
                <div class="asst-spine" id="cs-running-spine">
                  ${frag.thinkRow("th-run", "1.4", "先读取网关日志与 router.rs 的 429 处理分支，确认是否与 5xx 共用了同一个熔断器。")}
                  ${frag.exploreGroup({ id: "exp-run", running: true, files: 2, searches: 1, open: true })}
                  ${frag.shellRow({
                    id: "sh-run-live",
                    st: "running",
                    cmd: "cargo test -p spanbay-core router",
                    dur: "4.2s",
                    lines: [["out", "   Compiling spanbay-core v0.9.0"], ["out", "    Running unittests src/lib.rs (target/debug/deps/spanbay_core)"]],
                  })}
                </div>
                <div class="cs-md"><p><span id="cs-stream-text">正在验证拆开后的 429 Retry-After 退避分支，</span><span class="cs-caret"></span></p></div>
              </div>`}
        </div>`,
      dock: (v) =>
        v === "thinking"
          ? ""
          : `${CS.queueBarHtml(queueItems, false)}${CS.workStripHtml({ steps: 4, secs: 12, showTodos: true, todosOpen: false })}`,
      after: (v) => { if (v !== "thinking") streamText(); },
    },

    // 3. Finished Multi-Step Turn (Expanded vs Auto-Collapsed)
    {
      id: "turn",
      group: "1 核心对话流",
      title: "3. 已完成多步回合：步骤合并、单次模型头、展开 vs 收拢",
      ref: "spec §3 · §4 · §5",
      note: "一轮只有一个模型头、一张变更卡、一条悬停操作栏；切换变体或顶栏「紧凑/常规/详细」对比折叠效果",
      variants: [["expanded", "常规（当前轮展开步骤）"], ["collapsed", "紧凑（收拢为已工作 11.4s ›）"], ["verbose", "详细（全展开含内联 Diff）"]],
      thread: (v) => {
        const isCollapsed = v === "collapsed" || viewMode === "compact";
        const isVerbose = v === "verbose" || viewMode === "verbose";
        return `
          <div class="turn-block">
            ${frag.userWithAtts()}
            ${frag.asstTurn({
              id: "t-main",
              collapsed: isCollapsed,
              foldSummary: t("turn.fold", { d: "11.4s", e: 2, f: 3, s: 2 }),
              spineHtml: frag.fullSpine(isVerbose),
              mdHtml: frag.markdownAnswer(),
              sourcesHtml: frag.sourcesRow(),
              changesHtml: frag.changesSummaryCard(),
            })}
          </div>`;
      },
    },

    // 4. Edit/Diff Summary + Right Review Panel Open (Per-Hunk Revert, Line Comments, Safe Undo)
    {
      id: "review",
      group: "2 代码审查与工具",
      title: "4. 变更汇总卡 + 右侧审查面板（逐块回滚 & 行内批注 & 安全撤销预检）",
      ref: "spec §10",
      note: "右侧并排打开 Diff 审查栏：点「撤销此块 ↺」回滚单个 Hunk；点行号「+」添加批注芯片到输入框；切换变体看 SHA-256 冲突预检",
      variants: [["diff", "右侧 Diff 审查面板打开"], ["undo", "安全撤销冲突预检（SHA-256）"]],
      openReview: true,
      thread: (v) => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          ${frag.asstTurn({
            id: "t-rev",
            collapsed: false,
            foldSummary: t("turn.fold", { d: "11.4s", e: 2, f: 3, s: 2 }),
            spineHtml: frag.fullSpine(false),
            mdHtml: frag.markdownAnswer(),
            changesHtml: frag.changesSummaryCard({ showUndoCheck: v === "undo", fileStates: rvState.fileStates }),
          })}
        </div>`,
    },

    // 5. Shell Command Success (Collapsed) + Failure (Auto-Expanded) & All 10 Tool Statuses
    {
      id: "shell",
      group: "2 代码审查与工具",
      title: "5. 终端命令：成功默认折叠 vs 失败（非零退出码）自动展开",
      ref: "spec §4 · §5.2",
      note: "exit 0 不显示退出码徽标且输出默认折叠；exit 101 标红且 stderr 尾部自动展开；切换变体看 10 种工具状态",
      variants: [["pair", "成功折叠 + 失败自动展开"], ["statuses", "全部 10 种工具状态"]],
      thread: (v) => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          ${frag.asstTurn({
            id: "t-sh",
            dur: "4.2s",
            spineHtml: v === "statuses" ? CS.allStatusRows() : CS.shellPairSpine(),
            mdHtml: `<div class="cs-md"><p>${v === "statuses" ? "上方展示了统一状态表定义的 10 种工具调用行状态（仅「失败」计入失败数并自动展开错误详情）。" : "离线构建因缺少 <code>httpdate-fast</code> 缓存包返回退出码 101（已自动展开错误输出）；我已改用仓库内已有的解析函数并通过全部单测。"}</p></div>`,
            costTxt: "4.2s · 9.8k token · $0.009",
          })}
        </div>`,
    },

    // 6. Inline Approval Card (Command, Full Scrollable Diff, Deny with Feedback, High Risk)
    {
      id: "approval",
      group: "3 审批与人工关卡",
      title: "6. 内联权限审批卡：命令 / 完整 Diff / 带反馈拒绝 / 高危",
      ref: "spec §7.1",
      note: "支持 ↵ 允许一次、允许范围 ▾、Tab 或点「拒绝」输入反馈后按 ↵（智能体不中断本轮，按反馈调整方案继续）",
      variants: [
        ["command", "命令审批（操作符高亮）"],
        ["diff", "文件编辑审批（完整 Diff 默认展开）"],
        ["deny", "带反馈拒绝（不中断当前轮）"],
        ["high_risk", "高风险（远端推送 / 无沙箱）"],
      ],
      thread: (v) => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          <div class="asst-turn">
            <div class="asst-turn-head"><span class="asst-model">deepseek-v4-pro</span><span class="mono text-faint">· 工作中</span></div>
            <div class="asst-spine" id="cs-appr-spine">
              ${frag.exploreGroup({ id: "exp-ap", files: 2, searches: 1 })}
              ${frag.spRow({
                id: "w-ap",
                st: "waiting",
                wait: "approval",
                title: v === "diff"
                  ? "编辑 <code>crates/spanbay-core/src/llm/router.rs</code> (+18 −4)"
                  : v === "high_risk"
                    ? "运行 <code>$ git push origin main --force-with-lease</code>"
                    : "运行 <code>$ cargo test -p spanbay-core router &amp;&amp; git diff --stat &gt; /tmp/router.diff</code>",
              })}
            </div>
          </div>
        </div>`,
      dock: (v) => CS.approval(CS.approvalSpecs()[v || "command"]),
      after: (v) => {
        if (v === "deny") $("[data-cs-deny-input]")?.focus();
      },
    },

    // 7. Plan Confirmation Gate
    {
      id: "plan",
      group: "3 审批与人工关卡",
      title: "7. 计划确认关卡（Plan Approval Gate）",
      ref: "spec §7.2",
      note: "支持阶段/Markdown 切换；直接在下方输入框打字按 ↵ 即可提交修订意见（Revise），或点「批准并执行」",
      thread: () => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          <div class="asst-turn">
            <div class="asst-turn-head"><span class="asst-model">deepseek-v4-pro</span><span class="asst-effort">计划模式</span></div>
            <div class="asst-spine">
              ${frag.exploreGroup({ id: "exp-pl", files: 3, searches: 2 })}
              <div class="sp-row" id="cs-plan-block">
                ${CS.dot("warn")}
                <span class="sp-title"><b>计划 · 按 Retry-After 退避替换 429 直接熔断（4 步）</b></span>
                <span class="cs-word warn">${t("st.wait.plan")}</span>
              </div>
              <div id="cs-plan-stamps" class="col-stack-sm" style="margin-left:18px"></div>
            </div>
          </div>
        </div>`,
      dock: () => CS.planGate(),
    },

    // 8. Clarifying Question Gate
    {
      id: "question",
      group: "3 审批与人工关卡",
      title: "8. 澄清提问关卡（Question Gate）",
      ref: "spec §7.3",
      note: "数字键 1–3 快速选择方案，或底部输入自定义回答；↵ 回答，Esc 跳过",
      thread: () => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          <div class="asst-turn">
            <div class="asst-turn-head"><span class="asst-model">deepseek-v4-pro</span><span class="mono text-faint">· 等待回答</span></div>
            <div class="asst-spine">${frag.exploreGroup({ id: "exp-q", files: 2, searches: 1 })}</div>
          </div>
        </div>`,
      dock: () => CS.questionGate(),
    },

    // 9. Subagents Parallel & Child Chat
    {
      id: "subagents",
      group: "4 智能体、待办与长会话",
      title: "9. 并行子智能体与子会话（Parallel Subagents）",
      ref: "spec §4 · §12",
      note: "显式标注每个子智能体的模型与角色（如 deepseek-v4-flash · 探索）；点击展开工具步骤或进入子对话",
      variants: [["parallel", "主对话：并行子智能体 ×2"], ["child", "子对话：带返回父对话顶栏"]],
      head: (v) =>
        v === "child"
          ? `<div class="chat-head-top">
              <div class="chat-head-title-wrap" style="gap:8px">
                <button class="btn btn-sm btn-ghost text-accent" data-cs="back-parent">${t("sub.back", { title: "修复网关 429 熔断与退避" })}</button>
                <span class="work-title">统计日志中 429 的来源</span>
                <span class="tag">${t("sub.child_of")}</span>
                <span class="tag mono">deepseek-v4-flash · 探索</span>
              </div>
            </div>`
          : null,
      thread: (v) =>
        v === "child"
          ? `
            <div class="turn-block">
              ${frag.userTurn("统计 gateway-log-1106.json 中 429 的来源，按模型分组，核对每次是否带 retry-after。")}
              ${frag.asstTurn({
                id: "t-child",
                model: "deepseek-v4-flash",
                effort: "探索 · 只读",
                dur: "4.2s",
                spineHtml: frag.exploreGroup({ id: "exp-child", files: 1, searches: 2, open: true }),
                mdHtml: `<div class="cs-md"><p>31 次 429 全部来自 <code>deepseek-v4-pro</code>，100% 携带 <code>retry-after: 2</code> 响应头。</p></div>`,
                costTxt: "4.2s · 4.8k token · $0.002",
              })}
            </div>`
          : `
            <div class="turn-block">
              ${frag.userWithAtts()}
              ${frag.asstTurn({
                id: "t-sub",
                dur: "18.4s",
                spineHtml: `${frag.thinkRow("th-sub", "1.1", "派发两个并行子智能体：一个用快速模型统计日志 429 分布，另一个探测备用路由延迟。")}${CS.subagentsCard()}`,
                mdHtml: `<div class="cs-md"><p>两个子智能体已完成日志排查与备用节点首字延迟对比。</p></div>`,
              })}
            </div>`,
    },

    // 10. Todos (Shown in ONE place only — above composer)
    {
      id: "todos",
      group: "4 智能体、待办与长会话",
      title: "10. 待办进度条：全局唯一位置（流内静默 todo_write）",
      ref: "spec §1.2 · §4 · §8.1",
      note: "流内彻底不再为 todo_write 生成「已工作 0ms · 1步」卡片；唯一进度条固定在输入框上方，可展开查看 7 项任务",
      variants: [["open", "展开任务清单 (3/7)"], ["closed", "默认单行收起"]],
      running: true,
      thread: () => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          ${frag.asstTurn({
            id: "t-todo",
            dur: "9.2s",
            spineHtml: `${frag.exploreGroup({ id: "exp-td", files: 3, searches: 2 })}${frag.editRow({ path: "crates/spanbay-core/src/llm/router.rs", added: 18, removed: 4, fileIdx: 0 })}`,
            mdHtml: `<div class="cs-md"><p>已完成 <code>router.rs</code> 的 429 退避改造（注：中间的 <code>todo_write</code> 步骤已在流内静默，直接同步至下方唯一任务栏）。</p></div>`,
          })}
        </div>`,
      dock: (v) => CS.workStripHtml({ phase: "正在运行 router 模块单元测试", steps: 4, secs: 9, showTodos: true, todosOpen: v !== "closed" }),
    },

    // 11. Cost / Budget Pause Gate
    {
      id: "cost",
      group: "3 审批与人工关卡",
      title: "11. 费用/Token 预算暂停关卡（Budget Pause Gate）",
      ref: "spec §7.4",
      note: "展示主智能体/子智能体/压缩费用构成、下一次暂停阈值及未计价模型说明",
      thread: () => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          ${frag.asstTurn({
            id: "t-cost",
            dur: "41s",
            spineHtml: frag.fullSpine(false),
            mdHtml: `<div class="cs-md"><p>已完成核心代码修改与第一轮验证，当前运行累计估算费用已达设定的单次上限 <code>US$0.50</code>。</p></div>`,
            stopChip: frag.tag(t("st.wait.cost"), "warn"),
            costTxt: "41s · 142k token · $0.52",
          })}
        </div>`,
      dock: () => CS.costGate(),
    },

    // 12. Error + Retry, Cancelled, Max Steps, Degraded & Compaction
    {
      id: "error",
      group: "4 智能体、待办与长会话",
      title: "12. 错误横幅与重试、已取消、步数上限、流重连与上下文压缩",
      ref: "spec §3.2 · §5.2",
      note: "切换上方变体查看：模型全部失败 / 密钥被拒 / 超出上下文窗口 / 已取消（带队列暂停） / 达步数上限 / 实时压缩与备用切换提示",
      variants: [
        ["all_targets_failed", "模型全部失败 + 重试"],
        ["key_rejected", "密钥被拒 (401)"],
        ["context_full", "超出上下文窗口"],
        ["cancelled", "已取消 + 队列暂停"],
        ["max_steps", "达步数上限 (20步)"],
        ["compaction", "上下文压缩 + 备用链切换提示"],
      ],
      thread: (v) => {
        if (v === "cancelled") {
          return `
            <div class="turn-block">
              ${frag.userWithAtts()}
              ${frag.asstTurn({
                id: "t-can",
                dur: "48s",
                spineHtml: `${frag.exploreGroup({ id: "exp-can", files: 2, searches: 1 })}${frag.editRow({ path: "crates/spanbay-core/src/llm/router.rs", added: 18, removed: 4, fileIdx: 0 })}${frag.spRow({ id: "sh-can", st: "cancelled", title: "运行 <code>$ cargo test -p spanbay-core</code>", meta: "31s" })}`,
                extraHtml: `<div class="cs-end">${CS.dot("stopped")}<span>${t("end.cancelled", { d: "48s", steps: 4, files: 1, cost: "US$0.06" })}</span><button class="text-accent ml-auto" data-cs="open-review">${t("end.review")}</button></div>`,
                costTxt: "48s · 28.4k token · $0.06",
              })}
            </div>`;
        }
        if (v === "max_steps") {
          return `
            <div class="turn-block">
              ${frag.userWithAtts()}
              ${frag.asstTurn({
                id: "t-max",
                dur: "3m 10s",
                collapsed: true,
                foldSummary: t("turn.fold", { d: "3m 10s", e: 2, f: 8, s: 6 }),
                spineHtml: frag.fullSpine(false),
                mdHtml: `<div class="cs-md"><p>已完成 <code>router.rs</code> 与 <code>tests.rs</code> 的修改，还差最后一项集成基准测试未执行。</p></div>`,
                extraHtml: `<div class="cs-end">${CS.dot("warn")}<span>${t("end.max_steps", { n: 20 })}</span></div>`,
                stopChip: frag.tag("已到步数上限", "warn"),
                costTxt: "3m 10s · 94k token · $0.14",
              })}
            </div>`;
        }
        if (v === "compaction") {
          return `
            ${banner("", t("notice.full_access_off"), `<button class="btn btn-sm" data-cs="dismiss-parent">${t("notice.dismiss")}</button>`)}
            <div class="turn-block">
              ${frag.userWithAtts()}
              <div class="cs-divider">${t("thread.compacted", { before: "96,400", after: "18,200" })}</div>
              <div class="cs-notice">${CS.dot("idle")}${t("notice.retrying", { reason: "429", delay: 2, attempt: 1, of: 2 })}</div>
              <div class="cs-notice">${CS.dot("idle")}${t("notice.fallback", { from: "deepseek-v4-pro", reason: "429", to: "kimi-k2.6" })}</div>
              ${frag.asstTurn({
                id: "t-comp",
                model: "deepseek-v4-pro → kimi-k2.6",
                effort: "备用路由",
                dur: "6.4s",
                spineHtml: frag.exploreGroup({ id: "exp-comp", files: 2, searches: 1 }),
                mdHtml: `<div class="cs-md"><p>已通过备用模型 <code>kimi-k2.6</code> 完成日志核对：31 次 429 全部携带 <code>retry-after: 2</code>。</p></div>`,
                extraHtml: `<div class="cs-notice">${CS.dot("warn")}${t("notice.untracked", { n: 2, paths: "Cargo.lock、target/.rustc_info.json" })}</div>`,
                costTxt: "6.4s · 18.2k token · $0.015",
              })}
            </div>`;
        }
        return `
          <div class="turn-block">
            ${frag.userWithAtts()}
            ${frag.asstTurn({
              id: "t-err",
              dur: "6.0s",
              spineHtml: frag.exploreGroup({ id: "exp-err", files: 1, searches: 0 }),
              extraHtml: errorBanner(v || "all_targets_failed"),
              costTxt: "6.0s · 4.1k token · $0.003",
            })}
          </div>`;
      },
      dock: (v) => (v === "cancelled" ? CS.queueBarHtml(queueItems, true) : ""),
    },

    // 13. Long History with Older Turns Auto-Collapsed
    {
      id: "history",
      group: "4 智能体、待办与长会话",
      title: "13. 长对话历史：旧轮次自动收拢为「已工作 Ns ›」+ 加载更早 + 跳到最新",
      ref: "spec §3 · §5.1",
      note: "前 4 轮已完成对话自动收拢为单行「▸ 已工作 Ns · 已探索…」（点任意一轮可展开步骤），最新第 5 轮保持展开；点顶部「加载更早」视口不跳动",
      thread: () => `
        <div class="row-center" style="justify-content:center" id="cs-earlier">
          <button class="btn btn-sm" data-cs="load-earlier">${t("thread.load_earlier")}</button>
        </div>
        <div id="cs-list" class="col-stack" style="gap:18px">
          ${OLDER_TURNS.map(renderHistoryTurn).join("")}
          <div class="turn-block">
            ${frag.userWithAtts({ id: "u-latest" })}
            ${frag.asstTurn({
              id: "t-latest",
              collapsed: false,
              foldSummary: t("turn.fold", { d: "11.4s", e: 2, f: 3, s: 2 }),
              spineHtml: frag.fullSpine(false),
              mdHtml: frag.markdownAnswer(),
              changesHtml: frag.changesSummaryCard(),
            })}
          </div>
        </div>`,
      after: () => {
        el.scroller.scrollTop = 120;
        el.scroller.dispatchEvent(new Event("scroll"));
      },
    },

    // 14. Hover Message Actions & Turn Metrics Popover
    {
      id: "hover",
      group: "5 外框操作与输入框",
      title: "14. 悬停消息操作栏与回合 7 项指标明细浮层（Hover Chrome & Metrics）",
      ref: "spec §6",
      note: "平时隐藏操作按钮与冗长指标；悬停时显示用户操作（编辑并重发/分支）与助手回合操作（复制/重试/分支/执行图），点击右侧胶囊弹出 7 项指标卡",
      thread: () => `
        <div class="turn-block">
          ${frag.userWithAtts({ forceHover: true })}
          ${frag.asstTurn({
            id: "t-hov",
            collapsed: false,
            foldSummary: t("turn.fold", { d: "11.4s", e: 2, f: 3, s: 2 }),
            spineHtml: frag.fullSpine(false),
            mdHtml: frag.markdownAnswer(),
            changesHtml: frag.changesSummaryCard(),
            forceHover: true,
            metricsOpen: true,
          })}
        </div>`,
    },

    // 15. Slimmed Composer with Context Meter, Mode & Model Popovers, Sidebar Search
    {
      id: "composer",
      group: "5 外框操作与输入框",
      title: "15. 精简输入框：上下文水位环 ◔14%、模式/模型合一弹层、附件与批注芯片",
      ref: "spec §9",
      note: "底栏从 9 个控件精简为 5 个；顶部挂载截图、文件、/技能与 @router.rs:215 行内批注芯片；切换变体看上下文压缩、模型推理弹层与侧栏搜索",
      variants: [
        ["context", "上下文窗口明细 + 立即压缩 (◔ 14%)"],
        ["cannot", "上下文窗口 (can_compact = false)"],
        ["modes", "模型 + 推理强度 + 联网合一菜单"],
        ["search", "侧栏全文搜索命中"],
      ],
      sidebar: (v) => (v === "search" ? CS.searchSidebar("retry-after", false) : null),
      query: (v) => (v === "search" ? "retry-after" : ""),
      chips: [
        { kind: "thumb", key: "shot" },
        { kind: "file", label: "gateway-log-1106.json", sub: "209 KB" },
        { kind: "skill", label: "/rust-review" },
        { kind: "comment", label: "@router.rs:215", sub: "退避上限请读配置项" },
      ],
      ctxOpen: (v) => v === "context" || v === "cannot",
      modelOpen: (v) => v === "modes",
      thread: () => `
        <div class="turn-block">
          ${frag.userWithAtts()}
          ${frag.asstTurn({
            id: "t-comp-demo",
            collapsed: true,
            foldSummary: t("turn.fold", { d: "11.4s", e: 2, f: 3, s: 2 }),
            spineHtml: frag.fullSpine(false),
            mdHtml: frag.markdownAnswer(),
            changesHtml: frag.changesSummaryCard(),
          })}
        </div>`,
    },

    // 16. Unified Status & Tool Label Legend
    {
      id: "legend",
      group: "6 规范图例",
      title: "16. 统一状态表与各工具精确中英单行文案图例",
      ref: "spec §4.2 · design §6",
      note: "汇总 14 种状态的形状/词色/双语文案，以及参照 Codex/Claude Code/ZCode 核定的工具单行文案规范",
      thread: () => CS.legend(),
      after: () => { el.scroller.scrollTop = 0; },
    },
  ];

  const byId = Object.fromEntries(STATES.map((s) => [s.id, s]));

  // Backward-compatible aliases for any legacy deep links
  const ALIASES = {
    original: ["turn", "expanded"],
    "stream.markdown": ["turn", "expanded"],
    "stream.viewer": ["turn", "expanded"],
    "stream.streaming": ["running", "streaming"],
    "stream.long": ["history", null],
    "stream.skills": ["composer", "context"],
    "tools.statuses": ["shell", "statuses"],
    "tools.detail": ["shell", "pair"],
    "approval.command": ["approval", "command"],
    "approval.diff": ["approval", "diff"],
    "approval.deny": ["approval", "deny"],
    "approval.remote": ["approval", "high_risk"],
    "gate.plan": ["plan", null],
    "gate.question": ["question", null],
    "gate.cost": ["cost", null],
    "run.running": ["running", "streaming"],
    "run.cancelled": ["error", "cancelled"],
    "run.max_steps": ["error", "max_steps"],
    "run.error": ["error", "all_targets_failed"],
    "run.compaction": ["error", "compaction"],
    "agent.subagents": ["subagents", "parallel"],
    "agent.child": ["subagents", "child"],
    "agent.todos": ["todos", "open"],
    "agent.context": ["composer", "context"],
    "agent.search": ["composer", "search"],
  };

  // ── Render Composer Chips & Send Controls ─────────────────────────────────
  function renderComposerExtras(s, v) {
    if (editingQueueIdx !== null && queueItems[editingQueueIdx]) {
      el.editBanner.hidden = false;
      el.editBanner.innerHTML = `
        <span><b>${t("queue.editing", { i: editingQueueIdx + 1 })}</b></span>
        <div class="row-center" style="gap:6px">
          <button class="btn btn-sm" data-cs="cancel-edit-queue">${t("common.cancel")}</button>
          <button class="btn btn-sm btn-primary" data-cs="save-edit-queue">保存修改 (↵)</button>
        </div>`;
    } else {
      el.editBanner.hidden = true;
      el.editBanner.innerHTML = "";
    }

    const activeChips = composerChips.length ? composerChips : (s.chips || []);
    if (activeChips.length) {
      el.chips.hidden = false;
      el.chips.innerHTML = activeChips.map((c, idx) => {
        if (c.kind === "thumb") return frag.thumb(c.key);
        if (c.kind === "skill") return frag.chip(c.label, "", "accent", `data-cs="rm-chip" data-cs-idx="${idx}"`);
        if (c.kind === "comment") return frag.chip(c.label, c.sub, "accent", `data-cs="rm-chip" data-cs-idx="${idx}"`);
        return frag.chip(c.label, c.sub, "", `data-cs="rm-chip" data-cs-idx="${idx}"`);
      }).join("");
    } else {
      el.chips.hidden = true;
      el.chips.innerHTML = "";
    }

    const isRunning = Boolean(s.running);
    el.composer.placeholder = isRunning ? t("composer.ph_run") : t("composer.ph");
    if (isRunning) {
      el.sendSlot.innerHTML = `
        <button class="btn btn-sm" data-cs="queue-add" title="排入下一轮队列 (↵)">排队 ↵</button>
        <button class="btn btn-sm" data-cs="steer-send" title="立即插队引导当前回合 (⌘↵)">插队 ⌘↵</button>
        <button class="btn btn-sm btn-danger" data-cs="stop-run" title="停止当前运行">■ ${t("run.stop")}</button>`;
    } else {
      el.sendSlot.innerHTML = `<button class="send-circle" data-action="send-chat" title="发送 (↵)">↑</button>`;
    }
    const ctxOpen = s.ctxOpen?.(v);
    const modelOpen = s.modelOpen?.(v);
    el.ctxSlot.innerHTML = `
      <button class="btn btn-sm btn-ghost mono" data-cs="ctx" title="上下文窗口水位与立即压缩">◔ 14%</button>
      ${ctxOpen ? CS.contextPopover(v !== "cannot") : ""}
      ${modelOpen ? CS.modelPopover("low", true) : ""}`;
  }

  function syncReviewPanel() {
    if (!el.reviewPanel) return;
    el.reviewPanel.hidden = !rvState.open;
    const pane = $(".pane", mod);
    if (pane) pane.hidden = rvState.open;
    const restoreBtn = $("#chat-pane-restore", el.head);
    if (restoreBtn) restoreBtn.hidden = !rvState.open;
    const pill = $(".cs-changes-pill", el.head);
    if (pill) pill.classList.toggle("active", rvState.open);
    if (rvState.open) {
      el.reviewPanel.innerHTML = CS.reviewPanelHtml(rvState);
    }
  }

  function syncHeaderI18n() {
    const pop = $(".cs-head-more-pop", el.head);
    if (pop) {
      pop.querySelectorAll("[data-cs='view-mode']").forEach((btn) => {
        const m = btn.dataset.csMode;
        btn.textContent = t(`head.${m}`);
        btn.classList.toggle("active", m === viewMode);
      });
    }
  }
  // ── Main Render Function ───────────────────────────────────────────────────
  function render(rawId, rawV) {
    let id = rawId;
    let v = rawV;
    if (ALIASES[id]) {
      [id, v] = [ALIASES[id][0], rawV || ALIASES[id][1]];
    }
    const s = byId[id] || byId.turn;
    clearTimers();
    closeViewer();
    revise = false;
    earlierPages = 0;
    editingQueueIdx = null;
    current = s;
    variant = v || s.variants?.[0]?.[0] || null;

    if (s.id === "turn") {
      if (variant === "collapsed") viewMode = "compact";
      else if (variant === "verbose") viewMode = "verbose";
      else viewMode = "normal";
    }

    rvState.open = Boolean(s.openReview);
    el.head.innerHTML = s.head?.(variant) || defaultHeadHtml;
    syncHeaderI18n();
    el.list.innerHTML = s.sidebar?.(variant) || defaultListHtml;
    el.search.value = s.query ? s.query(variant) : "";
    el.col.innerHTML = s.thread ? s.thread(variant) : "";
    dock.innerHTML = s.dock ? s.dock(variant) : "";

    renderComposerExtras(s, variant);
    syncReviewPanel();
    const scrollBottom = (s.id === "review" && variant === "undo") || s.id === "hover";
    el.scroller.scrollTop = scrollBottom ? el.scroller.scrollHeight : 0;
    s.after?.(variant);
    syncBar();

    const url = new URL(location.href);
    url.searchParams.set("module", "chat");
    url.searchParams.set("chat", s.id);
    if (variant) url.searchParams.set("v", variant); else url.searchParams.delete("v");
    url.searchParams.set("lang", CS.lang);
    history.replaceState(null, "", url);
  }

  function syncBar() {
    if (!el.bar) return;
    el.select.value = current.id;
    $("#cs-ref", el.bar).textContent = current.ref;
    $("#cs-note", el.bar).textContent = current.note;
    $("#cs-note", el.bar).title = current.note;
    const vs = $("#cs-variants", el.bar);
    vs.hidden = !current.variants;
    vs.innerHTML = (current.variants || []).map(([k, lbl]) => `<button class="${k === variant ? "active" : ""}" data-cs-variant="${k}">${lbl}</button>`).join("");
    el.bar.querySelectorAll("[data-cs-lang]").forEach((b) => b.classList.toggle("active", b.dataset.csLang === CS.lang));
  }

  function populateSelect() {
    const groups = {};
    STATES.forEach((s) => { (groups[s.group] ||= []).push(s); });
    el.select.innerHTML = Object.entries(groups).map(([g, list]) =>
      `<optgroup label="${g}">${list.map((s) => `<option value="${s.id}">${s.title}</option>`).join("")}</optgroup>`).join("");
  }

  function go(id, v) {
    if (window.PD.setModule) window.PD.setModule("chat");
    else document.querySelector('.nav-item[data-nav-to="chat"]')?.click();
    render(id, v);
  }
  function step(delta) {
    const i = STATES.findIndex((s) => s.id === current?.id);
    go(STATES[(i + delta + STATES.length) % STATES.length].id);
  }

  function streamText() {
    const target = $("#cs-stream-text");
    if (!target) return;
    const words = "接下来将在首个内容事件前切到备用链，并补齐 4 个单元测试。".split("");
    if (matchMedia("(prefers-reduced-motion: reduce)").matches) { target.textContent += words.join(""); return; }
    let i = 0;
    timers.push(setInterval(() => {
      if (i >= words.length) { clearTimers(); return; }
      target.textContent += words[i++];
    }, 65));
  }

  // ── Image Viewer ──────────────────────────────────────────────────────────
  let viewer = null;
  function openViewer(key) {
    const m = CS.MEDIA[key];
    if (!m) return;
    closeViewer();
    viewer = document.createElement("div");
    viewer.className = "cs-viewer";
    viewer.setAttribute("role", "dialog");
    viewer.setAttribute("aria-modal", "true");
    viewer.innerHTML = `
      <div class="cs-viewer-bar"><b>${m.name}</b><span class="text-muted mono">${m.meta}</span>
        <span class="seg ml-auto"><button class="active" data-cs="viewer-fit">${t("viewer.fit")}</button><button data-cs="viewer-actual">${t("viewer.actual")}</button></span>
        <button class="btn btn-sm" data-toast="交给系统默认应用打开（核心写出临时副本）">${t("viewer.open")}</button>
        <button class="btn-icon" data-cs="viewer-close" title="${t("common.close")} (Esc)">×</button></div>
      <div class="cs-viewer-stage" data-cs="viewer-close-bg">${m.svg()}</div>`;
    document.body.appendChild(viewer);
    viewer.querySelector('[data-cs="viewer-close"]').focus();
  }
  function closeViewer() { viewer?.remove(); viewer = null; }

  // ── Approvals & Deny with Feedback ────────────────────────────────────────
  const card = (id) => document.querySelector(`[data-cs-appr="${id}"]`);
  function openDeny(id) {
    const c = card(id); if (!c) return;
    c.querySelector(`[data-cs-deny-row]`).hidden = false;
    c.querySelector(`[data-cs="deny"]`).hidden = true;
    c.querySelector(`[data-cs-deny-input]`).focus();
  }
  function closeDeny(id) {
    const c = card(id); if (!c) return;
    c.querySelector(`[data-cs-deny-row]`).hidden = true;
    c.querySelector(`[data-cs="deny"]`).hidden = false;
    c.focus();
  }
  function resolveCard(id, html, auditRowHtml = "") {
    const c = card(id); if (!c) return;
    const waitRow = $("#w-ap");
    if (waitRow && auditRowHtml) {
      waitRow.outerHTML = `<div class="sp-audit-line">${auditRowHtml}</div>`;
    }
    const done = document.createElement("div");
    done.innerHTML = banner("", html, `<button class="btn btn-sm" data-cs="reset">还原演示</button>`);
    c.replaceWith(done.firstElementChild);
    el.composer.focus();
  }
  const scopeLabel = { once: () => t("res.once"), session: () => t("res.session"), folder: () => t("res.folder"), tool: () => t("res.global") };

  // ── Plan Revise ───────────────────────────────────────────────────────────
  function stampPlan(text, approved) {
    const box = $("#cs-plan-stamps");
    if (box) box.insertAdjacentHTML("beforeend", `<div class="cs-plan-stamp">${approved ? t("plan.stamp_approved") : t("plan.stamp_revise", { text })}</div>`);
    const w = $("#cs-plan-block .cs-word");
    if (w) { w.className = "cs-word"; w.textContent = approved ? t("st.completed") : t("st.running"); }
    $("[data-cs-plan-gate]")?.remove();
    revise = false;
    el.composer.placeholder = t("composer.ph");
    dock.insertAdjacentHTML("afterbegin", CS.workStripHtml({ phase: t("thread.thinking"), steps: 1, secs: 1, showTodos: true, todosOpen: false }));
    el.scroller.scrollTop = el.scroller.scrollHeight;
  }
  function submitRevision() {
    const text = el.composer.value.trim();
    if (!text) { ov()?.toast("请先在输入框输入计划修改意见", { tone: "warn" }); return; }
    el.composer.value = "";
    stampPlan(text, false);
    ov()?.toast("已将修改意见发给智能体重新修订计划");
  }

  // ── Event Delegation ──────────────────────────────────────────────────────
  document.addEventListener("click", (e) => {
    const vb = e.target.closest("[data-cs-variant]");
    if (vb) { render(current.id, vb.dataset.csVariant); return; }
    const lb = e.target.closest("[data-cs-lang]");
    if (lb) { CS.lang = lb.dataset.csLang; render(current.id, variant); return; }
    const nav = e.target.closest("[data-cs-step]");
    if (nav) { step(Number(nav.dataset.csStep)); return; }
    const goBtn = e.target.closest("[data-cs-go]");
    if (goBtn) { go(goBtn.dataset.csGo); return; }

    const tgt = e.target.closest("[data-cs]");
    if (!tgt) {
      if (!e.target.closest(".cs-ctx-wrap") && !e.target.closest("#chat-model-label") && !e.target.closest(".cs-head-more-pop")) {
        $(".cs-ctx-pop")?.remove();
        $(".cs-model-pop")?.remove();
        $(".cs-head-more-pop")?.remove();
      }
      return;
    }
    const a = tgt.dataset.cs;
    const id = tgt.dataset.csId;
    switch (a) {
      case "go":
        go(tgt.dataset.csGo);
        break;
      case "toggle-head-more": {
        const actions = tgt.closest(".work-actions");
        const existing = $(".cs-head-more-pop", actions);
        if (existing) { existing.remove(); break; }
        actions.insertAdjacentHTML("beforeend", `
          <div class="cs-head-more-pop">
            <div class="row-between fs-2xs text-muted" style="padding:2px 6px"><span>过程详略视图 (⌃O)</span></div>
            <div class="seg cs-view-seg" style="width:100%">
              <button type="button" style="flex:1" class="${viewMode === "compact" ? "active" : ""}" data-cs="view-mode" data-cs-mode="compact">${t("head.compact")}</button>
              <button type="button" style="flex:1" class="${viewMode === "normal" ? "active" : ""}" data-cs="view-mode" data-cs-mode="normal">${t("head.normal")}</button>
              <button type="button" style="flex:1" class="${viewMode === "verbose" ? "active" : ""}" data-cs="view-mode" data-cs-mode="verbose">${t("head.verbose")}</button>
            </div>
            <div class="pop-sep"></div>
            <button class="pop-opt" data-pop-toast="已置顶此对话">置顶此对话</button>
            <button class="pop-opt" data-pop-toast="已导出脱敏对话 Markdown">导出脱敏对话 Markdown…</button>
            <button class="pop-opt" data-pop-toast="已打开本对话附加指令">本对话附加指令…</button>
          </div>`);
        break;
      }
      case "view-mode": {
        viewMode = tgt.dataset.csMode || "normal";
        $(".cs-head-more-pop")?.remove();
        if (current.id === "turn") {
          const mapV = viewMode === "compact" ? "collapsed" : viewMode === "verbose" ? "verbose" : "expanded";
          render("turn", mapV);
        } else {
          document.querySelectorAll(".asst-spine").forEach((sp) => {
            sp.hidden = viewMode === "compact";
          });
          ov()?.toast(`已切换过程视图为「${t(`head.${viewMode}`)}」`);
        }
        break;
      }
      case "toggle": {
        const inner = e.target.closest("button, [data-toast], [data-modal], [data-cs]:not([data-cs='toggle'])");
        if (inner && inner !== tgt && tgt.contains(inner)) break;
        const body = document.getElementById(tgt.dataset.csTarget);
        if (!body) break;
        body.hidden = !body.hidden;
        tgt.setAttribute("aria-expanded", String(!body.hidden));
        const chev = tgt.querySelector(".sp-chev, .sp-caret, .cs-row-caret, .text-faint");
        if (chev && /[›⌄▸▾]/.test(chev.textContent)) {
          chev.textContent = body.hidden ? "›" : "⌄";
        }
        break;
      }
      case "toggle-review":
        rvState.open = !rvState.open;
        syncReviewPanel();
        break;
      case "open-review":
        rvState.open = true;
        syncReviewPanel();
        break;
      case "open-review-file":
        rvState.open = true;
        rvState.selectedFile = Number(tgt.dataset.csFile || 0);
        syncReviewPanel();
        break;
      case "open-review-output":
        rvState.open = true;
        syncReviewPanel();
        ov()?.toast("已在右侧审查面板打开完整输出预览");
        break;
      case "close-review":
        rvState.open = false;
        syncReviewPanel();
        break;
      case "rv-select-file":
        rvState.selectedFile = Number(tgt.dataset.csFile || 0);
        rvState.commentingLine = null;
        syncReviewPanel();
        break;
      case "rv-scope":
        rvState.scope = tgt.dataset.csVal || "turn";
        syncReviewPanel();
        break;
      case "rv-diff-mode":
        rvState.diffMode = tgt.dataset.csVal || "net";
        syncReviewPanel();
        break;
      case "rv-revert-hunk": {
        const hId = tgt.dataset.csHunk;
        rvState.revertedHunks[hId] = true;
        syncReviewPanel();
        ov()?.toast("已回滚该代码块（Per-Hunk Revert）");
        break;
      }
      case "rv-line-comment": {
        rvState.commentingLine = tgt.dataset.csLine;
        syncReviewPanel();
        $("#rv-comment-input")?.focus();
        break;
      }
      case "rv-cancel-comment":
        rvState.commentingLine = null;
        syncReviewPanel();
        break;
      case "rv-submit-comment": {
        const lineKey = tgt.dataset.csLine;
        const val = $("#rv-comment-input")?.value.trim() || "请修改此处逻辑";
        composerChips.push({ kind: "comment", label: `@${lineKey}`, sub: val.slice(0, 18) });
        rvState.commentingLine = null;
        syncReviewPanel();
        renderComposerExtras(current, variant);
        el.composer.focus();
        ov()?.toast(`已将 @${lineKey} 批注芯片挂入底部输入框`);
        break;
      }
      case "rv-keep-file": {
        const fIdx = Number(tgt.dataset.csFile || 0);
        rvState.fileStates[fIdx] = "kept";
        syncReviewPanel();
        const cardEl = $("#cs-turn-changes");
        if (cardEl) cardEl.outerHTML = frag.changesSummaryCard({ fileStates: rvState.fileStates });
        ov()?.toast("已标记该文件变更为「已保留」");
        break;
      }
      case "rv-undo-file": {
        const fIdx = Number(tgt.dataset.csFile || 0);
        rvState.fileStates[fIdx] = "undone";
        syncReviewPanel();
        const cardEl = $("#cs-turn-changes");
        if (cardEl) cardEl.outerHTML = frag.changesSummaryCard({ fileStates: rvState.fileStates });
        ov()?.toast("已安全撤销该文件的修改（SHA-256 校验通过）");
        break;
      }
      case "rv-keep-all":
        rvState.fileStates = ["kept", "kept"];
        syncReviewPanel();
        if ($("#cs-turn-changes")) $("#cs-turn-changes").outerHTML = frag.changesSummaryCard({ fileStates: rvState.fileStates });
        ov()?.toast("已保留本轮全部 2 个文件变更");
        break;
      case "undo-turn-prompt": {
        const cardEl = $("#cs-turn-changes");
        if (cardEl && !$("#cs-undo-check")) {
          cardEl.outerHTML = frag.changesSummaryCard({ showUndoCheck: true, fileStates: rvState.fileStates });
        }
        break;
      }
      case "close-undo-prompt":
        $("#cs-undo-check")?.remove();
        break;
      case "confirm-undo-turn": {
        const force = tgt.dataset.csForce === "1";
        $("#cs-undo-check")?.remove();
        rvState.fileStates = force ? ["undone", "undone"] : ["conflict", "undone"];
        const cardEl = $("#cs-turn-changes");
        if (cardEl) cardEl.outerHTML = frag.changesSummaryCard({ undone: force, fileStates: rvState.fileStates });
        syncReviewPanel();
        ov()?.toast(force ? "已强制还原全部 2 个文件" : "已安全撤销 tests.rs，并保留外部修改过的 router.rs");
        break;
      }
      case "fill-starter":
        el.composer.value = tgt.dataset.csText || "";
        el.composer.focus();
        break;
      case "copy-text":
      case "copy-turn": {
        const orig = tgt.textContent;
        tgt.textContent = t("act.copied");
        setTimeout(() => { tgt.textContent = orig; }, 1200);
        break;
      }
      case "edit-user-msg": {
        el.composer.value = tgt.dataset.csText || USER_429;
        el.editBanner.hidden = false;
        el.editBanner.innerHTML = `
          <span><b>正在编辑历史指令</b> · 此轮包含 2 个文件修改</span>
          <div class="row-center" style="gap:6px">
            <button class="btn btn-sm" data-cs="cancel-edit-queue">${t("common.cancel")}</button>
            <button class="btn btn-sm" data-cs="submit-user-edit" data-cs-undo="0">仅重发指令</button>
            <button class="btn btn-sm btn-primary" data-cs="submit-user-edit" data-cs-undo="1">撤销文件修改并重发 (↵)</button>
          </div>`;
        el.composer.focus();
        break;
      }
      case "submit-user-edit": {
        const undoFirst = tgt.dataset.csUndo === "1";
        el.editBanner.hidden = true;
        el.composer.value = "";
        ov()?.toast(undoFirst ? "已回滚本轮 2 个文件修改并重新发起对话" : "已保留当前磁盘文件并重新发起对话");
        go("running", "streaming");
        break;
      }
      case "toggle-turn-metrics": {
        const foot = tgt.closest(".asst-turn-foot");
        const existing = $("#cs-turn-metrics-pop", foot);
        if (existing) existing.remove();
        else tgt.insertAdjacentHTML("afterend", `
          <div class="turn-metrics-pop" id="cs-turn-metrics-pop">
            <div class="row-between"><b class="text-ink1">${t("met.title")}</b><span class="tag tag-ok">200 OK</span></div>
            <div class="tm-row"><span class="k">${t("met.route")}</span><span class="v">DeepSeek / deepseek-v4-pro:low</span></div>
            <div class="tm-row"><span class="k">${t("met.steps")}</span><span class="v">${t("met.steps_val")}</span></div>
            <div class="tm-row"><span class="k">${t("met.in_out")}</span><span class="v">18,400 in · 1,100 out token</span></div>
            <div class="tm-row"><span class="k">${t("met.reason_cache")}</span><span class="v">420 reasoning · 12,000 cache token</span></div>
            <div class="tm-row"><span class="k">${t("met.ctx")}</span><span class="v">18,400 / 128,000 token (14%)</span></div>
            <div class="tm-row"><span class="k">${t("met.speed")}</span><span class="v">TTFT 1.20s · 52.4 token/s</span></div>
            <div class="tm-row"><span class="k">${t("met.cost")}</span><span class="v">US$0.018</span></div>
          </div>`);
        break;
      }
      case "queue-edit": {
        const idx = Number(tgt.dataset.csIdx || 0);
        editingQueueIdx = idx;
        el.composer.value = queueItems[idx]?.text || "";
        renderComposerExtras(current, variant);
        el.composer.focus();
        break;
      }
      case "cancel-edit-queue":
        editingQueueIdx = null;
        el.editBanner.hidden = true;
        el.composer.value = "";
        break;
      case "save-edit-queue": {
        if (editingQueueIdx !== null && queueItems[editingQueueIdx]) {
          queueItems[editingQueueIdx].text = el.composer.value.trim() || queueItems[editingQueueIdx].text;
        }
        editingQueueIdx = null;
        el.composer.value = "";
        dock.innerHTML = current.dock ? current.dock(variant) : "";
        renderComposerExtras(current, variant);
        ov()?.toast("已原位更新待发队列消息（agent_queue_update）");
        break;
      }
      case "queue-steer": {
        const idx = Number(tgt.dataset.csIdx || 0);
        const item = queueItems.splice(idx, 1)[0];
        dock.innerHTML = current.dock ? current.dock(variant) : "";
        const spine = $("#cs-running-spine");
        if (spine && item) {
          spine.insertAdjacentHTML("beforeend", `<div class="sp-steer-note"><span>↳</span><b>已插队引导当前轮：</b><span>「${item.text}」</span></div>`);
        }
        ov()?.toast("已将该消息立即注入当前运行回合（Steer）");
        break;
      }
      case "queue-remove": {
        const idx = Number(tgt.dataset.csIdx || 0);
        queueItems.splice(idx, 1);
        dock.innerHTML = current.dock ? current.dock(variant) : "";
        break;
      }
      case "queue-add": {
        const txt = el.composer.value.trim() || "补充检查一下 5xx 熔断恢复后的半开探测逻辑";
        queueItems.push({ id: `q-${Date.now()}`, text: txt });
        el.composer.value = "";
        dock.innerHTML = current.dock ? current.dock(variant) : "";
        ov()?.toast("已加入待发队列（当前回合结束后自动发送）");
        break;
      }
      case "steer-send": {
        const txt = el.composer.value.trim() || "先只改 router.rs，不要动网关配置文件";
        el.composer.value = "";
        const spine = $("#cs-running-spine");
        if (spine) {
          spine.insertAdjacentHTML("beforeend", `<div class="sp-steer-note"><span>↳</span><b>已插队引导当前轮：</b><span>「${txt}」</span></div>`);
        }
        ov()?.toast("已立即注入当前回合（下一步前生效）");
        break;
      }
      case "stop-run":
        go("error", "cancelled");
        break;
      case "rm-chip": {
        const idx = Number(tgt.dataset.csIdx || 0);
        if (!composerChips.length && current.chips) composerChips = [...current.chips];
        composerChips.splice(idx, 1);
        renderComposerExtras(current, variant);
        break;
      }
      case "attach-demo":
        composerChips.push({ kind: "file", label: "router-bench.json", sub: "42 KB" });
        renderComposerExtras(current, variant);
        ov()?.toast("已添加附件芯片 router-bench.json");
        break;
      case "toggle-skill-pop":
        composerChips.push({ kind: "skill", label: "/api-test-author" });
        renderComposerExtras(current, variant);
        ov()?.toast("已挂载技能 /api-test-author");
        break;
      case "toggle-model-pop": {
        $(".cs-ctx-pop")?.remove();
        const pop = $(".cs-model-pop");
        if (pop) pop.remove();
        else el.ctxSlot.insertAdjacentHTML("beforeend", CS.modelPopover("low", true));
        break;
      }
      case "pick-effort":
        tgt.parentElement.querySelectorAll("button").forEach((b) => b.classList.toggle("active", b === tgt));
        el.modelBtn.textContent = `deepseek-v4-pro · ${tgt.dataset.csVal} ▾`;
        break;
      case "toggle-web-in-pop": {
        const on = tgt.classList.toggle("tag-accent");
        tgt.textContent = on ? "已开启 ✓" : "已关闭";
        break;
      }
      case "viewer": openViewer(tgt.dataset.csMedia); break;
      case "viewer-close": closeViewer(); break;
      case "viewer-close-bg": if (e.target === tgt) closeViewer(); break;
      case "viewer-fit": case "viewer-actual": {
        viewer.querySelector(".cs-viewer-stage").classList.toggle("actual", a === "viewer-actual");
        viewer.querySelectorAll(".seg button").forEach((b) => b.classList.toggle("active", b === tgt));
        break;
      }
      case "load-earlier": {
        const list = $("#cs-list");
        const before = el.scroller.scrollHeight - el.scroller.scrollTop;
        const page = EARLIER_TURNS.slice(earlierPages, earlierPages + 1);
        list.insertAdjacentHTML("afterbegin", page.map(renderHistoryTurn).join(""));
        earlierPages += 1;
        if (earlierPages >= EARLIER_TURNS.length) {
          $("#cs-earlier").innerHTML = `<span class="fs-2xs text-faint">${t("thread.earliest")}</span>`;
        }
        el.scroller.scrollTop = el.scroller.scrollHeight - before;
        break;
      }
      case "deny": openDeny(id); break;
      case "deny-cancel": closeDeny(id); break;
      case "deny-confirm": {
        const reason = card(id)?.querySelector("[data-cs-deny-input]")?.value.trim();
        const audit = reason
          ? t("audit.denied", { cmd: "<code>$ cargo test …</code>", reason })
          : t("appr.resolved_deny_none");
        resolveCard(id, reason ? `${t("appr.resolved_deny")}：「${reason}」` : t("appr.resolved_deny_none"), audit);
        break;
      }
      case "allow": {
        const sc = scopeLabel[tgt.dataset.csScope || "once"]();
        resolveCard(id, t("appr.resolved_allow", { scope: sc }), t("audit.allowed", { cmd: "<code>$ cargo test …</code>", scope: sc }));
        break;
      }
      case "reset":
        rvState.revertedHunks = {};
        rvState.fileStates = ["pending", "pending"];
        render(current.id, variant);
        break;
      case "keep-planning": {
        revise = true;
        el.composer.placeholder = t("plan.hint");
        const w = $("[data-cs-plan-wait]");
        if (w) w.hidden = false;
        el.composer.focus();
        break;
      }
      case "approve-plan":
        stampPlan("", true);
        ov()?.toast(tgt.dataset.csMode === "ask" ? "已批准计划，按「每次询问」开始执行" : "已批准计划，按「自动批准只读」开始执行");
        break;
      case "pick":
        tgt.parentElement.querySelectorAll(".cs-opt").forEach((o) => o.classList.toggle("picked", o === tgt));
        break;
      case "resolve-gate":
        tgt.closest(".gate-card").outerHTML = banner("", tgt.dataset.csMsg, `<button class="btn btn-sm" data-cs="reset">还原演示</button>`);
        break;
      case "retry":
        $("#cs-err")?.remove();
        el.col.insertAdjacentHTML("beforeend", `<div class="cs-thinking">${CS.dot("run")}<span>${t("thread.thinking")}</span></div>`);
        ov()?.toast("已重新发起本轮对话（agent_message_retry）");
        break;
      case "dismiss-error": $("#cs-err")?.remove(); break;
      case "dismiss-parent": tgt.closest(".inline-banner")?.remove(); break;
      case "open-child": go("subagents", "child"); break;
      case "back-parent": go("subagents", "parallel"); break;
      case "ctx": {
        $(".cs-model-pop")?.remove();
        const pop = $(".cs-ctx-pop");
        if (pop) pop.remove();
        else el.ctxSlot.insertAdjacentHTML("beforeend", CS.contextPopover(!(current.id === "composer" && variant === "cannot")));
        break;
      }
      default: break;
    }
  });

  // ── Keyboard Shortcuts (Capture phase) ────────────────────────────────────
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape" && viewer) { closeViewer(); e.stopImmediatePropagation(); return; }
    if (e.key === "Escape" && ($(".cs-ctx-pop") || $(".cs-model-pop") || $("#cs-turn-metrics-pop"))) {
      $(".cs-ctx-pop")?.remove();
      $(".cs-model-pop")?.remove();
      $("#cs-turn-metrics-pop")?.remove();
      e.stopImmediatePropagation();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "o") {
      e.preventDefault();
      const order = ["compact", "normal", "verbose"];
      viewMode = order[(order.indexOf(viewMode) + 1) % order.length];
      syncHeaderI18n();
      if (current?.id === "turn") {
        render("turn", viewMode === "compact" ? "collapsed" : viewMode === "verbose" ? "verbose" : "expanded");
      }
      return;
    }
    if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key.toLowerCase() === "d") {
      e.preventDefault();
      rvState.open = !rvState.open;
      syncReviewPanel();
      return;
    }
    const appr = e.target.closest?.("[data-cs-appr]");
    if (appr) {
      const id = appr.dataset.csAppr;
      const inInput = e.target.matches("[data-cs-deny-input]");
      if (e.key === "Tab" && !inInput) {
        e.preventDefault();
        openDeny(id);
        return;
      }
      if (e.key === "Escape") {
        e.stopImmediatePropagation();
        e.preventDefault();
        if (inInput) closeDeny(id);
        else resolveCard(id, t("appr.resolved_deny_none"));
        return;
      }
      if (e.key === "Enter") {
        e.preventDefault();
        if (inInput) appr.querySelector('[data-cs="deny-confirm"]').click();
        else appr.querySelector('[data-cs="allow"]').click();
        return;
      }
    }
    if (e.key === "Escape" && e.target.closest?.("[data-cs-plan-gate]")) {
      e.stopImmediatePropagation();
      $('[data-cs="keep-planning"]')?.click();
      return;
    }
    const qg = e.target.closest?.("[data-cs-q-gate]");
    if (qg && !e.target.matches("input")) {
      const opts = [...qg.querySelectorAll(".cs-opt")];
      if (/^[1-9]$/.test(e.key) && opts[Number(e.key) - 1]) { opts[Number(e.key) - 1].click(); return; }
      if (e.key === "Escape") { e.stopImmediatePropagation(); qg.querySelector('[data-cs="resolve-gate"]').click(); return; }
      if (e.key === "Enter") { e.preventDefault(); qg.querySelectorAll('[data-cs="resolve-gate"]')[1].click(); return; }
    }
    if (e.target === el.composer && e.key === "Enter" && !e.shiftKey) {
      if (editingQueueIdx !== null) {
        e.preventDefault();
        $('[data-cs="save-edit-queue"]')?.click();
        return;
      }
      if (revise || $("[data-cs-plan-gate]")) {
        e.preventDefault();
        submitRevision();
        return;
      }
      if (current?.running) {
        e.preventDefault();
        if (e.metaKey || e.ctrlKey) $('[data-cs="steer-send"]')?.click();
        else $('[data-cs="queue-add"]')?.click();
        return;
      }
    }
    if (e.target === el.bar?.querySelector("select")) return;
    if (!e.target.closest("input, textarea, select") && (e.key === "[" || e.key === "]") && current) {
      step(e.key === "]" ? 1 : -1);
    }
  }, true);

  el.select?.addEventListener("change", () => go(el.select.value));

  // ── Scope Approval Dialog ─────────────────────────────────────────────────
  const prevSpec = window.PD.renderDialogSpec;
  window.PD.renderDialogSpec = (id, st) => {
    if (id !== "cs-scope") return prevSpec(id, st);
    const opt = (checked, title, sub, disabled) => `
      <label class="list-row clickable ${checked ? "selected" : ""}" style="background:var(--bg-surface);border:1px solid var(--border-subtle);${disabled ? "opacity:.55" : ""}">
        <input type="radio" name="cs-scope" ${checked ? "checked" : ""} ${disabled ? "disabled" : ""}/><div><b>${title}</b><div class="pop-opt-sub">${sub}</div></div></label>`;
    return {
      title: "选择允许范围 · shell",
      body: `<div class="gate-cmd"><span class="text-faint">$</span><span>cargo test -p spanbay-core router &amp;&amp; git diff --stat &gt; /tmp/router.diff</span></div>
        <div class="col-stack-sm">
          ${opt(true, t("res.once"), "仅放行这一次工具调用")}
          ${opt(false, "本对话内允许 <code>cargo test *</code>", "仅在当前对话生命周期内匹配，每次执行前校验参数")}
          ${opt(false, "本文件夹始终允许 <code>cargo test *</code>", "复合命令不可保存为文件夹级永久规则（包含 && 与 >）", true)}
          ${opt(false, "全局始终允许此工具", "写入本机权限配置，可在「设置 › 智能体 › 权限」随时撤销")}
        </div>`,
      foot: `<button class="btn" data-action="close-dialog">${t("common.cancel")}</button><button class="btn btn-primary" data-dialog-done="已保存允许范围并继续执行" data-cs="allow" data-cs-id="a-cmd" data-cs-scope="session">保存并允许</button>`,
    };
  };

  // ── Boot ──────────────────────────────────────────────────────────────────
  populateSelect();
  const q = new URLSearchParams(location.search);
  if (q.get("lang") === "en") CS.lang = "en";
  const initialChat = q.get("chat");
  if (initialChat && (byId[initialChat] || ALIASES[initialChat])) {
    go(initialChat, q.get("v") || undefined);
  } else {
    render("turn", "expanded");
  }
})();
