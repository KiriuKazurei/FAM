# MCP 服务面：四问一步不减，代价全在 metadata 那一跳

- 增量：12
- 提交：`feat(mcp)`（本文件之前一笔）
- 涉及：`crates/fam-svc-mcp/{lib,proto,gate,tools,server,mutations}.rs`（新 crate）、
  `apps/fam-mcp/{src/main.rs,tests/stdio.rs}`（新 bin）、
  `crates/fam-svc-store/{store.rs,notes.rs,error.rs,lib.rs,tests/store.rs}`（`notes` 表 + schema 2）、
  `crates/fam-svc-audit/src/record.rs`（14 个工具名）、根 `Cargo.toml`

## 1. 描述问题

增量 11 造好了票，但没有"用票的地方"。这一增量把第 10 节那 14 个动作接到 MCP 上：
stdio、一行一条 JSON-RPC 2.0、`initialize` / `tools/list` / `tools/call`，
每次调用一行审计。**权限层不是新写的**，是 11 那层被第一次真正调用。

写代码之前逐条查过的事实，每一条都决定了形状：

1. **单层列举根本不问 `Prune`**。增量 11 的 §2.8 已经演过：把凭据目录当树参数递给
   `abi-smoke`，它照样列出 `mcp-tokens.toml`。那时"搜不出来"与"读不到"是两句话，
   因为 21 个导出里没有任何读文件内容的入口。**`fs_read` 一落地，前者就不再蕴含后者。**
2. **`covers` 只看拼写**。junction 之下的路径仍然"以授权的目录开头"，
   而 `CreateFile` 打开的是别处。`PathKey` 折叠拼写、**不解析组件**（11 的复查账上写着）。
3. **`tools/call` 的参数是嵌套的**：`{"params":{"name":…,"arguments":{…}}}`。
   `Request::string_param` 读的是 `params` 这一层，不拆 `arguments` 就会
   **每次调用都报"缺参数"**——这不是假想的坑，是第一版真踩了（§2.11 第 1 条）。
4. **`search_roots` 可以为空**（配置文件是这进程收到的、比对调用方更早的输入），
   而"未限定作用域的搜索要覆盖所有根"这句话最省事的写法 `search_roots[0]` 在空切片上是 panic。
5. **`fs_delete` 与"递归删除"只隔一行**。工具面自己会给出提示（目录不为空），
   而"把它做对"的那次改动看起来像是修 bug。
6. **备注没有载体**。`Verb` 里有 `note_get` / `note_set` / `note_clear`，
   库里只有 `files` 与 `tags` 两张表，`SCHEMA_VERSION = 1`。

计划的硬约束照抄，不接受"以后再对"：14 个工具、协议面上**不存在**任何管凭据或管痕迹的入口
（admin 也不行）、每次调用恰好一行、范围强制到"未解析的写法都进不来"的程度、
凭据只从环境来。

## 2. 分析问题

### 2.1 四问的顺序不是审美问题

`Gate::admit`（`crates/fam-svc-mcp/src/gate.rs:101`）一次问完，顺序固定：

| 序 | 问什么 | 便宜的拒绝先说 | 拒绝的话里带什么 |
| --- | --- | --- | --- |
| 1 | tier：这个动词在不在等级的白名单上 | 一次位判断 | **不带路径**——你还没资格听 |
| 2 | scope：路径在不在授权的工作区里 | 纯字符串身份比较，0.71 µs | 路径原样 |
| 3 | exclusion：是不是程序自己的状态 | 一次前缀判定，2.41 µs | 路径 + "own state"（**与 2 分开**，见下） |
| 4 | reparse：文件系统解析之后还在不在里面 | 每个组件一次 `symlink_metadata`，120–130 µs/次 | 越界的那**一个组件** |

第 3 问和第 2 问为什么要留成两个 `Refusal`：修法不同。
落在范围外，人可以去改工作区授权；是程序自己的状态，**改授权也没用**——
一张 `*` 的 admin 票照样进不去。合并成一条消息，持票方就会去重试那条永远不该成功的路。

第 4 问必须最后：它是唯一要花 syscall 的一问，前三问否掉的话一次磁盘都不碰。
它也**不能省**：第 2 问用的是拼写，而拼写是外人递的。

### 2.2 协议的形状，两处不能想当然

- **一行一条**。`parse_line` 先拒含 `\0` 与裸换行的输入，再交给 `serde_json`；
  钉它的是 `a_message_cannot_forge_a_second_line`——如果一条输入能引出两条输出，
  一个 agent 就能借服务器的嘴对客户端说话。
- **`id` 可以是字符串**。`Id::{Number,Text}` 自己实现 `Deserialize`，
  数字走 `u64`、文本走 `String`、其余一律 invalid request。
  `a_string_id_survives_the_wire_unchanged` 断言回包里有 `"id":"abc"` 原样——
  用 `serde_json::Value` 承接会把 `42` 与 `"42"` 混成一类。
- **通知不回东西**。`notifications/initialized` 返回 `Response::Nothing`（`Option::None`），
  回了就是协议错误，客户端会把它当服务器的输出流坏了。
- `initialize` 的 `id` 可缺（那就是通知），所以那里不能用 `id?` 直接接在返回 `Response` 的函数里。

### 2.3 工具表是 `const`，schema 就只能是文本

`TOOLS: &[ToolSpec]` 想要 `const`——它必须与 `Verb::ALL` 同生同死，不能是 `lazy_static`
那种"第一次用时才知道有没有拼错"的东西。而 `json!` 宏在 `const` 上下文里报
**"allocations are not allowed in constants"**。
所以 `ToolSpec.schema` 的类型是 `&'static str`，存的是 schema 的 JSON 原文，
用时 `schema_json()` 解析。这个折中把"手写 JSON 会不会打错"变成了运行时风险，
于是配一条测试堵回去：`every_schema_is_json_and_shaped_like_an_input_schema`
（逐个 `serde_json::from_str`，且断言 `type == "object"`、每个 `required` 字段都在 `properties` 里）。

### 2.4 反向测试：清单里没有的那些名字

计划要求"枚举 tools 清单断言不含 token / permission / audit 之类"。
`the_advertised_tools_are_the_files_and_nothing_else` 两半都做：
快照 == 14 个协议名（多一个、少一个、改一个都红），
再把 12 个词逐个查——`token` `permission` `grant` `revoke` `rotate` `audit` `exclude`
`config` `secret` `password` `credential` `mcp`，任一命中即失败。
这条测试是 11 的 §2.1 那句话的**另一半**：那里证明了"ABI 面上这条路不存在"
（`fam-ffi` 不依赖 `fam-svc-auth`，导出 21 个里相关名 0 个），
这里证明"新长的这张面也没有"。

### 2.5 第 4 问的代价：648 → 384 → 284，两次实测分解

第一版照"逐组件解析 + 最后 `canonicalize` 复核"直译，实测一次 admission **648 µs**。
分解（同一进程、release、20 000 轮）：

| 动作 | 单价 | 次数 |
| --- | --- | --- |
| `symlink_metadata` | 121–129 µs | 每组件 1 次 |
| `covers`（路径身份） | 0.71 µs | 1 |
| `prune.excludes` | 2.41 µs | 1 |

`canonicalize` 是 `metadata` 的数倍，而它**只在有链接可解时才有意义**。
于是两处收手：

1. `canonicalize` 改成**只在组件上确实发现 reparse 点时**才跑（648 → 384 µs）；
   一个没有链接的路径不可能解析到自己的拼写之外。
2. 循环本来就一路 push 到**最后一个组件**，所以叶子已经被查过；
   另起一次叶子 `metadata` 是纯浪费（384 → 246–284 µs，本轮实测 246.53 µs）。

诚实的边界：这台机器上一次 `metadata` 要 120–130 µs（远高于常规认知里的亚微秒），
这是**本机文件系统过滤驱动的状态**，不是代码能改的量级。
代码能做的是"要花多少次"，所以下一条不修项里写清了省法的形状：
`fs_list` 的行里本来就带 `reparse` 标志（`ItemFlags::REPARSE`），
列举之后紧接着访问其中的某个路径时，可以从**已经取到手的那一行**判断，而不是再 stat 一次。

### 2.6 还 11 的账：把"在排除里"问进同一次判断

`crates/fam-svc-mcp/src/gate.rs:118` 就是那句 `self.prune.excludes(path)`。
它是不是真的挡住了？把这一句关掉跑同一批测试（A/B）：

```
---- even_an_admin_with_the_wildcard_cannot_read_the_programs_own_files stdout ----
C:\...\state/auth/mcp-tokens.toml was readable:
{"bytes":240,"text":"[[token]]\nid = \"3b3f4b50\"\nlabel = \"tester\"\ntier = \"admin\"\n
 workspaces = [\"*\"]\nsalt = \"a6bf90868594b330ff2528c3ecb2f3ff\"\n
 secret_hash = \"7efe6d93855c4916c1a83d2126909e445434edadaf807651ed22e85b76cfb1cd\"\n..."}
test result: FAILED. 0 passed; 1 failed
```

**一张 admin + `*` 的票，在最强化凭据下读到了 salt 与摘要全文。**
这就是 11 的 §2.8 记下的"从元数据变成完整泄露"，
它闭掉了，闭的依据不是"看起来有这一句"，是这一跑。测回来之后同一条测试绿。

同一件事在真管道上再演一次（配置把 `tree/` 当根、凭据文件在 `state/auth/`）：

```
2 isError=True {"error":"C:/.../state/auth/mcp-tokens.toml is this program's own state
                and is not reachable through the tools"}
3 isError=False {"entries":[{"name":"audit",...},{"name":"auth",...},
                {"name":"config.toml","kind":"code","size":288},...],"total":7}
4 isError=False {"query":"mcp-tokens","hits":[],"matched":0,"visited":11}
```

**边界要说清**：第 3 条是**列举一个未被排除的父目录**成功，
所以 `audit` 与 `auth` 这两个**文件夹名**会出现在别人的列表里（连 mtime 一起）；
被挡住的是"进去"。凭据文件本身的名字在两种布局下都不会泄——
默认布局整个 `%APPDATA%\FileAutoManage` 被排除（连父目录都列不出来），
自定义布局下它躲在自己那一层里。这条记在 §4.3 第 5 条。

### 2.7 票是启动时读的，那撤销对活着的会话算什么

`Server::authenticate` 只在启动时读一次凭据表。于是"撤销立即生效"这句话
对**已经开着的会话**是空的。选一条：**每个请求**问一次"文件还是不是那张"，
不一致就整条面拒绝服务并要求重启（fail closed），而不是"用旧表继续跑"。

- 问法是 `fingerprint()`（`server.rs:806`）：一次 `std::fs::metadata`，
  把 `len` 与 `mtime` 的纳秒一起哈希。**不重读文件内容**——那要每请求几 KB 的读 + 一次 TOML 解析。
- 这**不是**密码学承诺。同尺寸、且 mtime 落在同一粒度内的手工改写可以躲过去；
  NTFS 的粒度是 100 ns，实际做不到"改了内容而长度和时间都不动"，除非攻击者本来就能挑时间戳——
  而能挑时间戳的人已经能写这个文件，那就是 11 §4.4 那笔"手工签发不留痕"的账，不是这里新增的。
- **位置**必须在 `call()` 里、`dispatch()` 之前（`server.rs:247`），不能在门里。
  理由：`whoami` 不作路径主张，永远不走门。第一版放在 `admit` 里，
  于是**被撤销的票照样能问自己是谁**——A/B 把它演出来：

  ```
  ---- a_changed_credential_file_stops_the_running_server stdout ----
  the server kept serving after the file changed:
  {"actor":"agent:tester","allows":[...],"id":"f809438a","tier":"browse",...}
  test result: FAILED. 8 passed; 1 failed
  ```

  改回来之后同一条绿（文件哈希与改动前逐字节一致）。

### 2.8 列举的"文件夹在前"必须做在排序里，不是做在页上

**这一节写于复查之前，当时那段话是错的**：代码把"分组"做在 `fetch(0, offset+limit)` 拿回来的
那个窗口上，然后再 `skip(offset)`。窗口内看是对的，跨页就不是——
排在第一屏之后的目录在任何一屏里都取不到，而两屏之间还会重复若干行。
（第一条实测证据来自我自己写的那条分页测试：300 文件 + 2 目录时
`offset=0` 给出 `f1..f200`，`offset=200` 又给出 `f199..f300`，两个目录一次也没出现。）

现在的形状是**分组成为排序的第一键**：
`crates/fam-svc-index/src/collection.rs` 多一个 `set_grouped(bool)`（默认 `false`，
所以壳与 ABI 面上一个字节的行为都没变），
`compare()` 先比"是不是目录"再比原来的排序键；`fs_list` 只说"要分组"，
窗口仍然是 `fetch(offset, limit)`。

钉它的是 `a_folder_that_does_not_fit_one_page_still_leads_with_its_folders`：
一个装不下一屏的目录，逐页翻完要求 ① 每行恰好出现一次 ② 前 4 行全是目录 ③ 目录之后不再有目录。
A/B：`set_grouped(false)` → FAILED。

`total` / `returned` / `offset` / `generation` / `stale` 报的仍然是**回答的那个索引自己看到的**，
`stale` 必须能被报出来——一个半表不能被当成全表。

### 2.8b 门问完的第四问，在链接之后还要再问一遍

（复查补的一节，因为它改变了两条判断的形状。）
原来第 4 问是"组件走查到**第一个**链接为止，解析它，看它还在不在根里"。
这个形状有两个洞，都是同一条原则没走到底：**解析之后 stand 的地方，
第 2、3 问从来没在那儿问过**。

- 一个链接进 `state/` 的 junction：拼写上 `inbox/st/auth/mcp-tokens.toml` 在授权内、
  不在排除前缀里，解析之后确实还在根里——于是 admin 的一张票把凭据文件读出来了。
  **修法**：每一次跳转之后，把"还在根里吗"与"是我自己的状态吗"在新位置上再问一遍
  （`gate.rs` 的 `judge`），并且在链接以下继续走而不是停下。
- 两个链接接力：`inbox/via-one → inbox/inner`（在根里，旧代码就此放行）、
  `inner/via-two → <temp>/outside`（没人看过它）。**修法**：走完所有组件，
  每遇到一个链接就地解析并续上解析结果，另有 32 跳的上限挡住 `a→b→a`。

还有一处同族的错：组件对齐失败时（授权的拼写带 `\\?\` 而请求不带，或反之），
旧代码把**整条请求**的组件叠到根上去走——那会拼出一条谁都没问过的路径，
在它上面永远找不到链接，于是"检查通过"。现在 `below()` 返回 `Option`，
对不齐就走保守分支：解析整条路径，判断解析结果。

顺带把 `is_link` 从"只认 symlink/mount-point 标签"改成"任何 reparse 点都算"，
与列举层报给调用方的 `ItemFlags::REPARSE` 同一口径（agent 报的这一条我没能在这台机器上
造出非 symlink 的 reparse 标签来实测，所以它是**收紧**而不是**修复的证明**）。

### 2.9 不可逆的那两个动作，方向只能是"不猜"

- `fs_delete` 对**非空目录**直接拒绝，句子里说清"只删文件和空目录"。
  `remove_dir` 对非空目录本来也会失败，**但守卫不能省**：
  系统给的是本机语言的一行错误（实测：`cannot remove …: 目录不是空的。 (os error 145)`），
  递给 agent 就是不可复现、不可解析；更要紧的是把系统错误当 bug 的人，
  下一步就会写 `remove_dir_all`——那才是这一层唯一不可挽回的一次调用。
  测试**断的是句子**：`a_delete_will_not_take_a_directory_that_still_has_something_in_it`，
  只断"失败了"是无效的（不修也过，A/B 见 §2.11 第 2 条）。
- 判"空"用 `entries.next().is_none()` 而不是 `flatten().next().is_some()`：
  **一个读失败的目录项不是空的证据**。
- `fs_move` 两端都要过门，且目标存在就拒绝（不覆盖）。同一测试的第三段钉它。

### 2.10 备注：单独一张表，而且故意搜不到

`notes(path_norm TEXT PRIMARY KEY REFERENCES files(path_norm) ON DELETE CASCADE, body, updated_at)`，
`SCHEMA_VERSION` 1 → 2，迁移走阶梯（`if version < 1 { V1 } if version < 2 { V2 }`），
不是"重建整库"——用户的标签在旧库里。
上限 `MAX_NOTE_BYTES = 8 KiB`，按 **UTF-8 字节**算（测试用 `"字".repeat(8192)` 造一个
24 576 字节的 body，报的正是 `bytes=24576 limit=8192`）。
空白 body 拒绝而不是当成"清空"（清空有 `note_clear`，一句话两个作用是后面误用的种子）。

**为什么搜不到**：`fs_search` 走的是名字匹配的后端（walk / Everything），
备注内容是另一张表。把它们接起来就能"搜备注"，但那样一来
`fs_search` 的结果会混两种身份（文件命中的理由不一样），
而且备注里的一句话会获得与文件名同等的可达性——而备注是**这个工具写的批注**，
不是用户起的名字。这次宁可少一个功能，也不把两种东西煮成一锅。
钉住它：`a_note_is_not_a_tag_and_neither_of_them_is_a_search_result`
（写"the one with the diagram"，再搜 `diagram` → 0 命中）。
归属：真要搜内容，那是一次单独的增量（要带"命中理由"字段）。

### 2.11 自查：这五处是我的测试自己抓出来的

按规矩列全，包括**打我自己代码的**和**打我自己的账的**：

1. **`params.arguments` 没拆**。第一次跑门测试就全线红：每个路径参数都"missing"。
   修法是把 `arguments` 摊成一个 `method` 为工具名的合成 `Request`（`server.rs:240`），
   这样 14 个工具的取参代码与 `Verb` 层完全一致，不需要第二条路径。
2. **删除守卫的测试断言不到守卫**。`a_delete_will_not_take_…` 只断"失败 + 还在盘上"，
   而这两点**操作系统也保证**，所以守卫拿掉它照样绿。
   补一句断言（错误里必须含 `still has entries in it`），然后 A/B：
   把守卫用 `&& false` 关掉 → `that failure came from the operating system, not from the tool:
   {"error":"cannot remove …: 目录不是空的。 (os error 145)"}` → FAILED；
   还原（哈希 `cc5ad467…` 与改前一致）→ 9 条绿。
3. **空根守卫没有测试走得到**。`search_roots.is_empty()` 那一支写在纸上等于没写。
   补 `a_configuration_with_no_roots_is_answered_not_panicked`（admin + `*` + 清空 roots），
   再把守卫整支删掉跑它：`panicked at crates\fam-svc-mcp\src\server.rs:556:86:
   index out of bounds: the len is 0 but the index is 0` → FAILED；还原 → 绿。
4. **价格表把同一个变量印了两遍，其中一行的标号还写着"含门"**。
   `whole` 是 `whoami` 的一次 `handle()`，`whoami` 不过门；
   而第一行印的是"one request through handle (parse + gate + audit line)"。
   两个标号、一个数（都是 171.79 µs），拿它去减 `admit()` 会得出"门是负的"。
   现在分开测、分开印：`whoami through handle, no gate` 与 `fs_read through handle`，
   顺带删掉一个从没被用的 `let who = json!({...})` 和一句过时的注释
   （"plus two `canonicalize` calls"——那两处正是 §2.5 省掉的）。
5. **`fs_delete` 里躺着两行死代码**（`remaining = false; let _ = remaining;`），
   读起来像"这里本来还要做点什么"。跟着第 2 条一起清了。

另有五处是这个增量**写的时候**就不对、被测试当场拦下的（记在这里是因为它们是同一类错）：
`whoami` 绕过了 fail-closed 检查（§2.7）、`fs_list` 的"文件夹在前"是假的（§2.8）、
`written` 数的是字符不是字节（20 个字符的中文实际 25 字节）、
输出路径分隔符混排（`C:/dir\file`，现在过 `components()` 重建，Windows 上统一 `\`）、
`fs_search` 的 `search_roots[0]`（§2.11 第 3 条）。

## 3. 解决方式

### 3.1 分层

```
crates/fam-svc-mcp/src/lib.rs        Error + 再导出；crate 文档写着三步顺序与那条"不防自己人"的边界
                        proto.rs     一行一条：Id / Request / Response / ProtocolError / parse_line
                        gate.rs      Refusal 四种 + Gate::admit 四问 + 组件走查
                        tools.rs     const TOOLS: &[ToolSpec]，14 条，schema 是 JSON 文本
                        server.rs    Server::authenticate → handle → 14 个只读/列举工具
                        mutations.rs 写、建、删、移、标签、备注
apps/fam-mcp/src/main.rs             stdio 循环；票只从 FAM_MCP_TOKEN 来
```

14 个名字与 `Verb` 的线上名**逐字相等**，`fam-svc-audit` 那 14 个 `Op` 也叫这个名
（`the_mcp_tools_are_named_as_the_protocol_names_them`）——日志与清单对不上是最难查的一类账。
权重照 10 的规矩：`fs_list` / `fs_read` 记 `Read`，其余工具调用记 `Ask`（`whoami` 也是）。

### 3.2 一次调用发生的事

```rust
// server.rs:238 —— 摊开 arguments，再问"文件还是不是那张"，最后才 dispatch
let call = Request { id: Some(id.clone()), method: name.clone(), params: arguments };
// Checked here, once, rather than inside the gate: `whoami` claims no path and so never asks
// the gate's path questions, and a credential that has been revoked while this process was
// running must not be able to answer anything at all.
if let Err(why) = self.credentials_unchanged() { … return; }
let answer = self.dispatch(verb, &call);
self.record(verb, &call, &answer);
```

```rust
// gate.rs:101 —— 四问，一步不减
if !token.allows(verb) { return Err(Refusal::TierLacks { … }); }
if !verb.names_a_path() { return Ok(()); }               // whoami 到此为止
if !token.covers(path, &self.roots) { return Err(Refusal::Outside { … }); }
if self.prune.excludes(path) { return Err(Refusal::Excluded { … }); }
self.stays_inside(path, token)                            // 组件走查，有链接才 canonicalize
```

`fs_read` 三条拒绝：目录（"要列举去 `fs_list`"）、> 1 MiB（把 `size` 与 `limit` 都报回去，**不截断**）、
非 UTF-8（`{"binary":true,"bytes":N,"error":"… is not UTF-8 text"}`，实测 `tags.db` 32 768 字节命中这一条）。
`fs_write` 的 `written` 是 UTF-8 **字节**数。

### 3.3 跑起来

```bash
fam mcp token add organizer --tier edit --workspace "D:/inbox"     # 明文只印这一次
export PATH="$HOME/.rustup/toolchains/stable-x86_64-pc-windows-msvc/bin:$PATH"
cargo build --release -p fam-mcp
FAM_MCP_TOKEN=fam_8e640bfc_... target/release/fam-mcp.exe --config <path/to/config.toml>
```

**没有 `--token` 这个旗标**：命令行上的令牌对本机每个进程可见（`Get-CimInstance Win32_Process`
一行就抄走了），而环境变量只对这个进程可见。`--help` 与 `--config` 是仅有的两个旗标。
日志走 stderr，stdout 上只有协议。

### 3.4 一条真管道的实录

`browse` 票（只授权 `tree/inbox`），stderr 一行 `fam-mcp: serving as agent:reader-live; 1 roots, 14 tools`：

```
3 whoami     → {"actor":"agent:reader-live","allows":["fs_list","fs_read","fs_search","tags_get",
                "note_get","whoami"],"id":"8e640bfc","roots":["C:/.../fam12/tree"],"tier":"browse",
                "workspaces":["C:/.../fam12/tree/inbox"],"backends":["walk"]}   isError=false
4 fs_list    → {"entries":[{"name":"alpha.txt","path":"C:\\...\\inbox\\alpha.txt","size":8,
                "directory":false,"kind":"document","reparse":false}],
                "total":1,"returned":1,"offset":0,"generation":1,"stale":false}  isError=false
5 fs_read    tree/private/secret.txt
             → "C:/.../tree/private/secret.txt is not inside any workspace this credential was granted"
6 fs_write   → "a browse token cannot call fs_write; its tier allows a different set of tools"
7 fs_list    tree/inbox/../private
             → "C:/.../tree/inbox/../private is not inside any workspace this credential was granted"
```

`admin` + `*` 票，另一跑（写备注、读回来、删非空目录被拒、问一个不存在的工具名）：

```
5 note_set   → {"path":".../alpha.txt","written":21}           # 21 = UTF-8 字节，正文 21 字节
6 note_get   → {"body":"kept by the organizer","updated_at":"2026-09-24 16:31:42"}
7 fs_delete  tree/inbox
             → "…tree/inbox is a directory that still has entries in it;
                fs_delete removes files and empty directories only"
8 mcp_tools_list → {"code":-32601,"message":"there is no tool named \"mcp_tools_list\""}
```

同一条痕迹（`state/audit`，含这三跑与签发那几条）：

```
$ fam audit verify
1 file(s), 57 record(s), 16 session(s)
the chain is unbroken and every session sealed its own count            exit=0
$ fam audit stats
1 files, 57 records, 14178 bytes (249 bytes per record)
16 sessions, 16 sealed, 0 records reported dropped
by op: 16 session.close / 16 session.open / 8 fs_list / 5 fs_read / 4 token.add
       / 2 fs_search / 2 fs_write / 1 fs_delete / 1 note_get / 1 note_set
by actor: 16 system / 14 user / 10 agent:everything-admin / 7 agent:librarian-live
          / 6 agent:reader-live / 4 agent:reader
```

拒绝的那几条**也在日志里**，`result` 以 `error ` 开头原样带出句子——
这是"审计是给人事后看的"那句话的兑现方式（`a_session_leaves_one_line_per_call_and_the_chain_verifies_afterwards`、
`even_an_admin_…` 里那段"两条 fs_read 拒绝都有行"）。

## 4. 解决方式效果评估

### 4.1 正面证据

`cargo test --workspace`：**484 通过、0 失败、2 条 `#[ignore]`**（两条都是测价：
11 的一次认证、12 的一次门）。增量 11 结束时是 446，本增量 **+38**：

| 目标 | 条数 | 其中本增量新增 |
| --- | --- | --- |
| `fam-svc-mcp` lib（proto） | 6 | 6 |
| `fam-svc-mcp` tests/gate.rs | 10 | 10 |
| `fam-svc-mcp` tests/mcp.rs | 12 | 12 |
| `apps/fam-mcp` tests/stdio.rs | 4 | 4 |
| `fam-svc-store`（notes 4 + 迁移 1） | 11 + 21 | 5 |
| `fam-svc-audit`（14 个名字对齐） | 43 | 1 |

`cargo clippy --workspace --all-targets` **0 告警**；`cargo fmt --all` 干净。

**端到端**（不是单元测试的重复，是真的起了进程、走管道）：
`a_full_session_over_a_real_pipe_leaves_one_line_per_call_and_a_chain_that_verifies` 等 4 条，
外加 §3.4 那份人工实录。计划的验收五句逐条对上：
越界读被拒（第 5 条）、junction 不越界（`a_junction_out_of_the_workspace_does_not_open_it`）、
browse 令牌写被拒（第 6 条）、审计逐条可查（`audit verify` 16 会话全封印）、
断链能被 verify 发现（10 已钉，本次链条在真会话后仍验得过）。

**结构没被这个增量弄坏**（`famcore.dll` 用 GNU 工具链重建后再量一次）：
导出 21 个 == `famcore.h` 声明 21 个，`comm -3` 双向差集为空；
含 `token` / `mcp` / `auth` / `audit` 字样的 **0** 个。
`abi-smoke` 在新 DLL 上 `insert=1 cut=1 -> ok`，退出 0。
壳的无头闸门同一天复跑：`icons: extracted=19 destroyed=19 cached=19 pending=0`、
`ipc: headers=4 pages=4 polls=27 retries=0`、探针建了又删、选择保持、1–2 对信号收敛。

**A/B 四次**（每次都是"改坏 → 看它红 → 逐字节还原"），§2.6 / §2.7 / §2.11 第 2、3 条各一次。
两次文件哈希复原核对：`server.rs` = `ecba3aa300fd…`、`mutations.rs` = `cc5ad4677da3…`，与动手前一致。

### 4.2 代价

**一次请求的构成（实测，release，20 000 轮，`the_price_of_the_gate_and_a_request`）**：

| 量 | µs / 次 |
| --- | --- |
| 协议本身（`ping`：解析 + 回包，不写日志、不跑工具） | 1.85 |
| `covers()` 路径身份 | 0.71 |
| `prune.excludes()` | 2.41 |
| 一次 `symlink_metadata` | 121.57 |
| **门（`admit`，含它的 syscall）** | **246.53** |
| `whoami` 走 `handle()`（**不含门**） | 178.31 |
| **`fs_read` 走 `handle()`（含门、含开文件、含日志行）** | **750.02** |
| 循环自身的地板 | 0.51 |

三个结论，每个都能被上面这行数反驳或支持：

1. **代价几乎全在 metadata 那一跳**：门 246.53 µs 里，`covers` + `prune` 合计 3.12 µs，
   剩下的是组件走查的 stat。判据便宜，**问文件系统贵**。
2. **`whoami` 那 178.31 µs 与门无关**（它不过门），大头是 §2.7 那一次 `metadata` 指纹。
   标号写"no gate"就是为了不让人拿它去减 `admit`。
3. `fs_read` 的 750 µs 里，门 246 + 指纹 178 的份额之外，是打开并读一个 8 字节文件自己的那几跳。
   一次审计追加按 10 的实测是 12 µs（release），**在这一跑里被埋进了噪声**。

对照 11 的"认证 1.6 µs"：认证这一层依然便宜到可以忽略；
**贵的是所有"要看文件系统一眼"的判断**，而这一层的价值恰恰在那一眼。

**复查之后同一条测试重测**（门多了两处判断：每次跳转后在新位置上再问一遍，
以及"是不是我自己的文件"要逐条比对三条精确路径）：

| 量 | 修前 | 修后 |
| --- | --- | --- |
| 门（`admit`，含 syscall） | 246.53 | **259.32** |
| `whoami` 走 `handle()`（不含门） | 178.31 | 166.42 |
| `fs_read` 走 `handle()` | 750.02 | 738.94 |
| 一次 `symlink_metadata` | 121.57 | 118.82 |

**门贵了 12.8 µs（约 5%），两头端到端的数在两次跑动之间本来就是噪声级**
（后两跑都低于前一跑，那是缓存与温度，不是改动带来的收益）。
这一笔要记的只有门那一个数：它是**闭掉两个洞的价格**，形状是"每一次链接跳转付一次
`canonicalize` + 若干个 `PathKey` 比较"，没有链接的路径不付这笔钱。
基准闸门仍然十条全绿（`all measured cases within gate`，
其中 `volume_scan_capped` 这一跑 1.04x，同一条 case 在上一跑是 0.92x——
那是 `benchmark-floors-are-machine-state.md` 记过的量级，不重录地板）。

**上一轮记的 `read_flat_std` warn 这次没复现**（同一条 case：0.88x / 0.81x，verdict ok）。
本轮整片都优于地板（`patch_256_of_20k` 0.05x、`open_flat_dir` 0.60x、`audit_append` 0.29x），
**这些一律不计为收益**——本增量没碰索引、缓存、审计写入的任何一条热路径，
新代码只在 `fam-mcp` 这个新进程里跑；地板与上次记的是同一批，变的这台机器的状态
（前一轮 `read_flat_std` 连红两次时，D: 卷正被增量编译缓存压着）。
`all measured cases within gate`，十条 measured case 无一红。基准**没有重录地板**，
按 `benchmark-floors-are-machine-state.md` 的既定原则：不可复现的差值不写进账。

**复杂度**。+2 个 crate、+1 张表、schema 1 → 2（多一段阶梯）、+1 个环境变量契约（`FAM_MCP_TOKEN`）、
+1 份必须逐字对齐的命名表（`Verb` 线上名 == `TOOLS[].name` == `Op` 名，三处三测试）。
`fam-svc-mcp` 依赖 6 个 fam crate——它是目前依赖最重的一层，
因为它就是那"面"。

### 4.3 没解决的东西（严重度 / 归属）

1. **只有 stdio，没有 HTTP 传输**。计划里 HTTP 是后一步。
   严重度：中（不能远程接入）。归属：下一次增量，且届时要新增的不是传输而是**认证与速率**。
2. **门每个路径请求付 1–2 次 stat（121–130 µs/次）**。省法明确：
   `fs_list` 返回的行里已经有 `reparse` 标志（`ItemFlags::REPARSE`），
   列举之后紧跟着访问其中某个路径时该复用那一行，而不是再问文件系统。
   没做的理由不是懒：**复用会引入"标志可能过期"这个新失效方向**（列完之后目录被换成链接），
   而现在的形状是"每次都问活的文件系统"。严重度：中（这是这一层的头号常数开销）。
   归属：如果哪天这一层被拿去做大目录批量操作（几千次 `fs_read`），必须做，且要带
   "标志读自哪一代"的核对。
3. **凭据表只在启动时读**，之后只比指纹。改内容 → 整条面拒服务；
   但**同一份表内的一次撤销**（例如另一进程写了新文件、指纹确实变了）也只得到"重启我"，
   不会自动接住新表。方向是 fail closed，不是 fail open。严重度：低（行为可预期）。
   归属：真需要"热接新表"时再说，那要引入"哪些会话属于哪张表"的问题。
4. **指纹是 (len, mtime)，不是内容哈希**。理由与边界见 §2.7 第二条。
   严重度：低。归属：要收紧就把 `sha256(文件字节)` 存进会话（每请求读几 KB），
   代价是 §4.2 那 178 µs 变成"读 + 哈希"。
5. **没有速率限制，也没有单次会话的调用上限**。要说清能打的是哪一条：
   递一个**范围外**的路径只能让服务器付前三问的钱（3.12 µs），便宜到不构成攻击；
   递**范围内**的路径每条要付第 4 问的 1–2 次 stat（246 µs），
   也就是贵的恰恰是"合法请求"——一个不断列举大目录的 agent 能把这个进程占满。
   严重度：中低（stdio 面只有一条管道，客户端就是它自己那一个会话）。
   归属：HTTP 传输那次一起做（那时才有"多个客户端"这回事）。
6. **`fs_read` 没有区间读**（`offset` / `length`）。1 MiB 以上的文件一律不给内容，
   只能报大小。严重度：中（对"给 agent 读大日志"这类活是硬限制）。
   归属：13 或之后。加区间读时必须同时定"越界 offset 是错还是空"，
   这次不做是因为**没有真实调用方能定这个语义**。
7. **备注内容搜不到**，§2.10 讲了为什么这是有意的。严重度：低（少功能，非缺陷）。
   归属：单独一次增量，带"命中理由"字段。
8. **`whoami.allows` 是等级的集合，不是"这次能做什么"**：票的工作区不覆盖某个目录时
   `fs_write` 仍然在 `allows` 里。它答的是"我这等级有哪几个工具"，不是"哪里可写"。
   `whoami_answers_for_the_credential_and_claims_no_path` 钉的是形状（不作路径主张），
   没有钉这句语义。严重度：低（误导面：agent 会先试再撞墙）。
   归属：如果哪天有 agent 因此反复重试，就在 `whoami` 里加 `writable_roots`——
   那是新增字段，不是改这行的含义。
9. **`admin` 与 `edit` 的动词仍然相等**，只差能持 `*`（11 §4.3 第 2 条原样搬来）。
   本增量没有新增词表动词，所以那条快照测试 `admin_has_no_verb_that_edit_lacks` 仍绿。
   严重度：中（按名字理解"admin 更大"会错）。归属：词表管理那次增量。
10. **展开式布局下，程序自己的 `config.toml` 落在可达范围里**（`tags.db` 已经不在，见下）。
    实测（§2.6 那跑）：`*` 的 admin 票能 `fs_read` 到 `config-wide.toml` 全文（283 字节）。
    信息面：配置里是路径，**没有秘密**。
    严重度：低。归属：11 §4.3 第 4 条，修法是 prune 支持文件级排除
    （`crates/fam-core/src/prune.rs:41-46` 那句注释就是这件事的现场：
    "walk 只对**将要进入的目录**做前缀判定，单文件路径在这张表里匹配不到任何东西"）。
10b. **标签库本身：这一条原来写错了，是复查抓出来的**。
    我原来写的是"`tags.db` 因为不是 UTF-8 被挡住（实测报 32 768 字节）"——
    那只是因为那个库里真有几行数据。**一个刚建好、页内大量 NUL 填充的库是合法 UTF-8**：
    同一条 `fs_read` 返回过 `{"bytes":4096,"text":"SQLite format 3\u0000…"}`，
    也就是**用户的标签与备注表整个被当作文本交出去了**。
    修在了门上（`gate.rs` 的 own-file 列表：`tags.db` 与 `-wal` / `-shm` 三条精确路径），
    A/B 的证据就是把那张列表清空 → 4096 字节回来了。
    **为什么不是把它的父目录整个排除**：先试过，`prune` 里注入 `database_path.parent()`
    之后 `crates/fam-ffi/tests/v2.rs` 六条搜索测试当场变红——那个布局里数据库文件在搜索根的
    **同一层**，排除前缀于是罩住了整棵被搜的树，症状是"搜索返回 0 行且自称覆盖完全"。
    **这条失效方向比原来那个泄露坏得多**（静默空结果 vs 明确拒绝），所以收回到"只按文件排"。
    严重度：中（默认布局本来被 `mcp_token_file.parent()` 罩住，走自定义布局的人会碰到）。
    归属：闭上了（门那一层）；剩下的口子是 `config.toml`，见上一条。
11. **被排除目录的名字仍会出现在父目录列表里**（实测：`state/` 列出 `audit`、`auth`，含 mtime）。
    不改的理由：把它们从列表里剔掉，`total` 与磁盘就不一致了，
    那是"撒谎式省略"，比"看到一个文件夹名"更坏。
    严重度：极低。归属：不修，写了理由。
12. **非 Windows 的符号链接测试在没权限时静默 `return`**（`gate.rs:376`）。
    与同族的 junction 测试标准不一致——后者明确要求"`mklink` 失败就是测试失败"。
    严重度：低（这一层今天只在 Windows 上跑；Linux 上的形状要靠它，而现在它可能不跑）。
    归属：真上 Linux 之前改成显式 skip 断言。
13. **票没有过期时间**（11 §4.3 第 5 条原样）。严重度：中低。归属：交付真人长期使用之前。
14. **musl 交叉检查仍卡在 `libsqlite3-sys`**：
    `cargo check -p fam-svc-mcp --target aarch64-unknown-linux-musl` →
    `error: failed to run custom build command for libsqlite3-sys v0.30.1`。
    `fam-core` / `fam-svc-auth` / `fam-svc-audit` 三个不带 SQLite 的目标同一轮里通过。
    这是既有的排除项（见 `increment-6-what-this-machine-can-prove.md`），
    **不是本增量新坏的**；本增量把 `fam-svc-mcp` 也放进了这批"带 SQLite 因而量不到"的名单。
    严重度：低。归属：真上 musl 时统一处理（bundled 之外的构建问题）。
15. **打包仍然缺**（`install()` 目标、windeployqt、启动脚本）。
    新增的是 `fam-mcp.exe` 也得跟着打包，还要交代 `FAM_MCP_TOKEN` 由谁注入。
    严重度：高（这是"交给别人试"的门槛）。归属：一直没做，是这一串增量共同的那一笔。
16. **一条输入的长度没有上界**（复查抓出来的，且推翻了我自己在 §4.4 原来那句话）。
    `proto::parse_line` 收到什么长度的行都先吞下来，`main.rs` 用 `BufRead::lines()` 逐行喂，
    于是"最大的单次分配在 1 MiB 以内"只在**出去的方向**成立。
    没现在改的理由是形状的：要真 bounded 就得换掉 `lines()`，
    而换掉之后**"截断到哪里重同步"是一次新的失效方向**（框架错位比 OOM 更难查），
    这台机器上唯一的客户端是本机自己起的那一个进程。
    严重度：中（远程传输一上就来不及）。归属：HTTP 传输那次一起做，
    那时本来就要读长度头、限请求体、定超时。

### 4.4 恶意行为评估（提权 / 甩锅 / DoS）

- **提权**：这一层没有给自己发权的入口——§2.4 的反向测试把"名字里出现 token/grant/revoke"
  变成了当场红；`fam-mcp` 不接受 `--token`，所以**进程自己从不知道明文**
  （它拿到的票只用于 `verify`，比对的是摘要）。
  能提权的只剩两条外部路径：写凭据文件（11 §4.4 已记为"无痕签发"，同一笔账），
  或**绕过这一层**（能以本用户身份跑程序的人不需要票）。
  这一层自己**没有**新增提权面。它新增的是一个**破坏面**，而且不需要任何漏洞：
  `fs_write` 能改工作区内的文件内容——如果用户把 `.ssh/config`、`Startup` 文件夹
  或某个 PATH 上的目录授给了 edit 票，写入就是执行。
  严重度：中。归属：授权粒度，不是代码——`fam mcp token add` 的文档该写这句；
  而默认配置（家目录做根 + `*` 的 admin 票）在今天意味着"整片可写"，
  只该在给人看过后果之后再发。
- **甩锅**：每条调用一行，`actor = agent:<label>`，label 在签发时烤进票里（11 §2.4）；
  **被拒的调用同样有行，且句子里带原路径**（§3.4 实录）；
  痕迹里 `target` 是路径身份（折叠大小写与分隔符），但**不解析 `.` / `..`**，
  所以"逃出去的写法"在日志里仍写着点号——这一条有测试盯着
  （`a_path_that_has_not_been_resolved_is_not_reachable` 新增那三句断言），
  否则日志会把一次越界尝试记成一次正常访问。
  剩下能冒充的仍是**人**：同一个人签两张用途不同的票，日志分不出来。
- **DoS**：**出去的方向**有界——`fs_read` 的返回走 `take(1 MiB + 1)`，所以那个上限是"读多少算多少"，
  不再是"先 `metadata` 看一眼、然后文件长多少就读多少"（这是复查改的一处：
  原来那句"最大单次分配 1 MiB"在**进来的方向**上是不成立的）。
  **进来的方向没有界**：`parse_line` 对一行多长不作声，
  一个客户端可以先递一条 4 GB 的单行再等服务器把它吞完。见 §4.3 第 16 条。
  最大行数是 2 000（`MAX_ROWS`），且**都是 clamp 不是拒绝**，所以攻击者不能靠超限让请求失败来放大重试。
  `fs_search` 的 `limit` 同样夹住，且**未限定作用域**的搜索现在要**每个根都覆盖**才算数
  （复查抓出来的：原来只问第一个根，见 `increment-12-review-findings.md`）。
  真正没防的还是 §4.3 第 5 条那种"合法的慢"。
  凭据猜测这条面：未知 id 261 ns、错秘密 1.52 µs（11 §4.2），
  对一个攻击者来说这连一次系统调用都不到，**没有放大收益**。
