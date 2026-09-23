# 问题分析记录

重建过程中每发现一处"改之前是错的"或"改之前有代价"的问题，就在这里留一篇。
目的不是文档量，而是让半年后接手的人知道**当时怎么确认的、留下什么没修**。

每篇固定四段，顺序不可换：

1. **描述问题** —— 现象、涉及的代码位置、旧实现长什么样。只写事实，不写判断。
2. **分析问题** —— 为什么会错、影响面多大（实测计数，不用"若干"）、排除掉的候选
   方案及否决理由。危害有方向性时必须写清方向（例如"合并两个文件"与"拆成两个键"
   不是同一级别的错误）。
3. **解决方式** —— 最终落地的做法，带代码片段与 `文件:行号`。
4. **解决方式效果评估** —— 三件缺一不可：
   - 正面证据：哪条测试、哪个命令、哪个数字。写测试名，不写"已验证"。
   - 代价：性能、内存、复杂度、新增的维护面。测过就写实测值，没测就写"未实测"。
   - 残留风险与局限：明确列出**没解决什么**、为什么这次不解决。

约定：

- 数字必须来自一次真实运行，并注明是实测还是折算；折算要给公式。
- 优于基线的观测值若来自缓存变暖之类的外部因素，要显式声明不计为收益。
- 与代码位置绑定的引用用 `文件:行号`，代码移动后允许失效，但新提交若改到相关
  文件，顺手核对一次。

| 记录 | 增量 | 一句话 |
| --- | --- | --- |
| [pathkey-identity-fold.md](pathkey-identity-fold.md) | 1 | 旧路径键会把两个不同文件折成同一个身份，改为 UTF-16 键 + 仅折叠 ASCII，并记录字符串形态贵了 3.5 倍 |
| [partial-directory-cache.md](partial-directory-cache.md) | 1 | 被取消的目录读取会以完整表的身份长期留在缓存里，改为枚举层分别报告"读完/取消/截断"并由缓存层执行门槛 |
| [test-name-leak.md](test-name-leak.md) | 1 | 测试辅助函数签名过窄，逼出一处 `Box::leak`，放宽签名后泄漏与传染一起消失 |
| [per-entry-syscall-premise.md](per-entry-syscall-premise.md) | 2 | "每条目录项两次 syscall"被实测否掉（两种读者同为 1.3µs/条目），换 Win32 枚举记录买到的其实是 attrs、长路径控制权与单层原语 |
| [row-generation-vs-load-attempt.md](row-generation-vs-load-attempt.md) | 3 | 缓存代号该描述"行"还是"尝试"：失败的刷新与放弃的读取都不该让内容看起来变了 |
| [backoff-and-byte-accounting.md](backoff-and-byte-accounting.md) | 3 | 状态被换走时附属计数没跟着搬：退避窗口翻倍差一、tries 丢失导致不退避、失败保留行时字节漏记 |
| [notify-watch-loss-is-invisible.md](notify-watch-loss-is-invisible.md) | 4 | 缓冲区溢出与"监视被悄悄撤销"都不可观测：改按目录写时间对账，并用真后端把"删除被监视目录"做成端到端正向证据 |
| [event-carries-no-target-type.md](event-carries-no-target-type.md) | 4 | 事件只给名字不给身份：翻译处一次算清父目录与 UTF-16 名，改名不配对，`Target` 保留 `Unknown` 且不许猜 |
| [watch-sweep-costs-a-path-lookup.md](watch-sweep-costs-a-path-lookup.md) | 4 | 一次写时间采样实测 190µs（三个 API 等价、与冷热无关），"探针比重读便宜两个量级"被否，预算改按时间 |
| [incremental-patch-cost.md](incremental-patch-cost.md) | 5 | 计划里"定向属性查询改一行"实测约一条就回本：只有零 I/O 的消失类事件就地改行，其余交给重读 |
| [v2-wire-and-windowing.md](v2-wire-and-windowing.md) | 5 | 行数据从 JSON 换成定长缓冲 + 窗口化模型：`open` 239µs、删 1000 个文件 3 对信号；两处只有真机能暴露的协议 bug |
| [window-show-costs-a-second.md](window-show-costs-a-second.md) | 5 | 首屏那 1.1 秒花在 `QWidget::show()`，不在数据通路：核心启动 8ms、行到位 28ms，慢的地方被定位 |
| [search-hit-path-was-folded.md](search-hit-path-was-folded.md) | 5 | 搜索行的路径取自折叠身份，目录会被印成小写：`SearchHit` 改带逐字整路径、`scope` 改 `PathBuf`，provider 自己折一次只用于比较 |
| [cancelled-search-had-no-state.md](cancelled-search-had-no-state.md) | 5 | 被取消/到 limit 的搜索既不是 FAILED 也不是永远 LOADING：`settled` 与 `complete` 分开，READY + INCOMPLETE 表达"终态但只是部分" |
| [test-helper-dangling-cstring.md](test-helper-dangling-cstring.md) | 5 | 测试辅助把 `CString` move 进 `map_or` 闭包，C 侧读到已释放内存，表现成 `unknown kind ""` 与 0 行——它不总是错，所以最难查 |
| [benchmark-floors-are-machine-state.md](benchmark-floors-are-machine-state.md) | 5 | 三条地板不可复现（写下它们的提交今天自己也只测出 61µs vs 记录的 13µs），A/B 对照后重录；补记两处工具修正：选批按 p50 而门槛只看 min、`--write` 会删掉没跑的用例的地板 |
| [scoped-search-was-empty-and-said-so.md](scoped-search-was-empty-and-said-so.md) | 6 | 配置根被当成栅栏：`--in`/`within` 指向根外的目录时返回 0 行还报"已覆盖"。改为指名的子树**替换**根列表，并把两条排除规则在边界处分开问（绝对前缀看整条路径，目录名只看边界以下） |
| [increment-6-what-this-machine-can-prove.md](increment-6-what-this-machine-can-prove.md) | 6 | Everything 客户端的账：哪些代码真跑过（真 DLL 绑定与退化、真卷扫描、真 `drain` 分页/取消/复核），这台机器上永远验证不到的五条各错了会怎样，以及拆掉 v1 搜索面时随 v2 走齐的两处语义 |
| [increment-6-review-findings.md](increment-6-review-findings.md) | 6 复查 | 全面复查的账：修 9 处（含一处实测的跨平台编译断裂、一处 unsigned 剪裁计数可绕回、一处让每次状态轮询白跑两趟 IPC 的协议位），驳回 1 条"limit 不生效"的高危误报，10 处确认不修并标归属 |
| [flag-loss-was-a-latch.md](flag-loss-was-a-latch.md) | 6 复查欠账 #1 | `FLAG_LOSS` 是全 core 单调计数而 v2 只比 0：丢过一次事件后每次浏览都永久报"events dropped"。改为每视图基线（打开时记、成功重读抬），v1 drain 的 `swap(0)` 换成自己的记账以免清零把 v2 静音 |
| [shell-icons-never-open-a-path.md](shell-icons-never-open-a-path.md) | 7 | 壳的图标层：第一层每行一次哈希（`SHGFI_USEFILEATTRIBUTES` 不打开路径），第二层仅白名单走真实路径；STA worker 上 `HICON→QImage→DestroyIcon`，重画只打物化窗口。闸门是提取/销毁计数相等（9/9/9 两次复现） |
| [increment-7-review-findings.md](increment-7-review-findings.md) | 7 复查 | 图标层的账：修 4 处（第二层键无上限会随目录长、`SHGetFileInfoW` 返回值没查会把错 glyph 缓存到进程结束、一段说反了实测结论的注释、闸门缺"worker 空闲"的证据），驳回零条，7 处不修并标归属（含增量 8 必须同时上 `SizeHintRole` 这条约束） |
| [watched-folders-never-showed-new-files.md](watched-folders-never-showed-new-files.md) | 8a-1 | 打开的目录里新文件永远不出现：事件放不下新行、对账又因"事件已解释"不再重扫、没人替 stale 的表约读——旧测试还把"要重开一次"写成了契约。核心去约那次读（测试去掉修复就等满 10s 才输），壳把随之到来的 resync 摊成 1 对信号而不是扔掉整屏 |
| [increment-8a-search-and-states-wired.md](increment-8a-search-and-states-wired.md) | 8a | 五种状态各配一句人话（`EMPTY` 是"没读过"不是"是空的"）、`available` 每次搜索重读、搜索不再关目录且作用域明说（改成悄悄收窄是回归）、壳侧 IPC 计数 `retries=0`；日志顺手抓出一个"一个变量服务两个 token"的别名 bug |
