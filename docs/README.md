# PingDeck 文档索引

按读者的任务找文档。项目是什么、怎么跑，先看 [`../README.md`](../README.md)。

## 三个入口

- 使用：[`../README.md`](../README.md) 的快速开始，再读 [`product.md`](product.md) 里对应模块的一章。
- 开发：[`development.md`](development.md) 讲启动和检查，动手前读 [`architecture.md`](architecture.md)。
- 编码代理：先读 [`../AGENTS.md`](../AGENTS.md)，命令、仓库规则和信任边界都在那里。

## 想知道什么，读哪份

| 想知道 | 读 |
| --- | --- |
| 要做到什么、范围、运行限制 | [`product/PRD.md`](product/PRD.md) |
| 某个模块管什么、何时保存、操作留下什么 | [`product.md`](product.md) |
| 系统由什么组成、信任边界、失败时保证到哪一步 | [`architecture.md`](architecture.md) |
| 某个取舍为什么这样、什么时候重议 | [`decisions.md`](decisions.md) |
| 视觉、状态归属、焦点、危险操作的界面原则 | [`design.md`](design.md) |
| 九大模块的离线交互原型 | [`design/prototype/index.html`](design/prototype/index.html) |
| 怎么启动、改完跑什么、怎么排错 | [`development.md`](development.md) |
| 发布前人工走查 | [`product/smoke-checklist.md`](product/smoke-checklist.md) |
| 一个词怎么叫、哪些叫法不用 | [`reference/glossary.md`](reference/glossary.md) |
| Demo 与测试假数据用哪些名字 | [`design/content-bible.md`](design/content-bible.md) |

## 每份文档的职责

- `product/PRD.md`：需求基线，写目标、范围、验收与运行限制。用户可见的限制数值只在这里。
- `product.md`：按模块写对象、保存语义、模块交接和产品规则。
- `architecture.md`：机制与信任边界，从 crate 依赖到网关、智能体和退出顺序。
- `decisions.md`：短 ADR，写约束、选择、理由、代价和重议条件。
- `design.md`：UX 原则，写视觉语言、状态归属、键盘焦点和不可信内容的显示。
- `design/prototype/index.html`：九大模块与色彩主题的离线交互原型，颜色来源统一为 `tokens/tokens.json`。
- `development.md`：启动、代码定位、改动触发矩阵、冒烟、构建与排障。
- `product/smoke-checklist.md`：发布构建上的人工场景、动作与期望。
- `reference/glossary.md`：中英文术语与不用的叫法，界面文案按它取词。
- `design/content-bible.md`：Demo 与测试假实现共用的虚构数据。

## 冲突时听谁

- 代码、测试和真实运行输出定义现在是什么；PRD 定义应该做到什么。
- 两者冲突时先分类：实现缺陷就改代码，文档过期就改文档，需求缺口就改 PRD 或记成待办。现有实现不自动降低验收要求。
- 截图和设计稿只作视觉参考，验证要在 Demo 或桌面应用里做。

## 文档契约

- PRD 的运行限制表由 `cargo xtask docs` 对照智能体运行常量，改数字要同时改代码。
- `design/content-bible.md` 的虚构故事与样例数据作为设计参考与测试数据契约。
- 源码注释按章节号引用文档，重排时保留编号。`cargo xtask docs` 对结构与文风均按零误差检查。
