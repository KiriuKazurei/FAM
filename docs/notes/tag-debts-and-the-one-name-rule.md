# 标签的账清一次：改名、合并、回收与索引（增量 17d）

## 1. 描述问题

增量 16 与 17c 的残留清单在 17c 收尾时有八条，其中五条是"功能缺一半"，三条是"没人验过"：

- **改标签名做不到**（16 的 #4）：`rename_tag` 在 store 里有、不在 ABI 上，人和 agent
  都只能删了再加——而"删了再加"对一个被几十个文件共享的名字不是改名，是数据丢失。
- **重复的名字没有归并的路**（17c 的新请求）：`PDF` 与 `pdf` 在词表里是两个标签
  （store 按 BINARY 精确比较，这是 17c 定下的语义），谁也变不成谁。
- **行上的 chip 不可点**（17c 的 #1）：标签画在行上，但那枚 chip 只是颜料。
- **agent 打的标签不进筛选栏**（#2）：词表只在启动、搜索、对话框改动三个时刻重读，
  而另一进程的 `tags_set` 一个信号也不发。
- **`files` 表的行永不回收**（16 的 #3）：给一个路径打过标签再删掉路径，行留着；
  abi-smoke 在这次工作中真的被它咬了一口（见第 2 节）。
- **删标签没有确认**（#5）；**"一名两源"的显示从未被真实写入选通过**（#6）；
  **对话框的键盘路径没测过**（#7）。
- **`fam_find` 仍在 ABI 与 bridge 上、仍无调用者**（17c 的 #3）。

## 2. 分析问题

### 改名与合并是同一件事

"把 `pdf` 改名成 `PDF`"和"把 `pdf` 合并进 `PDF`"是同一个操作看到的两个距离：目标名字
还没有任何行时它是改名，已经有了它就是归并。所以 store 只做**一个**操作：

```sql
UPDATE OR IGNORE tags SET tag = ?into WHERE tag = ?from;   -- 落在已占用的身份上的行被跳过
DELETE FROM tags WHERE tag = ?from;                        -- 被跳过的正是重复，收走
```

`(path_norm, tag, source)` 的 UNIQUE 让 `OR IGNORE` 恰好跳过重复，DELETE 收走的恰好是
它们——`moved + collapsed` 就是源名字的全部行。折叠时保留的是**目标行自己的优先级**：
一行被折叠说明目标行先在，先在的行的元数据站着，被并入的行贡献的只有名字。未知名是
错误而不是"0 行也成功"——把拼错的名字归并到一个真名字上，恰恰是"0 行受影响"最像成功的
那次。`fam_tag_rename`（单文件、可选改 source）仍然独立存在：对话框里"这个文件的这条
标签改个名"与"全库归并"是两个动作，reach 不同，走同一个 store 句法的两端。

### 索引：`source` 从来不是按名字查的

v1 的 `tags_tag_source_idx (tag, source)` 没有任何读路径在用：`source` 只在 `path_norm`
前缀之下被过滤（主键自己的前缀就覆盖了），从不跨库按它查。而按名字出去的两个读——
`tag:` 搜索与词表计数——都要拿到行上的 `path_norm`，`(tag, source)` 索引逼它们每条命中
再回表一次。v3 换成覆盖索引 `(tag, path_norm)` 并删掉旧的：搜索的 `IN` 与词表的
`GROUP BY` 都走索引、到 `files` 的 join 从它自己的主键开始。同一台机器、同一构建、同一
case 前后各量一遍（debug，取 pass 的 min）：

```
tags_search      min 13457us -> 11392us   (-15%)
tags_vocabulary  min  5160us ->  1234us   (-76%)
```

词表是筛选栏每次重建都付的那次读，4.2 倍是它自己的收益；搜索剩下的成本在 `ORDER BY
f.path` 的排序上——显示路径不在任何索引里，把它塞进索引是用一次写换一次读的另一个
买卖，这次不做。两条 case 进了 `bench/baseline.json`（`tags_search` 11377、
`tags_vocabulary` 1237），并且一条测试钉住查询计划必须走 `tags_tag_path_idx`。

### 回收是指令，不是习惯

`prune_missing` 按磁盘上的 `metadata` 对账，干跑只数、`--write` 才删，删的是 `files`
行——标签与备注跟着 `ON DELETE CASCADE` 走，**这是 schema 自己的决定**，不是这条函数
的。它被做成 `fam store prune` 命令而绝不在打开时或拍子上运行：一块只是暂时掉线的网络盘
与一块文件真没了的盘，在 `metadata` 的返回值里长得一模一样。这次它咬了人：abi-smoke 的
merge 阶段删文件不删标签，下一遍 smoke 的 batch 读吃上一遍的残留，`batch=0`——
"路径没了标签还在"从账面数字变成了闸门上的红灯（修法是 smoke 清自己的名字，但那一课
记在这里）。

### 行上的 chip：命中测试与绘制必须同源

`paint()` 的梯子（让位顺序、`+N` 折叠、名字地板）决定每枚 chip 画在哪。命中测试若另写
一遍这份几何，两份迟早漂移——所以 17d 把梯子抽成 `layoutRow()`，绘制与 `tagAt()` 各调
一遍，"能点的地方"按构造就是"画了的地方"。点击以 `editorEvent` 接住**释放**并吞掉：
这次点击说的是标签，不是选行，一个没人要的选中状态变化会让 chip 瞄不准。

### 筛选栏的新鲜度：一 stat 一次读

跨进程没有信号，SQLite 的 WAL 又让主库文件的 mtime 落后于提交——所以拍子上 stat 的是
**两个文件**（库与 `-wal`），mtime+size 有变才买一次词表读，并把两栏的行缓存标脏。一次
外部的标签写只花一 stat/拍 + 一次读，不写不花。

### 确认与键盘：住在主题里

删除确认做成按钮的两段式（第一下亮出"Really remove"并写明会删什么，第二下才删，换行
即解除武装），而不是一个 `QMessageBox`——那会是又一张没吃主题的表面，16 的第一张对话框
截图教过的。键盘路径（Return=add、Esc=关）只能靠发真的键事件证明。

## 3. 解决方式

- `crates/fam-svc-store`：schema v3（覆盖索引替换，`user_version` 阶梯）、
  `merge_tags(from, into) -> MergeOutcome{moved, collapsed, files}`、
  `prune_missing(apply) -> PruneOutcome{files, tags, notes}`、错误变体 `TagNotFound`；
  六条新测试（折叠/未知/校验、干跑/落盘、v2→v3 迁移、plan 走索引）。
- `crates/fam-bench`：`tags_search` / `tags_vocabulary` 两条 case（2000 文件的库，
  `errors` 当自检），地板已录。
- `crates/fam-svc-audit`：`Op::TagMerge` / `Op::TagRename`（Ask 族）与 `Op::McpTagsMerge`。
- `crates/fam-svc-auth`：`Verb::TagsMerge`——不指路径的写，`names_a_path()` 与 `whoami`
  同侧；白名单矩阵里与 `tags_set` 同座（Edit/Admin），Browse 被矩阵测试挡住。门补了
  `admit_verb`：无路径动词问不到 `admit` 的后三问，tier 这一问必须单独有人问。
- `crates/fam-ffi`：`fam_tag_merge`、`fam_tag_rename` 上 ABI（`#[no_mangle]` 24→25）；
  **`fam_find` 收掉**（导出、`Op::TagFind`、`borrow.find`、`FileTagsOut`、bridge 包装全删
  ——ABI 与 bridge 上无人调用的那半，16 号账记的旧形状）。
- `crates/fam-svc-mcp`：`tags_merge` 第 15 个工具（快照测试有意更新为 15），门测试钉住
  "Browse 拒、Edit 过、答案只有计数、未知名拒"。
- `apps/fam-cli`：`fam tag merge` 与 `fam store prune`（干跑默认，`--write` 落盘），
  两条 CLI 测试。
- 壳：`RowDelegate::layoutRow`/`tagAt`/`editorEvent` + `tagClicked` 信号，经
  `FileView::relayTagClick` → `ColumnChain`/`Pane` → `MainWindow::searchTagActivated`
  （写 box、跑同一条 `runSearch`）；`TagDialog` 增 `renameTag` 操作与 Rename 按钮、
  删除按钮两段式；筛选栏 chip 右键 → 归并菜单（`QMenu#tagChipMenu` 上了主题，对比度
  两对新对入表，受门槛的 14→16 对全过）→ `mergeRequested` → `MainWindow::mergeTags`；
  拍子上 stat 库与 WAL，变了才重读词表并标脏行缓存；搜索框占位文案早一步写明了 `tag:`。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（fresh 树，light 与 dark 两遍逐字一致，全部为 1）：

```
self-check tag keys on fam-self-check-probe.txt: return-refused=1 ("a tag needs some text") return-add=1 listed=[keyed] esc-closed=1 confirm: armed=1 after-first=[keyed] after-second=[]
self-check tag dual-source on fam-self-check-probe.txt: manual=1 auto=1 names=[dual] display=[dual  (manual, auto)] rename=1 renamed=[dual-renamed] rename-back=1 back=[dual] remove-both=1 final=[]
self-check tag filter on fam-self-check-probe.txt: ... strip-hidden=1 merge: before=1 gone=1
self-check row chip on fam-self-check-probe.txt: seeded=1 aimed=1 clicked=1 query=[tag:click-me] settled=1 hit=1
```

- `confirm: armed=1 after-first=[keyed] after-second=[]`——第一下点了**没删**（名单还在）
  且说明文字写着"press again"，第二下才删。只看最终状态不算证据，第一下的名单才是；
- `display=[dual  (manual, auto)]`——两个真实的写（manual 一次、auto 一次）读回成一行，
  #6 的覆盖缺口关上；
- `query=[tag:click-me] … hit=1`——行上 chip 的按下与释放是发给 viewport 的真鼠标事件，
  落点由 delegate 自己的命中测试给出，之后全是生产路径；
- `merge: before=1 gone=1`——两枚 chip 进、一枚出，词表与行缓存同拍跟进；
- `return-refused=1 ("a tag needs some text")`——Return 从焦点所在的任意位置被对话框路由
  到 Add（`autoDefault` 按钮），空输入的拒绝句是键真的到了槽的证据；Esc 关掉对话框。

**核心侧**：`cargo test --workspace` 38 个二进制全绿；clippy `-D warnings` 干净；fmt 干净。
MCP 侧两条新门测试（`a_merge_is_a_write_the_read_only_tier_cannot_ask_for`、工具快照 15）；
auth 侧矩阵测试把 `TagsMerge` 钉在 Edit/Admin；store 侧六条新测试；CLI 两条新测试。

**壳侧**：light/dark 两遍退出 0，`icons 21/21/21 pending=0`、`ipc retries=0`，对比度两面
"受门槛的 16 对里 0 对不过"（菜单两对新对：浅 15.39/12.51、深同表），14 那三条落放证据
原样还在。`abi-smoke` 连跑两遍 `-> ok`（第二遍证明自清），`merge=1` 进了导出闸门行；
`model-smoke` 六条 ok。

**索引的数字**（见第 2 节）进 `bench/baseline.json`：`tags_search` 11377µs、
`tags_vocabulary` 1237µs（debug min），两条 case 与既有 12 条同库记录。

### 三处自记

1. **abi-smoke 被残留标签咬了一口，红了一轮才绿**：merge 阶段删文件不清标签，上一遍的
   `fam-abi-*` 名字让下一遍的 batch 读断言红掉。修法是 smoke 清自己的名字——而那笔账
   本身（路径没了标签还在）就是 #5，`fam store prune` 现在收它。这条记下是因为它红得
   有意义：它就是那笔债第一次在闸门上显形。
2. **行 chip 步骤第一版撞上了 6100ms 的 drive-pick 定时器**：结算窗口 2 秒，drive-pick
   在 60ms 处替换了面板，搜索从未 settle。日志里 `search queued` 后面没有 `settled` 是
   识别它的地方。修法是把 mutate 时间线尾部整体后移 600ms——新步骤要给既有步骤让路，
   而不是让闸门互相踩。
3. **两条新 bench case 的地板是在索引改造之前量的**（`tags_search` 13457、
   `tags_vocabulary` 5160，debug），改造之后按最终 schema 重录。顺序反了一次（先写了
   结论再量）被当场收回重量； SCHEMA_V3 的文档注释里因此先留了空，数字后补。

### 代价

- 每拍一次库文件 stat（GUI 线程，无 IPC、无审计线）；一次外部标签写买一次词表读。
- `(tag, path_norm)` 索引的写入放大与被删的 `(tag, source)` 相当（同列数，换了一列）。
- 壳新增约 420 行（命中测试、菜单、监视、四步无头断言），store 约 150 行。
- `merge_tags` 是一个事务里三条语句；2 千行的库上未单独计时（词表/搜索的地板已覆盖
  同表的读路径）。

### 残留风险与局限（这次没解决什么、为什么）

1. **筛选栏归并菜单的"菜单→信号"半段没有无头断言**——`QMenu::exec` 要一只真鼠标。
   断言从信号开始（`merge: before=1 gone=1` 走的是槽），菜单本身的接线是一次 `emit`。
   严重度：低。
2. **撤销仍然不存在**。两段式确认把"点错"挡在发送之前，但发出去之后没有回手——
   归并可以用第二次归并原路变回去（`into → from`），单文件的删除也可以重新加回来，
   一台通用撤销栈服务的是这两个手动可逆的动作，仍然不值得。严重度：低。
3. **`auto` 来源仍然没有生产者**：验证覆盖的是"两条真实写读回一行"，不是"索引器写标签"。
   什么时候有索引器，`auto` 的语义（谁批准它写、写什么）再一起设计。严重度：低。
4. **prune 对掉线的网络盘不设防**——靠"必须人来跑 `fam store prune`"这条纪律挡，
   命令输出在落盘前把数报全。严重度：低（有意的取舍，已写进命令文档）。
5. **含空格的标签仍打不进搜索框**（`tag:` 按空白切词），chip 与行 chip 的点击是整名入口。
   严重度：低。
6. **搜索的 `ORDER BY f.path` 排序**是 `tags_search` 剩余成本的主体（11.4ms/500 行，
   2000 文件库）；把显示路径放进索引是用写换读的另一笔买卖，这次不做。严重度：低
   （真正的主导成本是 TagSearch 每命中一次 `metadata`，那是与枚举同种的机器相关代价）。
