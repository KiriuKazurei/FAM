# 增量 6 的账：这台机器上到底执行过什么，以及五条永远执行不了的

- 增量：6（6a–6d）
- 提交：`06d5b6a`（假后端 + 路由）、`e614809`（范围规则）、`022ff72`（拆 v1 搜索面）
- 涉及：`crates/fam-svc-everything/`、`crates/fam-svc-index/src/router.rs`、
  `crates/fam-svc-walk/src/search.rs`、`crates/fam-ffi/`、`apps/fam-cli/`

## 1. 描述问题

增量 6 交付了一个真的 Everything 客户端：运行期绑 `Everything64.dll`、解析 16 个导出、
探测状态、编译查询、按页取结果、按卷决定该不该路由。**但这台机器上没有 Everything 服务**，
所以"真的那一半"从来没收到过一行真数据。

需要说清楚的是哪些代码**被跑过**、哪些只是**被读过**。前者是证据，后者是推断，
混在一起写就成了"已经验证"。这篇是这一增量的账。

顺带两件同批发生的事也要记账：v1 的搜索面被拆掉（两处语义随 v2 走齐，不是无声改变），
以及一个只在有服务的机器上才会现形的结构性后果（后端中途死掉，行收不回来）。

## 2. 分析问题

**为什么"没有服务"不等于"没法测"。** `Everything` 这条路的代码分两层：
一层是**关于服务的假设**（它的查询语言、它的回包形状、它的分页语义），
一层是**拿到行之后做什么**（翻页算术、limit 记账、取消、类型分桶、逐行复核、
路由与范围）。第二层是普通代码，可以在没有任何服务的情况下用真的那一层跑。
之前它没被跑，是因为能跑它的唯一入口（`EverythingBackend`）是一句 "not implemented"。

**为什么不能从零造一个假 provider。** 那样测的是一套凭空的形状：假的自己写一遍分页循环，
真的那条循环就永远没被测过——而 bug 恰恰长在循环里（`offset += got`、短页判结束、
`sink` 返回 false 时 `complete` 该是什么）。所以假的是**最底下那一层**：
`FakeEverything` 只交出 `Vec<RawRow>`，上面全部走真实的
`search::drain`/`to_hit`（`crates/fam-svc-everything/src/testing.rs`）。

**假的一端必须诚实到哪个份上。** 它**不看编译后的查询**，也不按 scope 过滤自己的语料——
因为真实的 Everything 索引整卷、不知道用户排除过什么。一个会自己收窄的假货正好把
`Scope` 要补的洞遮起来（那个洞真被遮住了，见
[scoped-search-was-empty-and-said-so.md](scoped-search-was-empty-and-said-so.md)）。
代价写在下面第 4 节第 1 条：这是这套测试**结构上**测不到的一类错误。

**分页算术能不能信？** `page()` 里 `count = GetNumResults().min(want)`，
如果 `GetNumResults` 返回的是**总匹配数**而不是本页行数，最后一页就会读越界、
`drain` 会认为列表没完而继续翻页。这一条在这台机器上**问不到运行期**，
但问得到规格：`third_party/everything-sdk/include/Everything.h:148-152` 同时有
`GetNumFileResults`/`GetTotFileResults`、`GetNumResults`/`GetTotResults` 两对——
`Num*` 是本次取回数、`Tot*` 是总数，命名成对，取的是前者。加上 `.min(want)` 的钳制，
以及生产路径上每个请求都带非零 limit（`search_begin` 把 0 折成 `DEFAULT_SEARCH_LIMIT`，
CLI 折成 200），`drain` 只有三条出口且其中两条必然终止。

## 3. 解决方式

### 3.1 用真循环 + 假数据源跑第二层

`FakeEverything` 实现 `SearchProvider`，`search` 里做的是真 provider 做的同一串：
拒绝空请求、`compile(request)` 为 `None` 就拒绝（否则会把整个索引拉回来）、
然后把 `fetch` 交给 `drain`。`breaks_at(offset)` 让它在某一页之后开始报错，
用来演"服务中途没了"。路由侧新增 9 条，全部用真 `drain`：

| 钉住的事 | 测试 |
| --- | --- |
| 服务在位时**只有**索引的行到达，枚举那次根本没跑 | `an_up_index_answers_and_enumeration_is_not_consulted` |
| 2 页 + 7 行的列表完整到达且自称完整 | `a_result_list_longer_than_one_page_arrives_in_full` |
| limit 短于列表 = 终但不完 | `a_ceiling_shorter_than_the_list_is_final_but_not_complete` |
| sink 返回 false 就到此为止 | `a_sink_that_stops_takes_no_more_rows_than_it_asked_for` |
| 取消在**页与页之间**被理会，已发出的一页整页交付 | `a_stopped_signal_is_honoured_between_pages_and_is_not_an_error` |
| 中途死掉的后端：行收不回，回退再加，`provider` 只说谁跑完 | `a_service_that_dies_partway_keeps_its_rows_and_adds_the_fallback_s` |
| 覆盖判定用真规则而不是"就是不说能答"的桩 | `an_index_that_cannot_cover_every_root_is_not_asked`、`a_scope_on_a_volume_the_index_does_not_hold_goes_to_enumeration` |

原来那两个手搓的桩（`FakeProvider`、一说不能答就 panic 的 `PartialProvider`）被换掉了，
断言从"没被问"变成可观察的"它那一行没到"。假的一端自己也有 4 条测试——
其中"短页表示列表读完"那条是**假的契约**在被检查：一个永远回满页的假货会让 `drain` 死循环。

### 3.2 拆 v1 的搜索面

`SearchBackend`/`SearchQuery`/`SearchResults`/`WalkBackend`/`EverythingBackend`/`SearchService`
删除；`fam-svc-walk/src/search.rs` 从 543 行降到 302 行（生产代码只剩 `WalkProvider` 的
84 行，其余是它的测试）。三个消费者改问同一个 `Arc<SearchRouter>`：
CLI、v1 的 `fam_view_search`、`fam_backends_json`。
路由器多三个由配置构造的入口（`from_config`/`walking`/`indexed`），
因为两个前端各抄一遍同样的五处决定（顺序、`load(roots)`、`scan_timeout`、根列表、排除规则）
正是 `--in` 那个洞的来源。`Insisted` 包装器从 CLI 搬进路由器：
`--backend everything` 要的是**那个后端自己的理由**，所以 `is_available` 和 `covers`
两边都得顶住，否则它会在开口解释之前就被绕过去。

**两处语义随 v2 走齐**，都是有意的改变而不是顺手：

1. **撞到 limit 现在算"不完"**。v1 明确把两者分开（`report.stopped_early() && !at_limit`），
   v2 的规则是有界答案要自己说清楚。`fam_view_search` 的 `incomplete` 字段在这一格翻面。
2. **未知修改时间从 `0` 变成 `i64::MIN`**。v1 用 `unwrap_or(0)`（会印成 1970），
   v2 全程序统一 `i64::MIN` 表示"没有日期"。为一条正在拆的入口留第二种写法就是留第三种约定。

v1 自己保留的只剩**答案的形状**：默认 200 行（`LEGACY_SEARCH_LIMIT`，
`fam-ffi/src/lib.rs:69`）——一个冻结 ABI 可观察的行数，不在拆它的路上顺手改。

## 4. 解决方式效果评估

### 4.1 正面证据（跑过的）

- **真 DLL 的绑定与退化**：`Api::load_from(vendored())` 加载
  `third_party/everything-sdk/dll/Everything64.dll`、解析 16 个导出、探测得到
  `NotRunning`；`a_bound_client_with_no_service_reports_itself_unavailable` 断言
  `is_available()==false` 且 `search()` 报 `Unavailable{…"NotRunning"…}`——
  不是返回 0 行（那会被读成"这张盘上没有"）。
- **真扫描**：`volumes::scan()` 在这台机器上实跑出 `["C:\\","D:\\","E:\\","K:\\"]`
  （`the_scan_returns_volume_roots_only` 只断言形状，不把磁盘布局当规格）。
- **纯函数层**：`query::compile` 的引号/切分/`path:` 前缀、`state::judge` 的四态判定、
  `search::to_hit` 的 FILETIME 换epoch（`filetimes_cross_the_epoch_boundary_the_way_windows_means`：
  134116992000000000 → 1767225600000）。
- **端到端**：全仓 **334 条**绿（`fam-svc-everything` 46、`fam-svc-index` 68、
  `fam-ffi` v2 19、`fam-cli` 12），clippy 与 fmt 干净，GNU `famcore.dll` 可编
  （只有既有的 `corrupt .drectve` 链接器提示）。
- **真机两道闸门**：`abi-smoke` 退出 0，v1 的 JSON 搜索经路由器仍回
  `search backend=walk rows=0 incomplete=0`（0 是设计如此——它查的 `probe` 文件在搜索**之后**才创建）；
  `--self-check --query notes` 回 `search settled at 386ms: 1 row(s)`，截图里是
  `notes.txt  1 B  [document]`。
- **性能**：`fam-bench check` 九例全 ok。两条大 IO 用例
  `volume_scan_capped` 1.12x、`walk_large_tree` 1.10x 落在门槛（1.25x）内，
  属于 [benchmark-floors-are-machine-state.md](benchmark-floors-are-machine-state.md) 记的漂移那一类，
  **不计为改动带来的收益或损失**。

### 4.2 这台机器上永远验证不到的五条

写在这里是为了下次不要假装它们被验证过。每一条都标了"错了会怎样"，
因为这才是决定要不要去有服务的机器上补测的依据。

1. **真结果数据**：一行都没收到过。目录行的 `size`/`date` 到底是什么、
   名字查询里会不会混进卷根（`Everything_IsVolumeResult` 存在但我们没绑，
   卷根会以 `folder=true` + 空 path 回来，`to_hit` 有对应测试但那是我们这边的形状）。
   错了会怎样：显示出来的大小/日期是错的，或者多出行——**看得见，不致命**。
2. **排序保真**：`page()` 设 `SetSort(SORT_NAME_ASCENDING)` 但从不问 `Everything_IsFastSort`。
   服务若无法快排，会按别的顺序回包而我们当"已按名字排好"呈现。
   错了会怎样：结果顺序不对，且**没有人会报错**。这条我认为最值得去有服务的机器上补。
3. **`GetResultFileNameW`/`GetResultPathW` 的指针寿命与越界行为**：
   按"下次查询前有效"处理（所以 `wide_string` 立刻拷走），越界假设返回 NULL
   （`wide_string(null)` → 空 Vec，已处理）。
   错了会怎样：读到脏内存或空行。这是全部五条里唯一可能**崩**的一条。
4. **`GetDBLoad`/`Indexing` 的真实时长**：服务刚启动时索引要多久才 `Ready`、
   `Ready` 会不会退回 `Indexing`。探测节奏（1s 起、倍增到 30s、状态一变就回到 1s）
   是按假设定的。错了会怎样：要么问得太勤，要么启动后等太久才开始用索引。
5. **等待式查询的真实阻塞时长**：`QueryW(TRUE)` 在服务忙时到底阻塞多久。
   发查询之前先探一次状态就是为它设计的，但没对着一个正在建索引的服务量过。
   错了会怎样：搜索线程被占住，界面停在 `LOADING`。

（`GetResultFullPathNameW` 的缓冲契约不在列表里——我们没用它，是自己拼 path+name，
所以是**绕开了**而不是验证了。）

再加一条结构性的、跟机器无关的：**假的一端测不到 `compile` 的语义**。
它不看编译后的查询，所以一条"语法正确但意思错"的查询（比如误发 `ext:` 而不是引号形式）
不会在任何假路径上暴露。`compile` 有它自己的字符串断言，
可没有任何东西能保证那个字符串对真服务意味着什么。这是这套测试的天花板。

### 4.3 代价

- `pub mod testing` 进了出货的 crate（`#[cfg(test)]` 到不了别的 crate 的测试）。
  生产代码没有理由构造它，文档写在模块头上。
- 分页测试的语料是 1024–3072 行 `RawRow`，`fam-svc-index` 整包 0.33s 跑完（实测）。
  内存峰值**未实测**。
- 三个 `SearchRouter` 构造入口 + `Scope::from_config` + `Insisted`：多出来的公共面，
  换来的是"哪几个后端、什么顺序、什么规则"只有一个地方能写。
- v1 的两处语义翻面，见 3.2 —— 有消费者（`abi-smoke`、`tests/ffi.rs`）在守 v1 的形状，
  两处都改在字段自己的定义上而不是偷偷改数字。

### 4.4 残留风险与局限（没解决什么）

1. 上面五条 unverifiable 一条都没消掉，只是**写清楚了**。要消得去装了 Everything 的机器上跑，
   那需要一台新机器或一个真的服务实例。
2. **一个后端交完行再失败，行收不回来**（sink 只增）。结果是并集，`provider` 只说谁跑完。
   它是安全的（每行都过了复核，多出来的是真匹配不是猜测），但调用方**事后分不出哪行来自谁**。
   写进了 `SearchRouter::search` 的文档，没有改结构。
3. `covers` 只看卷根，junction/挂载点穿到未索引卷那一半仍未修
   （详见 [scoped-search-was-empty-and-said-so.md](scoped-search-was-empty-and-said-so.md) 第 4 节）。
4. `Everything_IsVolumeResult`、`GetTotResults`、`IsFastSort`、`GetResultListSort` 都没绑。
   前两个是"以后可能要用"，`IsFastSort` 是第 2 条风险的正解，留给下一次碰这条路的增量。
5. v1 的搜索面拆了，**v1 的视图面还在**（`fam-view` + `fam_view_*`），
   按计划增量 9 整体拆除。`fam_backends_json` 现在同时是"这个构建有几种搜法"和
   "此刻几种能用"两个问句的答案，`ids()` 与 `available()` 分开，没有合并成一个 bool。
