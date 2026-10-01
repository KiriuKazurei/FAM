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
| [increment-8b-miller-columns.md](increment-8b-miller-columns.md) | 8b | 计划里"用 QColumnView（每列一个模型）"自相矛盾，按意图实现列链；`open()` 把追加写成替换被闸门当场抓住；侧栏的容量查询跳过网络盘（`GetDiskFreeSpaceExW` 会按 SMB 超时挂数秒）；一次"排序坏了"的怀疑用一行码点打印结案为两个前导空格的文件名 |
| [increment-8ab-review-findings.md](increment-8ab-review-findings.md) | 8a/8b 复查 | 严重一条：lambda 捕获列的**位置**，而深度上限从最前丢列 → 越界崩溃 + 排序排到右边那列 + 下潜静默失效。修完做了 A/B：同一条 `--self-check-deep 8` 修前 exit=127、修后绿。另有空 `m_chain` 析构、一条把暴露面说反了的注释、两处多余 `setLayout`、三样写了没人读的东西 |
| [increment-9-v1-is-gone.md](increment-9-v1-is-gone.md) | 9 | 拆掉 `fam-view` 与 v1 视图/监视入口（lib.rs −379 行，导出表里 `fam_view_*` 归零）。删的时候才撞出**v2 的记录里没有标签字段**；`abi-smoke` 搬到 v2 之后才第一次能测到 8a-1 那个 bug（A/B：无修复 exit=1 等满 10s）；337→320 条测试逐条对账 |
| [audit-log-is-append-only-and-priced.md](audit-log-is-append-only-and-priced.md) | 10 | 审计层一次请求一行：链按 session 而非文件（两进程同日追加会互相 fork），手写 SHA-256 的 64 个常量里凭记忆敲错 5 个、由已知答案向量当场抓住；实测单次追加 41µs(debug)/12µs(release)、一次会话 24 行 5.8KB，心跳 `dir.poll` 折算 3.4MB/小时所以永不记账；prune 注入让 `open_flat_dir` 涨到 3.90x，A/B 后把前缀判定从每条记录挪到每个目录，回到 1.06x 且顺带快过接手时的状态 |
| [increment-10-review-findings.md](increment-10-review-findings.md) | 10 复查 | 两处判据方向错到能反过来帮攻击者：`verify` 把"没有任何记录"判成干净（清空审计目录即退 0），而读路径 `read_to_string` 让**一个 0xFF 字节**同时废掉 `verify` 与 `tail`。另修七处：搜索作用域整个落在排除里时答"0 命中且已覆盖"（默认配置就活，因为审计目录就是那条排除）、相对 `audit_dir` 让两个守卫同时失聪、两个 close 替没发生的事作证、dropped 的原因被改写、封印按文件尾而非按会话数、时钟跨午夜回拨自己制造断链、注释收回一条做不到的承诺 |
| [tokens-are-human-only-and-tiered.md](tokens-are-human-only-and-tiered.md) | 11 | 令牌层：`fam-svc-auth` **不是** `fam-ffi` 的依赖（协议面上不存在这条路，21 个导出里相关名 0 个）；三等级是 42 组白名单矩阵而非 `>=` 梯子；明文只印一次、存 per-token salt 摘要，用 Python 独立复算才抓住"salt 是文本还是字节"这种自指验证；实测认证 1.6µs/次。自查用真命令对账抓出两处**命令自己那侧的拒绝没有审计行**（A/B：修前 13 行 / 修后 15 行），并演了一遍"手工写一条自洽 admin 行 = 无痕签发" |
| [design-sample-wears-ant-design-5.md](design-sample-wears-ant-design-5.md) | 样机（不占编号） | Ant Design 5 落到 Qt，**`apps/fam-shell` 一行未改**：独立的 `apps/fam-design` 只链 Qt。暗面官方只给到底基 `#141414`，其余按 antd 算法派生，而派生对不对由程序自己算 WCAG——当场抓出**暗色气泡白字压近白底 1.35:1**（`QToolTip{color:#fff}` 是我硬写的，antd 暗面气泡本来是亮的）；另有两处是 antd 默认值本身不过线（白字压 `#1677ff` 4.10:1、`#4096ff` 当正文 2.99:1），拆成 `primary` 与 `accentText` 两个 token 各管一职；输入框边框 1.41:1 与禁用文字 1.83:1 **保留原值但照测点名**，免除不等于不测 |
| [increment-11-review-findings.md](increment-11-review-findings.md) | 11 复查 | 严重的一条：`covers` 用的是路径身份，却仍让 `D:/notes/../../windows` 从一张 browse 票下面走出去（`PathKey` 折叠拼写、**不解析组件**）。另修九处，其中一处是**测试把缺陷钉成契约**（`assert_eq!(stale.tokens().len(), 2, "it still holds what it read")`）：并发守卫在"文件原本不存在"时整个关闭、同进程第二次保存必失败、临时文件名共用；手改 `revokedAt` 一个字母会让撤销失效，而下一次写把全表抹掉。驳回三条并给证据（时间差要有已绕过的立场才测得出；禁止 token_file 落在根内会让出厂配置拒绝启动） |
| [mcp-server-four-questions-before-the-disk.md](mcp-server-four-questions-before-the-disk.md) | 12 | 14 个工具接上 MCP：门一次问四步（tier → 范围 → 排除 → reparse），**排除那一步是还 11 的账**——A/B 把它关掉，一张 admin+`*` 的票当场读到 `mcp-tokens.toml` 全文（salt 与摘要）。代价全在 metadata 那一跳（本机 121–130 µs/次，门 246 µs，而判据本身 3 µs；648→384→246 两次分解）。四次 A/B 逐条还原，其中三处是**我的测试自己不合格**：价格表同一个变量印两遍还标着"含门"、删除守卫的断言不修也过（系统错误也失败）、空根守卫没有测试走得到（删掉守卫就 panic 在 `search_roots[0]`） |
| [increment-12-review-findings.md](increment-12-review-findings.md) | 12 复查 | 两条安全级：门的第四问走完链接就停下，而第二、三问**只在调用方的拼写上问过**——一张 admin 票可以 `inbox/st → state/` 读出凭据文件，也可以两个链接接力走出去；`fs_search` 只问了 `search_roots[0]`，另一个根里的文件名照样返回。还有一条**推翻我自己 note 的话**：`tags.db` 不是"因为非 UTF-8 被挡住"，刚建好的库大量 NUL 填充是合法 UTF-8，4096 字节整表外流。**"更一般的修法"跑出来更差**：注入 `database_path.parent()` 让六条搜索测试红成"0 行且覆盖完全"，于是退回按文件排。十三处改了、一条驳回并给理由（不强制握手是兼容性选择）、八处确认不修；另有两条新写的测试第一版就不合格（凭据根本不覆盖 `[0]`，新旧代码都绿） |
| [shell-wears-the-ant-design-baseline.md](shell-wears-the-ant-design-baseline.md) | 13 | 用户改口"保留 Ant Design 样式版本，接下来的开发基于它"，于是**产品壳吃下主题**（推翻增量"样机保持独立"那条约束）。动手前先量：壳里 `setStyleSheet`/`setItemDelegate`/`setObjectName` 是 **0 处**，行是模型拼的 `"name 1.2 KB [archive]"` 且**每个文件夹都印 0 B**。渲染路径整条换成 Fusion，所以 8a/8b/7 那些闸门**全部重跑**（1 对/2 对 reconcile 且选择保持、图标 18/18/18 pending=0、ipc retries=0）。两处值得记：我按常识加了 `WA_StyledBackground` 并写成道理，**手解 PNG 取点 245,245,245 之后把它撤了**（规则本来就生效）；复制四个文件最省事，但"基于这个版本"要求改 token 只有一处，于是有了 `apps/fam-ui` 静态库 + `RowRoles` 那条缝 |
| [split-panes-and-drag-to-move.md](split-panes-and-drag-to-move.md) | 14 | 分栏变成 `Split` 按钮开关的显示项（两个独立浏览面，共用搜索令牌就是披着布局外壳的数据竞争），**拖到另一列 = 真实搬文件**：先有 `fam-svc-fs` 让 agent 与人撞上同一句"永不覆盖"，再开 ABI 上第一个写入口 `fam_path_op`（版本仍报 2，因为门的是行布局而 `record_size` 才是那条缝）。载荷用私有 mime，代价是**与资源管理器互拖不通**。三条落放证据看的是磁盘上源文件还在不在：无修饰 `exists=0/1`、Ctrl `exists=1/1`、同列 `0 transferred, 1 already there`。两处是自己的问题：单栏窄**第一版诊断只猜对一半**（真因是 `Pane` 没装布局），而 `postDrop` 只发 drop 不发 enter 时**Qt 的默认接受让闸门假通过**（`taken=1` 而处理函数根本没跑） |
| [reconcile-moved-the-readers-row.md](reconcile-moved-the-readers-row.md) | 14d 附带 | 对账的前缀/后缀按"全字段相等"对齐，于是**一个只是长大了的文件夹**被报成"这一行走了、新的一行来了"：视图删掉读者那一行，`QItemSelectionModel` 把当前索引挪到下一行，而列链把这次移动读成"有人点了它"—— 链深 2→3、下一步拖放的目标从 `2024-合同` 变成 `photos`。改成按**身份**对齐、内容不同才发 `dataChanged`：`grown folder alone` 从 2 对信号 + 当前行 0→1，变成 **0 对** + 当前行不动。两条阴性对照（真删除、真改名）修复前后各花一对，证明没把删除的通道吞掉。顺带收回 8a-1 那句 `selection kept`：**"没重建整屏"与"位置没动"是两件事** |
| [packages-install-and-windeployqt.md](packages-install-and-windeployqt.md) | 15 | 补上 `install()` 与 windeployqt：三个 CMake 工程只发两个程序（`fam-ui` 是静态库，给它写安装规则会把"第三个工程"错当成"第三个交付物"），规则放一份是因为两遍 flags 会漂移。验收是**同一清空 PATH 条件下的一成一败**：装好的退 0 且跑完整闸门（`icons 18/18/18`、`ipc 107` 与 build 目录逐项相同），build 目录那个退 `0xC0000135` 且**连日志都没生成**。两处自记：我钉的 `--qtpaths` 建立在"windeployqt 靠 PATH 找 Qt"上，实测否掉就删了；`--no-translator` 这个开关根本不存在，是真实安装炸出来的。`opengl32sw.dll` 19.7MB **知道能省但故意不省**（这台机器造不出无 GPU 驱动的条件） |
| [tags-were-write-only-in-the-shell.md](tags-were-write-only-in-the-shell.md) | 16 | 标签的读与删**早就在 ABI 与 bridge 上，界面调用者 0 个**——写进去之后在产品里再也没出现、也删不掉。补一个 `TagDialog`（列 / 加 / 删）；显示形状是被 wire 语义定的：`remove_tags` 的 SQL 在 source 为 None、名单为空时**删掉这个文件的全部标签**，所以列表按名字合并（`contract (manual, auto)`），而"没选行"绝不能转成"删空的名单"。另两半**量了代价再不做**：行上画标签要 7510µs/窗口（批量 1194µs，6.3 倍），但否决它的是"绘制不查询"那条老规矩而不是倍数；按标签搜缺的是答案面——`find` 只给 `{path, tags}`，硬塞进 `FamListModel` 会让每个被标过的文件印 `0 B`，而 13 刚把这事当缺陷修掉。**所有线的断言都是绿的，只有截图暴露出新对话框没吃主题**（`QDialog` 没有 objectName → 落回桌面配色） |
| [tag-search-was-a-different-question.md](tag-search-was-a-different-question.md) | 17c | 按标签搜不是名字搜索的子集：名字索引被问 `tag:合同` 会答出**名字里有"合同"的文件**还报完整，所以路由在 `covers` 之前问新的能力位 `handles_tags()`。标签库作为一个搜索后端，命中补上 `metadata` 以 `SearchHit` 出——与名字命中同形，行画法、分页、作用域全是现成那套；门的过滤白拿（agent 面测试钉住"根外的标签不外流"）。壳的词表筛选栏是 `fam_tag_vocabulary` 的第一个界面调用者：box 是查询唯一活着的地方，chip 写 box、再从 box 读回勾选——重建的静默与点击的发声分开，否则追自己的尾巴。机械对比抓到 `{{primaryBg}}` 被引用却不在替换表里，Split 选中态的底色一直被静默丢弃；第一版筛选栏截图抓不到选中态（找错了父），线的断言依旧全绿——16 的教训第二次应验 |
| [tag-debts-and-the-one-name-rule.md](tag-debts-and-the-one-name-rule.md) | 17d | 标签的八笔账一次清：改名与归并是同一句话（`UPDATE OR IGNORE` 跳过的恰好是重复，DELETE 收走的恰好是它们），`fam_tag_merge` 上 ABI、`tags_merge` 成第 15 个 MCP 工具（无路径动词的 tier 问题从此要单独有人问）、`fam tag merge` 与筛选栏右键同源；索引换覆盖列——`(tag, source)` 从没有读在用，`(tag, path_norm)` 让词表快 4.2 倍（5160→1234µs）、搜索快 15%，前后同机同构建各量一遍；`fam store prune` 收"路径没了"的行（干跑默认），abi-smoke 真被这笔债咬红过一轮；行上 chip 点得动（命中测试与绘制同源），agent 写的标签靠 stat 库与 WAL 的拍子进筛选栏；删除两段式、键盘路径、"一名两源"的显示全部有了断言。`fam_find` 作为无人调用的旧形状被收回（导出 24→25：−1+2） |
| [notes-were-never-on-the-human-face.md](notes-were-never-on-the-human-face.md) | 18 | 用 16 的尺子量出备注：store 在 v2 就有、agent 在 12 就能用，ABI/bridge/界面全是 0——agent 能写、人永远看不见。三段线补齐（`fam_note_*` 三条导出 25→28、bridge、`NoteDialog` + `Note…` 按钮），store 的语义定死界面的形状：空写必须拒绝且不得读成清除（拒后原文还在有断言）、8 KiB 按 UTF-8 本地先量、"第二次清除如实说没有"（`clear-again=0` 是期望值不是失败计数）。CLI 补 `fam note` 三件与 agent 同句。残留：行上没有"有备注"的记号（需要 17b 那一整套的第二遍） |
| [a-note-you-cannot-see-is-a-note-you-do-not-have.md](a-note-you-cannot-see-is-a-note-you-do-not-have.md) | 19 | 18 的"中"级残留：行上看不出谁带备注。`fam_notes_for_paths` 作为 `fam_tags_for_paths` 的孪生（同形同约、`note.fetch` 心拍不记账），模型一条独立缝合点共享跳过条件与到期，行右缘 6px 圆点、梯子首位让位——不是 chip，否则会被读成"有个标签叫 note"；对比度一对取容器/悬停/选中三种行底的最不利者现算（浅 5.50/深 5.01，非文字 3:1 不豁免），受门槛 17 对全过。`marker: shown=1 gone=1` 证明记号随数据来去；abi-smoke `marker=1` 并入旧闸门。一课：半成品分两次落树，构建拦住编译错、盘点收拢双份实现——"写了两遍"是"两份将来会漂移"的事实 |
| [the-undo-the-window-can-own.md](the-undo-the-window-can-own.md) | 20 | 14 号账的"中"级残留：日志回放与事务之间还有第三条路——本窗口自己的传输，逆信息就在 `Transferred{from,to,action}` 里，移动的逆是移回、复制的逆是删掉那份重复。删除原语收进 fam-svc-fs（`remove_path`，句子与 MCP 逐字一致，`fs_delete` 改为委托），`fam_path_delete` 上 ABI（29→30），`Ctrl+Z` 弹栈、失败回栈重试；self-check 用两发真 Ctrl+Z 在磁盘上断言 `copy-undone`/`move-undone`。一课：`secondRefused` 存的是"删除成功"，名字与语义相反时红绿都不可信 |
| [a-copy-has-a-middle-now.md](a-copy-has-a-middle-now.md) | 21 | 14 号账"最实际的坑"：大文件传输冻结 GUI。复制拆成 1 MiB 一块（首块前先问一次——取消的粒度就是块的粒度，"cancelled"意味着什么都没到达），任务住核心：`fam_transfer_begin/poll/cancel`（29→33 里的 30→33），终态只答一次、答完退役，审计两行（begin 在开始、end 在首次观测到终态的那一拍）；壳的拖放改入队、拍子轮询进度、Esc 发取消、done 才入撤销栈。一课：`const io::Error` 哨兵不成立，`CopyStop` 枚举让读失败与说停在类型上可分 |
| [the-column-lights-when-it-will-take-it.md](the-column-lights-when-it-will-take-it.md) | 22 | 14 号账最后一个"中"：拖放悬停无反馈。高亮跟着语义走——落点是列的文件夹，所以是整列边框亮起而非行间插入线；`dropHover` 属性在 enter/move/leave/drop 四事件里维护，外来载荷与离开同一步归 false；对比对"拖放描边/容器"受门槛 3:1（浅 4.10/深 4.49）。self-check 发真 enter 断言属性、当场抓帧（该状态只存在于一次拖的中间）、再发真 leave 断言熄灭。14 号账三个"中"至此清零 |
| [project-audit-after-increment-22.md](project-audit-after-increment-22.md) | 22 审计 | 全项目文档与代码是否说同一套事实：38 测试二进制、clippy 0 错、fmt 干净、self-check light/dark 断言全 1（对比度 18 对 0 不过）、abi-smoke 十二标志两连 ok、model-smoke ok；事实核对表（导出 33、工具 15、schema v3、fs 三原语）逐条查文档偏差。修正：英文 README 从增量 16 同步到 22（14→15 工具、13→18 对、补齐五篇功能段），sqlDesign.md 文首标注"旧栈设计意图，PostgreSQL 从未实现"，主 README 补 33 导出与文档指引 |
| [the-bin-and-the-redo.md](the-bin-and-the-redo.md) | 27/28 | 23/24 与 25 各自的最后残留：键盘删除改 QFile::moveToTrash（人的 Delete 进回收站可恢复，agent 的 fs_delete 硬删除契约原样——风险不对称的现实分歧写进注释），句子报人删的原名而非 bin 内地址；重做栈与撤销栈对称——undone 条目进 m_redoable，Ctrl+Shift+Z 重跑前进传输，完成回撤销栈、失败回重做栈，完成分支绝不推新条目；新 drop 截断重做半边。self-check 的 redo 回合四断言：撤销后文件在回退端、重做后回到前进端、副本重新出现在两端 |
| [the-bin-takes-folders-too.md](the-bin-takes-folders-too.md) | 29 | 27 号账残留：目录进回收站未断言。moveToTrash 的 Windows 实现目录与文件同权——bin 可恢复，整目录进 bin 正是人对 Delete 的期待，agent 的硬删除守卫（非空按名拒绝）不必照搬。self-check 键盘步骤追加目录轮：新建含内文件的目录、等 reconcile 上行、真两下 Delete、断言目录与内文件都从树里消失；bin 内地址成对入 qInfo |
| [tag-grammar-learns-or-and-not.md](tag-grammar-learns-or-and-not.md) | 30 | `tag:` 语法学了 OR 与排除：`,` 是 AND、`||` 是 OR（逗号绑得更紧，`a,b||c` = (a AND b) OR c）、`!` 排除（两种拼写都收），全部半角——全角 `，｜｜！` 是中文标签名的普通字符。SQL 三段各归其位：required 进 HAVING 计数、OR 组各一条 SUM>=1、排除是文件属性走 NOT IN 子查询；`match_all` 删除（required 天然 ALL）。`tag:!a` 单独成询合法——"全部带标签的文件排除带 a 的"是真问题。自记：OR 组第一版并进平列表被测试拦下——(a AND b) OR c 不能写成平 AND |
| [the-keyboard-finally-edits.md](the-keyboard-finally-edits.md) | 23/24 | 审计点名的"从未入账"第一条：壳内没有文件删除与改名。键盘事件住在视图、语义决定在窗口——三个信号（delete/rename/open）沿 chip 同一条中继链上达；删除两段式（状态行武装，硬删除无回收站）、改名是移动（走 pathOp，成功入撤销栈）走一行对话框、打开交给平台（无关联有句）。两个自伤的坑：`QDir` 构造自文件路径把文件当目录（os error 3），ShellExecute 内部泵事件让后续定时器提前开火（20 号账硬币的背面）——慢操作放步骤最后、断言读句子不读文件存在性 |
| [the-undo-rides-the-beat.md](the-undo-rides-the-beat.md) | 25 | 21 号账残留：撤销的移动回退是全壳最后一个同步大操作（跨卷移回 = 再冻一次）。分支本就不对称——复制撤销是一次 unlink（保持同步），移动撤销是一次传输（改走任务）：弹出的条目随待定任务入队，done 即撤销的句子、失败/取消回栈重试，且完成分支绝不推新条目（否则 Ctrl+Z 撤销撤销）。self-check 的第二次 Ctrl+Z 后等队列清空再断言磁盘 |
| [the-installed-folder-stands-alone.md](the-installed-folder-stands-alone.md) | 26 | 15 号账的验收方式在当前构建上重跑：cmake --install 两包，PATH 只剩 Windows 系统目录，self-check-mutate 退出 0、断言全 1（icons 21/21/21、retries=0、对比度 18 对）。自记一处：对话框截图的 listed=2 是泵内定时器竞态（grab 步在 tagsForSelfCheck 的泵里提前跑，拍到了 dual 块的中间态）——步骤间的时序隔离靠间隔，竞态类是这套时间线的已知形状 |
| [the-undo-the-window-can-own.md](the-undo-the-window-can-own.md) | 20 | 14 号账的"中"级残留：日志回放与事务之间还有第三条路——本窗口自己的传输，逆信息就在 `Transferred{from,to,action}` 里，移动的逆是移回、复制的逆是删掉那份重复。撤销原语收进 fam-svc-fs（`remove_path`，句子与 MCP 逐字一致，`fs_delete` 改为委托），`fam_path_delete` 上 ABI（29→30），`Ctrl+Z` 弹栈、失败回栈重试；self-check 用两发真 Ctrl+Z 在磁盘上断言 `copy-undone`/`move-undone`。一课：`secondRefused` 存的是"删除成功"，名字与语义相反时红绿都不可信 |


