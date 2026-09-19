# 已归档：旧栈设计文档

本目录下的文档描述的是**已被删除**的多语言旧栈（Go 核心 + Java 业务 + Electron 界面 + C++ 桥），
保留仅为了追溯当时的意图。**不要按它们实现任何东西。**

共同问题是：它们写的是设想的目录树，不是实际存在的代码。核对结论如下（对照快照 `84342b5`）：

| 文件 | 设想 | 实际 |
|---|---|---|
| `struct.md` | Go `services/` 六大域 + gateway | gateway 与索引层职责已整体移交 Everything；`ai/` 域从未创建 |
| `coreStruct.markdown` | `core/` 含 `hardware/`、`cache/`、`ipc/`、`logging/`、`utils/` 五模块，含 `detector.cpp`、`lru_cache.ts`、`protocol.json` 等 | 前三者**从未存在**；`core/ipc/` 只有一个 0 字节 `shared_memory.cpp`；`core/logging/logger.ts` 从未进过 git 且为 0 字节 |
| `frontend.md` | Electron 主/渲染进程 + preload + builder 配置 | 渲染进程 12 个文件全为 0 字节，`package.json` 与 `electron-builder.config.js` 也是 0 字节，且 `main.ts` 指向的 `index.html` 从不存在。实际目录另有拼写错误：`compents/`、`preeload-scripts/`、`baes.css` |
| `后端结构树Java` | `com.fileauto.manage` 包树 | 实际包名是 `com.fam.manage`，且该包目录从未创建；Spring Boot 因此扫不到任何 Bean，也无法启动 |

仍然有效的文档在上一级：`../sqlDesign.md`、`../代码辅助器.md`（各自的部分章节），以及迁移必读的 `../legacy-specs.md`。
