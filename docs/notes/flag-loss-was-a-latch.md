# FLAG_LOSS 是一只闩：丢过一次事件，之后每次浏览都报"events dropped"

- 增量：6 复查欠账 #1（不属于任何 UI 增量，单独一笔清掉）
- 提交：`fix(ffi)`（本文件之后一笔）
- 涉及：`crates/fam-ffi/src/{browse.rs,lib.rs}`

## 1. 描述问题

`fam-ffi` 里"事件被丢弃"是一个**全 core 单调递增**的计数器 `losses`
（`crates/fam-ffi/src/lib.rs` 的 `FamCore.losses`），三个地方往里加：
管线 sink 的 `batch.dropped`、v1 `fam_watch_start` 的 sink 把 `dropped + rescans` 一起加、
v1 `fam_watch_stop` 把 `handle.shutdown()` 的返回值加。

v2 的 `dir_status` 对它的判断是 `losses > 0`。而它唯一的清零在 v1 的 `fam_view_drain`
（`self.losses.swap(0, …)`）——壳走 v2，从不调用它。于是：

**丢过一次事件之后，这个 core 上每个目录的每一次状态轮询、每一次取行，都带
`FLAG_LOSS`，直到进程结束。** 壳把它印成状态栏里永久的 `· events dropped`
（`apps/fam-shell/src/mainwindow.cpp` 的 `polled.lostEvents()` 分支）。
一个不再携带任何信息的警告比没有更糟：用户学会无视它，等它哪天真的在说事也没人看。

复查留档把它记为欠账 #1，归属写的是"按视图记基线，约 30 行，跨 `browse.rs` 与 `lib.rs`"。
本篇是那一笔的账。

## 2. 分析问题

要回答的问题是"**这个视图**看到的行，有没有可能因为丢事件而缺？"。它不是计数器的问题，
是"谁欠谁"的问题：

- 视图**打开之前**丢的事件，不是这个视图被承诺过又没拿到的——它拿到的缓存表就是丢完之后
  的样子；
- 视图**活着的时候**丢的事件，才是它的行可能缺的理由；
- 一次**成功的整目录重读**之后，行又齐了，之前丢的那些不再构成怀疑的理由。

所以正确的形态是**每个视图一个基线**：打开时记下当时的计数，超过才置位；成功的重读把基线
抬到"重读开始之前"的计数。抬到之前而不是之后，是因为重读期间丢的事件那次读不可能看见，
必须继续报警。失败的读不抬——行没变，关于行的确定性也没变。

顺带撞出一个**我自己差点引入的回归**：如果保留 v1 drain 的 `swap(0)`，清零会把共享计数打到
所有 v2 基线之下，`FLAG_LOSS` 从此永远不再亮。所以 v1 的"自上次 drain 以来丢了多少"必须换成
自己的记账（`loss_seen`，记录 v1 已经报过的量，`dropped = now - seen`），观测值不变
（`abi-smoke` 的 `dropped:0` 与 `dropped_events:0` 都还是 0），但共享计数不再被谁清零。

**这个计数器是全 core 的，不是按目录的**，所以基线方案有两处已知的不精确，都是"该不该报警"
在错的目录上提前收声或误报：

1. 目录 X 丢事件，会点亮**所有**开着的视图（包括只看着 D 的）——因为计数不区分目录；
2. 目录 D 成功重读，会清掉"X 丢过事件"这条对 D 视图的报警——D 的行确实又齐了，但 X 的没有。

要修得干净得让 `fam_sched::Batch::dropped` 按目录归因，那是 fam-sched 的改动，不是这一笔的
范围。两个方向都是"警告比理想情况更早收声/更宽"，而修之前的形态是"永远、对所有人报警"，
所以这一笔是严格改进，不是等价替换。

## 3. 解决方式

`crates/fam-ffi/src/browse.rs`：

- `DirView` 加 `loss_baseline: u64`，`DirView::new(snapshot, losses)` 在打开时记下；
  `lost_events(losses) = losses > baseline`；
- `Views::open(index, key, losses)` 收当前计数当基线；
- `Views::refresh(index, key, seen: Option<u64>)`：`Some` 只来自**成功的**读（计数在读开始之前
  取），把该目录每个视图的基线抬上去；`None`（读失败）行不变、基线不变；
- `Views::status(index, token, losses)` 多收一个当前计数，`Status` 多一个 `loss: bool`。

`crates/fam-ffi/src/lib.rs`：

- `Loader::start` 多收 `losses` 的 `Arc`，`Job::Load` 在读**之前**取计数，成功 `Some(seen)`、
  失败 `None`；
- `dir_status` 改成 `if status.loss { flags |= FLAG_LOSS }`，不再自己读计数器；
- 新增 `loss_seen: AtomicU64`，v1 drain 的 `swap(0)` 换成 `now - swap(now)`；
- `loss_count()` 一个读数入口，`stats` 的 `dropped_events` 也走它。

三条新单测（`browse.rs` 的 `mod tests`，本 crate 第一批 src 内测试）：

- `a_loss_lands_on_the_view_open_for_it_and_not_on_the_one_that_came_later`——闩的那一面：
  同一目录两个 token，基线 0 的那个报警、基线 1 的不报；
- `a_completed_read_clears_the_loss_it_postdates_and_a_failed_read_does_not`——
  `Some(3)` 之后计数 3 不再报警、计数 4 又报、`None` 之后 4 仍然报；
- `closing_a_view_does_not_move_the_baseline_of_the_one_still_open`——基线按 token 记，
  关掉一个不动另一个。

## 4. 解决方式效果评估

**正面证据。**

- 全仓 **337 条**绿（334 + 3 条新单测），clippy `--all-targets` 零告警，fmt 干净。
- GNU 工具链 `famcore.dll` 与壳、`abi-smoke` 均可构建；`abi-smoke` 一字未改退出 0
  （`dropped_events:0`、`dropped:0`、`inserts observed=1`）——v1 的 drain 记账换了实现，
  观测值没换。
- 真机两道 `--self-check`（`D:\Datas\fam-loss-check`，3 个文件的树）：
  浏览 `opened … at 1223ms: state=0 total=3 generation=1`，截图状态栏是
  `D:\Datas\fam-loss-check\tree · 3 row(s)`，**没有** `· events dropped`；
  搜索 `search settled at 2032ms: 1 row(s)`。这两道证明的是"没回归"，不是"修好了"——
  见下。

**这次没解决、以及验证不到的（如实列）。**

1. **真机上没有一次真实的丢事件被诱发。** 丢事件的来源是队列溢出与监视被撤，增量 4 的压测
   （8000 文件、1624 create/s）就没能让它溢出过；所以"修好"的证据是上面三条单测——它们钉的
   是规则所在的那一层（`Views`/`DirView`），真机两道只钉"没回归"。这与增量 4 的
   `Overflowed` 覆盖至今是推理而非观测，是同一个洞。
2. **计数器不按目录归因**，第 2 节那两处不精确留在原地；要修是 fam-sched 的事。
3. 复查留档里其余 9 条欠账一条没动：`FAM_FAILED`/`FAM_EMPTY` 无标签、排序与目录内过滤无调用者、
   `fam_backends_json` 的 `available` 被壳丢掉、两套 token 命名空间、`visited` 语义、
   `--backend walk` 其实是 auto、壳侧 IPC 计数自检、v1 关闭通路无测试、lossy 排除往返——
   归属不变（增量 8 / 增量 9 / 已知语义瑕疵）。
4. **`fam-bench check` 这次是红的，且红得有据可查**：`volume_scan_capped` 2.29x、
   `walk_large_tree` 1.96x FAIL。**不是这次改动**——`cargo tree -p fam-bench -i fam-ffi`
   报 "did not match any packages"，fam-ffi 根本不在 fam-bench 的依赖闭包里，bench 二进制与
   提交前逐字节相同。机器侧的证据：D: 盘 513G 只剩 4.3G（100% 满），`volume_scan_capped`
   这次跑出 `errors 1`（有一个目录打不开）；同一二进制连跑两次 min 从 1,343,951µs 摆到
   1,163,417µs（15%）。按 `benchmark-floors-are-machine-state.md` 的纪律，**地板没有重录**——
   把一台满盘的机器录进闸门，比一次红闸门更糟。盘空出来之后应当重跑并按需重录。
