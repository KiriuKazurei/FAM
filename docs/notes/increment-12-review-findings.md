# 增量 12 复查的账：门的第四问在链接之后没再问过，标签库把这句话证伪了

- 增量：12（复查）
- 提交：`fix(mcp)`（本文件之前一笔）
- 复查面：`crates/fam-svc-mcp/{gate,server,mutations}.rs`、`apps/fam-mcp/src/main.rs`、
  两个测试文件，以及它们下面的 `crates/fam-svc-index/src/collection.rs`、
  `crates/fam-svc-router` 那条 `allows()`
- 方式：两个只读 agent（一个看门与工具的可达性，一个看协议/接线/测试能不能失败）。
  **每条指认都先到行、再到跑**——agent 说"这里是洞"不算数，把守卫关掉看它红才算。

## 1. 一句话结论

十三处改了，一处驳回，八处确认不修并标归属。
两条是**安全级**的（一张 admin 票能借 junction 读到凭据文件与标签库；
一张只授权一个根的票能搜出另一个根里的文件名），
三条是**我自己的测试不合格**（其中一条写第一版时也不合格，A/B 才暴露）。

最该记住的一件事不是洞本身，是**它的形状**：四问的顺序是对的，
但第四问走完链接就停了，而第二问与第三问**只在调用方给的拼写上问过**。
"顺序不能减一步"这句话，我按"步数"实现了，没按"每步问的是不是同一个位置"实现。

## 2. 改了的十三处

### 2.1 门：解析之后没再问的那两问（安全，严重）

`crates/fam-svc-mcp/src/gate.rs` 的 `first_escape` 原来找到**第一个**链接就 `break`，
只问一句"这个链接解析后还在根里吗"。三处后果：

1. **两个链接接力**：`inbox/via-one → inbox/inner`（在根里，放行），
   `inner/via-two → outside`（没人看过）。A/B（把 `break` 加回去）：

   ```
   two links carried the token out of the grant:
   {"bytes":12,"path":"C:/.../tree/inbox/via-one/via-two/crown-jewel.txt","text":"not granted\n"}
   test result: FAILED
   ```

2. **落点在排除目录里**：`inbox/st → <temp>/state`，请求 `inbox/st/auth/mcp-tokens.toml`。
   拼写在授权内、不在排除前缀里、解析后确实还在根里——三条都过。
   A/B（把每次跳转之后的 `judge` 关掉，`hops > 0 && false`）：
   `a_junction_is_not_a_door_into_the_programs_own_state` 当场红。
3. **对不齐的拼写直接绕过整条走查**：`below()` 在 `strip_prefix` 失败时把**整条请求**的组件
   叠到根上，拼出一条谁都没问过的路径（`<root>\C:\...`），在它上面永远找不到链接，于是"通过"。
   `covers` 用折叠身份，所以 `\\?\C:\dir` 对 `C:\dir` 是**同一个授权**却 strip 不动——
   这条是 agent 没提、我读它时自己撞出来的。

改后的形状：走完所有组件；每遇到一个链接就地解析、把游标换成解析结果、**继续走**；
每一次换位置（以及链接以下的每一步）都再问一遍"还在根里吗 / 是不是我自己的状态吗"（`judge`），
外加 32 跳上限挡 `a→b→a`；对不齐时返回 `Option::None`，调用方解析整条路径后判断那一个结果。
测试：`a_link_below_a_link_is_still_a_link`（含 `\\?\` 那条腿）、
`a_junction_is_not_a_door_into_the_programs_own_state`（三条路径都要拒，
且**句子必须是 own state 而不是 scope**，否则等于把新口子中缀掉）。

### 2.2 `fs_search` 只问了第一个根（安全，严重）

`server.rs` 原来 `self.admit(Verb::FsSearch, &self.config.search_roots[0])`，
而 `crates/fam-svc-index/src/router.rs:58` 的 `allows()` 在没指名作用域时**让每一个配置根都出行**。
注释写的正是"要覆盖所有根否则一张授权一个文件夹的票能学到另一个里的文件名"——
代码没做到，注释替它背书。实测（把判断改回 `[0]` 之后跑新测试）：

```
the ungranted root was searchable:
{"hits":[{"path":"C:\\...\\.tmp376XjB\\quarterly-report.txt","size":2}],"matched":1,"visited":10}
```

改成"任何一个根过不了门就整条拒"。顺手把**拒绝的话里不再报名字**：
原来那句把 `search_roots[0]` 的完整路径交给了一个刚刚没覆盖它的凭据。

**这一条的测试写第一版时也不合格**：我拿 `Place::Inbox`（授权 `tree/inbox`）配
`search_roots = [tree, outside]`，于是 `[0]` 那个根**根本不在授权里**，
新代码和坏代码都拒绝它——测试绿得毫无意义。改成 `Place::Root`（授权恰好等于 `[0]`）之后
才第一次能失败。这类"不修也过"的测试是我自己那三条里的第三条（前两条见 §4）。

### 2.3 `tags.db` 的内容能被读走（安全，严重；同时推翻我 note 里的一句话）

`mcp-server-four-questions-before-the-disk.md` §4.3 第 10 条原来写着
"`tags.db` 因为不是 UTF-8 被挡住（实测报 32 768 字节）"。
**那句话是错的**，错在我拿一个**有数据的**库去试。
一个刚建好、页内大量 NUL 的库**是合法 UTF-8**：`fs_read` 返回过
`{"bytes":4096,"text":"SQLite format 3\u0000…"}`——标签与备注整表外流。

修法选在门上：`Gate` 多一张 own-file 列表（`tags.db` 与 SQLite 的 `-wal` / `-shm` 三个精确路径），
`own_state()` = `prune.excludes(path) || 命中那张表`，两个问处都用它。
A/B（构造 `Gate` 时把那张表传空）：

```
C:\...\state\tags.db was readable: {"bytes":4096,"text":"SQLite format 3\u0000\u0010…"}
test result: FAILED
```

**为什么不是在 `Prune` 里注入 `database_path.parent()`**：先这么做了，
`crates/fam-ffi/tests/v2.rs` **六条搜索测试当场红**——那个布局里库文件与被搜的树同层，
排除前缀罩住了整棵根，症状是"搜索 0 行且自称覆盖完全"。
**那条失效方向比原来的泄露坏得多**（静默空结果 vs 明确拒绝），所以退回"只按文件排"，
`git checkout -- crates/fam-core/src/prune.rs` 还原（还原后与 HEAD 逐字节一致）。
这条决策值得单独留在账上：**"更一般的修法"在这里是更差的修法**。

### 2.4 一个 `browse` 凭据能让进程死在写日志的路上（可用性，中）

`args_of()` 尾部原来 `if text.len() > 200 { text.truncate(200) }`——
`String::truncate` 按**字节索引**切，不在字符边界上就 panic，
而 `fs_search` 的 query、`fs_move` 的两端都是调用方给的文本。
panic 发生在 `record()` 里，也就是**在写那一行日志的路上**：调用没留痕，进程没了，
`apps/fam-mcp` 没有 `catch_unwind`。A/B（把按字符的 clip 换回按字节的 truncate）：

```
thread 'a_query_in_any_language_is_clipped_not_fatal' panicked at crates\fam-svc-mcp\src\server.rs:740:17
```

改成 `fam_svc_audit::clip(&text, 200)`（审计层本来就有按字符并补 `~` 的那一个）。
测试跑 199 / 200 / 201 / 500 个 `字` 四种长度，再断日志里那句是**截过的**
（断 `chars().count() <= 200`——第一版我断的是 `len()`，字节数，597，
一个把"按字符"和"按字节"混起来的断言，正是这个 bug 本身；自己纠正）。

### 2.5 `fs_read` 按 stat 的大小判断，按后来实际的大小分配（可用性，中低）

`metadata` 与 `read` 之间文件可以长。改成 `File::open` + `take(MAX_FILE_BYTES + 1)` +
`read_to_end`，读不到那么多就说"长过了上限"。**没解决**的是名字到打开之间的替换（§5 第 7 条）。

### 2.6 列举的分组做在窗口上（正确性，中）

§2.1 上面那条测试红过一次之后，主 note 的 §2.8 也被迫改写：
"分组在取窗口之前做"这句话当时**是假的**——代码是 `fetch(0, offset+limit)` 再组窗口再 `skip(offset)`。
修法是把分组做成**排序的第一键**（`Collection::set_grouped`，默认 `false`，
壳与 ABI 面上零行为变化），`fs_list` 只说"要分组"，窗口仍是 `fetch(offset, limit)`。
A/B：`set_grouped(false)` → `a_folder_that_does_not_fit_one_page_still_leads_with_its_folders` FAILED。

### 2.7 其余六处（小的，逐条）

| 处 | 原来 | 现在 | 钉它的 |
| --- | --- | --- | --- |
| `initialize` 的 `protocolVersion` | 把客户端要的版本**原样回显**（与 `server.rs:30-32` 那句注释相反：注释说"不支持就把我们的 newest 回给他，让他自己决定走不走"） | 永远回我们说的那个 | `the_handshake_answers…` 多问一个 `1999-01-01` |
| `tools/call` 不带 `id` | `id?` 直接 `None`：**不执行、不回话、不留痕**，客户端干等 | 回 `-32600`（并断没有工具行） | `a_tool_call_without_an_id_is_answered_rather_than_dropped` |
| `is_link` 的口径 | 只认 symlink/mount-point 标签，而列举给调用方报的是任意 reparse 点 | 认 `FILE_ATTRIBUTE_REPARSE_POINT`（收紧；见 §3 第 3 条） | 两条 junction 测试 |
| `mutations.rs` 的模块文档 | 写着"`fs_write` 与 `fs_create` 不会凭空造父目录"，而 `"dir"` 分支就是 `create_dir_all` | 文档按两个分支分开说清 | 无（文字） |
| `fs_search` 的拒绝话 | 带出 `search_roots[0]` 全路径 | 不报名字 | `an_unscoped_search_is_refused…` 里那句 `!message.contains(second)` |
| `fs_delete` 判"空" | `entries.flatten().next().is_some()`——一个**读失败**的目录项被丢掉，等于"它不存在" | `entries.next().is_none()`：报错不是空的证据 | 无新增（既有的 `a_delete_will_not_take…` 仍绿） |

## 3. 驳回的一条，以及两条"指认对、说法要改"的

1. **驳回**：agent 说 `initialized` 写了没人读，所以"不握手也能调工具"是缺陷。
   到行核过：`server.rs:90/159/185` 确实只写不读——**事实对**，但按 MCP 的做法，
   服务器不强制这条顺序是**兼容性的**选择，而强制它会让"发完 initialize 就等着调工具"的
   客户端立刻不可用。这台机器上没有这样的客户端可测，所以不改，
   降级成 §5 第 1 条（不修 + 严重度 + 归属）。
   这不是"agent 错了"，是"这条不该在只有 stdio 的时候动"。
2. **指认对、说法要改**：agent 说"`mklink` 造的 junction 能让 `fs_write` **改写凭据文件**"。
   读路径能证（§2.1 第 2 条实测的就是读），写路径我没有实测——
   它走同一个 `admit`，所以同一条修复覆盖它；但**没测过就不写成"实测"**，
   账上记为"同一判断的另一侧，未单独复现"。
3. **未验证的收紧**：`is_link` 那处，agent 说"哪些别的 reparse 标签会重定向，未验证"。
   同意，所以按**最保守**的口径改（任何 reparse 点都当链接），
   并且**没有**在这台机器上造出非 symlink 的 reparse 点来证明旧代码会漏。
   这是一次预防性收紧，不是一次修好的证明——这个区别要写下来。

## 4. 我自己的测试里，三条不合格的（这是本次复查对我最有用的部分）

| # | 不合格的形状 | 怎么暴露的 |
| --- | --- | --- |
| 1 | `an_unscoped_search_is_refused…` 第一版：凭据不覆盖 `[0]`，于是新旧代码都拒绝，测试**永远绿** | 把被测的守卫改坏跑它 → 仍然 ok（这就是"闸门够不到坏路径"） |
| 2 | `a_folder_that_does_not_fit_one_page…` 第一版含一句 `assert!(seen.push(name) == () \|\| true)`（一句无意义断言）和一句 `seen[0] == "z-first"`（对排序的想当然：授权的 inbox 里本来就有 `empty`/`full` 两个目录，它们才是头两个） | 编译期没意见，跑起来红；红的方式让我重读了一遍断言而不是改代码 |
| 3 | `a_query_in_any_language…` 第一版断 `args.len() <= 200`——把**字节**当**字符**，正是这个 bug 的同一处混淆 | 修好之后测试反而红（597 > 200） |

另有两处**是我编辑文件时弄坏的**，记在这里因为它们是同一类风险：
一次 Edit 把 `crates/fam-svc-mcp/tests/gate.rs` 里一个函数的 `#[cfg(windows)] #[test] fn …() {` 三行
吞掉了（函数体悬空），一次把 `collection.rs` 的 `set_filter` 尾部 `self.rebuild(); }` 删掉。
两次都是**编译报错抓回来的**，不是靠肉眼复查。教训照旧：
往大文件里塞新函数时，`old_string` 必须带足上下文，改完立刻看编译而不是相信 success。

## 5. 确认不修的八条（严重度 / 归属）

1. **不强制 `initialized` 就能调工具**。严重度：低（stdio 面上只有一个客户端，
   而且它没有"绕过握手能获得什么"——每句话都要过门）。归属：HTTP 传输那次一起定。
2. **一条输入的长度没有上界**（`parse_line` 不限量，`lines()` 吞完为止）。
   严重度：中（远程一上就是现成的）。归属：与 HTTP 同一次，那时本来就要读长度、限体、定时。
   这次不改的理由是**换掉 `lines()` 会引入"截断后从哪儿重同步"这个新失效方向**。
3. **门贵了 12.8 µs**（246.53 → 259.32）：own-file 列表要三次 `PathKey` 比较，
   每次跳转要一次 `canonicalize`。严重度：低。归属：不修，这是闭洞的价格；
   真有批量需求时按 §"没解决"第 2 条（复用已经取到手那行里的 REPARSE 标志）省第 4 问。
4. **非 symlink 的 reparse 标签在这台机器上造不出来**，所以 §2.7 那处收紧没有正向证据。
   严重度：低。归属：真遇到时补一条 fixture（`ReparsePoint` 的手工构造要 `FSCTL_SET_REPARSE_POINT`）。
5. **`fs_read` 的名字→打开之间有替换窗口**（把名字换成另一个文件、或换成一个链接）。
   门判的是**递给 OS 的那个字符串**，替换发生在门之后，所以 §2.5 修的是分配大小，
   **没有修身份**。严重度：低（本机单用户，且写这条的人要能在一次调用之内完成替换）。
   归属：真要防，得打开句柄之后按**句柄**问身份（`GetFinalPathNameByHandle`），那是另一次增量。
6. **`config.toml` 在展开式布局里可读**（实测 283 字节，路径清单，无秘密）。
   严重度：低。归属：prune 支持文件级排除时统一处理（11 §4.3 第 4 条）。
7. **`state/`、`audit/` 这些**名字**仍会出现在父目录列表里**。不改的理由同 11：
   把它们剔掉会让 `total` 与磁盘不一致，那是撒谎式省略。严重度：极低。
8. **非 Windows 的符号链接测试在没有权限时静默 `return`**（`gate.rs:376` 那一支），
   与同族 junction 测试"mklink 失败就是红"的标准不一致。
   严重度：低。归属：真上 Linux 之前改成显式 skip 断言。

## 6. 复查之后还成立的三条总结

1. **顺序不是集合，是"每一步问的位置"**。四问都在、顺序也对，红的那两条
   全都是"某一步问的是拼写，而请求已经走到别处了"。
2. **测试要按"改了它会不会红"来验收**，不是按"跑起来绿不绿"。
   本次三条不合格的测试里两条是**新写的**，如果我不做 A/B 就会带着它们交付，
   并把"这条已经钉住了"写进 note。
3. **"更一般的修法"要跑一遍才知道**。`database_path.parent()` 一行注入
   听起来比"三条精确路径"漂亮，跑出来是六条搜索测试变红——
   而红的方式是"搜索说 0 行且覆盖完全"。那次还原是本次复查里做得对的一个决定。
