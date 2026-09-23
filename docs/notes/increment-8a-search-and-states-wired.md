# 增量 8a：把已经存在的能力接上，并让五种状态说各自的话

- 增量：8a（接线与标签；换结构的 8b 另做）
- 提交：`feat(shell)` + 本文件
- 涉及：`apps/fam-shell/{src/bridge.{h,cpp}, src/mainwindow.{h,cpp}, src/main.cpp}`
- 清掉的复查欠账：#2（FAILED/EMPTY 无标签）、#4（`available` 被丢弃）、#8（壳侧 IPC 计数没测）；
  #3 只清了一半，理由写在最后

## 1. 描述问题

三件各自不大、但都属于"两边都写好了没接上"的事：

1. **五种目录状态在界面上只有两种看得见。** 壳只对 `FAM_STALE` 特判了一个 `· stale`，
   `FAM_FAILED` 与 `FAM_EMPTY` 什么也不说。于是一个读不出来的目录（ACL 拒绝、已删除、死掉的 UNC）
   长得和"这个文件夹是空的、而且我在跟着更新"**一模一样**。
2. **`fam_backends_json` 的 `backends` / `available` 被壳丢掉**（只读了 `kinds`），所以界面永远说不出
   "索引没在跑，现在每次名称搜索都在遍历"。这台机器上没装 Everything，这句话**每次都是真的**，
   而用户永远看不到。
3. **"一次状态轮询不贵"这个主张只在 Rust 侧成立。** 增量 6 复查修过的那条（`count = 0` 的轮询被误标
   `FLAG_TRUNCATED`，于是壳每次轮询白跑两趟 IPC）补了 Rust 测试，但**壳侧从没数过自己花了几趟**——
   修的是症状的因，没修"我们看不见症状"这件事。

还有一件计划里明写、这半增量只做了一半的：**发起搜索会关掉当前目录**（`runSearch` 里
`closeDir` + `m_dirToken = 0`），而 `beginSearch` 的 `within` 参数一直传空。

## 2. 分析问题

**五种状态各是什么主张。** `READY`+0 行 = "这个文件夹是空的"；`FAM_EMPTY` = "没人读过它，也没人在读"
（`wire.rs` 的原文是 nothing cached and nothing in flight）；`FAILED` = "读不出来"；`LOADING` = "在读"；
`STALE` = "行还是那些行，但已经不保证对得上磁盘"。把 `EMPTY` 印成 "empty" 是**说谎**——它说的是
"没读过"，不是"没东西"。所以标签必须一句一句对：

| 状态 | 界面句子 |
| --- | --- |
| `READY`（有行） | 不加词（一切正常不值得占字） |
| `READY`（0 行） | `· empty` |
| `LOADING` | `· reading` |
| `STALE` | `· may be out of date` |
| `FAILED` | `· cannot be read (permissions, deleted, or unreachable)` |
| `EMPTY` | `· not read yet` |

`FAILED` 那句**给不出原因**：v2 的头里没有任何错误文本，核心的退避只记在槽位里。要原因得给 ABI 加
字段，那是另一次的事（记在"没解决"）。

**`available` 不能记一次用到底。** 索引后端的可用性由核心自己的线程在后台探测（1s→30s 退避），
**会在窗口开着的时候变**。所以这句话必须在搜索落定时重读 `fam_backends_json`，而不是启动时读一次
存起来——后者会在 Everything 起来之后继续说"我在遍历"。

**一个模型同一时刻只能显示一个 token，但两个 token 都得轮询。** 搜索期间继续开着目录，就要求
`pollChanges` 一拍里服务两边：目录的 `dir_poll` **必须照抽**——一个没人抽的视图会把剪裁位置
一直堆到关掉为止（`DirView::cuts` 是无界的 `Vec`）；但模型不能被告知去剪它没在显示的行。
于是 `pollFolder(bool ownsModel)`：抽干、采纳头、只在拥有模型时动模型。

**排序/过滤这半没做，是有意的。** `fam_dir_set_sort` 的入口是**列头的排序指示**，而 `QListView`
没有列头——为它加一个工具栏下拉，8b 换 `QColumnView` 时第一件事就是把它删掉。所以 #3 只清了
"搜索关掉目录"这一半，排序那一半跟着 8b 的列头一起做（ABI 侧一直有测试守着：
`v2.rs:591,610,615,618`）。

**我自己这次改出来的一个回归。** 把搜索改成"传 `within = 当前目录`"之后，**全局搜索就做不到了**：
目录一旦打开，`m_dirToken` 再没有归零的路径（只有析构会关），于是每次搜索都被悄悄收窄。
改之前是"只能全局"，改之后是"只能局部"——两个都不是答案。加了一个明确的作用域下拉
（`this folder` / `every root`），默认"这个文件夹"，没目录可作用域时整个禁用。
**猜测式收窄比不收窄更坏**：一次"什么都没找到"和"找的地方不对"在屏幕上长得一样。

## 3. 解决方式

- `bridge.h/cpp`：`coreKinds` → **`coreBackends(kinds, backends, available, error)`**（三个都是
  出参且**先清空再填**，因为这个调用值得重复）；新增 `FamIpcCount{headers,pages,polls,retries}`
  与 `ipcCount()`，在 `dirStatus`、`dirCuts`、`readRows`（`count == 0` 记 header，重试记 retry）
  三处计数。
- `mainwindow.cpp`：`folderStatus(head, extra)` 一处措辞，三个原先各自拼字符串的地方全部改走它；
  `searchBackendNote()` 在搜索落定时重读可用性；`runSearch` 不再 `closeDir`，作用域由
  `m_scope` 决定；`pollChanges` 拆成 `pollFolder(ownsModel)` + `pollSearch()`，一拍服务两边；
  清空搜索框 = `showFolder()` 回到目录（不重读盘，视图还开着）；`bindFolderView` /
  `bindResultsView` 把"绑一个 token"这件事各自收口。
- `main.cpp`：自检时间线扩成**一次跑完整条浏览面**——建文件 → 带目录搜索 → 清空回目录 → 删文件，
  并在末尾打 `ipc:` 一行。
- 顺带：`FamListModel::unbind()` 终于有了调用者（全局搜索清空后没有目录可回，就是它）。

## 4. 解决方式效果评估

**真机时间线（`--self-check-mutate`，126 行树）：**

```
folder on screen: ...\icons at 1136ms: state=1 total=0 generation=0
rows 0 -> 126 with 0 cut(s) at 1145ms, state=0 generation=1
self-check created ...\fam-self-check-probe.txt
re-read reconciled in 1 pair(s), 127 row(s) left, selection kept
search for doc-7 in D:\Datas\fam-loss-check\icons queued at 3599ms: state=1 total=0
search settled at 3613ms: 1 row(s) · enumerating for names (no index answering)
folder on screen: ...\icons at 4605ms: state=0 total=127 generation=3
self-check removed the probe: yes
rows 127 -> 126 with 1 cut(s) at 5618ms, state=0 generation=4
cut 1 position(s) in 1 signal pair(s) in 0ms, 126 row(s) left
icons: extracted=10 destroyed=10 cached=10 pending=0
ipc: headers=6 pages=5 polls=25 retries=0 total=36
```

- **`retries=0`** 就是欠账 #8 要的读数：约 5.5s、250ms 一拍 ≈ 22 拍，`polls=25` 对得上，
  **没有一次轮询花两趟**。这条以前只能靠读代码相信。
- **作用域两个方向都跑了**：带目录时 `in D:\...\icons` → 1 行；不带目录（`--query`，没开目录）时
  `in every configured root` → **0 行**，因为这份配置的根是 `tree/`，`doc-7` 不在里面。
  两个结果都对，而且正是"悄悄收窄"与"如实说明范围"的差别。
- 回到目录是 `generation=3`（**没有重读盘**），删探针的剪裁落在回到屏幕的模型上（1 cut / 1 pair）。
- `ipc: headers=2 pages=0 polls=0 retries=0 total=2`（纯搜索那次）——没开目录就不该有轮询。
- `abi-smoke` 一字未改退出 0；Rust 侧这一笔零改动（C++ only），337 条仍绿。

**日志抓到的一处我自己引入的缺陷。** 第一版时间线里出现：

```
folder changed behind the results: 0 -> 127 row(s)
folder changed behind the results: 1 -> 127 row(s)
```

`0 ->` 和 `1 ->` 都不可能是目录的真实变化——因为 `m_head` 是**一个变量服务两个 token**：
`pollSearch` 与 `pollFolder` 互相覆盖它，于是目录的"上一次"其实是结果的头。修法是拆成
`m_folderHead` / `m_resultsHead`（`mainwindow.h`），修完那两行就不再出现（因为期间目录确实没变）。
**这是"把数字打出来"这件事本身的收益**：这类别名 bug 在截图里完全看不出来。

**这次没解决、以及验证不到的。**

1. **`FAILED` 给不出原因**：v2 的头里没有错误文本字段。要说"因为 ACL 拒绝"得给 ABI 加东西。
2. **排序与目录内过滤仍然没有调用者**（欠账 #3 的一半）：等 8b 的列头。
3. **`!ownsModel` 的"目录在结果背后变了"那行日志这次没被触发**（期间目录确实没变），
   只有修别名前那两次**错误**的触发证明代码路径是通的——它需要一次"搜索期间动目录"的时间线，
   归 8b 的测试台。
4. **`available` 的"变得可用"方向没测**：这台机器装不上 Everything，只测得到"一直不可用"这一支。
5. **作用域下拉是新增界面件**，8b 换布局时它会挪位置（逻辑保留）。
6. `polls=25` 里含搜索期间对目录的轮询——**目录不在屏幕上时也在被抽干**，这是有意的（不抽会堆），
   代价是每次搜索期间多 4 趟 IPC。真要省，得让核心侧的剪裁队列有界并允许丢弃后重读，那是核心改动。
