# 状态被换走时附属信息没跟着搬：退避计数与缓存字节记账

- 增量：3（`fam-svc-index` 懒缓存）
- 提交：`683f1fd`（实现与度量台同一提交）
- 涉及：`crates/fam-svc-index/src/cache.rs`（`Slot::backoff_for`、`Map::replace/take`、
  `Slot::tries`、`Slot::evictable`）

## 1. 描述问题

写完成功路径后，三处只在**失败/淘汰**路径上才成立的东西是错的，三处都是编译通过、
成功路径测试全绿的状态：

1. 第一次失败要等的时间翻倍。`backoff_for(tries)` 写成 `BASE << tries`，而传进去的
   `tries` 已经把本次算进去了，所以第一次失败等的是 200ms 而不是 100ms。
2. 反复失败的目录不会被真正退避。`tries` 只存在 `Slot::Failed` 里；重试时 `claim` 把槽换成
   `Loading`/`Reloading`，历史就丢了，下一次失败又从 1 开始计数 —— 一个永远读不出来的目录
   会以 100ms 的节奏被反复重扫，正是退避要避免的事。
3. 缓存字节数在"失败但保留旧行"这条路上会漏记。`fail()` 先 `take`（减去旧槽字节），再插回
   `Failed { table: keep }`，而那次插入没把 keep 的字节加回来。结果是缓存自己报告的占用
   低于真实占用；更糟的是下一次 `take` 会减掉从没加过的字节 —— debug 构建下是
   `subtract with overflow` panic，release 下是回绕成天文数字，于是淘汰逻辑再也不工作。

## 2. 分析问题

三处是同一个模式的三个实例：**状态转换时，主数据搬了，附属计数没搬。**

- 第 1 项是差一（off-by-one），且方向隐蔽：窗口不是变短（会立刻被察觉）而是变长，
  只有拿绝对值断言才会撞上。
- 第 2 项需要走"失败 → 重试 → 再失败"两步才暴露，单测一条路径永远看不到。
- 第 3 项最阴：错误只在失败路径出现，而失败路径在开发期很少触发；一旦触发，破坏的是
  预算判定这个**全局量**，症状（缓存无界增长或突然全被清空）离根因很远。

为什么值得较真：`held_bytes` 是淘汰的唯一依据，它错了整个 LRU 就是装饰品；退避是权限拒绝、
U 盘拔插、杀软锁文件这些日常场景下唯一的自保手段。

**排除的方案。**

| 方案 | 否决理由 |
| --- | --- |
| 记账交给调用方（谁插入谁负责加减） | 三处转换都要手写一遍，正是这次出错的形态 |
| 淘汰时现算总占用（遍历所有表求和） | 每次淘汰 O(行数)，比它保护的 LRU 扫描贵两个量级 |
| 记账收敛进两个函数 + 单一 `bytes()` 来源（**选定**） | 见下 |
| `tries` 挪进独立 map | 又是一份要同步的状态，和刚犯的错同类 |

## 3. 解决方式

```rust
/// `tries` counts failures including this one, so the first waits `BASE_BACKOFF`.
fn backoff_for(tries: u32) -> Duration {
    BASE_BACKOFF
        .saturating_mul(1u32 << tries.saturating_sub(1).min(6))
        .min(MAX_BACKOFF)
}
```

`tries` 跟着槽走而不是跟着状态走：`Loading`、`Reloading`、`Failed` 三个变体都带
`tries`，`claim` 从旧槽搬过去，`fill` 成功时归零（`Slot::tries()` 对 `Ready` 返回 0，
所以能读到的历史随成功一起消失）：

```rust
// claim(): 仍有可服务行的说明上次刷新做了事，只有彻底没行的失败才继续累加。
(table.clone(), *generation, if table.is_some() { 0 } else { *tries })
```

字节记账收敛到两个入口，槽自己只暴露一个来源：

```rust
fn take(&mut self, key: &PathKey) -> Option<Slot> {
    let slot = self.slots.remove(key)?;
    self.held_bytes -= slot.bytes();
    Some(slot)
}

fn replace(&mut self, key: PathKey, slot: Slot) {
    let previous = self.slots.get(&key).map(Slot::bytes).unwrap_or(0);
    self.held_bytes = self.held_bytes - previous + slot.bytes();
    self.slots.insert(key, slot);
}

fn bytes(&self) -> u64 {                       // 唯一的"这张表值多少"
    self.rows().map_or(0, |table| table.approx_bytes() as u64)
}
```

所有插入/删除改走 `take`/`replace`，不再有裸 `slots.insert`。淘汰判定同时要求**无主**且
**没有别的持有者**：

```rust
fn evictable(&self) -> bool {
    self.owner().is_none()
        && self.rows().is_some_and(|table| Arc::strong_count(table) == 1)
}
```

## 4. 解决方式效果评估

**正面证据。**

- `repeated_failures_escalate_the_backoff`：三次连败实测窗口递增
  （100 → 200 → 400ms 量级，按 `retry_in` 的余量断言），并断言首个窗口就是
  `BASE_BACKOFF` 量级 —— 正是这条测试先抓到差一错误的（改前它报出约 200ms）。
- `a_successful_read_clears_the_failure_history`：同一个目录先连败两次（累计到 200ms 档），
  读成一次后再失败，窗口回到基础值。
- `held_bytes_tracks_the_tables_actually_cached`：`bytes_held()` 与槽内表的
  `approx_bytes()` 严格相等，且替换成更小的表后总数随之下降 —— 这条覆盖第 3 项。
- `a_failed_reload_keeps_rows_at_the_same_generation` 里同时断言了失败保留行时的
  `bytes_held()`，即漏记的那条路径。
- `a_table_someone_holds_is_not_evicted` 与 `a_table_being_read_is_not_evicted`：
  1 字节预算下，被快照持有的表、被在途 claim 占用的表都不被淘汰，释放后可以。
- 全量 199 测试通过、clippy 零告警。

**代价。** 每槽多一个 `u32`；`replace` 每次转换多算一次 `approx_bytes()`（一次长度读取，
不遍历行）。扫描式淘汰的复杂度是 O(缓存目录数)：目前**没有**实测数据 —— 生产路径还没接
缓存（增量 5 才接），`viewport_from_cache` 只有两个目录在缓存里。按计划默认 256MB、
每目录约 100 行估算，满载约 2 万个目录，淘汰一轮扫描要覆盖这个规模；
等增量 5 接上真实浏览后必须补测，否则"扫描式够用"只是推断。

**残留。**

1. `viewport_from_cache` 目前测的是"命中缓存后交出一屏"：整轮 20 页查找 + 全部窗口
   合计 188 行，floor 12µs（约 65ns/行）。它**不含** `Collection` 的重建与重排序 ——
   排序是每次 `rebuild()` 的 O(n log n)，增量 5 要么复用已排序的 order 数组，要么把这
   部分算进 UI 预算。
2. `peek()` 每次都构造一个 `Snapshot`（一次 `Arc::clone` + 三个字段）。相对 65ns/行的
   交行成本可以忽略，但没有单独测量。
3. `held_bytes` 仍是 `u64` 裸计数，`take`/`replace` 之外没有防线。真正根治是把不变量做进
   类型（例如只有这两个方法能拿到 `&mut Map`）；当前靠"其他代码不碰 `slots`"这条纪律，
   以及上面那两条断言。
4. 退避的指数在 `tries.saturating_sub(1).min(6)` 处封顶：100ms 起步逐次翻倍
   （100/200/400/800/1600/3200ms），第 7 次失败起 `100ms << 6` = 6.4s 被 `MAX_BACKOFF`
   夹成 5s 并从此不再增长。封顶是为了让 `saturating_mul` 不可能溢出，但"第 7 次起恒定"
   这条没有断言。
