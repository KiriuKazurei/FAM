# 按标签搜是另一个问题，不是名字搜索的子集（增量 17c）

## 1. 描述问题

增量 16 的残留清单里第二条：**按标签搜没有答案面**。`find` 在 ABI 与 bridge 上都就绪
（`fam_find`，`bridge.cpp` 包好了），界面里调用者为 0；而它给不了一个能画出来的行——
返回的是 `[{path, tags}]`，没有 size、没有 mtime、没有 kind，硬塞进 `FamListModel`
会让每个被标过的文件印 `0 B`，增量 13 刚把"每个文件夹都印 0 B"当缺陷修掉。

同一份清单还有一条已经成型的事实：`fam_tag_vocabulary`（增量 17a）是**为一个不存在的
筛选栏**加的——包在 `FamBridge::tagVocabulary` 里，界面里调用者为 0。到了 17b 收尾时，
一个标签在这个产品里的全部命运是：被写进去、被画在它自己那行上、被它自己那行的对话框删掉。
"这个标签还在哪些文件上"没有一条路能问。

## 2. 分析问题

### 把答案面放在哪：不在 `find`，在路由

候选有三条：

| 候选 | 为什么不走 |
| --- | --- |
| 给 `find` 的结果补元数据，塞进 `FamListModel` | 结果集的形状救不回来：`FamViewRow` 要 size/mtime/kind，`find` 一概没有，逐条补齐就是又一条"每行一次查询"的绘制路径，增量 7 的规矩不答应 |
| 界面单独开一个"标签结果"表面 | 两套行画法、两套分页、两套排序——名字命中和标签命中在人的眼里是同一种东西，界面却要说两次 |
| **标签库作为一个搜索后端**，命中按 `SearchHit` 出，元数据在搜索线程补 | 命中与名字命中同形（一条 `SearchHit` 携带 key、path、kind、size、mtime），行画法、分页、取消、作用域全部是现成的那一套 |

第三条就是这次的做法。它成立的代价只有一条：`crates/fam-svc-store/src/search.rs` 里每个
候选一次 `metadata`——store 的 `files` 表只有 `(path_norm, path, updated_at)`，没有任何
元数据列，库答不出一个文件多大。这是**搜索线程上**、按请求上限封顶的文件系统读，与枚举
后端做的交换是同一种；不是绘制路径上的。

### 名字索引不许被问标签：能力位，不是 `covers()` 的又一行

`covers` 说的是"哪个文件系统子树你够得着"，标签是它根本没有的字段。一个名字索引被问
`tag:合同`，会答出**名字里有"合同"的文件**——一个形状相同的另一个问题，还会报"完整"。
所以路由问的是新的能力位 `handles_tags()`（默认 false），在 availability 与 `covers`
**之前**问，`Insisted` 也翻不过它：坚持要 Everything 并不能让 Everything 知道谁打了标签
（`router.rs` 的 `a_tag_request_is_not_asked_of_a_name_backend`、
`insisting_does_not_give_a_backend_tags_it_lacks` 两条测试钉住）。

### 写法是被 store 的语义定的

`SearchRequest::new` 把 `tag:` 前缀的词拆进 `tags`，**逐字保留、不小写化**——store 的
`tag IN (...)` 走 BINARY 列精确比较，`tag:PDF` 被折成 `tag:pdf` 会答"0 条且已覆盖"，
读起来像库是空的而不是拼错了（`a_tag_spelled_in_another_case_matches_nothing_because_tags_are_exact`）。
多个 `tag:` 是 AND（与名字词同一约定）；裸 `tag:` 是空标签，丢掉不吞下一个词。

### 范围：门的结构白拿

router 的 `Scope` 本来就对每个命中过滤（增量 10/12 那套），所以标签后端"知道全盘"这件事
不需要新守卫：无 scope 的标签搜索由**现有的门**回答——agent 的凭据必须覆盖每个配置根；
命中里落在根外的被 Scope 丢掉，`matched` 多于交给调用者的行是文档里写明的语义
（`fam-svc-mcp` 测试 `a_tag_search_answers_inside_the_roots_and_not_beyond_them`：
库里种一条根外路径的标签，答的是根内的那个文件，根外的那条不出现在结果里，`provider` 是
`tags`）。

### 筛选栏：点击 chip 就是那句话

`tag:` 前缀是打字的写法；人的手势是点。筛选栏（`TagFilterBar`）把词表摆成一行 chip，
点击即搜索——box 是查询唯一活着的地方，chip 写 box、再从 box 读回自己的勾选态
（`applyQuery`），没有第二台状态机可漂移。重建（每次搜索、每次对话框改动、启动）都带上
当前 box 文本重新推导勾选，所以"手改了 box"与"点了 chip"永远一致。

一个真递归在这里被拦下：重建要按 box 重新勾选，而 `setChecked` 会发 `toggled` →
`filterChanged` → 搜索 → 重建——追自己的尾巴。`applyQuery` 因此静默（屏蔽信号），
`toggle()`（手那条路）才有声。

### 一个潜伏缺陷顺带落地

机械对比 sheet 里的 `{{token}}` 引用与替换表，`primaryBg` 被引用、不在表里——
`QPushButton#toggleButton:checked` 的 `background` 声明一直被静默丢弃，Split 按钮选中态
从来没有底色。补上 `primaryBg`/`tagBorder`/`radiusSM` 三个映射（后两个是筛选栏要用的），
修后的主截图里 Split 选中态第一次有了浅蓝底。

## 3. 解决方式

- `crates/fam-core/src/search.rs`：`SearchRequest.tags` + `tag:` 解析 + `names_tags()`
  + `SearchProvider::handles_tags()`（默认 false）。
- `crates/fam-svc-store/src/search.rs`（新）：`TagSearch` 后端——store 出候选
  （`CANDIDATE_CEILING` 5000 封顶，锁只护 SQL、不护 `metadata`），逐条补元数据、
  按 kind 与名字词收窄；`complete` 只对"上限截断/取消/sink 停"为假，到达调用者自己的
  limit 不算不完整。
- `crates/fam-svc-index/src/router.rs`：标签请求只问 `handles_tags()` 的后端；
  `with_provider` 把产品自己的标签库放进默认路由。
- `crates/fam-ffi/src/lib.rs`：`FamCore` 的 store 变 `Arc<Mutex<_>>`，router 挂
  `TagSearch`；`fam_search_begin` 不用改——`NameRequest::new(query)` 解析 `tag:`。
- `crates/fam-svc-mcp/src/server.rs`：agent 面同一条规则，router 挂同一个后端；
  `fs_search` 的 schema 与描述写明 `tag:`。`apps/fam-cli/src/commands.rs`：`fam search`
  的默认路由同样挂上（指名 `walk-only`/`everything` 的调用者得到的是那个后端自己的拒绝）。
- `apps/fam-shell/src/tagfilterbar.{h,cpp}`（新）：词表 chip 条，滚动不过挤；
  `refresh(query)` / `applyQuery`（静默）/ `toggle`（手）/ `markSilently`（相机）。
- `apps/fam-shell/src/mainwindow.cpp`：工具栏变两行（控件 + 筛选栏，无标签时整条收起）；
  词表在启动、搜索、对话框改动三个时刻重读——按动作计次，不按拍；占位文案写明 `tag:`。
- `apps/fam-ui/src/theme.cpp/.h`：`tagFilterBar`/`tagChip` 两条 QSS 规则（选中态与行上
  chip 同一套 token：`primaryBg` 地、`accentText` 字、`tagBorder` 线），替换表补三个映射，
  对比对多一对（"筛选芯片描边 / 容器"，装饰性，免除）。
- self-check：`tagSearchForSelfCheck`（种标签 → chip 在 → `toggle` 走手的同一条信号路
  → 搜索结算且命中探针 → 再点一次 → 回文件夹 → 删标签 → chip 没了、条收起）与
  `grabTagFilterForSelfCheck`（两枚 chip 一勾一不勾的像，light/dark 各一张）。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（light，带 mutate；dark 同样九个 1）：

```
self-check tag filter on fam-self-check-probe.txt: seeded=1 chip=1 clicked=1 settled=1 hit=1 second-click=1 folder-back=1 chip-gone=1 strip-hidden=1
```

- `clicked=1` 走的是 `TagFilterBar::toggle`，即真点击会发的那个 `toggled` 信号——
  没有为测试另搭旁路；
- `settled=1 hit=1`：chip 写出的 `tag:self-check` 经 `fam_search_begin` 结算出 1 行，
  且那一行就是探针文件（日志里 `search settled at 8245ms: 1 row(s)`）；
- `second-click=1 folder-back=1`：同一枚 chip 的第二次点击清空 box、回到文件夹——
  开与关是同一个手势的两半，各自有断言；
- `chip-gone=1 strip-hidden=1`：删标签后词表里没有它了，最后一条标签被删掉时整条收起。

**核心侧**：`cargo test --workspace` 38 个测试二进制全绿（含 `fam-svc-store` 新文件
`tests/search.rs` 六条、`fam-ffi` 过线两条 `a_tag_query_answers_with_rows_that_carry_metadata`
——标签命中带着真实 size 与目录位过线，"塞进 `FamListModel` 印 `0 B`"的旧反对不再成立——
与 `a_tag_query_narrows_by_name_and_answers_empty_honestly`；`fam-svc-mcp` 的越界一条）；
`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cargo fmt --all -- --check` 干净。

**壳侧**：light（带 mutate）与 dark（带 mutate）两遍退出 0；
`icons: extracted=21 destroyed=21 cached=21 pending=0`；
`ipc: retries=0`；对比度两面都是"受门槛的 14 对里 0 对不过"（新免除对：浅 1.14 / 深 1.40）；
14 那三条落放证据原样还在（move `exists=0/1`、copy `exists=1/1`、同列
`0 transferred, 1 already there`）。`abi-smoke` 全部 `-> ok`，且它的后端行第一次打出
`backends=tags,everything,walk available=tags,walk`；`model-smoke` 六条 ok。

**两张筛选栏的像看过**（light/dark 各一）：勾选态是行上 chip 的那一套
（浅蓝地、蓝字、发丝线），未勾选态是描边加次级灰，两面与各自的窗口同底色。

### 三处自记

1. **筛选栏的第一版截图抓不到选中态**：勾选循环在 `m_tagBar->children()` 里找 chip，
   而 chip 的父是滚动区的 content widget——循环没找到任何人，静默过去，两张 chip 都是
   描边态。线的断言（`chips=2`）是绿的，只有打开那张图看了一眼才知道。修法是把
   "静默置位"收进 `TagFilterBar::markSilently`（它握着 `m_chips`），日志加 `checked=1`
   让这一步有自己的读数。增量 16 那句"只有截图暴露"第二次应验。
2. **`primaryBg` 的潜伏缺陷是机械对比抓的**，不是眼睛：枚举 sheet 里全部 `{{token}}`
   引用与替换表求差，差的只有一个。它坏的方式（声明被静默丢弃、按钮照画）意味着不修它
   也没有任何测试会红——这类缺陷的账只能记在这里。
3. **完整 bench 这次没重跑**。这次触碰的 Rust 路径是 `SearchRequest` 解析、router 的
   问询顺序、`fam-ffi` 的 store 所有权与一个新后端；`bench/baseline.json` 的十二条 case
   （store 读、dir 打开、viewport、walk、audit、卷扫描）没有一条经过它们。丢的是
   "再次确认环境没变"，与增量 16 自记 #1 同一条理由、同一条限度。

### 代价

- 标签命中的一次 `metadata` 落在搜索线程，≤ 请求上限封顶（store 的 `LIMIT` 5000 候选
  先封一次）。无单独计时；这是与枚举后端同一种交换。
- 词表读落在三个动作时刻（启动、每次搜索、对话框改动），一次动作至多一条
  `tag.vocabulary` 审计线；不落在 250ms 的拍上。
- 筛选栏 + 两个 self-check 步骤 + 主题规则，新增约 330 行（壳 208、账面测试与规则在外）。
- `FamCore` 的 store 从裸 `Mutex` 变 `Arc<Mutex<_>>`（与搜索线程共享），锁只在
  SQL 那一段被持有。

### 残留风险与局限（这次没解决什么、为什么）

1. **行上的 chip 仍不可点**。点击手势住在筛选栏里；行上那一枚只是显示。让它可点需要
   delegate 的命中测试与事件路由（`editorEvent`），是又一次独立的改动。
   严重度：低（打字与筛选栏两条路都在）。
2. **agent 打的标签不立刻进筛选栏**。行上的 chip 一秒内自愈（拍子 + 到期），但词表要等
   下一次搜索、对话框改动或重启才重读。严重度：低。
3. **`fam_find` 的旧形状还在 ABI 与 bridge 上、依旧没有调用者**。标签搜索走的是 router
   这条缝，`find` 的 `{path, tags}` 无人消费；收不收它是独立的一步（ABI 减法）。
   严重度：低。
4. **`rename_tag` 仍不在 ABI 上**（增量 16 残留 #4，原定"与批量读同一次追加"但 17a 没有
   带上它），人和 agent 改标签名仍只能删了再加。严重度：低。
5. **`files` 表的行仍永不回收**（残留 #3 原样）；**删标签仍无确认/撤销**（#5）；
   **`auto` 来源仍无法在壳里产生**（#6）；**对话框键盘路径仍未测**（#7）。
6. **含空格的标签打不进搜索框**——`tag:` 是按空白切词的，带空格的标签只能靠筛选栏的 chip
   （点击即整名）。这是设计好的分工，但值得知道：chip 是那类标签唯一的入口。
   严重度：低。
7. **clippy 在这次工作里新拦了一条文档注释**（17a 的 `vocabulary()` 文档里行首的
   `- once...` 被 markdown 当列表项、下一行成了懒延续）。修的是缩进，不是文字；
   记下它是因为 17a 交账时 clippy 是绿的——lint 本身会变老。
