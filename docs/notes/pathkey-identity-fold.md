# 路径身份键会合并两个不同的文件

- 增量：1（词汇表上收 fam-core）
- 提交：`a09ac32`（实现）、`d39011b`（同批的纯格式提交，无关）
- 涉及：`crates/fam-core/src/pathkey.rs`、`crates/fam-view/src/model.rs`、`crates/fam-svc-store/src/store.rs`、`crates/fam-sched/src/batch.rs`、`crates/fam-svc-walk/src/walk.rs`

## 1. 描述问题

FAM 用一个字符串键代表"哪个文件"。它是标签表的列主键（`sqlDesign.md`），
也是目录排除判定、批处理去重与排序、视图行定位（Qt 侧 `KeyRole`）的唯一依据。
键算错，等于两个文件共享一份身份。

增量 0 时该键由三步算出（`b611a6e:crates/fam-core/src/pathkey.rs:11-23`）：

```rust
let raw = path.to_string_lossy();          // 第 1 步：UTF-16 路径 → UTF-8 字符串
let raw = raw.strip_prefix(VERBATIM_PREFIX)...unwrap_or(&raw);
let mut key = raw.replace('\\', "/").to_uppercase();   // 第 2、3 步
while key.len() > 1 && key.ends_with('/') { key.pop(); }
```

两条独立的原因使它把不同的路径折成同一个键：

1. **`to_string_lossy` 丢弃信息。** NTFS 允许文件名里出现孤立代理项（单个
   `0xD800`–`0xDFFF`），UTF-8 无法表示，Rust 一律替换成 U+FFFD。两个名字只差
   那一个代理项时， lossy 结果完全相同。
2. **`str::to_uppercase` 会改变长度。** 它做的是完整 Unicode 大写映射，
   `ß → SS`、`ﬁ → FI`。Windows 侧的大小写不敏感来自 1:1 的 upcase 表
   （`RtlUpcaseUnicodeChar`），`ß` 不等于 `SS`，因此 NTFS 视为两个名字的
   `straße` 与 `STRASSE` 在这里变成同一个键。

两者都不是显示问题：键参与相等比较与哈希，合并即误认。

## 2. 分析问题

**影响面。** 计划按"10 处调用点"估算，实测偏低：`b611a6e` 上 `path_key(` 共
39 处，其中生产代码 22 处、5 个文件（`store.rs` 9、`model.rs` 7、`walk.rs` 3、
`batch.rs` 2、`config.rs` 1），另有测试 17 处。改动必须逐字保住既有语义，
否则标签与排除规则会静默漂移。

**危害的方向不对称。** 折叠过度（把两个文件并成一个键）会让标签挂到别的文件上、
让排除规则误伤、让两个目录共用一个缓存槽——都是不可见的错误数据。折叠不足
（同一文件因大小写写法不同得到两个键）只会多一次未命中、多一个缓存槽。
因此任何折中都必须选"宁可拆、不可并"。这条原则决定了方案取舍。

**候选方案。**

| 方案 | 做法 | 否决理由 |
| --- | --- | --- |
| A | 保留 `String` 键，只把 `to_string_lossy` 换成严格转换 | `String` 装不下孤立代理项，严格转换只能报错或改类型；改类型即方案 B |
| B | `Vec<u16>` + 贴平 NTFS upcase 表 | 正确性最好，但要引 `windows-sys` 或 Unicode 上查表；fam-core 的约定是"无平台依赖"，且 upcase 表需要独立测试集。留给平台层，不阻塞本增量 |
| C | `Vec<u16>` + 仅折叠 ASCII（**选定**） | 消除两类合并；代价是非 ASCII 的大小写变体不再归一，属可容忍的"拆"方向 |

**成本落点。** 键在两类位置被算：遍历时每个**目录**一次（`walk.rs:73-79` 的
`excludes`，且被 `!excluded_prefixes.is_empty()` 挡住）；以及每个**显示行**一次
（`Row::new`，`fam-view/src/model.rs:27`）。前者量级无关紧要，后者是本增量
唯一需要认真核算的性能面。

## 3. 解决方式

身份改为 UTF-16 新类型，折叠就地完成，字符串形态降级为派生的显示/线格式。

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PathKey(Vec<u16>);            // pathkey.rs:24

fn fold_name_only(units: &mut [u16]) {   // pathkey.rs:115
    for unit in units.iter_mut() {
        if *unit == BACKSLASH {
            *unit = SLASH;                       // 分隔符统一
        } else if matches!(*unit, ASCII_A..=ASCII_Z) {
            *unit -= ASCII_CASE_DELTA;           // 只做 ASCII 大小写，1:1，不改变长度
        }
    }
}

pub fn path_key(path: &Path) -> String {         // pathkey.rs:138
    PathKey::of(path).to_lossy_string()          // 旧名保留，22 处调用点零改动
}
```

配套三点：

- `child_units(name)`（`pathkey.rs:34`）在已折叠的父键后追加名字片段，只对新增
  片段做折叠——父目录路径不再被重走一遍，这是"每行一个键"改为"每目录一个键 +
  每行一次追加"的基础。
- `raw_name_units(path)`（`pathkey.rs:86`）给出未折叠的末段 UTF-16，搜索命中携带
  它，界面显示的就是文件系统报出的原名。
- 删除零调用点的 `is_within_path`；边界判断改为在 UTF-16 单元上做的
  `is_within_units`，`is_within(&str, &str)` 保持原语义（先转单元再比）。

## 4. 解决方式效果评估

**合并问题已消除（测试证据）。** fam-core 现在 27 个测试，其中四条直接钉住本问题：

- `lone_surrogate` 用例 `lone_surrogates_keep_distinct_identities`：`0xD800` 与
  `0xD801` 两个名字 `assert_ne!(first_key, second_key)`，同时
  `assert_eq!(first_key.to_lossy_string(), second_key.to_lossy_string())`——
  后者正是旧键的失败现场，用断言留在测试里。仅 `#[cfg(windows)]`。
- `non_ascii_case_never_merges_two_names`：`D:\straße` 与 `D:\STRASSE` 键不同，
  `D:\café` 与 `D:\CAFÉ` 键不同，`D:\照片` 与 `d:/照片` 键相同。
- 既有语义逐字保住的回归测试：`separators_and_case_normalise_to_one_key`、
  `trailing_separators_collapse`（补了 `path_key("/") == "/"`）、
  `device_prefixes_are_stripped`、`sibling_with_shared_prefix_is_not_inside`、
  `drive_and_unix_roots_contain_their_children`。
- 复现证据：把增量 0 的实现原样抄成独立程序跑，输出
  `sharp-s: "D:/STRASSE" vs "D:/STRASSE" -> equal=true` 与
  `surrogate: "D:/WEIRD/\uFFFDA" vs "D:/WEIRD/\uFFFDA" -> equal=true`，
  即两类碰撞在旧实现上都真实发生。同一输入走新实现后两组均 `equal=false`。

**性能代价（实测 + 折算）。** 用同一份独立程序（`-O`，20k 条 83 字节路径，
5 轮取最快）测三种算法：

| 算法 | 每条 | 20k 条最快值 |
| --- | --- | --- |
| 旧 `path_key`（`to_string_lossy` + `replace` + `to_uppercase`） | 241ns | 4.82ms |
| 新 `path_key`（`encode_wide` + 就地折叠 + `from_utf16_lossy`） | 855ns | 17.11ms |
| 每行追加到已折叠父键（`child_units` 的等价操作） | 153ns | 3.05ms |

即：**字符串形态的键贵了 3.5 倍**，而新架构实际要用的追加路径比旧实现还便宜。
折算到落点：

- 每目录一次——78k 树有 4,730 个目录，+2.9ms，被 syscall 支配；`fam-bench check`
  实测四例全 ok（floor 比值 flat 0.96x / mixed 0.86x / tree 0.60x / volume 0.57x），
  未观测到回归。**注意**：tree/volume 两项优于 1.00x 的部分应归因于文件缓存变暖
  （本增量没碰被测路径），不计为性能收益。
- 每显示行一次——`Row::new` 仍在算 `path_key`，78k 行按上表是 **+48ms。这是推算值**，
  公式：`(855-241)ns × 78k`，尚无对应的 bench 用例。它随增量 3 用
  `(dir, name_off)` 取代行键而消失，故本轮不加 ASCII 快路径：那会引入第二份折叠
  实现，而"同一语义只有一处实现"正是本次要立的规矩。若增量 3 之后行键仍未消失，
  再把这条改成实测并重新评估。

**残留风险（明确接受）。**

1. 非 ASCII 大小写变体各占一号：`café` 与 `CAFÉ` 是同一目录的两个键。表现为一次
   缓存未命中或同一目录两个槽，不产生错误数据。要消除需要 NTFS upcase 表（方案 B）。
2. 配置文件里若用非 ASCII 大写形式书写排除前缀，可能匹配不上——前缀与路径都出自
   `path_key`，两侧一致即可，风险仅出现在手写配置与文件系统实际大小写不同时。
3. 非 Windows 路径下 `units_of` 走 `to_string_lossy`（`pathkey.rs:93`），孤立代理项
   精度回落到旧行为。本项目只发布 Windows，但该代码路径存在，测试里
   `#[cfg(windows)]` 的用例在非 Windows 上被跳过，不能当作跨平台保证。
