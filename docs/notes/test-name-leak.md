# 测试辅助函数靠 Box::leak 绕过它自己的签名

- 增量：1（fam-svc-index 骨架）
- 提交：`a09ac32`
- 涉及：`crates/fam-svc-index/src/collection.rs`

## 1. 描述问题

`collection.rs` 的测试需要一个造表的辅助函数，第一版签名是
`fn table_with(entries: &[(&str, Kind, u64)])`。两个测试用字面量元组调用它，没问题；
第三个测试（`fetch_returns_only_the_requested_window`）需要 10 个**运行时生成**的名字
`file00.txt`…`file09.txt`，`format!` 出来的是 `String`，与签名要求的 `&str` 在
生命周期上接不上——`String` 属于闭包作用域，切片要求它们活得比调用点更久。

第一版用 `Box::leak` 把它强行抹平：

```rust
let entries: Vec<(&str, Kind, u64)> = (0..10)
    .map(|i| (
        Box::leak(format!("file{i:02}.txt").into_boxed_str()) as &str,   // 每次跑测试泄漏 10 段
        Kind::Document,
        1,
    ))
    .collect();
```

## 2. 分析问题

**泄漏本身不致命，写法才致命。** 测试进程退出即回收，10 段短字符串没有任何可观测
后果。问题在于它把一次性的成本变成会传染的模式：`fam-bench` 与增量 3 的
"10 万行目录表"测试大概率要从这里抄构造代码，而基准里泄漏是真实的干扰——
`Vec` 不断增长、内存足迹变化会进 cache 与页错误的账。到那时再拆，就要在被测代码
里做手术。

**根因是签名过窄，不是生命周期难解。** `&str` 参数要求的其实只有两件事：能拿到
一段 UTF-16 可编码的文本。`String`、`&str`、`Cow<str>` 都满足。签名写成具体的
`&str` 又把返回值借出给切片，等于要求调用方保证比调用更长的生命周期，而生成名字
的调用方天然做不到——它只能在作用域里拥有这些 `String`。`Box::leak` 是用堆上
永久所有权换一次类型检查通过，代价是把设计缺陷转成了运行时债务。

**辅助函数根本不需要调用方持有名字。** `DirTable::push` 把名字拷进自己的 arena
（`self.names.extend_from_slice(name)`，`dir_table.rs:33`），返回后调用方就可以丢掉。
所以签名可以放宽成"任何能借出 `&str` 的东西"，所有权留在调用方局部，两边都自然。

**排除掉的方案。**

| 方案 | 否决理由 |
| --- | --- |
| `Vec<Cow<'static, str>>` 或改成按值收 `Vec<String>` | 让两个用字面量的调用点也要 `.to_string()`，为了一个调用点惩罚另外两个 |
| 测试里提前 `let names: Vec<String> = ...`，再 `names.iter().map(String::as_str)` 拼元组 | 可行，但把调用点的样板加到 6 行；根因仍在签名里 |
| 保留 `Box::leak` 并加注释说明"测试里可以" | 边界模糊的规则最容易被复制到不该用的地方 |
| 放宽签名为 `impl AsRef<str>`（**选定**） | 调用点零改动，泄漏消失，静态与生成两类输入都能用 |

## 3. 解决方式

```rust
fn table_with(entries: &[(impl AsRef<str>, Kind, u64)]) -> Arc<DirTable> {   // collection.rs:133
    let mut table = DirTable::new(1, PathKey::of(std::path::Path::new("D:/x")), None);
    for (name, kind, size) in entries {
        let item = Item { /* ... */ };
        table.push(&name.as_ref().encode_utf16().collect::<Vec<u16>>(), item);
    }
    Arc::new(table)
}

let entries: Vec<(String, Kind, u64)> = (0..10)
    .map(|i| (format!("file{i:02}.txt"), Kind::Document, 1))
    .collect();
```

生成的名字由 `entries` 自己拥有，随测试函数结束一起释放；`&[(String, ...)]` 与
`&[(&str, ...)]` 都满足 `impl AsRef<str>`，所以另外两个用字面量的调用点一字未改。

## 4. 解决方式效果评估

**行为不变，泄漏消失。** 三个调用点、7 个断言全部原样通过：
`fetch_returns_only_the_requested_window`（窗口切片与越界截断）、
`descending_keeps_the_name_tie_break_ascending`、`filters_narrow_the_visible_set`。
workspace 162 个测试通过，clippy 与 rustfmt 干净。测试可泄漏内存无法在
`cargo test` 里直接断言（没有分配计数器），故本条不声称"实测泄漏字节数为 0"；
判断依据是代码事实：`Box::leak` 已从仓库消失（`grep -rn "Box::leak" crates apps` 无结果），
`entries` 的所有权归局部 `Vec`。

**传染性已断。** 现在从 `table_with` 复制构造代码到基准或增量 3 的大表测试，
唯一能写的就是"给一个 `AsRef<str>` 的来源"，不会把 `Box::leak` 一起带走。这条是
本次修复真正买到的东西。

**代价与诚实定位。** 泛型签名让 `table_with` 在每个单态化处生成一份代码（仅测试
构建，无生产影响）。这是一次**可维护性修复，不是缺陷修复**：改前没有任何用户可见
行为是错的。把它写进文档的理由是记录"为什么这里不许再出现 `Box::leak`"，
避免下一次遇到生命周期不匹配时又用同一个手法抹平。
