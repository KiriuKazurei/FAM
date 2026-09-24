# 令牌与权限：签发只有人手能碰，等级是白名单不是梯子

- 增量：11
- 提交：`feat(auth)`（本文件之前一笔）
- 涉及：`crates/fam-svc-auth/{lib,verb,tier,scope,rand,token,store}.rs`（新 crate）、
  `crates/fam-core/{hash,time,config,prune,lib}.rs`、`crates/fam-svc-audit/{chain,record}.rs`、
  `crates/fam-ffi/src/lib.rs`、`apps/fam-cli/{src/main.rs,src/commands.rs,tests/cli.rs}`、
  根 `Cargo.toml`、`apps/fam-cli/Cargo.toml`

## 1. 描述问题

增量 12 要把第 10 节那 14 个工具交给外部 agent。届时第一个问题不是"它能做什么"，而是
**"谁替它决定的"**。这层要在那一刻之前就存在，否则第一次接入就已经是"权限追不上已发生的事"。

写代码之前的状态是这样：`fam-ffi` 的面上**没有任何身份概念**。谁能加载 `famcore.dll`，谁就是你——
`Actor` 恒为 `user`（增量 10 的 §4.4 记着这条），而 `Prune` 只防"程序自己的目录被当成数据"，
不防"程序被叫去看任何目录"。

计划给的约束是硬的，逐条抄在这里：

1. 存放 `AppPaths::config_dir()/mcp-tokens.toml`；每条 `id / label / tier / workspaces /
   secret_hash`（加盐，**每令牌独立 salt**）`/ created_at / revoked_at`；
   **明文只在 `add` 时打印一次**。
2. 三等级是**动词白名单而非继承**：`browse` = 列举、读取、搜索、读标签读备注；
   `edit` = browse + 工作区内写/建/删/移 + 改标签改备注；`admin` = edit + 工作区内一切 + 标签词表管理。
   **三者都不得越出自己的 workspaces。**
3. 人工面只有 CLI `fam mcp token add|list|revoke|rotate`。
   **MCP 协议面上不存在任何 token / permission / audit 相关的 tool 或 resource（admin 也不行）**——
   落地方式是那条路径在协议面上不存在，不是运行时检测。反向测试：枚举 tools 清单断言不含这些名字。
4. 诚实边界：能以本用户身份执行任意程序的 agent 可绕过，需 OS 级隔离，不在范围内。

## 2. 分析问题

### 2.1 为什么"管权限的接口"不能和被管的一方共享任何入口

如果签发/撤销能通过 ABI 或 MCP 到达，那么被限制的那一方就是申请解除限制的那一方——
即使接口本身要求管理员令牌，"要不要用一个管理员令牌"这个决定也由被限制方做出。
所以约束必须是**编译期形状**，不是运行时判断。

落地是删依赖，不是加检查：`crates/fam-ffi/Cargo.toml` 的 fam-svc 列表今天仍然只有四行——

```
fam-svc-audit.workspace = true
fam-svc-index.workspace = true
fam-svc-store.workspace = true
fam-svc-walk.workspace = true
```

`fam-svc-auth` 不在里面，也**不能**进去（`crates/fam-ffi/src/lib.rs` 里 `create()` 只借它的
目录做冲突检查，见 §2.8）。于是"协议面上不存在这条路"不是承诺而是链接期事实。
可量的一面照增量 10 的办法量了：`objdump -p famcore.dll` 的导出名 21 个，与
`apps/fam-shell/include/famcore.h` 声明的 21 个逐项相等，双向差集为空；
其中含 `token` / `mcp` / `auth` / `audit` 字样的 **0** 个。

计划里"枚举 tools 清单断言不含这些名字"的反向测试要等 tools 清单存在（增量 12）；
今天能钉住的是**这条路径不存在**，所以那半边测试随 12 一起补，不是被忘掉。

### 2.2 白名单，不是梯子

`Tier` 没有 `Ord`、没有 `at_least`、没有 `impl From<Tier> for u8`。理由只有一个：
`if token.tier >= Edit` 这种写法会让**下个月新加的动词自动落进高等级**，而"自动落进"正是没人批准过的授权。
所以等级写成三份显式列表（`tier.rs:44` 的 `allows`），新动词必须在三处各表一次态：

| | browse | edit | admin |
| --- | --- | --- | --- |
| `fs_list` `fs_read` `fs_search` `tags_get` `note_get` `whoami` | ✓ | ✓ | ✓ |
| `fs_write` `fs_create` `fs_delete` `fs_move` `tags_set` `tags_remove` `note_set` `note_clear` | — | ✓ | ✓ |

矩阵测试 `every_tier_and_verb_pair_is_declared` 枚举全部 3 × 14 = 42 组，每组断言"允许"或"拒绝"，
不许 `unreachable`。

**一处必须说明白的不对称**：`admin` 今天在动词上**等于** `edit`，唯一的实质特权是能持有 `*` 工作区
（`may_scope_everything`）。计划里"admin = edit + 标签词表管理"的那半句**还没有载体**——
词表 API 目前不存在（`Verb` 里只有 `tags_get` / `tags_set` / `tags_remove`），所以也就无从列进白名单。
我没有偷偷给 admin 多开一个动词，也没有假装三者相同：
`admin_has_no_verb_that_edit_lacks` 是把"此刻相等"钉成一条会响的快照测试，
`Tier::summary()` 里 admin 的文案是 "edit, and may hold \* as its workspace" 而不是"更高一级"。
归属与严重度记在 §4.3 第 2 条。

### 2.3 明文只出现一次，那文件里该存什么

令牌是**给机器用的 256 位随机串**，不是人写的口令，所以常见的"要用慢 KDF"的动机（防离线字典猜）
在这里不存在：猜测空间是 `2^256`，一次 SHA-256 和一万次 SHA-256 之间没有区别。
慢 KDF 唯一的实际效果是把每次请求的认证成本抬到毫秒级，而认证是**每个 MCP 请求一次**。
所以存 `sha256(DIGEST_SCOPE ‖ salt ‖ id ‖ secret)`：

- `DIGEST_SCOPE = b"fam-mcp-token-v1"`：把"这张票是给谁的"写进哈希输入，
  同一秘密在别处的哈希对不上这里。
- 每令牌独立 16 字节 salt，随机：两行同秘密也**看不出同秘密**
  （`two_tokens_do_not_share_a_digest_shape`）。
- **id 也在哈希输入里**：否则把 A 行的 `secret_hash` 复制进 B 行，A 的秘密就能登录成 B——
  一个纯粹的冒名通道。`the_digest_covers_the_id_so_rows_cannot_be_swapped` 钉这条。

哈希不是自指的：拿这次真发的令牌，用另一台"实现"（Python `hashlib`）独立复算——

```
sha256("fam-mcp-token-v1" ‖ "78770eea963cede06c65b22d5fb151e5" ‖ "67280c18" ‖ "<64 hex 明文>")
  = f2552708e2d146972734ca4c557be66b27d454b0d3ea528b7e5e48c8a37a3ebc
文件里写的 secret_hash
  = f2552708e2d146972734ca4c557be66b27d454b0d3ea528b7e5e48c8a37a3ebc   MATCH
```

（第一次复算我算成了 `f14c7b4c…`，因为我把 salt 当**十六进制解码后的 16 字节**喂进去，
而代码用的是**文件里那 32 个 ASCII 字符**。`token.rs:229` 的 `digest` 收 `&str`，
所以哈希覆盖的是文本形态。这条写下来是因为它正是"自指验证"会放过的错——
自己的实现配自己的测试永远绿，换个实现才对不上。）

比较走常数时间（`constant_time_eq`，`token.rs:241`）：定长十六进制的逐字节比较，
第一个差异字节不做提前返回。

### 2.4 撤销保留行、轮换不动 `created_at`

`revoke` 只写 `revoked_at`，行不删。理由在审计那侧：`actor` 里存的是 label
（`Actor::agent(label)`），行没了，日志里那些 `agent:organizer` 就成了查无此人——
抹掉一次授权的历史比留着它更容易甩锅。

`rotate` 换 salt + secret，**不动 `created_at`**：身份与寿命是同一条，"这票什么时候开的"
不该被"换了个钥匙"改写。这条差点做错（写的时候顺手把 `created_at` 重新盖了一遍），
被真跑一遍的输出抓住——文件里 `c9db034c` 的 `created_at` 停在 `05:49:44.607`，
而 rotate 发生在 `05:49:56` 那一跑。测试名：`rotating_keeps_the_identity_and_changes_the_key`。

撤销是**立即且永久**的：`a_revoke_stops_the_secret_immediately_and_permanently` 外加"换个进程重开
同一个文件也同意"（`reopened.verify(...)` 为 `None`，因为 `revoked_at` 在文件里，不在内存里）。
对已撤销的票再 `rotate`/`revoke` 一律拒绝（`AlreadyRevoked`），不做幂等成功——
幂等会让人以为点了两次等于没点到。

### 2.5 并发改这张表：丢一次撤销 = 静默提权

两个人（或一个人开两个终端）同时改这个文件时，"最后写入者赢"的失效方向是**确定的**：
后写的进程手里有前一份全表，它写回去就把前一个进程刚写的那一行抹掉——
被抹掉的如果正好是一条 `revoke`，结果就是"人以为收回了权限，而权限还在"。

所以 `save()` 先重读文件，与自己当初读到的内容摘要比对，不一致就 `ConcurrentEdit` 拒绝写，
**而不是写**（`a_stale_store_refuses_to_write_rather_than_clobber`）。
写本身走 `写临时文件 + rename`（`the_temporary_file_never_survives_a_save`），
不留半张表。

### 2.6 工作区比较必须是路径身份，不是字符串前缀

`"D:/notes" is_prefix_of "D:/notes-secret"` 为真，于是字符串前缀比较会让一条只授权看
`D:/notes` 的令牌看见隔壁那个文件夹。这层的包含判断全部走 `fam_core::PathKey`
（UTF-16、只折叠 ASCII），也就是索引和标签用的同一个身份：

- `a_workspace_covers_its_subtree_and_not_its_neighbour`：`D:/notes` 覆盖 `D:/notes/a.txt`，
  **不**覆盖 `D:/notes-secret/a.txt`。
- `containment_ignores_separators_and_case`：`\` 与 `/`、大小写差异不产生两个目录。
- `the_wildcard_is_an_admin_privilege_and_means_the_configured_roots`：`*` 展开成
  **配置里那批根**，不是整台机器。
- `a_relative_or_empty_workspace_is_refused_on_the_way_in`：相对路径进不来——
  它永远匹配不上任何被列举出来的绝对路径，留着等于一张静默失效的票。
- `any_of_several_workspaces_is_the_union`：多个工作区是并集，不是交集，也不是最长匹配。

### 2.7 等级和范围是一次决定，不是两步问

`Verb::names_a_path()` 是唯一一处"这个动词作不作路径主张"的判断，
`whoami` 是唯一不作主张的那个。所以调用方拿到的是"凭据自己回答授权"这一步
（`crates/fam-svc-auth/src/lib.rs` 的 `a_grant_is_answered_by_the_credential_itself`），
不存在"等级过了但范围忘了问"的中间态。14 个动词的名字里**没有一个**含
`token` / `permission` / `grant` / `revoke` / `audit` / `exclude` / `workspace` / `config` /
`password` / `secret`（`no_verb_reaches_the_credential_or_the_trail`），
这条是 §2.1 那个"协议面上不存在"的**命名层面**的保险：以后有人往 MCP 上摆工具时，
名字里出现这些词就是当场可疑。

### 2.8 凭据文件自己不能是数据

`Prune::from_config` 现在注入**两**条自动前缀：`audit_dir` 与 `mcp_token_file.parent()`
（`crates/fam-core/src/prune.rs`）。一条真跑的证据，把凭据文件放进搜索根**之内**：

```
$ fam --config home/state/config.toml search mcp --limit 20     backend=walk hits=0   visited=5
$ fam --config home/state/config.toml search notes --limit 20   backend=walk hits=1   visited=5
$ fam --config home/state/config.toml search report --limit 20  backend=walk hits=1   visited=5
```

`visited=5` 说明整棵树确实走过了，差别只在它在不在那个目录里。
**注意边界**：排除的是 `mcp_token_file` 的**父目录**，所以上面这跑里
`state/config.toml` 仍然搜得出来（`search config` → `hits=1`），因为我手工把凭据文件放进了
`state/auth/`。默认布局下父目录就是 `%APPDATA%\FileAutoManage` 本身：

```
$ fam config show
audit_dir       = 'C:\Users\Kirig\AppData\Roaming\FileAutoManage\audit'
mcp_token_file  = 'C:\Users\Kirig\AppData\Roaming\FileAutoManage\mcp-tokens.toml'
database_path   = 'C:\Users\Kirig\AppData\Roaming\FileAutoManage\tags.db'
```

也就是说默认情况下 `tags.db` 与 `config.toml` 一起被排除——**增量 10 的残留 3 顺带闭掉了**，
闭的方式不是新增规则而是同一条规则的父目录刚好罩住（`the_programs_own_directory_is_never_reported`
里那条 `assert!(rules.excludes(&own.join("tags.db")))` 就是这个意思）。
自定义 `mcp_token_file` 到别处时，罩不住的部分照旧是残留，见 §4.3 第 4 条。

还有一条方向不能反的守卫：`fam_core_new` 现在对**两个**目录做同样的冲突检查
（`crates/fam-ffi/src/lib.rs`，审计目录与凭据目录一起进那个 `for` 循环）——
配置的搜索根落在凭据目录之内时拒绝启动并点名是哪一个。这条**没有**为默认配置开后门：
默认 `mcp_token_file` 的父目录在 `search_roots = ['C:\Users\Kirig']` 之内，方向是"凭据在根里"，
那是常态，交给 prune。

### 2.9 没发生的授权也必须留痕（这条是自查发现的，不是我一开始做对的）

写完 CLI 之后拿真命令跑了一遍，逐条对账 `audit tail`，结果抓出两处漏记：
**由命令自己那侧做出的拒绝没有行**。

| 拒绝来自 | 例 | 修前有没有行 |
| --- | --- | --- |
| store（`add`/`revoke`/`rotate` 内部） | 非 admin 持 `*`、重复 label、坏标签、`user` 是保留名、相对工作区 | 有 |
| 命令行自己 | `--tier superadmin`（还不成其为 `Tier`）、一个 `--workspace` 都没给 | **没有** |

修前的实测：日志里 `token.add` 行数在连跑这两条命令前后**都是 13**，
而会话数从 15 涨到 17——多出来的两个会话是两对光秃秃的 `session.open` + `session.close`，
日志上看不出发生过什么。这跟"一次调用恰好一行"（增量 10 的判据）是同一类错，方向是**漏**。

修法是让这两条也走同一个 `refused()`：命令先按原样记下 `result="error …"`，再对人报错
（人类看到的是点名 `--workspace` 的那句话，日志里是 store 会用的那句话，两个受众两条文案）。
另外给 `fam mcp token list` 补了一行 `token.list`——它什么也不改，但它是 `Ask`，
而增量 10 的规矩是"Ask 永远记"；补上之后"一个光会话"本身就是异常信号，不再和"有人看了看权限表"混在一起。

修后同两条命令的实录：

```
{"seq":2,...,"op":"token.add","args":"label=no-workspace tier=edit workspaces=","result":"error a token needs at least one workspace; no-workspace has none",...}
{"seq":2,...,"op":"token.add","args":"label=bogus-tier tier=superadmin workspaces=C:/.../tree/inbox","result":"error unknown tier \"superadmin\"; it is one of browse, edit, admin",...}
{"seq":2,...,"op":"token.list","result":"3 on file",...}
```

钉住它的两条测试：`a_refusal_the_command_makes_itself_still_gets_a_line`、
`no_credential_session_is_empty`（后者断言 6 次命令 = 6 个会话 = 6 个都带 op 行，
`ops.len() == opens`）。

## 3. 解决方式

### 3.1 形态

```toml
[[token]]
id = "67280c18"
label = "inbox-reader"
tier = "browse"
workspaces = ["C:/Users/Kirig/AppData/Local/Temp/fam-inc11/tree/inbox"]
salt = "78770eea963cede06c65b22d5fb151e5"
secret_hash = "f2552708e2d146972734ca4c557be66b27d454b0d3ea528b7e5e48c8a37a3ebc"
created_at = "2026-09-24T05:49:09.410Z"
```

撤销过的行只多一行 `revoked_at = "…"`（`c9db034c` 就是这么留着的）。
凭据字符串 `fam_<8hex id>_<64hex secret>`：id 在前缀里，所以校验时先按 id 定位行，
再比对该行的摘要——**不需要把全表逐个试一遍**（那会让"命中"与"没命中"的时间差成倍数）。

`Credential::parse` 用 `split_once('_')` 加显式校验（id 必须 8 位十六进制、secret 必须 ≥ 64 位十六进制），
不是 `split_at(8)`：后者对短串或非 ASCII 输入会 panic，而输入是外人递的。
未知 id、错秘密、已撤销、格式坏四种情况返回**同一个** `None`，不看错误种类就不给方向。

### 3.2 人工面：四条命令

```
$ fam mcp token add inbox-reader --tier browse --workspace C:/.../tree/inbox
id         67280c18
label      inbox-reader
tier       browse (list, read, search, read tags and notes)
workspaces C:/Users/Kirig/AppData/Local/Temp/fam-inc11/tree/inbox
created    2026-09-24T05:49:09.410Z

credential fam_67280c18_4f9d998aa9c4a5d8beb16b852f01291649a74a8857145f15da4d8ee8cd6a410f

This is the only time the secret is shown. It is stored as a salted digest, so it cannot be
recovered from C:/.../state/auth/mcp-tokens.toml - to replace a lost one, run `fam mcp token rotate 67280c18`.
```

```
$ fam mcp token list
id        label                tier    state  workspaces
67280c18  inbox-reader         browse live   C:/.../tree/inbox
c9db034c  organizer            edit gone   C:/.../tree/inbox   revoked 2026-09-24T05:49:57.344Z
fab30566  librarian            admin live   *
```

拒绝的原文（全部实测，`exit=1`）：

```
a token needs a --workspace to act inside; without one it could read nothing at all
workspace ./secrets is not an absolute path, so it could never contain a file
only an admin token may hold "*" as its workspace; a token of tier browse cannot
the label inbox-reader is already used by another credential
bad/label is not a usable token label: letters, digits, dot, dash and underscore only
user is a name the audit trail already means; a token cannot be called that
no credential with id deadbeef
credential c9db034c is already revoked          （rotate 与 revoke 各一条）
```

label 的规则 `[A-Za-z0-9._-]{1,48}`，且 `user` / `system` / `unnamed` 保留——
这三个是审计里已经有人名的位置，让票占用它们等于给冒名留门
（`a_label_cannot_impersonate_a_real_actor`）。这是增量 10 §4.4 记下的要求：
**标签必须由签发方定死，且不能由持票一方自选**——`Actor::agent(label)` 在签发时就把 label
烤进票里，持票方能影响的只有它自己的秘密。

### 3.3 机器的读法只有一个函数

`TokenStore::verify(credential) -> Option<Authenticated>`，
`Authenticated` 里带 `token`（→ `allows(verb)`、`covers(path, roots)`、`actor()`）。
增量 12 的每个请求都是"verify → 等级 → 范围 → 记一行"这一条顺序，
`a_browse_token_cannot_write_and_an_edit_token_cannot_leave_its_folder` 就是那条顺序的浓缩版。

## 4. 解决方式效果评估

### 4.1 正面证据

`cargo test --workspace`：**440 通过、0 失败、1 条 `#[ignore]`**（那条是 §4.2 的测价，
命令写在它的注释里）。分目标：fam-svc-auth 43、fam-core 56、fam-svc-audit 42、fam-cli 19
（12 → 19，+7）、其余目标不变。`cargo clippy --workspace --all-targets` 零告警；
`cargo check -p fam-svc-auth -p fam-svc-audit --target aarch64-unknown-linux-musl` 通过。
`abi-smoke` 在新建的 GNU `famcore.dll` 上 `insert=1 cut=1 -> ok`，退出 0。

关键行为逐条点名（不只列名字，写它钉的是什么）：

- 结构：`fam-ffi` 的依赖表里没有 `fam-svc-auth`；导出 21 = 声明 21，含相关字样的 0 个。
- 摘要可外部复算：Python `hashlib` 对真令牌 `MATCH`（§2.3）。
- 白名单形状：`every_tier_and_verb_pair_is_declared`（42 组）、
  `admin_has_no_verb_that_edit_lacks`、`an_unknown_tier_in_the_file_is_an_error_not_a_default`。
- 范围：`a_workspace_covers_its_subtree_and_not_its_neighbour`、
  `containment_ignores_separators_and_case`、`the_wildcard_is_an_admin_privilege_and_means_the_configured_roots`、
  `a_relative_or_empty_workspace_is_refused_on_the_way_in`、`any_of_several_workspaces_is_the_union`。
- 秘密：`the_file_row_does_not_carry_the_secret`、`two_tokens_do_not_share_a_digest_shape`、
  `the_digest_covers_the_id_so_rows_cannot_be_swapped`、`constant_time_comparison_answers_the_same_question`、
  `a_malformed_credential_is_not_a_token`、`a_secret_verifies_once_and_no_other_secret_does`。
- 生命周期：`a_revoke_stops_the_secret_immediately_and_permanently`、
  `rotating_keeps_the_identity_and_changes_the_key`、`a_label_is_taken_once_and_case_does_not_make_it_a_different_one`。
- 文件坏了不猜：`an_unparsable_file_is_an_error_not_an_empty_store`、
  `a_duplicated_row_in_the_file_is_refused_on_load`、`a_row_missing_its_digest_is_refused_on_read`、
  `a_hand_edited_privilege_escalation_is_refused_on_load`（手改 `tier = "admin"` 的行在装载时被拒）、
  `a_stale_store_refuses_to_write_rather_than_clobber`、`the_temporary_file_never_survives_a_save`。
- 痕迹：`every_change_leaves_a_line_and_no_line_leaks_a_secret`（每次改动一行，
  且没有一行的 `args`/`result` 含秘密或摘要）、`a_refused_grant_is_recorded_and_the_trail_still_verifies`、
  §2.9 那两条新补的。一次完整人工演练（17 次命令）后的日志：

  ```
  $ fam audit verify
  1 file(s), 47 record(s), 17 session(s)
  the chain is unbroken and every session sealed its own count        exit=0
  $ fam audit stats
  1 files, 47 records, 10464 bytes (223 bytes per record)
  17 sessions, 17 sealed, 0 records reported dropped
  by op: 17 session.close / 17 session.open / 8 token.add / 3 token.revoke / 2 token.rotate
  ```

  凭据文件里 grep 三个打印过的明文：**0 命中**。
- 不落到真人机器上：这一增量的所有演练都在 `%TEMP%\fam-inc11`，
  `%APPDATA%\FileAutoManage` 里至今没有 `mcp-tokens.toml`（只有 `audit/` 与 `tags.db`）。
  CLI 测试全部自带 `--config`，`audit_dir` 与 `mcp_token_file` 指到 temp 目录——
  一条会写真人权限表的测试不能留。

### 4.2 代价

**每个请求的认证成本（实测）。** `the_price_of_one_authentication`，release 构建，
每档 200,000 次，`store.verify()` 全路径（含摘要、含常数时间比较，行已在内存）：

| 输入 | ns / 次 |
| --- | --- |
| 正确的凭据 | **1,600** |
| 同 id、秘密错一位 | 1,524 |
| 文件里没有的 id | 261 |

对照增量 10 记下的"一次审计追加 = 12 µs(release) / 41 µs(debug)"：认证大约是它的 **1/8**，
也就是说这一层不会成为请求开销的主要部分。第三条那个 261 ns 不是 bug，是"按 id 定位行"的必然结果，
它的含义见 §4.3 第 3 条。

**每进程一次的文件读**：`TokenStore::open` 读 TOML 并逐行 `accept()`。
本次**未实测**其耗时（几 KB 的一次读，与增量 10 记过的同类读同量级——这是估计不是测量）。

**基准闸门。** `cargo run -p fam-bench -- check` 十条 measured case 全部在门槛内
（`all measured cases within gate`）。这一句值得单独写，因为 prune 这次**又多注入一条前缀**
（凭据文件的父目录），"每个被列举的目录要问几次前缀"从 1 变 2：

```
audit_append 41us 1.00x ok   open_flat_dir 1032→983us 0.95x ok   open_mixed_dir 1.00x ok
patch_256_of_20k 1.01x ok    read_flat_std 0.95x ok   read_flat_win32 0.97x ok
rebuild_view_20k 0.99x ok    viewport_from_cache 1.00x ok
volume_scan_capped 586534→519647us 0.89x ok   walk_large_tree 205068→193330us 0.94x ok
```

**其中优于地板的部分不计为收益**：这台机器在跑这次基准之前，D: 卷只剩 24 MB 可用（连接器
因此报 `LNK1318 PDB LIMIT`），我清掉了 3.4 GB 的增量编译缓存才跑得动。
`volume_scan_capped` 与 `walk_large_tree` 上次被压红正是那块满盘造成的
（见 `benchmark-floors-are-machine-state.md` 与增量 10 的 §4.2）。
**机器状态变了，地板按既定原则没有重记。**

**复杂度。** 多一个 crate、七个模块、一个配置项（`mcp_token_file`）、一条 CLI 子命令树；
动词×等级是**人工维护的三份列表**，加一个动词要改三处并有 42 组断言兜着——
这是有意的摩擦，不是没找到更聪明的写法。
`SHA-256` 与 UTC 日历换算从 `fam-svc-audit` 搬到 `fam-core`（`hash.rs` / `time.rs`），
审计那边只剩 `link_of`，两处不再各存一份轮常量。

### 4.3 残留风险与没解决的东西

1. **OS 级隔离没有，这层不是沙箱。** 能以本用户身份执行任意程序的进程可以直接读本凭据文件
   （拿到的是 salt + 摘要，拿不到明文），更省事的是**它根本不需要票**——它有权读你要它读的一切。
   这层挡的是"通过 MCP/ABI 说话的那条面"。严重度：设计边界（计划明写不在范围内）。
   归属：真要防自己人，得换服务账号或容器，那是另一次增量。
2. **`admin` 目前没有比 `edit` 多的动词**，只有能持 `*`。计划里"标签词表管理"还没有载体。
   严重度：中（如果今天有人照名字理解"admin 更大"，实际只多一个 `*`）。
   归属：增量 12 落地工具清单时一并决定词表动词，然后 `admin_has_no_verb_that_edit_lacks` 会当场红——
   那条测试是快照，改的人必须显式改它。
3. **时间差会泄露"id 在不在表里"**（1.6 µs vs 0.26 µs，§4.2 第三行）。
   能被读到这一点的前提是能让本机进程反复试并测到亚微秒，而那个前提已经等于 §1 的绕过。
   严重度：极低。归属：不修，写了理由，等哪天"按 id 定位"改成"全表扫"时这条自然消失。
4. **排除的是凭据文件的父目录**，自定义到别处时罩不住 `config.toml` / `tags.db`；
   反过来，把凭据文件放进 `%APPDATA%\FileAutoManage` 会把那一整片变成不可搜。
   严重度：低（默认布局是好的，实测见 §2.8）。归属：如果哪天真要"排除到文件而非目录"，
   那需要 prune 支持文件级前缀。
5. **没有过期时间。** 一张票永久有效，直到人工撤销或轮换。
   严重度：中低。归属：交付给真人长期用之前需要 `expires_at`（计划没要求，这次不发明）。
6. **`list` 看不出哪张票真被用过。** `verify()` 不写审计行——写了就变成"被审计方 produces 日志"，
   而且每个请求两行。今天从 CLI 面上看，只有改动才有行。
   严重度：低。归属：增量 12，那时被验证的一方本身就有行（`actor = agent:label`），
   "用过"从请求行里数得出来。
7. **`*` 的含义跟着 `search_roots` 漂。** 管理员票 + `*` = 当前配置的根集合；
   改配置就改了边界，而票上看不出来。可核对的面是 `fam config show` 与会话里的
   `session.open` 参数。严重度：低。归属：12 之后考虑把 roots 摘要写进会话行。
8. **id 只有 32 位**（8 个十六进制字符）。撞车由 `DuplicateId` 拒绝；
   想靠枚举 id 登录必须同时猜中 256 位秘密，不可行。严重度：极低。
9. **非 Windows 上没设 0600。** 写文件走 `OpenOptions` + rename，权限继承 umask；
   Windows 上靠 `%APPDATA%` 的 ACL（默认只有本用户与 SYSTEM）。
   **未实测**任何 POSIX 权限位。严重度：低（这层今天只在 Windows 上跑）。
   归属：真上 Linux 时补 `permissions(0o600)`。
10. **`fam-svc-walk` 的 lib 测试仍带着那条偶发红**（`--lib` 一次跑出 10 通过 / 1 失败，
    随后 6 次干净；嫌疑是 `deep_queues_recruit_more_workers_than_a_flat_start` 的 30 秒上限在高负载下不够）。
    本增量没碰它。严重度：低（不影响正确性判断，但会脏 CI）。归属：增量 12 之前定死。
11. **`list` 的列宽不跟数据长**（`{:<6}` 遇上 `gone` 就错位一格，实测输出里可见）。只是难看。
    严重度：极低。

### 4.4 恶意行为评估（能不能提权 / 能不能甩锅 / 会不会变成 DoS）

- **提权**：程序性的入口只有一个——改这张表，而改表只有人手那条面（§2.1 的依赖事实）。
  手改文件能被抓住的是**改一半**的那种：把 `workspaces` 改成 `*` 而 `tier` 还是 `edit`，
  装载时整份文件拒读并点名原因（`a_hand_edited_privilege_escalation_is_refused_on_load`，
  报的是 `Parse`，理由原样带出 "only an admin token may hold"），
  等级与工作区是**互相核对**的，不是各读各的。
  **抓不住的要说清**：能写这个文件的人可以自己算一条自洽的 admin 行
  （`tier = "admin"` + `workspaces = ["*"]` + 自己选的 salt 与摘要），
  或者整份换成自己的一份。**文件就是权威，写它等于签发**，而且这条路不留审计行——
  日志记的是"store 做了什么"，不是"文件长什么样"。这不是推演，是演过一遍：

  ```
  # 手工追加一行，salt 与 secret_hash 由 Python 按同一公式算出（secret = "a"*64）
  $ cat >> state/auth/mcp-tokens.toml        # id=deadbeef label=intruder tier=admin workspaces=["*"]
  $ fam mcp token list
  deadbeef  intruder             admin live   *          exit=0
  $ fam audit tail -n 80 | grep -c intruder
  0
  ```

  这与增量 10 那条"整链重写=一本自洽的假账"是同一个盲区，同一句解法：
  需要一份不在同一块磁盘上的锚。严重度：中（设计边界，前提是已能以本用户身份写文件）。
  归属：§4.3 第 1 条的 OS 级隔离；在那之前，人的核对面是 `fam mcp token list` 与文件本身。
- **甩锅**：label 签发方定、保留字挡住 `user`/`system`/`unnamed`、`id` 进摘要防换行冒名（§2.3），
  撤销保留行防"抹掉历史"。剩下的口子是**人自己**：同一个人签发两张同名不同用途的票是禁止的
  （`LabelTaken`，大小写不敏感），所以日志里的 `agent:<label>` 在任一时刻只对应一张票。
- **DoS**：这层新增的可被打死的地方是"凭据文件写不进"——`save()` 失败就不改内存
  （`a_stale_store_refuses_to_write_rather_than_clobber` 的后半段验证的就是这个），
  于是要么都成功要么都不动。真正的失控点是 `verify` 被拿去做密码学放大攻击：
  每次一个 SHA-256、1.6 µs，攻击者没有票也就要付这 1.6 µs（他连 id 都猜不到），
  不构成放大。**没做**的是速率限制：一个乱递凭据的客户端每个请求只花 1.6 µs 就被拒，
  这层的成本对攻击者毫无意义，防线在增量 12 的 stdio 那侧（协议面本身只有一条管道）。
