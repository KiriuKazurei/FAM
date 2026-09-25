# 被监视的目录里，新文件从来不会出现

- 增量：8a-1（计划里"delta 合成连续区间"那一格，撞出一个功能不联通）
- 提交：`fix(ffi)` + `feat(shell)`（本文件之前两笔）
- 涉及：`crates/fam-ffi/src/lib.rs`、`crates/fam-ffi/tests/v2.rs`、
  `apps/fam-shell/{src/famlistmodel.{h,cpp}, src/mainwindow.cpp, src/main.cpp}`

## 1. 描述问题

做 8a-1 的动机本来只是计划里那句"1000 事件突发 `beginInsertRows` ≤ 50"。读代码时发现这条门槛
问错了问题：新行**根本没有通道**。于是先给壳加了 `--self-check-mutate`（在自检窗口内创建一个文件
再删掉），想量一次新建花多少对信号。跑出来的日志里**一个 reconcile 都没有**：

```
info: self-check created D:\Datas\fam-loss-check\icons\fam-self-check-probe.txt
（此后直到进程退出，没有任何一行提到行变化）
```

用户能看见的症状是：**打开一个目录，往里拖进一个文件，状态栏变成 stale，然后新文件永远不出现**——
除非把目录关掉再打开。删除是好的（消失类事件能就地改行），只有"多出来一行"不行。

## 2. 分析问题

三条机制各自看起来都"应该会兜住"，接起来却断了：

1. **事件本身放不下新行。** `browse.rs` 的 `to_patch` 只把 `Removed` 变成能落地的名字，其余一律
   `unread += 1`。`DirIndex::apply` 里 `placed()` 只看"有没有删掉东西或欠一次读"
   （`patch.rs:63`），于是它**照常换代表**：`generation + 1`、`stale = true`、`removed: []`
   （`cache.rs:553-569`）。视图跟着新代号走（`patch_with`，剪裁为空），**零信号**，行还是老行。
   这一步是对的：事件不带 size、mtime、"是不是目录"，凭空造一行才是说谎。
2. **兜底的重扫被事件自己吃掉了。** 写时间对账是增量 4 建的兜底，但规则是
   `if mtime != watch.mtime && !watch.seen`（`reconcile.rs:171`），而 `mark_seen(dir)` 对**每一个**
   到达该目录的事件都置位（`pipeline.rs:424-427`），采样后清回 false（`reconcile.rs:178-180`）。
   也就是说：**"事件已经解释了这次改动"正是它不再重扫的理由**——可这次改动恰恰是事件解释不了的那一种。
3. **没人替 stale 的表约一次读。** 会发 `Job::Load` 的只有两处：`needs_rescan()` 分支
   （新建事件不走这里）和 `dir_open` 里的 `request_load`（视图已经开着，没人再 open）。

而且这不是漏看——`crates/fam-ffi/tests/v2.rs` 里原来那条
`a_file_created_behind_the_view_is_a_resync_not_an_invented_row` 的注释写着
**"Opening again is what asks for the re-read"**，测试自己 `core.open(&tree.root)` 开了第二个视图
就为了触发重读。机制被当成结论钉住了：它证明"不会凭空造行"，同时也把"要重开一次才更新"写成了契约。

**为什么这次一定要连壳一起改。** 补上第 3 条之后，重读会真的到来，而它到来时壳的
`pollChanges` 见到 `FLAG_RESYNC` 就 `readHead` → `beginResetModel`。也就是说：**修好核心，
每新建一个文件就把整屏行扔掉一次**（选择、滚动位置一起没了），比原来"什么都不发生"更扰民。
所以 8a-1 是一个整体：核心去约那次读，壳把那次读**摊平成一到两对信号**。

顺带说明为什么不"从事件直接改一行"：那要一次定向属性查询，本机一条路径查找约 190µs，
与重读一个小目录同量级（`docs/notes/watch-sweep-costs-a-path-lookup.md`），增量 5 已经据此决定
"只有零 I/O 的消失类事件就地改行，其余交给重读"（`incremental-patch-cost.md`）。这次没推翻那个决定，
只是把"交给重读"那半句真的接上。

## 3. 解决方式

**核心（`crates/fam-ffi/src/lib.rs`）**：把"这个目录欠不欠一次读"抽成一个判据
`owed_read(index, key)`（当前行是新的就 `None`；有读在跑、或失败还在退避，`claim` 给 `None`），
`request_load` 与 sink 共用它；sink 在 `absorb` **之后**再问一次：

```rust
sink_views.absorb(&sink_index, delta);
ask_for_read(&sink_index, &sink_queue, &delta.dir);
```

`needs_rescan()` 分支也改走同一个 `ask_for_read`（原来那五行是它的复制）。

**测试（`crates/fam-ffi/tests/v2.rs`）**：删掉旧的那条，换成
`a_file_created_while_a_view_is_open_reaches_that_view_without_a_reopen`——**只开一个视图**，
新建文件后轮询到 `FLAG_RESYNC`，断言 `poll` 报 resync 且不带位置，再断言行里出现了 `brand-new.txt`。
旧测试里"表先变 stale"那一步不再断言：修好之后 stale 是几十毫秒的瞬态，拿时间窗口去抓它只会变成
一条偶尔失败的闸门（这条仓库已经吃过一次这种亏）。

**壳（`famlistmodel.cpp` 新 `reconcile()`）**：`FLAG_RESYNC` 不再等于扔掉视图。做法是
按当前窗口再取一次行，与手上的窗口做**公共前缀 + 公共后缀** diff，只对中间那段发
`beginInsertRows` / `beginRemoveRows`；窗口外也变了（总数就是这么动的），而 `rowCount()` 答的是
整个视图，所以余量在**模型末尾**发一对信号，那里没有委托在画。三条退回重建的口子：窗口起点被
总数挤得移动了（整体位移不是"差异"）、取数失败或取回的头与轮询的头不一致、以及尾部减法会伸进
窗口内。判同两行用的是**画出来的一切**（name/path/size/mtime/kind/directory/attrs），所以只被
改过的文件算"不是同一行"，代价是一删一插——它该重画。

`--self-check-mutate` 把删除也放进轮询窗口（3.4s 删、4.6s 抓图），同一次跑覆盖两条通道。

## 4. 解决方式效果评估

**先说这条测试有没有牙。** 把 sink 里那一行 `ask_for_read` 换成 `let _ = (...)`，重跑新测试：

```
panicked at crates\fam-ffi\tests\v2.rs:565: the watch alone must bring the new file into the open view
test result: FAILED. 0 passed; 1 failed; ... finished in 10.08s
```

10.08s 是它自己 10s 的等待上限——**没有修复时它等满才输**，不是碰巧。改回来后
`git diff` 与事先存的补丁逐字节一致（`restore is byte-exact`），测试 0.12s 通过。

**真机（126 行的 scratch 树，`--self-check-mutate`）：**

```
rows 0 -> 126 with 0 cut(s) at 1712ms, state=0 generation=1
self-check created ...\fam-self-check-probe.txt
re-read reconciled in 1 pair(s), 127 row(s) left, selection kept
self-check removed the probe: yes
rows 127 -> 126 with 1 cut(s) at 5247ms, state=0 generation=4
cut 1 position(s) in 1 signal pair(s) in 0ms, 126 row(s) left
icons: extracted=9 destroyed=9 cached=9 pending=0
```

新建一个文件 = **一次重读 + 1 对信号 + 不重建**；删除 = 原来的剪裁通道（1 对）。探针被自己清掉了
（`removed the probe: yes`），下一次跑不会带上它。
另外一次跑因为上一轮异常退出留了探针，日志是 `reconciled in 2 pair(s)` 且总数不变——那正是
"改了一行已存在的行"（一删一插），与 `sameRow` 的定义一致，不是抖动。

> **增量 14d 在这句话上停住了，而它不该停。** 上面那句 `selection kept` 只保证了"没重建整屏"，
> 没保证"读者的当前行没动"——而"一删一插"删掉的如果正是当前那一行，
> `QItemSelectionModel` 会把当前索引挪到下一行，列链把这次挪动读成一次点击。
> 当时我看见了 2 对，把它判成"与定义一致"就放过去了。
> 现在两个开销分开作证（`pairs` 与 `modified`），详见
> [reconcile-moved-the-readers-row.md](reconcile-moved-the-readers-row.md)。

**闸门：** 全仓 **337 条**绿、clippy `--all-targets` 零告警、fmt 干净、
`cargo check -p fam-svc-index --target aarch64-unknown-linux-musl` 通过；GNU `famcore.dll` 与壳可构建；
`abi-smoke` 一字未改退出 0（v1 通路不受影响）；不带 mutate 的搜索自检照旧
（`icons: extracted=1 destroyed=1 cached=1 pending=0`）。

**这次没解决、以及验证不到的。**

1. **对账仍会吞掉事件解释过的那次改动**——这是设计（否则每个事件都触发整目录重读）。修的是
   "事件解释不了"这一类由事件自己约读；如果事件**丢了**（队列溢出、监视被撤），仍靠下一轮对账，
   那条路径的覆盖依旧是增量 4 记过的"推理而非观测"。
2. **改一个已存在的文件花 2 对信号**（一删一插），不是一条 `dataChanged`。窗口只有几十行时不值得
   再引第三条通道；真要省，得让 `sameRow` 分"身份相同/内容不同"两类，留给真有人嫌的时候。
3. **reconcile 只覆盖已物化的窗口**：窗口外的变化以模型末尾的一对信号记账；窗口起点被挤动时仍
   重建。三条退路里**只有"正常路径"被测到**（真机日志），退路口子没有测试——那需要构造总数骤变，
   属于 8b 的测试台活。
4. **`--self-check-mutate` 会往被监视的目录写一个文件**，所以它是 opt-in，且用完自删。对着真实
   工作目录跑自检的人必须自己知道这一点。
5. 壳侧模型没有单测台（C++ 侧无测试目标），这一半的证据只有真机日志；8a-4 的 IPC 计数是补这个
   缺口的第一步。
