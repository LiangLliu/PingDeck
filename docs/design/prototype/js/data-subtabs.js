/* PingDeck Prototype — Sub-Tab & Lower-Pane View Templates */
window.PD = window.PD || {};

(() => {
  const { sw, num, row } = window.PD.ui;

  // ── 1. Mock Rule Branch Templates (422 / 409 / 201) ───────────────────────
  window.PD.MOCK_BRANCHES = {
    "201": {
      title: "返回内容（3 · 201 Created ★ 默认命中）",
      status: "201",
      delay: "120",
      ruleExpr: `Body JSONPath: <code>$.invoice_id</code> 存在 且 Header <code>Idempotency-Key != dup-1</code>`,
      headers: `Location: /v1/refunds/{{$guid}} · X-Mock-Branch: created-default`,
      bucketAction: `向数据桶 <code>refunds</code> 写入响应对象（主键 <code>id</code>）`,
      body: `<div>{</div>
<div>  <span class="tk-key">"id"</span>: <span class="tk-str">"{{$guid}}"</span>,</div>
<div>  <span class="tk-key">"object"</span>: <span class="tk-str">"refund"</span>,</div>
<div>  <span class="tk-key">"status"</span>: <span class="tk-str">"succeeded"</span>,</div>
<div>  <span class="tk-key">"invoice_id"</span>: <span class="tk-str">"{{req.body.invoice_id}}"</span>,</div>
<div>  <span class="tk-key">"amount"</span>: <span class="tk-num">{{req.body.amount}}</span>,</div>
<div>  <span class="tk-key">"route"</span>: <span class="tk-str">"refunds.create"</span></div>
<div>}</div>`,
    },
    "422": {
      title: "返回内容（1 · 422 Invalid 缺少 invoice_id）",
      status: "422",
      delay: "15",
      ruleExpr: `Body JSONPath: <code>$.invoice_id</code> 为空或未提供`,
      headers: `X-Mock-Branch: invalid-missing-invoice`,
      bucketAction: `不修改数据桶（校验失败直接返回）`,
      body: `<div>{</div>
<div>  <span class="tk-key">"error"</span>: {</div>
<div>    <span class="tk-key">"code"</span>: <span class="tk-str">"parameter_missing"</span>,</div>
<div>    <span class="tk-key">"param"</span>: <span class="tk-str">"invoice_id"</span>,</div>
<div>    <span class="tk-key">"message"</span>: <span class="tk-str">"invoice_id is required to create a refund."</span></div>
<div>  }</div>
<div>}</div>`,
    },
    "409": {
      title: "返回内容（2 · 409 Duplicate 幂等冲突）",
      status: "409",
      delay: "35",
      ruleExpr: `Header: <code>Idempotency-Key == "dup-1"</code>`,
      headers: `X-Mock-Branch: idempotency-conflict · Retry-After: 0`,
      bucketAction: `不修改数据桶（幂等冲突拦截）`,
      body: `<div>{</div>
<div>  <span class="tk-key">"error"</span>: {</div>
<div>    <span class="tk-key">"code"</span>: <span class="tk-str">"idempotency_key_in_use"</span>,</div>
<div>    <span class="tk-key">"message"</span>: <span class="tk-str">"Idempotency key dup-1 has already been consumed."</span></div>
<div>  }</div>
<div>}</div>`,
    },
  };

  // ── 2. AI SSE Frame Payloads (Interactive Frame Chip Inspector) ───────────
  window.PD.SSE_FRAMES = {
    "1": { meta: "帧 #1 · +233 ms · role_delta", data: `{"id":"chatcmpl-01J9","choices":[{"index":0,"delta":{"role":"assistant"}}]}` },
    "2": { meta: "帧 #2 · +241 ms · content_delta", data: `{"id":"chatcmpl-01J9","choices":[{"index":0,"delta":{"content":"p"}}]}` },
    "3": { meta: "帧 #3 · +318 ms · content_delta", data: `{"id":"chatcmpl-01J9","choices":[{"index":0,"delta":{"content":"ong"}}]}` },
    "5": { meta: "帧 #5 · +590 ms · finish_stop", data: `{"id":"chatcmpl-01J9","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}` },
    "6": { meta: "帧 #6 · +604 ms · usage_summary", data: `{"usage":{"prompt_tokens":9,"completion_tokens":2,"total_tokens":11}}` },
    "7": { meta: "帧 #7 · +612 ms · stream_end", data: `[DONE]` },
  };

  // ── 3. Flow Step Inspector Data (Steps 1..6 + Flow Inputs) ────────────────
  window.PD.FLOW_STEPS = {
    "1": { name: "Health", n: 1, method: "GET", path: "/healthz", status: "200 OK · 42 ms", extract: "—（无提取变量）", assert: "status == 200（通过）", retry: "不重试 · 失败时停止", body: `{"status":"ok","db":"wal","vault":"attached"}` },
    "2": { name: "Login", n: 2, method: "POST", path: "/auth/token", status: "200 OK · 118 ms", extract: `token ← $.access_token ("eyJhbG…9kQ")`, assert: "status == 200 && $.access_token 存在（2/2 通过）", retry: "重试 1 次 · 间隔 300 ms", body: `{"access_token":"eyJhbG...9kQ","expires_in":3600}` },
    "3": { name: "List invoices", n: 3, method: "GET", path: "/invoices?status=open", status: "运行中 · 已耗时 312 ms", extract: `invoice_id ← $.data[0].id（等待响应）`, assert: "继承原请求 2 条断言 + status == 200", retry: "重试 2 次 · 间隔 500 ms", body: `// 正在等待 billing.staging.acme.dev 响应...` },
    "4": { name: "Create invoice", n: 4, method: "POST", path: "/invoices", status: "等待前置步骤", extract: `created_id ← $.id`, assert: "status == 201 && $.amount_due == {{amount}}", retry: "不重试 · 失败时停止", body: `// 尚未执行` },
    "5": { name: "Cleanup", n: 5, method: "POST", path: "/refunds", status: "等待前置步骤", extract: `refund_id ← $.id`, assert: "status == 201", retry: "重试 2 次 · 间隔 500 ms", body: `// 尚未执行` },
    "6": { name: "Verify", n: 6, method: "GET", path: "/invoices/{{invoice_id}}", status: "等待前置步骤", extract: "—", assert: `$.status == "refunded"`, retry: "不重试", body: `// 尚未执行` },
  };

  // ── 4. Usage Breakdown Dimension Tables (6 Dimensions matching StatsView.tsx) ──
  window.PD.USAGE_DIMS = {
    model: () => `
      <table class="kv-table">
        <thead><tr><th>模型（点击下钻调用日志）</th><th>服务商</th><th class="text-right">调用次数</th><th class="text-right">失败</th><th class="text-right">平均首字</th><th class="text-right">总 token（含缓存）</th><th class="text-right">估算费用</th></tr></thead>
        <tbody>
          <tr class="clickable" data-usage-view="calls"><td class="mono text-accent">deepseek-v4-pro</td><td>DeepSeek</td><td class="mono text-right">540</td><td class="mono text-right text-muted">9</td><td class="mono text-right">412 ms</td><td class="mono text-right">2.31M (38%)</td><td class="mono text-right">¥18.60</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="mono text-accent">claude-sonnet-5-5</td><td>Anthropic</td><td class="mono text-right">218</td><td class="mono text-right text-warn">18</td><td class="mono text-right">690 ms</td><td class="mono text-right">1.14M (29%)</td><td class="mono text-right">¥19.40</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="mono text-accent">deepseek-v4-flash</td><td>DeepSeek</td><td class="mono text-right">412</td><td class="mono text-right text-muted">2</td><td class="mono text-right">233 ms</td><td class="mono text-right">1.19M (35%)</td><td class="mono text-right">¥3.62</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="mono text-accent">kimi-k2.6</td><td>Moonshot</td><td class="mono text-right">114</td><td class="mono text-right text-warn">8</td><td class="mono text-right">520 ms</td><td class="mono text-right">0.18M (18%)</td><td class="mono text-right">¥0.56</td></tr>
        </tbody>
        <tfoot><tr><td>合计（4 个模型）</td><td>3 个服务商</td><td class="mono text-right">1,284</td><td class="mono text-right">37 (2.9%)</td><td class="mono text-right">418 ms</td><td class="mono text-right">4.82M (34.2%)</td><td class="mono text-right">¥42.18</td></tr></tfoot>
      </table>`,
    provider: () => `
      <table class="kv-table">
        <thead><tr><th>服务商</th><th class="text-right">调用次数</th><th class="text-right">失败率</th><th class="text-right">中位首字 / P95</th><th class="text-right">总 Token</th><th class="text-right">估算费用</th></tr></thead>
        <tbody>
          <tr class="clickable" data-usage-view="calls"><td class="text-accent fw-600">DeepSeek</td><td class="mono text-right">952</td><td class="mono text-right text-ok">1.2%</td><td class="mono text-right">340 ms / 890 ms</td><td class="mono text-right">3.50M</td><td class="mono text-right">¥22.22</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="text-accent fw-600">Anthropic</td><td class="mono text-right">218</td><td class="mono text-right text-warn">8.3%</td><td class="mono text-right">690 ms / 1.84 s</td><td class="mono text-right">1.14M</td><td class="mono text-right">¥19.40</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="text-accent fw-600">Moonshot</td><td class="mono text-right">114</td><td class="mono text-right text-warn">7.0%</td><td class="mono text-right">520 ms / 1.32 s</td><td class="mono text-right">0.18M</td><td class="mono text-right">¥0.56</td></tr>
        </tbody>
      </table>`,
    source: () => `
      <table class="kv-table">
        <thead><tr><th>调用来源</th><th class="text-right">调用次数</th><th class="text-right">占比</th><th class="text-right">总 Token</th><th class="text-right">估算费用</th></tr></thead>
        <tbody>
          <tr class="clickable" data-usage-view="calls"><td class="text-accent fw-600">智能体对话（含子智能体）</td><td class="mono text-right">689</td><td class="mono text-right">53.7%</td><td class="mono text-right">2.84M</td><td class="mono text-right">¥24.90</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="text-accent fw-600">本地网关转发 (:4141)</td><td class="mono text-right">412</td><td class="mono text-right">32.1%</td><td class="mono text-right">1.76M</td><td class="mono text-right">¥15.82</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="text-accent fw-600">AI 请求工作台与探针</td><td class="mono text-right">183</td><td class="mono text-right">14.2%</td><td class="mono text-right">0.22M</td><td class="mono text-right">¥1.46</td></tr>
        </tbody>
      </table>`,
    client: () => `
      <table class="kv-table">
        <thead><tr><th>网关客户端</th><th>令牌前缀</th><th class="text-right">转发次数</th><th class="text-right">故障转移成功</th><th class="text-right">估算费用</th></tr></thead>
        <tbody>
          <tr class="clickable" data-usage-view="calls"><td><b>Claude Code</b></td><td class="mono">pdk_c_••••8a1f</td><td class="mono text-right">214</td><td class="mono text-right text-ok">9 次自动切换</td><td class="mono text-right">¥9.84</td></tr>
          <tr class="clickable" data-usage-view="calls"><td><b>Codex CLI</b></td><td class="mono">pdk_c_••••4c02</td><td class="mono text-right">112</td><td class="mono text-right text-ok">3 次自动切换</td><td class="mono text-right">¥4.72</td></tr>
          <tr class="clickable" data-usage-view="calls"><td><b>Zed Editor</b></td><td class="mono">pdk_c_••••9e7b</td><td class="mono text-right">86</td><td class="mono text-right text-muted">0</td><td class="mono text-right">¥1.26</td></tr>
        </tbody>
      </table>`,
    key: () => `
      <table class="kv-table">
        <thead><tr><th>密钥名称</th><th>所属服务商</th><th class="text-right">调用次数</th><th class="text-right">429 限流次数</th><th class="text-right">估算费用</th></tr></thead>
        <tbody>
          <tr class="clickable" data-usage-view="calls"><td class="mono">key_ds_prod</td><td>DeepSeek</td><td class="mono text-right">952</td><td class="mono text-right">11</td><td class="mono text-right">¥22.22</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="mono">key_anth_team</td><td>Anthropic</td><td class="mono text-right">218</td><td class="mono text-right text-warn">17</td><td class="mono text-right">¥19.40</td></tr>
          <tr class="clickable" data-usage-view="calls"><td class="mono">key_moon_main</td><td>Moonshot</td><td class="mono text-right">114</td><td class="mono text-right">0</td><td class="mono text-right">¥0.56</td></tr>
        </tbody>
      </table>`,
    failure: () => `
      <table class="kv-table">
        <thead><tr><th>失败类别 / 状态码</th><th>主要受影响模型</th><th>网关处理结果</th><th class="text-right">次数</th><th style="width:110px">排查</th></tr></thead>
        <tbody>
          <tr><td><span class="tag tag-warn">429 Rate Limit</span></td><td class="mono">claude-sonnet-5-5, deepseek-v4-pro</td><td>12 次经 Retry-After 或备用链成功恢复</td><td class="mono text-right">28</td><td><button class="btn btn-sm" data-usage-view="calls">查看日志 →</button></td></tr>
          <tr><td><span class="tag tag-bad">504 Upstream Timeout</span></td><td class="mono">glm-5, kimi-k2.6</td><td>首包前超时，自动回退至备用模型</td><td class="mono text-right">6</td><td><button class="btn btn-sm" data-usage-view="calls">查看日志 →</button></td></tr>
          <tr><td><span class="tag tag-bad">403 Permission Denied</span></td><td class="mono">gemini-3.2-pro</td><td>上游令牌无该模型权限，已标记需关注</td><td class="mono text-right">3</td><td><button class="btn btn-sm" data-usage-view="calls">查看日志 →</button></td></tr>
        </tbody>
      </table>`,
  };

  // ── 5. Environment Extra Views ────────────────────────────────────────────
  window.PD.ENV_EXTRA_VIEWS = {
    resolved: () => `
      <div class="card-panel">
        <div class="card-panel-head"><span>变量最终解析顺序（覆盖优先级从高到低）</span><span class="tag">实时求值</span></div>
        <table class="kv-table">
          <thead><tr><th>变量名</th><th>生效值</th><th>生效来源</th><th>被覆盖的更低层</th></tr></thead>
          <tbody>
            <tr><td class="mono">base_url</td><td class="mono">https://billing.staging.acme.dev</td><td><span class="tag tag-ok">环境：Staging</span></td><td class="text-faint">全局变量 (http://localhost:8080)</td></tr>
            <tr><td class="mono">api_version</td><td class="mono">2026-10-01</td><td><span class="tag tag-ok">环境：Staging</span></td><td class="text-faint">—</td></tr>
            <tr><td class="mono">token</td><td class="mono">••••••••••••</td><td><span class="tag tag-warn">保险库 (0600)</span></td><td class="text-faint">—</td></tr>
            <tr><td class="mono">trace_header</td><td class="mono">pd-local-dev</td><td><span class="tag">工作区全局变量</span></td><td class="text-faint">—</td></tr>
          </tbody>
        </table>
      </div>`,
    protect: () => `
      <div class="col-stack" style="max-width:680px">
        <div class="card-panel" style="border-color:var(--warn-border)">
          <div class="card-panel-head"><span class="text-warn">▲ 生产环境发送前二次确认</span>${sw(true)}</div>
          <p class="set-row-desc">当选中受保护环境发送请求（含智能体 <code>send_request</code> 工具与流程步骤）时，命中下列方法将强制弹出确认框，防止误删或误改线上数据。</p>
          <div class="row-wrap">
            <span class="tag">GET（直发）</span>
            <span class="tag tag-warn">✓ POST 需确认</span>
            <span class="tag tag-warn">✓ PUT 需确认</span>
            <span class="tag tag-warn">✓ PATCH 需确认</span>
            <span class="tag tag-bad">✓ DELETE 需确认</span>
          </div>
        </div>
      </div>`,
    globals: () => `
      <table class="kv-table">
        <thead><tr><th style="width:36px"></th><th style="width:26%">全局变量名</th><th style="width:16%">类型</th><th>当前值（全部环境共享，可被环境覆盖）</th></tr></thead>
        <tbody>
          <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">base_url</td><td><span class="tag">文本</span></td><td class="mono">http://127.0.0.1:8080</td></tr>
          <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">trace_header</td><td><span class="tag">文本</span></td><td class="mono">pd-local-dev</td></tr>
          <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">default_currency</td><td><span class="tag">文本</span></td><td class="mono">usd</td></tr>
        </tbody>
      </table>`,
    cookies: () => `
      <div class="col-stack">
        <div class="row-between"><span class="set-row-desc">工作区 Cookie 罐（6 条有效；Flow 独立会话不写回此处）</span><button class="btn btn-sm btn-danger" data-toast="已清空当前工作区 Cookie 罐" data-toast-tone="warn">清空全部 Cookie</button></div>
        <table class="kv-table">
          <thead><tr><th>域名</th><th>名称</th><th>路径</th><th>标记</th><th>过期时间</th></tr></thead>
          <tbody>
            <tr><td class="mono">billing.staging.acme.dev</td><td class="mono">__pd_edge</td><td class="mono">/</td><td><span class="tag">HttpOnly</span> <span class="tag">Secure</span></td><td class="text-muted">会话结束</td></tr>
            <tr><td class="mono">billing.staging.acme.dev</td><td class="mono">csrf_nonce</td><td class="mono">/v1</td><td><span class="tag">SameSite=Lax</span></td><td class="text-muted">24 小时后</td></tr>
          </tbody>
        </table>
      </div>`,
    certs: () => `
      <div class="col-stack">
        <div class="row-between"><span class="set-row-desc">工作区级 mTLS 客户端证书（私钥存放在本地 0600 保险库）</span><button class="btn btn-sm btn-primary" data-toast="已打开客户端证书添加表单（支持 PFX / PEM）">+ 添加客户端证书</button></div>
        <table class="kv-table">
          <thead><tr><th>目标主机</th><th>证书格式</th><th>有效期至</th><th>状态</th></tr></thead>
          <tbody>
            <tr><td class="mono">*.staging.acme.dev:443</td><td><span class="tag">PKCS#12 (.p12)</span></td><td class="mono">2027-06-30</td><td><span class="tag tag-ok">● 有效</span></td></tr>
          </tbody>
        </table>
      </div>`,
  };

  // ── 6. Sub-Tab Content Registries ─────────────────────────────────────────
  window.PD.buildSubtabs = (DEFAULT_TAB_HTML) => ({
    "req-ed": {
      params: () => DEFAULT_TAB_HTML["req-ed"],
      headers: () => `
        <table class="kv-table">
          <thead><tr><th style="width:36px"></th><th style="width:28%">请求头</th><th style="width:38%">值</th><th>来源</th></tr></thead>
          <tbody>
            <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">Accept</td><td class="mono">application/json</td><td><span class="tag">显式</span></td></tr>
            <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">Authorization</td><td class="mono">Bearer <span class="url-var">{{token}}</span></td><td><span class="tag tag-accent">集合认证</span></td></tr>
            <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">X-API-Version</td><td class="mono"><span class="url-var">{{api_version}}</span></td><td><span class="tag">显式</span></td></tr>
            <tr class="kv-off"><td class="text-center"><span class="check-box"></span></td><td class="mono">User-Agent</td><td class="mono">PingDeck/0.1.0</td><td><span class="tag">自动</span></td></tr>
          </tbody>
        </table>`,
      auth: () => `
        <div class="col-stack">
          <div class="row-center" style="flex-wrap:wrap;gap:6px">
            <span class="field-label">认证方式</span>
            <div class="seg"><button class="active" data-action="seg-pick">继承集合</button><button data-action="seg-pick">无</button><button data-action="seg-pick">Bearer</button><button data-action="seg-pick">Basic</button><button data-action="seg-pick">API Key</button><button data-action="seg-pick">OAuth 2.0</button><button data-action="seg-pick">Digest</button><button data-action="seg-pick">AWS SigV4</button></div>
          </div>
          <div class="card-panel">
            <div class="card-panel-head"><span>继承自集合「Billing API」</span><span class="tag tag-ok">● Bearer Token</span></div>
            <div class="mono fs-xs text-ink2">Authorization: Bearer <span class="url-var">{{token}}</span>（来自当前环境 Staging 的本地 0600 保险库）</div>
          </div>
        </div>`,
      body: () => `
        <div class="col-stack">
          <div class="row-between">
            <div class="seg"><button class="active" data-action="seg-pick">无正文</button><button data-action="seg-pick">JSON</button><button data-action="seg-pick">Raw</button><button data-action="seg-pick">Form</button><button data-action="seg-pick">Multipart</button><button data-action="seg-pick">GraphQL</button><button data-action="seg-pick">Binary</button></div>
            <button class="btn btn-sm btn-ghost" data-toast="已格式化 JSON 请求体">格式化</button>
          </div>
          <p class="set-row-desc">GET / HEAD 仅在正文非空时发送；支持 JSON、多语言 Raw、URL 编码表单、Multipart（含文件）、GraphQL（Query + Variables）与二进制文件校验。</p>
        </div>`,
      assert: () => `
        <div class="col-stack-sm">
          <table class="kv-table">
            <thead><tr><th style="width:36px"></th><th>目标（状态 / JSONPath / 响应头 / 正文 / 耗时 / Schema）</th><th>运算符</th><th>期望值</th><th style="width:84px">上次结果</th></tr></thead>
            <tbody>
              <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">status</td><td><span class="tag">==</span></td><td class="mono">200</td><td><span class="tag tag-ok">通过</span></td></tr>
              <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">$.data[0].currency</td><td><span class="tag">==</span></td><td class="mono">"usd"</td><td><span class="tag tag-ok">通过</span></td></tr>
              <tr><td class="text-center"><span class="check-box checked">✓</span></td><td class="mono">$.data (JSON Schema)</td><td><span class="tag">schema</span></td><td class="mono">{"type":"array"}</td><td><span class="tag tag-ok">通过</span></td></tr>
            </tbody>
          </table>
          <div><button class="btn btn-sm" data-toast="已新增一条断言规则">+ 添加断言</button></div>
        </div>`,
      scripts: () => `
        <div class="col-stack-sm">
          <div class="row-between">
            <div class="seg"><button class="active" data-action="seg-pick">响应后脚本 (Post)</button><button data-action="seg-pick">请求前脚本 (Pre)</button></div>
            <span class="work-meta">Boa 沙箱 · 兼容扫描通过（0 项不支持 API）</span>
          </div>
          <div class="code-box" style="white-space:pre">pm.test("returns open invoices", () => {
  const body = pm.response.json();
  pm.expect(body.object).to.eql("list");
  if (body.next_cursor) pm.environment.set("cursor", body.next_cursor);
});</div>
        </div>`,
      net: () => `
        <div class="grid-2col fs-xs">
          <div class="card-panel">
            <div class="card-panel-head"><b>7 项可继承网络选项</b><span class="tag">跟随设置 / 本请求</span></div>
            <div class="set-row" style="padding:5px 0"><span>空闲超时 <span class="tag">跟随 30s</span></span>${num("30", "秒")}</div>
            <div class="set-row" style="padding:5px 0"><span>传输重试 <span class="tag">跟随 0次</span></span>${num("0", "次")}</div>
            <div class="set-row" style="padding:5px 0"><span>HTTP 版本 <span class="tag">跟随自动</span></span><span class="tag">Auto</span></div>
            <div class="set-row" style="padding:5px 0;border:0"><span>跟随重定向 / 最大跳数 / TLS 校验 / 代理</span><span class="tag tag-ok">全部跟随</span></div>
          </div>
          <div class="card-panel">
            <div class="card-panel-head"><b>本请求附加项与全局来源</b><button class="btn btn-sm" data-nav-to="settings" data-set-page="network">网络设置 ↗</button></div>
            <div class="set-row" style="padding:5px 0"><span>总时限（含重定向与正文）</span>${num("", "不限")}</div>
            <div class="set-row" style="padding:5px 0"><span>携带工作区 Cookie 罐</span>${sw(true)}</div>
            <div class="set-row" style="padding:5px 0;border:0"><span>自动 URL 编码</span>${sw(true)}</div>
          </div>
        </div>`,
      docs: () => `
        <div class="col-stack-sm">
          <div class="row-between"><b>List invoices（支持 Markdown 与响应示例）</b><button class="btn btn-sm" data-toast="已将当前响应保存为示例">存当前响应为示例</button></div>
          <p class="set-row-desc">按状态与游标分页查询计费账单列表，支持 <code>status</code>（open / paid / void）与 <code>limit</code>（1–100）。</p>
          <div class="card-panel"><div class="card-panel-head"><span>已保存示例 · 200 OK — Open invoices</span><span class="tag">Postman / OpenAPI 导入</span></div><div class="mono fs-2xs text-muted">{"object":"list","has_more":true,"total_count":42}</div></div>
        </div>`,
    },
    "req-resp": {
      body: () => DEFAULT_TAB_HTML["req-resp"],
      headers: () => `
        <div class="kv-wrap"><table class="kv-table">
          <thead><tr><th style="width:34%">响应头</th><th>值</th></tr></thead>
          <tbody>
            <tr><td class="mono">content-type</td><td class="mono">application/json; charset=utf-8</td></tr>
            <tr><td class="mono">x-request-id</td><td class="mono">req_01J9X3K2P8</td></tr>
            <tr><td class="mono">x-ratelimit-remaining</td><td class="mono">98 / 100</td></tr>
            <tr><td class="mono">cache-control</td><td class="mono">no-store</td></tr>
            <tr><td class="mono">strict-transport-security</td><td class="mono">max-age=31536000; includeSubDomains</td></tr>
          </tbody>
        </table></div>`,
      cookies: () => `
        <div class="kv-wrap col-stack-sm" style="padding:10px 14px">
          <div class="row-between fs-xs"><span class="tag tag-ok">● 已存入工作区 Cookie 罐</span><span class="text-muted">0 条 Set-Cookie 被拒绝</span></div>
          <table class="kv-table">
            <thead><tr><th>名称</th><th>值</th><th>域 / 路径</th><th>属性</th></tr></thead>
            <tbody>
              <tr><td class="mono">__pd_edge</td><td class="mono">stg-hkg-02</td><td class="mono">billing.staging.acme.dev /</td><td><span class="tag">HttpOnly</span> <span class="tag">Secure</span></td></tr>
            </tbody>
          </table>
        </div>`,
      trace: () => `
        <div class="pad-pane col-stack fs-xs">
          <div class="card-panel">
            <div class="card-panel-head"><span>连接与计时瀑布（HTTP/2 · TLS 1.3 · 代理：系统代理）</span><span class="mono">总计 184 ms</span></div>
            <div class="wire-pop-row"><span class="wire-pop-swatch" style="background:var(--ph-dns)"></span><span style="width:76px">DNS 解析</span><div class="wire-pop-bar"><i style="width:8%;background:var(--ph-dns)"></i></div><span class="mono">8 ms</span></div>
            <div class="wire-pop-row"><span class="wire-pop-swatch" style="background:var(--ph-connect)"></span><span style="width:76px">TCP 建连</span><div class="wire-pop-bar"><i style="width:14%;background:var(--ph-connect)"></i></div><span class="mono">24 ms</span></div>
            <div class="wire-pop-row"><span class="wire-pop-swatch" style="background:var(--ph-tls)"></span><span style="width:76px">TLS 握手</span><div class="wire-pop-bar"><i style="width:18%;background:var(--ph-tls)"></i></div><span class="mono">31 ms</span></div>
            <div class="wire-pop-row"><span class="wire-pop-swatch" style="background:var(--ph-wait)"></span><span style="width:76px">等待首包</span><div class="wire-pop-bar"><i style="width:58%;background:var(--ph-wait)"></i></div><span class="mono">109 ms</span></div>
            <div class="wire-pop-row"><span class="wire-pop-swatch" style="background:var(--ph-recv)"></span><span style="width:76px">接收正文</span><div class="wire-pop-bar"><i style="width:12%;background:var(--ph-recv)"></i></div><span class="mono">12 ms</span></div>
          </div>
          <div class="card-panel">
            <div class="card-panel-head"><span>调用链与实发请求（凭据已脱敏）</span><span class="tag">认证：集合 Bearer</span></div>
            <div class="mono fs-2xs text-ink2">GET https://billing.staging.acme.dev/invoices/inv_01J9X3K2P8?status=open&amp;limit=25</div>
            <div class="fs-2xs text-muted">变量解析：base_url (Staging), token (保险库 · 已遮蔽), api_version (Staging)</div>
          </div>
        </div>`,
      tests: () => `
        <div class="pad-pane col-stack-sm">
          <div class="list-row" style="background:var(--ok-soft);border-color:var(--ok-border)"><span class="tag tag-ok">通过</span><b class="mono">status == 200</b><span class="ml-auto text-muted fs-2xs">声明式断言</span></div>
          <div class="list-row" style="background:var(--ok-soft);border-color:var(--ok-border)"><span class="tag tag-ok">通过</span><b class="mono">returns open invoices</b><span class="ml-auto text-muted fs-2xs">响应后脚本 · 1.2 ms</span></div>
        </div>`,
    },
    "ai-ed": {
      messages: () => DEFAULT_TAB_HTML["ai-ed"],
      params: () => `
        <div class="field-grid">
          <div class="field-col"><span class="field-label">Temperature</span><div class="input-box"><input value="0.2" /></div></div>
          <div class="field-col"><span class="field-label">Max Tokens</span><div class="input-box"><input value="256" /></div></div>
          <div class="field-col"><span class="field-label">Top P</span><div class="input-box"><input value="1.0" /></div></div>
          <div class="field-col"><span class="field-label">流式输出 (stream)</span><div>${sw(true)}</div></div>
        </div>`,
      tools: () => `<div class="card-panel"><div class="card-panel-head"><span>Function Calling 工具定义（0 个）</span><button class="btn btn-sm" data-toast="已插入示例 JSON Schema 工具定义">+ 添加函数工具</button></div><p class="set-row-desc">在此定义 OpenAI / Anthropic 工具 Schema，发送时自动按协议编码。</p></div>`,
      raw: () => `<div class="code-box" style="white-space:pre">{\n  "model": "deepseek-v4-flash",\n  "stream": true,\n  "messages": [\n    { "role": "system", "content": "You are a terse assistant." },\n    { "role": "user", "content": "Reply with the single word pong." }\n  ]\n}</div>`,
      assert: () => `<div class="list-row" style="background:var(--ok-soft);border-color:var(--ok-border)"><span class="tag tag-ok">通过</span><b class="mono">reply.trim().toLowerCase() == "pong"</b></div>`,
      scripts: () => `<div class="code-box" style="white-space:pre">// 校验首字延迟不超过 1000 ms\npm.expect(pm.ai.ttftMs).to.be.below(1000);</div>`,
    },
    "ai-resp": {
      merged: () => DEFAULT_TAB_HTML["ai-resp"],
      events: () => `
        <div class="kv-wrap"><table class="kv-table">
          <thead><tr><th>#</th><th>时间</th><th>事件类型</th><th>载荷片段</th></tr></thead>
          <tbody>
            <tr><td class="mono">1</td><td class="mono">+233ms</td><td><span class="tag">delta.role</span></td><td class="mono">{"role":"assistant"}</td></tr>
            <tr><td class="mono">2</td><td class="mono">+241ms</td><td><span class="tag tag-accent">delta.content</span></td><td class="mono">{"content":"p"}</td></tr>
            <tr><td class="mono">3</td><td class="mono">+318ms</td><td><span class="tag tag-accent">delta.content</span></td><td class="mono">{"content":"ong"}</td></tr>
            <tr><td class="mono">5</td><td class="mono">+590ms</td><td><span class="tag tag-ok">finish</span></td><td class="mono">{"finish_reason":"stop"}</td></tr>
            <tr><td class="mono">6</td><td class="mono">+604ms</td><td><span class="tag">usage</span></td><td class="mono">{"prompt_tokens":9,"completion_tokens":2}</td></tr>
            <tr><td class="mono">7</td><td class="mono">+612ms</td><td><span class="tag">done</span></td><td class="mono">[DONE]</td></tr>
          </tbody>
        </table></div>`,
      raw: () => `<div class="code-view" style="white-space:pre">data: {"id":"chatcmpl-01J9","choices":[{"delta":{"role":"assistant"}}]}\ndata: {"id":"chatcmpl-01J9","choices":[{"delta":{"content":"p"}}]}\ndata: {"id":"chatcmpl-01J9","choices":[{"delta":{"content":"ong"}}]}\ndata: {"id":"chatcmpl-01J9","choices":[{"finish_reason":"stop"}]}\ndata: [DONE]</div>`,
      headers: () => `<div class="kv-wrap"><table class="kv-table"><thead><tr><th>响应头</th><th>值</th></tr></thead><tbody><tr><td class="mono">content-type</td><td class="mono">text/event-stream; charset=utf-8</td></tr><tr><td class="mono">x-ds-trace-id</td><td class="mono">ds_01J9X9A2M7</td></tr></tbody></table></div>`,
      timing: () => `<div class="pad-pane"><div class="card-panel"><div class="card-panel-head"><span>流式阶段耗时</span><span class="mono">612 ms</span></div><div class="row-center fs-xs" style="gap:18px"><span>建连：<b>48 ms</b></span><span>首字 (TTFT)：<b>233 ms</b></span><span>生成吞吐：<b>5.3 tok/s</b></span></div></div></div>`,
      tests: () => `<div class="pad-pane"><div class="list-row" style="background:var(--ok-soft);border-color:var(--ok-border)"><span class="tag tag-ok">通过</span><b class="mono">reply == "pong" · ttft &lt; 1000ms</b></div></div>`,
      usage: () => `<div class="pad-pane"><div class="card-panel"><div class="card-panel-head"><span>Token 与目录估算费用</span><span class="mono fw-600">¥0.00002</span></div><div class="fs-xs text-muted">输入 9 tok (¥0.50/1M) + 输出 2 tok (¥2.00/1M)</div></div></div>`,
    },
    "mock-main": {
      routes: () => DEFAULT_TAB_HTML["mock-main"],
      data: () => `
        <div class="card-panel" style="grid-column:1 / -1">
          <div class="card-panel-head"><span>内存数据桶（2 个集合 · 支持 CRUD 路由持久化）</span><button class="btn btn-sm btn-primary" data-toast="已新增空数据桶">+ 新建数据桶</button></div>
          <table class="kv-table">
            <thead><tr><th>桶名称</th><th>记录数</th><th>最近写入</th><th style="width:90px">操作</th></tr></thead>
            <tbody>
              <tr><td class="mono">invoices</td><td class="mono">14</td><td class="text-muted">3 分钟前</td><td><button class="btn btn-sm" data-toast="已重置数据桶 invoices 为初始种子">重置种子</button></td></tr>
              <tr><td class="mono">refunds</td><td class="mono">3</td><td class="text-muted">1 分钟前</td><td><button class="btn btn-sm" data-toast="已重置数据桶 refunds 为初始种子">重置种子</button></td></tr>
            </tbody>
          </table>
        </div>`,
      headers: () => `
        <div class="card-panel" style="grid-column:1 / -1">
          <div class="card-panel-head"><span>全局默认响应头（附加到全部路由）</span><span class="tag tag-ok">CORS 自动开启</span></div>
          <table class="kv-table">
            <thead><tr><th>响应头</th><th>值</th></tr></thead>
            <tbody>
              <tr><td class="mono">X-Mock-Server</td><td class="mono">PingDeck Billing stub</td></tr>
              <tr><td class="mono">Access-Control-Allow-Origin</td><td class="mono">*</td></tr>
            </tbody>
          </table>
        </div>`,
      callbacks: () => `
        <div class="card-panel" style="grid-column:1 / -1">
          <div class="card-panel-head"><span>异步 Webhook 回调规则（1 条）</span><button class="btn btn-sm" data-toast="已手动触发一次测试回调">立即测试</button></div>
          <div class="list-row" style="background:var(--bg-surface);border-color:var(--border-subtle)">
            <span class="m-post">POST</span>
            <div class="list-row-col"><span class="list-row-title mono">http://127.0.0.1:4011/hooks/refund-created</span><span class="list-row-sub">触发条件：POST /refunds 返回 201 · 延迟 500 ms · 上次投递 200 OK</span></div>
            <span class="tag tag-ok">启用</span>
          </div>
        </div>`,
      traffic: () => `
        <div class="card-panel" style="grid-column:1 / -1">
          <div class="card-panel-head"><span>完整流量日志（环形保留最近 500 条 · 点击任意行查看请求与回调载荷）</span><button class="btn btn-sm" data-toast="已清空当前 Mock 流量日志">清空日志</button></div>
          <table class="kv-table">
            <thead><tr><th>时间</th><th>方法</th><th>路径</th><th>命中分支</th><th>响应</th><th>回调</th><th class="text-right">耗时</th></tr></thead>
            <tbody>
              <tr class="clickable" data-drawer="mock-hit-detail"><td class="mono">14:32:11</td><td class="m-post">POST</td><td class="mono">/v1/refunds</td><td>3 · 201 Created</td><td><span class="tag tag-ok">201</span></td><td><span class="tag tag-ok">200</span></td><td class="mono text-right">128 ms</td></tr>
              <tr class="clickable" data-drawer="mock-hit-detail"><td class="mono">14:32:04</td><td class="m-get">GET</td><td class="mono">/v1/invoices?status=open</td><td>默认列表路由</td><td><span class="tag tag-ok">200</span></td><td class="text-faint">—</td><td class="mono text-right">3 ms</td></tr>
              <tr class="clickable" data-drawer="mock-hit-detail"><td class="mono">14:31:58</td><td class="m-post">POST</td><td class="mono">/v1/refunds</td><td>1 · 422 Invalid</td><td><span class="tag tag-warn">422</span></td><td class="text-faint">—</td><td class="mono text-right">121 ms</td></tr>
              <tr class="clickable" data-drawer="mock-hit-detail"><td class="mono">14:30:19</td><td class="m-get">GET</td><td class="mono">/v1/customers</td><td>默认列表路由</td><td><span class="tag tag-ok">200</span></td><td class="text-faint">—</td><td class="mono text-right">5 ms</td></tr>
            </tbody>
          </table>
          <div class="pg-bar compact" data-pg-bar="mock-traffic" data-pg-total="128" data-pg-size="50" data-pg-page="1">
            <span class="pg-summary">显示第 1–50 条，共 128 条</span>
            <div class="pg-controls">
              <div class="pg-sizes"><button class="active" data-pg-size="50">50/页</button><button data-pg-size="100">100/页</button><button data-pg-size="200">200/页</button></div>
              <div class="pg-nav"><button disabled data-pg-btn="first">«</button><button disabled data-pg-btn="prev">‹</button><span class="pg-page-num">1 / 3</span><button data-pg-btn="next">›</button><button data-pg-btn="last">»</button></div>
            </div>
          </div>
        </div>`,
      proxy: () => `
        <div class="card-panel" style="grid-column:1 / -1">
          <div class="card-panel-head"><span>未命中路由时透明代理回源</span>${sw(false)}</div>
          <div class="field-col"><span class="field-label">上游回源地址（仅未匹配任何本地路由时转发）</span><div class="input-box"><input value="https://billing.staging.acme.dev" /></div></div>
          <p class="set-row-desc">开启后，本地只 Mock 正在开发的接口，其余请求无缝透传至测试环境，并支持一键录制成新路由。</p>
        </div>`,
      settings: () => `
        <div class="card-panel" style="grid-column:1 / -1">
          <div class="card-panel-head"><span>服务监听配置</span><span class="tag">仅绑定 127.0.0.1</span></div>
          <div class="field-grid">
            <div class="field-col"><span class="field-label">端口</span><div class="input-box"><input value="4010" /></div></div>
            <div class="field-col"><span class="field-label">基础路径前缀</span><div class="input-box"><input value="/v1" /></div></div>
          </div>
          <div class="row-center" style="justify-content:flex-end"><button class="btn btn-sm btn-primary" data-toast="已保存 Mock 服务设置（端口变更将在下次重启生效）">保存设置</button></div>
        </div>`,
    },
    "hist-resp": {
      body: () => DEFAULT_TAB_HTML["hist-resp"],
      headers: () => `<div class="kv-wrap"><table class="kv-table"><thead><tr><th>响应头</th><th>值</th></tr></thead><tbody><tr><td class="mono">status</td><td class="mono">202 Accepted</td></tr><tr><td class="mono">retry-after</td><td class="mono">5</td></tr><tr><td class="mono">x-request-id</td><td class="mono">req_01J9X7Q2W1</td></tr><tr><td class="mono">content-type</td><td class="mono">application/json; charset=utf-8</td></tr></tbody></table></div>`,
      req: () => `<div class="code-view" style="white-space:pre">POST https://billing.staging.acme.dev/refunds HTTP/2\nAuthorization: Bearer •••••••••••• (已脱敏)\nContent-Type: application/json\nIdempotency-Key: stg-9f2a1c\n\n{\n  "invoice_id": "inv_01J9X3K2P8",\n  "amount": 12400,\n  "reason": "duplicate"\n}</div>`,
      def: () => `<div class="code-view" style="white-space:pre">POST {{base_url}}/refunds\n认证：继承集合 Bearer {{token}}\n断言：status == 200（发送时快照，原集合请求后续改动不影响此历史记录）</div>`,
      trace: () => `<div class="pad-pane"><div class="card-panel"><div class="card-panel-head"><span>总耗时 512 ms（HTTP/2 · 复用连接）</span><span class="tag tag-warn">异步受理</span></div><div class="fs-xs text-muted">DNS 6 ms → TCP/TLS 44 ms → 等待首包 451 ms → 接收正文 11 ms</div></div></div>`,
      tests: () => `<div class="pad-pane col-stack-sm"><div class="list-row" style="background:var(--bad-soft);border-color:var(--bad-border)"><span class="tag tag-bad">未通过</span><b class="mono">status == 200</b><span class="ml-auto fs-2xs text-bad">实际收到 202 Accepted</span></div><div class="list-row" style="background:var(--ok-soft);border-color:var(--ok-border)"><span class="tag tag-ok">通过</span><b class="mono">$.refund_id 存在</b><span class="ml-auto fs-2xs text-muted">"re_01J9X7Q2W1"</span></div></div>`,
    },
    "chat-det": {
      changes: () => DEFAULT_TAB_HTML["chat-det"],
      progress: () => `
        <div class="card-panel">
          <div class="card-panel-head"><span>任务清单</span><span class="tag">3 / 5</span></div>
          <div class="col-stack-sm fs-xs">
            <div><span class="text-ok">✓</span> 搜索 DeepSeek 429 Retry-After 规范</div>
            <div><span class="text-ok">✓</span> 修改 <code>router.rs</code> 拆分 429 与 5xx 熔断</div>
            <div><span class="text-ok">✓</span> 运行 <code>cargo test -p pingdeck-core router</code></div>
            <div><span class="text-warn">●</span> 等待批准运行 <code>cargo clippy</code></div>
            <div class="text-faint">○ 输出根因摘要与验证结论</div>
          </div>
        </div>`,
      context: () => `
        <div class="card-panel">
          <div class="card-panel-head"><span>上下文窗口占用</span><span class="mono">38%</span></div>
          <div class="prog-track" style="height:6px;max-width:none"><div class="prog-fill" style="width:38%"></div></div>
          <div class="fs-2xs text-muted">48,640 / 128,000 Token（自动压缩阈值 80%）</div>
          <div><button class="btn btn-sm" data-toast="已手动压缩早期上下文（释放 19% 窗口）">立即压缩上下文</button></div>
        </div>`,
      opts: () => `
        <div class="card-panel">
          <div class="card-panel-head"><span>本对话运行选项</span></div>
          <div class="row-between"><span>联网搜索</span>${sw(true)}</div>
          <div class="row-between"><span>OS 沙箱隔离</span><span class="tag tag-ok">强制开启</span></div>
          <button class="btn btn-sm" data-nav-to="settings" data-set-page="ai/permissions">查看 6 层权限顺序 ↗</button>
        </div>`,
    },
    "gw-main": {
      overview: () => DEFAULT_TAB_HTML["gw-main"],
      clients: () => `
        <div class="gw-body">
          <div class="card-panel">
            <div class="card-panel-head"><span>客户端令牌与已接入工具（3 个有效）</span><div class="row-center"><button class="btn btn-sm" data-modal="connect-client">接入编码工具…</button><button class="btn btn-sm" data-toast="在卡片内展开创建令牌表单">+ 创建令牌</button></div></div>
            <table class="kv-table">
              <thead><tr><th>客户端</th><th>配置文件 / 类型</th><th>令牌</th><th>允许模型范围</th><th style="width:220px">操作</th></tr></thead>
              <tbody>
                <tr><td><span class="dot dot-ok"></span> <b>Claude Code</b> <span class="tag tag-ok">已验证</span></td><td class="mono">~/.claude/settings.json</td><td class="mono">pdk_c_••••9d2f</td><td><span class="tag mono">DeepSeek/v4-pro</span></td><td><button class="btn btn-sm btn-ghost" data-toast="连通性测试通过 · 388 ms">测试</button> <button class="btn btn-sm" data-toast="已为 Claude Code 轮换新令牌并写回配置">换令牌</button> <button class="btn btn-sm btn-ghost" data-toast="已从时间戳备份还原 ~/.claude/settings.json">还原</button></td></tr>
                <tr><td><span class="dot dot-ok"></span> <b>Codex CLI</b> <span class="tag">已配置</span></td><td class="mono">~/.codex/config.toml</td><td class="mono">pdk_c_••••41ab</td><td><span class="tag mono">Anthropic/sonnet-5-5</span></td><td><button class="btn btn-sm btn-ghost" data-toast="连通性测试通过 · 512 ms">测试</button> <button class="btn btn-sm" data-toast="已为 Codex CLI 轮换新令牌">换令牌</button> <button class="btn btn-sm btn-ghost" data-toast="已还原 Codex CLI 配置">还原</button></td></tr>
                <tr><td><span class="dot dot-ok"></span> <b>Cursor</b></td><td class="text-muted">手动客户端令牌</td><td class="mono">pdk_c_••••c07e</td><td><span class="tag mono">全部模型</span></td><td><button class="btn btn-sm" data-toast="已为 Cursor 轮换新令牌">换令牌</button> <button class="btn btn-sm btn-ghost" data-toast="打开客户端限额与模型范围策略">策略</button></td></tr>
              </tbody>
            </table>
          </div>
          <div class="card-panel" style="padding:9px 12px">
            <div class="row-between"><span class="fs-xs text-muted">▸ 配置备份与恢复记录（2 个工具 · 3 份备份）</span><span class="fs-2xs text-muted">PingDeck 改过的文件与备份</span></div>
          </div>
        </div>`,
      traffic: () => `
        <div class="gw-body">
          <div class="row-center" style="flex-wrap:wrap;gap:8px">
            <div class="input-box" style="flex:1;min-width:220px"><input placeholder="筛选网关流量：model:deepseek client:claude body:<关键词>…" /></div>
            <div class="seg">
              <button class="active" data-action="seg-pick">全部</button>
              <button data-action="seg-pick">失败</button>
              <button data-action="seg-pick">备用切换</button>
              <button data-action="seg-pick">鉴权/限流拦截</button>
            </div>
          </div>
          <div class="card-panel">
            <table class="kv-table">
              <thead><tr><th style="width:74px">时间</th><th style="width:136px">客户端</th><th>模型与路由</th><th style="width:88px">协议模式</th><th style="width:110px">状态</th><th style="width:108px">首字 / 总耗时</th></tr></thead>
              <tbody>
                <tr class="clickable" data-drawer="call-detail"><td class="mono text-muted">14:32:04</td><td><b>Claude Code</b></td><td class="mono">DeepSeek/v4-pro</td><td><span class="tag tag-accent">转码</span></td><td><span class="tag tag-ok">200</span></td><td class="mono">388ms / 1.4s</td></tr>
                <tr class="clickable" data-drawer="call-detail"><td class="mono text-muted">14:29:51</td><td><b>Codex CLI</b></td><td class="mono">DeepSeek/v4-pro → kimi-k2.6</td><td><span class="tag">直通</span></td><td><span class="tag tag-warn">200 · 故障转移</span></td><td class="mono">612ms / 2.1s</td></tr>
                <tr class="clickable" data-drawer="call-detail"><td class="mono text-muted">14:27:19</td><td class="text-muted">匿名客户端 · 127.0.0.1</td><td class="mono">gpt-6.1</td><td><span class="tag tag-bad">已拦截</span></td><td><span class="tag tag-bad">401</span></td><td class="mono">— / 1ms</td></tr>
              </tbody>
            </table>
          </div>
          <div class="row-between"><span class="fs-2xs text-muted">仅展示本地网关代理流量（含 401/403/429 路由前拦截）· 点击任意行查看抓包详情、重放或复制 cURL</span><button class="text-accent fs-xs" data-nav-to="usage">查看网关费用统计 ↗</button></div>
        </div>`,
      access: () => `
        <div class="gw-body">
          <div class="card-panel">
            <div class="card-panel-head"><span>服务与安全边界</span><span class="tag tag-ok">严格本机模式</span></div>
            ${row("监听端口", "默认仅绑定 127.0.0.1；修改端口后会重启网关。", `<span class="tag mono">127.0.0.1:4141</span>`)}
            ${row("要求客户端令牌鉴权", "每次请求都必须携带有效客户端令牌；未知或已吊销令牌一律返回 401。", sw(true))}
            ${row("在局域网提供服务 (0.0.0.0)", "仅在信任内网临时共享时开启（需要至少一个有效客户端令牌）。", sw(false))}
            ${row("额外允许的 Host 名", "逗号分隔，用于 Docker / devcontainer 或隧道（如 host.docker.internal）。", `<span class="tag mono">localhost, 127.0.0.1</span>`)}
            ${row("启动时自动开启网关", "PingDeck 打开时自动启动网关。", sw(true))}
            ${row("请求体调试捕获", "决定哪些网关交互保留请求与响应体供日志排障。", `<div class="seg"><button data-action="seg-pick">关闭</button><button class="active" data-action="seg-pick">仅出错时</button><button data-action="seg-pick">全部</button></div>`)}
          </div>
          <div class="card-panel">
            <div class="card-panel-head"><span>当前接受的 Host 名称与运行控制</span></div>
            ${row("本机回环（始终接受）", "防重绑定策略：不发送 CORS 头，未知 Host 或 Origin 请求一律返回 403。", `<span class="mono fs-xs">127.0.0.1, localhost, [::1]</span>`)}
            ${row("重置全部熔断器", "当上游故障恢复后，可手动清空所有服务商与模型的熔断器状态。", `<button class="btn btn-sm btn-danger" data-toast="已重置全部熔断器">重置全部熔断器</button>`)}
          </div>
        </div>`,
      rules: () => `
        <div class="gw-body">
          <div class="card-panel">
            <div class="card-panel-head"><span>网关备用规则（首个内容事件后绝不切换）</span><div class="row-center"><button class="btn btn-sm btn-ghost" data-nav-to="settings" data-set-page="ai/routing">重试与熔断参数 ↗</button><button class="btn btn-sm btn-primary" data-toast="已新增网关备用规则草稿">+ 添加规则</button></div></div>
            <table class="kv-table">
              <thead><tr><th>主模型</th><th>有序备用链</th><th style="width:90px">状态</th><th style="width:72px">今日命中</th><th style="width:140px">操作</th></tr></thead>
              <tbody>
                <tr><td class="mono"><b>DeepSeek/deepseek-v4-pro</b></td><td><span class="tag mono">Moonshot/kimi-k2.6</span> → <span class="tag mono">Anthropic/claude-sonnet-5-5</span></td><td><span class="tag tag-ok">生效</span></td><td class="mono">214</td><td><button class="btn btn-sm btn-ghost" data-toast="预览路由顺序">预览</button> <button class="btn btn-sm" data-toast="编辑备用链">编辑</button></td></tr>
                <tr><td class="mono"><b>Anthropic/claude-sonnet-5-5</b></td><td><span class="tag mono">OpenAI/gpt-6.1</span></td><td><span class="tag tag-ok">生效</span></td><td class="mono">78</td><td><button class="btn btn-sm btn-ghost" data-toast="预览路由顺序">预览</button> <button class="btn btn-sm" data-toast="编辑备用链">编辑</button></td></tr>
                <tr><td class="mono"><b>DeepSeek/deepseek-v4-flash</b></td><td><span class="tag mono">Moonshot/kimi-k2.6</span> → <span class="tag mono">Ollama/qwen3-coder:30b</span></td><td><span class="tag tag-ok">生效</span></td><td class="mono">104</td><td><button class="btn btn-sm btn-ghost" data-toast="预览路由顺序">预览</button> <button class="btn btn-sm" data-toast="编辑备用链">编辑</button></td></tr>
              </tbody>
            </table>
          </div>
        </div>`,
    },
    "prov-detail": {
      overview: () => DEFAULT_TAB_HTML["prov-detail"],
      models: () => `
        <div class="col-stack">
          <div class="row-between" style="gap:8px;flex-wrap:wrap">
            <div class="input-box sunken" style="width:280px"><input data-filter-rows="#prov-models-rows tr" placeholder="筛选模型 ID…" /></div>
            <div class="row-center" style="gap:6px">
              <button class="btn btn-sm" data-toast="已刷新 relay.acme.dev 模型目录">刷新模型</button>
              <button class="btn btn-sm btn-primary" data-modal="benchmark-cost">基准测试全部…</button>
              <button class="btn btn-sm" data-toast="已展开手动添加模型输入框">+ 添加模型</button>
            </div>
          </div>
          <div class="row-between" style="gap:8px;flex-wrap:wrap">
            <div class="row-wrap" data-pill-group="prov-models-flt">
              <button class="tag tag-accent" data-pill-pick>全部</button>
              <button class="tag" data-pill-pick>对话</button>
              <button class="tag" data-pill-pick>向量</button>
              <button class="tag" data-pill-pick>图像</button>
              <button class="tag" data-pill-pick>失败</button>
              <button class="tag" data-pill-pick>新增</button>
            </div>
            <div class="row-center" style="gap:8px">
              <button id="prov-models-pricing-pill" class="btn btn-sm btn-ghost" data-tab-jump="prov-detail:config" title="点击前往配置页修改计价基准与倍率">计价：OpenRouter · ×0.5 CNY</button>
              <span class="fs-xs text-faint">34/38 已启用 · 显示 38</span>
            </div>
          </div>
          <div class="card-panel" style="padding:0;overflow:hidden">
            <table class="kv-table" style="border:0;border-radius:0">
              <thead><tr><th>模型 ID</th><th>上下文 / 输出</th><th class="text-right">价格 / 1M</th><th class="text-right">最近探测</th><th class="text-right" style="width:72px">启用</th></tr></thead>
              <tbody id="prov-models-rows">
                <tr><td class="mono">deepseek-v4-pro <span class="tag" style="height:17px">推理</span> <span class="tag" style="height:17px">工具</span></td><td class="mono">128K / 16K</td><td class="mono text-right">¥1.00 / ¥4.00</td><td class="text-right"><span class="tag tag-ok">412 ms</span></td><td class="text-right">${sw(true)}</td></tr>
                <tr><td class="mono">claude-sonnet-5-5 <span class="tag" style="height:17px">推理</span> <span class="tag" style="height:17px">视觉</span></td><td class="mono">200K / 64K</td><td class="mono text-right">¥1.50 / ¥7.50</td><td class="text-right"><span class="tag tag-ok">690 ms</span></td><td class="text-right">${sw(true)}</td></tr>
                <tr><td class="mono">gemini-3.2-pro</td><td class="mono">1M / 64K</td><td class="mono text-right">¥0.62 / ¥5.00</td><td class="text-right"><span class="tag tag-bad">403 无权限</span></td><td class="text-right">${sw(true)}</td></tr>
                <tr><td class="mono">gemini-4-neon <span class="tag tag-accent" style="height:17px">新增</span></td><td class="mono">—</td><td class="text-right"><span class="tag tag-warn">未定价</span></td><td class="text-right"><span class="tag tag-warn">超时</span></td><td class="text-right">${sw(true)}</td></tr>
              </tbody>
            </table>
            <div class="pg-bar" data-pg-bar="prov-models" data-pg-total="38" data-pg-size="50" data-pg-page="1">
              <span class="pg-summary">显示第 1–38 条，共 38 条</span>
              <div class="pg-controls">
                <div class="pg-sizes"><button class="active" data-pg-size="50">50/页</button><button data-pg-size="100">100/页</button><button data-pg-size="200">200/页</button></div>
                <div class="pg-nav"><button disabled data-pg-btn="first">«</button><button disabled data-pg-btn="prev">‹</button><span class="pg-page-num">1 / 1</span><button disabled data-pg-btn="next">›</button><button disabled data-pg-btn="last">»</button></div>
              </div>
            </div>
          </div>
        </div>`,
      config: () => `
        <div class="col-stack">
          <div class="card-panel">
            <div class="card-panel-head"><span>基本信息</span><span class="tag">new-api 0.9.7</span></div>
            ${row("启用服务商", "关闭后，网关、对话与 AI 请求将跳过此服务商。", sw(true))}
            <div class="field-grid">
              <div class="field-col"><span class="field-label">服务商名称</span><div class="input-box"><input value="relay.acme.dev" /></div></div>
              <div class="field-col"><span class="field-label">基础地址（Base URL）</span><div class="input-box"><input value="https://relay.acme.dev" /></div></div>
            </div>
          </div>

          <div class="card-panel" id="prov-pricing-card">
            <div class="card-panel-head">
              <span class="row-center" style="gap:6px"><span>计价与费率</span><span class="tag tag-accent">2个单模型单独定价</span></span>
              <span id="prov-pricing-flash" class="tag tag-ok" hidden>已保存</span>
            </div>
            ${row(
              "目录基准价",
              "让该服务商下的模型默认按指定分发商目录价计费（选定后立即生效）。",
              `<button id="prov-pricing-basis-btn" class="select-pill" data-popover="prov-pricing-basis" data-cur-basis="openrouter">OpenRouter ▾</button>`
            )}
            <div id="prov-pricing-cov" class="fs-xs text-muted">OpenRouter 目录价覆盖 32 / 34 个启用模型，其余按官方价</div>
            <div id="prov-pricing-sug-banner" class="inline-banner" hidden>
              <span>检测到该地址匹配 OpenRouter 目录价</span>
              <button class="btn btn-sm" data-pick-basis="openrouter" data-basis-label="OpenRouter">一键采用推荐</button>
            </div>
            ${row(
              "中转站倍率与币种",
              "在基准价之上乘以折算倍率并指定结算币种（留空表示按基准原价）。",
              `<div class="row-center" style="gap:6px;flex-wrap:wrap">
                <div class="input-box" style="width:108px"><input id="prov-relay-mult" value="0.5" placeholder="例如 0.5" /></div>
                <button id="prov-pricing-curr-btn" class="select-pill" data-popover="prov-pricing-curr" data-cur-curr="CNY">CNY ▾</button>
                <button id="prov-relay-apply" class="btn btn-sm btn-primary" hidden data-action="apply-relay-pricing">应用</button>
                <button id="prov-relay-reset" class="btn btn-sm btn-ghost" hidden data-action="reset-relay-pricing">重置</button>
              </div>`
            )}
            <div id="prov-pricing-formula" class="pricing-formula">计费规则：OpenRouter 目录价 × 0.5（按 CNY 结算）· 单模型单独定价优先</div>
          </div>

          <div class="card-panel">
            <div class="card-panel-head"><span>高级配置</span><span class="fs-2xs text-muted">计价倍率已迁入上方「计价与费率」</span></div>
            <div class="field-grid">
              <div class="field-col"><span class="field-label">模型列表路径</span><div class="input-box"><input value="/v1/models" /></div></div>
              <div class="field-col"><span class="field-label">独立出站代理</span><div class="input-box"><input placeholder="留空跟随设置 › 网络" /></div></div>
            </div>
            <div class="row-center" style="justify-content:flex-end;gap:8px;padding-top:4px">
              <button class="btn btn-sm btn-danger" data-toast="已停用服务商 relay.acme.dev（不删除历史用量）" data-toast-tone="warn">删除服务商</button>
              <button class="btn btn-sm btn-primary" data-toast="已保存 relay.acme.dev 配置">保存配置</button>
            </div>
          </div>
        </div>`,
    },
    "usage-dim": window.PD.USAGE_DIMS,
  });
})();
