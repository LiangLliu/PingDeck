/* PingDeck Prototype — Settings Registry (5 Groups / 20 Real Pages) */
window.PD = window.PD || {};

(() => {
  const sw = (on) => `<button type="button" class="switch-ui${on ? " on" : ""}" data-action="toggle-switch" aria-pressed="${on}"></button>`;
  const seg = (opts, idx = 0) => `<div class="seg">${opts.map((o, i) => `<button type="button" class="${i === idx ? "active" : ""}" data-action="seg-pick">${o}</button>`).join("")}</div>`;
  const num = (v, unit = "") => `<div class="input-box" style="width:128px"><input value="${v}" class="text-right" />${unit ? `<span class="text-muted fs-2xs">${unit}</span>` : ""}</div>`;
  const row = (label, desc, ctl, meta = "") => `<div class="set-row"><div class="set-row-info"><span class="set-row-label">${label}${meta ? `<span class="set-meta-pill">${meta}</span>` : ""}</span>${desc ? `<span class="set-row-desc">${desc}</span>` : ""}</div><div>${ctl}</div></div>`;
  const sec = (title, body, reset = true, padded = false) => `<div class="set-section"><div class="set-section-head"><span class="set-section-title">${title}</span>${reset ? `<button class="btn btn-sm btn-ghost" data-modal="restore-page">恢复本区默认值</button>` : ""}</div><div class="set-card${padded ? " set-card-pad" : ""}">${body}</div></div>`;

  window.PD.ui = { sw, seg, num, row, sec };

  window.PD.SETTINGS_PAGES = {
    general: {
      group: "通用", title: "通用", changed: 0,
      html: () => [
        sec("语言", row("界面语言", "即时切换界面文案与排版规则。", seg(["简体中文", "English"], 0))),
        sec("启动", row("启动时恢复会话", "重新打开上次退出时的工作区与请求标签页。", sw(true))),
        sec("后台信号", [
          row("窗口不在前台时发送系统通知", "用于等待审批、提问、计划或耗时超过 30 秒的运行。", seg(["关闭", "仅需要你时", "含长运行完成"], 1)),
          row("Dock 图标弹跳提醒", "待审批数量始终显示在 Dock 角标上。", sw(true)),
        ].join("")),
      ].join(""),
    },
    appearance: { group: "通用", title: "外观", changed: 1 },
    shortcuts: {
      group: "通用", title: "快捷键", changed: 0,
      html: () => `
        <div class="row-center">
          <div class="input-box sunken flex-1"><input data-filter-rows="#shortcuts-rows .set-row" placeholder="查找命令或按键…" /></div>
          <span class="fs-xs text-muted">34 条命令，22 条有快捷键</span>
          <button class="btn btn-sm" data-modal="restore-page">全部恢复</button>
        </div>
        <div id="shortcuts-rows">${sec("全局", [
          row("打开命令面板", "搜索请求、服务商、设置与全部命令。", `<span class="kbd">⌘K</span>`),
          row("折叠或展开侧栏", "切换 220 px 完整侧栏与 54 px 图标窄轨。", `<span class="kbd">⌘B</span>`),
          row("打开设置", "快速进入设置或返回原模块。", `<span class="kbd">⌘,</span>`),
          row("切换底部控制台", "展开或收起请求工作台底部的执行日志控制台。", `<span class="kbd">⌘J</span>`),
          row("发送当前请求", "在请求工作台立即发出当前标签页。", `<span class="kbd">⌘↵</span>`),
          row("停止当前运行", "停止当前活跃的请求、流程或对话运行。", `<span class="kbd">⌘.</span>`),
        ].join(""), false)}</div>`,
    },
    requests: {
      group: "请求与数据", title: "请求默认值", changed: 0,
      html: () => [
        sec("请求默认值", [
          row("超时时间", "单个请求等待响应的最长时间。", num("30", "秒"), "下一次请求生效"),
          row("自动重试次数", "网络错误或 5xx 响应时的默认重试上限。", num("0", "次")),
          row("HTTP 版本", "协商 HTTP/1.1 或 HTTP/2。", `<button class="select-pill" data-toast="HTTP 版本：自动协商（HTTP/1.1 与 HTTP/2）">自动 ▾</button>`),
          row("跟随重定向", "收到 3xx 状态码时自动跟进跳转。", sw(true)),
          row("最大重定向次数", "单次请求允许的最大跳转跳数。", num("10", "次")),
          row("验证 TLS 证书", "关闭仅建议用于临时调试，自签名服务推荐在「网络」中配置 CA 包。", sw(true)),
        ].join("")),
        sec("编辑器", [
          row("Tab 缩进宽度", "请求体与脚本编辑器的缩进空格数。", num("2", "空格")),
          row("自动换行", "超出编辑器宽度时折行显示。", sw(true)),
          row("编辑语法高亮上限", "超过该体积暂停语法高亮以保证输入流畅。", num("512", "KB")),
        ].join("")),
        sec("响应与大体积正文", [
          row("查看高亮上限", "响应正文超过该体积切换为纯文本虚拟视图。", num("2", "MB")),
          row("响应正文最大缓存", "单次响应落盘缓存的最大字节数。", num("64", "MB")),
          row("内联阈值 / 预览字节", "大于内联阈值的正文按句柄流式预览，不整体塞入界面状态。", num("256", "KB")),
        ].join("")),
      ].join(""),
    },
    network: {
      group: "请求与数据", title: "网络", changed: 0,
      html: () => [
        sec("代理", [
          row("代理模式", "所有 HTTP 请求、探测与模型调用的出站代理规则。", seg(["系统代理", "自定义代理", "不使用代理"], 0)),
          row("自定义代理 URL", "支持 http://、https://、socks5://，附带一键连通性测试。", `<div class="row-center"><div class="input-box" style="width:220px"><input placeholder="http://127.0.0.1:7890" /></div><button class="btn btn-sm" data-toast="代理连接成功（127.0.0.1:7890 · 14 ms）">测试连接</button></div>`),
          row("绕过代理主机（NO_PROXY）", "本地地址 127.0.0.1、localhost 默认直连。", `<div class="input-box" style="width:220px"><input value="localhost, 127.0.0.1, *.local" /></div>`),
        ].join("")),
        sec("TLS 根证书", row("自定义 CA 证书包", "选择 PEM / CRT 文件后立即校验并统计可用证书数；工作区级客户端证书在「环境 › 证书」管理。", `<button class="btn btn-sm" data-toast="已打开证书文件选择器（支持 PEM / CRT）">选择证书文件…</button>`, "仅这台电脑")),
      ].join(""),
    },
    mock: {
      group: "请求与数据", title: "Mock 服务", changed: 0,
      html: () => [
        sec("新服务默认值", [
          row("起始监听端口", "新建 Mock 服务时自动分配的起始端口（不可与网关端口 4141 冲突）。", num("4010"), "新建时取值"),
          row("默认响应延迟", "新建路由时的默认模拟耗时。", num("0", "ms"), "新建时取值"),
          row("默认开启 CORS", "为本地前端联调自动附加跨域响应头。", sw(true), "新建时取值"),
        ].join("")),
        sec("流量日志", row("每个服务保留的命中条数", "同时作用于正在运行的 Mock 服务内存环形日志。", num("500", "条"))),
      ].join(""),
    },
    data: {
      group: "请求与数据", title: "数据与同步", changed: 0,
      html: () => [
        sec("本地存储", [
          row("数据目录（SQLite + 明文保险库 0600）", "所有数据仅存本机，无云端同步与遥测。", `<div class="row-center"><span class="tag mono">42.6 MB</span><button class="btn btn-sm" data-toast="已在访达中打开 ~/Library/Application Support/com.pingdeck.desktop">在访达中打开</button></div>`, "仅这台电脑"),
          row("大正文与附件缓存", "可随时清理缓存文件，不影响集合与环境定义。", `<div class="row-center"><span class="tag mono">11.2 MB</span><button class="btn btn-sm" data-toast="已清理 11.2 MB 正文与附件缓存">清理缓存</button></div>`),
        ].join(""), false),
        sec("历史与对话保留", [
          row("请求历史保留期限", "到期记录自动清理。", `<button class="select-pill" data-toast="当前保留策略：30 天（可选 7 / 30 / 90 天）">30 天 ▾</button>`),
          row("对话回收站保留", "删除的对话在彻底清除前保留的天数。", `<button class="select-pill" data-toast="回收站保留：14 天">14 天 ▾</button>`),
        ].join("")),
        sec("同步文件夹", [
          row("本地文件夹同步目录", "将集合、环境、Mock 与流程导出为可读 JSON 以便 Git 协作（不含秘密值）。", `<button class="btn btn-sm" data-toast="已打开目录选择器（仅导出非秘密定义）">选择同步目录…</button>`, "仅这台电脑"),
          row("变更时自动导出", "保存工作区定义后自动写入同步目录。", sw(false)),
        ].join("")),
      ].join(""),
    },
    "ai/agent": {
      group: "智能体", title: "默认", changed: 0,
      html: () => [
        sec("新对话默认配置", [
          row("默认模型", "新建对话时自动选中的主模型或网关别名。", `<button class="select-pill mono" data-popover="chat-model">deepseek-v4-pro ▾</button>`),
          row("主备用链", "主模型失败或超时后的自动回退顺序。", `<span class="tag mono">kimi-k2.6</span> <span class="text-faint">→</span> <span class="tag mono">claude-sonnet-5-5</span>`),
          row("默认思考力度", "支持推理强度的模型按此默认档位运行。", seg(["自动", "低", "中", "高"], 0)),
          row("默认项目目录", "新对话自动绑定的工作目录。", `<button class="btn btn-sm" data-toast="已打开默认项目目录选择器">~/me/PingDeck</button>`, "仅这台电脑"),
          row("自动命名对话", "首轮对话结束后自动生成简短标题。", sw(true)),
        ].join("")),
        sec("上下文窗口", [
          row("自动压缩上下文", "接近模型上下文上限时自动摘要早期对话。", sw(true)),
          row("触发压缩阈值", "上下文占用超过该比例时启动压缩。", num("80", "%")),
          row("保留最近轮数", "压缩时完整保留的最近对话轮数。", num("6", "轮")),
        ].join("")),
        sec("运行限制与费用上限（默认收起，可展开调整）", [
          row("单轮最大工具调用步数", "防止无限工具循环的安全上限。", num("30", "次")),
          row("最大并发顶层运行数", "同时在后台运行的对话上限。", num("3", "个")),
          row("单次运行费用暂停上限", "留空表示关闭；触发后弹出费用门禁由你决定是否继续。", `<div class="row-center">${num("2.0", "USD")}${num("15.0", "CNY")}</div>`),
        ].join("")),
      ].join(""),
    },
    "ai/personalization": {
      group: "智能体", title: "个性化", changed: 0,
      html: () => sec("我的说明（全局指令）", `
        <p class="set-row-desc">所有对话共用的一段固定偏好说明（上限 4,000 字符），会作为用户偏好附加在系统提示之后。</p>
        <textarea class="code-box w-full" style="height:140px;resize:vertical;outline:none">回答优先使用简洁中文；涉及接口排错时先给出根因定位，再给出最小复现步骤与修改建议。</textarea>
        <div class="row-between fs-2xs text-muted"><span>46 / 4000 字符</span><button class="btn btn-sm" data-toast="已保存全局个性化指令">立即保存</button></div>`, false, true),
    },
    "ai/memory": {
      group: "智能体", title: "项目指令与信任", changed: 0,
      html: () => sec("已信任的项目目录", `
        <p class="set-row-desc">PingDeck 仅从已明确信任的目录读取项目指令（AGENTS.md / CLAUDE.md）与项目内技能，不做静默跨项目记忆。</p>
        <table class="kv-table">
          <thead><tr><th>目录路径</th><th>读取内容</th><th>授权时间</th><th style="width:92px">操作</th></tr></thead>
          <tbody>
            <tr><td class="mono">~/me/PingDeck</td><td><span class="tag">AGENTS.md</span> <span class="tag">2 项技能</span></td><td class="text-muted">今天 09:10</td><td><button class="btn btn-sm btn-danger" data-toast="已撤销 ~/me/PingDeck 的信任授权" data-toast-tone="warn">撤销信任</button></td></tr>
            <tr><td class="mono">~/work/billing-api</td><td><span class="tag">AGENTS.md</span></td><td class="text-muted">3 天前</td><td><button class="btn btn-sm btn-danger" data-toast="已撤销 ~/work/billing-api 的信任授权" data-toast-tone="warn">撤销信任</button></td></tr>
          </tbody>
        </table>`, false, true),
    },
    "ai/subagents": {
      group: "智能体", title: "子智能体", changed: 0,
      html: () => [
        sec("并发与内置类型", [
          row("单次运行最大子智能体数", "限制主智能体一次可派生的并发子任务数量。", num("4", "个")),
          row("Explore（只读探索）", "仅具备搜索、读文件等只读工具，无网络写入权限。", `<button class="select-pill mono" data-popover="chat-model">deepseek-v4-flash ▾</button>`),
          row("General（通用执行）", "继承主会话权限评估顺序与沙箱约束。", `<button class="select-pill mono" data-popover="chat-model">跟随主模型 ▾</button>`),
        ].join("")),
      ].join(""),
    },
    "ai/tools": {
      group: "智能体", title: "工具", changed: 0,
      html: () => [
        sec("内置工具审批策略", `
          <table class="kv-table">
            <thead><tr><th>工具</th><th>用途说明</th><th style="width:150px">默认策略</th></tr></thead>
            <tbody>
              <tr><td class="mono">read_file / grep</td><td>读取与搜索工作区文件</td><td><button class="select-pill" data-toast="策略可选：跟随模式 / 总是允许 / 总是询问 / 禁止">跟随模式 ▾</button></td></tr>
              <tr><td class="mono">edit_file / write_file</td><td>修改或新建本地文件（编辑前自动快照）</td><td><button class="select-pill" data-toast="策略可选：跟随模式 / 总是允许 / 总是询问 / 禁止">跟随模式 ▾</button></td></tr>
              <tr><td class="mono">shell</td><td>在 OS 沙箱内执行终端命令</td><td><button class="select-pill" data-toast="shell 默认策略：总是询问">总是询问 ▾</button></td></tr>
              <tr><td class="mono">send_request</td><td>执行工作区 HTTP 请求（受生产环境确认约束）</td><td><button class="select-pill" data-toast="send_request 默认策略：跟随模式">跟随模式 ▾</button></td></tr>
            </tbody>
          </table>`, false, true),
        sec("联网搜索", row("搜索后端", "配置 web_search 工具的检索提供方。", `<button class="select-pill" data-toast="当前搜索后端：DuckDuckGo / 本地配置">DuckDuckGo / 本地配置 ▾</button>`)),
      ].join(""),
    },
    "ai/permissions": {
      group: "智能体", title: "权限", changed: 0,
      html: () => [
        sec("固定权限评估顺序（从上到下，命中即止）", `
          <div class="perm-order" style="margin-top:0">
            <div class="perm-step"><span class="perm-ix">1</span><b>硬阻止</b><span class="text-muted">敏感系统路径、越界写入等不可覆盖的安全底线</span><span class="tag">内置</span></div>
            <div class="perm-step"><span class="perm-ix">2</span><b>工具禁止</b><span class="text-muted">在「工具」页显式设为禁止的内置或 MCP 工具</span><span class="tag">0 条</span></div>
            <div class="perm-step"><span class="perm-ix">3</span><b>强制询问</b><span class="text-muted">跨工作区操作、危险命令模式与越权请求</span><span class="tag">内置</span></div>
            <div class="perm-step"><span class="perm-ix">4</span><b>已记住的规则</b><span class="text-muted">你曾点击“允许范围”保存的作用域放行规则</span><button class="tag tag-accent" data-toast="当前共 2 条作用域规则（可在明细中逐条撤销）">2 条</button></div>
            <div class="perm-step"><span class="perm-ix">5</span><b>单工具允许</b><span class="text-muted">在「工具」页显式设为总是允许的工具</span><span class="tag">1 条</span></div>
            <div class="perm-step"><span class="perm-ix">6</span><b>模式默认</b><span class="text-muted">按对话当前权限模式决定（当前默认：自动批准只读）</span><span class="text-faint">—</span></div>
          </div>
          <p class="set-row-desc">完全权限（Full access）不降低前 3 层防护，也不会关闭 OS 沙箱。</p>`, false, true),
        sec("默认权限与沙箱", [
          row("新对话默认权限模式", "可在单个对话底部模式芯片临时切换。", seg(["每次询问", "自动批准只读"], 1)),
          row("无人应答审批超时", "超时后自动标记为超时未运行（不记作拒绝）。", num("600", "秒")),
          row("Shell 命令执行超时", "单条沙箱命令最长执行时间。", num("120", "秒")),
        ].join("")),
      ].join(""),
    },
    "ai/mcp": {
      group: "智能体", title: "MCP 服务器", changed: 0,
      html: () => sec("外部工具服务器", `
        <div class="row-center"><button class="btn btn-sm btn-primary" data-modal="add-mcp">+ 添加 MCP 服务器</button><button class="btn btn-sm" data-modal="mcp-json">编辑 JSON…</button></div>
        <table class="kv-table">
          <thead><tr><th>服务器</th><th>传输</th><th>状态</th><th style="width:90px">启用</th></tr></thead>
          <tbody>
            <tr><td><b>github</b></td><td class="mono">HTTP</td><td><button class="tag tag-warn" data-modal="add-mcp">▲ 需要处理：缺少令牌</button></td><td>${sw(true)}</td></tr>
            <tr><td><b>filesystem</b></td><td class="mono">stdio</td><td><span class="tag tag-ok">● 已连接 · 8 个工具</span></td><td>${sw(true)}</td></tr>
            <tr><td><b>linear</b></td><td class="mono">HTTP</td><td><span class="tag">已停用</span></td><td>${sw(false)}</td></tr>
          </tbody>
        </table>`, false, true),
    },
    "ai/skills": {
      group: "智能体", title: "技能", changed: 0,
      html: () => sec("技能目录与已发现技能", `
        <div class="row-center">
          <button class="btn btn-sm" data-toast="重新扫描完成：发现 2 项有效技能">重新扫描</button>
          <button class="btn btn-sm btn-primary" data-toast="已打开目录选择器以添加技能目录">+ 添加目录</button>
          <button class="btn btn-sm" data-modal="import-export">导入技能…</button>
        </div>
        <table class="kv-table">
          <thead><tr><th>技能名称</th><th>来源目录</th><th>触发方式</th></tr></thead>
          <tbody>
            <tr><td><b>api-contract-audit</b></td><td class="mono">~/.pingdeck/skills</td><td><span class="tag mono">/api-contract-audit</span></td></tr>
            <tr><td><b>sql-migration-review</b></td><td class="mono">~/me/PingDeck/.skills</td><td><span class="tag">项目信任目录</span></td></tr>
          </tbody>
        </table>`, false, true),
    },
    "ai/routing": {
      group: "模型", title: "重试与故障转移", changed: 0,
      html: () => sec("模型调用重试策略（对话、子智能体与网关共用）", [
        row("单次调用最大重试次数", "首个内容事件发出后绝不再切换目标。", num("2", "次")),
        row("单次重试退避上限", "遇到 429 时优先遵循上游 Retry-After 头。", num("15", "秒")),
        row("网关备用规则", "按主模型配置的有序故障转移链位于「服务商 › 网关 › 备用规则」。", `<button class="btn btn-sm" data-nav-to="providers" data-sub-view="gateway" data-tab-jump="gw-main:rules">前往本地网关 ↗</button>`),
      ].join("")),
    },
    "ai/catalog": {
      group: "模型", title: "模型与价格", changed: 0,
      html: () => [
        sec("同步", row("models.dev · 内置快照", "524个模型、937条报价，自建0个（离线时继续使用本地缓存）。", `<div class="row-center"><span class="fs-xs text-muted">自动同步</span>${sw(false)}<button class="btn btn-sm" data-toast="已从 models.dev 同步完成">立即同步</button></div>`), false),
        sec("模型", `
          <div class="row-between"><div class="input-box sunken" style="width:260px"><input data-filter-rows="#catalog-rows tr" placeholder="按名称、ID、厂商或分发商搜索…" /></div><button class="btn btn-sm btn-primary" data-drawer="catalog-model">+ 新增自定义模型</button></div>
          <table class="kv-table">
            <thead><tr><th>模型</th><th>上下文</th><th class="text-right">输入 / 1M</th><th class="text-right">输出 / 1M</th></tr></thead>
            <tbody id="catalog-rows">
              <tr class="clickable" data-drawer="catalog-model"><td class="mono text-accent">deepseek-v4-pro</td><td class="mono">128K</td><td class="mono text-right">¥2.00</td><td class="mono text-right">¥8.00</td></tr>
              <tr class="clickable" data-drawer="catalog-model"><td class="mono text-accent">claude-sonnet-5-5</td><td class="mono">200K</td><td class="mono text-right">$3.00</td><td class="mono text-right">$15.00</td></tr>
              <tr class="clickable" data-drawer="catalog-model"><td class="mono text-accent">deepseek-v4-flash</td><td class="mono">128K</td><td class="mono text-right">¥0.50</td><td class="mono text-right">¥2.00</td></tr>
              <tr class="clickable" data-drawer="catalog-model"><td class="mono text-accent">kimi-k2.6</td><td class="mono">256K</td><td class="mono text-right">¥4.00</td><td class="mono text-right">¥16.00</td></tr>
            </tbody>
          </table>`, false, true),
        sec("未收录的模型", `
          <p class="set-row-desc">你的服务商提供、但目录里还没有的 2 个模型；点击行可关联到目录模型或单独定价。</p>
          <table class="kv-table">
            <thead><tr><th>服务商</th><th>模型 ID</th><th>价格</th><th style="width:68px">操作</th></tr></thead>
            <tbody>
              <tr class="clickable" data-drawer="catalog-unlisted"><td>relay.acme.dev</td><td class="mono text-accent">gemini-4-neon</td><td class="text-muted">无价格</td><td><button class="btn btn-sm" data-drawer="catalog-unlisted">关联…</button></td></tr>
              <tr class="clickable" data-drawer="catalog-unlisted"><td>relay.acme.dev</td><td class="mono text-accent">deepseek-v4-exp</td><td class="text-muted">无价格</td><td><button class="btn btn-sm" data-drawer="catalog-unlisted">关联…</button></td></tr>
            </tbody>
          </table>`, false, true),
      ].join(""),
    },
    "ai/usage": {
      group: "模型", title: "用量统计", changed: 0,
      html: () => [
        sec("日志保留", row("调用日志保留天数", "到期自动清理明细，不影响历史汇总。", `<button class="select-pill" data-toast="日志保留期限：90 天">90 天 ▾</button>`)),
        sec("余额自动刷新", row("服务商余额轮询间隔", "定期刷新支持余额查询的服务商额度。", `<button class="select-pill" data-toast="余额轮询间隔：每 15 分钟">每 15 分钟 ▾</button>`)),
      ].join(""),
    },
    advanced: {
      group: "系统", title: "高级", changed: 0,
      html: () => [
        sec("脚本执行上限", [
          row("脚本循环最大迭代数", "防止预请求/测试脚本死循环。", num("100000")),
          row("脚本最大递归深度", "Boa 运行时栈深度限制。", num("256")),
          row("单脚本最多 send_request 次数", "限制脚本发起的并发/串行子请求数。", num("20", "次")),
        ].join("")),
        sec("控制台上限", [
          row("控制台最大条目数", "超出后按 FIFO 滚动淘汰。", num("2000", "条")),
          row("单条消息最大字节", "超长日志自动截断并提示。", num("64", "KB")),
          row("单次运行最大测试结果数", "防止超大脚本产出过多断言行。", num("500", "条")),
        ].join("")),
        sec("危险区", row("重置全部设置", "将全部设置项恢复出厂默认值（不删除集合、环境与服务商记录，60 秒内可撤销）。", `<button class="btn btn-sm btn-danger" data-modal="reset-all">重置全部设置…</button>`), false),
      ].join(""),
    },
    about: {
      group: "系统", title: "关于", changed: 0,
      html: () => `
        <div class="set-card set-card-pad">
          <div class="row-center" style="gap:14px">
            <div style="width:48px;height:48px;border-radius:12px;background:var(--accent);color:var(--on-accent);display:grid;place-items:center;font-weight:700;font-size:19px">P</div>
            <div>
              <div style="font-size:16px;font-weight:600">PingDeck <span class="tag">Desktop</span></div>
              <div class="fs-xs text-muted">版本 0.1.0 · 本地优先桌面 API 工作台与 AI 网关</div>
              <div class="fs-2xs text-faint">无账号 · 无云同步 · 无遥测</div>
            </div>
          </div>
        </div>
        ${sec("构建与系统", [
          row("运行环境", "", `<span class="mono fs-xs">macOS · GPUI + Rust 2024</span>`),
          row("诊断信息", "复制脱敏后的系统、版本与数据库健康摘要，用于问题排查。", `<button class="btn btn-sm" data-toast="已复制脱敏诊断摘要到剪贴板">复制诊断信息</button>`),
        ].join(""), false)}`,
    },
  };
})();
