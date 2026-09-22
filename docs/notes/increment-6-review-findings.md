# 增量 6 之后的全面复查：修了什么、驳回了什么、什么留在原地

- 增量：6 复查（不改计划，只把已完成的部分按缺陷重读一遍）
- 提交：`fix(everything)`/`fix(ffi)`/`fix(shell)`/`docs` 四笔（见 `git log` 本文件之后）
- 涉及：`crates/fam-svc-everything/`、`crates/fam-svc-index/`、`crates/fam-ffi/`、
  `apps/fam-shell/{include/famcore.h,src/famlistmodel.cpp}`、`README.md`、`README.en.md`

## 1. 描述问题

增量 6 收尾后，对整个搜索通路做一遍缺陷复查：逻辑错误、越界/资源问题、
"两边都写好了但没接上"、以及结构上已经说不通的地方。做法是两条并行：
自己逐行读关键路径（provider 的锁、`drain` 的算术、路由器的范围规则、FFI 的分页），
同时派两个只读 agent 分别去核 **C++ 壳 ↔ Rust ABI 的贯通**与**文档/清单与代码的一致性**。

结果分三类：**9 处确认并当场修**、**1 条最吓人的指控被驳回**、**10 处确认但不修**（写清代价与归属）。

## 2. 分析问题

### 2.1 驳回的那一条

agent 报了一条高危："**`limit` 在遍历这条路上不是上限**——FFI 的 sink 永远返回 true，
而 `WalkProvider` 只靠 sink 的返回值停，所以 Everything 不在时
`fam_search_begin(…, 500)` 会把整棵树的匹配全攒下来。"

**这条是错的。** `crates/fam-svc-walk/src/search.rs` 里闭包的返回值是：

```rust
sink(hit) && (limit == 0 || outcome.matched < limit as u64)
```

到达上限时它自己返回 false，`walk` 就停。既有测试
`a_bounded_request_stops_at_the_limit` 断言的正是"命中 2 条、`!complete`"。
之所以看着像真的：FFI 侧的 sink 确实恒真，但那只是"不主动提前停"，
上限由 provider 这一层守——这是契约里写明的分工（`limit` bounds how much a provider may visit）。

记下来是因为它是这次复查里唯一"如果为真就该立刻停手"的指控，而判断依据只能是读代码。

### 2.2 确认的九处，按成因分

**A. 我自己写的文档在说代码没做的事（3 处）**

1. `LEGACY_SEARCH_LIMIT` 的注释写着"上限是共享的，因为两个版本在这件事上都不允许那个后果"，
   而 `view_search` 只做了 `.max(1)`，**从没 `.min(MAX_SEARCH_LIMIT)`**。
   v1 的 `limit` 是 ABI 上任意 u32：`fam_view_search(core, v, "x", null, null, 4_000_000_000, …)`
   就能把整棵树的匹配全收进内存。这个洞 v1 本来就有（旧 `SearchQuery::limit` 同样不钳），
   但注释是新写的、且它断言的正是那条不存在的保护。
2. `EverythingState::Ready { volumes: Vec<String> }` 的文档说"路由器就按这个列表收窄选择"。
   实际上 `probe()` 里**硬编码 `volumes: Vec::new()`**（客户端 API 没有"你索引了哪些卷"的调用），
   而 `covers` 读的是 `self.indexed`（本地扫描）。字段永远为空、永远没人读，
   还带着两条断言它有用的测试。
3. `fam-svc-everything/src/lib.rs` 说 `drain`/`to_hit`/`RawRow` 之所以 `pub`，
   是"给本 crate 测试里的确定性假货用"——可 crate 内部走的是 `crate::search::…` 直接路径，
   这三个外部**一个都没人用**（只有 `PAGE` 被 `fam-svc-index` 用）。
   收窄之后立刻暴露 `RawRow::file` 在非测试构建里是死代码，于是补 `#[cfg(test)]`。

**B. 一处跨平台签名漂移（1 处，实测）**

4. `absent::EverythingProvider::load()` 不收参数，Windows 那个收 `roots: &[PathBuf]`；
   而 `router.rs` 的 `from_config`/`indexed` 不带 cfg 地传了 roots，
   测试还用了只在 Windows 存在的 `without_client()`。
   **这不是推断**：`cargo check -p fam-svc-index --target aarch64-unknown-linux-musl`
   在改之前报 2 条 E0061，改之后 `Finished`。
   `absent.rs` 的模块头写着"所以这个 crate、以及无条件 box 一个 provider 的 core，
   在别处也还能编"——那句话当时是假的。

**C. 协议与算术（2 处，都在真机上量得到）**

5. `fam_search_page` 无条件按"还有行没给"设 `FLAG_TRUNCATED`，
   而 `count = 0` 是文档化的廉价状态轮询（每 250ms 一次）。空窗口 + 有行 → 必然被标 TRUNCATED，
   而壳的 `readRows` 在"TRUNCATED 且没拿到行"时会**换 4 倍缓冲重试**——
   于是每一次状态轮询都白跑两次 IPC。现在只有真的要行时才设这个位。
6. `FamListModel::applyCuts` 用 `m_total -= quint32(positions.size())` 记账，
   可上面的循环会把**越界的位置钳掉并跳过**（`first = qMin(pos, m_total)`、`first <= last` 才发信号）。
   跳过的那些照样被减掉，`quint32` 就绕回一个巨大的数，
   `rowCount()` 返回 `int(m_total)` 变负——模型契约直接破。
   而"位置 ≥ 模型总数"是**真实可达状态**：同一个文件里 `fetchAt` 就承认
   `m_total` 会落后于 core（"Taking the new number without model signals"）。
   改成累加真正删掉的行数，并对 `m_total` 取 min。

**D. 结构与说法（3 处）**

7. `fam-ffi` 的 `fam-svc-everything` 依赖变成死的——provider 的构造搬进 `fam-svc-index` 之后
   它源码里再没出现过这个名字。删。
8. 两份 README：crate 表漏了 `fam-svc-enumerate`/`fam-svc-index`/`fam-svc-everything`/`fam-bench`
   四个成员，把 `fam-svc-walk` 描述成拥有"名称检索"（现在只剩一个后端），
   状态段还写着"当前是查询即递归遍历的原型"；而 "Software Architecture" 一节
   在**没有标注是旧栈**的标题下写着"不自行遍历建索引"——今天恰恰相反。
   `fam-svc-index` 的 crate 头也还写着 "Nothing here recurses"，
   可这个 crate 现在装着路由器。
9. 一条**机制没查清的 flaky 测试**被从闸门里拿掉（见下）。

### 2.3 那条 flaky：为什么不是"重跑等到绿"

`client::tests::a_machine_without_the_client_deployed_fails_to_load` 断言
`load_from_application_dir()` 返回 `LoadFailed`。它在**全仓跑时 4 次里失败 1 次**，
单 crate 跑约 30 次（含 `--test-threads=1` 与 8 线程）全绿；失败那次目录里确实没有
`Everything64.dll`。它的注释给的唯一解释是"除非某个构建步骤把客户端拷到那儿"——而没人拷，
所以**注释给的解释是错的**。

能确定的是：这个断言依赖的不只是那个目录，还有**同进程里别的测试已经映射进来的模块**
（按基名的 `LoadLibrary` 可能直接命中已加载模块，与搜索目录无关），
而本测试既不持 `CLIENT_LOCK`、也无法知道别的测试有没有在跑。机制没钉死。

处置不是删掉它，而是**把它断言的东西换成它能控制的东西**：
测试目录里不该有部署好的客户端（直接查文件），
"文件真的不在时加载器怎么报错"由隔壁那条
`a_path_that_is_not_a_dll_fails_to_load_rather_than_panicking`（走一条没人碰的路径）守。
理由写在测试的注释里：**一条失败原因说不清的断言不配待在闸门里**。

## 3. 解决方式

九处各自的最小修法（不夹带重构）：

| # | 落点 | 做法 |
| --- | --- | --- |
| 1 | `crates/fam-ffi/src/lib.rs` `view_search` | 补 `.min(MAX_SEARCH_LIMIT as usize)`，让注释成为事实 |
| 2 | `state.rs`/`client.rs`/`provider.rs` | `Ready { volumes }` → `Ready`，`Probe.volumes` 一并删；文档改成"能不能答"与"按本地扫描收窄"两句话各说各的 |
| 3 | `fam-svc-everything/src/lib.rs` | 根导出只留 `PAGE`；`RawRow::file` 加 `#[cfg(test)]` |
| 4 | `absent.rs` | `load(_roots: &[PathBuf])`（参数被忽略的理由写明）、补 `without_client()` |
| 5 | `fam-ffi/src/lib.rs` `search_page` | `count > 0` 才可能设 TRUNCATED |
| 6 | `famlistmodel.cpp` `applyCuts` | 累加真正删掉的行数，`m_total -= qMin(removed, m_total)` |
| 7 | `fam-ffi/Cargo.toml` | 删死依赖 |
| 8 | 两份 README + `fam-svc-index/src/lib.rs` | 补齐成员表、改检索归属、给旧架构节加"这是被替换的那套"标注 |
| 9 | `client.rs` 测试 | 环境断言换成目录断言，机制与理由写进注释 |

## 4. 解决方式效果评估

**正面证据。**

- 跨平台：`cargo check -p fam-svc-index --target aarch64-unknown-linux-musl`
  改前 2 条 E0061，改后 `Finished`。**这是本次唯一一条从"读出来的怀疑"变成"量出来的"的**。
- 全仓 **334 条**绿（比复查前的 334 不多不少，但内容变了：删掉两条断言 `Ready { volumes }`
  有用性的测试、合并成一条 `an_answering_service_with_its_database_loaded_is_ready`，
  又补回一条 `a_status_poll_says_nothing_about_truncation_while_a_short_page_does`），
  clippy `--all-targets` 零告警，fmt 干净。
- 真机三道：`abi-smoke` 退出 0（`search backend=walk rows=0 incomplete=0`、`inserts observed=1`）；
  搜索 `--self-check --query shot-07` → `search settled at 854ms: 1 row(s)`，截图是
  `shot-07.png  1 B  [image]`；剪裁路径专门重跑一次（20 个文件先建满、视图 populate 之后再删
  3 连 + 1 单 + 2 连）→ `rows 20 -> 14 with 6 cut(s)`、
  `cut 6 position(s) in 3 signal pair(s) in 0ms, 14 row(s) left`，
  剩下的行数与删掉的位置数对得上，正是第 6 处守的那个不变量。
- `fam-bench check` 九例全在门槛内（`walk_large_tree` 1.10x warn、其余 ok）。
- 修第 5 处之前**没有**为它写测试：TRUNCATED 与状态轮询的相互作用只有壳的重试逻辑才暴露，
  而那是 C++ 侧。这条记在下面"没解决"的第 8 条里。

**代价。**

- `Ready` 从结构体变体退化成单元变体：将来真要报告索引卷时得改回来（改动面很小，
  但 `Probe` 也得跟着加字段）。
- v1 的 `limit` 现在被钳到 20000——**这是行为改变**：理论上有个 v1 调用方指望过更大的数。
  今天唯一的 v1 调用方传的 0，`abi-smoke` 与 `tests/ffi.rs` 都过。
- `applyCuts` 多一个 `removed` 累加与一次 `qMin`，可读性略降，换来的是不会绕回。

**残留风险与局限（确认存在、这次没修，按严重度排）。**

1. **`FLAG_LOSS` 对只用 v2 的客户端会永久粘住。** 计数 `losses` 唯一的清零在
   `view_drain`（v1 专用，壳从不调用）里，而监视拆除时还会 `fetch_add`。
   于是丢过一次事件之后，**每个目录的每一次浏览**都带着"events dropped"——
   一个不再携带任何信息的警告，比没有更糟（用户学会无视它）。
   正解是按视图记基线（bind 时存下当时的计数，超过才置位），约 30 行，跨 `browse.rs` 与 `lib.rs`。
2. **`FAM_FAILED` / `FAM_EMPTY` 在浏览通路上没有任何标签。** 壳只对 `FAM_STALE` 特判，
   模型又故意不在 FAILED 时重置视图——结果是一个读不出来的目录（ACL、死掉的 UNC、已删除）
   长得和"这个文件夹是空的、而且我在跟着更新"一模一样。归增量 8（UI）。
3. **`fam_dir_set_sort` 与 `fam_dir_set_filter` 没有任何调用者**：壳既不能排序也不能在目录内过滤；
   而且发起搜索时是**关掉**当前目录而不是收窄它。归增量 8。
4. **`fam_backends_json` 的 `backends` / `available` 被壳丢掉**（只读了 `kinds`），
   所以界面永远说不出"索引没在跑，现在是枚举"。第 2 条的 `Searched.provider` 同理到不了 v2 界面。
5. 一个 core 发**两套各自从 1 开始的 token**（目录 / 搜索）。把搜索 token 交给
   `fam_dir_set_sort` 会得到 "unknown view token"——不会串数据，但错误信息误导。
6. `SearchOutcome::visited` 在两条路上不是一个意思：枚举是"看过的目录项"（可能十万），
   Everything 是"服务交回的行"。CLI 把两者印成同一个 `visited=`。
7. `--backend walk` 其实是 auto（两个后端都在），`walk-only` 才是只有遍历——
   这是从 v1 的 `match` 臂原样搬过来的，错误提示里两个名字并列，读起来像同义词。
8. 第 5 处那种"只有壳的重试逻辑会撞上"的协议错配，Rust 侧本来**没有测试面**：
   "count=0 不该设 TRUNCATED"对 Rust 自己没有任何可观察后果。补了
   `a_status_poll_says_nothing_about_truncation_while_a_short_page_does`，
   三条断言把标志的语义夹住（要 0 行 → 不设；要 3 行而总数 10 → 设；读到末尾 → 不设），
   其中第一条与第三条只差在 `count > 0` 那个门上。**壳侧"每次轮询省了一次 IPC"仍未测**——
   那需要一个能数调用次数的自检，属于增量 8 的测试台工作。
9. `FamListModel::unbind()` 从没被调用；`abi_smoke` 不调 `fam_view_free` / `fam_watch_stop`，
   所以 v1 的关闭通路其实没被任何测试走过。
10. `excludes_key` / `excludes_below` 的判断走的是 UTF-16→String 的 **lossy** 往返：
    两个不同的非 ASCII 目录名若在 lossy 之后相同，会被错误地一起排除。继承自增量 6 之前，
    不是新引入，但排除规则现在每行都跑，命中面变大了。
