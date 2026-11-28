# 开发与交付

维护者的操作手册：启动、定位、检查、冒烟与发布。测试总数、包体字节和耗时以命令输出为准。

## 1. 前提与启动模式

Rust 版本由 `rust-toolchain.toml` 固定，含 `clippy` 与 `rustfmt`；应用完全由纯 Rust（GPUI + `gpui-kit`）构建，不依赖 Node.js。

Linux 需安装 X11/Wayland/Vulkan/XKB/Fontconfig 开发包（详见 `.github/actions/setup-linux-deps/action.yml`）。冒烟需要 `sqlite3` 与 `curl`，Linux 退出步骤需要 `xdotool`（或 `wmctrl`）与 `xvfb-run`。

启动之前先定数据目录：

- 调试构建（`cargo run -p spanbay-gpui`）写 `com.spanbay.desktop.dev`，发布构建写 `com.spanbay.desktop`，两者父目录相同。启动时诊断日志记录 `INFO startup data directory: <路径>`。
- `SPANBAY_DATA_DIR=<绝对路径>` 在两种构建里都覆盖默认目录，相对路径和空值被忽略。目录不存在时按 `0700` 新建。
- 手工跑发布构建时，`HOME` 与 `CFFIXED_USER_HOME` 指向同一个 `mktemp -d`，写法见 §4。绝不碰真实的 `~/Library/Application Support/com.spanbay.desktop`。

常用启动与构建命令：

```bash
SPANBAY_DATA_DIR="$(mktemp -d)" cargo run --locked -p spanbay-gpui   # 桌面开发，临时空库
cargo run --locked -p spanbay-gpui                                     # 桌面开发，使用调试数据目录
cargo build --release -p spanbay-gpui --locked                         # 发布构建 -> target/release/spanbay
cargo xtask bundle                                                      # 组装 macOS .app / Linux 分发目录
```

## 2. 修改定位与检查

### 2.1 代码分层

Rust 工作区采用四层完全解耦结构（`crates/` + `xtask/` + `tokens/` + `locales/`）。依赖方向与各层机制见 [`architecture.md`](architecture.md) §2。

| 位置 | 职责 |
| --- | --- |
| `tokens/`、`locales/`、`assets/` | Layer 0 资产：8 套护眼 OKLCH 色系定义、14 域双语 JSON 词表、应用图标与依赖许可清单 |
| `spanbay-base` | 错误与秘密契约、取消、平台路径与文件权限、SSRF 守卫、代理 URL、URL 编码、脱敏、诊断日志 |
| `spanbay-response` | 大正文缓存、响应正文视图与唯一的 JSONPath 方言，只依赖 base |
| `spanbay-model` | 纯类型、设置注册表与校验、协议规则，没有 I/O |
| `spanbay-llm-protocol` | 各协议与 hub 互转的编解码、有界 SSE 解析，无网络与文件 I/O |
| `spanbay-client-config` | AI 编码工具配置的预览、写入、检查、还原与换令牌，只做文件 I/O |
| `spanbay-llm-domain` | LLM 记录类型、服务商校验、预设、探针规划、健康与状态规则、用量计价、目录分组，以及余额、抓包、扫描摘要的存储形状；无 SQLite |
| `spanbay-llm-ops` | 嗅探、余额适配器、抓包脱敏与计时、SSRF 守卫传输与 CA 包 |
| `spanbay-store` | 唯一依赖 SQLite 的仓储层：schema、连接池、明文保险库、附件；LLM 记录类型取自 llm-domain |
| `spanbay-executor` | HTTP 发送、重定向与重试、认证、脚本沙箱 |
| `spanbay-workbench` | 导入导出、Mock 与 Flow 引擎 |
| `spanbay-llm` | 路由与备用、探测执行、本地网关，并转出 llm-protocol 的协议编解码与 llm-domain 的记录和规则 |
| `spanbay-agent` | 运行循环、权限、OS 沙箱、MCP、技能、文件与终端工具、外部文本的提示围栏，以及聊天附件桥接与窗口路径授权 |
| `spanbay-app` | 组合根，以及请求、工作区、Flow、Mock、设置、系统用例和请求代码片段生成 |
| `spanbay-app-llm` | 服务商、网关、模型目录、余额与用量用例 |
| `spanbay-app-agent` | 对话、智能体运行、MCP 与技能用例 |
| `spanbay-app-sync` | 文件夹同步与秘密清理用例 |
| `spanbay-core` | 桌面运行时装配（第二段启动与退出编排）、核心门面，跨 crate 集成测试也在这里 |
| `spanbay-desktop` | Layer 2 桌面宿主：运行时、合帧、OS 服务、主题、i18n、持久化与状态机 |
| `spanbay-gpui` | Layer 3 薄 GPUI 渲染壳：仅 `ui` 适配组件库，`views` 渲染 9 模块 |
| `xtask` | 纯 Rust 门禁与打包：`check-arch`、`check-i18n`、`check-tokens`、`docs`、`bundle` |

四层依赖边界和视图代码约束由 `cargo xtask check-arch` 在真实源码树上强制执行。

生产代码不写 stderr：失败与提示走 `spanbay_base::logging::log_line`，进 `<数据目录>/logs/spanbay.log`（每条先脱敏）。没有日志文件时与调试构建里，同一行也写到 stderr。工作区 lint 把 `clippy::print_stderr` 设为 deny，测试内豁免（`clippy.toml` 的 `allow-print-in-tests`）。

SQLite 只在 `spanbay-store`。连接池借出独占连接，归还时回滚遗留事务；拿住共享存储锁挡不住池连接上的写者，锁也不跨 `.await` 持有。固定形状的高频 SQL 走语句缓存，文本变体可能多于缓存容量的动态查询不进缓存。

### 2.2 桌面与无头 UI 测试基建

- **Layer 2 无头逻辑测试**：`cargo test -p spanbay-desktop --locked` 在无窗口、零 GPU 环境下直接测试 OKLCH 编译、双语 i18n、原子 `0600` 持久化、命令面板模糊排序、流合帧与聊天线程 Reducer。
- **Layer 3 无头 GPUI 测试**：运行 `cargo test -p spanbay-gpui --features test-support --locked`。它在 `TestAppContext` 中挂载 `AppShell` 与内存宿主，验证 9 模块、16 套主题、20 页设置、6 个工具及万行虚拟表 / 10 MiB JSON 压测。

### 2.3 契约与夹具

- UI 与核心同进程直调纯 Rust 类型，无跨语言 IPC 桥接。核心运行时线格式由 `crates/spanbay-core/tests/ipc_contract/` 守卫。
- 设置注册表的样本冻结。智能体一类的新设置键在 `spanbay-model` 的设置校验里登记为补丁可指名的字段，默认值不序列化，机制见 [`architecture.md`](architecture.md) §13。
- 跨语言与行为夹具在仓库根的 `fixtures/`，覆盖 JSON 与标记格式化、JSONPath、代码片段、网关请求、LLM 目录与预设、核心一致性。它们固定宽容的读法，例如重复键、原始数字、畸形片段，换库之前先跑这些夹具。
- PRD 的 `#### Run limits (normative)` 表定义运行限制。改数字要同时改 PRD、`spanbay-agent` 的运行常量与智能体设置默认值；`cargo xtask docs` 对照表与常量，Rust 测试比较常量与默认值。
- 源码注释按 `docs/<文档>.md §N` 引用章节号，`cargo xtask docs` 会检查。重排章节时保留编号；要修正就只改注释，不改测试断言。

### 2.4 改动触发矩阵

| 改动 | 必跑 |
| --- | --- |
| 单个 Rust crate | `cargo test -p <crate> --locked`；跨 crate 或提交前跑 workspace 三件套 |
| 存储 schema | `cargo test -p spanbay-store --locked`，再用新数据目录冒烟 |
| LLM 记录类型与规则 | `cargo test -p spanbay-llm-domain -p spanbay-store --locked` |
| 预设的余额适配器表 | `cargo test -p spanbay-llm-domain -p spanbay-llm-ops --locked` |
| 响应正文视图与 JSONPath | `cargo test -p spanbay-response -p spanbay-executor --locked` |
| 桌面宿主与 Reducer | `cargo test -p spanbay-desktop --locked`、`cargo xtask check-arch` |
| GPUI 视图与主题组件 | `cargo test -p spanbay-gpui --features test-support`、`check-arch` |
| i18n 文案或键（`locales/`） | `cargo xtask check-i18n`、`cargo test -p spanbay-desktop --locked` |
| 颜色 token（`tokens/`） | `cargo xtask check-tokens`、`cargo test -p spanbay-desktop --locked` |
| 启动、数据库、Mock、追踪、秘密、网关 | 发布构建后跑 §4 的冒烟，再走人工清单 |
| 文档与 xtask | `cargo xtask docs`、`cargo test -p xtask`（改限额另测 `spanbay-agent`） |

workspace 三件套是 `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets --locked -- -D warnings` 与 `cargo test --workspace --locked`。

`cargo xtask docs` 依次跑 `xtask/src/docs.rs`（链接、锚点、章节引用、路径、运行限制表）与 `xtask/src/docs_lint.rs`（文风零误差检查），修改 `xtask` 后跑 `cargo test -p xtask --locked`。

CI/CD 为 100% 纯 Rust 双管线（统一 CI + 独立 Release），共用 `.github/actions/` 下的复合动作。Rust 测试在 CI 里用 cargo-nextest（`.config/nextest.toml`），本机用 `cargo test`：

- `ci.yml`（统一 CI 管线）：PR 按路径分流，并行跑 `xtask` 静态守卫、`cargo-deny`、双平台 `clippy`、`nextest` 与无头 GPUI 集成测试。推到 `main`/`master` 时补齐双平台全量 `nextest` 与发布版 `scripts/smoke.sh` 冒烟；每日定时追加忽略测试、10 MiB 发布压测与 `cargo audit`/`cargo machete`，最终由 `ci-gate` 汇总。
- `release.yml`（独立发布管线，`v*.*.*` 标签与手动预演）：核对版本号后并行编译 macOS（arm64、x64）与 Linux（x64），经 `cargo xtask bundle` 打包并通过 `scripts/smoke.sh` 冒烟，生成校验和并创建草稿 Release。

## 3. 测试、性能与压测

### 3.1 Rust 测试、并发与文件描述符

- 新测试只写三类：安全不变量；持久化与线格式契约（DDL、DTO、导入导出、网关线格式）；纯业务逻辑的一条主流程加一条错误路径。转发和默认值回显不写测试，取舍见 [`decisions.md`](decisions.md) §25。
- 单元测试与被测模块同文件，crate 自己的集成测试放它的 `tests/`。跨 crate 集成测试集中在 `spanbay-core`，用 `cargo test -p spanbay-core --test <目标> --locked` 跑。
- 不提高全局测试超时，机器忙时缩小范围或减少并发。不 `pkill`、`killall` 自己没启动的进程。测试自己限住文件描述符：Mock 的高并发连接测试串行化，网关测试限制同时存活的 harness 数。报 “Too many open files” 时不叠跑套件或构建，也不靠提高全局 `ulimit` 掩盖。
- `deny.toml` 打开 `unmaintained = "all"`，已审查的传递依赖条目逐条写明原因。

### 3.2 大正文格式化与 GPUI 原生部件压测

10 MiB 响应原生格式化由 `spanbay-response` 的忽略测试单独计时，nightly 在发布构建下断言 1.5 s 预算：

```bash
cargo test -p spanbay-response --release --locked -- --ignored ten_mib --nocapture
```

原生 GPUI 部件压测（中文 IME 控件、流式 Markdown 增量渲染、10,000 行虚拟 `DataTable` 与 10 MiB JSON Rope 载入）内置在「控制台」实用工具与无头集成测试中：

```bash
cargo test -p spanbay-gpui --release --features test-support --locked -- phase0_native_widget_bench --nocapture
```

[PRD](product/PRD.md) §8 的暖启动首绘、可交互与请求标签切换目标需在发布构建上同会话实测，报告写明构建、平台、数据量与测法。

### 3.3 网关延迟夹具

`spanbay-core` 的 `gateway_latency` 测试默认只断言结构性事实：每次调用只多一个上游请求；同协议直通的响应体除 `model` 外逐字节一致；流式首块在上游结束前到达；日志写入被阻塞时请求照常应答。相对开销只在 release 下按配对差值比较，噪声过大时打印 `inconclusive` 并通过。打印分位延迟的性能夹具不进 CI，要显式运行：

```bash
cargo test -p spanbay-core --release --locked --test gateway_latency -- --ignored --nocapture
```

## 4. 隔离数据目录与冒烟

`scripts/smoke.sh target/release/spanbay` 测发布构建（也可用 `SPANBAY_BIN` 指定二进制）。它只碰自己 `mktemp -d` 出来的 HOME，启动前去掉 `SPANBAY_DATA_DIR`、`CLAUDE_CONFIG_DIR`、`CODEX_HOME` 与 `GEMINI_CLI_HOME`。

脚本需要 `sqlite3` 与 `curl`，已有 `spanbay` 进程时拒绝运行。它依次建库并等 schema 版本对上，播种工作区与网关自启设置，再启动查 `/health`、目录 `0700` 与文件 `0600`、退出后端口释放，最后重启读回播种数据，并在 macOS 上确认未产生任何 WebKit 网站数据。schema 版本对不上说明二进制比源码旧，先重建。脚本覆盖不了的界面步骤见 [`product/smoke-checklist.md`](product/smoke-checklist.md)。

手工隔离启动时先命名变量再用 `env` 传入，免得同一条命令里的展开取到旧 `HOME`：

```bash
SMOKE_HOME=$(mktemp -d)
env -u CLAUDE_CONFIG_DIR -u CODEX_HOME -u GEMINI_CLI_HOME \
    HOME="$SMOKE_HOME" CFFIXED_USER_HOME="$SMOKE_HOME" XDG_DATA_HOME="$SMOKE_HOME/.local/share" \
    XDG_CONFIG_HOME="$SMOKE_HOME/.config" XDG_CACHE_HOME="$SMOKE_HOME/.cache" target/release/spanbay
```

- `CLAUDE_CONFIG_DIR`、`CODEX_HOME`、`GEMINI_CLI_HOME` 会把客户端工具的配置目录移到变量指向的路径，连接客户端时写入就落到那里，所以隔离运行要用 `env -u` 去掉。
- 本地界面状态（`ui_state.json`）与对话草稿（`chat_drafts.json`）由 `spanbay-desktop` 以 `0600` 权限原子写入数据目录，随隔离 HOME 完全隔离。

## 5. 构建、打包与平台

```bash
cargo build --release -p spanbay-gpui --locked
cargo xtask bundle
```

- macOS 产物组装在 `target/release/bundle/SpanBay.app`，Linux 产物组装在 `target/release/bundle/spanbay-linux/`（含可执行文件、`.desktop` 入口与图标）。
- 图标母版位于 `assets/icons/`；`Cargo.lock` 变更后可重新生成 Rust 许可清单：`cargo about generate --frozen --fail -o assets/licenses/rust.txt about.hbs`。

## 6. 已知限制

- Windows 不在 CI 构建或测试范围内，路径与打包配置没有验证过，macOS 与 Linux 的结论不外推到 Windows。
- macOS 应用未签名、未公证，Gatekeeper 会拦截分发包。发布检查要记录 `codesign` 与 `spctl`，并在签名构建上重跑沙箱自检。
- 绝对毫秒不能跨会话比较；性能回归必须在同一机器同一会话内交替对照测量。

## 7. 故障排查

| 现象 | 先查 |
| --- | --- |
| 启动提示“SpanBay started with a new database” | schema 指纹不符：旧库、`.secrets` 与 `attachments` 已改名为 `.bak-<UTC 时间>`；全部改回原名，用创建它的构建打开 |
| 启动拒绝打开数据库 | 旧库改名失败（权限或文件被占用）；手动把库和同名 `.secrets` 移出数据目录，或用 `SPANBAY_DATA_DIR` 换目录 |
| 冒烟拒绝启动 | 已有 `spanbay` 进程；先退出它，不杀别人的进程 |
| 启动提示“SpanBay is already running” | 同一数据目录已有一个 SpanBay 进程持有 `spanbay.lock`；切到它，或用 `SPANBAY_DATA_DIR` 指向别的目录再启动第二份 |
| 关窗时弹出退出确认框 | 有未保存的编辑或正在运行的智能体/流程/网关/Mock/MCP 服务；确认停止后关窗退出，点取消保持运行 |
| 冒烟第一步失败或超时 | 看脚本打印的 `app.log`，查隔离 HOME 是否可写、二进制是否比源码旧 |
| 网关或 Mock 端口被占 | 换端口或停掉占用进程；Mock 改端口、前缀后要重启 |
| `cargo xtask check-arch` 失败 | 检查是否打破了 4 层依赖边界、视图是否直接引入了 `gpui_kit::component`，或单文件是否超过 600 行 |
