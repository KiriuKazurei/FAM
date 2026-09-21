# 就地改行的价钱：一次属性查询 ≈ 一次整目录重读

- 增量：5（`fam-svc-index` 的 `patch.rs`、`DirIndex::apply`、`Collection::patch`）
- 提交：`059de25`（索引侧）、`8ed52f3`（v2 通路把它接到事件流上）
- 涉及：`crates/fam-svc-index/src/{dir_table,patch,collection,cache}.rs`、
  `crates/fam-core/{src/list.rs,src/pathkey.rs}`

## 1. 描述问题

主计划"懒缓存"一节写着失效有两条路：**外科式增删单行**（常见）与**兜底重扫**（溢出、改名进子树、
目标未知），并且给前者配了具体做法——"`Modified` 时一次定向 `GetFileAttributesExW`，因为
`ReadDirectoryChangesW` 不报新大小，逐次重扫会变成每敲一键 O(dir)"。

增量 4 把事件按目录分好组（`Batch::by_directory()`），增量 5 要把它落到表上。落到哪一步为止，
取决于"改一行"到底多少钱。旧实现根本没有这条路：事件只喂给一个 JSON 视图，缓存表一旦被打开就
不再变，直到整个目录被重新读一遍。

## 2. 分析问题

**一条事件能确定地推出什么。** 只有"消失"。`FILE_ACTION_REMOVED` 说的就是这个名字没了，
手里的行就是它的全部信息，不需要任何查询。
"出现"和"内容变了"推不出行来：事件不带大小、不带时间、也不带"是不是目录"
（见 [event-carries-no-target-type.md](event-carries-no-target-type.md)），要补就得查。

**查一次多少钱。** 见 [watch-sweep-costs-a-path-lookup.md](watch-sweep-costs-a-path-lookup.md)：
本机一次路径查询 **187–223µs**，三个 API 等价；而 `read_flat_win32` 重读一个 188 条的暖目录
floor 是 **231µs**（本次 `fam-bench check` 读数）。也就是说计划里"一次定向查询"的兜底手段，
在 188 条目录上大约**一条就回本、两条就开始亏**——它省下的那次重读本来就不贵。

折算的盈亏点（公式：`查询数 × 190µs` 对 `条目数 × 1.228µs/条`，后者是本次
`read_flat_win32` 的 `min_us ÷ entries`）：**约 155 条**。目录小于它，逐行查属性比重读还贵；
大于它才划算，但要 155 行以上都"需要新属性"才勉强打平，而一次批量粘贴往往就是几百行。

**那"就地改行"还剩什么价值。** 全在不需要查询的那一半：
删除、隐藏、改名后的旧名——这些是**零 I/O** 的，一行 32 字节的标记而已。
一次批量删除 5000 个文件，落到表上是 5000 次就地标记；重读则要 5000 条 × 1.28µs ≈ 6.4ms 的枚举
外加一次 `FindFirstFileExW` 开关。前者还立刻给出**位置**，这是模型动画唯一能用的东西。

**排除掉的方案。**

1. **给每行存一份属性查询的句柄/缓存。** 没有这种东西：NTFS 不给"这个文件下次变了"的廉价订阅。
   否决。
2. **每行插入一个哈希索引（名字 → 下标）。** 一行记录 32 字节，名字索引再要 32~48 字节，
   等于把表的内存翻一倍去买一个"每窗口一次遍历"就能替代的东西。
   否决，改为**一批名字一次遍历**（见 §3）。
3. **COW 之外，直接 `Arc<RwLock<DirTable>>` 就地改。** 能让快照读到一半被改，
   等于把"表整体服务"这条不变量（`cache.rs` 头部第 1 条）拆掉。
   否决：复制一份的成本是 memcpy（188 条约 7KB，微秒级），换来读侧永远无锁。

**一个必须提前定死的细节：下标稳不稳定。** 视图自己存一份"记录下标的有序表"来给位置。
若删除用 `swap_remove`，一次删除就把末尾行的下标搬走，视图里所有比它大的位置全错。
所以离开的行**盖墓碑**（`ItemFlags::TOMBSTONE`），记录留在原地，下标终身有效。
代价是墓碑占着 arena 和记录字节，于是需要一条"烂到一半就重读"的规则。

## 3. 解决方式

`crates/fam-svc-index/src/patch.rs`：

```rust
pub struct Patch { pub removed: Vec<Vec<u16>>, pub unread: usize }
pub struct Patched { pub table: DirTable, pub removed: Vec<u32>, pub missed: usize, pub unread: usize }
impl DirTable { pub fn patched(&self, patch: &Patch) -> Patched }   // 返回副本，原表不动
```

一批名字**只走一遍记录**：先按折叠哈希把要问的名字分桶，再遍历一次活行命中它们，最后逐个用
`name_units_match` 确认。哈希只是分桶，**确认靠逐字节比较**，所以碰撞只多比一次、不可能删错行。
不这么做的话，成本是"行 × 名"：2 万行目录一次批量删除 = 上亿次名字比较，
而这件事发生在**给 UI 投递批次的线程**上。

`DirIndex::apply(key, against, patch)`（`cache.rs`）：无锁期算副本，取锁一次 `Arc` 换入并
`generation + 1`，换入前在锁内**再核对一次代号**；`Applied::Patched{snapshot, removed}` 或
`Applied::Nothing{marked_stale}`。三条规则各自有测试：

- **代号不配对就丢弃**（`a_patch_against_rows_that_were_replaced_is_dropped_not_applied`）：
  这也兑现了 `invalidate` 注释里欠下的那句"读取可能带回事件之前的行"。
- **墓碑超过表一半字节就标 stale**（`WASTE_BUDGET = 2`，
  `a_table_eaten_by_its_own_tombstones_is_asked_to_re_read`）。
- **换入不刷新 `seen`**（`a_patch_does_not_claim_a_tables_place_in_line`）：
  驻留该跟着"谁在看"走，不该让一个没人打开的构建输出目录把自己钉在预算里。

`Collection::patch(snapshot, removed) -> Vec<usize>` 返回**按视图序**的位置，
每个位置按前序剪裁之后的视图算——这正是模型逐段 `beginRemoveRows` 要的形态。

**"改不动"的那一半没有硬编。** 事件带不出大小的行不猜：表标 stale，交给重读；
重读由 `fam-ffi` 的 loader 线程做。增量 5 的壳日志里能看到它真的按这条规则走
（见 [v2-wire-and-windowing.md](v2-wire-and-windowing.md) 里 1000 文件突发那一段：
墓碑过半后 `state=2`）。

## 4. 解决方式效果评估

**正面证据。** `cargo test -p fam-svc-index` 从 31 条涨到 50 条，本步新增 19 条，其中
`a_bulk_delete_resolves_every_name_in_one_walk`（500 个名字一次遍历全部命中，无 `missed`）、
`an_event_naming_a_different_ascii_case_still_finds_the_row`（NTFS 大小写不敏感这件事落在
`name_units_match` 上，与 `PathKey` 同一条规则，并由 `folding_and_comparing_a_name_always_agree`
用 9×9 差分锁住）、`a_tombstone_holds_its_place_and_its_bytes`、
`patch_positions_are_for_applying_in_the_order_returned`、
`a_filter_hides_a_row_a_patch_took_out`（位置是**过滤后视图**里的位置，不是记录序号）。
端到端在 `fam-ffi`：`a_file_deleted_behind_the_view_comes_back_as_a_cut` 与
`two_cuts_in_one_window_come_back_in_the_order_they_must_be_applied`。

**代价。**

- 一条记录离开后，它的 32 字节和名字字节仍然占着表，直到重读；`wasted_bytes()` 只是记账，
  真正的回收靠重读，而重读的触发条件是"烂过一半"。也就是说**最坏情况下一张表可以长期带着
  接近一半的死行**，这是明码标价的取舍。
- 每窗口一次 O(行) 遍历：2 万行目录约 0.4ms（纯 CPU，按每行一次哈希 + 一次比较估）。
  未做成 bench case，是估的。
- 多了一个跨模块不变量："下标终身稳定"。`Collection` 的位置、`Applied::removed`、
  `DirIndex::apply` 的换入都依赖它；将来若有人加"压缩表"，必须同时改这三处。

**残留风险与局限（没解决什么）。**

1. **计划里"外科式增删单行"的"增/改"这一半没有实现，且按实测**不该**实现。
   如果哪天换到一台路径查询只要 2µs 的机器（无筛选栈税），盈亏点会从 155 条降到 1.6 条，
   那时逐行查属性才真正划算——这条判断本身是可被测量推翻的，故写在此处。
2. **改名在表里是两条独立补丁**（旧名墓碑 + 新名待读），不是一次移动；
   视图因此会先"少一行"再"多一行"。
3. 新建的行要等重读才出现，**延迟 = 重读落地时间**，而不是事件到达时间。
   对"刚保存的文件立刻出现在列表里"这种期待，本机目前的答案是"下一次打开该目录时"。
4. 位置只在同一批内单调有效；跨批之间若发生了重读，视图整体重建（`RESYNC`），
   不做"位置平移"这种补救。
