# 每条目录项两次 syscall：这个前提没被证实

- 增量：2（`fam-svc-enumerate` 换 `FindFirstFileExW`，遍历改写在其之上）
- 提交：本次提交（前序：`a09ac32` 骨架、`b611a6e` 遍历修复）
- 涉及：`crates/fam-svc-enumerate/src/{lister,portable,win32}.rs`、`crates/fam-svc-walk/src/walk.rs`、
  `crates/fam-bench/src/{main,measure}.rs`、`bench/baseline.json`

## 1. 描述问题

增量 0 的收尾结论写进了提交信息：78k 条目录的 floor ~380ms"受 syscall 数支配"，
每个条目 `read_dir` 拿名字、`DirEntry::metadata()` 再拿其余字段，即两次查询；
计划表因此把增量 2 定为"每条目一次 syscall"，验收线"每条目 < 2µs 热"。

骨架期的 `list_one_level` 就是 std 实现，正好是这条结论描述的形态。

## 2. 分析问题

**先要能测出来，才谈得上验证前提。** 第一次尝试是把两种读者各做成一个遍历 78k 树的
case，结果无法判读：

| 同一变体的 floor 跨度 | win32 | std |
| --- | --- | --- |
| 三轮交替观测（ms） | 1016 / 1079 / 957 | 1117 / 1048 / 1295 |
| 另一批 | 1085 / 920 / 980 | 951 / 905 / 961 |

单个变体自己的摆动（±20%~40%）大于两个变体之间的差，而且同一进程内先后跑的 case
受热文件缓存影响——把 win32 与 std 放在一次运行里比较，先后顺序就能决定谁赢。
这条路径被放弃：树级规模不是能分辨 1µs 级差异的地方。

**换成一个目录、重复几千次。** 188 条的暖目录、4000 轮、每个变体单独进程跑、顺序轮换：

| 读者 | floor（三轮，µs/188 条目录） | ns/条目 |
| --- | --- | --- |
| `FindFirstFileExW` 记录 | 248 / 249 / 248 | 1319 / 1324 / 1319 |
| `std::fs` + `metadata()` | 244 / 250 / 247 | 1297 / 1329 / 1313 |

差值 1% 以内，且这一级别的重复性（±1%）比树级高一个数量级。**两者等速。**

**为什么 `metadata()` 不是第二次查询。** 再插一个变体：std 循环里完全不调 `metadata()`
（改用 `file_name()` 长度防止被优化掉）。结果不是更快而是更慢（281/282/284µs 对
244/250/247µs）——因为 `DirEntry::file_name()` 自身要分配一个 `OsString`，而
`metadata()` 走的是 std 从枚举记录里已经拿到的数据，几乎免费。
也就是说，"每条两次 syscall"里的那第二次，本来就不存在。

**那树级 3µs/条目花在哪。** `walk_large_tree` floor 在本增量前后分别是
229/233ms（改动前）与 242/254/256/280ms（改动后，同一台机器同一天），
即没有可测变化；而单线程纯读者扫同一棵树要 900–1200ms。多线程把树级耗时压到 1/4，
说明瓶颈是**每次目录打开的延迟**被并发隐藏了一部分，而剩下的每条目成本在
`PathBuf` 构造 + mpsc 通道 + 收集进 `Vec` 这条流水线上，不在 reader 里。
这对增量 3/5 是好消息也是坏消息：换 reader 不解决问题，定长二进制 + 调用方缓冲 +
arena 才是那条账的正主。

## 3. 解决方式

前提被否，实现照旧落地——不是因为"应该更快"，而是计划里另外三件事只有拿到 Win32
枚举记录才做得到。

`crates/fam-svc-enumerate/src/win32.rs`：

- `FindFirstFileExW` + `FindExInfoBasic` + `FindNextFileW`，一条记录给全名字、属性、
  大小、时间；`FindClose` 在循环每条出口都执行。
- `search_pattern()` 自己造 `\\?\` verbatim 串：绝对路径加前缀、UNC 改写成
  `\\?\UNC\…`、分隔符统一成反斜杠、盘根补分隔符、相对路径**不**加前缀
  （verbatim 下相对路径非法）。
- `ERROR_NO_MORE_FILES` 与真失败在返回值上同形，只看 last-error 区分；中途失败记
  `errors` 且置 `truncated`，于是不完整读取依旧进不了缓存（见
  [partial-directory-cache.md](partial-directory-cache.md)）。
- `FILE_ATTRIBUTE_REPARSE_POINT` → `ItemFlags::REPARSE`，`attrs` 原样带上。
- `RawItem.name` 改为借用（`&'a [u16]`）：记录被复用，条目名不再每次分配 `Vec`。

配套：`fam-core` 加 `classify_units`（直接在 UTF-16 单元上取扩展名，无分配，与
`classify` 用一条差分测试锁住）与 `path_buf_from_units`（名字并回父目录时不经
lossy 字符串）。std 实现留在 `portable.rs`，非 Windows 用、Windows 上当对照。
`fam-svc-walk` 的工人循环改为按目录调用 `list_one_level`，删除
`Entry::build`/`fs::read_dir`/`item.metadata()` 这条旧链。

度量台配套：`--only` 过滤、`read_flat_win32` / `read_flat_std` 两个 case、每行输出
`ns/entry`，并把这两项并入 `bench/baseline.json`（原 4 项 floor 未动）。

## 4. 解决方式效果评估

**性能收益：没有，实测为 0。** `read_flat_win32` 1297ns/条目 对 `read_flat_std`
1292ns/条目（另一次 1319 vs 1297），差在 1% 内；`walk_large_tree` floor 前后同为
230–280ms 档。计划"每条目 < 2µs 热"这一验收线**达标**，但达标不是本增量的功劳：
std 读者在同一目录上也是 1.3µs/条目。树级两种读者都远超 2µs/条目，因为那里
2µs 的门槛本来就不是 IO 延迟能满足的——这条门槛的真实含义应在"单目录热读"上，
现在它由 `read_flat_*` 两个 case 长期看住。

**前提修正。** "78k 慢是 syscall 支配、去掉一次 stat 就好"这一判断是错的，错在把
API 形状当成 syscall 形状。据此要修正的预期是：增量 3/5 的收益只能来自**不再按条目
分配与拷贝**（arena + 定长记录 + 调用方缓冲）以及**不再重复整目录读取**（缓存命中），
而不是来自 reader 本身。若继续按旧假设排优先级，会白做一轮。

**实际买到的东西（都有测试）：**

1. `attrs` 与 `REPARSE` 位。`a_junction_is_flagged_as_a_reparse_point_by_both_readers`
   实测：两种读者都置 `REPARSE`（std 把 `IO_REPARSE_TAG_MOUNT_POINT` 归入 symlink，
   所以旧遍历本来也不会陷入 junction——这一点我原先猜错了），但只有枚举记录同时置
   `DIRECTORY`（`ItemFlags(3)` 对 `ItemFlags(2)`）。效果是 junction 现在按文件夹列出，
   与资源管理器一致；`a_junction_is_listed_but_never_descended` 钉住"列出但不进入"。
2. 长路径由我们自己掌控，且是**载荷性**而非装饰性的：
   `a_plain_pattern_cannot_reach_that_depth` 用 356 单位的目录实测——不加 `\\?\` 时
   `FindFirstFileExW` 直接 `ERROR_PATH_NOT_FOUND`，加了则 `a_child_beyond_max_path_is_still_enumerated`
   正常列出子项。std 内部本来也做这件事，所以这不是用户可见的修复，而是把机制收进来：
   增量 5 要按名字段拼路径，届时必须自己造串。
3. 每条目少一次分配：`RawItem` 借名 + `classify_units` 无分配 + 不再走 lossy 字符串。
   这一条没有单独计时（与 1 混在同一次测量里，被 ±1% 噪声盖住），属推算收益。
4. 增量 3 需要的单层原语与完整性判定就位，缓存骨架已经建在它上面。

**代价。** fam-svc-enumerate 新增 498 行（生产代码 189 行，含 3 处 `unsafe`，每处配一条
SAFETY 注释；边界是"指针指向的缓冲区在本作用域内有效，句柄在每条出口都关闭"），加一个
`windows-sys` 依赖（进入 DLL 依赖图，GNU 工具链实测可编译可链接）。`--only` 与两个新
case 让一次完整 `check` 多花约 6 秒（4000 轮 × ~250µs × 3 趟 × 2 个 case）。

**残留问题。**

1. 树级/卷级成本仍未解决，且已确认不在 reader：`walk_large_tree` 3.1µs/条目、
   `volume_scan_capped` 4.0µs/条目。归因（通道 + 每条 `PathBuf` + 收集）是推算，
   没有单独实验；增量 3 上缓存之后这类扫描只剩兜底路径在用，优先级由那时决定。
2. 度量台没有"单目录多轮"以外的对照：`open_flat_dir` 走的是多线程带通道的全流水线
   （floor 747–790µs / 188 条 ≈ 4µs/条目），与 `read_flat_*` 的 1.3µs/条目差 3 倍。
   这个 3 倍就是流水线的账，但两条 case 目前不相减成一条，读表的人得自己算。
3. `portable.rs` 在生产路径上不再被 Windows 使用，只作对照与非 Windows 实现；
   它与 win32 的分歧（junction 的 `DIRECTORY` 位、`attrs` 恒为 0）已在测试里写明，
   但如果将来非 Windows 也要出资源管理器，这些差异要重新审。
