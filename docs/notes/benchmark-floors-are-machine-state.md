# 基准地板是机器状态：三条数字已经不可复现，记录它们的提交自己也测不出来

- 增量：5b（下）
- 提交：本次 `chore(bench)` 重录 `bench/baseline.json`
- 涉及：`crates/fam-bench/src/main.rs`、`crates/fam-bench/src/measure.rs`、`bench/baseline.json`

## 1. 描述问题

搜索分页写完，跑门槛 `cargo run -p fam-bench -- check`，三条 FAIL、两条反而快了三到四成
（2026-09-21，同一条命令连跑两次）：

| 用例 | 地板 base | 第 1 次 now | 第 2 次 now | 地板 x | 判定 |
| --- | --- | --- | --- | --- | --- |
| `open_flat_dir` | 790µs | 1041µs | 1079µs | 1.32x / 1.37x | FAIL |
| `read_flat_win32` | 247µs | 390µs | 384µs | 1.58x / 1.55x | FAIL |
| `viewport_from_cache` | 13µs | 64µs | 63µs | 4.92x / 4.85x | FAIL |
| `volume_scan_capped` | 1102706µs | 914845µs | 724658µs | 0.83x / 0.66x | ok |
| `walk_large_tree` | 381928µs | 280915µs | 259638µs | 0.74x / 0.68x | ok |

一次改动不会同时让三条变慢、两条变快 30%，而且变慢的三条里有一条贵了 5 倍——
这个形状不像代码，像尺子。

## 2. 分析问题

**先排除"是我这次改的"。** 本次只动了 `fam-core` 的两个类型（`SearchHit`、
`SearchRequest.scope`）、`fam-svc-walk` 的 provider、以及 `fam-ffi` 的新模块。
`fam-bench` 的依赖里没有 `fam-ffi`（`crates/fam-bench/Cargo.toml:18-19` 只有
`fam-svc-index` 与 `fam-svc-walk`），而 `viewport_from_cache` 的热路径
（`DirIndex` 查找 + `Collection::fetch`）一行都没改。

**再做 A/B 对照**（与增量 5d 同一手法：`git worktree` 建旧提交，交替跑，同一台机器同一时段）：

| 用例 min | `b786ca0`（改前，含增量 5 全部） | `HEAD`（改后） | `683f1fd`（**写下这三条地板的那次提交**） | 地板 |
| --- | --- | --- | --- | --- |
| `viewport_from_cache` | 64µs / 63µs | 62µs / 62µs | **61µs** | 13µs |
| `read_flat_win32` | 401µs / 408µs | 400µs / 395µs | **390µs** | 247µs |
| `open_flat_dir` | 1024µs / 1071µs | 1084µs / 1123µs | **1044µs** | 790µs |
| `open_mixed_dir` | 2291µs | 2196µs | 2492µs | 2415µs（这条没报红） |

决定性的一行是第一行：**`683f1fd` 就是记录 13µs 那次提交，它今天在自己身上只测出 61µs**。
同一份二进制、同一份语料（`D:/Applications/Qt/6.11/6.11.2/mingw_64/bin`，
188 条目，目录 mtime 2026-09-20 11:43，与 baseline 里记的 `entries: 188` 一致）。
三个提交之间的差在 ±5% 以内，与地板差到 4.9 倍不是同一个量级的问题。

所以结论是：**地板与机器状态绑定，而机器状态在 2026-09-20 之后变了**。
`open_mixed_dir` 没红（2415 vs 2196–2492），因为它那条语料（`plugins`，15 子目录）
恰好还在原来的量级上——漂移不是全局等比例的，这解释了为什么只有三条红。

**没有查下去的部分**：13µs 与 247µs 当时为什么可达。候选是防病毒实时扫描状态、
NTFS 名称缓存 / SysMain 预取、CPU 睿频窗口，全部未验证。这次不查，
因为查出来也不能把尺子修回昨天。

**顺带暴露的一个真缺陷**：`bench/baseline.json` 的 `recorded_on` 里原本手写着两句出处
（"read_flat_* added at increment 2" / "viewport_from_cache added at increment 3"），
而 `run --write` 用 `describe_machine()` 的固定字符串整个覆盖它（`crates/fam-bench/src/main.rs:209`）——
重录一次，出处就没了。

## 3. 解决方式

1. **重录**：`cargo run -p fam-bench -- run --write`，七条全部来自同一次运行、同一台机器。
2. **把出处从会被覆盖的字段里搬走**：`describe_machine()` 现在明说这件事
   （`crates/fam-bench/src/main.rs:209-218`）：

```rust
/* Rewritten by `run --write`, so anything hand-written here is lost the next time the floors are
 * re-recorded - which is why the baseline's provenance lives in `docs/notes/` instead. */
format!("{} {}, warm cache, fam-bench {} | provenance: docs/notes/", ...)
```

3. 本文就是那份出处：旧数字、A/B 表、以及"哪个提交记录的哪条地板"都留在上面。

## 4. 解决方式效果评估

**正面证据。** 重录后再跑 `check`：

```
case                   floor base  floor now floor x   p50 x  verdict
open_flat_dir               1032us      1004us   0.97x   0.98x  ok
open_mixed_dir              2464us      2252us   0.91x   0.97x  ok
read_flat_std                212us       213us   1.00x   0.91x  ok
read_flat_win32              390us       395us   1.01x   1.02x  ok
viewport_from_cache           64us        64us   1.00x   0.98x  ok
volume_scan_capped        705786us    738373us   1.05x   1.01x  ok
walk_large_tree           260152us    257327us   0.99x   0.98x  ok
all measured cases within gate
```

七条的 floor x 落在 0.91–1.05，门槛重新在量同一台机器上的**相对**变化，
这正是它该做的事（`measure.rs:132-134` 的注释已经说明只 gate `min_us`）。

**代价。** 两条地板被**收紧**：`volume_scan_capped` 1102706→705786、
`walk_large_tree` 381928→260152。以后大遍历贵过 25% 就会红——这是更严，不是更松，
但下一次真变慢时它可能只是机器又漂了。
另有 `describe_machine()` 多一句字符串。

**残留风险与局限（没解决什么）。**

1. **为什么 13µs/247µs 曾经可达，没查、也不打算查**。
2. **地板仍然与机器绑定**，换机器必须重录；`check` 没有"这台机器不是那台机器"的自检
   （`recorded_on` 只写 OS/ARCH/版本，太粗）。真要做得加一个机器指纹或环境哈希。
3. **语料是外部目录**（Qt 安装目录、`D:\`），别人机器上不一定存在；
   `FAM_BENCH_FLAT` 可以覆盖单条，但没有"语料不存在就跳过"的机制。
4. 重录把 `max_us`/`p50`/`p95` 一起换掉了（`run --write` 写整条结果），
   所以旧分布形状没留下副本——本文的表只记了 min。
5. **漂移方向不可控**：这次是三条变慢、两条变快。若下次是全部变慢，
   同一套 A/B 手法仍然会得出"不是代码"的结论，门槛就会连续红灯——
   那时候该考虑的是把 gate 从"绝对地板"改成"与本次运行内的对照构建比"，
   那需要 `check` 能同时跑两个二进制，属于工具改造，留给增量 9（拆脚手架）之后。
