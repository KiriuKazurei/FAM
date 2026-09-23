# 增量 7：壳的图标层——每行一次哈希，而不是每行一次磁盘

- 增量：7（shell 图标，纯 C++，Rust 一行未碰）
- 提交：`feat(shell)`（本文件之后一笔）
- 涉及：`apps/fam-shell/{CMakeLists.txt, src/iconcache.{h,cpp}, src/famlistmodel.{h,cpp},
  src/bridge.{h,cpp}, src/mainwindow.{h,cpp}, src/main.cpp}`

## 1. 描述问题

壳的行以前**没有图标**：`FamListModel::data()` 只答 DisplayRole/ToolTip 与几个自定义角色，
`DecorationRole` 根本不存在（主计划里记的是"现在 `famlistmodel.cpp:30-58` 完全没有
`DecorationRole`，所以没有旧行为要保，只有坑要避"）。

坑是明写的：朴素做法——每行拿真实路径调一次 `SHGetFileInfoW`——是 Qt/Win32 文件浏览器卡死的
经典成因，在网络命名空间上还会无限期阻塞；而首次 `SHGetImageList` 会构建系统图像列表，在注册了
一堆 shell handler 的机器上是 100ms 到秒级的停顿，绝不能落在绘制线程上。所以这一增量的问题是：
**给 78k 行的视图每行一个图标，而每行的代价是一次哈希查找，磁盘与 COM 都只能在专职线程上碰。**

## 2. 分析问题

分两层，分界的判据是"这个图标是不是它扩展名的函数"：

- **第一层（每行）**：键 = 小写扩展名 + 原始 `dwFileAttributes`。解析走
  `SHGetFileInfoW(探针串, attrs, …, SHGFI_SYSICONINDEX | SHGFI_USEFILEATTRIBUTES)`——
  置了 `SHGFI_USEFILEATTRIBUTES` 之后 shell 只按"名字的形状 + 属性"回答，**根本不打开路径字符串**，
  所以这一层没有 I/O。索引再交给 `IImageList::GetIcon` 从系统图像列表取 `HICON`。
  探针串：有扩展名就用 `.txt` 这样的后缀，没有就用无点的 `File`，目录用无点的 `Directory`，
  免得扩展名解析器把目录名里的点读错。
- **第二层（仅视口、仅白名单）**：`.lnk`/`.url`/`.library-ms`（图标在目标上）、驱动器根
  （图标是卷的）、目录（`desktop.ini` 可能改名它的图标）。这些才允许真实路径解析，且带
  `SHGFI_ADDOVERLAYS` 让 shell 自己把快捷方式箭头、共享手形叠上去。目录还多一道前置探针：
  `desktop.ini` 不存在就直接退回第一层——一次路径查找在这台机器上约等于重读一个小目录
  （`docs/notes/watch-sweep-costs-a-path-lookup.md`），不值得为"没有自定义图标的目录"付。

线程与生命周期：

- 单个专职 **STA** worker（`CoInitializeEx(COINIT_APARTMENTTHREADED)`），启动时把
  `SHIL_SYSSMALL/EXTRALARGE/JUMBO` 三个图像列表**一次全暖**，后面任何视图要更大尺寸都不会
  在滚动中途付构建费；今天只有 SYSSMALL 被用，暖另外两个是便宜的保险。
- **`HICON` 不跨线程**。worker 里 `HICON → 32 位 DIB + DrawIconEx → QImage`，随即
  `DestroyIcon`；跨过线程边界的只有 `QImage`（Qt 允许在 GUI 线程外构建的唯一像素容器），
  `QIcon`/`QPixmap` 在 GUI 线程的 `store()` 里才造。提取与销毁之间没有任何提前返回，
  且两个原子计数夹着这一段——闸门读的就是这对计数。
- 缓存是 `QHash<IconKey, QIcon>`；**请求表上限 4096、超了连图标表整体清空**（复查后从"图标表
  上限 512"改到请求表上：第一层的键总共也就几十种扩展名，真正会随目录长大的是第二层"一路径一键"
  的请求与图标，而几万子目录的文件夹正是这个壳的设计目标，所以上限要压在请求那一侧）。

重画范围：计划写的是由滚动条推 `setViewportRange(first,last)`。实现里用的是**模型当前物化的窗口**
`[m_first, m_first + m_rows.size())`——因为这个模型是拉式的，视口能画的就是这个窗口，滚动会先触发
取数再触发绘制，所以"物化窗口"与"在屏行"是同一个集合，不需要再维护一份滚动条推来的范围。
`dataChanged` 带 `{Qt::DecorationRole}`，只监听文本的委托整个跳过这个信号。

图标**不进 delta 流、不 bump generation、不触发重排**：它是视图侧拿行内已有数据（名字、属性）
做的 join，所以 `--self-check` 的确定性不受影响——协议面的日志里一个图标字节都没有。

## 3. 解决方式

- 新 `src/iconcache.{h,cpp}`：`IconKey`（后缀/路径/attrs/目录位/层级）、`IconWorker`（STA 线程、
  请求队列、三个图像列表、提取/销毁计数）、`IconCache`（GUI 线程面：`iconFor` 命中给真图标、
  未命中给**样式自带的占位 glyph** 并把键排队；`store()` 落库并发 `resolved()`）。
- `FamViewRow` 加 `quint32 attrs`，`bridge.cpp` 的 decode 把记录里**本来就有**的 `attrs` 抄过来
  ——不新增 ABI 字段，只是不再丢掉它。
- `FamListModel`：`data()` 加 `Qt::DecorationRole`（键由名字与属性现算；只有第二层的键才合成
  完整路径）；`setIconCache()`；`iconsResolved()` 只重画物化窗口。
- `MainWindow` 持有一个 `IconCache` 并交给模型；新增 `scrollForSelfCheck()`：九个停点扫过整个
  滚动范围再回顶，每个停点 `processEvents()` + `viewport()->repaint()`。
- `main.cpp` 的自检在抓图前打一行 `icons: extracted=… destroyed=… cached=…`。

**`repaint()` 而不是 `update()` 是测出来的，不是想出来的。** 第一版风暴用
`setValue + processEvents`：滚动范围确实动了（日志 `scrolled a 110-step range`），但只解析出
**4** 个键——延迟的绘制把九个停点折叠成了"最后停在哪儿就画哪儿"的一次绘制。换成同步 `repaint()`
之后同一次运行解析出 **9** 个键（目录、`.txt`、`.png`、`.csv`、`.md`、`.mp3`、`.url`、无扩展名、
以及第二层的目录探针），正是九个停点扫过的全部带。

## 4. 解决方式效果评估

**正面证据（全部真机实跑，`D:\Datas\fam-loss-check`，126 行 / 9 种键的树）。**

- 闸门读数：`icons: extracted=9 destroyed=9 cached=9`，**同一片树跑两次都是这组数**；
  3 行树是 `extracted=1 destroyed=1 cached=1`；搜索 11 行是 `extracted=1 destroyed=1 cached=1`。
  提取与销毁相等就是"反复滚动无 HICON 泄漏"这条门槛的读数形式。
- 截图三张核对过：目录视图里 `arch` 是文件夹 glyph、`.txt` 行是文档 glyph；搜索视图里
  `pic-*.png` 行是另一种（图像）glyph——两层键各自落到了对的图标上。
- `abi-smoke` 一字未改退出 0（`dropped_events:0`、`inserts observed=1`）；协议面日志里没有
  图标带来的任何新行、新标志或新 generation——"图标不进 delta 流"这条由日志的**不变**证明。
- Rust 侧这一增量零改动：337 条测试、clippy、fmt 的绿色是上一笔提交的状态，C++ 不碰它们。

**这次没做、没测到的（如实列）。**

1. **`.lnk` 的真目标解析没在本机测过**：造一个真 `.lnk` 需要 COM 的 `IShellLink`，超出这一笔；
   第二层的真实路径分支由 `.url`（纯 INI 文本）与目录探针代跑。`.library-ms` 同样只白名单在列、
   没有样本。
2. **`desktop.ini` 的正向分支没跑过**：样本树里没有自定义图标的目录，跑到的只有"探针不存在 →
   退回第一层"那条负路径。叠图标（`SHGFI_ADDOVERLAYS`）也没有样本（没有共享/链接目标）。
3. **4096 上限的清空路径没测过**，也不打算为测它造一万个子目录。
4. 图标解析的**延迟没有单独的数字**：风暴落在自检 1.5s→2.5s 的窗口里，抓图时刻（约 3.6–3.9s）
   只能证明"解析完了"，不能把毫秒归到图标上。要这个数字得给 worker 加计时，留给真有人嫌慢的时候。
5. 一次**预先存在**的现象被这次跑撞见并确认与图标无关：刚创建几秒的目录在状态栏显示 `· stale`，
   几分钟后同一片树再跑就没了——那是增量 4 的写时间对账在目录 mtime 尚未落定时的正常反应，
   它的**标签**问题归增量 8 的欠账 #2，不在这一笔。
6. 暖起来的 `SHIL_EXTRALARGE`/`SHIL_JUMBO` 今天没有消费者，要等增量 8 的大图标视图；
   在那之前它们是启动时一次性的便宜保险，不是功能。
