# 增量 8a/8b 复查：一条严重缺陷、它的 A/B、以及四条我自己写的过度断言

- 增量：8a/8b 复查（不改计划，只把刚落的部分按缺陷重读一遍）
- 提交：`fix(shell)`（本文件之前一笔）
- 涉及：`apps/fam-shell/{src/columnchain.{h,cpp}, src/drivelist.{h,cpp}, src/mainwindow.{h,cpp},
  src/main.cpp, src/famlistmodel.h}`

## 1. 描述问题

做法照增量 6/7 那两次：自己逐行读新代码，同时派一个只读 agent 独立审同一批文件，问题按
**对象生命周期 / diff 算术 / 线程 / 接线 / 析构顺序** 五类问。纪律不变：**agent 说的每一条都要
读到那一行才算数**。

结果：**5 处确认并当场修**（其中一处是崩溃级），**3 类 agent 报"none"、我读完同意**，
**7 处确认不修并标严重度**。另外这次有一条特别的东西：**修完之前先证明了修前会坏**。

## 2. 分析问题

### 2.1 严重：lambda 捕获的是"位置"，而位置会变

`columnchain.cpp:83` 在 `addColumn` 里捕获 `const int index = m_columns.size();`，
两个 lambda（`:84` 列头点击、`:104` 选择变化）用它回头找列。而 `:151` 的深度上限是
`dropColumn(m_columns.takeFirst())`——**从最前面丢一列，后面每一列的位置都变了，捕获的索引不会
跟着变**。第一次丢列之后：向量里是 0..4，lambda 里还是 1..5。

三件后果，agent 全说对了：

1. 最深那一列的列头点击执行 `m_columns.at(5)`，向量大小 5 → **越界**。debug 下 `Q_ASSERT` 直接
   `qFatal` 终止进程；release 下是解引用野指针。
2. 其余每一列的列头**给它右边那列排序**（索引差一，仍在范围内，所以不报错），
   而且 `applySortTo` 找的是**错那列的 frame** 里的按钮 → 点下去箭头永远不动。
3. `onCurrentChanged(index)` 的守卫 `index >= m_columns.size()` 对最深那列成立 →
   **选中子目录不再开新列**，静默失效。

我原来的闸门看不见它：时间线只下潜一次（深度 2），而 `sortDeepest()` 用的是
`m_columns.last()` 的引用、不走捕获索引。**一条只在"下潜超过 5 层"时才触发的崩溃，
被一个只下潜一层的测试盖住了。**

### 2.2 修前的证据：同一条时间线，A/B

修完之后我没有直接收工，而是把两处 lambda 临时改回"按捕获位置查列"（其余全部保持新代码），
跑同一条 `--self-check-deep 8`：

```
（修前）exit=127
  self-check descended: depth 1 -> 2
  self-check descended: depth 2 -> 3
  self-check descended: depth 3 -> 4
  self-check descended: depth 4 -> 5
  self-check descended: depth 5 -> 5
  self-check descended: depth 5 -> 5      ← 三次，且再没有新的 column 到达
  （没有 clicked the deepest column's header，没有 icons:，没有 self-check wrote）
```

进程**在点列头那一步死了**（GUI 子系统的 `qFatal` 不写控制台，所以只有退出码 127 与断掉的日志），
而且下潜**停止开新列**。同一二进制、只把查找换回位置，改回按 token：

```
（修后）exit=0
  self-check descended: depth 1 -> 2 … 4 -> 5, 5 -> 5 ×4
  self-check clicked the deepest column's header: yes, chain depth 5
  icons: extracted=9 destroyed=9 cached=9 pending=0
  ipc: headers=9 pages=8 polls=79 retries=0 total=96
```

为了让这条时间线存在，加了 `--self-check-deep <n>`（下潜 n 次然后**真的点**列头按钮，
走 `button->click()` 而不是直接调 `sortDeepest()`——只有前者会经过捕获索引的那条 lambda）。

### 2.3 其余四处

3. **`~MainWindow` 解引用空 `m_chain`。** 构造函数在 `!m_bridge->healthy()` 时早退（核心句柄没建
   起来），此时 `m_chain` 是 `nullptr`；而析构的守卫是 `if (m_bridge == nullptr) return;`——
   `m_bridge` 是 `make_unique` 出来的，**永远非空**，所以守卫形同虚设，退出时必空指针解引用。
   同一个错代理还出现在另外 5 个槽里（全是死代码）。改法：加 `bool m_healthy`，在 UI 建完那一刻
   置真，所有守卫改问它。（agent 顺带确认了我对**析构顺序**的判断是对的：成员先于
   `QWidget` 基类删子对象死掉，所以显式 `m_chain->clear()` 是必需的，token 不会漏。）
4. **`~DriveList` 会阻塞 GUI 线程，而且我的注释把这件事说反了。** 注释写"有界，因为不问网络盘
   容量"，但 `GetDriveTypeW` 在**问容量之前**就跑过了，而它对死掉的映射盘同样要走重定向器。
   改不动本质（不能强杀 QThread），就把暴露面缩小：`run()` 每块盘检查一次
   `isInterruptionRequested()`，析构先 `requestInterruption()` 再 `wait()`——最坏代价从"整轮枚举"
   降到"一次调用"；**注释改成实话**：这台机器四个盘全可达，这个会挂的情形没有样本，没测过。
5. **`setLayout` 调了两次。** `new QVBoxLayout(this)` 已经把它装上，再 `setLayout(layout)` 会往
   自检日志里打一条 `QWidget::setLayout: ... already has a layout`。两处（不健康分支与主分支）。
   无害，但闸门日志里的噪声就是将来漏看真问题的地方。
6. **写了没人读的东西。** `FamListModel::setViewport` 全仓库零调用（`m_viewport` 恒为 128）、
   `m_resultsHead` 只写不读、`m_backends`/`m_available` 只写不读。前两个删掉；
   后两个改成**在启动时打一行日志**——`search backends: everything, walk; answering now: walk`
   这一行本身就是欠账 #4 的正证据（这台机器上 Everything 作为后端存在但不在答话），
   比留两个成员变量强。另外 `goUpForSelfCheck` 漏了 `refreshScopeAvailability()`，补上。

### 2.4 agent 报"none"、我读完同意的三类

- **`reconcile()` 的算术**：每一对 `begin/end` 前后 `rowCount()` 该是多少，逐条对着
  `first + before.size() > oldTotal` 与 `first+prefix+suffix > tailAfterGone+outside` 两个守卫核过，
  成立；所有提前返回都在第一个 `begin*` 之前，没有"发出去收不回来"的信号。
- **线程**：`DriveWorker::ready` 与 `IconWorker::resolved` 都是队列连接（发射方在 `run()` 里，
  接收方在 GUI 线程），`run()` 不碰任何 widget，元类型在 `start()` 之前注册。
- **接线**：`deepestHead` / `selectedPath` / `clear` / `goBack` / `showResults` 都可达。
  另外 agent 自己驳回了一条它本来要报的：`dropColumn` 先关 token、模型活到 `deleteLater`，
  中间若有绘制会拿已关的 token 去 `dirFetch`——核心回 `unknown view token`
  （`browse.rs:224`），`fetchAt` 画空，**不损坏数据**（token 单调不复用）。同意。

## 3. 解决方式

| # | 落点 | 做法 |
| --- | --- | --- |
| 1 | `columnchain.{h,cpp}` | 列一律**按 token 找**（`find(token)` / `indexOf(column)`），lambda 捕获 `token` 而不是位置；`onCurrentChanged(quint64)` 找不到列就返回 |
| 2 | `main.cpp`、`columnchain` | 新增 `--self-check-deep <n>` 与 `clickDeepestHeader()`，走真按钮；`descendForSelfCheck` 改打"深度 a -> b"而不是 yes/no（撞上限时不增长是设计，yes/no 分不出它与"处理程序坏了"） |
| 3 | `mainwindow.{h,cpp}` | `bool m_healthy` 取代 6 处 `m_bridge == nullptr`；析构先问它 |
| 4 | `drivelist.{h,cpp}` | 每块盘查 `isInterruptionRequested()`；析构 `requestInterruption()+wait()`；注释改口 |
| 5 | `mainwindow.cpp` | 删两处多余的 `setLayout` |
| 6 | `famlistmodel.h`、`mainwindow.{h,cpp}` | 删 `setViewport`、`m_resultsHead`、`m_backends`/`m_available`；启动打一行后端可用性；`goUpForSelfCheck` 补 `refreshScopeAvailability()` |

## 4. 解决方式效果评估

**正面证据。**

- **A/B 见上**：同一条时间线，修前 exit=127 且下潜停止开新列，修后 exit=0 且
  `clicked the deepest column's header: yes, chain depth 5`。
- 两条真机闸门都绿：`--self-check-deep 8`（`icons 9/9/9 pending=0`、`ipc retries=0`）；
  `--self-check-mutate`（新建 `reconciled in 1 pair(s), selection kept`、
  降序重排 `reconciled in 2 pair(s), selection kept`、`went up → depth 1`、
  浅列删除 `cut 1 in 1 pair`、点侧栏 `C:/ → 51 row(s)`、`icons 29/29/29 pending=0`、
  `ipc retries=0`）。
- 顺带第一次跑到了 `reconcile` 的**退路口子**：`reconciled in 0 pair(s), 0 row(s), window rebuilt`
  ——8a 的留档写过"三条退路只有正常路径被测到"，这次是被撞到的（新列在窗口还没物化时就收到
  resync）。它是**日志里出现的**，不是我推出来的。
- Rust 侧这一笔零改动：337 条绿、clippy 零告警、fmt 干净（与 8a-1 那笔同状态）。

**确认存在、这次不修的（按严重度排）。**

1. **`reconcile` 里有一个未证明可达的次序隐患**（agent 提的）：`outside > 0` 在模型末尾插入后，
   视图可能立刻去要那些行，`fetchAt` 会在 `begin/end` 之间悄悄改 `m_total`，随后我又把 `m_total`
   覆成头的值。今天没有样本能触发（窗口通常覆盖到末尾时才走这支）。要真出事，症状是行数与
   内容短暂不一致。归属：给壳侧模型做测试台的时候一并处理（8b 之后）。
2. **列的 frame/model 活得比 `~FamBridge` 晚**，中间 `bridge` 裸指针是悬的。今天无害，
   因为析构期间不跑事件循环。**这是一条约束，不是缺陷**：以后谁要在析构路径上触发绘制或事件，
   就会踩到。
3. **侧栏退出仍可能被一次 `GetDriveTypeW` 卡住**（一台有死映射盘的机器上）。本机无样本，
   没测过；已经把它从"整轮"缩到"一次调用"。
4. **深度上限丢最左列**这个体验没有验证过（要人真在 960px 窗口里点到第 6 层）。
   token 会关掉，不漏监视。
5. **侧栏"不可达"那一支没有样本**：这台机四个盘全可达，`ItemIsEnabled` 被摘掉、`UserRole` 留空
   那条路径没跑过。网络盘跳容量同样没样本（没有映射盘）。
6. **`showFolder` 在链空时留着结果页**（已 `unbind`，是空的）。措辞是 "type a name to search"，
   看着不算错，但没有"没有目录打开"的明确状态。归 8b 之后的 UI 打磨。
7. **`SizeHintRole` 仍然没有**：所有列都是 16px。谁把列换成大图标视图，`setUniformItemSizes(true)`
   下立刻裁行（增量 7 复查就记过这条）。
