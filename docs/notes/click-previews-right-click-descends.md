# 点击出预览，右键才开列（增量 33）

## 1. 描述问题

用户对浏览模型的改版要求：单击文件夹/文件**不再开新列**，而是右侧弹出预览面板；只有
右键菜单里的"在新列打开"才开新列。这是从 Miller Columns 的"选中即导航"到"选中即预览"
的模型变更。

## 2. 分析问题

### 选中与导航的分离

原来 `onCurrentChanged` 对目录调用 `open(path)` ——选中即导航。改为选中只发
`previewChanged(path, isDirectory)`（预览面板跟随），导航由右键菜单
`openInColumnRequested(path)` 显式触发。链条上的既有信号（delete/rename/open）不变。

### 预览面板读的是 shell 侧（QDir/QFile）

面板读的是 shell 侧数据（QDir 列子项、QFile 读文本预览）——ABI 的 watching/audit 是给
浏览列的，预览是被动的快照，跟随光标，不是数据访问路径。

## 3. 解决方式

`ColumnChain::onCurrentChanged` 移除 auto-open（选目录→发 previewChanged + 裁剪尾部、
选文件→发 previewChanged + 裁剪尾部）；`FileView::contextMenuEvent` 右键菜单（目录→
"在新列打开"、文件→打开/改名/删除）；`FileView::currentChanged` 重写发 previewChanged；
新类 `PreviewPanel`（目录→QListWidget 子项列表、文件→元数据 + QTextEdit 文本预览≤8KB
非二进制）；Pane 布局 `[pages | preview]`；selectFirstFolder 显式调 open()。

## 4. 效果评估

self-check 全断言为 1（35 个 `=1` 匹配）；38 个二进制全绿；clippy/fmt 干净。
`selectFirstFolder` 显式 `open()` 让 descendForSelfCheck 照常工作；transfer 步骤的
dropBetweenPanes → waitForTransfers 流程不变。**残留**：预览面板不可交互（纯只读，
点击列表里的子项不会导航）；分栏打开后两个 Pane 各有预览面板但共用选中（这是 14 分栏
设计的对称性）。
