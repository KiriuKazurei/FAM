# 壳吃下 Ant Design 基准：一处"顺序不能减一步"的教训在样式上重演了

- 增量：13
- 提交：`feat(shell)`（本文件之前一笔）
- 涉及：新增 `apps/fam-ui/{CMakeLists.txt,src/theme.{h,cpp},src/rowdelegate.{h,cpp}}`（从
  `apps/fam-design/src/` **搬过来**，不是复制）、
  `apps/fam-shell/{CMakeLists.txt,src/main.cpp,src/mainwindow.cpp,src/columnchain.cpp,src/drivelist.cpp,src/famlistmodel.h}`、
  `apps/fam-design/{CMakeLists.txt,src/preview.cpp}`

## 1. 描述问题

用户 2026-09-25 的指令是"保留 Ant Design 样式版本，接下来的开发均基于这个版本"。这**推翻**了
2026-09-24 那条"原项目界面设计保持独立"——那条把我正在做的主题层从产品壳里拽出来，
关进了只链 Qt 的 `apps/fam-design`。

动手之前先把两边的真实状态量了一遍，不按"差不多"办事：

| 事实 | 怎么量的 |
| --- | --- |
| 产品壳**一处样式都没有** | `grep -nE "setStyleSheet\|setItemDelegate\|setObjectName" apps/fam-shell/src/mainwindow.cpp` → **0 处**；`main.cpp` 里 `setStyleSheet\|setFont\|palette` → **0 处**。整个壳只有一处 `setObjectName("sortHeader")`（`columnchain.cpp:80`，8b 写的），而**没有任何 QSS 读它**——那是一个挂给不存在听众的名字 |
| 壳的行是模型拼好的一个字符串 | `famlistmodel.cpp:146`：`"%1  %2"` + `" [%3]"` → `"name  1.2 KB  [archive]"`，一个颜色一个字号，且**每个文件夹都印 `0 B`**（`size` 字段是零，不是"未知"） |
| 主题层与模型无关，可以整块搬 | `grep -cE "Preview\|preview::" theme.cpp` → **0**；`rowdelegate.h` 只 `#include "theme.h"` |
| 两个模型的 role 名字**已经一样** | `FamListModel::{Name,Kind,Size,Modified,Path,Directory}Role` vs `PreviewModel::{Name,Kind,Size,Modified,Directory}Role`——当初样机就是照壳写的 |

指令有三种落法，差别很大，问清了再动：
**壳吃样机主题**（选定）/ 样机长成产品壳 / 只共用 token。样机去向：**保留为基准**（选定）。

## 2. 分析问题

### 2.1 为什么不是"样机长成产品壳"

壳里有真跑过、真被 A/B 过的东西：8a-1 的窗口 diff（重读保住选择与滚动，测试去掉修复会等满 10s）、
8a-4 的 IPC 计数（`retries=0` 是闸门）、7 的图标层（提取/销毁/缓存三数相等 + `pending=0`）、
8b 那条"lambda 捕获列的**位置** + 深度上限从最前丢列 → 越界崩溃"的修复、9 把 abi-smoke 迁到 v2。
换壳等于把这些在新壳里重长一遍，中间还会出现"两个壳都能跑、只有一个能用"的时期。
**样式采纳不需要付这个价**：要改的是 paint 层，数据通路一行不用动。

### 2.2 为什么"只共用 token"不够

token 共用解决颜色，解决不了行——壳那行"terminal 的答案"来自**模型的 DisplayRole 字符串**，
修它的是委托，不是色板。所以共用必须包含 `rowdelegate`，而一旦包含委托，
委托就同时依赖两个不同的模型类型（壳的 `FamListModel`、样机的 `PreviewModel`）。

### 2.3 复制是最省事也最坏的做法

第一版我就是复制的（`cp` 四个文件进 `apps/fam-shell/src/`），当场能跑。
但"接下来的开发均基于这个版本"意味着**改 token 的地方必须只有一个**：
两份拷贝会漂，而漂移的表现是"样机是对的、产品是旧的"，且只有一个是天天在屏幕上的那个。
所以最终形状是 `apps/fam-ui`：一个 STATIC 库，两个 target 都 `add_subdirectory` 它，
**物理上只有一份**。

依赖边界照旧守得住：`fam-ui` 只链 `Qt6::Widgets`，没有 `famcore`、没有 Rust crate——
样机原来那条"参考物不能依赖被参考的东西"因此继续成立。

代价是委托不能再 `#include` 任何一个模型头。缝在这里：

```cpp
/* Which data roles a model uses for the four things a row paints, and how it wants a byte count
 * spoken. ... Without that seam the one copy of the row could not serve both, and the choice would
 * be back to two copies drifting. */
struct RowRoles {
    int name, kind, size, directory;
    QString (*formatSize)(quint64);
};
```

壳在 `famlistmodel.h` 尾部给一份 `kFamRowRoles`（两处调用点共用，防止列与搜索结果读错角色），
样机在调用点内联一份。两边各自的文件里都不出现对方的类型。

### 2.4 主题必须挂在 app 上、在建窗之前

sheet 在 widget 建好之后才换，那些 widget 的 size hint 是按旧样式算的；
而委托在 `paint()` 时读同一个 `Theme::instance()`。
所以"窗口构造之前"是让 sheet 与手绘行一致的唯一时机——这条不是风格，是**两处读同一个数**的必要条件。

**Fusion 不是可选项**：sheet 沉默的地方，原生 Windows 样式会自己画渐变与焦点光晕，跟 token 集打架。
这等于换掉壳的整条渲染路径，所以壳的既有闸门**必须全部重跑**（§4.1），
"看着没坏"在这层不算证据。

### 2.5 我给自己写了一句错的注释，然后用一个像素把它撤了

`QWidget#page { background: bgLayout }` 对 `MainWindow` 这种 QWidget 子类生效吗？
我按"必须 `WA_StyledBackground`"的常识加了那一行，注释还写成了道理。
然后想起样机里**没有**这一行（`grep WA_StyledBackground apps/fam-design/src/` → 0），
而它浅色底一直是画出来的。

没有吵赢自己，去量了：手解 PNG 取点。

| 取点 | 样机 light | 壳 light | 壳 dark |
| --- | --- | --- | --- |
| 页底（列卡片之外） | `245,245,245` | `245,245,245` | `0,0,0` |
| 列卡片内 | `255,255,255` | `255,255,255` | `20,20,20` |

`245` 就是 `#f5f5f5`、`0` 就是暗面 `bgLayout`、`20` 就是 `#141414`——
**规则自己就生效**（两个窗口都没重写 `paintEvent`，QStyleSheetStyle 拥有背景）。
于是那一行属性与它那句"道理"一起删掉，注释改成实测结论。
这条留在账上是因为它和增量 12 那三条是同一类错：**先相信一般原则，再补证据**。

### 2.6 列头：基准与产品在同一处表面有两个版本

样机的列头是"路径标签 + 排序按钮"，壳只有排序按钮。
这不是颜色问题，是**采纳之后基准长什么样、产品就得长什么样**的问题，所以补进壳（约 6 行）：
`QWidget#columnHeader` 里 `QLabel#columnPath` + 原来那个 `QPushButton#sortHeader`。
列的 `path` 在 `addColumn` 时定死（`grep "\->path = "` 只有一处），所以不需要"跟着变"的状态。

**踩到两处小的**：
- 第一版写了 `title->setElideMode(Qt::ElideMiddle)` —— `QLabel` 没有这个 API，编译当场拦下。
  改成整条路径进 `toolTip`（230px 的列装不下 `D:\Datas\...\深入一层`，任何拼法都装不下）。
- 链接失败两次，报 `cannot open output file fam-design.exe: Permission denied`——
  不是代码错，是**我几分钟前起的那两个窗口正占着自己的 exe**。关掉重链就过。
  记下来是因为这类"看起来像构建系统坏了"的错，第一反应应该是数一数进程。

### 2.7 每列的空状态标签：**没补**，理由是它会和状态行打架

样机每列有个 `QLabel#emptyState`，壳没有。看着像"再补 6 行"，其实不是：
壳的 `EMPTY` / `FAILED` 是状态行的职责（8a-2 专门做过五种状态各配一句人话，
其中 `EMPTY` 是"没读过"而不是"是空的"）。再在每列挂一个空状态标签，
同一件事就有两个地方各说一遍，而它们**能不一致**（列说"空的"、状态行说"没读过"）。
那是行为变更，不是样式采纳。所以 QSS 里 `QLabel#emptyState` 现在匹配不到东西（一条哑规则），
记 §4.3 第 1 条。

## 3. 解决方式

```
apps/fam-ui/            ← 设计系统的唯一一份：theme.{h,cpp} + rowdelegate.{h,cpp}，只链 Qt6::Widgets
apps/fam-design/        ← 基准与截图工具（--shot / --theme，实测对比度表打 stdout），不再长产品功能
apps/fam-shell/         ← 产品面：数据通路一行未动，多了主题挂载与委托
```

壳里挂的东西，逐处：

```cpp
// main.cpp —— 建窗之前，挂在 app 上
QApplication::setStyle(QStringLiteral("Fusion"));
Theme::instance().setScheme(scheme);            // --theme light|dark|system，默认跟桌面
app.setFont(Theme::instance().applicationFont());
app.setStyleSheet(Theme::instance().styleSheet());
qInfo("theme: %s\n%s", name, contrastReport());  // 可读性是跑出来的数字，不是 note 里的算术
```

```cpp
// columnchain.cpp —— 每一列
column->frame->setObjectName(Theme::columnFrameName());
column->view->setObjectName(Theme::listName());
column->view->setItemDelegate(new RowDelegate(&Theme::instance(), kFamRowRoles, column->view));
column->view->setMouseTracking(true);                      // 委托自绘 hover，不 tracking 就永远没有
column->view->viewport()->setAttribute(Qt::WA_Hover, true);
// 列头：路径标签 + 排序按钮，与基准同一形状
```

`mainwindow.cpp` 是 `page` / `toolbar` / `searchInput` / `primaryButton` / `statusLine`
加搜索结果视图的委托；`drivelist.cpp` 是 `sidebar`。
`toolbar` 从"一个裸 layout"变成"一个有 objectName 的 QWidget"，因为
`QWidget#toolbar` 的底色与那条下沿 split 需要有个东西去匹配。

跑：与之前完全一样，多一个旗标。

```bash
cmake -S apps/fam-shell -B apps/fam-shell/build -G Ninja \
  -DCMAKE_CXX_COMPILER=<Qt mingw>/bin/g++.exe -DCMAKE_PREFIX_PATH=<Qt>/6.11.2/mingw_64 \
  -DFAMCORE_IMPLIB=target/x86_64-pc-windows-gnu/debug/libfamcore.dll.a \
  -DFAMCORE_DLL=target/x86_64-pc-windows-gnu/debug/famcore.dll
cmake --build apps/fam-shell/build            # fam-ui 由 add_subdirectory 带进来，无需单独配
apps/fam-shell/build/fam-shell.exe --theme dark --watch <目录>
```

## 4. 解决方式效果评估

### 4.1 正面证据

**渲染路径换了，所以壳的闸门全部重跑**（light 带 `--self-check-mutate`、dark 一遍）：

```
theme: light
column ...\tree reconciled in 1 pair(s), 5 row(s), selection kept
column ...\tree\2024-合同 reconciled in 2 pair(s), 2 row(s), selection kept
self-check removed the probe: yes
column ...\tree cut 1 position(s) in 1 pair(s), 4 row(s) left
icons: extracted=18 destroyed=18 cached=18 pending=0
ipc: headers=5 pages=6 polls=44 retries=0 total=55
```

逐项与采纳主题之前同形：窗口 diff 仍是 1 对 / 2 对且**选择保持**、探针建了又删、
删除走的是剪裁而不是重建、图标三数相等且 `pending=0`、IPC `retries=0`。
`abi-smoke` 在同一棵树上 `insert=1 cut=1 -> ok`，退出 0。

**只有一份实现**：`fam-shell` 与 `fam-design` 都链接 `fam-ui/libfam-ui.a`；
`apps/fam-design/src/` 下已经没有 theme/rowdelegate（`ls` 只剩 main/preview/previewmodel 五个文件），
样机的 `--shot` 仍然出图、stdout 仍打实测对比度表（light 13 对受门槛 0 对不过，
两处免除 1.41:1 与 1.83:1 照点名）。

**读图核对**（我自己逐张看过）：light 与 dark 各一张产品壳、一张演示树
（`2024-合同` / `photos` / `README.txt 7 B document` / `收件箱`）。
CJK 与 Latin 同一字面（`fontAt` 的家族序里点名了中文字体），文件夹 demi-bold、
尺寸三级色、类型 Tag 成芯片；**文件夹不再印 `0 B`**——那是委托替模型说的一句实话。
像素取点见 §2.5 那张表。

**Rust 侧一行未改**：`git diff --stat -- crates apps/fam-cli apps/fam-mcp` 为空；
`famcore.dll` 没重建，导出表因此不必重测（21 个那一版仍是当前版）。

### 4.2 代价

- 多一个 CMake 工程（`apps/fam-ui`，34 行）。两个 target 各自 `add_subdirectory` 它，
  binary dir 要显式给（两个 app 是分别配置的独立工程）。
- 委托多一个构造参数（`RowRoles`），换来的是它不依赖任何模型；
  壳里那份 `kFamRowRoles` 放在 `famlistmodel.h` 尾部，因此那个头现在 `#include "rowdelegate.h"`。
- **壳从此不吃原生 Windows 样式**。这是一次真实的观感与行为变更（滚动条、下拉、焦点环都归 sheet 管），
  §4.1 那些闸门就是为它重跑的。
- `--theme` 只在启动时读；样机那个"暗色/亮色"运行时切换按钮，壳没有（§4.3 第 2 条）。
- 复杂度：样式相关的 C++ 从"0 行"变成 955 行（theme 755 + delegate 200），
  其中**新写的只有 `RowRoles` 那 8 行**，其余是从基准搬过来的。

### 4.3 没解决的东西（严重度 / 归属）

1. **每列的空状态标签没有**，于是 sheet 里 `QLabel#emptyState` 是一条匹配不到东西的哑规则。
   理由见 §2.7：它要和状态行分工，那是行为设计。严重度：中低（空目录只有一片白 + 状态行一句话）。
   归属：下次动壳的工具栏/状态区时一起定"谁负责说这一列是空的"。
2. **壳没有运行时主题切换**。`Theme::setScheme()` 已经支持原地换（委托持引用、不拷色），
   缺的是"换完之后重贴 sheet + 让每列重绘"那一段调用方该做的事。
   严重度：中低。归属：要么补一个按钮（样机有现成形状），要么明确"跟随桌面、不给按钮"。
3. **列头路径在窄列下会被裁掉**（`QLabel` 不省略号），只有 tooltip 兜着。
   严重度：低。归属：真觉得碍眼再换 elided label，那是一个组件不是三行。
4. **基准的"布局"没有单一来源**，只有"颜色 + 行"有。样机的控件顺序、列宽、
   工具栏组成仍是它自己那份 `preview.cpp`。所以"基于这个版本开发"目前精确地意味着
   **token 与行绘制同源**，不意味着布局同源。严重度：中（这句话不说清就会有人以为已经同源）。
   归属：要么把布局也收进来（那是 2.1 那条路的轻量版），要么在 README 里把边界写死。
5. **暗面下系统图标偏亮的观感没有评估**。文件夹 glyph 是 shell 给的彩色图，
   非文字元素按 WCAG 不适用 3:1，所以**没有数字**，只有"看图不刺眼"。
   严重度：极低。归属：不修，写了理由；真要管就得自己出一套暗面图标。
6. **打包仍缺**（`install()`、windeployqt、启动脚本、`FAM_MCP_TOKEN` 由谁注入），
   现在还要加一句：`fam-ui` 是第三个 CMake 工程，脚本要能带着它配。
   严重度：高（"交给别人试"的门槛）。归属：一直没做，这一串增量共同的那一笔。
