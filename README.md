# FileAutoManage（FAM）

#### 介绍
FAM 是一个通用资源管理器，用于应对多数量、大体积文件的浏览、查找与基础编辑。
全盘名称检索交由 **Everything SDK** 承担，本项目自身只维护浏览所需的懒目录索引；
图片、缩略图、RAW 解析与 AI 推理已划出范围，由另一项目承担。
文件标签仍需持久化，存储于 SQLite。

#### 当前状态
旧栈（Go / Java / Electron / C++）已整体删除，代码重建为 Rust workspace + Qt 界面：

| 位置 | 内容 |
|---|---|
| `crates/fam-core` | 配置与 Windows 路径归一（`path_key`） |
| `crates/fam-svc-store` | SQLite 标签存储 |
| `crates/fam-svc-walk` | 可取消的递归遍历与名称检索 |
| `crates/fam-sched` | 监视事件 → 有界队列 → 批量合并的行变更 |
| `crates/fam-view` | 带位置的行模型 |
| `crates/fam-ffi` | 对 C++ 暴露的 JSON-over-C ABI（`famcore.dll`） |
| `apps/fam-cli` | `fam` 命令行（无头测试台） |
| `apps/fam-shell` | Qt6 Widgets 界面 |

工程处于**资源管理器重建**的起点：当前实现是"查询即递归遍历"的检索原型，
正在改为以懒目录索引 + 流式虚拟化为核心的浏览架构，见 `docs/legacy-specs.md` 与重构计划。

#### 构建
CLI 与测试用默认工具链：

```bash
cargo test --workspace
cargo build --release -p fam-cli
```

`famcore.dll` 必须用 GNU 工具链构建（Qt 侧是 MinGW kit，MSVC 版无法与之链接），
且 PATH 首位须是 Qt 自带的 posix MinGW——系统里那个 MCF 线程模型的 MinGW 会让 GNU 工具链链接失败：

```bash
export PATH="/d/Applications/Qt/6.11/Tools/mingw1310_64/bin:$PATH"
cargo +stable-x86_64-pc-windows-gnu build -p fam-ffi --target x86_64-pc-windows-gnu
```

界面用 Qt 自带的 CMake/Ninja 配置构建，导入库指向上一步产物：

```bash
cmake -S apps/fam-shell -B apps/fam-shell/build -G Ninja \
  -DCMAKE_CXX_COMPILER=<Qt mingw>/bin/g++.exe \
  -DCMAKE_PREFIX_PATH=<Qt>/6.11.2/mingw_64 \
  -DFAMCORE_IMPLIB=target/x86_64-pc-windows-gnu/debug/libfamcore.dll.a \
  -DFAMCORE_DLL=target/x86_64-pc-windows-gnu/debug/famcore.dll
cmake --build apps/fam-shell/build
```

抓界面截图验证时用原生 `windows` 平台插件——`QT_QPA_PLATFORM=offscreen` 没有字体库，
所有字形（含 ASCII）都会渲染成豆腐块。

#### 软件架构
单进程形态，不再设网关层与多语言分层：

> 核心逻辑
>> Rust 单体，文件监视采用有界队列 + 满即丢弃 + 单消费者串行化，向界面回推变更

> UI 层
>> Qt 原生界面，列表以 `QAbstractListModel` 承载，条目须批量合并插入而非逐条通知

> 文件搜索
>> 调用 Everything SDK 按文件名检索，限定目录与后缀即可，不自行遍历建索引

> 持久层
>> SQLite 存储标签与文件路径，表结构见 `docs/sqlDesign.md`

#### 文档指引
- `docs/legacy-specs.md` —— 迁移必读，旧栈中仅存在于代码里的规格已提取于此
- `docs/notes/` —— 重建期的问题分析记录，每篇按"描述问题 / 分析问题 / 解决方式 /
  效果评估"四层写，含当时未解决的风险；写作约定见 `docs/notes/README.md`
- `docs/sqlDesign.md`、`docs/代码辅助器.md` —— 仍部分适用
- `docs/history/` —— 已作废的旧栈设计文档，仅作意图追溯，**不要按它实现**

旧栈代码可从归档标签 `archive/pre-rust-migration` 取回；迁移前的完整工作区快照是 `84342b5`，
`docs/legacy-specs.md` 中的行号均以该快照为基准。

#### 参与贡献

1.  Fork 本仓库
2.  新建 Feat_xxx 分支
3.  提交代码
4.  新建 Pull Request

#### 特技

1.  使用 Readme\_XXX.md 来支持不同的语言，例如 Readme\_en.md, Readme\_zh.md
2.  Gitee 官方博客 [blog.gitee.com](https://blog.gitee.com)
3.  你可以 [https://gitee.com/explore](https://gitee.com/explore) 这个地址来了解 Gitee 上的优秀开源项目
4.  [GVP](https://gitee.com/gvp) 全称是 Gitee 最有价值开源项目，是综合评定出的优秀开源项目
5.  Gitee 官方提供的使用手册 [https://gitee.com/help](https://gitee.com/help)
6.  Gitee 封面人物是一档用来展示 Gitee 会员风采的栏目 [https://gitee.com/gitee-stars/](https://gitee.com/gitee-stars/)
