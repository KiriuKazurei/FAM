# 监视被悄悄撤销，而这件事在本进程内不可观测

- 增量：4（`fam-sched` 单层监视 + 目录分组）
- 提交：本次提交（前序：`517c7e0` 单层枚举、`683f1fd` 懒目录缓存）
- 涉及：`crates/fam-sched/src/{reconcile,pipeline,batch}.rs`、`crates/fam-sched/tests/watch.rs`、
  依赖 `notify-8.2.0/src/windows.rs`

## 1. 描述问题

缓存按目录持有一级表，所以每个被缓存的目录都要挂一个监视。挂上去之后，唯一能让它失效的事件
——"监视不再投递了"——**不会以任何形式到达我们手里**。

旧实现的消费者只切 Create/Write 两种事件，删除与改名从来不落到 UI（§4.2），但它同时还有一个
更隐蔽的问题：它假设"没收到事件"等于"没发生改动"。这一次要把这个假设要么证实要么推翻。

`notify-8.2.0/src/windows.rs` 里能观测到的事实（行号为该版本）：

| 位置 | 事实 |
| --- | --- |
| `:40` | `const BUF_SIZE: u32 = 16384;`，每个在途读请求一个 16 KiB 缓冲区 |
| `:331-335` | `handle_event(error_code, _bytes_written, …)`：`bytes_written` 直接丢弃 |
| `:339-368` | 错误分支只匹配 `ERROR_OPERATION_ABORTED` / `ERROR_ACCESS_DENIED` / `ERROR_SUCCESS`，**其余全部落到 `_ =>`** |
| `:357-367` | `_ =>` 做的事：`log::error!("unknown error in ReadDirectoryChangesW for directory {}")` + `request.unwatch()` + `return` |
| `:59-61` | `fn unwatch(&self)` 只是往内部 action 通道发 `Action::Unwatch(dir)`，不通知任何外部订阅者 |
| `:416-421` | 送给 `EventHandler` 的只有 `emit_event(Ok(ev))` —— 事件流里根本没有 `Err` 这条路径 |

也就是说：内核因为变更缓冲区装满而回报 `ERROR_NOTIFY_ENUM_DIR`(107) 时，它不在已匹配的三个码里，
于是走 `_ =>`——记一条日志、把监视撤掉、返回。**我们既收不到那批事件，也收不到"收不到"这个事实。**

## 2. 分析问题

**先确认这真的能发生，而不是只写在文档里。** 两次尝试，都是 release 构建、真实后端、16 KiB 缓冲：

| 压法 | 结果 |
| --- | --- |
| 单线程连建 3000 个文件 | 3048ms（984 create/s），3051 行全到，`dropped=0`，marker 立刻到 |
| 16 线程 × 500 = 8000 个文件 | 4926ms（**1624 create/s**），8143 行全到，`dropped=0`，marker 立刻到，`watched_count` 仍为 1 |

换线程只把速率从 984 抬到 1624（5.5 倍并行度换来 1.65 倍吞吐），因为每次 create 本身就要付一次
路径查询（见姊妹篇 [watch-sweep-costs-a-path-lookup.md](watch-sweep-costs-a-path-lookup.md)），
瓶颈在内核栈不在我们进程。

**缓冲区装得下多少。** `FILE_NOTIFY_INFORMATION` 是 12 字节头 + 2×名字长度、按 4 对齐；本机临时目录
里 12 字符的名字一条 36 字节 → 16384 / 36 ≈ **455 条**。而一次 `fs::write` 新建至少产生两条记录
（`FILE_ACTION_ADDED` 与紧随的 `FILE_ACTION_MODIFIED`），所以要**约 227 个文件同时压在缓冲里**才算溢出。
折算：若排空间隔 1ms，需要 45.5 万条/s 的产生率才填得满，比实测的 1624 条/s 高约 280 倍。
（这条是折算，公式如上；结论是**本机没能触发溢出**，不是"溢出不会发生"。）

**那这条路径是不是白防？** 不是，因为 `_ =>` 分支覆盖的远不止溢出。同一个 `log::error! + unwatch`
还兜住任何我们没列出的完成码，而 `ERROR_ACCESS_DENIED` 分支（`:345-353`）在目录消失时更是
**连日志都不记**就直接撤。撤销之后目录内容还在变（别的进程往里写），缓存就长期服务一张
再也不会更新的表。这条链路**在本机是可复现的**，测试见 §4。

**排除掉的候选方案：**

1. **把缓冲区调大。** 没有旋钮：`BUF_SIZE` 是 crate 内 `const`（`:40`），`configure_raw_mode`
   （`:249-252`）忽略传入的 `Config` 并直接回 `Ok(false)`，Windows 后端不支持任何配置项。
   要走这条路就得 fork/vendor 一个 notify。**否决**：为一个兜底机制养一份分支，代价大于收益。
2. **装一个 `log` 捕获，把那行 `unknown error…` 抓回来。** 技术上可行（notify 用的就是 `log`）。
   **否决**：库抢占进程全局 logger 会偷走宿主程序（Qt 壳、CLI 各自拥有日志）的输出权；
   而且这是把契约下注在对方的一句错误文案上，改一个词就失效。
3. **定期 re-arm（先 unwatch 再 watch）。** 实测一次 `Handle::watch` 要 **904µs**（200 个目录、
   release、floor；最差单次 1475µs）。归因：`add_watch` 里 `path.is_dir()` 调了两次
   （`:169`、`:177`）、一次 `CreateFileW`，加上我们自己的 `write_time`，四次路径查询 ≈ 4×190µs。
   也就是说 re-arm 比"只采样一次写时间"贵 4~5 倍，**而且 remove 与 add 之间天然有一个丢事件的空窗**。
   **否决**。
4. **只信我们自己的 `dropped` 计数。** 它统计的是"事件已经到手、我们的队列装不下"，
   监视被撤销时它一动不动。不是替代品。

**顺带撞上的两个同族陷阱**（同一份源码，都影响这次设计）：

- `add_watch` 用**调用方给的原始路径**做键（`:238`：`self.watches.insert(path.clone(), ws)`）。
  `D:\a` 与 `d:/a` 是两个条目 → 同一目录两个句柄两路事件，重复上报；而同一个键被再次
  `insert` 时，旧 `WatchState` 直接被 drop，**既不 `CloseHandle` 也不 `CancelIo`**，于是句柄泄漏，
  且那个仍会往我们通道投递的读请求再也无法撤销。
- `remove_watch` → `stop_watch`（`:255-268`）里有一句
  `while WaitForSingleObjectEx(complete_sem, INFINITE, 1) != WAIT_OBJECT_0 {}`：
  取消监视会在调用线程上自旋排空 APC。实测平均 2µs、最差 286µs，但**没有上界保证**。

## 3. 解决方式

**去重按身份，不按字符串。** `Handle::watch` 先查 `WatchSet`（`PathKey` 键），已在看护就直接返回
`false`，绝不把两种写法递给 notify（`pipeline.rs:276-298`）。
测试 `the_same_directory_written_two_ways_is_one_watch` 钉住"两种写法只算一个监视、create 不重复"。

**对账兜底。** 新增 `crates/fam-sched/src/reconcile.rs`：目录被创建/删除/改名时，**它自己的**
last-write time 会变，这是外部可观测的替代品。每个在看的目录周期性采样一次，
"写时间前移 + 本窗口一条事件都没到" = 覆盖丢失，发一条 `Signal::Rescan{Reconciled}`；
采样本身读不到（目录被删、拒绝访问）发 `Rescan{WatchLost}`。

```rust
// reconcile.rs:152-174
match read_mtime(&path) {
    Err(_) => {
        if !watch.lost {
            signals.push(Signal::Rescan { dir: key.clone(), why: RescanWhy::WatchLost });
        }
        watch.lost = true;
    }
    Ok(mtime) => {
        watch.lost = false;
        if mtime != watch.mtime && !watch.seen {
            signals.push(Signal::Rescan { dir: key.clone(), why: RescanWhy::Reconciled });
        }
        watch.mtime = mtime;
    }
}
```

三个决定各自对应一处上面读到的事实：

- **`lost` 一次性**（`reconcile.rs:37`）：目录持续缺席时，轮转每圈都会再采样一次。若不去重，
  一个已删除的目录会**永远每圈报一次重扫**——消费者的补救动作本来是"丢掉表并 unwatch"，
  但它不 unwatch 的话就会被同一条通知追着喊。采样照旧（这样"回来了"还能被发现），只有上报去重。
- **`seen` 由事件路径置位**（`pipeline.rs:425`）：`mark_seen` 让"确实收到了"解释掉写时间的移动，
  否则每次正常浏览都会变成重扫。
- **预算按时间不按条数**：见 [watch-sweep-costs-a-path-lookup.md](watch-sweep-costs-a-path-lookup.md)。

**词汇表里删掉无法产生的那一项。** `RescanWhy::Overflowed` 在本增量内被移除：全仓库只有测试构造它，
没有任何生产者，而 §2 已经确定这个后端不可能给出生产者。留一个永不出现的枚举值，等于让 UI 为
一个到不了的场景写文案。现在只剩 `WatchLost`（后端点名报错 / 目录读不到）与
`Reconciled`（由写时间推出来），二者都是本进程真能确立的事实。

## 4. 解决方式效果评估

**正面证据（真实后端，端到端）。**
真程序冒烟（`fam watch <目录> --seconds 6`，release 构建，另起 shell 在 t=2s 删掉被监视的目录）：

```
t+ 2172ms  batch #1  0 change(s)  dropped=0
  rescan   C:/USERS/KIRIG/APPDATA/LOCAL/TEMP/FAM-SMOKE/VICTIM (WatchLost)
stopped: 1 batch(es), 0 change(s), 1 rescan(s), 0 dropped event(s)
```

删除本身一个事件都没进来（`0 change(s)`——删除的是被监视目录自己，只会报到父目录，而父目录没在看），
2.2s 后由采样拍兜出来。同一台机器上，创建+删除**目录内**文件的正常冒烟是
`2 batch / 2 change / 0 rescan`，即对账没有把正常浏览误判成丢失。
（顺带一处可见缺陷：重扫行打印的是 `PathKey` 的身份串，大小写已被折叠，不是原始路径。
信号里只有身份没有显示形态，改它得让 `Signal::Rescan` 带上 `PathBuf`——本次没做。）

`crates/fam-sched/tests/watch.rs::a_deleted_watch_target_surfaces_as_a_rescan_though_the_backend_says_nothing`：
监视一个子目录 → `remove_dir_all` 删掉它 → 断言 sink 里出现的**恰好**是一条
`rescan WatchLost <目录>`，并且该测试同时断言这一段时间里**没有任何 `row` 行**——这句就是
"后端确实什么都没说"的正向证据（若 notify 将来会报，这行会失败并留下记录）。
实测时延：整条测试 8.3s，其中前段（等到第一条 `WatchLost` 出现）约 **2s 出头**（推算：总时长减去
后段固定的 6s 静默窗口与启停开销；量级与"下一个采样拍必到"一致，不是精确值）。那 6s 里一条重扫都
不该再出现，这就是"一次性"的端到端钉法。
单元侧配套：`a_missing_directory_is_reported_once_until_it_comes_back`（一次性 + 回来后能再报）、
`a_sweep_visits_a_rotating_window_not_the_same_directories`（200 个目录一圈不重不漏）、
`a_delivered_change_does_not_come_back_as_a_rescan`（正常浏览不触发重扫风暴）。

**代价。**

- 每个目录每圈一次路径查询（约 190µs，本机），由 `SWEEP_WINDOW = 4ms` / `RECONCILE_INTERVAL = 2s`
  限住占用的消费者线程时间 ≈ 0.2%。
- 每个监视多一个 `bool`；`reconcile.rs` 生产代码 242 行（测试模块之前）。
- `tests/watch.rs` 从 4.77s 涨到 8.23s（新增的这条要跑满两个采样窗口）。
- `Handle::watch` 本身贵（904µs，其中 notify 的两次 `is_dir` 白付 380µs）——这次没有绕开它，
  只是记下来：增量 5 若"每次目录打开都加一个监视"，加监视会比读目录贵 3.7 倍
  （247µs 是 `read_flat_win32` 的 floor / 188 条）。

**残留风险与局限（没解决什么）。**

1. **缓冲区溢出至今未在本机触发过**，所以 `Reconciled` 这条推断路径对"溢出"这一类的覆盖是
   **推理而非观测**；实测能覆盖的是"监视目标消失"。不解决的理由：触发需要把产生率抬到实测的
   约 280 倍，而抬升手段（并行 create）已证明被同一条路径查询税率卡死。
2. **检测延迟 = 整圈时长**，公式 `N_目录 / (SWEEP_WINDOW ÷ 单次采样) × RECONCILE_INTERVAL`。
   本机单次 190µs、每圈 4ms → 每拍约 21 个：42 个目录以内的常用集合 2~4s 覆盖完；
   2 万个则约 32 分钟（分档表见 [watch-sweep-costs-a-path-lookup.md](watch-sweep-costs-a-path-lookup.md)）。这次不解决，因为"该先采样谁"取决于增量 5 的缓存热度信息，现在做是猜。
3. **`unwatch` 可能在调用线程上自旋到不确定的时长**（`INFINITE` 等待 + APC 排空）。这次不解决，
   留给增量 5：那条 ABI 会决定 `unwatch` 跑在哪个线程，届时必须给出"不得在绘制路径上调用"的约束。
4. 我们按 `PathKey` 去重只保护自己这一个 pipeline。同进程若再起第二个 watcher 实例，
   `add_watch` 的原始路径键照样会重复挂监视。
5. 采样读不到就报 `WatchLost`，无法区分"目录被删"与"权限变了"——两种都该重扫，所以这次合并；
   若将来 UI 要分开提示，需要在 `reconcile.rs` 里带上 `io::Error::raw_os_error()`。
