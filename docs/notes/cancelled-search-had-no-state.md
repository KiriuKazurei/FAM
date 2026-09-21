# 被取消的搜索该是什么状态：既不是失败，也不是还在跑

- 增量：5b（下）
- 提交：本次 `feat(ffi)` 搜索分页
- 涉及：`crates/fam-ffi/src/search.rs`、`crates/fam-ffi/src/wire.rs`、
  `crates/fam-ffi/tests/v2.rs`、`apps/fam-shell/src/famlistmodel.cpp`

## 1. 描述问题

搜索分页需要一个头（`FamRowHeader`）来回答"这堆行能不能用、还要不要再来一页"。
第一版 `Search` 只有一个 `complete: bool` 加一个 `failed: bool`，
`head()` 里 `complete` 同时决定两件事：状态（READY / LOADING）和 INCOMPLETE 位。
取消路径写成：

```rust
Ok(searched) if !cancel.is_stopped() => state.finish(searched),
Ok(_) => state.failed = true,      // ← 取消被记成失败
Err(_) => state.failed = true,
```

而同一处注释写的是"取消后行仍然可翻页，头仍然说明范围没覆盖完"。
`failed` 会把状态打成 FAILED **并清掉 INCOMPLETE 位**——代码与注释相反，
前端按 FAILED 处理就会把已经显示的结果丢掉。

## 2. 分析问题

一个搜索的终局其实有三个正交的事实：

| 事实 | 谁用 |
| --- | --- |
| 还有行要来吗 | 前端要不要继续轮询 |
| 这些行覆盖全了吗 | 前端要不要写"结果不完整" |
| 后端出错了吗 | 前端要不要报错 |

`complete` 一个布尔把前两个捏在一起，于是"取消"和"到 limit"这两种
**终态 + 部分**只能挤进 FAILED 或永远 LOADING。后者尤其糟：那正是 v1 的老毛病——
调用方超时放弃，而遍历还在跑（`crates/fam-core/src/cancel.rs` 顶部注释记着这件事）。

排除掉的方案：

- **加一个 `FAM_CANCELLED` 状态**：线格式多一个状态、前端多一个分支，
  而 READY + INCOMPLETE 已经能表达同样的信息（"这就是全部了，但不是全部范围"），否掉。
- **取消即错误返回**：取消是调用方自己要的，不是失败；把它报成错误会让
  "取消后继续翻页看已得结果"这种正常用法变成异常路径，否掉。
- **让取消后的搜索停在 LOADING**：前端会一直轮询，否掉。

## 3. 解决方式

把"还要不要来行"独立成一个字段（`crates/fam-ffi/src/search.rs:47-56`）：

```rust
/// Set when the provider covered the scope. ...
complete: bool,
/// Nothing more will be pushed: the provider answered, the caller stopped it, or it failed. This
/// is what separates `LOADING` from a final state; `complete` alone would call a cancelled
/// search "still running" forever.
settled: bool,
failed: bool,
```

`head()`（`search.rs:142`）按 `failed / settled / 其余` 选状态，INCOMPLETE 只看 `complete`：

```rust
let flags = if self.complete { 0 } else { wire::FLAG_INCOMPLETE };
```

`run()` 的三条出口（`search.rs:285-295`）：正常完成 `finish()`（顺带排序）、
取消 `settled = true`（保留行、保留 INCOMPLETE）、出错 `failed = true` 且 `settled = true`
（行仍可翻页，通常为空）。

前端配套：`FamListModel::readHead` 原来只认 READY/STALE，现在多认"带 INCOMPLETE 的头"，
并且**状态变了就重置模型**——搜索在结束那一刻才排序，行数一样而顺序不同
（`apps/fam-shell/src/famlistmodel.cpp`）。

## 4. 解决方式效果评估

**正面证据。**

- `a_cancelled_search_stops_and_frees_the_thread_that_ran_it`
  （`crates/fam-ffi/tests/v2.rs`）：对 1500 条的树发起搜索后**立刻**取消，
  等到终态 → 头是 `READY` 且带 `FLAG_INCOMPLETE`，行仍可翻页；
  接着再开第二个搜索并等它 READY——只有一个搜索线程，第二个能跑完就证明第一个真放开了。
  这条测试同时钉住"不是 FAILED"和"不是永远 LOADING"。
- `a_limited_search_is_final_but_says_the_scope_was_not_covered`：`limit=3` →
  READY + INCOMPLETE，行数正好 3（到 limit 与被取消走同一条"终态 + 部分"）。
- 真机日志（`C:\Users\Kirig\AppData\Local\Temp\fam-search.png.log`）：
  `search for invoice queued at 110ms: state=1 total=0` →
  `search settled at 1044ms: 3 row(s)`——`state=1` 是 LOADING，
  说明 begin 真的没等，且终态被轮询抓到了。

**代价。** 一个布尔字段、`head()` 里一次三选一。前端 `readHead` 多一个 `m_state` 成员，
并且目录视图在 STALE→READY 时也会重置一次模型（一次重画，不是功能变化）。

**残留风险与局限（没解决什么）。**

1. **失败原因没上线**：provider 的错误文本在 FFI 里被丢弃，前端只看到 FAILED。
   线格式目前也没有放字符串的通道（`FamRowHeader` 只有一个 `reserved` u32）。
2. **哪个后端答的没上线**：v1 的 JSON 有 `backend: walk`，v2 的头里没有。
   `Searched.provider` 因此没人读，就没往 `Search` 里存字段。
   等增量 6 真有两个后端可切换时，这个信息会变成用户可见的（"这次是枚举搜的，慢"）。
3. **取消是协作式的**：`Cancellation` 在目录之间和条目之间被轮询，
   但一次 `FindNextFile` 循环内部不会中断，最坏要等当前目录读完。
   这个上界**未实测**（单层目录 188 条时读满约 390µs，见 `bench/baseline.json` 的
   `read_flat_win32`，可以当作一个数量级参考，但不是同一处代码的测量）。
4. "取消之后又翻页"与"取消之后又发同一查询"之间没有幂等保护：
   旧 token 的行还在，新 token 从零开始，屏幕上以最后一次 `runSearch` 绑定的 token 为准。
