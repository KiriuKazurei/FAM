# 分栏改成开关，拖到另一列就是真的搬文件（增量 14）

## 1. 描述问题

用户提的原话：

> 界面布局优化，左右分栏功能变成通过按钮开关的展示功能，关闭时界面参考 Windows 自带的资源管理器，
> 展开后双列独立，允许从其中一列拖拽到另一列

三个追问的回答把形状钉死：

- 左格是**第二个独立浏览器**（不是"同一份内容的另一个视图"）；
- **直接拖动进行真实的文件移动，按住 Ctrl 拖动松手则是复制文件**；
- 折叠时**只收左格**（右栏那一套就是原来的窗口）。

动手之前，代码长这样：

- `MainWindow` 只有一个 `ColumnChain *m_chain`、一个结果 `QListView`、一个 `m_searchToken`。
  "那个链""那个结果列表""那个正在跑的搜索"在窗口里各自只指一个东西。
- 分栏不是按钮：`--split` 之类的开关根本没有，左右两栏无从谈起。
- **ABI 上没有任何写入口**。`crates/fam-ffi/src/lib.rs` 的 22 个 `#[no_mangle]` 导出里，
  改磁盘的只有 `fam_tag_add` / `fam_tag_remove`（改的是库里的标签，不是文件系统）。
  壳里也没有任何一条路径能把一个文件从 A 挪到 B。
- 移动/复制这件事在核心里**没有实现**：`fam-svc-mcp` 的 `fs_move` 是增量 12 写的，
  它当时直接调 `std::fs::rename`，规则散在工具里。
- 模型不产 mime：`FamListModel` 没有 `mimeData`/`mimeTypes`/`flags` 的重写，
  `QAbstractItemModel::flags()` 默认**不含** `Qt::ItemIsDragEnabled`，
  而 `QAbstractItemView::canDrag()` 没这个位就不启动拖拽 —— 也就是说行了不可拿。

增量 14 期间另有一条来自用户的现场反馈（截图）：**"单栏布局错误，单栏时应当占用所有空白区域"**。
这条记在第 3 节，因为它和分栏是同一个改动的两半。

## 2. 分析问题

### 为什么写入口要过人这一侧，而规则要在核心那一侧

一动手就有的分岔：拖放要落文件，最省事的做法是壳里直接 `std::filesystem::rename`。
那会让"移动一个文件"在这个项目里存在**两条实现**，而其中一条不过审计、不过 `path.move` 这个名字、
将来也不会出现在 MCP 那一层的判据里。

所以顺序反过来：先把移动/复制抽成一个 crate（`fam-svc-fs`），让 `fs_move` 与壳都调它，
再在 ABI 上开第一个写入口 `fam_path_op`，壳只负责**组合目标路径**与**说清哪条规则拒的**。

规则一条都不留在 C++ 里。今天壳里的 `handleDrop` 只做三件事：
拼 `目标目录 + 原名`、跳过同目录、把核心回的那句话贴到状态行。
"永不覆盖""目录不能复制""跨卷目录不做"全部在 `fam_svc_fs::transfer` 里，
两边（人和代理）撞上是同一句话。

### 代理能搬、不能复制：一处刻意的不对称

`crates/fam-svc-mcp/src/mutations.rs:175` 只有 `Action::Move`，工具表里也只有 `fs_move`；
`Action::Copy` 在核心里存在、在审计里有名字（`path.copy`，增量 12 就登记了），
但**没有工具走到它**。目前唯一的调用方是壳的 Ctrl 拖放。

理由不是"忘了加"：多一个工具就多一处门要覆盖的面，而这次没人要求代理复制文件。
复制与移动的危害方向不同 —— 移动会少一份，复制会多一份，
一个跑飞的代理拿复制可以把盘填满，拿移动则是把东西搬走；
两者都需要能撤销才好放开，而撤销这次没做（见第 4 节）。
**这一条是这次的决定，不是实测结论**，写在这里是为了下一次加 `fs_copy` 时先看到它。

### 跨卷的目录为什么直接拒

`std::fs::rename` 跨卷会失败（Windows 17 / unix 18）。要跨过它只有递归复制 + 递归删除一条路，
而那条路中间断电就留下两份、且删除是不可回退的一半。
`fam-svc-fs` 里对**文件**做了这件事（`By::CopiedThenRemoved`，复制成功后删源），
因为单个文件的复制-删除是可判定的；对**目录**不做，直接
`Error::CrossVolumeDirectory`，句子是"moving the directory X across volumes is not supported;
copy it with another tool"。这不是没做完，是**故意留一个会响的洞**：
一个静默把目录树复制两遍再删一半的实现，比一句"我不做"坏得多。

### "永不覆盖"对资源管理器的"替换/换名"

用户说"参考 Windows 自带的资源管理器"，说的是**布局**；拖放语义按用户自己给的规则走。
资源管理器拖到已有同名文件时会弹窗问替换还是换名。这里不弹窗：**目标存在就拒绝，什么都不改**
（`Error::Exists`，`an_existing_target_is_refused_and_nothing_changes` 那条测试钉着它）。
理由：弹窗是一种状态，而壳里目前没有任何模态交互需要被无头闸门驱动；
自动换名（`file (2).txt`）更糟 —— 它会让一次拖放的结果不确定，
而"拖完文件叫什么"是用户能预测的那类事情。两者都进第 4 节的不修清单。

### 拖拽的载荷用私有 mime 类型，不用 `text/uri-list`

`FileView::kMime = "application/x-fam-paths"`，一行一个路径（`fileview.cpp:8`）。
用标准 `text/uri-list` 的话，这一窗行就能拖到**任何**别的程序那里去，
那边会按它自己的规则搬文件：不过 `fam_path_op`、不过"永不覆盖"、审计里一行都没有。
一个项目里存在两条搬文件的路、只有一条被记账，不是取舍而是漏洞。
私有类型同时把反方向也关掉：`FileView::takes()` 只认自己的类型，
从资源管理器拖进来会在边界上被拒 —— 这条有证据（见第 4 节的 `border of ...: ours accepted, text/uri-list refused`）。

代价要说出来：**壳与资源管理器之间不能互相拖文件**。这是这次选的边界，不是漏的。

### 落点是"这一列显示的文件夹"，不是光标下那一行

`FileView` 在构造时被命名一个文件夹（`setDropFolder`，列在自己的生命周期里目录不变，
所以设一次），落下时目标 = 那个文件夹 + 原名。
光标正好停在某个文件夹行上**不会**改成"落进那个文件夹"。
与用户确认的形状（"从一列拖到另一列"）一致；也避免"位置决定语义"这种看不见就猜不到的规则。

### ABI 版本为什么仍然报 2

`fam_abi_version()` 门的是**行的布局**（`FamRecord` 的字段与偏移），
`main.cpp` 在 `QApplication` 之前比它，不等就退 75 —— 因为读错布局会画出一屏垃圾还管它叫文件夹。
加一个导出函数不改布局；`record_size` 才是布局演进的那条缝。
所以 `fam_path_op` 是**追加**，版本不动。这是判断，不是默认。

### "单栏时应当占用所有空白区域"：两个原因，第一个是猜的

截图上单栏只占左边一条。第一个假设是列宽上限 —— 确实有一处 `setMaximumWidth(460)`
和 `kColumnIdealWidth`，删掉之后**仍然窄**。真正的原因是 `Pane` 这个新容器**自己没装布局**：
`pages` 作为子控件保住了出生时的几何，窗口把一半宽度交给 `Pane`，
`Pane` 里那份列表还是 340px。两处都改（`pane.cpp` 的 `QHBoxLayout(this)`、
`columnchain.cpp` 里删掉上限并留一段"为什么没有上限"的注释），
截图确认单栏铺满。

**记这一条是为了那个方法论**：如果只删了上限就收工，屏幕上还是错的，
而我当时已经准备把"上限导致窄"写进账里 —— 是截图把它拦下来的。

## 3. 解决方式

四层，每层一次提交（代码一层、账一层）。

### 14a —— `crates/fam-svc-fs`：移动/复制只有一个实现

`pub fn transfer(from, to, Action::{Move,Copy}) -> Result<Transferred, Error>`，
`By::{Renamed, CopiedThenRemoved, Copied}` 说清"它是怎么过去的"。
错误七种：`Missing` / `Exists` / `SamePath` / `NoParent` / `CopyUnsupported` /
`CrossVolumeDirectory` / `Io`，每一种是一句完整的人话（壳直接贴到状态行）。
跨卷靠原始 OS 错误码识别（Windows 17 / unix 18），不靠字符串。
`fs_move` 改成调它（`mutations.rs:175`）。8 条测试。

### 14b —— ABI 上第一个写入口

`fam_path_op(core, from, to, flags, out, err)`，`FAM_PATH_COPY = 1`（`fam-ffi/src/lib.rs:1150`），
导出表从 21 个变 22 个。`apps/fam-shell/include/famcore.h` 把规则写在使用点旁边，
`bridge.cpp:137` 的 `FamBridge::pathOp` 是壳里唯一一条会改磁盘的调用。
`abi-smoke` 加三条断言（move / copy / refuse-overwrite），末行变成
`insert=1 cut=1 move=1 copy=1 refuse-overwrite=1 -> ok`。

### 14c —— 双栏与那个按钮

- `apps/fam-shell/src/pane.{h,cpp}`：一个浏览面 = 一条列链 + 自己的结果模型/视图 +
  **自己的 `searchToken` / `searchRunning`**。两个面共用一个搜索令牌的话，
  一个面的轮询会去重写另一个面的行 —— 那是披着布局外壳的数据竞争。
- `MainWindow` 持 `m_left` / `m_right` / `m_active`；`Split` 是 `checkable` 的
  `QPushButton`，objectName 新加一个 `toggleButton`（复用 `iconButton` 会得到 32×32 且没有
  `:checked` 规则的按钮 —— 按下去看不出按下了）。
- 折叠 = `m_left->hide()`，右栏原样，所以折叠后的窗口就是改动前的那个窗口，
  不是新窗口的缩水版。
- 状态行只由**活动**那一面写。`openFolderIn` 一开始无条件写，无头跑自己抓到了：
  窗口底部在描述一个工具栏不会作用的文件夹。加了 `if (pane == active())`。
- 时间线加 `toggleSplitForSelfCheck(bool)`，日志：
  `self-check opened the split: left visible=1 left-depth=1 right-depth=1 (was 1) active=right`。
  那个 `(was 1)` 是"右栏没被左栏开合影响"的证据。

### 14d —— 拖到另一列

- `apps/fam-shell/src/fileview.{h,cpp}`：`QListView` 的子类，
  `setDropFolder()` 同时决定它是 DragOnly 还是 DragDrop；
  `mimeDataForPaths` / `pathsOf` 是那对编解码；
  `dropEvent` 从**事件自带的** `modifiers()` 读 Ctrl，不读 `proposedAction()`
  （Qt 给列表视图的默认落放动作是 Copy，"移动悄悄变成复制"在状态行上看不出来）。
- `FamListModel` 补 `flags()`（`ItemIsDragEnabled`）、`mimeTypes()`、`mimeData()`、
  `supportedDragActions() = Copy | Move`（只答 Move 的话，拖拽层会在 Ctrl 时直接拒掉，
  drop 事件根本到不了这个窗口）。
- 信号三段接力：`FileView::dropped` → `ColumnChain::dropped` → `Pane::dropped` →
  `MainWindow::handleDrop`（`mainwindow.cpp:551`）。签名一致，中继是 signal-to-signal，没有 lambda。
- 结果列表是 `FileView` 但**不命名文件夹**：它的行来自许多目录，没有"那一个目录"可落。
  它是拖拽的来源，不是目标。
- 无头侧四步（`--self-check-mutate`）：`prepareTransferForSelfCheck` →
  `moveAcrossPanesForSelfCheck` → `copyBackAcrossPanesForSelfCheck` → `dropInPlaceForSelfCheck`。
  `postDrop`（`mainwindow.cpp:798`）把**进入 + 落下**两个事件都发给 viewport，
  因为 `QAbstractItemView` 要先见过 enter 才会把 drop 路由到 `dropEvent`（见第 4 节）。
  两端都在 `--watch` 那棵树里，写进去的文件由 `removeSelfCheckProbe` 清干净。

## 4. 解决方式效果评估

### 正面证据（每条都是一次真实运行的输出）

**核心侧未动**：`cargo test --workspace` → `passed=499 failed=0 ignored=2`；
`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cargo fmt --all -- --check` 干净。
`fam-svc-fs` 8 条测试逐条绿，其中三条是这次依赖的规则：
`an_existing_target_is_refused_and_nothing_changes`、
`copying_a_directory_is_refused_rather_than_walked`、
`only_the_different_device_code_turns_a_move_into_a_copy`。
`abi-smoke`：`insert=1 cut=1 move=1 copy=1 refuse-overwrite=1 -> ok`，退出 0。

**壳的无头闸门**（同一棵演示树 `...\fam-demo\tree`，light 带 `--self-check-mutate`、dark 一遍）：

- 拖到另一列 = **真实移动**：
  `self-check moved by drop from ...\tree\2024-合同 to ...\tree: left depth=2, right depth=1,
  view answered move, taken=1, ...\2024-合同\fam-self-check-drag.txt exists=0,
  ...\tree\fam-self-check-drag.txt exists=1`
  —— 源不在了、目标在了，这是磁盘上的事实，不是"处理函数跑过"。
- Ctrl 拖 = **复制**：`view answered copy, taken=1`，两端 `exists=1`、`exists=1`。
  两条 case 唯一可分辨的就是源还在不在，所以两个方向各测一次。
- 同目录落下 = **不发调用**：`drop moved into ...\tree: 0 transferred, 1 already there, 0 refused:
  1 item(s) were already in that folder`，状态行从 `copied 1 item(s) into ...` 变成
  `1 item(s) were already in that folder`，文件仍在原位。
- 边界：`self-check border of ...: ours accepted, text/uri-list refused`（两个方向都测）。
- 载荷往返：`self-check encoded a listed row as a drag: 2024-合同 carried 1 path(s), same=1`
  —— 由 `FamListModel::mimeData` 产生、由 `FileView::pathsOf` 解回，中文名列穿过 UTF-8 载荷不变形。
- 两栏独立：`left visible=1 left-depth=1 right-depth=1 (was 1)`；
  拖放三步全程 `left depth=2, right depth=1` 不变。
- 图标与往返未退化：`icons: extracted=18 destroyed=18 cached=18 pending=0`，
  `ipc: headers=10 pages=19 polls=78 retries=0 total=107`；
  两遍的对比度报告都是"受门槛的 13 对里 0 对不过"。
- 一次拖放之后两栏各自收到更新（两个面各有一个 `tree` 列，同一句话打两遍就是两条独立 watch）：
  `column ...\tree reconciled in 1 pair(s) + 1 modified row(s), 5 row(s), selection kept` ×2，
  清理时 `column ...\tree cut 1 position(s) in 1 pair(s), 4 row(s) left` ×2。
- 截图读过三页：`drop-light.png` 三列并排（左栏两列 + 右栏一列）、状态行、`Split` 处于按下态；
  `drop-dark.png` 同一形状在暗面；`collapsed.png`（不带 `--self-check-mutate` 的那一遍）
  是收起状态：**一列铺满驱动器栏右边的整幅**，列头一路伸到最右的排序按钮，中间没有空白带
  —— 那就是用户报"单栏布局错误"时缺的东西。

**新工具 `model-smoke`**（`apps/fam-shell/tools/model_smoke.cpp`）：六条全绿，`MODEL_SMOKE=0`。
它测的是 14d 顺带抓出的对账缺陷，详见
[reconcile-moved-the-readers-row.md](reconcile-moved-the-readers-row.md)。

### 一处**我的测试不合格**（第三次了，仍然要记）

`dropInPlaceForSelfCheck` 第一版只发 `QDropEvent`，日志报
`view answered move, taken=1, still there=1`，看起来完全通过。
但 `handleDrop` 那一行**根本没出现**，状态行也没动。
插桩到 `FileView::dropEvent` 才看清：前两步（各自前面有一次 drag-enter）触发了 `dropEvent`，
这一步没有触发 —— `QAbstractItemView` 要先在 enter 里进入拖拽状态，才把后续的 drop 路由给
`dropEvent`。单独发一个 drop，事件被 Qt 的默认路径接受，`dropAction()` 就成了我传进去的那个值。
**`taken=1` 于是是"没人处理过这个事件"的假通过。**

修法不是给那一步补一次 enter，而是让 `postDrop` 自己发**进入 + 落下**这一对
（真实拖放本来就是一对），这样任何调用方都不可能只发一半。修后同一步产出上面那条
`0 transferred, 1 already there` 的真实证据。

### 代价

- **无头跑长了约 1.3 秒**：mutate 那遍从 `wrote ... at 10719ms` 到 `12021ms`（实测，同机同树）。
  轮询数随之 57 → 78 次，`ipc total` 72 → 107 —— 这是时长换来的，不是每次操作变贵。
- **壳里多两个文件、多一个 CMake 目标**：`fileview.{h,cpp}` 与 `model-smoke`。
  `model-smoke` 要链 `Qt6::Widgets` + `fam-ui` + `shell32/comctl32/ole32`
  （它把模型接在真 `QListView` 上，否则测不到"当前行被搬走"这件事）。
- **`--self-check-mutate` 现在会往 `--watch` 那棵树里写两个文件**（原来一个），
  用完自删并在日志里逐个点名（`self-check removed ...: yes`）。
  opt-in 的边界没变，但**代价变大了**：拿真实目录跑它，最坏情况是留下一个
  `fam-self-check-drag.txt`（进程中途被杀时）。README 里那句警告现在覆盖两个文件。
- 拖拽的载荷是私有类型：**与资源管理器互拖不可用**（这是决定，不是缺陷，但它是代价）。
- `Pane` 抽出来之后，`MainWindow` 里凡是"那个链/那个列表"的地方都要先问一句"哪一面"。
  这类问句共 22 处（`active()` 的调用点），漏一处就是"状态行描述另一面"那种 bug。

### 残留风险与局限（这次没解决什么、为什么不）

1. **没有撤销**。一次拖放直接改盘，Ctrl 拖会留下第二份。
   不做的理由：撤销要么靠日志回放、要么靠事务，两者都比"这次要能搬文件"大一个数量级。
   严重度：**中**（误拖一个文件夹进另一个文件夹，人现在只能自己拖回来）。归属：撤销/回收这一层。
2. **没有自动换名，也没有替换确认**。同名就拒。
   理由见第 2 节。严重度：低（一句话会告诉你是谁拒的、为什么）。归属：与 1 同层。
3. **没有进度**。跨卷复制一个大文件时界面不会显示任何东西 —— 调用是同步的，
   发生在 GUI 线程上，所以**大文件会把窗口冻住**直到核心返回。
   严重度：**中**（这是这次留下的最实际的坑）。归属：把 `pathOp` 推到工作线程，
   或者在核心侧提供进度；两者都要求先有"一次写可以取消"的语义。
4. **跨卷目录不做**（故意的，见第 2 节）。严重度：低。归属：谁做谁先答"断电留两份怎么办"。
5. **不能拖到左侧盘符栏**：`DriveList` 不是 `FileView`，没有目标文件夹可命名。
   严重度：低。归属：分栏的后续。
6. **不能拖到搜索结果列表**（它是来源不是目标），也**不能从资源管理器拖进来**。
   理由都在第 2 节。严重度：低。
7. **落在文件夹行上不会改目标**（目标恒为该列显示的目录）。严重度：低。
   这条与"位置不决定语义"是同一个决定，改它要先想清楚"落在文件夹行 vs 落在空白处"两种意图。
8. **拖拽悬停时列上没有任何高亮反馈**：`setDropIndicatorShown(false)` 关掉了那条"插在中间"的线
   （语义不对），但没有换成"这一列会收下它"的替代提示。
   严重度：**中**（人只能凭落下之后的结果判断目标对不对）。归属：主题层，一条 `:hover` 之外的规则。
9. **窄列里行名会缩成一个省略号**：三列并排时（左栏两列 + 右栏一列）行名只剩 `…`，
   大小与类型标签把名字挤掉了。列头的路径重叠那一处**这次修了**
   （`title->setMinimumWidth(0)` + `QSizePolicy::Ignored`，截图确认按钮不再被压住），
   行内的没修。严重度：低（视觉）。归属：`RowDelegate` 的省略策略。
10. **折叠时左栏仍在被轮询**：`pollChanges` 对两个面都跑，包括 `m_left->isVisible() == false` 的那一个。
    这是刻意的（收起再展开不能看到旧内容），但代价是收起状态下仍占着 watch 与 IPC。
    严重度：低。归属：如果哪天要省这份开销，得先定义"藏起来多久算关掉"。
11. **`emptyState` 那条 QSS 规则仍然没有控件在用**（13 遗留），**运行时不切主题**（13 遗留）。
    归属不变。
