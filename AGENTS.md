# AGENTS.md

面向在此仓库工作的编码智能体（Coding Agents），记录通用的架构边界、工程准则与验证守则。

## 项目是什么

PingDeck 是纯 Rust 本地优先的桌面 API 工作台与 AI 网关/智能体宿主（Rust 2024 + GPUI + `gpui-kit`，无账号、无云同步、无遥测、无 WebView、无 IPC、无 Node.js 运行时）。

文档用中文写，代码注释用英文。从 `docs/README.md` 进入；动手前先读 `docs/architecture.md`（机制与信任边界）和 `docs/development.md`（启动、检查命令、§2.4 的“改什么必须跑什么”触发矩阵）。代码、测试和真实运行输出定义“现在是什么”，`docs/product/PRD.md` 定义“应该做到什么”：两者冲突时先判断是实现缺陷、文档过期还是需求缺口，不要让现状自动降低验收要求。

## 常用命令

```bash
# 运行（调试构建写 com.pingdeck.desktop.dev，不碰发布版数据）
PINGDECK_DATA_DIR="$(mktemp -d)" cargo run --locked -p pingdeck-gpui   # 临时空库启动
cargo run --locked -p pingdeck-gpui                                     # 调试数据目录启动
cargo build --release -p pingdeck-gpui --locked                         # 发布构建 -> target/release/pingdeck
cargo xtask bundle                                                      # 组装 macOS .app / Linux 分发包

# 检查（与 CI 一致）
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo test --workspace --locked
cargo test -p pingdeck-gpui --features test-support --locked            # 无头 GPUI UI 集成测试
cargo deny check advisories bans sources
cargo xtask check-arch                                                  # 4 层架构解耦与视图行数守卫
cargo xtask check-i18n                                                  # locales/ 中英键对齐、占位符与排版守卫
cargo xtask check-tokens                                                # tokens/tokens.json OKLCH 色域与 WCAG 对比度
cargo xtask check-version                                               # Cargo.toml 版本号校验
cargo xtask docs                                                        # 文档链接、章节引用、路径、PRD 运行限制与文风
```

单个测试：

```bash
cargo test -p pingdeck-desktop --locked                                 # 无头桌面宿主与 Reducer 测试
cargo test -p pingdeck-gpui --features test-support --locked -- --nocapture
cargo test -p pingdeck-app --locked <名称片段>                           # 某个 crate 内按名称过滤
cargo test -p pingdeck-core --test ipc_contract --locked                # 跨 crate 集成测试目标
```

改动后的最小检查集（完整矩阵见 `docs/development.md` §2.4）：

- 单个 crate：`cargo test -p <crate> --locked`，提交前再跑 workspace 三件套。
- 桌面宿主与状态机：`cargo test -p pingdeck-desktop --locked` + `cargo xtask check-arch`。
- GPUI 视图与主题：`cargo test -p pingdeck-gpui --features test-support --locked` + `cargo xtask check-arch`。
- 色板与多语言：`cargo xtask check-tokens` + `cargo xtask check-i18n`。
- 启动、迁移、Mock、网关、秘密：release 构建后跑 `scripts/smoke.sh target/release/pingdeck`。二进制比源码旧时第一步就会因 schema 版本不符失败，需要先重建。

## 架构

### 四层完全解耦（UI 与实现彻底隔离）

仓库严格划分为四层，由 Cargo 依赖图和 `cargo xtask check-arch` 在 CI 中硬拦截：

1. **Layer 0（资产与契约）**：`tokens/tokens.json`（8 套护眼 OKLCH 色系）与 `locales/<area>/{en,zh-CN}.json`（14 域双语词表）、`assets/icons/`、`assets/licenses/`。
2. **Layer 1（纯业务核心）**：`crates/` 下 17 个核心 crate（经 `crates/pingdeck-core` 门面收敛），`#![forbid(unsafe_code)]`，零 UI / 窗口 / GPUI 依赖。
3. **Layer 2（UI 无关桌面宿主）**：`crates/pingdeck-desktop`（`#![forbid(unsafe_code)]`，禁用 `gpui*` / `objc2*`）。负责 Tokio 运行时、启停与单实例锁、流合帧、纯 Rust OS 服务、OKLCH 主题、嵌入式 i18n、原子 JSON 持久化及无头 ViewModel/Reducer。
4. **Layer 3（薄 GPUI 渲染壳）**：`crates/pingdeck-gpui`（`#![forbid(unsafe_code)]`，产出二进制 `pingdeck`）。**`crates/pingdeck-gpui/src/ui/` 是全仓唯一允许引入 `gpui_kit::component::*` 的隔离边界**；`src/views/**` 只通过 `crate::ui::*` 和 `crate::bridge::*` 消费 `pingdeck-desktop` 状态并派发意图，且单文件非测试代码 ≤ 600 行。

### Rust 工作区 DAG

箭头是“调用方 → 直接依赖”，这是有分支的 DAG，不是线性分层：

```
pingdeck-gpui → pingdeck-desktop → pingdeck-core（运行时装配、门面与集成测试）
pingdeck-core → app-sync, app-agent, app-llm, app, agent, llm, workbench, executor, store, client-config, response, model, base
app-sync → app-agent, app-llm, app, agent, llm, workbench, model, base
app-agent → app-llm, app, agent, llm, executor, store, response, model, base
app-llm → app, llm, client-config, store, model, base
app → agent, llm, workbench, executor, store, response, model, base
agent → llm, store, model, base     llm → llm-ops, llm-domain, llm-protocol, store, model, base
llm-ops → llm-domain, llm-protocol, response, model, base     workbench → executor, store, model, base
store → llm-domain, client-config, model, base     llm-domain → client-config, response, model, base
executor → response, model, base     llm-protocol → model, base     client-config → base
model → response, base     response → base
```

- **SQLite 严格收敛在 `pingdeck-store`**：只有 `pingdeck-store` 直接依赖 `rusqlite`，DDL 与全部 SQL 读写封装在 `storage::*` 与 `llm::store::*`。连接池 `DbHandle` 最多保留 `MAX_POOL_SIZE = 8` 个空闲连接，不限制同时借出的连接数；新连接设 WAL 与 `secure_delete = FAST`、附着 `vault` 并设语句缓存 64。归还 `DbConn` 时会丢弃未提交事务，不要假设拿住 `app.store` 就排除了池连接上的并发写者。
- 存储采用从头建立的单一基线（`storage/schema.rs`）：新建库在单一事务中建出完整 schema，把基线 DDL（含 `vault` 列定义）的指纹 `CURRENT_SCHEMA_VERSION` 写进 `user_version`。指纹相同的库直接打开，不执行迁移，也不保留历史增量迁移步骤。写事务统一走 `crate::sqlite::atomic_with`：自动提交时 `BEGIN IMMEDIATE`，已在事务内时用 savepoint 嵌套。
- 指纹不同的非空库不打开：启动时连同 `.secrets`、日志文件与 `attachments` 目录改名为 `<文件>.bak-<UTC 时间>` 移到一旁，新建空库并通知备份位置（`WorkspaceStore::open_or_set_aside`）。
- `AppState.store` 只串行化共享连接：锁不跨 `.await` 持有。文件库上 `SqliteSecretStore` 走 `DbHandle` 池连接，不取这把锁；只有内存库（测试）共用它，那里持锁时再调 SecretStore 会自锁。
- 秘密值在主库旁的 `<db>.secrets`（以 `vault` 附着）。它是**明文** SQLite，Unix 模式 0600；代码和文案都不能称它“加密”。
- HTTP 执行：Send/草稿/Runner/Flow/原生 `send_request` 共用 `app/execution_service` + `executor`；AI 服务商目标另走 `app/ai_run_service`。Flow 用 `Persistence::HistoryOnly`（写历史、不写响应 Cookie）。历史与 Cookie 不在同一事务，取消不回滚已提交的变量和远端副作用。
- Mock 管理器与网关管理器各自拥有独立的两线程 Tokio 运行时。网关的 Host/Origin 与令牌校验顺序、同协议直通与跨协议转码、“首个内容事件后锁定当前目标”这一提交点，见 `docs/architecture.md` §8。
- 信任边界：外部输入、导入内容、网络响应、模型输出、MCP 结果都不是授权，核心要重新验证对象归属、目标、权限与路径。智能体权限评估顺序固定（硬阻止 → 规则 Block → 强制询问 → 作用域 Allow → 单工具 Allow → 模式默认），Full access 不降低前三层，也不关闭 OS 沙箱（§11.2）。
- 长流（Chat、日志、Flow、Runner）直接走 `pingdeck-desktop` 的类型化内存通道（`ChannelSink` / `CoalescingSink`）与有序回放；`ChatThreadState` 负责序号去重与缺口重读，无需 JSON IPC 序列化。
- Lint 策略：`unsafe_code = forbid`；clippy 对 `unwrap_used`、`expect_used`、`panic`、`todo`、`unimplemented`、`unreachable`、`dbg_macro`、`print_stderr` 设为 deny（测试内豁免，见 `clippy.toml`）。失败与提示走 `pingdeck_base::logging::log_line` 写诊断日志，不用 `eprintln!`。

### 由 `xtask` 强制的仓库规则

- **`cargo xtask check-arch`**：验证 4 层 crate 依赖隔离；拦截 `crates/pingdeck-gpui/src/views/` 中直接引用 `gpui_kit::component`、`gpui_component::`、`pingdeck_core::storage` 或 `tokio::spawn`；强制视图文件非测试代码 ≤ 600 行。
- **`cargo xtask check-i18n`**：校验 `locales/` 下全部 14 域的 `en.json` 与 `zh-CN.json` 键完全对齐、占位符/复数对一致、中英文间距合规，并拒绝虚假加密或系统钥匙串宣称。
- **`cargo xtask check-tokens`**：验证 `tokens/tokens.json` 中 8 套色系（16 变体）的 OKLCH 色域与 WCAG ≥ 4.5 对比度。
- **PRD 运行限制表**：`docs/product/PRD.md` 的 `#### Run limits (normative)` 标题、英文行键、值单元格和表后锚点句不能移动或改写，`cargo xtask docs` 解析这张表并对照 `crates/pingdeck-agent/src/agent/limits.rs` 的常量。改限制数字要同时改 PRD、`limits.rs` 常量、`AiAgentSettings::default()` 和测试。
- **文档章节锚点与编号**：源码注释只引用 `docs/{design,development,architecture,product,decisions}.md` 的章节号与 `docs/decisions.md` 里的 `D-nnn`、`FE-R*`（§12）。重排章节时保留编号。

### 平台与运行时

- macOS 面向 13.0+（Metal GPU 渲染），Linux 面向 X11/Wayland + Vulkan。Windows 不在 CI 构建或测试范围内，不要把 macOS/Linux 的结论外推过去。
- CI 在 Ubuntu 与 macOS 上跑全套纯 Rust 检查，另有 `cargo deny check advisories bans sources`。

## 工程与性能准则

- **优先复用成熟开源库，拒绝重复造轮子**：实现通用能力前先查 `Cargo.lock` 已有依赖（如 `sha2`/`hmac`/`md-5`、`chrono`、`memchr`、`similar`、`ignore`/`globset`、`gpui-kit`）。仅在逻辑极简或开源库无法满足硬性安全预算（如 PDF 跨流 64 MiB 总解压上限）时保留有界自实现，并在文档中写明依据。
- **性能优先与实测验证**：
  - SQLite：固定 SQL 形状的高频查询使用 `prepare_cached`（连接缓存上限 64）；动态拼接 `WHERE`/`IN` 变体数可能超过缓存上限的查询使用 `prepare`，避免 LRU 抖动退化。
  - 流式与字节处理：优先使用 SIMD 扫描（`memchr`）、所有权移动与零拷贝合帧通道，避免热路径多余拷贝。
  - 性能结论必须基于同会话实测数据，不凭估算声称优化幅度。
- **测试精简与有效性**：只测试可观测行为契约、边界条件、错误路径与安全不变量；禁止跨层纯转发重复测试、默认值/字面量回显测试和无意义永真断言。

## 测试与环境守则

- 不要提高全局测试超时；机器忙时缩小范围或降低并发。
- 不要 `pkill`/`killall` 不是你启动的进程，只停自己启动的 job 或 PID。
- `cargo test` 报 “Too many open files” 时，不要叠跑多个套件或构建，也不要靠提高全局 `ulimit` 掩盖；测试自身已经有连接数和 harness 上限。
- 手工启动桌面应用或跑冒烟时必须用隔离数据目录：`HOME` 与 `CFFIXED_USER_HOME` 指向同一个 `mktemp -d`，Linux 另设 `XDG_*`，写法见 `docs/development.md` §4。绝不能动真实的 `~/Library/Application Support/com.pingdeck.desktop`。`scripts/smoke.sh` 需要 `sqlite3` 与 `curl`，已有 `pingdeck` 进程在运行时会拒绝启动。
