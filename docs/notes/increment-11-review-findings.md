# 增量 11 复查：`..` 能从一张只授权一个文件夹的票下面走出去

- 增量：11 复查（不改计划，把刚落的那一层按缺陷重读一遍）
- 提交：`fix(auth)`（本文件之前一笔）
- 涉及：`crates/fam-svc-auth/{scope,token,store,lib}.rs`、`crates/fam-core/src/config.rs`、
  `crates/fam-ffi/tests/audit.rs`、`apps/fam-cli/src/commands.rs`、
  `docs/notes/tokens-are-human-only-and-tiered.md`

## 1. 描述问题

做法照前几次：自己逐行读，同时派两个只读 agent 分头审——一个审 `fam-svc-auth` 本体
（摘要、白名单、范围、随机数、并发写），一个审接线（依赖图、CLI 的记账路径、config/prune、
FFI 的两道守卫、以及**这份笔记里说过头的地方**）。纪律不变：agent 说的每一条都要读到那一行才算数，
能用实验判定的就用实验判定。

结果：**确认并当场修 10 处**，其中一处是"能越出授权范围"级别的；**驳回 3 条**并给出证据；
**新增 3 处确认不修**并标严重度与归属。三份"说过头"的注释和四处笔记措辞按实际能力收回。

最值钱的一条不是判据方向错，而是**判据用对了身份、却用错了写法**：

`covers` 全部走 `PathKey`，没有一处字符串前缀比较——这在上份笔记里被当成 §2.6 的结论。
但 `PathKey` 折叠的是分隔符、ASCII 大小写与 `\\?\` 前缀，**它不解析组件**：
`D:\notes\..\windows` 的键里那两个点还在原地。于是 `is_within(D:\notes, 那个键)` 说"在范围内"，
而 `CreateFile` 打开的是 `D:\windows`。一张 `browse` 票可以列举盘根。

第二条值得单说：`a_stale_store_refuses_to_write_rather_than_clobber` 里那句
`assert_eq!(stale.tokens().len(), 2, "it still holds what it read")` 是把一个缺陷**钉成契约**——
它读到的是一行，被拒的写入之后手里是两行。断言的字面意思和它的说明文字已经互相矛盾，
而测试一直绿。

## 2. 分析问题

### 2.1 `..`：为什么不能在这一层做规范化

被抓出来时的第一反应是"那就 `canonicalize()` 一下再比"。这条被自己否掉：

1. 解析需要文件系统。同一个 `D:\notes\..` 在普通目录上是盘根，在 junction 上是链接目标的上层——
   这层若自己算一遍，就和真正去打开那个路径的那一层**可能算出两个答案**，而授权判断必须和
   实际发生的事一致，哪怕两边都错。
2. 解析要花钱（一次 `GetFileAttributesEx`），每个请求的每次路径比较都付一次。
3. 更根本的是：`covers` 的问题不是"这个文件在哪"，而是"调用方**说**它在哪个工作区下面"。
   一个还没解析过的路径本来就没有答案，所以正确答案是**不回答**（`false`），不是替它猜一个。

落地成两侧同一条规则：`Workspace::parse` 拒收带 `.` / `..` 的授权（新错误 `DottedWorkspace`），
`covers` 对带点的查询路径一律 `false`，文件里的行也读不进来。

一个实现细节值得记：第一版用 `Path::components()` 匹配 `Component::CurDir`，
测试当场红——**`components` 会悄悄丢掉内部的 `.` 段**。`D:/notes/./x` 和 `D:/notes/.`
都拿不到 `CurDir`，于是尾部那个点的写法漏过去，而一张拼成 `D:/notes/.` 的票
恰好是"静默什么都不覆盖"的那种票（键带一个尾点，任何被列举出来的路径都不带）。
改成拆原始文本按 `/` 与 `\` 逐段比。

### 2.2 手改这张表时，"认不出来"必须是失败而不是空

`TokenRow` 派生 `Deserialize` 而没有 `deny_unknown_fields`。失效路径是这样的：

- 人手工把 `revoked_at` 写成 `revokedAt`（或 `revoked`、或编辑器换行时把 key 拆断）→
  serde 忽略不认识的 key → 这一行**读成没有 `revoked_at`** → 刚收回的权限又活了；
- 把 `[[token]]` 写成 `[[tokens]]` → `Document.token` 走 `#[serde(default)]` → **整份读成空表**，
  而 `list` 会老实打印"no credentials yet"；下一次 `add` 把这份"只有一行"的表存回磁盘，
  **其余授权全没了**，日志里只有一行 `token.add`。

两条都是"错误路径往**放宽**的方向掉"。所以 `Document` 与 `TokenRow` 一起上
`deny_unknown_fields`——包括 `Document`，虽然它的注释写着"为将来加顶层字段留余地"：
加顶层字段本来就要改这个结构体，那个宽容度换不到任何东西，换到的是**静默丢表**。
新增 `a_misspelled_key_is_refused_rather_than_read_as_a_default`，三种拼错写法加一种表名拼错，
四种都必须报错而不是读成"没有"。

### 2.3 并发守卫有三处洞，其中一处正好在"第一张票"上

`fn save(&self)`——不是 `&mut self`。这条签名把守卫限死成一次性：

1. **同一进程的第二次保存必失败**。基线永远停在打开时读到的那份，而第一次保存已经把文件改成了
   自己写的内容 → 第二次比较必然不等 → `ConcurrentEdit`。今天没红，是因为 CLI 一命令一进程一保存；
   增量 12 的服务进程会长时间持有这张表。
2. **文件原本不存在时，守卫永久关闭**。`loaded_digest = None` → 整个 `if let Some(..)` 跳过 →
   两个进程都"读到没有文件"，后写的那个用 rename 盖掉前一个的表。这正是这份文件
   唯一不允许犯的错（丢一次撤销＝静默保留权限），而它偏偏活在**第一张票**的窗口里。
   现在 `None` 的语义变成"文件必须仍然不存在"。
3. **临时文件名是定死的** `mcp-tokens.toml.tmp`。两个并发保存共用一个名字：rename 是原子的，
   **往临时文件里写不是**，于是一次 rename 可能把别人写了一半的字节发布出去。名字里加进程号。

三条一起改完，配两条新测试：`a_second_change_in_the_same_session_is_not_a_conflict`、
`a_store_that_read_no_file_refuses_to_overwrite_one_that_appeared`。

### 2.4 内存不能跑在磁盘前面

`add` / `revoke` / `rotate` 都是先改 `self.tokens` 再 `save()?`。保存失败时，
这个进程继续用一份**磁盘上不存在的表**回答 `verify`——而且那行审计也不会写
（`record` 在 `save` 之后）。今天 CLI 立刻退出所以看不出来；一个长驻的 store 就会看出来。

改法是三段式：在 `self.tokens.clone()` 上改 → `save_as(&rows)` → 成功才 `self.tokens = rows`。
于是"改动生效"与"内存里有这一行"是同一件事。§2.1 那条把缺陷钉成契约的断言改成 1，
并在说明文字里写清楚它以前是多少、为什么那是错的。

### 2.5 一次改动落没落地，之前是问不出来的

`AuditLog::record` 返回 `()`，丢弃走 `self.lost(..)`，唯一的痕迹是一条 `log::warn!` 和一个
"由下一条落地的记录带出来"的计数。所以理论上存在：**票发出去了、明文打印了，日志里一行没有**。

`AuditLog::health()` 是公开的，里面有 `writing` / `records` / `dropped_total` / `last_error`，
所以这件事不必靠自觉：

- 三个改动入口在动手之前先看一眼 `health().writing`，不为真就
  `Err(Error::Audit { .. })`，**什么也不改**。这就是 core 对浏览用的那条"无日志不浏览"，
  现在同样管着"无日志不授权"。
- 落不落地仍无法在写之后挽回（改动已经在磁盘上），所以把计数递给调用方：
  `TokenStore::trail_dropped()`，CLI 在命令末尾发现非零就往 stderr 喊一句
  "文件里有的和日志里说的不是一份，去跑 `fam audit stats`"。

**这条只有前半是测得到的**：`writing = false` 需要"句柄还开着而写不进"，
Windows 不允许删自己正打开的文件，测试里构造不出这个状态（`AuditLog::open` 阶段失败的话
连 store 都没有）。所以它是**读得懂、不是测得到**的一道预检；测得到的是老那条
`every_change_leaves_a_line_and_no_line_leaks_a_secret`。写在这里，免得下次把它当成有覆盖的防线。

### 2.6 `Debug` 是这条链上唯一没人审的出口

`Token` / `Issued` / `Credential` 都派生 `Debug`，里面有明文秘密与摘要。
今天没有任何地方打印它们（`describe()` 只印 id/label/tier/workspaces）。但
`TokenStore::audit()` 是**故意**公开的——它就是让调用方能把"被拒的尝试"记进日志，
于是 `Draft::args(format!("{:?}", credential))` 离这条链被写脏只有一次重构。
改成手写 `Debug`：秘密与摘要一律 `<hidden>`，其余字段照旧。
`a_debug_dump_hides_everything_that_unlocks_a_credential` 钉三种 `{:?}`。

### 2.7 三处"说过头"（两条在代码注释里，一条在笔记里）

- `config.rs` 的 `audit_dir` / `mcp_token_file` 写着"never inside anything the tool indexes /
  excluded from every listing"。`Prune` 只被**递归**列举与搜索问；`fam_open_dir` 走单层读，
  根本不问 `Prune`。**这不是推演**：把 abi-smoke 的树参数直接给凭据目录——
  `dir token=1 rows=1 / first row=mcp-tokens.toml kind=6 directory=0`，退 0。
  注释按实际能力改写，并把"今天只泄露存在性与尺寸（21 个导出里没有读内容的入口），
  增量 12 的 `fs_read` 一落地就变成完整泄露"这个方向写进去。
- `scope.rs` 头写着"一个在排除里的路径会在问到范围之前被拒"。**没有任何地方做那个拒绝**——
  `covers` 从来没见过 `Prune`。改成说明白：范围判断只能自己回答对，它拿不到排除规则。
- 笔记 §4.4 写"save() 失败就不改内存，于是要么都成功要么都不动"——§2.4 说明它当时是假的。
  现在它因为改了代码而为真。

### 2.8 一道没有测试的守卫等于一道会被删掉的守卫

`create()` 里那个两臂循环，只有审计那一臂有测试（`crates/fam-ffi/tests/audit.rs` 只构造了
`audit_dir` 冲突）。把凭据那一臂整个删掉，446 条测试照绿。补
`a_search_root_inside_the_credential_directory_refuses_to_start`，并要求错误文案点名
是哪个目录（"credential directory"），否则两条守卫合并成一条也无人发现。

## 3. 解决方式

十处修的落点（每条给文件与判据）：

| # | 缺陷 | 落点 | 钉住它的测试 |
| --- | --- | --- | --- |
| 1 | `..` 越出工作区 | `scope.rs:43` `has_dots`，用在 `parse`（`scope.rs:55`）、`covers`（`scope.rs:91`）与 `Deserialize` 三处；新错误 `lib.rs:83` `DottedWorkspace` | `a_path_that_has_not_been_resolved_covers_nothing`、`the_file_round_trips_both_spellings` 新增断言 |
| 2 | 拼错的 key 读成"没有" | `token.rs:303` 与 `store.rs:38` 各加 `#[serde(deny_unknown_fields)]` | `a_misspelled_key_is_refused_rather_than_read_as_a_default` |
| 3 | 基线不前进 | `store.rs:262` `save_as(&mut self, rows)`；`store.rs:332` 把基线换成刚写出去的字节 | `a_second_change_in_the_same_session_is_not_a_conflict` |
| 4 | 新文件窗口 | 同上，`None` 分支改成"文件必须仍不存在" | `a_store_that_read_no_file_refuses_to_overwrite_one_that_appeared` |
| 5 | 临时名共用 | `store.rs:330` `with_extension(format!("toml.tmp.{}", pid))` | `the_temporary_file_never_survives_a_save`（改成扫目录里任何 `mcp-tokens.toml*` 残留） |
| 6 | 失败保存仍改内存 | 三个入口改成 clone → `save_as` → 赋值 | `a_stale_store_refuses_to_write_rather_than_clobber`（断言从 2 改成 1） |
| 7 | 无日志也能授权 | `save_as` 开头查 `health().writing`；`store.rs` 的 `trail_dropped()` 交给 CLI 喊 | 见 §2.5 的坦白 |
| 8 | `Debug` 泄秘密 | `token.rs` 三个手写 `Debug`，掩码常量 `token.rs:96` | `a_debug_dump_hides_everything_that_unlocks_a_credential` |
| 9 | 装载时 label 不设大小写防 | `store.rs:110` `check_identifiers` 改用 `eq_ignore_ascii_case`，与 `add` 同一条规则 | 装载侧走的是同一个函数，`a_duplicated_row_in_the_file_is_refused_on_load` 保形状 |
| 10 | 结构 | `rotate(&mut self, id)` 去掉没用到的 `now` 参数（原来靠 `let _ = now;` 静音） | 三处调用点跟着改 |

外加接线侧一处：`crates/fam-ffi/tests/audit.rs` 补上凭据目录那一臂的测试（§2.8）。

## 4. 解决方式效果评估

### 4.1 正面证据

测试 440 → **446**（+6：scope 2、store 3、token 1，`fam-ffi/tests/audit.rs` +1），
0 失败；`cargo clippy --workspace --all-targets` 零告警。
`famcore.dll` 用 GNU 工具链重建后 `abi-smoke` 照旧 `insert=1 cut=1 -> ok`，
导出仍 21 个，与 `apps/fam-shell/include/famcore.h` 里非注释声明的 21 个
**双向差集为空**（`comm -23` 与 `comm -13` 都空）——修这十处没有动到协议面。
`--ignored` 那条测价仍给出 accept 1.6 µs（`verify` 的形状没改，`has_dots` 在 `covers` 侧）。

五处修复逐条做了 A/B：**把修复本身拿掉，跑对应的那条测试**，跑完按哈希还原。
每一次都是"没有它就红"，不是"反正会绿"。

| 拿掉的东西 | 那条测试的反应 |
| --- | --- |
| `covers` 里的 `has_dots` 分支 | `panicked at scope.rs:241` — `assertion failed: !granted.covers(&path("D:/notes/../../windows/system32"), &roots)` |
| `TokenRow` 的 `deny_unknown_fields` | `panicked at store.rs:837` — `revokedAt` 那一档就红（读成了活的行） |
| `save_as` 末尾那句基线前进 | `panicked at store.rs:784` — 同进程第二次保存 `ConcurrentEdit` |
| `None` 分支的"文件必须不存在" | `panicked at store.rs:806` — 后开的进程把自己的表盖到别人刚写的表上 |
| `create()` 两臂循环里的凭据那一臂 | `crates/fam-ffi/tests/audit.rs` 11 passed / **1 failed**（`a_search_root_inside_the_credential_directory_refuses_to_start`），其余 11 条完全不受影响——正是"删掉一道守卫无人发现"的形状 |

第 6 项（失败保存仍改内存）是从**反方向**证到的：修好之后，那条原本钉着 `2` 的旧断言立刻红
（`left: 1, right: 2`），说明它之前确实在替缺陷作保；断言改成 1 之后全绿。

（还原核对：`sha256sum -c` 对 `scope.rs` / `store.rs` / `token.rs` / `fam-ffi/src/lib.rs`
四个文件全部 byte-exact，A/B 没有留下残迹。）

### 4.2 代价

- **`covers` 每问一次多一次文本拆分。** `to_string_lossy` 在 Windows 的常规路径上是 `Cow::Borrowed`
  （不分配），拆段是 O(长度)。本次**未实测**每次请求的增量，因为 `covers` 还没有调用方；
  量级上它必然小于认证本身（1.6 µs）。归属：增量 12 第一次端到端跑时量。
- **`deny_unknown_fields` 让"新字段 + 旧程序"变成硬失败**，而不是静默少读一个字段。
  这是选出来的代价：读不懂的东西要响，尤其当"读不懂"会掉进"表是空的"。
- 多一个错误类型、两个测试辅助、一处手写 `Debug`。

### 4.3 驳回的三条（附证据）

1. **"`verify` 的时间差要修，它区分四种失败"**。成立的事实：未知 id 0.26 µs、错秘密 1.52 µs、
   正确 1.6 µs，已撤销在算摘要之前返回。不修的理由：要测出这个差必须让**本机**进程反复调用并计时，
   而那种前提之下整个凭据层已经绕过（它读得到文件、也发得出票）。这条被**接受为文档缺陷**：
   上份笔记 §4.3 第 3 条只写了"id 在不在表里"，收窄了，已扩成三个问题，并点明
   `verify` 的注释说的"同一个答案"指返回值不是耗时。
2. **"`validate()` 应当禁止 `mcp_token_file` 落在搜索根之下"**。这条如果落地，出厂配置就拒绝启动：
   Windows 默认 `mcp_token_file = %APPDATA%\FileAutoManage\mcp-tokens.toml`，
   而默认 `search_roots = ['C:\Users\Kirig']` 罩住它（`fam config show` 实录见上份笔记 §2.8）。
   增量 10 已经在审计目录上踩过同一跤并给出方向敏感的判据 `conflicting_root`，
   这里用的就是它——**根落在凭据目录之内**才拒绝启动（新测试钉住）。
3. **"装载时 id 也该大小写不敏感"**。`add` 侧本来就是十六进制小写；两条只差大小写的行
   **不可能被同一个秘密解锁**（摘要按逐字文本算），而 `revoke`/`rotate` 按精确 id 命中，
   输错大小写得到的是 `UnknownId`，不是撤销了另一张票。方向是"人要输对"，不是提权。不修。

### 4.4 确认不修的三条（严重度与归属）

1. **单层直开仍然能列出凭据目录与审计目录**（实测见 §2.7）。
   今天暴露的是存在性、尺寸、修改时间（21 个导出里没有读内容的入口）。严重度：**中**，
   且**在增量 12 加 `fs_read` 的那天升为高**——那时这张表连同 salt 与摘要会被完整读走。
   归属：增量 12。修法不是在这层再立一个判据，而是让 `covers` 拿到 `Prune`，
   "在范围里"与"在排除里"在同一次判断里问完（`scope.rs` 的头注释现在就是这么写的）。
2. **`TokenStore::open` 失败不留名**。审计日志能开而凭据文件读不懂时，日志里只有
   `session.open` + `session.close` 两行，看不出有人试过什么。严重度：**低**
   （人当场看到错误；且这条命令什么也没改）。归属：增量 12——那时"该记哪一类 op"的
   答案才凑齐（今天为它发明一个 op 名，是在给还没有的调用方定契约）。
3. **`Prune` 的排除前缀同样不解析 `..`**：`D:/notes/../secret` 不被 `D:/notes` 那条排除挡住。
   严重度：**低**——失效方向是**多报**（把排除内的东西报出来），而今天没有任何调用方递未解析路径
   （值都来自 `fam_open_dir` 或配置）。归属：与第 1 条同一处（12 在边界上把两件事一起问完）。

上一份笔记里已经写了不修的东西（OS 级隔离、没有 `expires_at`、`list` 看不出哪张票用过、
`*` 跟着 `search_roots` 漂、`fam-svc-walk` 的 lib 偶发红、非 Windows 上没设 0600）
这次不重复，仍然成立。
