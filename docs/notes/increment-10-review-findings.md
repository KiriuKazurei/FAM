# 增量 10 复查：审计者自己能被一句话关掉，以及"没有记录"被判成干净

- 增量：10 复查（不改计划，把刚落的那一层按缺陷重读一遍）
- 提交：`fix(audit)`（本文件之前一笔）
- 涉及：`crates/fam-svc-audit/{chain,inspect,writer,lib}.rs`、`crates/fam-ffi/{src/lib.rs,tests/audit.rs}`、
  `crates/fam-core/src/config.rs`、`crates/fam-svc-walk/{src/walk.rs,src/search.rs}`、`apps/fam-cli/src/commands.rs`

## 1. 描述问题

做法照前几次：自己逐行读，同时派两个只读 agent 分头审——一个审 `fam-svc-audit` 的链与记账，
一个审三处接线（FFI 外壳、config/prune、walk 与 CLI）。纪律不变：**agent 说的每一条都要读到
那一行才算数**。

结果：**9 处确认并当场修**（其中两处是"这道检查反过来害人"级别），**驳回 2 条**（一条是设计边界
被当成缺陷，一条是把快照语义当成 off-by-one），**新增 2 处确认不修**并连同上一份笔记里的 9 条
一起标严重度。

两条最值得说的都不是"算错了"，而是**判据的方向错了**：

1. `verify` 在没有任何记录时返回"无 finding"。于是把审计目录清空、把 `audit-*.jsonl` 改名成
   `notes.txt`、或者整个删掉之后——**只要还剩一个空目录**——`fam audit verify` 打印
   "the chain is unbroken and every session sealed its own count" 并退 0。删日志是消音最省事的
   写法，把"无话可说"判成"一切正常"，等于把这道检查倒过来交给攻击者用。
2. 读路径用 `fs::read_to_string`。**往当天的文件追加一个 `0xFF` 字节**，`verify` 与 `tail` 同时
   返回 I/O 错误——一个字节就能让审计者读不了自己要审的东西，而 `tail` 恰恰是"`verify` 抱怨时
   人该敲的那条命令"。

## 2. 分析问题

### 2.1 "没有记录"和"记录都在"是两种答案

修法是加一条 finding 而不是加一个 bool：`Problem::NoRecords { files }`，在 `records == 0` 时入账
并直接返回。两个方向都要——空目录、以及"目录里有文件但没有一个是我们命名的文件"——都算不上干净，
`an_empty_log_is_not_a_clean_bill_of_health` 把两种都钉住（后者用 `notes.txt` 触发：`files()` 只认
`audit-*.jsonl`，所以它是 0 个日志文件、0 条记录）。

### 2.2 审计者的工具不能比被审的对象更脆

`inspect.rs` 的读法整体下沉到字节：`read` 返回 `Vec<u8>`，`split_lines` 只按 `\n` 切、什么都不 trim。
两个理由：

- 哈希的对象是**行的原始字节**，任何"顺手修一下编码"都会毁掉正要审的证据；
- 结尾没有换行的半行**必须留下**并报告为 Unparsable，而不是被当成"最后一行是完整的"丢掉。

`tail` 用 `from_utf8_lossy` 只做显示，`verify` 用 `serde_json::from_slice` 直接吃字节，链比对
`chain::link_of(&lines[..].2)` 用的还是原始字节。测试
`a_byte_that_is_not_utf8_is_reported_rather_than_refused` 先写一份真会话日志、再追加
`FF FE \n`，然后要求：`verify` 仍能跑并给出 Unparsable + BrokenLink，`tail` 仍能返回 6 行
（5 条记录 + 那个不是记录的字节）。

### 2.3 搜索作用域被排除时说的谎，是增量 6 那道疤的同一处

`walk()` 现在在播种处拒掉"位于排除之下"的 root（这是把前缀判定挪到目录级的必然补票：root
没被谁 descends 过，不在目录级判定的覆盖面里）。但 `search.rs` 的完整性是
`!report.stopped_early()`，而"root 被排除"三个早退位一个都不置——于是
`fam search x --in <审计目录>` 会答"0 命中，已覆盖"。这正是
`scoped-search-was-empty-and-said-so.md` 处理过的失败类型：**返回空还声称找过**。

而且它**在出厂默认配置下就是活的**：审计目录本身就是那条排除。修法是把这件事单独记账
（`WalkReport::excluded_roots`），完整性由它参与：

```rust
outcome.complete = !report.stopped_early() && report.excluded_roots == 0;
```

测试 `a_scope_that_was_refused_says_it_was_not_covered`。CLI 那行也分开打印，因为
"预算用完了"和"你点名的目录不许搜"是两句话。

### 2.4 一个相对路径能让两个守卫同时失聪

`validate()` 原来只查 `audit_dir` 非空。相对值一路通过：`Prune::from_config` 存下
`path_key("audit")` = `"AUDIT"`，**它永远匹配不上任何绝对的遍历键**；
`conflicting_root` 同样比不出来。后果不是"日志在奇怪的地方"而已，而是**日志被搜索、被列举、
可能位于被索引的树里**——两道防护一起没了。

更要紧的是这不是用户写错才会有的状态：`AppPaths` 在 `dirs` 答不出来时回退
`.fam/FileAutoManage`（相对）。所以两处一起改：`validate()` 拒绝非绝对路径，回退锚到
`current_dir()`。后者顺带把 `database_path`/`config_file` 的同类毛病一并止住——
`every_default_location_is_absolute` 四条一起断言。

驳回一个更"彻底"的方案：`canonicalize()` 一次。它把不存在的路径也变成错误，而
"目录还没建"是这个程序的正常初态（`AuditLog::open` 自己 `create_dir_all`）。

### 2.5 两个 close 之前会替没发生的事作证

`fam_dir_close` / `fam_search_close` 签名里没有 `err` 槽，所以审计行是它们唯一的输出；
而两处都是 `borrow.dir_close(token); record("ok")`——`dir_close` 的 `bool` 被丢掉，
`search_close` 连返回类型都是 `()`（内部 `searches.close` 明明给了 bool）。传一个不存在的
token，或者把同一个 token 关两次，日志里就多一条"释放过了"。同一层里 `fam_dir_fetch`
遇到未知 token 记的是 `error unknown view token`，两件事的说法不该不一样。

修法不是就地补 if，而是加**第五个外壳** `with_core_void`：一次调用、一行、从结果决定写什么，
而且带上其他四个形状早就有的 `catch_unwind`。顺带纠正一处已经写在文档里的过度承诺——
lib.rs 头注释说"每个入口点都捕获展开"，而这两个在此之前是唯一的例外（我读到 callee
`browse.rs:200-220` / `search.rs:207-216` 的锁都能从 poison 恢复，今天到不了 panic，
但契约的形状不该靠这个）。测试 `a_close_that_released_nothing_says_so`。

### 2.6 记账的三处：谁胜出、谁不归零、谁按会话数

- **原因被改写**：50 条 cap 抑制后跟着 1 次写失败，`dropped_because` 从 `"cap"` 变成 `"io"`——
  一个"跑满了当天额度"的日子会被读成"磁盘出过错"。改成**开洞的那个原因胜出**
  （`suppressed()` 只在 `dropped_because.is_none()` 时写）。
- **计数会被带走并清零**：`dropped` 的语义是"下一条落地记录携带的洞"，携带之后就没了；
  于是 `fam_stats` 里看不出这一天曾经挤满过。加 `dropped_total`（永不归零）与
  `daily_limit_bytes`（`bytes_today` 单独摆在那儿没有意义，要有可比的那个数）。
- **封印按文件尾计数**：`sealed_sessions` 原来是"每个文件最后一行是不是 close"。一天一个文件
  可以装任意多个会话，只有最后一个装在文件末尾——所以两个会话的日志报"2 sessions, 1 sealed"，
  我上一份笔记里贴的正是这个错数字。改成按会话看最后一条记录
  （`the_summary_counts_seals_by_session_not_by_file`）。

### 2.7 时钟往回走会自己制造断链

轮转的条件是 `self.day != Some(day)`，所以跨午夜后时钟回拨（NTP 校正、时区改动）会让这个会话把
**更新的一行追加进更早的那个文件**。`verify` 按"文件名顺序 → 文件内顺序"读，于是这条最新行排在
它的旧行之前——`BrokenLink` 加 `Sequence`，一份诚实写的日志被判成被改过。
修法：手里还有后一天的健康句柄时不回拨轮转（文件名里的日期宁可落后，链不能倒着走）；
但如果句柄已经坏了，仍然在当下这个日期重开——**拒绝写会把记录丢掉，而落后只是把日期写得难看**。
`a_clock_that_steps_backwards_keeps_the_chain_intact`。

同一处注释（`chain::link_of`）还承诺了做不到的事：它写"能抓跨文件移动的行"。链只保证会话内
相对顺序，没有任何东西把 `ts` 与所在文件名绑起来，所以把某一行挪到另一天的文件尾部，
每一环和每个计数都仍然自洽。注释改成四个能抓、两个抓不到（跨文件移动、整链重写），
本笔记 §4 也补上第一条。

**为什么不顺手加"ts 的日期必须等于文件名日期"这一检**：它会在 §2.7 那个真实场景里把诚实的日志
判成篡改（时钟向前跨过午夜、当天文件里留下后一天的行），而且攻击者重算链时本来就会顺手改 `ts`。
一条会误报又拦不住目标对手的规则，不如不写。

## 3. 解决方式

九处逐条落在 `fix(audit)` 里，改动集中在四个位置：

| 位置 | 改动 |
| --- | --- |
| `inspect.rs` | `Problem::NoRecords`；`read`/`split_lines` 按字节；`summary` 按会话数封印 |
| `writer.rs` | `suppressed()`（原因胜出）+ `dropped_total` + `last_dropped_because`；不回拨轮转 |
| `fam-ffi/src/lib.rs` | `with_core_void`；两个 close 走它；`search_close` 返回 `bool`；`AuditOut` 两个新字段 |
| `config.rs` / `walk.rs` / `search.rs` / `commands.rs` | 绝对路径校验与回退；`excluded_roots` 与 `complete`；CLI 分开说 |

另外删掉一个从没被任何调用方读过的 `inside_a_root`（写了、注释里说"值得在 stats 里报"、然后没接），
它的两条测试合并成一条 `conflicting_root` 的正反两向断言。

## 4. 解决方式效果评估

### 4.1 正面证据

- 上面各条都点名了测试；全量：**389 条通过，0 失败**（`cargo test --workspace`），
  `cargo clippy --workspace --all-targets` 零告警，`cargo fmt --all` 干净。
  上一份笔记的 382 条 → 389 条：+3（inspect）+2（writer）+1（ffi 的 close）+1（search 的 refused scope）
  = +7，另有一处两条 lib 测试合并成一条（−1），净 +6 记账在此。
- **端到端重跑，不是只跑单测**：重建 GNU `famcore.dll` 与壳之后，
  `abi-smoke` 仍是 `insert=1 cut=1 -> ok`、`SMOKE=0`；同一条 `--self-check-mutate` 会话
  仍是 **24 行 / 5,836 字节 / 243 B 每行**（修前 5,835），说明这一批修改没有动体积；
  会话结束后 `fam audit verify` 干净、退 0，`audit stats` 报 `1 sessions, 1 sealed`。
- 地板复测（同一台机、同一个 debug 配置）：`open_flat_dir` min **1,062µs**（地板 1,032，1.03x），
  `walk_large_tree` min **209,073µs**（地板 205,068，**1.02x**——比本次增量开始前那两次 1.96x/2.81x
  都更接近地板），`audit_append` min **41µs**（未变）。
  `volume_scan_capped` 仍受 D: 卷 100% 满影响，按既定原则**不重录地板**（见
  `benchmark-floors-are-machine-state.md`）。
- 修前后方向都能反证：`NoRecords` 与 UTF-8 那两条，我先在真日志副本上看到"改之前确实判干净 /
  确实拒绝运行"，再改代码；§2.3 的 complete 位则有 `walk.rs` 那条
  `an_excluded_root_is_skipped_rather_than_walked` 与新的 search 测试一头一尾。

### 4.2 代价

- `verify` 现在对"新装还没跑过"的环境会**报错退 1**（NoRecords），不再退 0。这是刻意的，但它是
  一个行为改变：把 `fam audit verify` 塞进任何定时检查的人，得先把"确实还没用过"和"日志被删了"
  区分开——目前的说法是"no records in 0 file(s)"，够用但不自动。
- `inspect.rs` 的行从 `String` 变成 `Vec<u8>`：每条记录多一份行缓冲的拷贝。`tail`/`verify` 是
  CLI 手工跑的，不在任何热路径上。
- `excluded_roots` 让 `WalkReport` 多一个公开字段（`bench`、CLI、`search` 三处读者）。
- `with_core_void` 是第五个形状而不是四个之一——外壳数量增加，但它换掉的是"两个入口点自己
  手写记账"这种更容易走偏的形状。

### 4.3 残留与不修（含上一份笔记的 9 条，此处只列新增与仍成立的要点）

1. **新增｜unix 上"删掉正被打开的日志文件"会静默全丢**：`rotate_to` 只在句柄为 `None` 或日期变化时
   重开，而 posix 允许 unlink 一个打开中的文件——之后 `write_all` 继续成功写进孤儿 inode，
   `Health.writing` 仍为 `true`。严重度：**低**（壳只在 Windows 上跑；Windows 的 share mode 会让写
   失败并走已测的自动重开路径，见 `a_write_failure_is_counted_and_the_log_recovers_on_its_own`）。
   归属：真要上 posix，再加"每 N 条或每次日期变化时重开一次"。
2. **新增｜封印写在 `catch_unwind` 之外**：`fam_core_free` → `AuditLog::drop` 走的是 `get_mut`，
   不在任何展开屏障里。严重度：**极低**（这条路径上没有可 panic 的操作：`write_all` 的错误被吞、
   序列化失败走 `lost()`）。归属：增量 12 把 core 交给 MCP 进程时统一处理。
3. **仍成立的三条重的**：链提供的是篡改**可发现**不是**不可可能**——整条重写、整会话删除、
   以及本批新确认的"跨文件移动一行"都看不见（需要一份不在同一卷上的锚）；尾部是最便宜的删除方向
   （删最后一条数据行只需重写封印一行 + 一个哈希）；`tags.db` 与审计目录是同一类问题，这次只动了
   后一个。严重度：**中**（设计边界）。归属：外部锚点未排期；`tags.db` 归下一层 prune。
4. 其余不修项（可直接 `fam_open_dir` 打开审计目录、每日上限按进程记账、同毫秒顺序、
   finding 路径混分隔符、界面上没有"正在被记录"的提示）原样保留在
   `audit-log-is-append-only-and-priced.md` §4.3，严重度与归属不变。

### 4.4 驳回的两条，以及它们各自的证据

- **"封印那一行自己不被任何东西锚定，删掉最后一条数据行只需重写一行"**：现象我核对为真，
  但它是 §4.3 第 3 条的实例而不是独立缺陷——能写文件的人本来就能重算任意多行；要给"最后一条"
  加锚，唯一自洽的做法是让它依赖一个文件外的量，那是未排期的外部锚。已按更有用的说法吸收进
  `chain.rs` 的注释与 §4.3（"尾部是最便宜的删除方向"）。
- **"`fam_stats` 里的 `records` 比文件行数少一条"**：不是 off-by-one，是快照语义——stats 自己那一行
  尚未落地。`stats_says_whether_the_log_is_actually_being_written` 断言的就是"读取之刻"的值（2 =
  `session.open` + 一次 `dir.open`）。改成"加一"会让它在任何写失败时反过来撒谎。
