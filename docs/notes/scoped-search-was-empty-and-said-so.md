# 限定到子树的搜索被配置根清空，而且说"搜完了"

- 增量：6d
- 提交：`e614809 fix(index): 指名了子树的搜索，不再被配置根悄悄清空`
- 涉及：`crates/fam-svc-index/src/router.rs:53`、`crates/fam-core/src/prune.rs:111`、
  `crates/fam-ffi/tests/v2.rs`

## 1. 描述问题

`3456ced` 给路由器加了"出口处统一执行范围规则"那一步。规则本身是：

```rust
fn allows(&self, key: &PathKey) -> bool {
    if self.prune.excludes_key(key) {
        return false;
    }
    self.roots.is_empty() || self.roots.iter().any(|root| root.is_within(key))
}
```

它对**每一条**请求都问"这行在不在配置根里"，包括调用方自己指了子树的那些。于是
`fam_search_begin(query, within)` 与 `fam search --in <dir>`，只要 `<dir>` 不在
`search_roots` 里，就是：后端正常走完、`matched` 正常计数、行在最后一道被丢掉，
返回 **0 行**，`state=READY`，不带 `FLAG_INCOMPLETE`。

写不下这件事的另一半在同一个提交的注释里——`roots` 字段的文档当时就写着
"Roots **an unscoped** search may answer for"。注释知道，代码不知道。

谁能看见：Qt 壳今天固定传空 `within`（`apps/fam-shell/src/mainwindow.cpp:189`），
界面上看不见；看得见的是 ABI（`within` 是文档化入参）和 CLI 的 `--in`。

## 2. 分析问题

**方向性是这道题的重点。** 0 行 + "已覆盖" 对一个文件管理器是最坏的一种错：
用户据此判断"这个文件夹里没有"，然后新建、覆盖、或者删掉旁边那个。
反方向（多给几行）用户看得见并能自己划掉。所以这道题不能只记档，得改。

**怎么撞上的**：把 CLI 从 v1 面迁到路由器的时候。v1 的 `WalkBackend::search` 是
`policy.roots = vec![within]` 然后**不再复查**，所以 `--in` 一直是好的；
迁到路由器之后同一命令同一棵树会变成 0 行——不是路由器新增了限制，
是它把一条只对"不限定"成立的限制当成对所有人成立。

**排除规则在边界处必须分开问**，因为两个列表断言的不是同一件事：

- `excluded_dirs`（绝对前缀）说的是"永远别给我看 `D:/secret`"。从里面发起的搜索仍然在
  里面，所以这条要看整条路径。
- `excluded_names`（`.git`、`node_modules`、`target`…，见
  `crates/fam-core/src/config.rs:27-33`）说的是"别往下走"。而 `walk` 从来只问它**要进入**
  的目录，从不问给它的那个根。把 `D:\proj\node_modules` 指为搜索起点不算走进去。

只改前一件事会立刻踩后一件事：`fam search react --in D:\proj\node_modules` 会变成 0 行，
而 v1 的 CLI 是有结果的（上面那段 v1 代码就是证据）。所以两条规则不能共用一次判断。

**这台机器上量到的一对真实数字**（跑闸门时撞上的，不是设计出来的）。临时语料树放在
`target/gate-6d/tree`，而 `target` 正在默认排除名里。同一份配置、同一个词：

| 命令 | 输出 |
| --- | --- |
| `fam --config … search notes`（不限定） | `backend=walk hits=0` + `matched=1 visited=5` |
| `fam search notes --in …/tree`（限定到那棵树） | `backend=walk hits=1` |

第一行是规则**应该**生效（整棵树都在被排除的名字下面）；第二行靠的正是边界那条新规则。
需要说清的是：这两行都是在**改之后**量的。改之前第二行也会是 0——这是从规则推出来的，
不是量出来的；要量它得把修复临时改回去，而我不愿意在那台机器上再跑一遍已知错误的代码。

排除掉的候选方案：

1. **求交**（`within` 必须落在某个根里才放行）：就是现在这个 bug，只是变成显式的。
2. **干脆去掉根列表**：不限定的搜索就再没人管了——Everything 索引整卷，
   用户配置 `search_roots = [D:/Docs]` 却能在结果里看到 `E:` 上的东西。
3. **只在 `covers` 上修**：`covers` 决定问不问，管不到已经交回来的行，
   而越界的行恰恰是问过之后才出现的。

## 3. 解决方式

**一条规则：指名的子树替换根列表**（`crates/fam-svc-index/src/router.rs:53`）：

```rust
fn allows(&self, wanted: Option<&PathKey>, key: &PathKey) -> bool {
    match wanted {
        Some(scope) => scope.is_within(key) && !self.prune.excludes_below(scope, key),
        None => {
            !self.prune.excludes_key(key)
                && (self.roots.is_empty()
                    || self.roots.iter().any(|root| root.is_within(key)))
        }
    }
}
```

`named` 在每次搜索开始时折一次（`router.rs:184`），因为每一行都要和它比，
而 `SearchRequest::scope` 保留的是调用方自己的拼法（那是给重启遍历用的）。

**排除规则按边界切**（`crates/fam-core/src/prune.rs:111`，`Prune::excludes_below`）：
前缀规则照旧问整条键，名字规则只问边界**以下**的成分——两个键都已经折叠过，
所以 `strip_prefix` 是真的前缀关系；`parent()` 再去掉最后一段，因为那是文件自己的名字。
落在边界之外的行退回整条判断（`excludes_key`），而不是放行——
那等于对一条它没看过的路径说"清白"。

顺带收掉上一增量记下的一半洞：指名的 scope 现在**逐行**核对返回路径是否真在其下，
所以 Everything 穿过 junction 交上来的界外行会在出口被丢掉。

## 4. 解决方式效果评估

**正面证据。**

- `crates/fam-svc-index/src/router.rs`：`a_named_subtree_replaces_the_configured_roots`
  一条测试里同时钉住"同一行、无名时出界、有名时入界、且指名的子树自己是边界不是提示"；
  `a_scoped_search_outside_the_configured_roots_still_answers` 和
  `a_scope_rooted_inside_an_excluded_directory_keeps_its_own_rows` 走的是假后端 +
  真实 `drain`，后者同时断言"边界的同名不算下降"与"边界以下的 `.git` 仍算"；
  `crates/fam-core/src/prune.rs` 三条：
  `a_named_boundary_is_not_excluded_by_its_own_name`、
  `an_excluded_prefix_holds_inside_a_named_boundary`、
  `a_path_outside_the_boundary_is_answered_for_rather_than_silently_passed`。
- 端到端（C ABI，不是 Rust 内部）：`a_search_scoped_outside_the_configured_roots_still_finds_its_rows`
  断言限定到另一棵树时 `total_rows == 5` 且**不带** `FLAG_INCOMPLETE`，
  同一连接不限定时 `total_rows == 3`。
- 真机：`fam search notes --in D:/Datas/fam-gate-6d/tree` → `hits=1`；
  `--self-check --query notes` → `search settled at 386ms: 1 row(s)`，截图里是
  `notes.txt  1 B  [document]`。
- 全仓 334 条绿，clippy/fmt 干净。

**代价。**

- `excludes_below` 每行分配一次 `to_path_buf()` + 一次 `to_lossy_string()`（前者 `excludes_key`
  本来就在付）。**未实测**每行贵了多少；上界是调用方设的 limit，不是磁盘上的东西。
- 两条排除规则现在有两个入口谓词（`excludes_key` / `excludes_below`），
  读代码的人要先知道它们断言的是两件事。文档写在方法上。
- "根列表"与"指名的子树"是两套语义，`Scope::allows` 变成一个 match 而不是一个表达式。

**残留风险与局限（没解决什么）。**

1. **不限定的搜索，如果某个配置根本身就在被排除的名字下面，仍然返回 0 行**
   （`excludes_key` 看整条路径的祖先，而 `walk` 从不问给它的根）。
   这次不动它：没有任何一份配置这么写过，而且真这么写的话遍历那边也会给出空表——
   两边一致地空，比一边有一边空好。要修就得让 `Scope` 也带上"每次搜索的边界"，
   那是把根列表从一组路径改成一组 (路径, 边界) 对。
2. **junction 的另一半**：`covers` 仍然只看卷根。一个穿过 junction/挂载点进到**未索引卷**
   的 scope，Everything 会答得短而自称完整——界外的行现在会被丢掉（不再多报），
   但"少报还报 complete"这一半没修，修它需要知道索引里到底有什么，这台机器上没有服务可问。
3. `excludes_below` 依赖"两个键都折叠过所以前缀关系精确"。如果哪天 `PathKey` 改成不折叠
   或者分隔符可变（`\\?\` 前缀），`strip_prefix` 会静默失败并退回整条判断——
   那条 fallback 是安全的（更严），但不会有人知道规则降级了。
4. 界面侧仍然看不见这个 bug：壳还没把"当前目录"传成 `within`。
