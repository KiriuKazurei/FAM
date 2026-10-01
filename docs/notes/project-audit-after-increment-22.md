# 增量 22 之后的全项目审计

## 1. 描述问题

四个增量（17c–22）连同 17a/17b 把标签与备注做成了三面闭合的功能族，但项目文档没有
跟着走完：主 README 的增量段落是一步步追加的，英文 README 停在增量 16（工具数 14、
对比度 13 对、无备注无撤销无任务传输），`docs/sqlDesign.md` 仍是旧栈的 PostgreSQL 设计
而没有标注。本次审计回答一个问题：**文档与代码现在是否说同一套事实。**

## 2. 分析问题

审计分两半：先跑全部闸门拿事实，再拿事实逐条对文档。

**闸门**（fresh 树，全部在本机实测）：

| 闸门 | 结果 |
| --- | --- |
| `cargo test --workspace` | 38 个测试二进制全绿 |
| `cargo clippy --workspace --all-targets -- -D warnings` | 0 错 |
| `cargo fmt --all -- --check` | 干净 |
| self-check light/dark（带 mutate） | 退出 0；断言读数全 1；对比度受门槛 18 对 0 不过；icons 21/21/21 pending=0；ipc retries=0 |
| `abi-smoke` 连跑两遍 | `-> ok`（insert/cut/move/copy/refuse-overwrite/vocabulary/batch/empty/merge/marker/delete/job 十二个标志全 1） |
| `model-smoke` | ok |
| fam-design 构建 | 通过 |

**事实核对**（代码为准，查文档偏差）：

| 事实 | 代码位置 | 文档此前的说法 |
| --- | --- | --- |
| ABI 导出 33 个 | `#[no_mangle]` 计数 | 未提数（不算错，补上了） |
| MCP 工具 15 个 | `tools.rs` 的 `verb: Verb::` 计数 + 快照测试 | 中文已改 15；英文写 14 |
| schema v3 | `store.rs` 的 `SCHEMA_VERSION` | 中文已改；`sqlDesign.md` 描述 PostgreSQL 且无标注 |
| 对比度受门槛 18 对 | self-check 日志 | 英文写 13 对 |
| fam-svc-fs 收了删除原语 | `remove_path` | 两个 README 的 crate 表都写"移动与复制" |

## 3. 解决方式

- **`README.en.md` 与增量 22 对齐**：crate 表（store schema v3 + 归并/回收、mcp 15 工具、
  fs 三原语）、当前状态段补齐 17c–22 的全部功能（行上标签、按标签搜与词表筛选栏、改名
  归并回收、备注对话框与行上记号、撤销、异步传输、拖悬高亮）、对比度 13→18、工具数
  14→15、self-check 段落补齐六张新截图与各步骤的读数语义、文档指引同步。构建与安装
  段落核对后未动（与代码一致）。
- **`docs/sqlDesign.md` 文首加横幅**：明确这是旧栈（Go 时期）的设计意图记录，
  PostgreSQL 形状从未实现；现行存储是 fam-svc-store 的 SQLite 三张表（schema v3，覆盖
  索引与唯一约束），以 store.rs 的 DDL 为准；检索走索引与路由而非数据库查询。
- **`README.md` 补两处**：crate 表的 ffi 行加"33 个导出"与传输任务；文档指引把
  sqlDesign 从"仍部分适用"改为"旧栈设计意图记录，文首已标注"。
- **`docs/legacy-specs.md`、`docs/history/`、`docs/代码辅助器.md` 未动**：前者是迁移
  基线（行号有锚），后两者一个已标注、一个无关。

## 4. 效果评估

### 正面证据

全部闸门数字如上表；文档侧 `grep -cE "14 tool|schema v2\)|13 gated" README.en.md` 返回 0
——三个陈旧声明清零。事实核对表的每一行都有代码位置可查。

### 代价与自记

- 英文 README 是**意译同步**而非逐句翻译：功能段落按 17c–22 的中文账重写为英文，
  措辞可能与未来的中文调整漂移——下一次中文大改时英文要跟着走一遍，这本身记在本次
  审计里（本项目没有英文文档的自动同步机制）。
- `README.en.md` 的 "Software Architecture (legacy)" 一节保留原样：它描述的是被替换的
  旧栈，中文版同样保留，不属于陈旧。

### 残留风险与局限

1. **文档与代码的核对是人工的**：导出数、工具数、对比度对数都会继续变。可做的机械化
   （测试断言 README 里的数字）未做——README 里的数字多是叙述性的，正则断言会脆。
   严重度：低。
2. **`docs/代码辅助器.md` 未审计**：通读后判断与当前实现无冲突（它描述的是辅助工具的
   用法而非架构），未加标注。严重度：低。
3. **英文 README 的安装/使用两节仍是 "Not available yet"**：与中文一致（打包在增量 15
   之后以 `cmake --install` 存在，但两节的"使用说明"确实没写）。严重度：低，归属：
   写使用说明那一步。
