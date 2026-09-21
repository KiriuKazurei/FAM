# v2 线格式与窗口化模型：把每行五个 QString 换成每窗一次拷贝

- 增量：5（`fam-ffi` 的 `wire.rs`/`browse.rs`、`apps/fam-shell` 的 bridge 与模型）
- 提交：`8ed52f3`（Rust 半边）、`2200594`（Qt 半边）
- 涉及：`crates/fam-ffi/src/{lib,browse,wire}.rs`、`crates/fam-ffi/tests/v2.rs`、
  `apps/fam-shell/{include/famcore.h,src/bridge.*,src/famlistmodel.*,src/mainwindow.*,src/main.cpp}`

## 1. 描述问题

v1 的浏览通路一次调用返回整个结果集，走的是 JSON：

- Rust 侧把每行拼成 `RowOut{key,path,name,kind,size,modified_ms,tags}`，
  再 `serde_json::to_string` 成一整串，最后 `CString::new` 扫一遍找 NUL 再拷一次；
- C++ 侧 `QString::fromUtf8` → `toUtf8()` → `QJsonDocument::fromJson` 建 DOM →
  每行 5 个 `QString`（key/path/name/kind/tags）+ 一个 `QStringList`；
- 模型 `applyDeltas` 每个 delta 一对 `begin/endInsertRows`，1000 事件就是约 1000 对信号；
- 更根本的是：`fam_view_drain` 在**调用线程**上调 `row_for_path`，那里有 `fs::metadata`。
  也就是说 UI 线程每处理一批事件，就替每个受影响的路径付一次路径查询——本机 ~190µs 一个。

计划给 v2 定的形态是：定长二进制 + 调用方供缓冲 + 按 token 独立状态 + 调用线程零 FS I/O，
并且"没有任何 v2 调用返回整行数组"。

## 2. 分析问题

**每行的钱花在哪。** 一条记录在缓存里是 32 字节 + 名字；v1 把它变成 JSON 后，
`path` 与 `key` 是同一串路径的两种写法、`kind` 与 `tags` 是字符串——**每行存了整条路径两遍**。
一个 78k 条的目录，光这两项就是 78k × 2 × 89.6B ≈ 14MB 的字符串，再加 DOM 与每行 5 次分配。
所以真正要删掉的是"每行自带路径"和"每行一次分配"，不是"JSON 不够快"这一句笼统话。

**为什么"整行要么全给要么不给"。** 缓冲是调用方的，写一半是最坏结果：对端拿到一条
`name_off` 指向缓冲之外的记录，读出来是垃圾还不报错。所以宁可回 `row_count=0 + TRUNCATED`
让调用方换个够大的缓冲再来——错误要能被**识别**，不能被**忽略**。

**位置流为什么必须带前缀语义。** `fam_dir_poll` 最初也是 all-or-nothing：装不下就回
`row_count=0 + TRUNCATED`。但位置是**消费型**的——`take_cuts()` 已经把队列掏空了，
返回 0 等于**把这一批删除扔掉**，模型再也不会知道。这是壳那边逼出来的：
一个 1000 文件的突发一次轮询装不下（64 KiB 缓冲 ≈ 8000 条，够；但位置数不可预知时不够）。
改成"按缓冲容量取前缀，剩下的留在队列"，位置只有顺序有意义，所以前缀合法、中间截断不合法。

**头与位置会重复计一次改动。** 第一次真机截图就暴露了：列表显示 1 行，状态栏写"0 row(s)"。
原因是 `pollChanges` 先读 `dirStatus`（头里的 `total_rows` 已经是**打过补丁之后**的），
再应用 `dir_poll` 的位置（位置是相对**打补丁之前**的视图）——同一件事被算了两次。
正确的协议顺序是：**先 poll，用位置改模型，再用同一张 poll 的头核对行数**；
`dirStatus` 不参与变更，只在绑定时用来发现"行到了"。

**"从没有行到有行"不该报 RESYNC。** 一开始把 loader 落地的那次 `refresh` 也标成 resync，
于是每次开目录模型都要"重建"一次；而模型本来就没显示过任何东西。改成
`self.resync |= had_rows`：状态从 LOADING 变 READY 本身就是通知。

**排除掉的方案。**

1. **v2 也返回 JSON 行数组，只是分页。** 省了事，但每行 5 次分配与"每行自带路径"原样保留，
   而这两项才是账的大头。否决。
2. **把 `tags` 塞进定长记录。** 变长字段进不了 32 字节。改为：v2 的行不带标签，
   标签按窗口另要（增量 5 尚未做，见 §4 残留）。
3. **枚举名在 C++ 里抄一份。** v1 的 `kKinds[]` 就是这么干的——核心改了桶，壳就悄悄标错。
   改为 `fam_backends_json` 一并回 `kinds` 词表，`kind` 字节按索引解。

## 3. 解决方式

线格式（`crates/fam-ffi/src/wire.rs`，`pub mod` 以便测试与将来的 MCP 复用同一份定义）：

```text
offset 0     FamRowHeader 40B {magic, abi, record_size, row_count, state, flags, pad,
                               total_rows, reserved, generation, seq}
offset 40    row_count × record_size 字节记录（就是 `Item`，32B）
其后         UTF-16 名字 blob：名字起点 = 40 + row_count*record_size + name_off*2
```

`record_size` 是版本机制（字段只追加、绝不重排），布局在两侧都编译期钉住：
Rust 用 `const _: () = assert!(size_of::<Item>() == 32)`，
C++ 用 `static_assert(sizeof(FamHead) == 40 && sizeof(FamRecord) == 32 && sizeof(FamCut) == 8)`。

入口（`fam_abi_version() → 2`）：`fam_open_dir` / `fam_dir_state` / `fam_dir_fetch` /
`fam_dir_poll` / `fam_dir_set_sort` / `fam_dir_set_filter` / `fam_dir_close` /
`fam_backends_json`。返回字节数或 -1；控制/错误仍是 JSON。

- **按 token 独立状态**：`Views` 持注册表短锁，每个 `DirView` 自己的 `Mutex`；
  锁序固定"注册表 → 视图"，`each_view_of` 因此不碰视图锁。
- **调用线程零 I/O**：`fam-loader` 线程独占目录读取与 `watch/unwatch`；
  `fam_open_dir` 只做一次哈希查找 + 两次投递。
- **监视按视图引用计数**：同目录第 1 个视图挂、最后 1 个摘。
  依据是增量 4 的实测——挂一个监视 904µs，是读一个 188 条目录的 3.9 倍，
  所以"每个缓存目录都挂"在钱面前退让为"只挂有人看的"。
- **模型窗口化**：`rowCount()` 用头里的 `total_rows`，只物化一屏；
  越界绘制才 `dirFetch`。每行只剩 `name/size/modified/kind/directory`，
  路径按目录前缀现拼，kind 按核心词表解。

## 4. 解决方式效果评估

**正面证据（都是真实运行读数）。**

| 量 | 读数 | 来源 |
| --- | --- | --- |
| `fam_open_dir` 本身（1501 条目的目录） | **239µs** | `cargo test -p fam-ffi --test v2 -- --nocapture` |
| 打开后到"有行"（1501 条） | **10ms** | 同上 |
| 打开后到"有行"（300 条，真壳） | **28–35ms**（含 250ms 轮询的整数化） | `--self-check` 日志 |
| 删 1000 个文件（0.69s 内删完）→ 模型信号 | **991 个位置，3 对信号**（三批各 1 对） | `--self-check` 日志 |
| 计划门槛 | 1000 事件 ≤ 50 对 | **达标，且是 3 对** |
| 墓碑过半 | 突发后 `state=2`（STALE）→ 要求重读 | 同一日志，`incremental-patch-cost.md` 的规则被观察到生效 |
| v1 兼容 | `abi=2` 下 `abi-smoke` 一字未改通过（insert delta 照常出） | 闸门 |

**代价。**

- 三处"同一事实两份定义"：Rust 的 `Item`/`FamRowHeader` 与 C++ 的 `FamRecord`/`FamHead`。
  靠 `static_assert` + `const assert` 双向钉，但**字段语义**（比如 `mtime_ms = i64::MIN` 表示未知）
  仍然只写在注释里。
- 缓冲由调用方 sizing：`roomForRows()` 每行留 256 个 UTF-16 单元，装不下时重试一次 4 倍。
  路径极深的目录会走这条重试，多一次调用。
- 每批剪裁后模型丢掉整个窗口缓存（下次绘制重取）。窗口只有一屏，所以代价是一屏的重取；
  但**位置跨批不保留**，若一批里位置很多，绘制会连续重取。未量化。

**残留风险与局限（没解决什么）。**

1. **≤50 对信号这条门槛只在"连续名批量删除"上成立。** 位置按视图序返回，
   相邻才合成区间；若一次突发删的是**隔行**的名字（例如每隔一个删一个），
   1000 个位置就是 1000 对信号，门槛直接破。真实批量删除通常是连续名，所以达标；
   但这条不是"任意 1000 事件 ≤ 50 对"的证明。要证它得让核心按区间回，或模型改用
   `layoutChanged`——都没做。
2. **搜索半边还在 v1**（`fam_search_begin/page/cancel` 未落地），所以 `fam-view` 与
   `row_for_path` 那个"调用线程上做 `fs::metadata`"的点**仍然存在**，只是只影响搜索路径。
   v1 也还没变成"v2 之上的 shim"。
3. **绘制路径上的分配没动**：`DisplayRole` 每次拼一个 QString，`ModifiedRole` 每格一次
   `QDateTime::toString`。计划里"每行 5 次分配"变成"每可见格 1~2 次"，不是零。
4. 行的**标签**在 v2 里没有位置：模型现在不显示 `# tag`。要么按窗口另开一个 JSON 调用，
   要么给记录加一个标签计数位——先记在这里，等增量 7（图标/元数据）一起做。
5. 首屏时间的主导项不在这条通路上：见 [window-show-costs-a-second.md](window-show-costs-a-second.md)。
