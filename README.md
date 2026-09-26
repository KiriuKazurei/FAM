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
| `crates/fam-svc-store` | SQLite 标签存储 + 按路径的备注（`tags` 与 `notes` 两张表，schema v2） |
| `crates/fam-svc-enumerate` | 单层目录列举（Win32 后端 + 可取消、可超时的读者） |
| `crates/fam-svc-walk` | 递归遍历，以及"遍历"这一个名字检索后端 |
| `crates/fam-svc-audit` | 审计日志：追加式 JSONL、一天一文件、按会话串 SHA-256 哈希链 |
| `crates/fam-svc-auth` | MCP 令牌的签发与判定：三等级 × 动词白名单、工作区路径范围、加盐摘要存储 |
| `crates/fam-svc-mcp` | 给 agent 的那一面：JSON-RPC 2.0 分行协议、14 个工具、门（等级 → 范围 → 排除 → reparse） |
| `crates/fam-svc-fs` | 移动与复制的**唯一一份**实现：`transfer(from, to, Move\|Copy)`，规则（永不覆盖、目录不复制、跨卷目录不做）住在这里，agent 与人都撞同一句话 |
| `crates/fam-svc-index` | 懒目录索引（定长表 + 缓存 + 按目录就地改行）、视图窗口，以及搜索后端的路由与范围规则 |
| `crates/fam-svc-everything` | Everything 客户端：运行期绑定、状态探测、查询编译、按页取结果 |
| `crates/fam-sched` | 监视事件 → 有界队列 → 批量合并的行变更 |
| `crates/fam-ffi` | 对 C++ 暴露的 ABI（`famcore.dll`）：行是定长缓冲 + 窗口化，控制载荷（stats、标签、find、配置）是 JSON。v1 的视图与监视入口已在增量 9 删除 |
| `crates/fam-bench` | 度量台与地板门槛 |
| `apps/fam-cli` | `fam` 命令行（无头测试台） |
| `apps/fam-mcp` | MCP stdio 服务器进程：读一行答一行，凭据只从 `FAM_MCP_TOKEN` 环境变量来 |
| `apps/fam-ui` | 设计系统的**唯一一份**：Ant Design 5 的 token → QSS，以及那张手绘的文件行；只链 `Qt6::Widgets`，不链 `famcore` |
| `apps/fam-shell` | Qt6 Widgets 界面（产品面）。吃 `apps/fam-ui` 的主题，渲染走 Fusion，`--theme light\|dark\|system` |
| `apps/fam-design` | Ant Design 5 的**设计基准**：与壳共用 `apps/fam-ui`，只链 Qt；`--shot`/`--theme` 出图并把实测对比度表打到 stdout。产品功能不再长在这里 |

工程处于**资源管理器重建**之中：浏览与搜索都已改为懒目录索引 + 流式虚拟化，
名字检索按"能答全才问"在 Everything 与遍历之间路由，两者共用同一套范围规则。
界面已经是 Miller 列：一列一个目录、一个 v2 token、一个模型，列头按列各管各的排序，
左侧是驱动器栏；每行的 shell 图标走"扩展名→系统图像列表"，不在绘制路径上打开任何路径。
分栏是工具栏上那个 `Split` 按钮开关的**显示项**：收起时是一整幅的浏览面，
展开是两个各自独立的浏览面（各自的链、各自的结果列表、各自的搜索令牌）。
**把一行拖到另一列就是真的搬文件**：直接拖 = 移动，按住 Ctrl 拖 = 复制，
目标恒为该列显示的文件夹、名字不变、同名就拒（核心那句 `a transfer never overwrites`）。
载荷是私有类型 `application/x-fam-paths`，所以与资源管理器互拖**不通**——
这是刻意的：第二条搬文件的路会绕开 `fam_path_op`、绕开"永不覆盖"、在审计里留不下任何东西。
标签在人这一面**看得见也删得掉了**：`Tags…` 打开一个对话框，列出这个路径上的标签、能加能删。
列表按**名字**一行一条、把来源写在后面（`contract (manual, auto)`），
因为 `fam_tag_remove` 删的是名字——连带所有来源；而名单为空时那句 SQL 会删掉这个文件的**全部**标签，
所以"没选行"在操作层就被挡住，不会发出去。
样式是 Ant Design 5 的 token 集，**产品壳与设计基准共用同一份实现**（`apps/fam-ui`），
渲染走 Fusion 而不是原生 Windows 样式——sheet 沉默处原生样式会自己画渐变与焦点环，与 token 打架。
文件行由委托手绘：名字带 demi-bold、尺寸三级色、类型是 Tag 芯片，
**文件夹不再印 `0 B`**（那字段的零是"没写"，不是"没有大小"）。
可读性是量出来的不是抄来的：`Theme::contrastReport()` 现算 WCAG 比，
壳的 `--self-check` 日志里就带着这张表（13 对受门槛、两处免除照点名）。
每一次被 core 回答的请求都会在审计日志里留下一行（追加式 JSONL，一天一文件，链按会话串），
日志写不出来时 core 直接拒绝启动；而**读这份日志的路只有 CLI**——
`fam audit tail|verify|stats`，`famcore.dll` 的导出表里没有任何读接口。
代价是实测的：单次追加 41µs（debug 构建），一次完整浏览会话 24 行 / 5.8 KB。
agent 的凭证已经在同一侧：**签发、撤销、轮换只有人手那条面**（
`fam mcp token add|list|revoke|rotate`），`fam-svc-auth` 不是 `fam-ffi` 的依赖，
所以 ABI 面上不存在任何 token/permission/audit 的入口——不是运行时检测出来的，是链接期就没有这条路。
给 agent 的那一面（`apps/fam-mcp`，stdio 上一行一条 JSON-RPC）今天真的存在了，
它同样管不了凭据：14 个工具的名字是快照测试钉住的，
`token` `grant` `revoke` `rotate` `audit` `config` `secret` `credential` `mcp` 这些词出现在
任何一个名字里就会让测试红。三个等级是动词白名单而不是可以比较大小的阶梯，
谁都不能越出自己那几个工作区目录；未解析的写法（带 `.` 或 `..`）一律算越界，
授权根之下的 junction 也不算在里面，程序的审计目录与凭据文件本身对 tools 不可达。
明文只在 `add` 的那一次打印，文件里只有每令牌独立 salt 与摘要，单次认证实测 1.6µs；
每次工具调用（包括被拒的）在审计日志里留下一行，一次带门的请求实测 246µs（门）/ 750µs（整条）。
旧栈的规格见 `docs/legacy-specs.md`，逐增量的取舍见 `docs/notes/`。

#### 构建
CLI 与测试用默认工具链：

```bash
cargo test --workspace
cargo build --release -p fam-cli
```

给 agent 的那一面是 `apps/fam-mcp`，一个 stdio 上的进程，不需要 Qt：

```bash
cargo build --release -p fam-mcp
fam mcp token add organizer --tier edit --workspace "D:/inbox"   # 明文只印这一次
FAM_MCP_TOKEN=fam_xxxxxxxx_... \
  target/release/fam-mcp.exe --config <path/to/config.toml>
```

**没有 `--token` 这个旗标**：命令行上的令牌对本机每个进程可见（一行 `Get-CimInstance Win32_Process`
就抄走了），环境变量只对这个进程可见。没有凭据时这个进程**拒绝启动**并且说清缺的是什么，
而不是起一个"连不上"的服务；日志与那句拒绝都走 stderr，stdout 上只有协议。
`docs/notes/mcp-server-four-questions-before-the-disk.md` 记着 14 个工具的边界、
每次请求的实测代价，以及没解决的那一串（没有 HTTP 传输、没有区间读、没有速率限制）。

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

在**自己的开发机**上跑，需要三份运行时同时在 PATH 上：Qt 的、MinGW 的、以及放着 `famcore.dll` 的
GNU 产物目录。

```bash
PATH="<Qt>/6.11.2/mingw_64/bin:<Qt>/Tools/mingw1310_64/bin:target/x86_64-pc-windows-gnu/debug" \
  apps/fam-shell/build/fam-shell.exe --watch <目录> --self-check <截图.png> --self-check-mutate
```

**交给别人试用是另一件事，走 `cmake --install`**：它把可执行文件、它链接的那份 `famcore.dll`、
以及 `windeployqt` 判定的 Qt 运行库（平台插件、样式、MinGW 运行时）放进同一个文件夹，
不依赖上面那三行 PATH。

```bash
cmake --install apps/fam-shell/build --prefix dist/fam-shell
cmake --install apps/fam-design/build --prefix dist/fam-design
```

装完的文件夹里只有 Windows 系统目录在 PATH 上也能起——这正是这条命令要买的东西，
所以它也是这条命令的验收方式：

```cmd
set PATH=C:\Windows\system32;C:\Windows
dist\fam-shell\fam-shell.exe --config <配置.toml> --watch <目录> --self-check 截图.png
```

两个 Qt 面共用同一条规则（`apps/fam-ui/cmake/FamDeployQt.cmake`），因为复制两遍的打包规则会漂移，
而漂移是在别人机器上而不是在这台开发机上暴露的。`fam-ui` 自己是静态库，没有安装规则：
它的目标文件已经在可执行文件里——所以树里有三个 CMake 工程，要发的程序仍然只有两个。
两个控制台目标（`abi-smoke`、`model-smoke`）**不安装**：它们是读这份源码的闸门，不是产品。

Debug 产物的体积记在账里：壳 125MB / 23 个文件，其中 `famcore.dll` 51.2MB、
`opengl32sw.dll` 19.7MB（后者是唯一一处"知道能省但不敢在这台机器上验"的裁剪，理由见那篇账）。

`--self-check` 是无头闸门：渲染窗口、写截图与同名 `.log` 然后退出。加上
`--self-check-mutate` 会**在 `--watch` 那棵树里写两个文件再删掉**：
一个用来跑"新建 → 重读 → 按窗口 diff"与"删除 → 剪裁"两条通道，
另一个用来跑拖放那三条（开分栏 → 把两栏分别指向树里的两个目录 →
无修饰落下 = 移动、Ctrl 落下 = 复制、落回自己所在的列 = 不发调用），
日志里逐条写明落下之后**磁盘上**哪个路径还在、哪个不在——
"移动"与"复制"唯一可分辨的观测就是源文件还在不在。
同一次跑还走一遍**标签往返**（读回 → 加 → 读回 → 试删空 → 删），并把那个对话框单独抓一张
`<截图名>.tags.png`——线全绿不代表新表面吃到了主题，第一张图里它就没吃到。
最后把图标提取/销毁计数与 IPC 调用计数打进日志，这两个计数就是"无 HICON 泄漏"和"轮询不贵"的读数形式。
**进程中途被杀时这两个文件会留在你指给它的那棵树里**，名字都是 `fam-self-check-*.txt`。

抓界面截图验证时用原生 `windows` 平台插件——`QT_QPA_PLATFORM=offscreen` 没有字体库，
所有字形（含 ASCII）都会渲染成豆腐块。

#### 软件架构（旧栈，重建之前的形态）

> 这一节记的是**被替换掉的那套**，保留是因为它解释了重建在避开什么坑；
> 今天的架构看上面的 crate 表与 `docs/notes/`。当前实现与这里相反的一点是：
> 索引确实自建了（`fam-svc-index`），Everything 只是其中一个可选后端。


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
