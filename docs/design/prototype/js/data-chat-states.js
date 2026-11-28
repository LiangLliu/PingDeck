/* SpanBay Prototype — Agent Chat Redesign State Data & Templates (.overhaul/chat-redesign-spec.md).
   Bilingual UI chrome via `t()`, lowercase `token`, status words per docs/design.md §6,
   and exact one-line tool labels per .overhaul/chat-redesign-research.md. */
window.PD = window.PD || {};

(() => {
  const CS = (window.PD.CS = { lang: "zh" });

  // ── Bilingual UI strings (zh-CN + en) ──────────────────────────────────────
  const L = {
    zh: {
      "pane.title": "对话", "pane.new": "+ 新建", "pane.search": "搜索对话…",
      "pane.needs_you": "▲ 待你处理", "pane.pinned": "已置顶", "pane.recent": "最近对话",
      "pane.archived": "已归档", "pane.trash": "最近删除",
      "head.compact": "紧凑", "head.normal": "常规", "head.verbose": "详细",
      "head.workflow": "工作流", "head.more": "更多操作",

      // Status words (docs/design.md §2.2 & §6)
      "st.pending": "待开始", "st.queued": "排队中", "st.running": "运行中", "st.completed": "完成",
      "st.failed": "失败", "st.interrupted": "已中断", "st.blocked": "已阻止", "st.timed_out": "超时未运行",
      "st.denied": "已拒绝", "st.cancelled": "已取消", "st.expired": "已过期", "st.superseded": "已被取代",
      "st.skipped": "已跳过", "st.wait.approval": "等待批准", "st.wait.plan": "等待确认",
      "st.wait.question": "等待回答", "st.wait.cost": "费用暂停",

      // Exact one-line tool & turn labels (spec §4.2)
      "turn.fold": "已工作 {d} · 编辑 {e} 个文件，已探索 {f} 个文件，{s} 次搜索",
      "turn.fold_ro": "已工作 {d} · 已探索 {f} 个文件，{s} 次搜索",
      "exp.running": "正在探索 · 已读 {f} 个文件，{s} 次搜索",
      "exp.done": "已探索 {f} 个文件 · {s} 次搜索",
      "exp.read": "读取 {files}", "exp.list": "列出 {dir}",
      "exp.search": "搜索 {q} in {dir} · {n} 处命中",
      "exp.search_none": "搜索 {q} in {dir} · (无匹配)",
      "tool.edit": "编辑 {path}", "tool.add": "新建 {path}", "tool.del": "删除 {path}",
      "tool.diff_btn": "查看差异",
      "tool.sh_run": "正在运行", "tool.sh_ok": "已运行", "tool.sh_fail": "未能运行命令",
      "tool.web_search": "已搜索网页 \"{q}\" · {n} 个来源",
      "tool.web_fetch": "已抓取 {host}",
      "tool.api_send": "发送请求", "tool.mcp_call": "调用 {server} · {tool}",
      "tool.sub_inline": "运行子智能体 {model} · {role}：{task}",
      "audit.allowed": "✓ 已允许运行 {cmd}（{scope}）",
      "audit.denied": "□ 已拒绝运行 {cmd}：「{reason}」",

      // Thread & Thinking
      "thread.thinking": "思考中…", "thread.thought": "思考了 {s} 秒",
      "thread.expand": "展开 ▸", "thread.collapse": "收起 ▾",
      "thread.load_earlier": "加载更早的消息", "thread.earliest": "已是最早的消息", "thread.jump": "跳到最新 ↓",
      "thread.compacted": "上下文已压缩 · {before} → {after} token",
      "thread.mcp": "正在连接 MCP 服务器：{names}",
      "thread.degraded": "实时更新已中断，正在后台重连（下次 {s} 秒后）。线程内容可能不是最新。",
      "thread.retry_now": "立即重连", "sources": "来源",
      "common.retry": "重试", "common.close": "关闭", "common.details": "详情", "common.cancel": "取消",

      // Hover actions & Metrics popover
      "act.copy": "复制", "act.copied": "✓ 已复制", "act.retry": "重试此轮",
      "act.edit_resend": "编辑并重发", "act.fork": "从此处分支",
      "act.workflow": "查看执行图", "act.delete": "删除",
      "met.title": "本轮调用与消耗明细", "met.route": "实际服务模型",
      "met.steps": "模型步数与工具", "met.steps_val": "3 步 · 7 次工具调用（已隐藏 1 次 todo_write）",
      "met.in_out": "输入 / 输出", "met.reason_cache": "推理 / 缓存命中",
      "met.ctx": "上下文窗口峰值", "met.speed": "首字 / 生成速度", "met.cost": "本轮估算费用",

      // Post-turn Changes Card & Safe Undo Modal
      "chg.title": "{n} 个文件已更改", "chg.undone_title": "{n} 个文件已更改 · 已撤销",
      "chg.undo_turn": "撤销本轮修改", "chg.review_btn": "审查变更 ▸",
      "chg.st_pending": "待审阅", "chg.st_kept": "已保留", "chg.st_undone": "已撤销", "chg.st_conflict": "外部已改",
      "chg.kind_edit": "编辑", "chg.kind_add": "新建",
      "chg.untracked_warn": "另有 1 个文件由命令在文件工具外修改（Cargo.lock），自动撤销将忽略它。",
      "undo.modal_title": "撤销本轮文件修改预检（SHA-256 校验）",
      "undo.safe": "可安全撤销 (1)", "undo.conflict": "不能安全撤销 (1 · 文件已被外部修改)",
      "undo.ignored": "已忽略 (1 · bash/shell 命令修改)",
      "undo.safe_only": "仅撤销安全文件 (1)", "undo.force_all": "强制覆盖还原全部 (2)",

      // Right Review Panel
      "rv.title": "审查变更", "rv.scope_turn": "本轮", "rv.scope_all": "本对话",
      "rv.keep_all": "全部保留", "rv.undo_all": "全部撤销",
      "rv.net_diff": "本轮净变更", "rv.per_edit": "单次编辑 (2)",
      "rv.keep": "保留", "rv.undo": "撤销", "rv.revert_hunk": "撤销此块",
      "rv.hunk_reverted": "已撤销此代码块",
      "rv.comment_ph": "输入对此行的修改意见（按 ↵ 挂入底部输入框，支持多处批注批量发回）…",
      "rv.comment_add": "添加批注芯片 (↵)",

      // Trace details
      "trace.failed": "{n} 项失败", "trace.exit": "退出码 {code}",
      "trace.open_request": "在请求中打开 ↗", "trace.full_output": "在右侧查看完整输出 (2.1 MB)",
      "trace.tech": "技术细节",
      "res.once": "本次允许", "res.session": "本对话允许", "res.folder": "本文件夹允许",
      "res.global": "全局允许", "res.denied": "已拒绝", "res.timed_out": "审批超时",

      // Approvals
      "appr.deny": "拒绝", "appr.deny_ph": "告诉智能体改用什么方案（按 ↵ 拒绝并继续本轮，不填则直接拒绝）…",
      "appr.confirm_deny": "拒绝并继续",
      "appr.allow_once": "允许一次 (↵)", "appr.allow_net": "允许一次（联网）",
      "appr.scope": "允许范围 ▾",
      "appr.esc": "↵ 允许一次 · Tab 填写反馈并继续 · Esc 直接拒绝",
      "risk.low": "低风险", "risk.medium": "中风险", "risk.high": "高风险",
      "tag.chained": "组合命令", "tag.no_sandbox": "无系统沙箱", "tag.sandbox_off": "沙箱已关闭",
      "layer.forced_ask": "强制询问规则触发", "layer.mode_default": "本对话「自动批准只读」模式触发",
      "sb.seatbelt": "Seatbelt 沙箱内运行", "sb.none": "不在系统沙箱中运行",
      "why.url_provenance": "此主机未在对话、附件或已批准调用中出现过",
      "why.shell_remote": "命令将向远端仓库推送代码",
      "appr.cwd": "工作目录：{cwd}", "appr.waited": "已等待 {t}",
      "appr.resolved_allow": "✓ 已允许（{scope}），智能体继续运行",
      "appr.resolved_deny": "□ 已拒绝，反馈已交给智能体继续调整",
      "appr.resolved_deny_none": "□ 已拒绝（未附反馈）",

      // Gates: Plan, Question, Cost
      "plan.keep": "提出修改意见", "plan.approve_ask": "批准 · 每次询问", "plan.approve_go": "批准并执行 (↵)",
      "plan.hint": "直接在下方输入框输入修改意见并按 ↵ 发送（自动修订计划）",
      "plan.revise_waiting": "正在修订计划：在下方输入框写修改意见，按 ↵ 发送",
      "plan.stamp_revise": "已要求修改：{text}", "plan.stamp_approved": "已批准 · 按「自动批准只读」执行",
      "q.skip": "跳过 (Esc)", "q.answer": "回答 (↵)", "q.other": "或输入自定义回答…",
      "cost.title": "已达到本次运行的费用上限", "cost.stop": "停止运行", "cost.go": "继续运行 (↵)",
      "cost.main": "主智能体", "cost.subagent": "子智能体", "cost.compaction": "上下文压缩",
      "cost.next": "继续后下一次在 {amount} 暂停",
      "cost.unpriced": "{n} 个模型没有价格，不计入上限：{models}",

      // Running, Queue & Todos
      "run.stop": "停止运行", "run.step": "第 {steps} 步 · {s}s",
      "queue.title": "待发队列 ({n})", "queue.steer_now": "立即插队 ⌘↵",
      "queue.edit": "编辑 ✎", "queue.editing": "正在编辑排队消息 #{i}（按 ↵ 保存回队列）",
      "queue.paused": "队列已暂停 · 停止后不会自动发送 · 待发 {n} 条", "queue.resume": "继续并发送",
      "todos.summary": "任务 {done}/{total}", "todos.current": "当前：{c}",

      // Subagents
      "sub.parallel": "并行子智能体 ×{n}", "sub.open": "打开子对话", "sub.inspect": "右侧预览",
      "sub.stop": "停止", "sub.steps": "工具步骤",
      "sub.back": "← 返回父对话 · {title}", "sub.child_of": "子智能体对话",

      // Errors & Notices
      "end.cancelled": "已取消 · 运行了 {d} · {steps} 步 · 改动 {files} 个文件 · {cost}",
      "end.review": "审查变更 ▸",
      "end.max_steps": "已到步数上限（{n} 步）：最后一步写出了已完成项与剩余工作",
      "fail.all_targets_failed": "此对话路由到的模型全部失败",
      "fail.key_rejected": "服务商拒绝了该密钥",
      "fail.refused_or_empty": "模型拒绝回答或返回为空",
      "fail.stalled": "模型流式响应超时停滞",
      "fail.mcp_needs_token": "MCP 服务器需要凭据",
      "fail.context_full": "对话已超出模型的上下文窗口",
      "fail.offline": "无法连接到服务商",
      "fail.generic": "运行失败",
      "notice.retrying": "模型调用失败（{reason}），{delay} 秒后重试（第 {attempt}/{of} 次）。",
      "notice.fallback": "{from} 失败（{reason}），已自动切至备用模型 {to} 回答。",
      "notice.untracked": "本回合有 {n} 个文件在文件工具之外被改动：{paths}。",
      "notice.full_access_off": "应用重启后此对话的「完全权限」已复位为「自动批准只读」。",
      "notice.dismiss": "知道了",
      "skills.unavailable": "技能 /{name} 在此对话中不可用，未发送",
      "skills.unreadable": "技能 /{name} 无法读取，未发送",
      "skills.too_long": "技能 /{name} 过长未直接内联，已指示模型按需读取",

      // Context & Composer
      "ctx.title": "上下文窗口", "ctx.used": "{used} / {limit} token (14%)", "ctx.compact": "立即压缩",
      "ctx.cannot": "没有可总结的更早历史（最近 2 轮总是保留）", "ctx.session": "本对话累计消耗",
      "ctx.system": "系统提示词", "ctx.instructions": "项目指令", "ctx.skills": "技能",
      "ctx.tools": "工具定义", "ctx.messages": "消息", "ctx.results": "工具结果", "ctx.attachments": "附件",
      "search.matches": "消息与工具输出命中", "search.tool": "工具输出",
      "search.empty": "没有找到包含「{q}」的对话", "search.clear": "清除搜索",
      "viewer.fit": "适应窗口", "viewer.actual": "实际大小", "viewer.open": "用系统应用打开",
      "media.remote": "远程图片 · {host}",
      "composer.ph": "输入消息或编码任务… (↵ 发送 · @ 引用文件 · / 技能)",
      "composer.ph_run": "输入补充指令… (↵ 加入待发队列 · ⌘↵ 立即插队引导当前轮)",
    },
    en: {
      "pane.title": "Chats", "pane.new": "+ New", "pane.search": "Search chats…",
      "pane.needs_you": "▲ Needs you", "pane.pinned": "Pinned", "pane.recent": "Recent",
      "pane.archived": "Archived", "pane.trash": "Trash",
      "head.compact": "Compact", "head.normal": "Normal", "head.verbose": "Verbose",
      "head.workflow": "Workflow", "head.more": "More actions",

      "st.pending": "Pending", "st.queued": "Queued", "st.running": "Running", "st.completed": "Completed",
      "st.failed": "Failed", "st.interrupted": "Interrupted", "st.blocked": "Blocked", "st.timed_out": "Timed out",
      "st.denied": "Denied", "st.cancelled": "Cancelled", "st.expired": "Expired", "st.superseded": "Superseded",
      "st.skipped": "Skipped", "st.wait.approval": "Awaiting approval", "st.wait.plan": "Awaiting confirmation",
      "st.wait.question": "Awaiting answer", "st.wait.cost": "Cost pause",

      // Exact one-line tool & turn labels (Codex / Claude Code / ZCode verified)
      "turn.fold": "Worked for {d} · Edited {e} files, explored {f} files, {s} searches",
      "turn.fold_ro": "Worked for {d} · Explored {f} files, {s} searches",
      "exp.running": "Exploring · {f} files, {s} search",
      "exp.done": "Explored {f} files, {s} searches",
      "exp.read": "Read {files}", "exp.list": "List {dir}",
      "exp.search": "Search {q} in {dir} · {n} matches",
      "exp.search_none": "Search {q} in {dir} · (exit 1)",
      "tool.edit": "Edited {path}", "tool.add": "Added {path}", "tool.del": "Deleted {path}",
      "tool.diff_btn": "Diff",
      "tool.sh_run": "Running", "tool.sh_ok": "Ran", "tool.sh_fail": "Exited (status 101)",
      "tool.web_search": "Searched the web for \"{q}\" · {n} sources",
      "tool.web_fetch": "Fetched {host}",
      "tool.api_send": "Sent request", "tool.mcp_call": "Called {server} · {tool}",
      "tool.sub_inline": "Ran agent {model} · {role}: {task}",
      "audit.allowed": "✔ Approved to run {cmd} ({scope})",
      "audit.denied": "✗ Did not approve {cmd}: \"{reason}\"",

      "thread.thinking": "Thinking…", "thread.thought": "Thought for {s}s",
      "thread.expand": "Expand ▸", "thread.collapse": "Collapse ▾",
      "thread.load_earlier": "Load earlier messages", "thread.earliest": "This is the first message", "thread.jump": "Jump to latest ↓",
      "thread.compacted": "Context compacted · {before} → {after} tokens",
      "thread.mcp": "Connecting MCP servers: {names}",
      "thread.degraded": "Live updates stopped; reconnecting in the background (next try in {s}s).",
      "thread.retry_now": "Reconnect now", "sources": "Sources",
      "common.retry": "Retry", "common.close": "Close", "common.details": "Details", "common.cancel": "Cancel",

      "act.copy": "Copy", "act.copied": "✓ Copied", "act.retry": "Retry turn",
      "act.edit_resend": "Edit & resend", "act.fork": "Fork from here",
      "act.workflow": "Execution graph", "act.delete": "Delete",
      "met.title": "Turn Execution & Usage Breakdown", "met.route": "Routed target",
      "met.steps": "Steps & tool calls", "met.steps_val": "3 steps · 7 tool calls (1 todo_write hidden)",
      "met.in_out": "Input / Output", "met.reason_cache": "Reasoning / Cache hit",
      "met.ctx": "Peak context window", "met.speed": "TTFT / Speed", "met.cost": "Estimated turn cost",

      "chg.title": "{n} files edited", "chg.undone_title": "{n} files edited · Undone",
      "chg.undo_turn": "Undo turn", "chg.review_btn": "Review ▸",
      "chg.st_pending": "Pending", "chg.st_kept": "Kept", "chg.st_undone": "Undone", "chg.st_conflict": "Conflict",
      "chg.kind_edit": "Edited", "chg.kind_add": "Added",
      "chg.untracked_warn": "1 file changed outside file tools (Cargo.lock); automatic undo ignores shell changes.",
      "undo.modal_title": "Safe Undo Precheck (SHA-256 verification)",
      "undo.safe": "Safe to undo (1)", "undo.conflict": "Cannot safely undo (1 · modified externally)",
      "undo.ignored": "Ignored (1 · bash/shell modification)",
      "undo.safe_only": "Undo safe files only (1)", "undo.force_all": "Force overwrite all (2)",

      "rv.title": "Review changes", "rv.scope_turn": "Last turn", "rv.scope_all": "All turns",
      "rv.keep_all": "Keep all", "rv.undo_all": "Undo all",
      "rv.net_diff": "Net diff", "rv.per_edit": "Per edit (2)",
      "rv.keep": "Keep", "rv.undo": "Undo", "rv.revert_hunk": "Revert hunk",
      "rv.hunk_reverted": "Hunk reverted",
      "rv.comment_ph": "Add line review comment (↵ attaches chip to composer for batch follow-up)…",
      "rv.comment_add": "Attach comment (↵)",

      "trace.failed": "{n} failed", "trace.exit": "Exit code {code}",
      "trace.open_request": "Open in Requests ↗", "trace.full_output": "View full output in side panel (2.1 MB)",
      "trace.tech": "Technical details",
      "res.once": "Allowed once", "res.session": "Allowed in this chat", "res.folder": "Allowed in this folder",
      "res.global": "Allowed everywhere", "res.denied": "Denied", "res.timed_out": "Approval timed out",

      "appr.deny": "Deny", "appr.deny_ph": "Tell the agent what to do instead (↵ denies and continues turn)…",
      "appr.confirm_deny": "Deny & continue",
      "appr.allow_once": "Allow once (↵)", "appr.allow_net": "Allow once with network",
      "appr.scope": "Allow scope ▾",
      "appr.esc": "↵ Allow once · Tab add feedback & continue · Esc deny immediately",
      "risk.low": "Low risk", "risk.medium": "Medium risk", "risk.high": "High risk",
      "tag.chained": "Chained command", "tag.no_sandbox": "No OS sandbox", "tag.sandbox_off": "Sandbox off",
      "layer.forced_ask": "Asked by forced-ask rule", "layer.mode_default": "Asked by Auto-approve reads mode",
      "sb.seatbelt": "Runs in Seatbelt sandbox", "sb.none": "Runs without OS sandbox",
      "why.url_provenance": "Host has not appeared in this chat, attachments or approved calls",
      "why.shell_remote": "Command pushes to a remote repository",
      "appr.cwd": "Working directory: {cwd}", "appr.waited": "Waiting {t}",
      "appr.resolved_allow": "✔ Allowed ({scope}); the agent continues",
      "appr.resolved_deny": "✗ Denied with feedback; the agent will adjust its approach",
      "appr.resolved_deny_none": "✗ Denied (no feedback given)",

      "plan.keep": "Request changes", "plan.approve_ask": "Approve · ask each time", "plan.approve_go": "Approve and run (↵)",
      "plan.hint": "Type your plan revisions below and press ↵ to send",
      "plan.revise_waiting": "Revising plan: type your changes in the composer and press ↵",
      "plan.stamp_revise": "Changes requested: {text}", "plan.stamp_approved": "Approved · runs with Auto-approve reads",
      "q.skip": "Skip (Esc)", "q.answer": "Answer (↵)", "q.other": "Or type a custom answer…",
      "cost.title": "This run reached its cost cap", "cost.stop": "Stop run", "cost.go": "Continue (↵)",
      "cost.main": "Main agent", "cost.subagent": "Sub-agents", "cost.compaction": "Compaction",
      "cost.next": "If you continue, it pauses again at {amount}",
      "cost.unpriced": "{n} models have no price and count against no cap: {models}",

      "run.stop": "Stop run", "run.step": "Step {steps} · {s}s",
      "queue.title": "Queued ({n})", "queue.steer_now": "Steer now ⌘↵",
      "queue.edit": "Edit ✎", "queue.editing": "Editing queued message #{i} (↵ saves back to queue)",
      "queue.paused": "Queue paused · nothing is sent after a stop · {n} waiting", "queue.resume": "Resume & send",
      "todos.summary": "Tasks {done}/{total}", "todos.current": "Now: {c}",

      "sub.parallel": "Parallel sub-agents ×{n}", "sub.open": "Open child chat", "sub.inspect": "Inspect in side panel",
      "sub.stop": "Stop", "sub.steps": "Tool steps",
      "sub.back": "← Back to parent · {title}", "sub.child_of": "Sub-agent chat",

      "end.cancelled": "Cancelled · ran for {d} · {steps} steps · {files} files changed · {cost}",
      "end.review": "Review changes ▸",
      "end.max_steps": "Step limit reached ({n} steps): the last step summarises completed and remaining work",
      "fail.all_targets_failed": "Every model this chat routes to failed",
      "fail.key_rejected": "The provider rejected the API key",
      "fail.refused_or_empty": "The model refused or returned an empty response",
      "fail.stalled": "Model stream stalled without bytes",
      "fail.mcp_needs_token": "An MCP server requires credentials",
      "fail.context_full": "The chat exceeds the model's context window",
      "fail.offline": "Could not reach the provider",
      "fail.generic": "The run failed",
      "notice.retrying": "Model call failed ({reason}); retrying in {delay}s (attempt {attempt}/{of}).",
      "notice.fallback": "{from} failed ({reason}); routed to fallback {to}.",
      "notice.untracked": "{n} files changed outside file tools this turn: {paths}.",
      "notice.full_access_off": "Full access was reset to Auto-approve reads after app restart.",
      "notice.dismiss": "Got it",
      "skills.unavailable": "Skill /{name} is not available in this chat and was not sent",
      "skills.unreadable": "Skill /{name} could not be read and was not sent",
      "skills.too_long": "Skill /{name} was too long to inline; the model will read it on demand",

      "ctx.title": "Context window", "ctx.used": "{used} / {limit} tokens (14%)", "ctx.compact": "Compact now",
      "ctx.cannot": "Nothing earlier to summarise (the last 2 turns are always kept)", "ctx.session": "Chat totals",
      "ctx.system": "System prompt", "ctx.instructions": "Project instructions", "ctx.skills": "Skills",
      "ctx.tools": "Tool definitions", "ctx.messages": "Messages", "ctx.results": "Tool results", "ctx.attachments": "Attachments",
      "search.matches": "Matches in messages & tool output", "search.tool": "Tool output",
      "search.empty": "No chat contains “{q}”", "search.clear": "Clear search",
      "viewer.fit": "Fit", "viewer.actual": "Actual size", "viewer.open": "Open in system app",
      "media.remote": "Remote image · {host}",
      "composer.ph": "Ask or describe a coding task… (↵ send · @ mention · / skills)",
      "composer.ph_run": "Type follow-up… (↵ queue for next turn · ⌘↵ steer current run immediately)",
    },
  };
  const t = (k, a = {}) => String(L[CS.lang][k] ?? L.zh[k] ?? k).replace(/\{(\w+)\}/g, (_, n) => (a[n] ?? `{${n}}`));
  CS.t = t;
  CS.L = L;

  // ── Unified status table (§c.1 / docs/design.md §2.2 & §6) ────────────────
  const STATUS = {
    pending: { shape: "idle", tone: "neutral" }, queued: { shape: "idle", tone: "neutral" },
    running: { shape: "run", tone: "neutral" }, waiting: { shape: "warn", tone: "warn" },
    completed: { shape: "ok", tone: "neutral" }, failed: { shape: "bad", tone: "bad" },
    interrupted: { shape: "warn", tone: "warn" }, blocked: { shape: "warn", tone: "warn" },
    timed_out: { shape: "warn", tone: "warn" }, denied: { shape: "stopped", tone: "neutral" },
    cancelled: { shape: "stopped", tone: "neutral" }, expired: { shape: "stopped", tone: "neutral" },
    superseded: { shape: "stopped", tone: "neutral" }, skipped: { shape: "stopped", tone: "neutral" },
  };
  CS.STATUS = STATUS;
  const label = (st, wait) => (st === "waiting" ? t(`st.wait.${wait || "approval"}`) : t(`st.${st}`));
  const dot = (shape) => `<span class="dot dot-${shape}" aria-hidden="true"></span>`;
  const word = (st, wait) => `<span class="cs-word ${STATUS[st].tone === "neutral" ? "" : STATUS[st].tone}">${label(st, wait)}</span>`;
  const badge = (st, wait) => `<span class="cs-badge">${dot(STATUS[st].shape)}${word(st, wait)}</span>`;
  CS.label = label; CS.dot = dot; CS.badge = badge;

  const tag = (txt, tone = "") => `<span class="tag ${tone ? `tag-${tone}` : ""}">${txt}</span>`;
  const exitBadge = (c) => tag(t("trace.exit", { code: c }), "bad");
  const banner = (tone, msg, actions = "") =>
    `<div class="inline-banner ${tone}"><span class="row-center">${dot(tone === "bad" ? "bad" : tone === "warn" ? "warn" : "idle")}<span>${msg}</span></span><span class="row-center">${actions}</span></div>`;
  CS.banner = banner;

  // ── Fictional media ───────────────────────────────────────────────────────
  const shotSvg = () => `
    <svg viewBox="0 0 1440 900" role="img" aria-label="gateway-429.png">
      <rect width="1440" height="900" style="fill:var(--bg-surface)"/>
      <rect width="1440" height="56" style="fill:var(--bg-pane)"/>
      <circle cx="34" cy="28" r="9" style="fill:var(--bad)"/><circle cx="62" cy="28" r="9" style="fill:var(--warn)"/><circle cx="90" cy="28" r="9" style="fill:var(--ok)"/>
      <text x="140" y="36" font-size="22" style="fill:var(--ink-3);font-family:var(--font-mono)">claude — ~/me/SpanBay</text>
      <g font-size="26" style="font-family:var(--font-mono)">
        <text x="60" y="140" style="fill:var(--ink-2)">&gt; Summarise the failing invoice tests</text>
        <text x="60" y="210" style="fill:var(--bad)">API Error: 429 Too Many Requests</text>
        <text x="60" y="260" style="fill:var(--ink-3)">  model: deepseek-v4-pro  via http://127.0.0.1:4141</text>
        <text x="60" y="310" style="fill:var(--ink-3)">  retry-after: 2</text>
        <text x="60" y="380" style="fill:var(--bad)">API Error: 429 Too Many Requests (attempt 2/2)</text>
      </g>
    </svg>`;
  const chartSvg = () => `
    <svg viewBox="0 0 480 300" role="img" aria-label="429 per hour">
      <rect width="480" height="300" style="fill:var(--bg-surface)"/>
      <text x="16" y="28" font-size="14" style="fill:var(--ink-2);font-family:var(--font-ui)">429 / hour · gateway · Nov 26</text>
      <polyline points="20,250 70,240 120,236 170,180 220,90 270,70 320,150 370,210 420,232 460,240" fill="none" stroke-width="3" style="stroke:var(--warn)"/>
      <line x1="20" y1="260" x2="460" y2="260" stroke-width="1" style="stroke:var(--border-default)"/>
    </svg>`;
  CS.MEDIA = {
    shot: { name: "gateway-429.png", meta: "1440×900 · 179 KB", svg: shotSvg },
    chart: { name: "rate-limit-chart.png", meta: "480×300 · MCP github", svg: chartSvg },
  };
  const thumb = (key) => `<button class="cs-thumb" data-cs="viewer" data-cs-media="${key}" title="${CS.MEDIA[key].name}">${CS.MEDIA[key].svg()}</button>`;
  const chip = (name, sub = "", cls = "", rmAttr = "") =>
    `<span class="cs-chip ${cls}">${name}${sub ? `<span class="sub">${sub}</span>` : ""}${rmAttr ? `<button class="rm" ${rmAttr} title="${t("common.close")}">×</button>` : ""}</span>`;
  const linkChip = (name, url) => `<button class="cs-chip link" data-toast="由核心校验后在系统浏览器打开：${url}" title="${url}">${name}</button>`;

  // ── Redesigned Turn Primitives (One Turn = One Block, No Assistant Bubbles) ─
  const USER_429 = "网关这两天老是 429，日志在附件里，截图是 Claude Code 那边的报错，帮查是上游限流还是 router 熔断太敏感，并修复它。";

  const userTurn = (text, { atts = "", warnLines = "", forceHover = false, id = "u1" } = {}) => `
    <div class="user-turn ${forceHover ? "force-hover" : ""}" data-turn-user="${id}">
      ${atts}
      <div class="user-turn-row">
        <div class="turn-hover-actions" role="toolbar" aria-label="用户消息操作">
          <button class="turn-act-btn" data-cs="copy-text" data-cs-text="${text.replace(/"/g, "&quot;")}">${t("act.copy")}</button>
          <button class="turn-act-btn" data-cs="edit-user-msg" data-cs-text="${text.replace(/"/g, "&quot;")}">${t("act.edit_resend")}</button>
          <button class="turn-act-btn" data-toast="已从此处创建分支会话（继承此前上下文与文件状态）">${t("act.fork")}</button>
          <button class="turn-act-btn" data-toast="已删除此轮对话">${t("act.delete")}</button>
        </div>
        <div class="user-bubble">${text}</div>
      </div>
      ${warnLines}
    </div>`;
  const userWithAtts = (opts = {}) =>
    userTurn(USER_429, { ...opts, atts: `<div class="cs-atts">${thumb("shot")}${chip("gateway-log-1106.json", "209 KB")}</div>` });

  /** Single-line Spine Row: single status glyph on left, chevron (› / ⌄) at right end. */
  const spRow = ({ id, st = "completed", chev = "", title, meta = "", trailing = "", detail = "", open = false, badDetail = false, wait, clickAttr = "" }) => {
    const showWord = st !== "running" && st !== "completed";
    const c = chev || (detail ? (open ? "⌄" : "›") : "");
    const toggleAttr = detail ? `data-cs="toggle" data-cs-target="${id}" aria-expanded="${open}"` : clickAttr;
    return `
      <div class="sp-row" role="button" tabindex="0" ${toggleAttr}>
        ${dot(STATUS[st].shape)}
        <span class="sp-title">${title}</span>
        ${showWord ? word(st, wait) : ""}
        ${trailing}
        ${meta ? `<span class="sp-meta">${meta}</span>` : ""}
        ${c ? `<span class="sp-chev">${c}</span>` : ""}
      </div>
      ${detail ? `<div class="${badDetail ? "cs-detail bad" : "cs-detail"}" id="${id}" ${open ? "" : "hidden"}>${detail}</div>` : ""}`;
  };
  CS.spRow = spRow;

  /** Borderless single-line Thinking row (`● 思考了 1.4 秒 ›`). */
  const thinkRow = (id, secs, body, open = false) => `
    <div class="sp-row" role="button" tabindex="0" data-cs="toggle" data-cs-target="${id}" aria-expanded="${open}">
      ${dot("ok")}
      <span class="sp-title text-muted">${t("thread.thought", { s: secs })}</span>
      <span class="sp-chev">${open ? "⌄" : "›"}</span>
    </div>
    <div id="${id}" class="sp-think-body" ${open ? "" : "hidden"}>${body}</div>`;

  /** Consecutive Read/Search/List group (`● 已探索 3 个文件 · 2 次搜索 ›`) with Codex `└` sub-rows. */
  const exploreGroup = ({ id = "exp-1", running = false, files = 3, searches = 2, open = false } = {}) => {
    const headTxt = running ? t("exp.running", { f: files, s: searches }) : t("exp.done", { f: files, s: searches });
    return `
      <div class="sp-row" role="button" tabindex="0" data-cs="toggle" data-cs-target="${id}" aria-expanded="${open}">
        ${dot(running ? "run" : "ok")}
        <span class="sp-title"><b>${headTxt}</b></span>
        <span class="sp-meta">${running ? "1.1s" : "0.7s"}</span>
        <span class="sp-chev">${open ? "⌄" : "›"}</span>
      </div>
      <div id="${id}" class="sp-tree" ${open ? "" : "hidden"}>
        <div class="sp-tree-row" role="button" tabindex="0" data-cs="open-review-file" data-cs-file="0">
          <span class="sp-branch">└</span>
          <span class="flex-1">${t("exp.read", { files: "<code>router.rs</code>, <code>limits.rs</code>, <code>gateway-log-1106.json</code>" })}</span>
          <span class="sp-meta">第 180–260 行</span>
        </div>
        <div class="sp-tree-row">
          <span class="sp-branch">└</span>
          <span class="flex-1">${t("exp.list", { dir: "<code>crates/spanbay-core/src/llm</code>" })}</span>
          <span class="sp-meta">8 项</span>
        </div>
        <div class="sp-tree-row">
          <span class="sp-branch">└</span>
          <span class="flex-1">${t("exp.search", { q: "<code>\"retry-after\"</code>", dir: "<code>crates/spanbay-core</code>", n: 12 })}</span>
          <span class="sp-meta">0.2s</span>
        </div>
        <div class="sp-tree-row">
          <span class="sp-branch">└</span>
          <span class="flex-1 text-faint">${t("exp.search_none", { q: "<code>\"x-ratelimit-reset\"</code>", dir: "<code>crates/spanbay-core</code>" })}</span>
          <span class="sp-meta">0.1s</span>
        </div>
      </div>`;
  };

  /** Single-line File Edit row (`● 编辑 crates/.../router.rs +18 −4 查看差异 ›`). */
  const editRow = ({ path, added, removed, kind = "edit", fileIdx = 0, verboseDiff = "" }) => {
    const lbl = t(`tool.${kind}`, { path: `<code>${path}</code>` });
    const id = `ed-inline-${fileIdx}`;
    return `
      <div class="sp-row" role="button" tabindex="0" data-cs="open-review-file" data-cs-file="${fileIdx}">
        ${dot("ok")}
        <span class="sp-title">${lbl}</span>
        <span class="diff-stat mono fs-xs"><span class="add">+${added}</span> <span class="del">−${removed}</span></span>
        <span class="sp-action">${t("tool.diff_btn")} ›</span>
      </div>
      ${verboseDiff ? `<div id="${id}" class="cs-detail">${verboseDiff}</div>` : ""}`;
  };
  /** Shell command row: exit 0 collapsed by default; exit != 0 auto-expanded! */
  const outBlock = (segs) => `<pre class="cs-out">${segs.map(([s, x]) => `<span class="${s === "err" ? "stderr" : ""}"><span class="stream-tag">${s === "err" ? "stderr │ " : "stdout │ "}</span>${x}</span>`).join("\n")}</pre>`;
  const shellRow = ({ id, st = "completed", cmd, dur = "0.8s", exitCode = 0, lines = [], omitted = 0, open = false }) => {
    const verb = st === "running" ? t("tool.sh_run") : st === "failed" ? t("tool.sh_fail") : t("tool.sh_ok");
    const detail = lines.length
      ? `${outBlock(lines)}${omitted ? `<div class="row-between fs-2xs text-muted"><span>… 其余 ${omitted} 行已折叠</span><button class="text-accent" data-cs="open-review-output">${t("trace.full_output")}</button></div>` : ""}`
      : "";
    return spRow({
      id,
      st,
      title: `${verb} <code>$ ${cmd}</code>`,
      meta: dur,
      trailing: exitCode !== 0 ? exitBadge(exitCode) : "",
      detail,
      open: open || exitCode !== 0 || st === "failed",
      badDetail: exitCode !== 0 || st === "failed",
    });
  };

  /** Post-Turn File Changes Summary Card (`2 个文件已更改 +24 −6 [撤销本轮修改] [审查变更 ▸]`). */
  const changesSummaryCard = ({ undone = false, showUndoCheck = false, fileStates = ["pending", "pending"] } = {}) => {
    const stTag = (st) => {
      if (undone || st === "undone") return tag(t("chg.st_undone"));
      if (st === "kept") return tag(t("chg.st_kept"), "ok");
      if (st === "conflict") return tag(t("chg.st_conflict"), "bad");
      return tag(t("chg.st_pending"), "warn");
    };
    return `
      <div class="turn-changes-card" id="cs-turn-changes">
        <div class="tcc-head">
          ${dot(undone ? "stopped" : "ok")}
          <span>${undone ? t("chg.undone_title", { n: 2 }) : t("chg.title", { n: 2 })}</span>
          <span class="diff-stat mono"><span class="add">+24</span> <span class="del">−6</span></span>
          <div class="ml-auto row-center" style="gap:6px">
            ${undone ? `<button class="btn btn-sm" data-cs="reset">${t("common.retry")}</button>` : `
              <button class="btn btn-sm" data-cs="undo-turn-prompt">${t("chg.undo_turn")}</button>
              <button class="btn btn-sm btn-primary" data-cs="open-review">${t("chg.review_btn")}</button>
            `}
          </div>
        </div>
        <div class="tcc-body">
          <button class="tcc-file" data-cs="open-review-file" data-cs-file="0">
            <span class="tcc-path">crates/spanbay-core/src/llm/router.rs</span>
            ${tag(t("chg.kind_edit"))}
            ${stTag(fileStates[0])}
            <span class="diff-stat mono"><span class="add">+18</span> <span class="del">−4</span></span>
          </button>
          <button class="tcc-file" data-cs="open-review-file" data-cs-file="1">
            <span class="tcc-path">crates/spanbay-core/src/llm/tests.rs</span>
            ${tag(t("chg.kind_edit"))}
            ${stTag(fileStates[1])}
            <span class="diff-stat mono"><span class="add">+6</span> <span class="del">−2</span></span>
          </button>
        </div>
        <div class="tcc-foot-warn">${dot("warn")}<span>${t("chg.untracked_warn")}</span></div>
      </div>
      ${showUndoCheck ? `
        <div class="cs-undo-modal" id="cs-undo-check">
          <div class="row-between"><b>${t("undo.modal_title")}</b><button class="btn-icon" data-cs="close-undo-prompt">×</button></div>
          <div class="cs-undo-group">
            <span class="text-ok fw-600">${t("undo.safe")}</span>
            <span class="mono fs-2xs text-ink2">crates/spanbay-core/src/llm/tests.rs（1 次修改 · SHA-256 匹配）</span>
          </div>
          <div class="cs-undo-group" style="border-color:var(--bad-border);background:var(--bad-soft)">
            <span class="text-bad fw-600">${t("undo.conflict")}</span>
            <span class="mono fs-2xs text-ink1">crates/spanbay-core/src/llm/router.rs（磁盘哈希与 after_sha256 不一致，外部改过第 220 行）</span>
          </div>
          <div class="cs-undo-group">
            <span class="text-muted fw-600">${t("undo.ignored")}</span>
            <span class="mono fs-2xs text-muted">Cargo.lock（由 cargo test 命令修改，不在文件恢复记录中）</span>
          </div>
          <div class="row-between">
            <span class="fs-2xs text-muted">默认跳过外部已修改文件，防止冲掉手写代码</span>
            <div class="row-center">
              <button class="btn btn-sm" data-cs="close-undo-prompt">${t("common.cancel")}</button>
              <button class="btn btn-sm btn-danger" data-cs="confirm-undo-turn" data-cs-force="1">${t("undo.force_all")}</button>
              <button class="btn btn-sm btn-primary" data-cs="confirm-undo-turn">${t("undo.safe_only")}</button>
            </div>
          </div>
        </div>` : ""}`;
  };

  /** Complete Merged Assistant Turn Block (1 model header, spine, answer, changes card, hover footer). */
  const metricsPopover = () => `
    <div class="turn-metrics-pop" id="cs-turn-metrics-pop">
      <div class="row-between"><b class="text-ink1">${t("met.title")}</b><span class="tag tag-ok">200 OK</span></div>
      <div class="tm-row"><span class="k">${t("met.route")}</span><span class="v">DeepSeek / deepseek-v4-pro:low</span></div>
      <div class="tm-row"><span class="k">${t("met.steps")}</span><span class="v">${t("met.steps_val")}</span></div>
      <div class="tm-row"><span class="k">${t("met.in_out")}</span><span class="v">18,400 in · 1,100 out token</span></div>
      <div class="tm-row"><span class="k">${t("met.reason_cache")}</span><span class="v">420 reasoning · 12,000 cache token</span></div>
      <div class="tm-row"><span class="k">${t("met.ctx")}</span><span class="v">18,400 / 128,000 token (14%)</span></div>
      <div class="tm-row"><span class="k">${t("met.speed")}</span><span class="v">TTFT 1.20s · 52.4 token/s</span></div>
      <div class="tm-row"><span class="k">${t("met.cost")}</span><span class="v">US$0.018</span></div>
    </div>`;

  const asstTurn = ({
    id = "t1",
    model = "deepseek-v4-pro",
    effort = "低推理",
    dur = "11.4s",
    collapsed = false,
    foldSummary = "",
    spineHtml = "",
    mdHtml = "",
    sourcesHtml = "",
    changesHtml = "",
    extraHtml = "",
    forceHover = false,
    metricsOpen = false,
    stopChip = "",
    costTxt = "11.4s · 19.5k token · $0.018",
  }) => {
    const foldBar = (foldSummary && collapsed) ? `
      <button type="button" class="turn-fold-bar" data-cs="toggle" data-cs-target="spine-${id}" aria-expanded="false">
        ${dot("ok")}
        <span>${foldSummary}</span>
        ${changesHtml ? `<span class="diff-stat mono ml-auto"><span class="add">+24</span> <span class="del">−6</span></span>` : ""}
        <span class="sp-chev ${changesHtml ? "" : "ml-auto"}">›</span>
      </button>` : "";
    return `
      <div class="asst-turn ${forceHover ? "force-hover" : ""} ${collapsed && !forceHover ? "is-collapsed" : ""}" data-turn-asst="${id}">
        <div class="asst-turn-head">
          <span class="asst-model">${model}</span>
          ${effort ? `<span class="asst-effort">${effort}</span>` : ""}
          <span class="mono text-faint">· ${dur}</span>
        </div>
        ${foldBar}
        ${spineHtml ? `<div id="spine-${id}" class="asst-spine" ${collapsed ? "hidden" : ""}>${spineHtml}</div>` : ""}
        ${mdHtml}
        ${sourcesHtml}
        ${changesHtml}
        ${extraHtml}
        <div class="asst-turn-foot">
          <div class="turn-hover-actions" role="toolbar" aria-label="助手回合操作" style="width:100%">
            <button class="turn-act-btn" data-cs="copy-turn">${t("act.copy")}</button>
            <button class="turn-act-btn" data-cs="retry">${t("act.retry")}</button>
            <button class="turn-act-btn" data-toast="已从本轮创建分支会话">${t("act.fork")}</button>
            <button class="turn-act-btn" data-modal="run-monitor" data-rm-run-idx="0">${t("act.workflow")}</button>
            <button class="turn-act-btn" data-toast="已删除此轮回复">${t("act.delete")}</button>
            <span class="row-center ml-auto" style="gap:6px;position:relative">
              <button class="turn-metrics-pill" data-cs="toggle-turn-metrics" title="点击查看本轮 Token、速度与费用明细">${costTxt} ⓘ</button>
              ${metricsOpen ? metricsPopover() : ""}
            </span>
          </div>
          ${stopChip ? `<span class="ml-auto">${stopChip}</span>` : ""}
        </div>
      </div>`;
  };
  CS.asstTurn = asstTurn;

  const sourcesRow = () => `
    <div class="cs-sources"><span>${t("sources")}</span>
      ${linkChip("Retry-After · rfc-editor.org", "https://www.rfc-editor.org/rfc/rfc9110#field.retry-after")}
      ${linkChip("Rate limits · docs.acme.dev", "https://docs.acme.dev/gateway/rate-limits")}
    </div>`;

  const markdownAnswer = () => `
    <div class="cs-md">
      <h4>结论：主要是 <code>router.rs</code> 熔断分支过于激进，上游限流均携带了 <code>retry-after</code></h4>
      <p>日志里 412 次调用有 37 次失败，其中 31 次是 429。上游每次都返回了 <code>retry-after: 2</code>，但 <code>router.rs</code> 把 429 和 5xx 放进同一个分支，单次 429 即直接熔断 60 秒。</p>
      <table><thead><tr><th>状态</th><th>次数</th><th>来源</th><th>调整后行为</th></tr></thead>
        <tbody>
          <tr><td>429 Too Many Requests</td><td class="num">31</td><td>deepseek-v4-pro（上游）</td><td>先按 <code>Retry-After</code> 退避一次（上限 15s），仍失败则切备用链</td></tr>
          <tr><td>503（本地熔断拦截）</td><td class="num">4</td><td>本地网关</td><td>不再因单次 429 误触发熔断</td></tr>
          <tr><td>5xx 服务端错误</td><td class="num">2</td><td>kimi-k2.6</td><td>连续 5 次 5xx 才触发 30s 熔断</td></tr>
        </tbody></table>
      <pre class="mono">429 =&gt; honor_retry_after(&amp;headers, MAX_RETRY_WAIT),
500..=599 =&gt; breaker.record_failure(target, FAILURES_BEFORE_TRIP),</pre>
    </div>`;

  /** Standard multi-step spine for the finished turn (`turn` and `review` states). */
  const fullSpine = (verbose = false) => [
    thinkRow("th-main", "1.4", "先统计附件 gateway-log-1106.json 里 429 的时间分布与响应头，再检查 crates/spanbay-core/src/llm/router.rs 的状态码分支。日志里所有 429 都带了 retry-after: 2，说明是本地把 429 当成 5xx 直接 trip 了熔断器。", verbose),
    exploreGroup({ id: "exp-main", running: false, files: 3, searches: 2, open: verbose }),
    editRow({
      path: "crates/spanbay-core/src/llm/router.rs",
      added: 18,
      removed: 4,
      fileIdx: 0,
      verboseDiff: verbose ? `<div class="diff-box"><div class="diff-head"><span>@@ -212,7 +212,8 @@ impl Router</span></div><div class="diff-row del">- 215   429 | 500..=599 =&gt; breaker.trip(target, Duration::from_secs(60)),</div><div class="diff-row add">+ 215   429 =&gt; honor_retry_after(&amp;headers, MAX_RETRY_WAIT),</div><div class="diff-row add">+ 216   500..=599 =&gt; breaker.record_failure(target, FAILURES_BEFORE_TRIP),</div></div>` : "",
    }),
    editRow({
      path: "crates/spanbay-core/src/llm/tests.rs",
      added: 6,
      removed: 2,
      fileIdx: 1,
    }),
    shellRow({
      id: "sh-main",
      st: "completed",
      cmd: "cargo test -p spanbay-core router",
      dur: "9.2s",
      exitCode: 0,
      lines: [
        ["out", "running 14 tests"],
        ["out", "test llm::router::tests::honors_retry_after_seconds ... ok"],
        ["out", "test llm::router::tests::honors_retry_after_http_date ... ok"],
        ["out", "test llm::router::tests::trips_breaker_only_on_5xx ... ok"],
        ["out", "test result: ok. 14 passed; 0 failed; 0 ignored"],
      ],
      omitted: 12,
      open: verbose,
    }),
  ].join("");

  // ── Right Review Panel HTML (Diff per file, Per-Hunk Revert, Line Comments) ─
  const DIFF_FILES = [
    {
      path: "crates/spanbay-core/src/llm/router.rs",
      short: "router.rs",
      kind: "edit",
      added: 18,
      removed: 4,
      hunks: [
        {
          id: "h0",
          header: "@@ -212,7 +212,8 @@ impl Router",
          lines: [
            ["ctx", 212, "  pub async fn handle_status(&self, target: TargetId, status: u16, headers: &HeaderMap) {"],
            ["ctx", 213, "      match status {"],
            ["del", 214, "-         429 | 500..=599 => self.breaker.trip(target, Duration::from_secs(60)),"],
            ["add", 215, "+         429 => self.honor_retry_after(target, headers, self.max_retry_wait).await,"],
            ["add", 216, "+         500..=599 => self.breaker.record_failure(target, FAILURES_BEFORE_TRIP),"],
            ["ctx", 217, "          _ => self.breaker.record_success(target),"],
            ["ctx", 218, "      }"],
          ],
        },
        {
          id: "h1",
          header: "@@ -248,4 +249,18 @@ impl Router",
          lines: [
            ["ctx", 249, "  fn parse_retry_after(headers: &HeaderMap, cap: Duration) -> Duration {"],
            ["add", 250, "+     headers.get( http::header::RETRY_AFTER )"],
            ["add", 251, "+         .and_then(|v| v.to_str().ok())"],
            ["add", 252, "+         .and_then(parse_seconds_or_http_date)"],
            ["add", 253, "+         .map(|d| d.min(cap))"],
            ["add", 254, "+         .unwrap_or(Duration::from_secs(2))"],
            ["ctx", 255, "  }"],
          ],
        },
      ],
    },
    {
      path: "crates/spanbay-core/src/llm/tests.rs",
      short: "tests.rs",
      kind: "edit",
      added: 6,
      removed: 2,
      hunks: [
        {
          id: "h2",
          header: "@@ -88,5 +88,9 @@ mod tests",
          lines: [
            ["ctx", 88, "  #[tokio::test]"],
            ["del", 89, "- async fn rate_limit_trips_breaker_immediately() {"],
            ["add", 89, "+ async fn rate_limit_waits_for_retry_after_without_tripping_breaker() {"],
            ["add", 90, "+     let router = Router::for_test(Duration::from_secs(15));"],
            ["add", 91, "+     assert!(!router.breaker_is_open(TargetId::Primary));"],
            ["ctx", 92, "  }"],
          ],
        },
      ],
    },
  ];
  CS.DIFF_FILES = DIFF_FILES;

  CS.reviewPanelHtml = ({
    selectedFile = 0,
    scope = "turn",
    diffMode = "net",
    revertedHunks = {},
    fileStates = ["pending", "pending"],
    commentingLine = null,
  } = {}) => {
    const f = DIFF_FILES[selectedFile] || DIFF_FILES[0];
    const fState = fileStates[selectedFile] || "pending";
    const stTag = (st) =>
      st === "kept" ? tag(t("chg.st_kept"), "ok") : st === "undone" ? tag(t("chg.st_undone")) : tag(t("chg.st_pending"), "warn");

    return `
      <div class="rv-head">
        <span>${t("rv.title")}</span>
        <span class="tab-cnt">2</span>
        <div class="seg cs-view-seg ml-auto">
          <button type="button" class="${scope === "turn" ? "active" : ""}" data-cs="rv-scope" data-cs-val="turn">${t("rv.scope_turn")}</button>
          <button type="button" class="${scope === "all" ? "active" : ""}" data-cs="rv-scope" data-cs-val="all">${t("rv.scope_all")}</button>
        </div>
        <button class="btn btn-sm btn-ghost" data-cs="rv-keep-all">${t("rv.keep_all")}</button>
        <button class="btn-icon" data-cs="close-review" title="${t("common.close")} (Esc)">×</button>
      </div>
      <div class="rv-files">
        ${DIFF_FILES.map((item, idx) => `
          <button class="rv-file-row ${idx === selectedFile ? "selected" : ""}" data-cs="rv-select-file" data-cs-file="${idx}">
            ${dot(fileStates[idx] === "kept" ? "ok" : fileStates[idx] === "undone" ? "stopped" : "warn")}
            <span class="mono truncate flex-1">${item.path}</span>
            ${stTag(fileStates[idx])}
            <span class="diff-stat mono fs-2xs"><span class="add">+${item.added}</span> <span class="del">−${item.removed}</span></span>
          </button>`).join("")}
      </div>
      <div class="rv-toolbar">
        <div class="seg cs-view-seg">
          <button type="button" class="${diffMode === "net" ? "active" : ""}" data-cs="rv-diff-mode" data-cs-val="net">${t("rv.net_diff")}</button>
          <button type="button" class="${diffMode === "edit" ? "active" : ""}" data-cs="rv-diff-mode" data-cs-val="edit">${t("rv.per_edit")}</button>
        </div>
        <div class="row-center" style="gap:4px">
          <button class="btn btn-sm" data-cs="rv-keep-file" data-cs-file="${selectedFile}" ${fState !== "pending" ? "disabled" : ""}>${t("rv.keep")}</button>
          <button class="btn btn-sm btn-danger" data-cs="rv-undo-file" data-cs-file="${selectedFile}" ${fState === "undone" ? "disabled" : ""}>${t("rv.undo")}</button>
        </div>
      </div>
      <div class="rv-diff-scroll">
        <div class="fs-2xs text-muted row-between">
          <span>悬停行号点 <b class="text-accent">+</b> 添加批注芯片 · 每个代码块可独立回滚</span>
          <span class="mono">${f.short}</span>
        </div>
        ${f.hunks.map((h) => {
          if (revertedHunks[h.id]) {
            return `<div class="rv-hunk"><div class="rv-hunk-head"><span>${h.header}</span><span class="text-muted">✓ ${t("rv.hunk_reverted")}</span></div></div>`;
          }
          return `
            <div class="rv-hunk" data-hunk="${h.id}">
              <div class="rv-hunk-head">
                <span>${h.header}</span>
                <button class="text-accent fw-600" data-cs="rv-revert-hunk" data-cs-hunk="${h.id}">${t("rv.revert_hunk")} ↺</button>
              </div>
              ${h.lines.map(([kind, ln, code]) => {
                const key = `${f.short}:${ln}`;
                const isCommenting = commentingLine === key;
                return `
                  <div class="rv-line ${kind === "add" ? "add" : kind === "del" ? "del" : ""} ${isCommenting ? "commenting" : ""}" data-cs="rv-line-comment" data-cs-line="${key}">
                    <span class="rv-ln"><span class="rv-plus" title="${t("rv.comment_add")}">+</span>${ln}</span>
                    <span class="rv-code">${code.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")}</span>
                  </div>
                  ${isCommenting ? `
                    <div class="rv-comment-form">
                      <div class="row-between fs-2xs"><b class="mono text-accent">@${key}</b><span class="text-muted">批注将作为上下文芯片挂入底部输入框</span></div>
                      <div class="row-center" style="gap:6px">
                        <div class="input-box sm flex-1"><input id="rv-comment-input" placeholder="${t("rv.comment_ph")}" value="这里 429 退避上限请直接读配置项 max_retry_wait_s" /></div>
                        <button class="btn btn-sm" data-cs="rv-cancel-comment">${t("common.cancel")}</button>
                        <button class="btn btn-sm btn-primary" data-cs="rv-submit-comment" data-cs-line="${key}">${t("rv.comment_add")}</button>
                      </div>
                    </div>` : ""}`;
              }).join("")}
            </div>`;
        }).join("")}
      </div>`;
  };

  // ── Dock Components: Editable Queue Bar, Unified Working+Todo Strip ────────
  CS.queueBarHtml = (items = [], paused = false) => {
    if (paused) {
      return banner("warn", t("queue.paused", { n: items.length || 1 }), `
        <button class="btn btn-sm" data-cs="queue-clear">清空</button>
        <button class="btn btn-sm btn-primary" data-cs="queue-resume">${t("queue.resume")}</button>`);
    }
    if (!items.length) return "";
    return `
      <div class="cs-queue-card">
        ${items.map((item, idx) => `
          <div class="cs-queue-row">
            <span class="tag tag-accent">${t("queue.title", { n: items.length })}</span>
            <span class="mono text-faint">#${idx + 1}</span>
            <span class="flex-1 truncate text-ink1">${item.text}</span>
            <button class="btn btn-sm btn-ghost text-accent" data-cs="queue-steer" data-cs-idx="${idx}">${t("queue.steer_now")}</button>
            <button class="btn btn-sm btn-ghost" data-cs="queue-edit" data-cs-idx="${idx}">${t("queue.edit")}</button>
            <button class="btn-icon" data-cs="queue-remove" data-cs-idx="${idx}" title="${t("common.close")}">×</button>
          </div>`).join("")}
      </div>`;
  };

  const TODOS = [
    ["completed", "读取 router.rs 的 429 分支"],
    ["completed", "统计日志中 429 的来源与 retry-after"],
    ["completed", "实现 honor_retry_after 退避逻辑"],
    ["running", "运行 router 模块单元测试"],
    ["failed", "运行 cargo clippy（用户拒绝并要求先跑单测）", "反馈原因：先只跑 router 单测，确认通过后再跑 clippy"],
    ["blocked", "更新网关故障转移文档（等待单测通过）"],
    ["pending", "输出根因与改动总结"],
  ];

  CS.workStripHtml = ({ steps = 4, secs = 12, showTodos = false, todosOpen = false } = {}) => {
    if (!showTodos) return "";
    return `
      <div class="cs-work-strip">
        <button type="button" class="cs-todo-head" data-cs="toggle" data-cs-target="cs-todo-body" aria-expanded="${todosOpen}">
          ${dot("run")}
          <b>${t("todos.summary", { done: 3, total: 7 })}</b>
          <span class="text-muted truncate">· ${t("todos.current", { c: "运行 router 模块单元测试" })}</span>
          ${tag(t("trace.failed", { n: 1 }), "bad")}
          <span class="ml-auto mono text-faint">${t("run.step", { steps, s: secs })}</span>
          <span class="sp-chev">${todosOpen ? "⌄" : "›"}</span>
        </button>
        <div class="cs-todo-body" id="cs-todo-body" ${todosOpen ? "" : "hidden"}>
          ${TODOS.map(([st, title, err], i) => spRow({
            id: `td-item-${i}`,
            st,
            title,
            detail: err ? `<span class="text-bad">${err}</span>` : "",
            open: Boolean(err),
            badDetail: Boolean(err),
          })).join("")}
        </div>
      </div>`;
  };

  // ── Inline Approval Card (Allow Once / Scope Menu / Deny with Feedback) ────
  const approval = (a) => {
    const tone = a.risk === "high" ? "bad" : "warn";
    const tags = (a.tags || []).map((k) => tag(t(`tag.${k}`))).join("");
    const metaParts = [
      a.sandbox ? t(`sb.${a.sandbox}`) : "",
      a.layer ? t(`layer.${a.layer}`) : "",
      a.why ? a.why : "",
    ].filter(Boolean).join(" · ");

    return `
      <div class="gate-card cs-appr ${tone === "bad" ? "tone-bad" : ""}" tabindex="0" data-cs-appr="${a.id}" aria-label="${a.summary}">
        <div class="gate-head">
          ${dot(tone)}
          <span class="cs-word ${tone}">${t("st.wait.approval")}</span>
          <b class="text-ink1">· ${a.summary}</b>
          <span class="ml-auto row-center" style="gap:4px">
            ${tag(a.tool)}
            ${tag(t(`risk.${a.risk}`) + (a.riskReason ? ` · ${a.riskReason}` : ""), tone)}
            ${tags}
          </span>
          <span class="text-muted fs-2xs">${t("appr.waited", { t: a.waited || "1m" })}</span>
        </div>
        <div class="gate-body">
          ${a.preview}
          <div class="cs-reason">
            <span>${metaParts}</span>
            ${a.ruleNotUsed ? `<span class="warn">· 未使用已存规则：${a.ruleNotUsed}</span>` : ""}
          </div>
          <div class="cs-deny-row" data-cs-deny-row="${a.id}" ${a.openDeny ? "" : "hidden"}>
            <div class="input-box sm">
              <input data-cs-deny-input="${a.id}" placeholder="${t("appr.deny_ph")}" value="${a.defaultDenyText || ""}" />
            </div>
            <button class="btn btn-sm" data-cs="deny-cancel" data-cs-id="${a.id}">${t("common.cancel")}</button>
            <button class="btn btn-sm btn-danger" data-cs="deny-confirm" data-cs-id="${a.id}">${t("appr.confirm_deny")} (↵)</button>
          </div>
          <div class="gate-actions">
            <span class="fs-2xs text-muted" data-cs-esc-hint="${a.id}">${t("appr.esc")}</span>
            <div class="row-center" data-cs-actions="${a.id}" style="position:relative">
              <button class="btn btn-sm" data-cs="deny" data-cs-id="${a.id}">${t("appr.deny")}</button>
              ${a.moreScopes !== false ? `<button class="btn btn-sm" data-modal="cs-scope">${t("appr.scope")}</button>` : ""}
              ${a.network ? `<button class="btn btn-sm" data-cs="allow" data-cs-id="${a.id}" data-cs-scope="once">${t("appr.allow_net")}</button>` : ""}
              <button class="btn btn-sm btn-primary" data-cs="allow" data-cs-id="${a.id}" data-cs-scope="once">${t("appr.allow_once")}</button>
            </div>
          </div>
        </div>
      </div>`;
  };
  CS.approval = approval;

  const cmdPreview = () => `
    <div class="gate-cmd" style="flex-direction:column;align-items:stretch;gap:4px">
      <span><span class="text-faint">$ </span>cargo test -p spanbay-core router <span class="cs-op">&amp;&amp;</span> git diff --stat <span class="cs-op">&gt;</span> /tmp/router.diff</span>
      <span class="fs-2xs text-muted" style="font-family:var(--font-ui)">${t("appr.cwd", { cwd: "~/me/SpanBay" })}</span>
    </div>`;
  const diffLines = () => {
    const out = [];
    for (let i = 0; i < 48; i += 1) {
      const n = 210 + i;
      if (i === 4) out.push(`<div class="diff-row del">- ${n}     429 | 500..=599 =&gt; breaker.trip(target, Duration::from_secs(60)),</div>`);
      else if (i === 5) out.push(`<div class="diff-row add">+ ${n}     429 =&gt; honor_retry_after(&amp;headers, MAX_RETRY_WAIT),</div>`, `<div class="diff-row add">+ ${n + 1}     500..=599 =&gt; breaker.record_failure(target, FAILURES_BEFORE_TRIP),</div>`);
      else if (i > 20 && i < 32) out.push(`<div class="diff-row add">+ ${n}     // honor_retry_after: parse seconds or HTTP-date, clamp to MAX_RETRY_WAIT</div>`);
      else out.push(`<div class="diff-row">  ${n}     ${i % 7 === 0 ? "}" : "    let attempt = attempts.next(target);"}</div>`);
    }
    return out.join("");
  };
  const diffPreview = () => `
    <div class="col-stack-sm">
      <div class="row-between">
        <span class="mono fs-xs text-ink2">crates/spanbay-core/src/llm/router.rs <span class="text-faint">· +18 −4 · 完整差异默认展开（卡内可滚动，拒绝盲批）</span></span>
        <button class="text-accent fs-2xs" data-cs="open-review">${t("chg.review_btn")}</button>
      </div>
      <div class="cs-preview-scroll"><div class="diff-box"><div class="diff-head"><span>@@ -210,48 +210,60 @@ impl Router</span></div>${diffLines()}</div></div>
    </div>`;

  CS.approvalSpecs = () => ({
    command: { id: "a-cmd", tool: "shell", risk: "medium", tags: ["chained"], summary: "需要在 ~/me/SpanBay 运行命令", preview: cmdPreview(), layer: "mode_default", sandbox: "seatbelt", ruleNotUsed: "cargo test *（复合命令含 && 与 >，需重新确认）", network: true, moreScopes: true },
    diff: { id: "a-diff", tool: "edit_file", risk: "medium", summary: "需要编辑 crates/spanbay-core/src/llm/router.rs (+18 −4)", preview: diffPreview(), layer: "mode_default", sandbox: "seatbelt", moreScopes: true },
    deny: { id: "a-cmd", tool: "shell", risk: "medium", tags: ["chained"], summary: "需要在 ~/me/SpanBay 运行命令", preview: cmdPreview(), layer: "mode_default", sandbox: "seatbelt", openDeny: true, defaultDenyText: "先别跑全量命令，只跑 router::tests 单元测试，不要写 /tmp 文件" },
    high_risk: { id: "a-remote", tool: "shell", risk: "high", riskReason: "远端推送", tags: ["sandbox_off"], summary: "需要向远端仓库推送分支", preview: `<div class="gate-cmd"><span class="text-faint">$</span><span>git push origin main --force-with-lease</span></div>`, why: t("why.shell_remote"), layer: "forced_ask", sandbox: "none", moreScopes: false },
  });

  // ── Plan, Question & Cost Gates ───────────────────────────────────────────
  const planSteps = () => [
    ["completed", "探索 · 统计日志中 429 的来源与时间分布"],
    ["pending", "实现 · router.rs 拆分 429 与 5xx 熔断分支"],
    ["pending", "实现 · honor_retry_after 解析秒数与 HTTP-date"],
    ["pending", "验证 · 运行 cargo test -p spanbay-core router"],
  ].map(([st, x], i) => spRow({ id: `plan-s${i}`, st, title: x, meta: i === 0 ? "快速" : "" })).join("");

  CS.planGate = () => `
    <div class="gate-card tone-accent" data-cs-plan-gate tabindex="0">
      <div class="gate-head">
        ${dot("warn")}<span class="cs-word warn">${t("st.wait.plan")}</span>
        <b class="text-ink1">· 智能体提交了计划：按 Retry-After 退避替换 429 熔断</b>
        <span class="ml-auto seg cs-view-seg"><button class="active">阶段 (4)</button><button data-toast="切换为计划 Markdown 原文">Markdown</button></span>
      </div>
      <div class="gate-body">
        <div class="cs-plan-steps">${planSteps()}</div>
        <div class="fs-2xs text-accent" data-cs-plan-wait hidden>${t("plan.revise_waiting")}</div>
        <div class="gate-actions">
          <span class="fs-2xs text-muted">${t("plan.hint")}</span>
          <div class="row-center">
            <button class="btn btn-sm btn-ghost" data-cs="keep-planning">${t("plan.keep")}</button>
            <button class="btn btn-sm" data-cs="approve-plan" data-cs-mode="ask">${t("plan.approve_ask")}</button>
            <button class="btn btn-sm btn-primary" data-cs="approve-plan" data-cs-mode="read">${t("plan.approve_go")}</button>
          </div>
        </div>
      </div>
    </div>`;

  CS.questionGate = () => `
    <div class="gate-card tone-accent" data-cs-q-gate tabindex="0">
      <div class="gate-head">${dot("warn")}<span class="cs-word warn">${t("st.wait.question")}</span><b class="text-ink1">· 429 退避等待后重试仍失败时，是否自动切到备用模型链？</b></div>
      <div class="gate-body">
        <div class="col-stack-sm" data-cs-options>
          <button class="cs-opt picked" data-cs="pick"><span class="n">1</span><span><b>切到备用模型链（推荐）</b><br/><span class="text-muted">deepseek-v4-pro → kimi-k2.6，仅在首个内容事件发出前生效</span></span></button>
          <button class="cs-opt" data-cs="pick"><span class="n">2</span><span><b>只退避重试，不切换模型</b><br/><span class="text-muted">保持原模型，重试耗尽后直接返回 429 错误</span></span></button>
          <button class="cs-opt" data-cs="pick"><span class="n">3</span><span><b>遵循网关客户端规则</b><br/><span class="text-muted">读取当前客户端的 fallback_enabled 配置</span></span></button>
          <div class="input-box sm"><input placeholder="${t("q.other")}" /></div>
        </div>
        <div class="gate-actions"><span class="fs-2xs text-muted">数字键 1–3 选择 · ↵ 回答 · Esc 跳过</span>
          <div class="row-center"><button class="btn btn-sm" data-cs="resolve-gate" data-cs-msg="已跳过问题，智能体按默认推荐继续">${t("q.skip")}</button><button class="btn btn-sm btn-primary" data-cs="resolve-gate" data-cs-msg="已回答：切到备用模型链">${t("q.answer")}</button></div></div>
      </div>
    </div>`;

  const costRow = (lbl, v, pct) => `<div class="cs-ctx-row"><span class="lbl">${lbl}</span><span class="bar"><i style="width:${pct}%"></i></span><span class="val">${v}</span></div>`;
  CS.costGate = () => `
    <div class="gate-card">
      <div class="gate-head">${dot("warn")}<span class="cs-word warn">${t("st.wait.cost")}</span><b class="text-ink1">· ${t("cost.title")}</b><span class="ml-auto mono text-ink1">US$0.52 / US$0.50</span></div>
      <div class="gate-body">
        <div class="col-stack-sm">${costRow(t("cost.main"), "US$0.41", 79)}${costRow(t("cost.subagent"), "US$0.09", 17)}${costRow(t("cost.compaction"), "US$0.02", 4)}</div>
        <span class="fs-xs text-ink2">${t("cost.next", { amount: "US$1.00" })}</span>
        <span class="fs-xs text-warn">${t("cost.unpriced", { n: 2, models: "relay/glm-5、local/qwen3-coder:30b" })}</span>
        <div class="gate-actions"><span class="fs-2xs text-muted">费用为本机按目录价估算；前台无人值守 10 分钟后自动停止并暂停队列</span>
          <div class="row-center"><button class="btn btn-sm" data-cs="resolve-gate" data-cs-msg="已停止运行（达费用上限）">${t("cost.stop")}</button><button class="btn btn-sm btn-primary" data-cs="resolve-gate" data-cs-msg="已继续运行，下一次在 US$1.00 暂停">${t("cost.go")}</button></div></div>
      </div>
    </div>`;

  // ── Shell & All 10 Tool Statuses ──────────────────────────────────────────
  CS.shellPairSpine = () => [
    thinkRow("th-sh", "0.9", "先运行 router 单元测试验证解析函数，再尝试离线构建检查依赖。"),
    shellRow({
      id: "sh-ok-1",
      st: "completed",
      cmd: "cargo test -p spanbay-core router::parse",
      dur: "0.8s",
      exitCode: 0,
      lines: [
        ["out", "running 4 tests"],
        ["out", "test llm::router::parse::seconds ... ok"],
        ["out", "test llm::router::parse::http_date ... ok"],
        ["out", "test result: ok. 4 passed; 0 failed"],
      ],
      omitted: 6,
      open: false,
    }),
    shellRow({
      id: "sh-fail-1",
      st: "failed",
      cmd: "cargo build -p spanbay-core --offline",
      dur: "3.4s",
      exitCode: 101,
      lines: [
        ["err", "error: no matching package named `httpdate-fast` found"],
        ["err", "location searched: registry `crates-io` (offline mode)"],
        ["err", "help: pass `--locked` with existing Cargo.lock crates instead"],
      ],
      open: true,
    }),
  ].join("");

  CS.allStatusRows = () => [
    shellRow({ id: "st-run", st: "running", cmd: "cargo test -p spanbay-core router", dur: "12s", lines: [["out", "   Compiling spanbay-core v0.9.0"], ["out", "    Running unittests src/lib.rs"]] }),
    spRow({ id: "st-ok", st: "completed", title: "读取 <code>crates/spanbay-core/src/llm/router.rs</code>", meta: "第 180–260 行 · 0.1s" }),
    shellRow({ id: "st-fail", st: "failed", cmd: "cargo build --offline", dur: "3.4s", exitCode: 101, lines: [["err", "error: no matching package named `retry-after` found"]] }),
    spRow({ id: "st-int", st: "interrupted", title: "运行 <code>$ cargo bench router</code>", meta: "—", detail: `<span class="text-muted">应用在命令结束前退出；重启后标为已中断，无退出码。</span>` }),
    spRow({ id: "st-blk", st: "blocked", title: "删除 <code>~/.ssh/known_hosts</code>", detail: `<span class="text-muted">硬阻止：路径在项目目录外且属敏感凭据目录，直接拒绝且不弹审批卡。</span>`, open: true, badDetail: true }),
    spRow({ id: "st-to", st: "timed_out", title: "抓取 <code>status.relay.acme.dev</code>", detail: `<span class="text-muted">后台无人值守 600 秒未回答审批，调用未执行。</span>` }),
    spRow({ id: "st-deny", st: "denied", title: "运行 <code>$ git push origin main</code>", detail: `<span class="text-ink2">拒绝原因（已交给模型）：先别推送，等我审完右侧 Diff。</span>` }),
    spRow({ id: "st-can", st: "cancelled", title: "运行 <code>$ cargo clippy -p spanbay-core</code>", meta: "4.1s", detail: `<span class="text-muted">你点击了停止运行；进程组已终止。</span>` }),
    spRow({ id: "st-exp", st: "expired", title: "编辑 <code>gateway/limits.rs</code>", detail: `<span class="text-muted">审批卡尚未回答时运行已结束，此卡已失效。</span>` }),
    spRow({ id: "st-sup", st: "superseded", title: "已搜索网页 \"DeepSeek 429 rate limit\"", detail: `<span class="text-muted">你在调用开始前发送了插队引导，该调用已被取代。</span>` }),
  ].join("");

  // ── Subagents Card (Explicit Sub-model & Role, Clean Steps) ───────────────
  CS.subagentsCard = () => `
    <div class="cs-sub">
      <div class="cs-sub-head">
        <span class="text-accent">⑂</span>
        <span>${t("sub.parallel", { n: 2 })}</span>
        <span class="fs-2xs text-muted fw-400">显式标注子智能体模型与角色 · 完整对话可在右侧或子会话查看</span>
      </div>
      <div style="padding:4px 8px">
        ${spRow({
          id: "sub-row-1",
          st: "running",
          open: true,
          title: `${t("tool.sub_inline", { model: "<code>deepseek-v4-flash</code>", role: "探索", task: "统计日志中 429 的来源与 retry-after 分布" })}`,
          meta: "当前：grep · 4.2s",
          trailing: `<span class="row-center" style="gap:4px"><span class="btn btn-sm" role="button" data-cs="open-child">${t("sub.open")} ↗</span><span class="btn btn-sm btn-danger" role="button" data-toast="已停止该子智能体">${t("sub.stop")}</span></span>`,
          detail: `
            <div class="sp-tree" style="margin-left:4px">
              <div class="sp-tree-row"><span class="sp-branch">└</span><span>读取 <code>gateway-log-1106.json</code> · 0.2s</span></div>
              <div class="sp-tree-row"><span class="sp-branch">└</span><span>搜索 <code>"status":429</code> · 31 处命中 · 0.4s</span></div>
              <div class="sp-tree-row">${dot("run")}<span>正在搜索 <code>retry-after</code> · 1.1s</span></div>
            </div>`,
        })}
        ${spRow({
          id: "sub-row-2",
          st: "completed",
          open: false,
          title: `${t("tool.sub_inline", { model: "<code>deepseek-v4-pro</code>", role: "通用", task: "核对 relay.acme.dev 与 kimi-k2.6 的备用路由配置" })}`,
          meta: "18.4s · $0.012",
          trailing: `<span class="btn btn-sm btn-ghost" role="button" data-cs="open-child">${t("sub.open")} ↗</span>`,
          detail: `
            <div class="sp-tree" style="margin-left:4px">
              <div class="sp-tree-row"><span class="sp-branch">└</span><span>读取 <code>router.rs</code>, <code>failover.rs</code> · 0.3s</span></div>
              <div class="sp-tree-row"><span class="sp-branch">└</span><span>运行探针 <code>relay.acme.dev</code> × 3 · 16.8s</span></div>
            </div>`,
        })}
      </div>
    </div>`;

  // ── Welcome Empty State ───────────────────────────────────────────────────
  CS.welcomeHtml = () => `
    <div class="cs-welcome">
      <div class="cs-welcome-head">
        <div class="row-center" style="gap:8px">
          <span class="ws-badge">S</span>
          <span class="cs-welcome-title">有什么可以帮你？</span>
        </div>
        <p class="cs-welcome-sub">当前已绑定工作区 <b class="text-ink1">Billing platform (Staging)</b> 与本地仓库 <code class="text-ink1">~/me/SpanBay (main)</code>。只读探索自动放行，修改文件与运行命令前会请你在底部确认。</p>
      </div>
      <div class="cs-starters">
        <button class="cs-starter" data-cs="fill-starter" data-cs-text="网关遇到上游 429 时直接熔断了 60 秒，帮我查下 router.rs 并改成优先遵循一次 Retry-After 头。">
          <b>排查网关 429 熔断</b>
          <span>分析网关调用日志与 router.rs 分支，遵循 Retry-After 退避并补齐单测</span>
        </button>
        <button class="cs-starter" data-cs="fill-starter" data-cs-text="对比 relay.acme.dev 与 DeepSeek 官方端点的首字延迟（TTFT），给出主备路由建议。">
          <b>审计服务商首字延迟</b>
          <span>并行派生探索子智能体，核对各上游模型探测证据与备用链</span>
        </button>
        <button class="cs-starter" data-cs="fill-starter" data-cs-text="检查 Billing platform 工作区里 POST /refunds 请求的断言失败原因，并生成修复补丁。">
          <b>联调失败的 API 请求</b>
          <span>读取 Staging 环境与响应报文，定位状态码或脚本断言错误</span>
        </button>
      </div>
    </div>`;

  // ── Context Meter Popover & Model/Effort/Web Unified Popover ──────────────
  const ctxRows = [["ctx.system", 2100], ["ctx.instructions", 1400], ["ctx.skills", 600], ["ctx.tools", 5200], ["ctx.messages", 4800], ["ctx.results", 3700], ["ctx.attachments", 600]];
  CS.contextPopover = (canCompact = true) => `
    <div class="pop-box cs-ctx-pop">
      <div class="pop-head"><span>${t("ctx.title")}</span><span class="mono text-muted">14%</span></div>
      <b class="fs-xs">${t("ctx.used", { used: "18,400", limit: "128,000" })}</b>
      <div class="prog-track" style="max-width:none"><div class="prog-fill" style="width:14%"></div></div>
      <div class="col-stack-sm">${ctxRows.map(([k, v]) => costRow(t(k), v.toLocaleString("en-US"), Math.round((v / 5200) * 100))).join("")}</div>
      <div class="row-between"><span class="fs-2xs text-muted">${canCompact ? "将总结第 1–4 轮，保留最近 2 轮" : t("ctx.cannot")}</span>
        <button class="btn btn-sm" ${canCompact ? `data-toast="已压缩早期历史上下文（释放 14,200 token）"` : "disabled"}>${t("ctx.compact")}</button></div>
      <div class="pop-sep"></div>
      <div class="pop-head"><span>${t("ctx.session")}</span></div>
      <div class="cs-ctx-row"><span class="lbl">输入 / 输出</span><span class="val" style="flex:1">212,840 / 9,310 token</span></div>
      <div class="cs-ctx-row"><span class="lbl">缓存命中</span><span class="val" style="flex:1">148,000 token</span></div>
      <div class="cs-ctx-row"><span class="lbl">累计估算费用</span><span class="val" style="flex:1">US$0.21 · ¥0.04</span></div>
    </div>`;

  CS.modelPopover = (effort = "low", webOn = true) => `
    <div class="pop-box cs-model-pop">
      <div class="pop-head"><span>模型与推理配置（合一菜单）</span><span class="tag">TTFT</span></div>
      <div class="input-box sm sunken"><input placeholder="筛选服务商或模型…" /></div>
      <button class="pop-opt active" data-cs="pick-model- unified" data-cs-model="deepseek-v4-pro"><div class="pop-opt-col"><span class="mono">DeepSeek / deepseek-v4-pro</span><span class="pop-opt-sub">128K · 推理 + 工具 · 主力模型</span></div><span class="mono text-muted">412ms ✓</span></button>
      <button class="pop-opt" data-cs="pick-model-unified" data-cs-model="claude-sonnet-5-5"><div class="pop-opt-col"><span class="mono">Anthropic / claude-sonnet-5-5</span><span class="pop-opt-sub">200K · Messages</span></div><span class="mono text-muted">690ms</span></button>
      <button class="pop-opt" data-cs="pick-model-unified" data-cs-model="deepseek-v4-flash"><div class="pop-opt-col"><span class="mono">DeepSeek / deepseek-v4-flash</span><span class="pop-opt-sub">128K · 快速探索</span></div><span class="mono text-muted">233ms</span></button>
      <div class="pop-sep"></div>
      <div class="row-between fs-xs"><span class="text-ink2">推理思考强度</span>
        <div class="seg cs-view-seg">
          ${[["auto", "自动"], ["low", "低"], ["med", "中"], ["high", "高"]].map(([k, lbl]) => `<button type="button" class="${k === effort ? "active" : ""}" data-cs="pick-effort" data-cs-val="${lbl}">${lbl}</button>`).join("")}
        </div>
      </div>
      <div class="row-between fs-xs">
        <span class="text-ink2">允许联网搜索与抓取 (Web)</span>
        <button class="tag ${webOn ? "tag-accent" : ""}" data-cs="toggle-web-in-pop">${webOn ? "已开启 ✓" : "已关闭"}</button>
      </div>
    </div>`;

  // ── Sidebar Search & Status Legend ────────────────────────────────────────
  CS.searchSidebar = (q, empty) => {
    if (empty) {
      return `<div class="col-stack-sm" style="padding:16px 10px;align-items:flex-start"><span class="fs-xs text-ink2">${t("search.empty", { q })}</span><span class="fs-2xs text-muted">已搜索全部对话（含已归档）的消息正文与工具输出</span><button class="btn btn-sm" data-cs="go" data-cs-go="turn">${t("search.clear")}</button></div>`;
    }
    const hit = (title, snip, tool) => `<button class="cs-hit" data-cs="go" data-cs-go="turn"><span class="row-center"><span class="list-row-title flex-1">${title}</span>${tool ? tag(t("search.tool")) : ""}</span><span class="cs-hit-snip">${snip}</span></button>`;
    return `
      <div class="list-group-head"><span>${t("search.matches")}</span><span>3</span></div>
      ${hit("修复网关 429 熔断与退避", "…上游每次都带了 <b>retry-after</b>: 2，但 router.rs 把 429 和 5xx…", false)}
      ${hit("网关备用路由切换规则", "…首个内容事件前遵循 <b>Retry-After</b>，之后锁定目标不再切换…", false)}
      ${hit("中转站首字延迟对比分析", "…HTTP/1.1 429 Too Many Requests\\n<b>retry-after</b>: 5…", true)}`;
  };

  CS.legend = () => {
    const rows = [
      ["pending", "待办或执行图节点尚未开始"], ["queued", "运行在等待并发槽位"],
      ["running", "正在执行；列表行只画开口弧，不写词"], ["waiting", "需要你处理：等待批准 / 等待确认 / 等待回答 / 费用暂停"],
      ["completed", "成功完成；列表行只画圆点，不写词"], ["failed", "唯一计入「N 项失败」的状态，默认自动展开错误详情"],
      ["interrupted", "应用退出或崩溃打断"], ["blocked", "硬阻止或规则 Block，直接拒绝不弹卡"],
      ["timed_out", "无人值守审批超时，未执行"], ["denied", "你拒绝了该次调用（可附反馈继续本轮）"],
      ["cancelled", "你点击了停止运行"], ["expired", "审批卡在运行结束时失效"],
      ["superseded", "插队新消息先到，调用未执行"], ["skipped", "执行图节点被跳过"],
    ];
    const toneName = { neutral: "次级正文色", warn: "警告色", bad: "出错色" };
    const shapeName = { idle: "空心圆", run: "开口弧", warn: "三角", ok: "实心圆", bad: "菱形", stopped: "空心方" };
    return `
      <div class="asst-turn">
        <div class="row-between"><b>1. 统一状态表（docs/design.md §2.2 &amp; §6）</b><span class="fs-2xs text-muted">形状 + 状态词 + 颜色三线索</span></div>
        <table class="cs-legend"><thead><tr><th>状态</th><th>呈现</th><th>形状</th><th>词色</th><th>zh-CN</th><th>en</th><th>规则说明</th></tr></thead><tbody>
        ${rows.map(([st, when]) => {
          const s = STATUS[st];
          const zh = st === "waiting" ? ["approval", "plan", "question", "cost"].map((w) => L.zh[`st.wait.${w}`]).join(" / ") : L.zh[`st.${st}`];
          const en = st === "waiting" ? ["approval", "plan", "question", "cost"].map((w) => L.en[`st.wait.${w}`]).join(" / ") : L.en[`st.${st}`];
          return `<tr><td><code>${st}</code></td><td>${badge(st)}</td><td>${shapeName[s.shape]}</td><td>${toneName[s.tone]}</td><td>${zh}</td><td>${en}</td><td class="text-muted">${when}</td></tr>`;
        }).join("")}
        </tbody></table>

        <div class="row-between" style="margin-top:12px"><b>2. 各工具单行精确文案规范（参照 Codex / Claude Code / ZCode）</b><span class="fs-2xs text-muted">详见 .overhaul/chat-redesign-spec.md §4.2</span></div>
        <table class="cs-legend"><thead><tr><th>类别</th><th>zh-CN 单行文案</th><th>en 单行文案</th><th>折叠与展开规则</th></tr></thead><tbody>
          <tr><td>整轮折叠头</td><td><code>▸ 已工作 11.4 秒 · 编辑 2 个文件，已探索 3 个文件，2 次搜索</code></td><td><code>▸ Worked for 11.4s · Edited 2 files, explored 3 files, 2 searches</code></td><td class="text-muted">历史已完成轮次（或紧凑模式）自动收拢</td></tr>
          <tr><td>只读探索组</td><td><code>▸ 已探索 3 个文件 · 2 次搜索</code></td><td><code>▸ Explored 3 files, 2 searches</code></td><td class="text-muted">连续 ≥2 次只读自动合并；展开用 <code>└</code> 树枝且按文件名去重；绝不默认内联代码预览</td></tr>
          <tr><td>文件编辑</td><td><code>● 编辑 crates/.../router.rs +18 −4 [查看差异]</code></td><td><code>● Edited crates/.../router.rs (+18 −4) [Diff]</code></td><td class="text-muted">始终单列露出路径与行数；点击在右侧审查面板打开 Diff</td></tr>
          <tr><td>终端命令</td><td><code>● 已运行 $ cargo test · 9.2s</code> / <code>◆ 未能运行命令 $ cargo build · 退出码 101</code></td><td><code>● Ran $ cargo test · 9.2s</code> / <code>◆ Exited (status 101) $ cargo build</code></td><td class="text-muted">退出码 0 折叠输出；非零退出码自动展开尾部错误</td></tr>
          <tr><td>待办更新</td><td>仅在输入框上方：<code>▸ 任务 3/7 · 当前：运行 router 单测</code></td><td>Composer strip only: <code>▸ Tasks 3/7 · Now: Run router unit tests</code></td><td class="text-muted">流内完全隐藏 <code>todo_write</code>，彻底消除「已工作 0ms · 1步」噪音</td></tr>
        </tbody></table>
      </div>`;
  };

  CS.frag = {
    userTurn, userWithAtts, asstTurn, spRow, thinkRow, exploreGroup, editRow, shellRow,
    changesSummaryCard, sourcesRow, markdownAnswer, fullSpine, chip, thumb, linkChip, tag, exitBadge,
  };
})();
