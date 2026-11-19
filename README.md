# PingDeck

PingDeck 是本地优先的桌面 API 工作台，也是 LLM 网关和编码智能体的宿主。

你可以在一个应用里调试 HTTP 与 GraphQL 请求，起本地 Mock，编排多步流程，回看历史，管理环境变量。它也能接入 LLM 服务商，开本地网关给 Claude Code、Codex 等客户端用，或在对话里让智能体读代码、改文件、跑命令。数据只写在本机的 SQLite 与文件里，没有账号、云同步和遥测。

技术栈：纯 Rust 2024、GPUI + `gpui-kit` 原生 GPU 渲染（零 WebView、零 IPC、零 Node.js 运行时）。

## 平台与获取

- 当前版本 0.1.0，从源码构建并可通过 `cargo xtask bundle` 打包。
- 面向 macOS 13.0 以上与 Linux（X11/Wayland + Vulkan）。Windows 不在 CI 构建或测试范围内。
- 仅需 Rust 1.99.1（由 `rust-toolchain.toml` 固定，含 clippy 与 rustfmt）。Linux 另需 X11/Wayland/Vulkan/Fontconfig 开发包，见[开发指南](docs/development.md) §1。

## 快速开始

先选数据放哪。调试构建写独立的 `com.pingdeck.desktop.dev` 目录，与发布版数据分开。想要一个用完即弃的空库，把 `PINGDECK_DATA_DIR` 设成新目录：

```bash
PINGDECK_DATA_DIR="$(mktemp -d)" cargo run --locked -p pingdeck-gpui   # 桌面应用，临时空库
```

窗口打开后，第一步在「请求」里导入 Postman、OpenAPI、HAR 或 cURL，或者新建一个请求点发送。

打包当前平台，并对发布构建跑冒烟（脚本自建隔离 HOME）：

```bash
cargo build --release -p pingdeck-gpui --locked
cargo xtask bundle
scripts/smoke.sh target/release/pingdeck
```

## 模块

| 模块 | 做什么 |
| --- | --- |
| 请求 | HTTP、GraphQL 与 AI API 请求，认证、断言、脚本、代码片段、集合运行 |
| Mock 服务 | 本地 HTTP 服务器，按规则选响应，记录命中 |
| 流程 | 多步请求、提取变量、断言、条件与重试 |
| 历史 | 请求、流程、集合运行、Mock 命中与智能体调用的统一记录 |
| 环境 | 分环境的变量与秘密值，生产环境发送前确认 |
| 对话 | 编码智能体：流式 Markdown、工具审批、OS 沙箱、MCP、技能 |
| 服务商 | 服务商、密钥、模型、余额、测试台、本地网关与编码客户端 |
| 用量与日志 | 调用、失败、token、预估费用与延迟，以及模型调用日志 |
| 设置 | 通用、外观（8 套护眼 OKLCH 色系）、网络、Mock 默认值、数据与同步、智能体与模型等 |

## 限制

- 秘密值存在数据库旁的 `<db>.secrets`，是明文 SQLite，Unix 权限 `0600`。带秘密的导出文件同样是明文。
- 扫描、探测和测试台会向服务商发真实请求，可能产生费用。
- 网关没有 TLS。打开局域网模式后，令牌在网络上明文传输。
- macOS 应用未签名与公证，分发包会被 Gatekeeper 拦下。
- Windows 不在 CI 构建或测试范围内。

开发与平台限制见[开发指南](docs/development.md) §6。

## 文档

从 [`docs/README.md`](docs/README.md) 开始。编码代理先读 [`AGENTS.md`](AGENTS.md)。

## 许可

PingDeck 使用 [MIT](LICENSE)。Rust 第三方依赖许可清单位于 `assets/licenses/rust.txt`。
