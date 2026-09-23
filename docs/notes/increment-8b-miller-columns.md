# 增量 8b：Miller 列与驱动器侧栏，以及计划里那个自相矛盾的一句

- 增量：8b（8b-1 列链与导航、8b-2 驱动器侧栏；提交 `7dfff69` + `feat(shell)` 两笔）
- 涉及：`apps/fam-shell/{src/columnchain.{h,cpp}, src/drivelist.{h,cpp}, src/mainwindow.{h,cpp},
  src/main.cpp, CMakeLists.txt}`

## 1. 描述问题

里程碑"只读浏览"还剩两格：**Miller 列**与**驱动器侧栏**，外加"表头排序→`fam_dir_set_sort`"
（增量 6 复查欠账 #3 的另一半：ABI 有、测试有、壳里零调用者）。

动手之前先撞到一个**计划自身的矛盾**。计划第 6 条产品决定写"浏览界面用 `QColumnView`"，
而 shell 一节写的是"用 `QColumnView`（**每列一个 `QAbstractListModel`，与一个 `dir_token` 1:1**）"。
这两句不能同时成立：

- `QColumnView` 只接受**一个**模型，各列是同一个模型在不同 `rootIndex` 上的切片，走
  `index(row, 0, parent)` / `parent(index)` / `rowCount(parent)` 的树协议——它不接受"每列一个模型"；
- 它的列是 `QListView`，**没有 `QHeaderView`**，所以计划里"排序指示来自各列的 `QHeaderView`"
  在它上面根本拿不到。

而"每列一个 token 一个模型"恰恰是这个 ABI 的自然形状：`fam_open_dir` 一次给一个目录一个 token，
`FamListModel` 已经被增量 5/7/8a 验证过就是干这个的。所以这一增量**按意图实现**（列链、每列
一个 token 一个模型、每列一个可点排序头），没有按 `QColumnView` 这个名字实现。若坚持那个名字，
代价是把窗口化取数在树协议里重做一遍，仍然没有列头。这是一处要你认账的偏离，不是我能替你决定的。

## 2. 分析问题

**列链的语义只有一条容易写错的地方：`open(path)` 到底是"替换"还是"追加"。**
第一版我写成"一路丢到最后一列等于 `path` 为止"——**子目录永远不等于它的祖先**，所以从
`icons` 进入 `icons\arch` 会把 `icons` 那一列丢掉，链深始终是 1。闸门直接把它抓出来了：

```
info: self-check descended into a folder: no, chain depth 1
info: search for zip in D:\Datas\fam-loss-check\icons\arch queued at ...
```

`depth 1` 却搜的是 `arch`——**列被替换了，不是追加了**。正确的 Miller 语义是分开的两件事：
选一个**已经在屏上**的目录 = 把链截到它（它的孩子才接着往下开）；选一个**不在屏上**的目录 =
**追加**一列。而"打开文件夹"对话框与侧栏点驱动器是第三种意思：**整条链换掉**，所以另开一个
`openRoot()`（先 `clear()` 再 `open()`）。三个动作三种语义，混成一个函数就是这次的 bug。

**排序按列各管各的。** `fam_dir_set_sort` 是一个 token 一件事，链上每列记住自己的
`(key, order)`（核心**没有** getter 问一个视图当前按什么排，所以这个状态只能留在壳里）。
一次点击换方向、换完方向再换键：`name ▲ → name ▼ → size ▲ → …`。点完立刻 `poll()` 一次——
`set_sort` 走的是 `reorder` → 置 `resync`，行要在**下一拍**才变，不立刻抽一次的话，箭头会和
行不一致四分之一秒。

**列头为什么是一个按钮而不是 `QHeaderView`。** 计划要 `QHeaderView` 的排序指示，但
`QHeaderView` 要一个能答 `headerData(section, Qt::Horizontal)` 的模型，而列表模型只答**一个纵向列**。
为了一个箭头去垫一层代理模型不值当；一个扁平按钮写 `Name ▲` 对读的人说的是同一件事。

**侧栏的枚举不能放在绘制线程上，这不是形式问题。** `GetDiskFreeSpaceExW` 打在一个
服务器已经不存在的映射盘上，会按 SMB 超时**阻塞数秒**；而壳的全部承诺就是"绘制路径不等文件系统"
（增量 5 起，v2 入口一个都不碰磁盘）。所以侧栏有自己的 worker 线程，而且**根本不问网络盘的容量**
——`GetDriveTypeW` 从重定向器缓存里答，`GetDiskFreeSpaceExW` 才是会挂的那个。容量对网络盘也是
最没意思的信息。

**图标层在列链下不用改。** 每列一个 `FamListModel`，共用同一个 `IconCache`（一个 worker、
一张扩展名表、一个缓存），重画仍只打各列自己物化的窗口。

## 3. 解决方式

- 新 `columnchain.{h,cpp}`：`Column{token, path, model, view, frame, sort, order, head}`；
  `open()` / `openRoot()` / `goBack()` / `poll()` / `selectFirstFolder()` / `sortDeepest()`；
  深度上限 6 列，超了丢最左的一列。
- 新 `drivelist.{h,cpp}`：`DriveWorker`（`GetLogicalDrives` + `GetDriveTypeW` +
  `GetDiskFreeSpaceExW`，网络盘跳容量）+ `DriveList`（`QListWidget`，`selectFirst()`）；
  析构 `wait()`——它跑不到网络上去，所以这个等待是有界的。
- `mainwindow`：模型从"一个 `m_model` 戴两顶帽子"变成**两页**（`QStackedWidget`：列链 / 搜索结果），
  列链在结果背后**继续被轮询**；`8a` 那套 `ownsModel` 分叉因此整块消失——不再需要问
  "这个模型现在属于谁"，因为各有各家。`keyPressEvent` 里 Backspace / Alt+← 上一列。
- 时间线（`--self-check-mutate`）扩成八步：建文件 → 进入子目录 → 带作用域搜索 → 清空回列链 →
  点排序 → 上一列 → 删文件 → 点侧栏驱动器。

## 4. 解决方式效果评估

**真机一次跑完的读数（126 行树 + 40 行的 `arch` + `C:\` 51 行）：**

```
folder on screen: ...\icons at 1253ms: state=0 total=126 generation=1
self-check created ...\fam-self-check-probe.txt
column ...\icons reconciled in 1 pair(s), 127 row(s), selection kept
self-check descended into a folder: yes, chain depth 2
column ...\icons\arch rows 0 -> 40, state=0 generation=1
search for zip in D:\...\icons\arch queued at 4568ms: state=1 total=0
search settled at 4681ms: 40 row(s) · enumerating for names (no index answering)
column ...\icons\arch reconciled in 2 pair(s), 40 row(s), selection kept
self-check sorted the deepest column by name descending
self-check went up to D:\...\icons, chain depth 1
self-check removed the probe: yes
column ...\icons cut 1 position(s) in 1 pair(s), 126 row(s) left
folder on screen: C:\ at 7352ms: state=0 total=51 generation=1
self-check picked drive C:/, chain depth 1
icons: extracted=29 destroyed=29 cached=29 pending=0
ipc: headers=5 pages=7 polls=45 retries=0 total=57
```

截图三张核对过：两列并排、**两列各挂各的排序状态**（第一列 `Name ▲` 仍升序，第二列 `Name ▼`
从 `zip-40` 开始降序），侧栏四行 `C:/D:/E:/K: Local · 剩余/总量`，`.zip` 行是归档 glyph。
`abi-smoke` 一字未改退出 0；Rust 侧这一整笔零改动（337 条、clippy、fmt 仍是 8a 那笔的状态）。

**一次"看着像 bug"的调查，结论是不是。** 截图里 `C:\` 的头四行是
`cache206, SETUP245, $Recycle.Bin, .GamingRoot`——第五行往后才是整齐的字母序，看着像排序坏了。
没有猜：加一段临时诊断把前五行的**码点**打出来：

```
cache206 → 20,63,61,63,68,65,32,30,36   = " cache206"
SETUP245 → 20,53,45,54,55,50,32,34,35   = " SETUP245"
```

**这两个名字真的以空格开头**（某些安装器留下的 Windows 产物），而 `0x20 < 0x24('$') < 0x2E('.')
< 字母`，所以排序是对的。默认序确实是 `SortKey::Name`（`collection.rs:44`，`155` 处排），
比较器没有 bug。诊断用完即删。**记下来是因为它的形状**：一个看起来像缺陷的排序，用一次打印就
从"要不要改比较器"变成"这台机器上有两个前导空格的文件名"。

**这次没解决、没测到的。**

1. **`QColumnView` 这个名字没实现**，实现的是它的意图。若你要求字面一致，得把窗口化取数搬进
   一个树模型，而且仍然没有列头——这一条要你来定。
2. **深度上限 6 列、超了丢最左**：没测过（要造 7 层目录一路点下去）。丢列会关掉那一列的 token，
   所以不会漏监视，但"最左消失"这个体验值不值当，等真有人在 960px 窗口里点到第 6 层再说。
3. **侧栏只测了"点第一个可达驱动器"**：不可达那一支（`ItemIsEnabled` 被摘掉、`UserRole` 留空）
   本机没有样本——这台机四个盘全可达。网络盘跳容量那条也没样本（没有映射盘）。
4. **`SizeHintRole` 仍然没有**：所有列都是 16px 小图标，`setUniformItemSizes(true)` 下一行量所有行
   成立。**一旦有人把列换成大图标视图，这一条立刻变成裁行**（增量 7 复查就记过）。
5. **双击与 Enter 没有单独实现**：导航走的是"选中即进入"（`currentChanged`），所以双击在功能上是
   多余的——但它也就此不存在，键盘用户按方向键就会翻目录。这一点是**设计取舍**，不是遗漏：
   Miller 列的语义本来就是"选中即展开下一列"。
6. `poll()` 每拍对**每一列**各一趟 `fam_dir_poll`：五列就是五趟 IPC。列数少时无所谓，真要省得让
   核心提供一个"一次抽多列"的入口——那是 ABI 改动，没做。
