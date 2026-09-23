# 审计日志：一次请求一行，行价是测出来的

- 增量：10
- 提交：`feat(audit)`（本文件之前一笔）
- 涉及：`crates/fam-svc-audit/{lib,record,actor,chain,writer,inspect}.rs`、
  `crates/fam-core/{config,prune}.rs`、`crates/fam-ffi/src/lib.rs`、
  `crates/fam-ffi/tests/audit.rs`、`crates/fam-svc-walk/{src/walk.rs,tests/walk.rs}`、
  `apps/fam-cli/{src/main.rs,src/commands.rs}`、`crates/fam-bench/src/measure.rs`、`bench/baseline.json`

## 1. 描述问题

增量 12 要把同一套能力交给 MCP 背后的 agent。一个能在文件系统里随便走的东西如果不留痕，
"它到底看了什么"就只有它自己说了算。所以这层必须排在 MCP 之前：晚一步，12 交付的第一天就是
"审计追溯不到已发生的事"的第一天。

要定的不是"加个日志"，是四件互相拉扯的事：

1. 写在哪、**谁能把它读回去**；
2. 怎么证明这份日志没被悄悄改短；
3. 一次正常浏览会写多少；
4. 这笔钱由哪个线程付。

计划给的约束是硬的，逐条照抄在这里以免日后有人说"当时没这个要求"：追加式 JSONL、一天一文件、
落在 `data_dir()/audit/`、防篡改用哈希链（每条带 `prev` = 上一条的 SHA-256，首条为 0）、
审计 crate 不把查询/读取接口递给 MCP 与 FFI（读只有 CLI 子命令）、审计目录永远不能是 workspace、
**单次追加成本与每天多少 MB 用真实浏览会话实测，不预设数字**。

## 2. 分析问题

### 2.1 哈希链决定了不能排队

最省事的写法是开一条专职线程 + 一个有界队列，把记录扔过去。但队列满的时候只能丢记录，
而"丢记录"正是链要防的那件事——一条被丢的记录让**它后面所有记录**的 `prev` 指到一个
并不存在的邻居上。要么同步写，要么先证明丢弃是安全的（证明不了）。所以选了同步写：
一次调用一次 `WriteFile`，句柄在会话开始时打开一次并留着（每行重开文件会把成本变成
一次路径解析，那才是真正贵的部分）。

### 2.2 链按什么单位串

**按文件串会自己造出假阳性。** 一天一个文件，但同一天可以有两个会话在追加（shell 开着，
用户又跑了 CLI）：两边读到的"上一条"是同一条，于是两条记录的 `prev` 相同——`verify` 看到的是
一次分叉，而分叉和篡改长得一模一样。

所以链按 **session** 串，每条链首尾自己封自己：会话开始写一行 `session.open`，
结束（`Drop`，也就是 `fam_core_free`）写一行 `session.close`，里面带
`records=N dropped=M`——**N 含封印自己**，这样 `verify` 拿一个数就能对。
检测能力因此是四种：

| 动了什么 | 靠什么发现 |
| --- | --- |
| 改了某一行 | 下一行的 `prev` 对不上（`Problem::BrokenLink`） |
| 删了中间一行 | `prev` 对不上 **且** `seq` 跳号（两条独立信号） |
| 砍掉尾巴 | 最后一行不是 `session.close`（`Problem::Unsealed`） |
| 重算过整条链但留下计数不一致 | 封印说的 N 和活下来的行数不一致（`Problem::Seal`） |

**发现不了什么也要说清**：把整个会话的行全删掉、或者把整条链重算一遍并且连封印一起改——
这时文件是一本自洽的假账。链提供的是篡改**可发现**（tamper-evident），不是篡改**不可能**
（tamper-proof）；后者需要一份不在同一块磁盘上的锚，这次没有，见 §4 残留 2。

### 2.3 SHA-256 从哪来

工作区没有任何 crypto 依赖，`sha2` 会连带拉进 digest/block-buffer/crypto-common/generic-array/cpufeatures
五个包，而这里需要的只是"对一个字节串做抗碰撞哈希"——不做鉴权、不做 HMAC、不需要常数时间比较。
于是手写，代价是**必须自己验**，而实测证明这一步不是形式：

- 凭记忆敲的 64 个轮常量里，**5 个是错的**（索引 3、18、54、55、61，例如 `0xe9b5_dba5` 敲成了
  `0xe9b5_cdba`）。第一条已知答案向量（空串）当场红。
- 反过来用定义核算：K 是前 64 个素数立方根的小数部分前 32 位，IV 是前 8 个素数平方根的小数部分
  前 32 位（Python 整数开方，缩放 96 位）——两张表逐位 `MATCH`。
- 再拿第三方实现核对真实数据：对一次真会话日志的 24 行逐行 `openssl dgst -sha256`，
  与下一条的 `prev` 比对，结果 `mismatched links: []`。

任何一条常量错都会让**所有**向量红，所以向量表本身就是常量表的检查——这也是为什么
`every_padding_boundary` 把 55/56/57/63/64/65/128 这七个长度全列进去：手写填充最容易在
"56 字节要另起一块"和"64 的整数倍也要另起一块"上错。

### 2.4 注在哪一层

外壳（`with_core` / `with_core_token` / `with_core_status` / `with_core_buffer`）而不是各个方法里。
理由只有一条：**方法可以忘记上报，外壳不会忘记记账**。panic、拒绝、回答三条出口共用一次记账，
`one_request_is_one_line_even_when_it_is_refused` 就是钉这个的。

第二个决定关系到能不能被利用：`Call` 由 body 填写，外壳**不预先转抄指针**。参数如果在外壳里读一遍、
body 里再读一遍，同一个 `char*` 就被解引用两次——中间调用方一旦释放，第二次就是能被掐时间设计的
use-after-free。代价写在注释里：调用在"path 为 null"这类失败上，行的 `target` 是空的，
但 `result` 会说清是哪个参数没读到，于是一次调用仍然恰好一行，没有信息变成"什么都没有"。

`fam_abi_version`（没有句柄可记账）与 `fam_free_string`（释放内存不是对文件的请求）不记。
前者是签名决定的，后者是体积决定的：记它等于给每个回答翻倍行数，而它想说的信息回答那行已经说了。

### 2.5 审计目录怎么才不被当成数据

`Prune` 是"谁能被报出来"的唯一关口（walk 靠它裁、router 靠它在出口拦外部索引的命中），
所以注入点在那里：`crates/fam-core/src/prune.rs:38`。

但**默认配置本身就是冲突的**，这点是查出来的不是想到的：

```
$ fam config show
search_roots = ['C:\Users\Kirig']
database_path = 'C:\Users\Kirig\AppData\Roaming\FileAutoManage\tags.db'
```

Windows 的 data_dir 在 home 底下，审计目录也就是在默认根底下。如果照计划原文
"审计目录在任何一个 root 之下就拒绝启动"字面实现，**出厂默认配置会拒绝启动**——
那是我们自己的默认值和用户无关的冲突。于是把判据改成方向敏感（`conflicting_root`，
`crates/fam-svc-audit/src/lib.rs:57`）：

- 审计目录在 root 之内 → 正常，交给 prune（测试 `a_log_inside_a_root_is_the_ordinary_case`）；
- root 在审计目录之内或就是它 → 拒绝启动并点名是哪个根
  （`a_root_underneath_the_audit_directory_conflicts`、`a_search_root_inside_the_audit_directory_refuses_to_start`），
  因为这时"排除"等于偷偷删掉用户点名的根。

### 2.6 心跳不是证据，两类权重不够用

第一版按"问题 / 回答"两类分权重，`record_reads` 管后者。实测一次真实会话发现分错了一个东西：

```
（注入前的一次跑法，ui_batch_ms=120，窗口开着滚动一轮，3.9 秒）
19 records / 4,426 bytes，其中 dir.poll 11 行 + dir.state 1 行
```

也就是说 **63% 的行是心跳**：它在问"有变化吗"，绝大多数时候答案是"没有"。
它既不是一次决定（`dir.open` 才是），也不是一次内容的读取（`dir.fetch` 才是）。
折算成体积（公式见 §4）：单列开着不动 ≈ 3.4 MB/小时，五列并排 ≈ 17 MB/小时——
一天不动就能把 64 MB 的上限吃光，而这堆字节里没有一条能回答"它看了什么"。

所以权重改成三类（`crates/fam-svc-audit/src/record.rs:33`）：

| 类 | 有哪些 op | 记不记 |
| --- | --- | --- |
| `Ask` | open / search.begin / 各 sort / filter / close / tag.* / find / stats / backends / session.* | 永远记 |
| `Read` | `dir.fetch`、`search.page` | `audit_record_reads` 决定（默认开） |
| `Poll` | `dir.state`、`dir.poll` | 永远不记（`writer.rs:154`） |

被策略挡掉的不算 `dropped`——`dropped` 只记"想写而写不出"，混进去会让每个
`record_reads=false` 的会话看起来都像丢了数据。这条写在 `record()` 的注释里，
并由 `the_poll_never_gets_a_line_even_with_record_reads_on` 和
`record_reads_off_keeps_the_asks_and_says_nothing_was_lost` 分别钉住。

## 3. 解决方式

### 3.1 记录形态

```json
{"seq":5,"ts":"2026-09-23T16:41:31.148Z","actor":"user","op":"dir.open",
 "target":"D:/DATAS/FAM-LOSS-CHECK/ICONS","result":"ok token=1",
 "session":4722754795128916130,"prev":"<64 hex>"}
```

- `target` 存 `fam_core::path_key`，和索引/标签库同一个身份——审计行必须能回答"这是哪个文件夹"，
  而两种拼写就是两个答案（`the_target_of_a_line_is_the_same_key_the_index_uses`）。
- `ts` 是 UTC 到毫秒。日期边界因此是 UTC 的，文件名 `audit-2026-09-23.jsonl` 也就是 UTC 的
  （`a_day_is_a_day_in_utc`、`a_timestamp_reads_as_a_clock` 的期望值全部对着 `date -u -d @…` 校准）。
- 所有自由文本走 `serde_json`，绝不 `format!` 拼行：`Draft` 里 target 截到 512 字符、
  `args`/`result` 截到 240，转义由序列化器负责（`a_quoted_newline_cannot_start_a_second_line`
  就是拿 `evil"\n{"op":"clean"` 当标签/参数，断言整条记录仍是一行且能原样读回）。
  **手工拼 JSON 是这份日志唯一能被伪造出新行的入口**，所以把它堵在类型上。

### 3.2 写入侧

`writer.rs`：一天一块（`rotate_to`，`writer.rs:263`）、句柄常开、**一次 `write_all` 连换行一起写**
（早期版本两次 `write_all`，等于每行两次系统调用）、无 BufWriter（std 的 `File::write` 本来就不缓冲，
再加一层只会让"崩在中间"变成"少一段还看不出来"）。写失败就丢掉句柄，下一次调用重新开——
目录被人删了也走这条路恢复，而删掉的行不会被"续上"，因为 `prev` 还在内存里，
新行会链到一条已经不存在的行上（`a_write_failure_is_counted_and_the_log_recovers_on_its_own`）。

`dropped`/`dropped_because` 不在失败时写日志（写不了才失败的），而是**由下一条落地的记录带出来**；
控制台只警告第一次（`Writer::warned`），因为一个持续失败的局面不该自己再产一条流。
同一件事通过 `fam_stats` 的 `audit` 段结构化可见：`writing`、`records`、`bytes_today`、
`dropped`、`dropped_because`、`last_error`、`record_reads`、`session`。

### 3.3 读的路径只有 CLI

`inspect.rs` 提供 `files/tail/verify/summary`，`fam-ffi` 一行都不碰。"没递出去"这件事不能靠自觉，
所以对成品二进制量了一遍——`objdump -p famcore.dll` 的导出名 21 个，与
`apps/fam-shell/include/famcore.h` 里声明的 21 个**逐项相等**，双向差集为空，
含 `audit` 字样的导出 **0** 个。

```
fam audit tail -n 20      # 不解析，坏行也能看
fam audit verify          # 逐链重算 + 逐会话核封印，有 finding 就 exit 1
fam audit stats --top 10  # 文件/记录/字节/每 op/每 actor/最热 target
```

`tail` 对"还没有日志"返回空并退 0（新装機本来就该是空的），`verify` 对同一个状态**报错退 1**：
目录不见了正是验证要发现的事。

### 3.4 一次真会话长什么样

```
$ fam audit stats            # 一次 --self-check-mutate 会话，record_reads 开
1 files, 24 records, 5835 bytes (243 bytes per record)
2 sessions, 2 sealed, 0 records reported dropped      # （两个会话同一天，各 12 行）
by op: dir.fetch 6, search.page 3, dir.open 3, dir.close 3, backends 2,
       stats 1, session.open 1, session.close 1, search.close 1,
       search.cancel 1, search.begin 1, dir.sort 1
```

## 4. 解决方式效果评估

### 4.1 正面证据

测试总数 320 → **382**，全绿（`cargo test --workspace`），+62 条的来历：fam-svc-audit 46、
`crates/fam-ffi/tests/audit.rs` 10、fam-core config 2、fam-core prune 2、fam-svc-walk 2。
`cargo clippy --workspace --all-targets` 零告警；`cargo check -p fam-svc-audit
--target aarch64-unknown-linux-musl` 通过（这一层没有任何平台代码，也不需要 SQLite）。

关键行为逐条点名：

- 一次调用恰好一行，含拒绝与 panic：`one_request_is_one_line_even_when_it_is_refused`
  （断言 `args="token=1 sort=sideways order=ascending"` 与 `result="error unknown order \"sideways\""`）、
  `a_session_brackets_what_it_did_and_verifies_afterwards`（断言整个 op 序列）。
- 链、轮转、跨文件不断链：`each_line_chains_to_the_one_before_it`、
  `a_new_day_is_a_new_file_in_the_same_chain`。
- 篡改四种各一条：`an_edited_line_breaks_the_link_after_it`、
  `a_deleted_line_shows_as_a_gap_and_a_broken_link`、`a_removed_tail_leaves_an_unsealed_session`、
  `a_seal_that_disagrees_with_the_lines_is_caught`。
  又在真日志副本上手工演了一遍（`fam audit verify` 实录，全部 exit=1）：

  ```
  # 把第 5 行的 "op":"dir.fetch" 改成 "dir.open"
  ...jsonl:6 session 4722754795128916130: link is 6fb1e8bd…, should be 9215bf1e… - this line or the one before it was rewritten
  # 删掉一行
  ...jsonl:10 …: record number is 11, should be 10 - a line is missing        （外加同会话的 BrokenLink）
  ...jsonl:23 …: the closing record counts 24 lines, 23 are here
  # 只留前 20 行
  ...jsonl:20 …: the session has no closing record: it crashed, or its tail was removed
  ```
- 拒绝启动的两条硬约束：`a_search_root_inside_the_audit_directory_refuses_to_start`、
  `an_audit_directory_that_cannot_be_written_refuses_to_start`（外加
  `an_unwritable_directory_refuses_to_open_the_log`、`an_empty_audit_directory_is_rejected_before_anything_opens`）。
- 审计目录搜不出来——同一个目录树里，形状一模一样的文件，**唯一差别是它在不在审计目录里**：

  ```
  $ fam --config config-selflook.toml search audit --limit 30
  jsonl under audit-on  （配置的审计目录）: 0
  jsonl under audit-off （只是兄弟目录）    : 1
  ```

  这一条同时证明了 §2.5 的方向敏感判据没有把"日志在根之下"误当成冲突。

### 4.2 代价（实测，以及折算）

**单次追加。** 新增 bench case `audit_append`（一次 `record()` 的全部：拼 summary、
`path_key`、序列化、SHA-256、一次 `WriteFile`；2000 轮，地板已写入 `bench/baseline.json`）：

| 构建 | min | p50 | p95 | max |
| --- | --- | --- | --- | --- |
| debug（壳现在跑的就是这版 `famcore.dll`） | **41 µs** | 42 µs | 77 µs | 471 µs |
| release | **12 µs** | 13 µs | 15 µs | — |

**一次会话。** `--self-check-mutate`（开窗、滚动 110 档、建文件再删、下钻、带 scope 搜索、
翻页、点表头、回到盘根），默认 `ui_batch_ms=120`，跑两遍：

| | 记录 | 字节 | 会话时长 | 每行 |
| --- | --- | --- | --- | --- |
| `audit_record_reads = true` | 24 | 5,835 | 8,100 / 8,157 ms | 243 B |
| `audit_record_reads = false` | 15 | 3,843 | 同量级 | 256 B |
| 差额 = 6 次 `dir.fetch` + 3 次 `search.page` | 9 | 1,992 | — | 221 B |

**每天多少 MB（折算，公式在此）。** 这次不是"实测一天"，是从实测速率外推，且分两种状态：

1. **开窗不动**：现在 = **0 行**（心跳不再记账）。改三类之前是 4.1 行/秒（11 次 poll / 2.7 秒）
   × 233 B = 955 B/s → `955 × 3600 / 1048576 = 3.28 MB/小时`（单列），五列并排 ×5 ≈ 16 MB/小时，
   16 小时 ≈ **55 MB**——正好是 64 MB 上限那一档，这就是把它请出记录队列的理由。
2. **持续主动操作**：24 行 / 8.13 s = 2.95 行/秒 × 243 B = 718 B/s
   → 2.5 MB/小时。自检脚本是"每秒都在动键盘"的上界，真人不会一直动，所以这是**上界**。
   据此，64 MB/天 ≈ `64 × 1048576 / 243 ≈ 276,000` 行 ≈ 同一速率下 26 小时。
   也就是说：上限现在是失控保护，正常用不碰它。

**CPU。** 24 行 × 41 µs ≈ 1.0 ms，占 8,157 ms 会话的 **0.012%**。墙钟 A/B 不成立：
同配置两次会话一个 8,157 一个 8,100 ms，OFF 那两跑是 8,094 / 8,822 ms——**差值小于跑的抖动**，
所以"审计数值不影响时延"这个结论只来自算术，不来自墙钟对比。（要拿墙钟说事得几十次取中位，没做。）

**复杂度。** 多一个 crate、三个配置项、一条 CLI 子命令树；`fam-ffi` 的四个外壳各多一个
`&mut Call` 参数（17 个导出的函数体各加 1–2 行）；`fam_stats` 的 JSON 多一个 `audit` 段
（壳不读它，`abi-smoke` 会打印出来，人读）。

**顺带修掉的一处旧代价。** 注入 prune 让 `open_flat_dir` 从 1032 µs 涨到 **4,082 µs（3.90x）**：
前缀判定原先**每条记录**都算一次 `path_key`，而默认配置以前是"零个前缀"，这条热路径整段被跳过。
把判定从"每条记录"挪到"每个被列举的目录"（外加 root 自己问一次，
`crates/fam-svc-walk/src/walk.rs:141`、`:378`）之后 A/B 复测：

| case | 注入前地板 | 注入+逐条判定 | 注入+逐目录判定 |
| --- | --- | --- | --- |
| `open_flat_dir` | 1,032 µs | 4,082 µs（3.90x FAIL） | **1,091 µs（1.06x ok）** |
| `walk_large_tree` | 205,068 µs | 575,941 µs | **229,004 µs（1.12x，单独跑）** |
| `volume_scan_capped` | 586,534 µs | 1,172,025 µs | **648,368 µs（1.11x，单独跑）** |

后两个 case 在**本次改动之前**就是红的（2.29x / 1.96x，D: 卷 100% 满，见
`benchmark-floors-are-machine-state.md`）；逐目录判定把它们推回到 1.1x 附近——
也就是说这一改不只补回我造成的回归，还快过了我接手时的状态。
全量 `fam-bench check` 在同机压力下仍是这两条红（1.47x / 1.28x），与单跑差 30%，
是机器状态不是代码；地板按既定原则**没有重记**。

### 4.3 残留风险与没解决的东西

1. **直接 `fam_open_dir` 打开审计目录仍然可以。** 单层列举不看 `Prune`，watch 也只挂在"有视图开着"
   的目录上，所以有人显式打开 `AppData\Roaming\FileAutoManage\audit` 就能看见自己的日志，
   并对它建一个 watch。严重度：低（要手打路径；写自己看的目录不会成环——一次追加 → 一次事件 →
   一次重读 → 没有后续追加）。归属：增量 12 决定 agent 侧是否允许，那时一并处理。
2. **整链重写与整会话删除看不见。** 见 §2.2 表格下方。要闭掉需要一份不在同一卷上的锚
   （另一台机器、或至少另一个卷的副本），这次没有，也不该由这层自己发明。严重度：中，
   属设计边界。
3. **`tags.db` 和审计目录是同一类问题，这次只动了后一个。** 默认配置下 SQLite 文件也在 home 根底下，
   会被列举/搜索，若打开还会被 watch。严重度：中低（噪声，无环）。归属：下一层 prune，
   或者干脆排除整个 `data_dir`。
4. **每日上限是每进程记账。** 打开时读一次文件大小，之后只加自己写的字节；并发会话在
   本进程打开之后写的部分不会被计入，所以一天可以超过上限。严重度：低（会话数个位数）。
5. **`dropped` 需要"下一条落地的记录"才浮现。** 如果之后再没有任何记录，计数只活在
   `fam_stats` 与那条未封上的会话里。严重度：低。
6. **审计写在做响应的线程上（今天是 GUI 线程）。** 41 µs/行的 debug 值是本地 SSD + 杀软静默下测的；
   慢盘、网络盘、被拦截的写入这三档**未实测**。真要担心，路是"改成专职线程 + 无界队列 + 关闭时 flush"，
   但那会把内存变成新的上限，届时得重新算。严重度：低，需真机长测。
7. **同毫秒内两条记录**的顺序由 `seq` 与链保证正确，但人读 `ts` 会看到相同毫秒。严重度：极低。
8. **`verify` 的 finding 路径混分隔符**（`D:/…/check\audit-…jsonl`）：配置里写正斜杠、
   `join` 补反斜杠。只是难看。严重度：极低。
9. **壳界面上没有"你的浏览正在被记录"的提示**，README 之外用户只能靠 `fam audit tail` 发现。
   严重度：中（这是个交付给人用的东西的知情问题，不是正确性问题）。归属：原型测试前补一句状态栏文案。

### 4.4 恶意行为评估（按"能不能害人 / 能不能甩锅 / 会不会变成 DoS"三问）

- **日志本身是隐私面。** 它记完整路径、搜索词、tag 内容（`args`）和回答摘要（`result` 带返回的
  JSON 前 240 字节）。谁读到这个目录，谁就复述得出你查过什么。选择：**保留**，因为这层的用途
  就是回答"它看了什么"，摘要脱敏等于把它阉掉；控制手段是位置和权限（用户自己的 data_dir，
  继承其 ACL），以及**不把读接口递给被审计的那一方**（§3.3 的 21 项导出对照是这一条的证据）。
- **能不能伪造 actor 甩锅？** 现在 `Actor` 恒为 `user`（FFI 构造时定），只有一个人可怪，
  没有可冒充的第二方。`agent:<label>` 要等增量 11 的 token 才有真实取值；风险留在那一步：
  **标签必须由签发方定死、且不能由持有 token 的一方自选**，否则"agent:browser"就是用户背锅。
  这条写进 actor.rs 的注释以便下次别做错。
- **能不能靠"让它写不出"来关审计？** 不能悄悄关：目录不可写 → `fam_core_new` 失败（拒绝启动）；
  跑起来之后才失败 → 计数浮现，`fam_stats` 的 `writing=false` + `last_error` 可见，
  封印也会缺。**代价是给运维的**：磁盘满、AppData 被占，工具就不能用，且**没有"关掉审计"的配置项**
  （`audit_dir` 只能改到别的可写卷）。这是一个故意的选择：无日志不浏览。写在这里，
  这样下一次它变成工单时，理由不用重新想一遍。
