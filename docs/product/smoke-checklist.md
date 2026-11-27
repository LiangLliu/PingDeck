# 发布前 UI 冒烟清单

这份清单在**发布构建**上人工执行，覆盖脚本无法驱动的端到端闭环。先跑脚本化的那一半：

```bash
scripts/smoke.sh target/release/spanbay
```

脚本检查隔离数据目录、建库与 schema、网关自启、目录与文件权限、重启读回以及 macOS WebKit 数据隔离。UI、键盘、审批、取消、机密显示、模块交接与客户端联通按下面逐项人工核验。

范围说明：

- **Demo**：`npm run dev:demo` 或 `build:demo`，内存数据，只验证交互、视觉与键盘。
- **桌面**：发布构建 + 隔离数据目录，验证真实 SQLite、网络、Mock 监听、网关、智能体与平台行为。
- **Demo + 桌面**：交互与视觉在 Demo 核对，持久化与真实后端在桌面核对。

## 0. 准备

### 0.1 构建标识

在仓库根目录记录：

```bash
# 干净树
git rev-parse --short HEAD

# 脏树：dist/index.html 的短哈希
node -e "console.log(require('crypto').createHash('sha1').update(require('fs').readFileSync('dist/index.html')).digest('hex').slice(0,12))"
```

同时记录 OS 版本、构建时间与签名/公证状态。不要把真实数据目录或任何机密写进结果。

### 0.2 隔离数据目录

先命名变量，再用 `env` 传入，避免同句展开取到宿主 `HOME`：

```bash
SMOKE_HOME=$(mktemp -d) || exit 1
env -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u GEMINI_CLI_HOME \
    HOME="$SMOKE_HOME" \
    CFFIXED_USER_HOME="$SMOKE_HOME" \
    XDG_DATA_HOME="$SMOKE_HOME/.local/share" \
    XDG_CONFIG_HOME="$SMOKE_HOME/.config" \
    XDG_CACHE_HOME="$SMOKE_HOME/.cache" \
    target/release/bundle/macos/SpanBay.app/Contents/MacOS/spanbay
```

Linux 用 `target/release/spanbay`；macOS 用 `.app/Contents/MacOS/spanbay`。`HOME` 与 `CFFIXED_USER_HOME` 必须指向同一个新目录，真实 `~/Library/Application Support/com.spanbay.desktop` 与真实 WebKit 目录保持未被写入。

### 0.3 本机假上游（仅服务商/网关/对话场景）

监听 `127.0.0.1:4999`，只实现 `GET /v1/models` 与 `POST /v1/chat/completions`（按 `stream` 返回 JSON 或 SSE，结束原因 `stop`），其他路径返回 404：

```bash
node <<'NODE'
const http = require("node:http");
http.createServer((req, res) => {
  const path = new URL(req.url, "http://127.0.0.1").pathname;
  if (req.method === "GET" && path === "/v1/models") {
    res.writeHead(200, { "content-type": "application/json" });
    return res.end(JSON.stringify({ object: "list", data: [{ id: "fake-1", object: "model" }] }));
  }
  if (req.method === "POST" && path === "/v1/chat/completions") {
    let body = "";
    req.on("data", (chunk) => { body += chunk; });
    req.on("end", () => {
      let stream = false;
      try { stream = JSON.parse(body || "{}").stream === true; }
      catch { res.writeHead(400, { "content-type": "application/json" }); return res.end(JSON.stringify({ error: { message: "invalid JSON" } })); }
      const content = "Hello from the fake";
      if (!stream) {
        res.writeHead(200, { "content-type": "application/json" });
        return res.end(JSON.stringify({ id: "chatcmpl-fake", object: "chat.completion", choices: [{ index: 0, message: { role: "assistant", content }, finish_reason: "stop" }] }));
      }
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      const chunk = (delta, finish = null) => res.write(`data: ${JSON.stringify({ id: "chatcmpl-fake", object: "chat.completion.chunk", choices: [{ index: 0, delta, finish_reason: finish }] })}\n\n`);
      chunk({ role: "assistant", content: "" });
      content.split(" ").forEach((part, i) => chunk({ content: i === 0 ? part : ` ${part}` }));
      chunk({}, "stop");
      res.end("data: [DONE]\n\n");
    });
    return;
  }
  res.writeHead(404, { "content-type": "application/json" });
  res.end(JSON.stringify({ error: { message: "not found" } }));
}).listen(4999, "127.0.0.1", () => console.log("fake upstream on http://127.0.0.1:4999/v1"));
NODE
```

基础 URL 填 `http://127.0.0.1:4999/v1`，密钥用假值 `sk-FAKE-SMOKE-DO-NOT-USE-0001`。假上游未实现的探测项失败属预期，本机回环不产生服务商费用。

## 1. 人工场景

### 1.1 启动、工作区、窄屏与退出

**#1 空数据目录首启**（桌面）
- 动作：用 §0.2 的新目录启动。
- 期望：壳层正常绘出，请求模块可见，无错误屏和报错 toast；数据目录权限为 `0700`，库与 `.secrets` 为 `0600`。

**#2 工作区创建、切换与重启**（桌面）
- 动作：创建新工作区并切换过去，发一条请求，重启同一目录。
- 期望：新工作区生效且原工作区保留；重启后工作区、请求与历史均可读回。

**#3 结构版本不符时的隔离与新建**（桌面）
- 动作：在隔离目录里把 `spanbay.sqlite3` 的 `PRAGMA user_version` 改成错误值后重启。
- 期望：主库、`.secrets`、日志与 `attachments` 被改名为 `.bak-<UTC 时间>` 移到一旁，新建空库并弹窗列出备份路径；权限不足无法改名时报错拒绝打开。

**#4 窄屏、缩放、键盘与多主题**（Demo + 桌面）
- 动作：窗口缩到 980 × 680；200% 缩放；按 `⌘B` 两次、`⌘1…⌘8`、`⌘K`、Esc、Tab；在「设置 › 通用 › 外观」切换 5 套色彩主题与浅/深色模式。
- 期望：缩窗和进设置不改变侧栏形态；`⌘B` 折叠为图标窄轨并在重启后保持；窄于 80 em 时列表窗格收成浮层；无横向滚动；Esc 只关最内层；五套主题（含极简石墨在浅/深色下）对比度清晰且无重叠。

**#5 退出确认、macOS 驻留与单实例**（桌面）
- 动作：环境留未保存修改后按 `⌘Q` / 菜单退出（点取消）；Mock 或智能体在跑时退出（点取消再确认）；macOS 网关运行时点红灯再点 Dock；同目录用 `open -n` 起第二份进程。
- 期望：先问未保存文档，再问运行中工作（智能体为危险确认、仅常驻服务为普通确认），取消什么都不发生。macOS 红灯仅隐藏窗口、网关照常应答 `/health`；第二份弹 “SpanBay is already running” 后以 0 退出，不干扰第一份运行。

### 1.2 请求、Mock、流程、历史与环境

**#6 Mock → 请求 → 历史**（桌面）
- 动作：新建 Mock 服务，配 `GET /ping`（200 JSON `{"id":"smoke-1"}`）和带 `Set-Cookie: smoke=1` 的 `/cookie`，启动；在请求模块发 `GET http://127.0.0.1:<port>/ping`。
- 期望：返回 200 与响应体，状态行有耗时与大小；历史出现该记录，Mock 流量有命中。

**#7 保存语义与环境草稿**（Demo + 桌面）
- 动作：设置改外观/字号；Mock 改路由等 500 ms；请求改内容不保存；环境改 Staging 的 `base_url` 不保存，再发送请求并重载。
- 期望：设置与 Mock 自动保存并在重载后保留；请求标签保持未保存标记；未保存环境变量不参与发送。

**#8 流程抽取、断言失败与取消**（桌面）
- 动作：在 #6 的 Mock 加 `GET /smoke`（200 `{"id":"smoke-1"}`）和 `GET /smoke/use`（201，延迟 1500 ms，`Set-Cookie: smoke=1`）。建两步流程：抽取 `id` 传给第二步并断言 200；运行一次，再运行并在第二步延迟中取消。
- 期望：第一步抽取值进入第二步 URL；失败断言写明期望 200 / 实际 201；每次已发出尝试写一条 `source=flow` 历史；取消不继续后续步骤，已完成步骤不回滚；工作区 Cookie 不含流程的 `smoke=1`。

**#9 机密脱敏与生产环境确认**（桌面）
- 动作：环境存固定长 sentinel（如 `sk-SENTINEL-DO-NOT-USE-0123456789abcdef`）并发带 `{{token}}` 头的请求，查历史与 Mock 流量；在 Production 环境分别发 `GET`、`POST`、预请求脚本改方法、以及脚本 `pm.sendRequest`（开/关 `confirm_safe_methods`）。
- 期望：列表与常规 DTO 只显示掩码或尾号，sentinel 不出现在持久化日志与历史里；默认 `POST` 询问、`GET` 不问，开安全方法确认后 `GET` 也问，脚本改方法重新检查；`pm.sendRequest` 受同一生产确认守卫，未确认的受保护方法被拒。

**#10 模块交接与长内容边界**（Demo + 桌面）
- 动作：从失败响应和调用日志点「问智能体」；从服务商菜单点「配置」；打开不存在的深链；Demo 中打开 18.4 MB 模拟大响应与 10 MiB JSON 场景。
- 期望：交接只生成带脱敏上下文的草稿，发送前不调用模型；缺失深链显示内联提示；大响应明确标出截断或模拟尺寸，状态用形状加文字呈现。

### 1.3 服务商、网关、模型目录与用量

**#11 服务商、网关备用规则与客户端联通**（桌面）
- 动作：用 §0.3 假上游建 `Smoke 假上游` 与 `Smoke 备用`；在「服务商 › 网关 › 备用规则」给 `Smoke 假上游/fake-1` 配备用 `Smoke 备用/fake-1`；启动回环网关（开令牌）并建只允许该模型的客户端；分别用正确令牌、已吊销令牌、未知令牌和越权模型请求 `/v1/models` 与 `/v1/chat/completions`；停用主服务商密钥后再请求。
- 期望：`/v1/models` 输出名称形式 `Smoke 假上游/fake-1`；主密钥停用后由 `Smoke 备用/fake-1` 故障转移应答；吊销/未知令牌返 401、越权模型返 403 且均不调用上游。

**#12 客户端配置写入、验证与恢复**（桌面）
- 动作：在连接客户端向导预览差异并确认落点在 `SMOKE_HOME` 内，执行写入与本机探针验证；测试恢复上一次配置或恢复原配置。
- 期望：预览脱敏、路径位于隔离 HOME 内；界面明确区分本机探针通过与真实客户端首请求到达；恢复前展示差异确认。

**#13 密钥管理、分组与轮换**（桌面）
- 动作：在服务商「概览」增删、重命名、加备注、限时查看密钥（等 30 秒或切页）、配置密钥分组与轮询/冷却，再查「配置」标签与服务商列表卡片。
- 期望：密钥和端点只在「概览」就地编辑，明文 30 秒或切走后恢复掩码；分组密钥按组轮询，429 冷却按密钥与模型计并在重启后清零；「配置」标签不含重复的密钥/端点编辑。

**#14 模型目录播种、手动与自动同步**（桌面）
- 动作：断网首启打开「设置 › 模型 › 模型与价格」等待 2 分钟；联网点「立即同步」、断网重试、同步中取消；开关「自动同步」并切换 1/7/30 天间隔。
- 期望：内置快照正常播种，自动同步默认关闭且启动不联网；手动同步在页首显示摘要不弹对话框，失败和取消均保留现有表格与自定义价格。

**#15 模型目录分组、服务商计价与未收录模型**（Demo + 桌面）
- 动作：按厂商、分发商自有条目和“只看在用的”筛选目录；在「服务商计价」绑定分发商；点击「未收录的模型」行打开抽屉，在「这是哪个模型？」搜索并关联目录模型；验证动态/自定义/按次固定三种计费模式的用量费用。
- 期望：每模型一行且默认官方价，无官方价标参考价；「这是哪个模型？」列表项两行排布清晰、零重叠、搜索正常；归属后从未收录区消失并按生效优先级计费（服务商覆盖 > 服务商计价 > 模型默认 > 官方价 > 参考价），未知价不显示成 0，固定价不对失败请求计费。

**#16 用量与日志：窗口、拆解、未路由排除与延迟**（Demo + 桌面）
- 动作：打开「用量与日志」（`⌘8`），切换今天、本月、上月、1h、24h、7d、30d、90d、全部，检查统计段与调用段；从服务商和网关点「查看用量」跳转；制造一次网关 401 拒绝请求后刷新。
- 期望：左栏只列「全部服务商」与已配置服务商，不出现「未路由」。网关 401 拒绝保留在「调用」日志里供排查，不计入「统计」调用数、失败率与密钥拆解。延迟卡优先展示流式平均首 token 时间；跨模块跳转带上筛选，980 px 下标签条不溢出。

### 1.4 对话、智能体与权限

**#17 审批、重载、计划模式与越界拦截**（桌面）
- 动作：在「每次询问」下让智能体执行 shell，分别测试允许一次、拒绝、等待中重载页面；切到「计划」要求写文件；用文件工具读写项目文件夹外路径。
- 期望：审批卡列出完整命令与风险；重载后运行和审批状态恢复，关闭视图不算拒绝；计划模式下写工具由核心拒绝；文件夹外读写被拒绝。

**#18 模型角色、子智能体、个性化与默认权限**（Demo + 桌面）
- 动作：在「设置 › 智能体 › 子智能体」配快速角色与备用链并派只读调查；在「个性化」保存“我的说明”并验证超 4,000 字拦截；在「权限」切换默认权限为「完全权限」（先取消再确认）。
- 期望：子智能体使用角色模型与备用链并在回答头标明模型与费用；“我的说明”失焦保存且超长不截断保存；完全权限必须显式确认且只作用于新对话。

**#19 macOS Finder 启动与 `npx` MCP 服务器**（桌面，macOS）
- 动作：从 Finder 双击打开打包应用，在「设置 › 智能体 › MCP 服务器」测试 `npx` 与不存在的 `npx-missing`。
- 期望：`npx` 正常列出工具；缺失命令错误中列出命令名、搜索过的 PATH 和修法。

### 1.5 设置页与配置同步

**#20 设置结构、数据与同步存储、快捷键与关于**（Demo + 桌面）
- 动作：核对五组设置页（「通用」「请求与数据」「智能体」「模型」「系统」）；打开「数据与同步 › 存储」查路径、占用清理、压缩与备份数据库；在「快捷键」搜索；打开「关于」与「高级」；测试「请求默认值」关 TLS 校验后的跳转链接。
- 期望：页首仅面包屑无重复大标题；「存储」显示当前 schema 版本、占用清理、压缩、一致备份与明文保险库（0600）说明；设置页不链回工作模块；980 px 下控件不折行错位。

**#21 文件夹配置同步：预览、导入与坏文件**（桌面）
- 动作：向 `SMOKE_HOME` 内目录导出配置，修改外部文件后预览再导入；放入损坏 JSON 或非法 manifest 重新预览。
- 期望：预览列出新增、变更、移除、冲突和危险项且不改本地；本地较新默认保留；坏文件整体拒绝，不把对象当删除。

### 1.6 人工不做的事

- 不为验证审批超时等 600 秒，也不等 6 小时验证自动同步复查与退避；这些由单元测试覆盖。
- 不承诺扫描与测试台零 token、零费用；假上游只验证本机流程。
- 不承诺取消后历史与 Cookie 完全回滚，也不承诺任意正文或未知短机密全部自动脱敏。
- 不杀非自己启动的 `spanbay` 或测试进程，不在任何步骤触碰真实 `HOME`、数据目录、客户端配置或真实密钥。

## 2. 结果记录

每项按以下格式记录：

```text
#<n> <pass | fail: 看到什么 | n/a: 原因 | accepted: 已接受偏离与影响>  build=<id>  os=<版本>  scope=<Demo|桌面>
```

失败项要么修复，要么作为已接受偏离写进发布说明并注明影响范围，不用过往版本表现代替复现步骤。

## 3. 脚本那一半的契约

`scripts/smoke.sh [path/to/spanbay]`：

- 默认二进制：`$SPANBAY_BIN`，否则 `target/release/spanbay`。
- 只碰 `mktemp -d`；`HOME`、`CFFIXED_USER_HOME`、`XDG_*` 全部指向它；已有 `spanbay` 进程时拒绝运行。
- 需要 `sqlite3` 与 `curl`；Linux 无显示时用 `xvfb-run` 重入。
- 校验建库与 `CURRENT_SCHEMA_VERSION`、播种工作区与网关自启、`/health` 200、目录 `0700` 与文件 `0600`、退出释放端口、重启读回与 macOS WebKit 隔离。
- 退出码 0 表示通过，1 表示失败或缺少依赖；不覆盖 UI、键盘、审批、取消、流程、同步与视觉布局。

脚本与人工清单两侧都完成才算发布冒烟完成。
