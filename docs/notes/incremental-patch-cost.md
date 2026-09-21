# 就地改行的价钱：一次属性查询 ≈ 一次整目录重读

- 增量：5（`fam-svc-index` 的 `patch.rs`、`DirIndex::apply`、`Collection::patch`）
- 提交：`059de25`（索引侧）、`8ed52f3`（v2 通路把它接到事件流上）
- 涉及：`crates/fam-svc-index/src/{dir_table,patch,collection,cache}.rs`、
  `crates/fam-core/{src/list.rs,src/pathkey.rs}`、
  `crates/fam-bench/src/measure.rs`（`patch_256_of_20k`、`rebuild_view_20k` 两个合成 case）

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

> **这一段在 5d 之后被实测替掉**（两个合成 bench case 落地，见 §4"代价"）：
> 读一条 = **1079ns**（release `read_flat_win32` floor 203µs ÷ 188 条），
> 排一条 = **153ns**（release `rebuild_view_20k` 3064µs ÷ 20000 行），
> 所以重读的单价是 **1232ns/条**，交叉点 `E = Q × 190000 ÷ 1232 ≈ 154 × Q` 条。
> 结论没变（一条定向查询要 154 条以上的目录才回本，256 条要 3.9 万条目录），
> 但当年那个"155"是**两个错互相抵消**得到的：用的 1228ns/条来自一个后来被证明不可复现的旧地板
> （[benchmark-floors-are-machine-state.md](benchmark-floors-are-machine-state.md)），
> 而且整个漏掉了视图排序那一项。方法不对，数字碰对了。

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
- **每窗口一次 O(行) 遍历——现在是实测的了。** 两个合成 bench case 落进 `fam-bench`
  （`patch_256_of_20k`、`rebuild_view_20k`，不需要语料），拆开的一次性探针跑完即删，
  方法与数字留在下面。20k 行表、一批 256 个消失的名字：

  | 量 | release | debug（门槛记的是这一列） |
  | --- | --- | --- |
  | 一次批：表复制 + 一次遍历 + 视图跟随 | **738–930µs** | 13.1–16.8ms |
  | 其中 `DirTable::clone`（1.1MB memcpy） | 36–67µs | 32–35µs |
  | 其中"遍历 + 取名字" | 32µs | 1.68ms |
  | 其中"遍历 + 取名字 + 折叠哈希" | 282µs | 5.3ms |
  | 其中"遍历 + 哈希 + HashMap 查桶" | 539–542µs | 12.8ms |
  | 其中视图跟随 `Collection::patch` | 62µs | 未测（只在 release 的探针里跑过） |
  | 从零排 20k 行视图 `rebuild_view_20k` | **2890–3064µs** | 8.3–12.1ms |

  三点读法：① 当年估的"约 0.4ms"**低了两倍**，量级是对的；② 大头是**每行一次哈希 + 一次
  HashMap 查桶**（539µs 里 282µs 是哈希本身），表复制只占 36–67µs 且与优化级别无关（memcpy）；
  ③ debug 与 release 在这些纯 CPU 行上差 14–50 倍，而 `read_flat_win32` 那种 I/O 主导的用例只差
  1.9 倍——**所以门槛记的 debug 地板对 CPU 用例几乎没有诊断力**，要判 CPU 回归必须看 release。
  （读数时机器不干净：一个游戏客户端驻留 15GB。纯 CPU 用例四次独立运行落在 ±6% 内，
  I/O 用例落在 ±40% 内，故只取前者作结论。）
- **真正的乘数是"每拍几批"，不是"每批几行"。** 补丁代价是 O(行)/批，与批里名字多少无关：
  实测一次 1000 文件的删除突发（`--self-check` 日志，2026-09-21）在 250ms 轮询里
  代号 +9~+10，即**每拍 9–10 次表复制 + 遍历**（`ui_batch_ms = 20`）。20k 行表上这就是
  release 每拍 8–9ms、当前 debug 构建每拍 120–170ms。
- 跨模块不变量"下标终身稳定"：`Collection` 的位置、`Applied::removed`、`DirIndex::apply` 的换入
  都依赖它；将来若有人加"压缩表"，必须同时改这三处。

**残留风险与局限（没解决什么）。**

1. **计划里"外科式增删单行"的"增/改"这一半没有实现，且按实测**不该**实现。**
   如果哪天换到一台路径查询只要 2µs 的机器（无筛选栈税），交叉点会从"154 条目录/条查询"
   降到 1.6 条（`2000ns ÷ 1232ns`），那时逐行查属性才真正划算——这条判断本身是可被测量推翻的，故写在此处。
2. **改名在表里是两条独立补丁**（旧名墓碑 + 新名待读），不是一次移动；
   视图因此会先"少一行"再"多一行"。
3. 新建的行要等重读才出现，**延迟 = 重读落地时间**，而不是事件到达时间。
   对"刚保存的文件立刻出现在列表里"这种期待，本机目前的答案是"下一次打开该目录时"。
4. 位置只在同一批内单调有效；跨批之间若发生了重读，视图整体重建（`RESYNC`），
   不做"位置平移"这种补救。
5. **每拍 9–10 批这件事没有被合并。** 按实测单价算，20k 行表重读一次 = 21.6ms 枚举 +
   3.1ms 排序 = **24.7ms**，而一批补丁 0.9ms → **累计 27 批就比重读还贵**；
   现在的触发条件"死行过半"要 **39 批**（1 万行）才响。也就是说预算确实松了约 1.4 倍。
   **但收紧 `WASTE_BUDGET` 是错的杠杆**：批越小、名字越少，每批的 O(行) 越不划算，
   收紧预算只会更早触发一次 24.7ms 的重读，把 9–10 批的浪费换成一次大停顿。
   对的做法是把"每拍的 9–10 批并成一批"（同一目录的补丁在投递给视图前合并，
   一次 `patched()` 解决），并了以后 1000 行的突发只需 1–2 批 ≈ 0.9–1.8ms，
   比任何重读都便宜。这件事要改 `fam-ffi` 的 sink 与 `DirIndex::apply` 的配合，
   留给增量 6（它同样要动这条投递链）。**注意这条推荐的算法**：它建立在"每批代价与名字数无关"
   这个实测事实之上，不在上面那张表里就看不出来。
