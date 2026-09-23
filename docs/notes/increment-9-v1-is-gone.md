# 增量 9：v1 的视图脚手架拆掉了，包括它自己那个守门人

- 增量：9（拆脚手架）
- 提交：`refactor(ffi)` + `refactor(shell)`（本文件之前两笔）
- 删除：整个 `crates/fam-view`（691 行）、`fam-ffi` 的 v1 视图与监视入口、壳里为它们留的外壳
- 涉及：`Cargo.toml`、`crates/fam-ffi/{src/lib.rs, tests/ffi.rs, tests/v2.rs}`、
  `apps/fam-shell/{include/famcore.h, src/bridge.{h,cpp}, tools/abi_smoke.cpp}`

## 1. 描述问题

主计划第 9 行写的是：删 `fam-view` crate、v1 shim、`RowOut`/`DeltaOut`、（若不再使用）`collect()`
与 `DEFAULT_MAX_ENTRIES`；门槛"全绿 + clippy 零告警"。

这些脚手架是增量 5b 有意留下的：当时定了"**v1 不重写为 v2 之上的 shim**"，理由是壳已经走 v2、
`abi-smoke` 是 v1 的守门人，所以 v1 原样留着，等增量 9 整体拆。到 8b 结束时，v1 的视图面
（`fam_view_new` / `fam_view_search` / `fam_view_drain` / `fam_view_free` /
`fam_watch_start` / `fam_watch_stop`）已经没有任何真实调用者——只剩两处：
`crates/fam-ffi/tests/ffi.rs` 里 3 条测试，和 `bridge.cpp` 里三个专为 `abi-smoke` 存在的外壳。

顺带要回答一句"计划里那两个'若不再使用'到底还用不用"：**`collect()` 还在用**
（`crates/fam-bench/src/measure.rs:278,283` 两条 bench 用例走它），
**`DEFAULT_MAX_ENTRIES` 还在用**（`fam-svc-walk/src/walk.rs:43` 是遍历策略的默认上限）。
两个都不删。

## 2. 分析问题

### 2.1 删掉的东西里有一件真的能力，必须说明白

v1 的 JSON 行是 `RowOut{key, path, name, kind, size, modified_ms, tags}`；
**v2 的定长记录 `Item` 是 32 字节，没有标签字段**。所以 `fam_view_search` 一删，
"搜索回来的行自带标签"这条能力就没了——它不是被 v2 的别的方式替代了，是**根本不在 v2 的线上**。

这条以前没写在任何地方，是被删的时候才撞出来的（旧测试
`search_rows_carry_their_tags_in_display_order` 断言的正是它）。三条事实：

1. 壳今天不画标签（DisplayRole 是"名字 大小 [类别]"），所以**界面没有可见退化**；
2. 但"给选中的行打标签"之后要在列表里显示出来，就得把标签送到壳侧——要么给 `Item` **追加**字段
   （`record_size` 这套版本机制正是为此留的：只追加、不重排，老壳读它认识的前缀），
   要么另开一个按路径批量取标签的 JSON 入口（`fam_tag_list` 已经是单次一个文件）；
3. 里程碑一不含标签功能（计划第 3 条明写"标签等整理功能后置"），所以现在没有消费者。

**决定：不为了保住一条测试而给 ABI 加字段。** 但这条决定要留痕，所以它写在这里，而不是让将来
做标签的人去猜"标签是不是曾经上过线"。

### 2.2 随 v1 一起走的内部件

- `views: Mutex<HashMap<u64, ViewModel>>`、`pending: Arc<Mutex<Vec<FileChange>>>`、
  `watchers: Mutex<HashMap<u64, Handle>>`：v1 视图的三件家当。v2 用的是 `Views`（每 token 一把锁）
  与 loader 线程，从不碰这三样。
- `loss_seen`：**这个计数器存在的唯一理由就是 v1**。增量 6 复查欠账 #1 修完 `FLAG_LOSS` 之后，
  v1 的 drain 不能再 `swap(0)` 共享计数（那会把所有 v2 视图的基线之上永远够不着，`FLAG_LOSS`
  从此不亮），所以另记一个"v1 已经报过多少"。v1 的 drain 没了，它就没有存在意义了。
- `next_token: AtomicU64`：只有 `view_new` 与 `watch_start` 用它；`Views` 和 `Searches` 各有自己的
  计数器。删。
- `RowOut`/`DeltaOut`/`UpdateOut`/`RowsOut` 四个序列化结构，和 `fam_view` 的 `Row`/`Delta`/`Filter`/
  `ViewModel`/`row_for_path` 一起走。**`row_for_path` 里那次 `fs::metadata` 是 FFI 入口路径上最后一处
  在调用线程上打文件系统调用的地方**，它消失得比任何一条注释都有力：以后谁想在入口里加同步 I/O，
  已经没有现成的先例可抄了。
- `fam_core_free` 原来要把 v1 的 watchers 挨个 `handle.shutdown()`；现在唯一的 `Handle` 是共享管线，
  它随 loader 线程结束而被丢弃，而 `impl Drop for Handle` 就是 `stop_all()`
  （`fam-sched/src/pipeline.rs:346`）——所以不必手工关，也不能漏。

### 2.3 `abi-smoke` 不能删，得搬家，而且搬家之后它才真的在守东西

它是 C++ 侧唯一的 ABI 端到端闸门。旧版守的是"v1 消费者还能拿到答案"：建视图、搜一次、
`drain` 里等一条 `insert` delta。

搬到 v2 之后它守的是同一件事在**壳真正走的那条通路**上成立：开目录等到 READY、取一窗行、
搜索分页、**建一个文件后它在开着的视图里出现**、删掉它之后回来一个剪裁位置。

关键差别：**旧的那把闸门测不到 8a-1 修的那个 bug**——v1 的 `drain` 有自己一条从
`pending` 队列到 `ViewModel` 的路，新文件照样会出现在 v1 的 delta 流里；而 v2 走的是缓存 +
重读，那条路当时是断的。所以我把新闸门写完之后做了 A/B：把 sink 里" absorb 之后问一次欠不欠读"
那行临时换成 `let _ = (...)`，重编 DLL，跑 `abi-smoke`：

```
（无修复）dir token=1 rows=3 … search rows=3 …
          insert=0 cut=0 -> FAILED
          the new file never reached the open view          exit=1   （等满 10s）
（有修复）observed row at index 3
          observed 1 cut position(s)
          insert=1 cut=1 -> ok                               exit=0
```

**写这个闸门的过程中还抓到它自己两个错**，都记下来免得下次再犯：

1. 第一版把 `QFile::remove(probe)` 放进轮询 lambda 里——第二次删一个已经不存在的文件返回 false，
   在轮询里就等于"还没有"，于是再也没向核心问过第二趟，一路等到超时。删除要做在循环**外面**。
2. 旧版搜的是 `"probe"`，而探针文件那时还没创建，所以永远打印 `rows=0`——**一个恒为 0 的读数
   正反都证明不了**。改成搜 `"file"`（树里真有 3 个），现在打印 `rows=3`。

## 3. 解决方式

- `crates/fam-view/` 整个删除（`git rm -r`），工作区成员与 `workspace.dependencies` 一并摘掉，
  `fam-ffi` 的依赖表去掉它。
- `crates/fam-ffi/src/lib.rs`：删 6 个入口 + 4 个内部方法（`view_new`/`view_search`/`view_drain`/
  `view_free`/`watch_start`/`watch_stop`）+ 4 个序列化结构 + 3 个成员 + `loss_seen` + `next_token` +
  `LEGACY_SEARCH_LIMIT`；`fam_core_free` 不再手工关 v1 监视；`Order`/`SortKey` 改从 `fam_core` 直接
  取（以前绕道 `fam_view` 的再导出）。
- `crates/fam-ffi/tests/ffi.rs`：删 3 条 v1 视图测试；`a_null_handle_is_reported_not_dereferenced`
  里那个"void 入口也要能扛空句柄"的断言换到 `fam_dir_close` 上（它现在才是 v2 的 void 入口）。
- `crates/fam-ffi/tests/v2.rs`：新增 `try_set_sort` 辅助与
  `a_bad_sort_key_and_a_bad_kind_say_which_one_they_refused`——**搬的是"错误信息要说清是哪一个被拒"
  这件事**，不是搬调用。
- `apps/fam-shell`：`famcore.h` 删 6 个声明并把那节改名为"留在 JSON 的那一半：控制载荷，不是行"；
  `bridge.{h,cpp}` 删 `createView`/`search`/`drain`/`startWatch`；`tools/abi_smoke.cpp` 重写成 v2。

## 4. 解决方式效果评估

**数字，以及它们为什么少了。**

- 全仓 **320 条**（原 337）。差额逐项对得上：`fam-view` 自带 **15** 条随 crate 一起删、
  `ffi.rs` 删 **3** 条 v1 视图测试（10 → 7）、`v2.rs` 加 **1** 条迁移断言（20 → 21）。
  337 − 15 − 3 + 1 = 320。**没有一条测试是"跑不过所以删掉"的**。
- `crates/fam-ffi/src/lib.rs`：**+13 / −379**。工作区 crate 数 10 → 9。
- 导出表里 `fam_view_*` 与 `fam_watch_*` 的数量：**0**（`nm -D --defined-only` 与 `strings` 各查一次）。
- 闸门：clippy `--all-targets` 零告警、fmt 干净、
  `cargo check -p fam-svc-index --target aarch64-unknown-linux-musl` 通过；GNU `famcore.dll` 与
  壳、`abi-smoke` 均可构建；新 `abi-smoke` 退出 0（上面那段输出）；真机
  `--self-check-mutate` 全链照旧（`icons 29/29/29 pending=0`、`ipc retries=0`）。

**这次没解决、以及要留的痕。**

1. **标签不上线**（2.1）。里程碑一不需要它，但做标签的人必须知道 v2 的记录里没有这个字段。
2. **`fam_abi_version()` 的保护变了意思**。它原本防的是"按 v1 编的壳去读现在这块定长缓冲"，
   那个组合已经不存在了；现在它只防**不同版本的 v2**（`record_size`/布局漂移）。壳里那句
   退出码 75 的注释还写着 v1 的故事，等下一次动 `main.cpp` 时顺手改口——这次没改是因为
   它说的事情仍然成立（版本不符就不跑），只是理由变了。
3. **两套 token 仍然各自从 1 开始**（复查欠账 #5）：目录 token 与搜索 token 还是两个命名空间，
   把搜索 token 交给 `fam_dir_set_sort` 仍会说"unknown view token"。v1 那套没了，所以从三套变两套。
4. **`FamListModel::unbind()` 的调用者从 v1 变成了"清空搜索框且没有目录可回"**——它现在活着，
   但只在壳侧，C++ 侧没有测试台能断言它（8b 复查已记）。
5. `crates/fam-view` 的历史留在 git 里（以及 `archive/pre-rust-migration` 那个标签管的是更老的
   四套栈）。**没有为 v1 保留任何兼容层**：谁要 v1，去 git 里取。
