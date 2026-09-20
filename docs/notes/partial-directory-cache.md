# 被取消的目录读取会以完整表的身份长期留在缓存里

- 增量：1（fam-svc-index / fam-svc-enumerate 骨架）
- 提交：`a09ac32`
- 涉及：`crates/fam-svc-enumerate/src/lister.rs`、`crates/fam-svc-index/src/cache.rs`

## 1. 描述问题

骨架的第一版 `DirIndex::open` 是这样的：查缓存 → 未命中就调 `list_one_level`
把条目逐行推入一张新建的 `DirTable` → **无条件**把这张表插回缓存并返回。

`list_one_level` 接受 `Cancellation`，也允许 sink 提前返回 `false`。也就是说它
有三种结局，而第一版一种都不区分：

1. 目录读完；
2. 走到一半 `cancel` 置位（用户切走了目录、超时、进程要退出）；
3. sink 主动叫停（将来的分页拉取、`limit` 命中）。

后两种留下的表**缺少尾部**，却和第一种存在同一个缓存槽里、由同一个 `get` 取出、
不带任何标记。`invalidate` 之前不会再读第二遍——一次半途而废的打开会长期伪装成
一个正常的目录内容。

症状还是延迟出现的：用户看到的是"这个目录少了后面几个文件"，而代码里那半张表
大小合理、键正确、能排序能分页，没有任何一处会报错。这类问题最难归因，因为它
在数据层看起来完全正常。

## 2. 分析问题

**为什么第一版会犯。** 枚举侧当时只有一个 `cancelled` 旗标，而且我把 sink 早停
也写成 `report.cancelled = true`。三种结局被压成两个布尔值，缓存侧拿不到
"读完没有"这个信息——它需要的判断依据在上一层就被销毁了。

**后果矩阵。**

| 结局 | 第一版缓存了什么 | 期望 |
| --- | --- | --- |
| 读完 | 完整表 | 完整表 |
| 取消 | 半张表，无标记 | 什么都不留 |
| sink 截断 | 半张表，无标记 | 半张表 + 明确的"未完成"标记 |
| 单条目读失败 | 完整表（`errors` 计数 >0） | 完整表（可接受，见 §4） |

**为什么骨架期不采用"存半张表并标 incomplete"。** 那是计划的正解，但它需要
`DirSlot` 状态机（`Loading`/`Ready`/`Stale`/`Failed`）、世代号和"稍后继续把剩下的
读出来"的路径。骨架里这些都不存在：缓存条目就是一个 `Arc<DirTable>`，没有任何
机制会在稍后把它补全。一个标着 incomplete 却永远不会被补全的条目，和一张伪装成
完整的半表，对上层是同一件事——所以骨架期只能选"不入库"。

**约束要落在哪一层。** "入缓存的表必须来自一次完整读取"是缓存的不变量，但只有
枚举层知道自己是否读完。因此判定必须由枚举层给出、由缓存层执行，而不是让缓存层
去猜（例如"条目数少于某个阈值就不存"）。

## 3. 解决方式

枚举层把三种结局分开报告，缓存层据此决定入库与否。

```rust
pub struct EnumReport {
    pub entries: u64,
    pub errors: u64,
    /// 停止信号触发，而不是目录读完。
    pub cancelled: bool,
    /// sink 要求停止，目录尾部从未被读。
    pub truncated: bool,      // lister.rs:21
    pub elapsed_ms: u64,
}

impl EnumReport {
    /// 只有完整读取才允许进缓存：半张表会让目录看起来缺了尾巴。
    pub fn complete(&self) -> bool {
        !self.cancelled && !self.truncated
    }
}
```

`lister.rs:96` 处 sink 早停改为置 `truncated`，不再借用 `cancelled`。缓存侧执行
不变量：

```rust
let report = list_one_level(&key.to_path_buf(), cancel, &mut |raw| { ... })?;
// 半张表绝不能进缓存：下次 open 会交出缺尾的目录清单。
if !report.complete() {
    return Err(IndexError::Cancelled);   // cache.rs:69
}
let table = Arc::new(table);
self.tables.lock().expect("dir index").insert(key.clone(), Arc::clone(&table));
```

`IndexError::Cancelled`（`cache.rs:11`）新增，语义是"这次打开没有产出可信的表"。
未入库的表随作用域结束被 drop，`next_id` 允许跳号，无副作用。

## 4. 解决方式效果评估

**误缓存已不可达（测试证据）。**

- `cache.rs` 的 `a_cancelled_open_caches_nothing`：预先 `cancel.cancel()` 后 open，
  断言 `matches!(error, IndexError::Cancelled)`、`cached_count() == 0`、
  `get(&key).is_none()`——三条分别对应"报错""不计数""取不到"。
- `lister.rs` 的 `a_sink_that_stops_early_marks_the_read_incomplete`：5 个文件、
  sink 收 2 条即停，断言 `seen == 2`、`entries == 2`、`!cancelled`、`truncated`、
  `!complete()`。即"截断"与"取消"确实落在不同旗标上。
- 正向用例同步加了断言：`lists_one_level_only` 现在检查 `report.complete()`，
  `cancellation_stops_the_enumeration` 检查 `!report.complete()`，避免"永远完整"
  这种退化（把 `complete()` 写成常量 `true`）也能被测出来。
- 回归面：`separator_and_case_variants_share_one_slot`、`opening_twice_serves_the_same_table`、
  `invalidation_drops_the_table`、`rows_carry_kind_and_size_from_enumeration` 全部
  继续通过，说明新增判定没有把正常路径一起挡掉。整个 workspace 162 个测试通过。

**代价：零。** 新字段落在原有结构体的填充空隙里，实测两种布局的
`size_of::<EnumReport>()` 前后都是 32 字节（`u64 × 3` + 两个 `bool` 挤在同一
8 字节槽内）。热路径多一次分支，无额外分配；`complete()` 是纯函数，不引入锁或状态。

**局限（已知且刻意）。**

1. 当前语义把"取消"表达成整次 open 失败。增量 3 的 UI 需要的是"先给已读到的部分 +
   `state=Partial` + 继续推进"，届时返回类型会从 `Err(Cancelled)` 换成带状态的
   `DirSlot`。这条改动已被测试卡住：`complete()` 仍是入库门槛，只是不再直接映射成错误。
2. `errors > 0` 目前不影响 `complete()`——单个文件 `metadata()` 失败会被跳过并计数，
   目录本身仍然读完了。是否要求 `errors == 0` 才算"可信到可以长期缓存"，留到增量 3
   与 `Failed` 状态的退避策略一起决定，因为那需要同时定义"部分损坏的表要不要
   在服务的同时重扫"。
3. `truncated` 现在只有 sink 早停会触发，而 `DirIndex::open` 的 sink 恒返回 `true`，
   所以该分支在增量 3 接上分页拉取之前不会被生产代码走到。测试是它现在就该存在的
   理由：不变量的定义必须完整，否则加上分页的那天就会重新引入同一个 bug。
