# 看不见的备注等于没有备注：记号上了行（增量 19）

## 1. 描述问题

增量 18 残留清单的第一条，也是唯一一条"中"：备注能在对话框里读写，但**哪些文件带备注
看不出来**——要知道得一个一个点开。标签在 17b 之前就是这个处境，当时量出"一行一查
7510µs、批量 1194µs、而否决它的是'绘制不查询'那条老规矩"，解法是批量 ABI 入口 +
拍子驱动的每列缓存。备注的记号是同一套的第二遍，缺的件也一样：批量口、缝合点、
角色、绘制。

## 2. 分析问题

### 一次过线还是两次：记号只要"有没有"

行要的事实是一个布尔，不是正文。两条路：

| 路 | 代价 |
| --- | --- |
| **独立的 `fam_notes_for_paths`**，与 `fam_tags_for_paths` 同形同约 | 窗口变化的一拍多一次心跳过线（实测 ≈130µs/次，`note.fetch` 与 `tag.fetch` 同族不记账）；计数器里看得见（`notes=33`） |
| 并进 `fam_tags_for_paths` 的回答 | 省那一次过线，代价是改已发布的回答形状——bridge 的解析、abi-smoke 的 `batch-keyed-by-caller`、ffi 的键契约测试三处陪葬 |

选了独立口。心跳族（`Weight::Poll`）的过线本来就是这一拍里最便宜的事，而"一个入口答
一个问题"是这条 ABI 上已经付过学费的形状（`fam_find` 的教训是形状没人用，不是入口太多）。

### 记号画什么：点是点，chip 是 chip

一个写着什么的圆角小芯片会被读成"这个文件有个标签叫 note"——17b 为 `+N` 记号立过的
规矩（灰底灰字，因为蓝底蓝字的第三枚会读成标签）在这里同样成立。所以是 **6px 的实心圆点**
（`accentText`），贴行右缘、尺寸/类型芯片那一侧，**梯子里第一个让位**——它说的
是"自由文本存在于某处"，是整行最弱的事实。

对比度按诚实规矩入表：点是承载信息的 UI 组件（非文字 3:1 门槛，不豁免），而它脚下的底
有三种（容器、悬停、选中）——表里一对，**取三者中最不利者**，在 `contrastPairs()` 里现算
现选。实测：浅 5.50、深 5.01（选中底是暗色系里最不利的那块）。

### 一课：半成品分两次落树，构建是唯一的闸

这批件在会话中分两次落进了树里：第二次落下的圆点画在名字右侧、不占布局（会压进标签带）、
用了不存在的 `t.accent`，bridge 里同名的声明与实现各出现两份、store 里批量读也长出了第二个
副本。**构建当场拦住**（`t.accent` 编译错、abi-smoke 的 `QSet` 签名错），重复实现靠
`grep -c` 盘点出来手工收成一份。收尾状态：note 的语义都住在 `notes.rs`（`has_notes` 与
`set_note`/`note`/`clear_note` 同屋），ffi/bridge 各一份。

## 3. 解决方式

- `crates/fam-svc-store/src/notes.rs`：`has_notes(&[String]) -> HashSet<String>`（键 =
  `path_norm`，缺席即"无备注"），一条 IN 查询，空问空答；测试三种形状（有、有文件无备注、
  陌生人路径）。
- `crates/fam-svc-audit`：`Op::NoteFetch`（`note.fetch`，`Weight::Poll`——与 `tag.fetch`
  同族，永不记账）。
- `crates/fam-ffi`：`fam_notes_for_paths`（导出 28→29），回答 `{path:true}`，键按调用方拼写
  ——`fam_tags_for_paths` 的孪生契约；ffi 过线测试加两段（一键答案 + 空窗空答）。
- `apps/fam-shell`：`FamBridge::notesForPaths`（`noteReads` 计数进 `total()`）；
  `FamListModel` 的 `NoteFetcher` 缝合点（与 TagFetcher 同构：`setNoteFetcher`/
  `refreshNotes`/`invalidateNotes`，同一套跳过条件与 `kTagRefreshBeats` 到期）、
  `HasNoteRole`；`RowDelegate` 的 `kNoteDot` + `paintNoteMark` + 梯子首位让位
  （`layoutRow` 多一个输入，`tagAt` 同源同参——能点的地方与画了的地方仍按构造一致）；
  `PreviewModel` 对两个角色都答 `Qt::UserRole`（不答即不画，缝的原话）。
- 拍子与失效：列的 poll 调 `refreshNotes`、结果列表的拍同样、`Pane::invalidateTags` 与
  `ColumnChain::invalidateTags` 同时失效记号缓存（对话框改动、归并、库文件变化的
  watcher 都走这两条）。
- self-check：`notesForSelfCheck` 加 `marker: shown=1 gone=1`（同一行上，写入后拍子把它
  点亮、清除后拍子把它熄灭）；行图步骤种一条备注，帧里圆点与六枚芯片同屏。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（fresh 树，light 与 dark 两遍逐字一致）：

```
self-check notes on fam-self-check-probe.txt: ... clear=1 clear-again=0 final-empty=1 marker: shown=1 gone=1
```

`shown=1` 与 `gone=1` 之间隔着一次真实的 `clearNote`——记号随数据来去，而不是画上去就不走。

**行图**（`x.rowtags.png`，light/dark 各一）：探针行右侧、`document` 芯片旁的蓝点，选中
底上清晰可读；未选中的行不画（没有备注的行零开销）。

**核心侧**：`cargo test --workspace` 38 个二进制全绿；clippy `-D warnings` 干净；fmt 干净。

**壳侧**：light/dark 两遍退出 0；对比度"受门槛的 17 对里 0 对不过"（新对：浅 5.50 /
深 5.01）；`abi-smoke` 连跑两遍 `-> ok`（新 `marker=1`：三个路径一个带备注，答案一键、
清除后空答）；`model-smoke` ok；fam-design 构建通过（七参的 RowRoles）。

**IPC 日志**：`tags=33 notes=33 total=199`——第二次过线可见、可数，"一拍很便宜"依然是
壳这一侧只能靠数出来的话。

### 三处自记

1. **半成品分两次落树**（见第 2 节）：构建拦住了编译错，重复实现靠盘点收拢。教训与 17d
   的"筛选栏截图"同向：这个项目里，"写了两遍"不是风格问题，是"两份将来会漂移"的事实。
2. **对比度对一开始想用 `std::min` 直接选 QColor**——`QColor` 没有 `operator<`，编译器
   拦下后改成先算三个比率再选底。三行代码的差别，但"选最不利"这个决定本身进了代码而
   不是进了注释。
3. **abi-smoke 的 note 批量读顺手把 `emptyWindowIsAnAnswer` 扩到了备注侧**：清除后的
   单路径批量必须是空答，失败则降掉既有标志——新契约并入旧闸门，而不是并排再立一个。

### 代价

- 窗口变化的一拍多一次心跳过线（`notes=33`/次跑，每次 ≈130µs 量级，不记账）；
  空闲拍零成本（跳过条件与标签共用）。
- 行右缘常驻 6px + 8px 间距的预算，梯子里第一个让位。
- 约 210 行（store 20、ffi 60、壳 130）。

### 残留风险与局限

1. **圆点不可点**。点到它落在 `tagAt` 的空档，由视图当普通行点击处理（选行）——想让
   圆点打开备注对话框，需要 delegate 命中测试的第二个出口，与行 chip 的点击同机制。
   严重度：低。
2. **悬停无正文预览**。圆点只说"有"，内容仍要打开 `Note…`。tooltip 意味着绘制路径查询
   或另一次批量读——不做。严重度：低。
3. **备注正文仍不可搜索**（18 号账的原话照旧，按设计）。严重度：低（有意）。
