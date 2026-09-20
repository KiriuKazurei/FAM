# 版本号该描述"行"还是"尝试"：一次失败的刷新不该让内容看起来变了

- 增量：3（`fam-svc-index` 懒缓存）
- 提交：`683f1fd`（实现与度量台同一提交）
- 涉及：`crates/fam-svc-index/src/cache.rs`（`Slot`、`DirIndex::claim`、`Claim::fill/fail/abandon`）

## 1. 描述问题

计划里缓存槽写成 `Ready{id, gen, last_seen, pinned}`，但没说 `gen` 是什么。骨架期没有这个
字段；实现 single-flight 时必须决定它跟着什么走。第一版跟着"每次 `claim` 递增"写，理由是刷新
总归是一次新的动作。

后果在两条测试暴露出来：

- 一次刷新失败后，`peek` 交出的还是**原来那张表**，代号却已经 +1。
- 一次刷新被放弃（读线程取消或 panic，`Claim` 未结算即 drop），第一版直接删掉整个槽 ——
  用户正在看的那一目录行凭空消失。

两者是同一个混淆：把"我试了几次"当成了"内容是哪一版"。

## 2. 分析问题

**谁会消费这个号。** 计划里 `fam_dir_state(token)` 返回 `{state, total_rows, gen, seq}`，界面靠它
判断要不要重新取窗口；增量 4 的文件事件也要带代号，用来判断"这条事件针对的是我看到的那版吗"。
两种用法都要求 `gen` 是**内容的版本**：

| 事件 | 期望的 gen 变化 | 第一版 |
| --- | --- | --- |
| 首次读成 | 0 → 1 | 1 ✓ |
| 刷新成功，内容变了 | n → n+1 | n+1 ✓ |
| 刷新失败，旧行继续供 | 不变 | n+1 ✗ |
| 刷新被取消/放弃，旧行继续供 | 不变 | 槽被删 ✗ |
| 失效标记（`invalidate`） | 不变（行没换，只是不再可信） | 不变 ✓ |

错的两种表现不同但都致命：假的新版本号会让界面无条件重拉整个窗口（白付一次
`fetch` + 一次 `StrCmpLogicalW` 排序），而"事件属于哪一版"失去意义会让增量 4 的
外科式改行判据失效 —— 那是会把事件应用到错误行集上的那种错。

删槽更直接：违反计划里"Stale 与 Ready 分离且仍可服务（UI 永不空白）"这条硬要求。

**为什么编译期抓不到。** `generation` 是个 `u64` 字段，两种语义都能编译、都自洽；只有把失败
路径与放弃路径的行为写成断言才会显形。这也是这个字段值得单独留档的原因：它是跨模块契约，不是
实现细节。

## 3. 解决方式

**代号只描述当前可服务的行**，只有安装新表时前进。

```rust
enum Slot {
    Loading   { claim: DirId, tries: u32 },
    Reloading { claim: DirId, tries: u32, table: Arc<DirTable>, generation: u64, seen: u64 },
    Ready     { table: Arc<DirTable>, generation: u64, seen: u64, stale: bool },
    Failed    { table: Option<Arc<DirTable>>, generation: u64, seen: u64, tries: u32, not_before: Instant },
}
```

- `claim` 计算 `next = generation + 1` 但**不写进槽**，只放进 `Claim`；原先那个
  `Slot::Loading { next }` 字段因为没人读而被删掉（编译器 dead_code 报出来的）。
- `Claim::fill(table)` 是唯一让代号前进的地方。
- `Claim::fail()` 保留 `table` 与 `generation`，只加退避窗口 → 行还在、版本没变、状态是 Stale。
- `Claim::abandon()` 与 `Drop`（未结算）走同一条 `DirIndex::release`：槽里有旧表就还原成
  `Ready { stale: true }`（同表同代号），没有旧表才整槽删除。
- `release` 先校验 `slot.owner() == Some(claim.id)`，所以迟到的、已被别人接管的 `Claim` 不会
  误伤新状态。

## 4. 解决方式效果评估

**正面证据（测试名即断言）。**

- `a_failed_reload_keeps_rows_at_the_same_generation`：失败刷新后
  `after.generation == before.generation`，且 `Arc::ptr_eq` 证明是同一张表，
  `bytes_held() == table.approx_bytes()` 证明字节没被重复计。
- `abandoning_a_reload_keeps_the_old_rows`：放弃后行仍在、代号码不变、
  `retry_in == 0`（放弃不留退避）、`claim` 可再次成功。
- `a_refresh_replaces_the_rows_and_advances_the_generation`：只有真正换了内容才 +1。
- `two_callers_never_read_the_same_directory`：另一个线程在 claim 期间 `open` 得到
  `Opened::Loading`，`loads()` 全程为 0；释放后为 1。
- 全量：199 个测试通过（增量 2 后为 179，本增量 +20），clippy 零告警，`fam-bench check`
  七例全 ok。

**代价。** 每个槽多带 `tries: u32`（见
[backoff-and-byte-accounting.md](backoff-and-byte-accounting.md)），并删掉了没人读的
`next` 字段，净变化是每槽 +4 字节，相对每槽所服务的一百多行 × 32 字节可忽略。无运行时开销：
判定都在本来就要做的槽替换里。

**残留。**

1. `invalidate` 与在途读取仍有竞态窗口：事件在读取开始之后到达，读回来的行可能比事件旧。
   代码里写了注释，真正的关闭方式是增量 4 让事件携带它针对的代号 —— 也就是说本次修正
   是那条判据能成立的前提，但还没有闭环。
2. `State::Failed` 与 `Opened::CoolingDown` 目前不带原因；`Claim::fail()` 因此也不再收
   reason 字符串（写了但没人读，会变成槽里的死状态），失败原因现在只进 `log::debug!`。
   增量 5 若要在界面上显示"为什么这个目录打不开"，需要把它放回槽里并定义一个可读的枚举，
   而不是继续用 `String`。
3. `Snapshot.generation` 与计划里的 `seq`（事件流的水位）是两回事，`seq` 还不存在，
   留给增量 5 的 `fam_dir_poll`。
