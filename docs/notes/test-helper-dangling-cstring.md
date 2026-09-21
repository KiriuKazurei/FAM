# 测试辅助里被 move 进闭包的 CString：C 读到的是已释放的内存

- 增量：5b（下）
- 提交：本次 `feat(ffi)` 搜索分页
- 涉及：`crates/fam-ffi/tests/v2.rs`（`Core::try_search`）

## 1. 描述问题

新写的 v2 搜索测试辅助第一次跑就红了两条，报错点看起来毫不相关：

```
a_kind_filter_answers_with_the_kinds_it_was_asked_for:
  assertion failed: unknown kind ""        ← 传进去的是 Some("directory")
a_scope_is_walked_and_reported_as_it_was_spelled:
  scoped to the subtree: Header { total_rows: 0, ... }
```

写出来的是：

```rust
let kind = kind.map(CString::new).transpose().unwrap();
let token = unsafe {
    fam_search_begin(
        self.handle,
        query.as_ptr(),
        within.map_or(ptr::null_mut(), |path| path.as_ptr()),   // ← 这里
        kind.map_or(ptr::null_mut(), |name| name.as_ptr()),     // ← 和这里
        limit,
        &mut error.value,
    )
};
```

## 2. 分析问题

`Option::map_or` 的闭包**按值**接收 `CString`：`as_ptr()` 拿到堆缓冲的地址，
闭包返回时 `CString` 析构、缓冲被 `free`，`fam_search_begin` 读到的是已释放内存。
Rust 侧一切合法——借用检查看不见 C 侧要读多久。

为什么它"看起来只是返回了空串"：释放后的那 8 字节被分配器复用/清零，
`CStr::from_ptr` 立刻撞见 0 字节 → `Some("")`。于是：

- `kind = Some("")` → `parse_kind("")` 报 `unknown kind ""`；
- `within = Some("")` → provider 拿 `""` 当搜索根 → 走不到任何文件 → `total_rows: 0`。

**这是运气，不是行为。** 同一份代码在另一个分配器状态下可以读到任意垃圾而"通过"
（比如恰好残留上一次的字符串），或者在 ASAN 下才炸。测试里最难查的就是这一类：
它不总是错。

排除掉的候选：

- **先怀疑产品代码**：报错点确实写在 `parse_kind` 和 provider 里，
  但 `Some("directory")` 变成 `""` 只可能发生在跨越 FFI 边界的那一步，
  而边界这一侧只有这个辅助函数。
- **`toml_path` 把 `within` 弄坏了**：单独打印 `within` 的字节，指针非空但内容为空——
  内容不在，路径函数无从 responsible。
- **改成 `into_raw()` 泄漏**：同文件里 `set_filter` 就是这么写的（`name.into_raw()`），
  它不悬垂、只漏。测试里漏三次无所谓，但把"泄漏"当成"借用"的修法是把一个 bug 换成一个
  更安静的 bug，否掉。

## 3. 解决方式

把 `CString` 绑在**活得比这次调用久**的局部上，只对引用取指针：

```rust
let within = within.map(|path| CString::new(toml_path(path)).unwrap());
let kind = kind.map(CString::new).transpose().unwrap();
let token = unsafe {
    fam_search_begin(
        self.handle,
        query.as_ptr(),
        within.as_ref().map_or(ptr::null(), |path| path.as_ptr()),
        kind.as_ref().map_or(ptr::null(), |name| name.as_ptr()),
        limit,
        &mut error.value,
    )
};
```

`as_ref()` 之后闭包拿走的是 `&CString`，缓冲在整条语句结束时才释放——
这正好是 C ABI 承诺的借用期限（"参数指针在调用期间有效"）。

## 4. 解决方式效果评估

**正面证据。** 改完这两条测试立刻通过，并且 `a_scope_is_walked_and_reported_as_it_was_spelled`
从此才真的在断言逐字大小写（它前一版失败的原因就是这个悬垂指针，
而不是它想测的那件事）。`cargo test -p fam-ffi` → 18 项 v2 + 10 项 v1 全绿。

**代价。** 无。

**残留风险与局限（没解决什么）。**

1. **没有内存检查工具兜底**。这类错误只有 ASAN/`_CrtSetDbgFlag` 之类才可能在 CI 上稳定暴露，
   本项目现在没有 C 侧的内存检查（未引入，也不在这个增量里引入）。
2. **写法散在各处**。`crates/fam-ffi/tests/ffi.rs`（v1 测试）与
   `apps/fam-shell/tools/abi_smoke.cpp` 各自处理指针；测试侧没有一个"借用检查过的参数包"
   （比如一个持有 `Option<CString>` 并提供 `as_ptr()` 的小类型）。
   这次没抽象，因为只有一个调用点出错——**但同类错误只需要第二个调用点就会再出现一次**。
3. 同文件 `set_filter` 的 `into_raw()` 泄漏保留原样：它正确（不悬垂），只是每次漏一小块，
   测试里调用三次。改它属于无关清理。
