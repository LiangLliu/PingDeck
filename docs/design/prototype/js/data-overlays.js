/* PingDeck Prototype — Overlay Templates (Popovers, Dialogs/Wizards, Slide-Over Drawers) */
window.PD = window.PD || {};

window.PD.POPOVER_TEMPLATES = {
  workspace: () => `
    <div class="pop-head"><span>切换工作区</span><span class="tag">本地存储</span></div>
    <button class="pop-opt active" data-pop-toast="当前工作区：Billing platform"><span class="ws-badge" style="width:18px;height:18px;font-size:10px">B</span><div class="pop-opt-col"><span>Billing platform</span><span class="pop-opt-sub">17 个请求 · 3 个环境 · 仅本机</span></div><span>✓</span></button>
    <button class="pop-opt" data-pop-toast="已切换至工作区：Internal Gateway"><span class="ws-badge" style="width:18px;height:18px;font-size:10px;background:var(--accent)">I</span><div class="pop-opt-col"><span>Internal Gateway</span><span class="pop-opt-sub">9 个请求 · 2 个环境 · 文件夹同步</span></div></button>
    <div class="pop-sep"></div>
    <button class="pop-opt" data-modal="new-workspace">+ 新建工作区…</button>
    <button class="pop-opt" data-modal="import-export">导入 / 导出工作区…</button>`,
  running: () => `
    <div class="pop-head"><span>后台工作与需要关注</span><span class="tag">实时</span></div>
    <button class="pop-opt" data-nav-to="chat"><span class="dot dot-warn"></span><div class="pop-opt-col"><span>等待你批准 · cargo clippy</span><span class="pop-opt-sub">对话：429s from the gateway</span></div><span class="tag tag-warn">去处理</span></button>
    <button class="pop-opt" data-nav-to="mock"><span class="dot dot-live"></span><div class="pop-opt-col"><span>Mock 服务运行中</span><span class="pop-opt-sub">Billing stub (:4010)、Webhook sink (:4011)</span></div><span class="mono">2</span></button>
    <button class="pop-opt" data-nav-to="providers" data-sub-view="relay"><span class="dot dot-warn"></span><div class="pop-opt-col"><span>需要关注的服务商</span><span class="pop-opt-sub">relay.acme.dev · 2/34 模型测试失败</span></div><span class="mono">1</span></button>
    <div class="pop-sep"></div>
    <button class="pop-opt" data-nav-to="flows"><span class="dot dot-run"></span><div class="pop-opt-col"><span>流程运行中</span><span class="pop-opt-sub">Nightly billing smoke · 第 3 步 / 6</span></div></button>`,
  method: () => ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"].map((m) =>
    `<button class="pop-opt" data-pick-method="${m}"><span class="m-${m.toLowerCase()}" style="width:52px">${m}</span><span class="text-muted">${m === "GET" ? "读取资源" : m === "POST" ? "创建/提交" : m === "PUT" ? "全量替换" : m === "PATCH" ? "局部更新" : m === "DELETE" ? "删除资源" : "元信息/预检"}</span></button>`
  ).join(""),
  "env-pick": () => `
    <div class="pop-head"><span>当前工作区环境</span><button class="text-accent" data-nav-to="environments">管理 ↗</button></div>
    <button class="pop-opt" data-pick-env="Local" data-env-dot="idle"><span class="dot dot-idle"></span><div class="pop-opt-col"><span>Local</span><span class="pop-opt-sub">无变量覆盖</span></div></button>
    <button class="pop-opt active" data-pick-env="Staging" data-env-dot="ok"><span class="dot dot-ok"></span><div class="pop-opt-col"><span>Staging</span><span class="pop-opt-sub">10 个变量 · billing.staging.acme.dev</span></div><span>✓</span></button>
    <button class="pop-opt" data-pick-env="Production" data-env-dot="warn"><span class="dot dot-warn"></span><div class="pop-opt-col"><span>Production</span><span class="pop-opt-sub">发送前按方法确认 · 保护已开</span></div><span class="tag tag-warn">确认</span></button>`,
  "chat-perm": () => `
    <div class="pop-head"><span>此对话权限模式</span><button class="text-accent" data-nav-to="settings" data-set-page="ai/permissions">评估顺序 ↗</button></div>
    <button class="pop-opt" data-pick-perm="每次询问" data-perm-tone=""><span class="dot dot-idle"></span><div class="pop-opt-col"><span>每次询问</span><span class="pop-opt-sub">所有读写与命令工具均先由你批准</span></div></button>
    <button class="pop-opt active" data-pick-perm="自动批准只读" data-perm-tone="ok"><span class="dot dot-ok"></span><div class="pop-opt-col"><span>自动批准只读</span><span class="pop-opt-sub">读文件/搜索自动放行，修改与命令须确认</span></div><span>✓</span></button>
    <button class="pop-opt" data-pick-perm="计划模式" data-perm-tone="accent"><span class="dot" style="background:var(--accent)"></span><div class="pop-opt-col"><span>计划模式（Plan）</span><span class="pop-opt-sub">先产出可编辑方案，待你批准后再动手</span></div></button>
    <div class="pop-sep"></div>
    <button class="pop-opt" data-modal="full-access"><span class="dot dot-warn"></span><div class="pop-opt-col"><span class="text-warn fw-600">完全权限（Full access）…</span><span class="pop-opt-sub">仍保留硬阻止、强制询问与 OS 沙箱</span></div></button>`,
  "chat-model": () => `
    <div class="pop-head"><span>选择模型（服务商/模型）</span><span class="tag">TTFT</span></div>
    <button class="pop-opt active" data-pick-model="DeepSeek/deepseek-v4-pro"><div class="pop-opt-col"><span class="mono">DeepSeek/deepseek-v4-pro</span><span class="pop-opt-sub">128K · 推理 + 工具</span></div><span class="mono text-muted">412ms</span></button>
    <button class="pop-opt" data-pick-model="Anthropic/claude-sonnet-5-5"><div class="pop-opt-col"><span class="mono">Anthropic/claude-sonnet-5-5</span><span class="pop-opt-sub">200K · Messages</span></div><span class="mono text-muted">690ms</span></button>
    <button class="pop-opt" data-pick-model="DeepSeek/deepseek-v4-flash"><div class="pop-opt-col"><span class="mono">DeepSeek/deepseek-v4-flash</span><span class="pop-opt-sub">128K · 快速低价</span></div><span class="mono text-muted">233ms</span></button>
    <button class="pop-opt" data-pick-model="Ollama/qwen3-coder:30b"><div class="pop-opt-col"><span class="mono">Ollama/qwen3-coder:30b</span><span class="pop-opt-sub">本地免费 · 127.0.0.1</span></div><span class="mono text-muted">38ms</span></button>`,
  "chat-ws": () => `
    <div class="pop-head"><span>对话上下文绑定</span></div>
    <div style="padding:6px 8px;font-size:11.5px;color:var(--ink-3);line-height:1.5">已绑定工作区 <b class="text-ink1">Billing platform</b> 与项目目录 <code class="text-ink1">~/me/PingDeck</code>。跨工作区读取将触发强制询问。</div>
    <div class="pop-sep"></div>
    <button class="pop-opt" data-pop-toast="已打开目录选择器（仅桌面端）">更改项目目录…</button>
    <button class="pop-opt" data-nav-to="settings" data-set-page="ai/memory">管理已信任目录 ↗</button>`,
  "ai-protocol": () => ["Chat Completions", "Anthropic Messages", "OpenAI Responses", "Embeddings"].map((p) =>
    `<button class="pop-opt" data-pick-ai-proto="${p}"><span>${p}</span></button>`
  ).join(""),
  "usage-range": () => ["今天", "本月", "上月", "1h", "24h", "7d", "30d", "90d", "全部"].map((r, i) =>
    `<button class="pop-opt${i === 5 ? " active" : ""}" data-pick-range="${r}"><span>${r}</span>${i === 5 ? `<span class="ml-auto">✓</span>` : ""}</button>`
  ).join(""),
};

window.PD.renderDialogSpec = (id, { sniffStep, connectTool, snippetLang, importTab }) => {
  const D = {
    "new-request": {
      title: "新建请求或集合",
      body: `
        <div class="field-col"><span class="field-label">类型</span><div class="seg"><button class="active" data-action="seg-pick">HTTP 请求</button><button data-action="seg-pick">AI 协议请求</button><button data-action="seg-pick">集合 / 目录</button></div></div>
        <div class="field-col"><span class="field-label">名称</span><div class="input-box"><input value="List customers" /></div></div>
        <div class="field-col"><span class="field-label">保存到位置</span><div class="input-box sunken"><input value="Billing API / Customers" readonly /></div></div>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已创建请求「List customers」">创建并打开</button>`,
    },
    "new-workspace": {
      title: "新建工作区",
      body: `
        <div class="field-col"><span class="field-label">工作区名称</span><div class="input-box"><input placeholder="例如：Payments API" /></div></div>
        <p class="set-row-desc">工作区隔离集合、环境、Mock 与流程；AI 服务商、网关与对话在所有工作区之间共用。</p>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已创建新工作区">创建工作区</button>`,
    },
    "import-export": {
      size: "lg",
      tone: importTab === "secrets" ? "warn" : "",
      title: "导入与导出工作区数据",
      body: `
        <div class="seg" style="align-self:flex-start">
          <button class="${importTab === "import" ? "active" : ""}" data-import-tab="import">导入预览</button>
          <button class="${importTab === "export" ? "active" : ""}" data-import-tab="export">导出 JSON（脱敏）</button>
          <button class="${importTab === "secrets" ? "active" : ""}" data-import-tab="secrets">明文含秘密导出…</button>
        </div>
        ${importTab === "import" ? `
        <div class="card-panel">
          <div class="card-panel-head"><span>支持的格式</span><span class="tag">先预览，不直接覆盖</span></div>
          <div class="row-wrap"><span class="tag tag-accent">OpenAPI 3.1</span><span class="tag">Postman Collection v2.1</span><span class="tag">cURL 命令</span><span class="tag">PingDeck 同步包</span></div>
          <div class="code-box" style="height:105px;white-space:pre">检测到 OpenAPI 3.1 规范：Billing API v2
  + 新增 4 个请求（/subscriptions, /coupons…）
  ~ 保留已有 17 个请求（按路径去重，不覆盖本地脚本）
  • 环境变量默认跳过秘密值（0 个秘密写入）</div>
        </div>` : importTab === "export" ? `
        <div class="card-panel">
          <div class="card-panel-head"><span>导出内容摘要 · Billing platform</span><span class="tag tag-ok">安全：不含秘密值</span></div>
          <table class="kv-table">
            <thead><tr><th>对象类型</th><th>数量</th><th>导出处理</th></tr></thead>
            <tbody>
              <tr><td>集合与请求</td><td class="mono">1 集合 · 17 请求</td><td>完整导出</td></tr>
              <tr><td>环境与变量</td><td class="mono">3 环境 · 18 变量</td><td>2 个秘密变量自动置空</td></tr>
              <tr><td>Mock 服务与流程</td><td class="mono">3 Mock · 4 流程</td><td>完整导出（不含运行日志）</td></tr>
            </tbody>
          </table>
        </div>` : `
        <div class="card-panel" style="border-color:var(--warn-border)">
          <div class="card-panel-head"><span class="text-warn">▲ 导出文件将包含 2 个明文秘密值</span><span class="tag tag-warn">请勿提交至公开仓库</span></div>
          <p class="set-row-desc">包含 <code>Staging.token</code> 与 <code>Production.token</code> 的明文内容，仅建议用于本机离线迁移或加密归档。</p>
          <label class="check-row" style="background:var(--warn-soft);border-color:var(--warn-border)"><input id="secret-export-ack" type="checkbox" /><span>我确认将妥善保管导出的明文 JSON 文件。</span></label>
        </div>`}`,
      foot: importTab === "import"
        ? `<span class="dialog-foot-note">不可信导入内容不会自动执行脚本或发起网络请求</span><button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已导入 4 个请求到 Billing API">确认应用导入</button>`
        : importTab === "export"
          ? `<span class="dialog-foot-note">导出格式兼容 PingDeck 文件夹同步规范</span><button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已导出 billing-platform.pingdeck.json（已脱敏）">导出脱敏 JSON</button>`
          : `<span class="dialog-foot-note">仅单次导出生效，不会修改默认同步设置</span><button class="btn" data-action="close-dialog">取消</button><button class="btn btn-danger" id="secret-export-confirm" disabled data-dialog-done="已导出含明文秘密的备份文件" data-toast-tone="warn">导出含秘密 JSON</button>`,
    },
    "code-snippets": {
      size: "lg",
      title: "生成请求代码片段 · GET List invoices",
      body: `
        <div class="seg" style="align-self:flex-start">
          <button class="${snippetLang === "curl" ? "active" : ""}" data-snippet-lang="curl">cURL</button>
          <button class="${snippetLang === "fetch" ? "active" : ""}" data-snippet-lang="fetch">TypeScript fetch</button>
          <button class="${snippetLang === "python" ? "active" : ""}" data-snippet-lang="python">Python httpx</button>
          <button class="${snippetLang === "rust" ? "active" : ""}" data-snippet-lang="rust">Rust reqwest</button>
        </div>
        <div class="code-box" style="padding:12px 14px;white-space:pre-wrap">${
          snippetLang === "curl"
            ? `curl --request GET \\\n  --url 'https://billing.staging.acme.dev/invoices?status=open&amp;limit=25' \\\n  --header 'Accept: application/json' \\\n  --header 'Authorization: Bearer &lt;token&gt;'`
            : snippetLang === "fetch"
              ? `const res = await fetch("https://billing.staging.acme.dev/invoices?status=open&amp;limit=25", {\n  headers: { Accept: "application/json", Authorization: "Bearer &lt;token&gt;" },\n});\nconst data = await res.json();`
              : snippetLang === "python"
                ? `import httpx\n\nresp = httpx.get(\n    "https://billing.staging.acme.dev/invoices",\n    params={"status": "open", "limit": 25},\n    headers={"Authorization": "Bearer &lt;token&gt;"},\n)`
                : `let resp = reqwest::Client::new()\n    .get("https://billing.staging.acme.dev/invoices?status=open&amp;limit=25")\n    .bearer_auth("&lt;token&gt;")\n    .send()\n    .await?;`
        }</div>
        <span class="set-row-desc">秘密变量 <code>{{token}}</code> 已自动脱敏为占位符 <code>&lt;token&gt;</code>，避免复制泄漏。</span>`,
      foot: `<button class="btn" data-action="close-dialog">关闭</button><button class="btn btn-primary" data-dialog-done="已复制代码片段到剪贴板">复制代码</button>`,
    },
    "new-mock": {
      title: "新建本地 Mock 服务",
      body: `
        <div class="field-col"><span class="field-label">服务名称</span><div class="input-box"><input value="Payments stub" /></div></div>
        <div class="field-grid">
          <div class="field-col"><span class="field-label">监听端口（127.0.0.1）</span><div class="input-box"><input value="4013" /></div></div>
          <div class="field-col"><span class="field-label">路径前缀</span><div class="input-box"><input value="/v1" /></div></div>
        </div>
        <div class="set-row" style="border:0;padding:4px 0"><div class="set-row-info"><span class="set-row-label">默认开启 CORS</span><span class="set-row-desc">新建后不会自动启动监听，保存与启动相互独立。</span></div><button type="button" class="switch-ui on" data-action="toggle-switch"></button></div>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已创建 Mock 服务「Payments stub (:4013)」">创建服务</button>`,
    },
    "new-flow": {
      title: "新建流程",
      body: `
        <div class="field-col"><span class="field-label">流程名称</span><div class="input-box"><input placeholder="例如：Checkout smoke test" /></div></div>
        <div class="field-col"><span class="field-label">默认运行环境</span><div class="select-pill w-full">Staging (billing.staging.acme.dev) ▾</div></div>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已创建新流程">创建流程</button>`,
    },
    "new-env": {
      title: "新建环境",
      body: `
        <div class="field-col"><span class="field-label">环境名称</span><div class="input-box"><input placeholder="例如：QA / Sandbox" /></div></div>
        <div class="set-row" style="border:0;padding:4px 0"><div class="set-row-info"><span class="set-row-label">发送前二次确认（生产保护）</span><span class="set-row-desc">开启后，对 POST / PUT / PATCH / DELETE 发送前强制弹窗确认。</span></div><button type="button" class="switch-ui" data-action="toggle-switch"></button></div>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已创建新环境">创建环境</button>`,
    },
    "clear-history": {
      size: "sm", tone: "danger",
      title: "清空全部请求历史？",
      body: `<p>此操作会永久删除当前工作区 <b>Billing platform</b> 下的 48 条请求执行记录及关联响应缓存。</p><p class="text-bad fw-600">此操作不能撤销。</p>`,
      foot: `<button class="btn btn-danger" data-dialog-done="已清空工作区历史记录" data-toast-tone="warn">永久清空</button><button class="btn btn-primary" data-action="close-dialog">取消</button>`,
    },
    "allow-scope": {
      title: "选择允许范围 · shell 命令",
      body: `
        <div class="gate-cmd"><span class="text-faint">$</span><span>cargo clippy -p pingdeck-core -- -D warnings</span></div>
        <div class="col-stack-sm">
          <label class="list-row selected clickable" style="background:var(--bg-surface)"><input type="radio" name="scope" checked /><div><b>允许本仓库内所有 <code>cargo clippy *</code></b><div class="pop-opt-sub">保存为第 4 层作用域规则，随时可在「设置 › 权限」撤销</div></div></label>
          <label class="list-row clickable" style="background:var(--bg-surface);border-color:var(--border-subtle)"><input type="radio" name="scope" /><div><b>仅本次对话允许 <code>cargo *</code></b><div class="pop-opt-sub">关闭或切换对话后自动失效</div></div></label>
        </div>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已保存允许规则并继续执行命令" data-resolve-gate="1">保存规则并执行</button>`,
    },
    "full-access": {
      size: "sm", tone: "danger",
      title: "切换到「完全权限（Full access）」？",
      body: `
        <p>在当前对话中，所有未命中禁止规则的读写、网络与终端工具将不再逐次弹窗询问。</p>
        <p class="fs-xs text-muted">安全底线不变：<b>硬阻止</b>、<b>工具禁止</b>、<b>强制询问</b>以及 <b>OS 沙箱</b>仍然生效。</p>
        <label class="check-row"><input id="full-access-ack" type="checkbox" /><span>我已了解风险，并确认此授权仅作用于当前对话。</span></label>`,
      foot: `<button class="btn btn-danger" id="full-access-confirm" disabled data-dialog-done="当前对话已切换为「完全权限」" data-toast-tone="warn" data-set-full-access="1">开启完全权限</button><button class="btn btn-primary" data-action="close-dialog">取消</button>`,
    },
    "add-provider": {
      size: "lg",
      title: "添加 AI 服务商（智能嗅探向导）",
      body: `
        <div class="stepper">
          <span class="step-pill ${sniffStep === 1 ? "active" : "done"}"><span class="step-n">${sniffStep > 1 ? "✓" : "1"}</span>地址与密钥</span>
          <span class="step-arrow">›</span>
          <span class="step-pill ${sniffStep === 2 ? "active" : sniffStep > 2 ? "done" : ""}"><span class="step-n">${sniffStep > 2 ? "✓" : "2"}</span>协议嗅探证据</span>
          <span class="step-arrow">›</span>
          <span class="step-pill ${sniffStep === 3 ? "active" : ""}"><span class="step-n">3</span>审核模型并保存</span>
        </div>
        ${sniffStep === 1 ? `
          <div class="field-col"><span class="field-label">服务商地址（Base URL）</span><div class="input-box"><input id="sniff-base-url" value="https://relay.acme.dev" /></div></div>
          <div class="field-col"><span class="field-label">API 密钥（明文 0600 保险库存储，不会离开本机）</span><div class="input-box"><input type="password" value="sk-demo-7f3c" /></div></div>
          <div class="row-wrap">
            <button class="tag" data-sniff-preset="https://api.openai.com/v1">OpenAI</button>
            <button class="tag" data-sniff-preset="https://api.anthropic.com">Anthropic</button>
            <button class="tag" data-sniff-preset="https://api.deepseek.com">DeepSeek</button>
            <button class="tag" data-sniff-preset="https://api.moonshot.cn/v1">Moonshot</button>
            <button class="tag" data-sniff-preset="http://127.0.0.1:11434">Ollama 本地</button>
            <button class="tag tag-accent" data-sniff-preset="https://relay.acme.dev">new-api 中转站</button>
          </div>
          <span class="set-row-desc">嗅探会向目标地址发送少量探测请求，可能消耗极少量 Token 或产生费用。</span>
        ` : sniffStep === 2 ? `
          <div class="card-panel">
            <div class="card-panel-head"><span>嗅探完成 · 已识别为 new-api 0.9.7</span><span class="tag tag-ok">密钥有效</span></div>
            <table class="kv-table">
              <thead><tr><th>探测阶段</th><th>证据结果</th><th>状态</th></tr></thead>
              <tbody>
                <tr><td class="mono">identify</td><td><code>/api/status</code> · quota_per_unit = 500000</td><td><span class="tag tag-ok">已确认</span></td></tr>
                <tr><td class="mono">routes</td><td>Chat / Messages / Gemini / Embeddings 路由存在（Responses 404）</td><td><span class="tag tag-ok">4/5 协议</span></td></tr>
                <tr><td class="mono">models</td><td>发现 38 个模型（31 Chat / 3 Embed / 4 Image）</td><td><span class="tag tag-ok">38 个</span></td></tr>
                <tr><td class="mono">balance</td><td>读取到账户余额 $12.40 USD</td><td><span class="tag tag-ok">支持查询</span></td></tr>
              </tbody>
            </table>
          </div>
        ` : `
          <div class="card-panel">
            <div class="card-panel-head"><span>审核导入的模型列表（38 个）</span><span class="tag">3 个新模型 · 4 个已禁用</span></div>
            <table class="kv-table">
              <thead><tr><th style="width:32px"></th><th>模型 ID</th><th>协议</th><th>备注</th></tr></thead>
              <tbody>
                <tr><td><span class="check-box checked">✓</span></td><td class="mono">deepseek-v4-lite</td><td>Chat</td><td><span class="tag tag-accent">新增</span></td></tr>
                <tr><td><span class="check-box checked">✓</span></td><td class="mono">gpt-6.2</td><td>Chat</td><td><span class="tag tag-accent">新增</span></td></tr>
                <tr><td><span class="check-box checked">✓</span></td><td class="mono">gemini-3.3-flash</td><td>Gemini / Chat</td><td><span class="tag tag-accent">新增</span></td></tr>
              </tbody>
            </table>
          </div>`}`,
      foot: sniffStep === 1
        ? `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-sniff-step="2">开始嗅探探测 →</button>`
        : sniffStep === 2
          ? `<button class="btn" data-sniff-step="1">← 返回修改</button><button class="btn btn-primary" data-sniff-step="3">下一步：审核模型 →</button>`
          : `<button class="btn" data-sniff-step="2">← 上一步</button><button class="btn btn-primary" data-dialog-done="已添加服务商「relay.acme.dev」及 38 个模型">保存服务商</button>`,
    },
    "connect-client": {
      size: "lg",
      title: "一键接入本地 AI 编码工具",
      body: `
        <div class="seg" style="align-self:flex-start">
          <button class="${connectTool === "claude" ? "active" : ""}" data-connect-tool="claude">Claude Code</button>
          <button class="${connectTool === "codex" ? "active" : ""}" data-connect-tool="codex">Codex CLI</button>
          <button class="${connectTool === "zed" ? "active" : ""}" data-connect-tool="zed">Zed Editor</button>
        </div>
        <div class="field-grid">
          <div class="field-col"><span class="field-label">目标配置文件</span><div class="input-box sunken"><input value="${connectTool === "claude" ? "~/.claude/settings.json" : connectTool === "codex" ? "~/.codex/config.toml" : "~/.config/zed/settings.json"}" readonly /></div></div>
          <div class="field-col"><span class="field-label">默认模型</span><div class="select-pill w-full mono">${connectTool === "claude" ? "DeepSeek/deepseek-v4-pro" : connectTool === "codex" ? "Anthropic/claude-sonnet-5-5" : "DeepSeek/deepseek-v4-flash"} ▾</div></div>
        </div>
        <div class="diff-box">
          <div class="diff-head"><span>写入前差异预览（写入前自动创建时间戳备份，支持一键还原）</span><span class="tag tag-ok">自动生成独立客户端令牌</span></div>
          ${connectTool === "claude" ? `
          <div class="diff-row del">-  "ANTHROPIC_BASE_URL": "https://api.anthropic.com"</div>
          <div class="diff-row add">+  "ANTHROPIC_BASE_URL": "http://127.0.0.1:4141/anthropic"</div>
          <div class="diff-row add">+  "ANTHROPIC_AUTH_TOKEN": "pdk_c_••••8a1f"</div>` : connectTool === "codex" ? `
          <div class="diff-row del">-  base_url = "https://api.openai.com/v1"</div>
          <div class="diff-row add">+  base_url = "http://127.0.0.1:4141/v1"</div>
          <div class="diff-row add">+  bearer_token = "pdk_c_••••4c02"</div>` : `
          <div class="diff-row del">-  "api_url": "https://api.openai.com/v1"</div>
          <div class="diff-row add">+  "api_url": "http://127.0.0.1:4141/v1",</div>
          <div class="diff-row add">+  "api_key": "pdk_c_••••9e7b"</div>`}
        </div>`,
      foot: `<span class="dialog-foot-note">应用后可执行连通性验证</span><button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已写入配置并备份原文件（可随时一键还原）">备份并写入配置</button>`,
    },
    "benchmark-cost": {
      size: "sm", tone: "warn",
      title: "确认运行模型基准测试？",
      body: `
        <p>将向 <b>relay.acme.dev</b> 下的 <b>34 个已启用模型</b>发送短探针请求，以测量可用性、首字延迟（TTFT）与吞吐速度。</p>
        <div class="card-panel" style="padding:9px 12px;font-size:12px">
          <div class="row-between"><span>预估消耗</span><b class="mono text-warn">约 ¥0.12（≈ $0.017）</b></div>
          <div class="text-muted fs-2xs">依据：34 个模型 × 约 40 输入/输出 Token × 本地目录单价</div>
        </div>`,
      foot: `<button class="btn btn-primary" data-dialog-done="已开始基准测试（可在服务商页实时查看进度）">开始测试（约 ¥0.12）</button><button class="btn" data-action="close-dialog">取消</button>`,
    },
    "add-mcp": {
      title: "添加 MCP 服务器",
      body: `
        <div class="field-grid">
          <div class="field-col"><span class="field-label">服务器名称</span><div class="input-box"><input placeholder="例如：postgres-readonly" /></div></div>
          <div class="field-col"><span class="field-label">传输方式</span><div class="seg" style="align-self:flex-start"><button class="active" data-action="seg-pick">stdio</button><button data-action="seg-pick">Streamable HTTP</button></div></div>
        </div>
        <div class="field-col"><span class="field-label">启动命令或端点 URL</span><div class="input-box"><input placeholder="npx -y @modelcontextprotocol/server-filesystem ~/me" /></div></div>
        <div class="set-row" style="border:0;padding:4px 0"><div class="set-row-info"><span class="set-row-label">信任此服务器</span><span class="set-row-desc">未信任服务器返回的外部文本会被围栏隔离，其工具调用一律需询问。</span></div><button type="button" class="switch-ui" data-action="toggle-switch"></button></div>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已添加 MCP 服务器">保存并测试连接</button>`,
    },
    "mcp-json": {
      size: "lg",
      title: "编辑 MCP 服务器 JSON 配置",
      body: `
        <div class="code-box" style="height:210px;white-space:pre">{
  "mcpServers": {
    "github": { "url": "https://api.githubcopilot.com/mcp/", "enabled": true },
    "filesystem": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "~/me/PingDeck"] }
  }
}</div>
        <span class="set-row-desc">提交前将校验字段合法性与命令安全规则，秘密令牌仍存放在独立保险库中。</span>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-done="已保存 MCP JSON 配置">校验并保存</button>`,
    },
    "restore-page": {
      size: "sm",
      title: "恢复当前页的默认值？",
      body: `<p>当前页面的已更改设置项将回到出厂默认值，业务记录不会被删除。确认后 8 秒内可以撤销。</p>`,
      foot: `<button class="btn" data-action="close-dialog">取消</button><button class="btn btn-primary" data-dialog-undo="已恢复本页默认值">恢复默认值</button>`,
    },
    "reset-all": {
      size: "sm", tone: "danger",
      title: "重置全部 20 页设置？",
      body: `<p>全部偏好设置将恢复为出厂默认值（不删除工作区、集合、环境、服务商与历史记录）。</p><p class="text-muted fs-xs">重置后 60 秒内可在右下角通知条一键撤销。</p>`,
      foot: `<button class="btn btn-danger" data-dialog-undo="已将全部设置恢复出厂默认值">确认重置全部</button><button class="btn btn-primary" data-action="close-dialog">取消</button>`,
    },
  };
  return D[id] || null;
};

window.PD.DRAWER_SPECS = {
  "mock-try": {
    title: "试一下 · 向 Billing stub (:4010) 发送测试请求",
    body: `
      <div class="url-field" style="flex:none;height:34px">
        <span class="url-method"><span class="m-post">POST</span></span>
        <span class="url-text">http://127.0.0.1:4010/v1/refunds</span>
      </div>
      <div class="field-col">
        <span class="field-label">测试请求体（JSON）</span>
        <div class="code-box" style="height:110px;white-space:pre">{
  "invoice_id": "inv_01J9X3K2P8",
  "amount": 12400
}</div>
      </div>
      <div class="row-center" style="justify-content:flex-end"><button class="btn btn-primary" data-action="run-mock-try">发送本地测试</button></div>
      <div id="mock-try-out" class="card-panel">
        <div class="card-panel-head"><span>上次命中结果</span><span class="tag tag-ok">● 201 · 回调 200 · 126 ms</span></div>
        <div class="code-box" style="height:120px;white-space:pre">{
  "id": "re_01J9Y2K8A1",
  "status": "succeeded",
  "route": "refunds.create"
}</div>
      </div>`,
  },
  "mock-hit-detail": {
    title: "Mock 命中与回调详情 · POST /v1/refunds",
    body: `
      <div class="row-wrap"><span class="tag tag-ok">● 201 Created</span><span class="tag">分支 3 · 默认命中</span><span class="tag mono">耗时 128 ms</span></div>
      <div class="card-panel">
        <div class="card-panel-head"><span>收到的客户端请求</span><span class="mono fs-2xs text-muted">14:32:11.084</span></div>
        <div class="code-box" style="height:96px;white-space:pre">POST /v1/refunds HTTP/1.1
Host: 127.0.0.1:4010
Content-Type: application/json

{"invoice_id":"inv_01J9X3K2P8","amount":12400}</div>
      </div>
      <div class="card-panel">
        <div class="card-panel-head"><span>数据桶写入与异步回调结果</span><span class="tag tag-ok">回调 200 OK</span></div>
        <div class="fs-xs text-muted">1. 已向内存数据桶 <code>refunds</code> 写入记录 <code>re_01J9Y2K8A1</code></div>
        <div class="fs-xs text-muted">2. 延迟 500 ms 后向 <code>http://127.0.0.1:4011/hooks/refund-created</code> 投递 Webhook → 收到 200 OK (18 ms)</div>
      </div>`,
    foot: `<button class="btn" data-action="close-drawer">关闭</button><button class="btn btn-primary" data-nav-to="requests" data-action="close-drawer">保存为请求草稿</button>`,
  },
  "usage-call": {
    title: "调用详情 · call_01J9X8M2Q4",
    body: `
      <div class="row-wrap"><span class="tag tag-ok">● 200 OK</span><span class="tag mono">deepseek-v4-pro</span><span class="tag">来源：本地网关 (Claude Code)</span></div>
      <div class="card-panel">
        <div class="card-panel-head"><span>延迟与费用拆解</span><span class="mono fw-600">¥0.0184</span></div>
        <div class="grid-2col fs-xs">
          <div class="col-stack-sm" style="gap:2px"><span class="fs-2xs text-muted">首字延迟 (TTFT)</span><b class="mono">412 ms</b></div>
          <div class="col-stack-sm" style="gap:2px"><span class="fs-2xs text-muted">总耗时</span><b class="mono">2.84 s</b></div>
          <div class="col-stack-sm" style="gap:2px"><span class="fs-2xs text-muted">输入 Token</span><b class="mono">6,420 <span class="text-muted" style="font-weight:400">(缓存 2,100)</span></b></div>
          <div class="col-stack-sm" style="gap:2px"><span class="fs-2xs text-muted">输出 Token</span><b class="mono">840</b></div>
        </div>
      </div>
      <div class="field-col">
        <span class="field-label">响应摘要（已脱敏）</span>
        <div class="code-box" style="height:150px;white-space:pre-wrap">已定位到 router.rs：在收到上游 429 时直接触发了 60s 熔断，建议先解析 Retry-After 响应头。</div>
      </div>
      <div class="row-center">
        <button class="btn btn-sm" data-nav-to="requests" data-sub-view="ai" data-action="close-drawer">在 AI 请求中打开</button>
        <button class="btn btn-sm btn-primary" data-nav-to="chat" data-action="close-drawer">交给智能体排查</button>
      </div>`,
  },
  "catalog-model": {
    size: "wide",
    title: "编辑模型参数与单价 · deepseek-v4-pro",
    body: `
      <div class="field-grid">
        <div class="field-col"><span class="field-label">上下文窗口</span><div class="input-box"><input value="128000" /></div></div>
        <div class="field-col"><span class="field-label">最大输出 Token</span><div class="input-box"><input value="16384" /></div></div>
      </div>
      <div class="field-grid">
        <div class="field-col"><span class="field-label">输入单价（¥ / 1M）</span><div class="input-box"><input value="2.00" /></div></div>
        <div class="field-col"><span class="field-label">输出单价（¥ / 1M）</span><div class="input-box"><input value="8.00" /></div></div>
      </div>
      <div class="field-col"><span class="field-label">能力标记</span><div class="row-wrap"><span class="tag tag-accent">文本输入</span><span class="tag tag-accent">推理思考</span><span class="tag tag-accent">工具调用</span><span class="tag">视觉输入</span></div></div>`,
    foot: `<button class="btn" data-action="close-drawer">取消</button><button class="btn btn-primary" data-drawer-done="已保存 deepseek-v4-pro 单价覆盖">保存更改</button>`,
  },
  "catalog-unlisted": {
    size: "wide",
    title: "gemini-4-neon",
    subtitle: "未收录 · relay.acme.dev",
    body: `
      <div class="col-stack-sm">
        <b class="text-ink1">这是哪个模型？</b>
        <span class="set-row-desc">之后这个 ID 按所选模型计价，只对 relay.acme.dev 生效。</span>
        <div class="input-box"><input data-filter-rows="#which-model-list .which-opt" placeholder="搜索目录模型" /></div>
        <div id="which-model-list" class="which-list" role="listbox" aria-label="搜索目录模型">
          <button class="which-opt" data-pick-which="tencent/hy4"><span class="which-opt-name">Hy4</span><span class="which-opt-key">tencent/hy4</span></button>
          <button class="which-opt" data-pick-which="openai/gpt-6.1-terra"><span class="which-opt-name">GPT-6.1 Terra</span><span class="which-opt-key">openai/gpt-6.1-terra</span></button>
          <button class="which-opt" data-pick-which="moonshotai/kimi-k3.1-pro"><span class="which-opt-name">Kimi K3.1 Pro</span><span class="which-opt-key">moonshotai/kimi-k3.1-pro</span></button>
          <button class="which-opt" data-pick-which="minimax/minimax-m3.1-flash"><span class="which-opt-name">MiniMax-M3.1-Flash</span><span class="which-opt-key">minimax/minimax-m3.1-flash</span></button>
          <button class="which-opt" data-pick-which="deepseek/deepseek-v4.1-flash-1017"><span class="which-opt-name">DeepSeek V4.1 Flash 1017</span><span class="which-opt-key">deepseek/deepseek-v4.1-flash-1017</span></button>
          <button class="which-opt" data-pick-which="nvidia/nemotron-3.5-super-vl-120b-a12b"><span class="which-opt-name">Nemotron 3.5 Super VL 120B A12B</span><span class="which-opt-key">nvidia/nemotron-3.5-super-vl-120b-a12b</span></button>
          <button class="which-opt active" data-pick-which="google/gemini-4-argon"><span class="which-opt-name">Gemini 4 Argon</span><span class="which-opt-key">google/gemini-4-argon</span></button>
          <button class="which-opt" data-pick-which="anthropic/claude-haiku-5-5"><span class="which-opt-name">Claude Haiku 5.5</span><span class="which-opt-key">anthropic/claude-haiku-5-5</span></button>
        </div>
      </div>
      <div class="col-stack-sm">
        <b class="text-ink1">默认价格</b>
        <div class="seg" style="align-self:flex-start"><button data-action="seg-pick">自定义</button><button data-action="seg-pick">按次</button><button class="active" data-action="seg-pick">无价格</button></div>
        <span class="set-row-desc">用于你所有服务商，除非下面另有设置。</span>
        <div class="inline-banner warn"><span>▲ 没有价格，费用将显示为未知。</span></div>
      </div>
      <div class="col-stack-sm">
        <b class="text-ink1">你的服务商 (1)</b>
        <div class="list-row" style="background:var(--bg-surface);border-color:var(--border-subtle)">
          <span class="ws-badge" style="background:var(--bg-sunken);color:var(--ink-1);border:1px solid var(--border-subtle)">R</span>
          <div class="list-row-col">
            <span class="list-row-title">relay.acme.dev</span>
            <span class="list-row-sub mono">gemini-4-neon</span>
          </div>
          <button class="select-pill" data-toast="服务商计价：跟随默认">跟随默认 ▾</button>
        </div>
      </div>`,
    foot: `<button class="btn btn-ghost" style="margin-right:auto" data-toast="已恢复默认定价">恢复默认</button><button class="btn" data-action="close-drawer">取消</button><button class="btn btn-primary" data-drawer-done="已保存未收录模型 gemini-4-neon 的关联与定价">保存</button>`,
  },
};
