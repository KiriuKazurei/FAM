# 搜索结果的路径来自折叠身份：目录会被印成小写

- 增量：5b（下）
- 提交：本次 `feat(ffi)` 搜索分页
- 涉及：`crates/fam-core/src/search.rs`、`crates/fam-core/src/pathkey.rs`、
  `crates/fam-svc-walk/src/search.rs`、`crates/fam-ffi/src/search.rs`

## 1. 描述问题

v2 的搜索行把**整条路径**装进记录里那个 normally 只放文件名的 name 字段
（结果集跨目录，名字必须自己说清在哪儿）。第一版实现是拼出来的：

```rust
fn path_units(hit: &SearchHit) -> Vec<u16> {
    let mut path = hit.dir.units().to_vec();   // ← dir 是 PathKey
    ...
    path.extend_from_slice(&hit.name);
}
```

`PathKey` 按定义折叠 ASCII 大小写（`crates/fam-core/src/pathkey.rs:125` 的 `fold`、
`:142` 的 `fold_unit`），所以搜 `Invoice-2026.pdf` 拿到的行会写成
`c:/users/kirig/appdata/local/temp/…/Invoice-2026.pdf`：**目录部分被小写化**，
而同一串还要被壳拿去 `ShellExecute`。这条不是理论问题——写完没跑 UI 就发现了，
是在读自己代码时抓到的。

同一个洞的第二处：`SearchRequest.scope` 的类型是 `Option<PathKey>`，
而 walk provider 拿它当重举的根（`policy.roots = vec![scope.to_path_buf()]`），
于是**限定目录本身**也以折叠形态出现在结果里。

## 2. 分析问题

折叠是为"两个拼写是同一个文件"服务的（见
[pathkey-identity-fold.md](pathkey-identity-fold.md)）。对"是不是同一个"它是对的；
对"显示给人看、交给 API 打开"它是错的。这两件事共用一个类型，就一定会在某一侧出错。

**v1 为什么没这个问题**：v1 的 JSON 同时给了两份——`key`（折叠）和 `path`（逐字）。
2026-09-21 跑 `abi-smoke` 的输出里能直接看到：

```
"key":"C:/USERS/KIRIG/APPDATA/LOCAL/TEMP/FAM-GATE-SEARCH/CASINGCHECK/PROBE-CREATED-BY-SMOKE.TXT"
"path":"C:\\Users\\Kirig\\AppData\\Local\\Temp\\fam-gate-search\\CasingCheck\\probe-created-by-smoke.txt"
```

流式契约 `SearchHit` 当初只需要身份，逐字形态只保留了 base name
（`raw_name_units` 的注释就写着"what a search hit carries"）。
**换契约时丢的是父目录的逐字形态**，而没人读 `hit.dir`，所以编译器不报错。

排除掉的方案：

- **只显示 base name、不显示目录**：搜索结果跨目录，不写路径就分不清两个同名文件，否掉。
- **在 FFI 侧把 `dir` 的"大小写还原"回来**：信息已经没了，任何还原都是猜，否掉。
- **给 `SearchHit` 再加一个 `path`，同时留着 `dir` 和 `name`**：七个字段里两个可推导
  （`dir == key.parent()`，`name == path` 的尾），否掉——留一个逐字整路径 + 一个折叠身份就够。

## 3. 解决方式

`SearchHit` 变成"身份 + 逐字路径 + 属性"（`crates/fam-core/src/search.rs:64-76`）：

```rust
pub struct SearchHit {
    /// Folded identity, for a caller that has to match the hit against the directory cache.
    pub key: PathKey,
    /// Verbatim UTF-16 path as the filesystem reported it, separators included.
    pub path: Vec<u16>,
    pub kind: Kind, pub size: u64, pub mtime_ms: i64,
}
```

`dir` 由 `key.parent()` 得到，base name 是 `path` 最后一个分隔符之后的部分。
新增 `fam_core::raw_path_units`（`crates/fam-core/src/pathkey.rs:109`），
与 `raw_name_units` 同层，唯一区别是整条路径。

`SearchRequest.scope` 改成 `Option<PathBuf>`（`crates/fam-core/src/search.rs:18`），
provider 自己折一次做身份比较，根用调用方那份逐字字符串
（`crates/fam-svc-walk/src/search.rs:284-287`）：

```rust
let mut inside = None;
if let Some(scope) = &request.scope {
    policy.roots = vec![scope.clone()];   // 逐字：这是要显示的那份
    inside = Some(PathKey::of(scope));    // 折叠：这是要比的那份
}
```

FFI 侧 `push` 直接把 `hit.path` 当 name；Qt 侧 `FamBridge::searchPage`
（`apps/fam-shell/src/bridge.cpp`）把它拆成 `row.path`（整条）与 `row.name`（尾段），
列表显示尾段、打开用整条。

## 4. 解决方式效果评估

**正面证据。**

- `a_scope_is_walked_and_reported_as_it_was_spelled`（`crates/fam-ffi/tests/v2.rs`）：
  树里造一个 `MixedCase` 目录，断言行里含 `MixedCase`。折叠过的形态是 `MIXEDCASE`，
  这条断言会红——它是这次唯一直接钉住大小写的测试。
- `the_provider_streams_hits_and_counts_every_entry_it_saw`（`crates/fam-svc-walk/src/search.rs`）：
  `assert_eq!(path_buf_from_units(&hit.path), dir.path().join("Report-2026.pdf"))`，
  即逐字整路径与文件系统给的一致；同一测试里 `hit.key.parent()` 仍等于折叠后的父目录。
- `cargo test --workspace`：265 项全绿，0 失败。
- 真机：`--self-check --query invoice` 的截图里三行显示 `Invoice-2025.pdf  2 B  [document]`，
  状态行 `3 row(s) for invoice`（`C:\Users\Kirig\AppData\Local\Temp\fam-search.png`）。

**代价。** 每条命中多拷一次整路径（`raw_path_units` 一次 `encode_wide`），
少一个 `PathKey` 与一个 base-name `Vec`——净变化接近零，但**没有单独实测**每命中成本。
`SearchRequest.scope` 换类型波及 4 处代码 + 3 处测试断言。

**残留风险与局限（没解决什么）。**

1. 折叠身份仍然在 `key` 里，谁拿 `key.to_lossy_string()` 去显示就重新掉进同一个坑。
   这次没加编译期防护（比如 newtype），只写进了字段注释和本文。
2. 限定搜索的根如果被 Windows 规范化过（8.3 短名、大小写不敏感的卷映射），
   结果里显示的是**调用方给的那份**，不是文件系统那份。二者指向同一文件，显示不冲突，
   但没有校验。
3. 增量 6 接 Everything 时，它的结果字符串必须走同一条规则（逐字路径 + 折叠 key）。
   这条约束目前只写在文档里，没有测试能拦住它。
4. `fam_search_begin` 的 `within` 参数在 FFI 里直接 `request.scope(within)`，
   没有做"这个目录存在吗"的校验：不存在的目录会走成"0 行、complete"，
   而不是一句"这个范围不存在"。留给错误通道（见
   [cancelled-search-had-no-state.md](cancelled-search-had-no-state.md) 残留 1）。
