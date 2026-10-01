# 键盘终于会编辑了：删除、改名、打开（增量 23/24）

## 1. 描述问题

全项目审计（22 后）里"从未入账的常规功能"第一条：**壳内没有文件删除与重命名**——删文件
只有 agent 的 `fs_delete`，人在壳里既没有 Delete 键也没有 F2；双击一个文件什么也不发生。
一个"应对基础编辑"的资源管理器，键盘上唯一会做的编辑动作是打标签。

原语其实全都在：`remove_path`（20 收进 fam-svc-fs）、`fam_path_op`（14 的移动/改名）、
`QDesktopServices`。缺的是把键盘和它们接起来的那一段。

## 2. 分析问题

### 键盘事件住在视图里，语义决定在窗口里

Delete/F2/Enter 的按键事件到达的是**有焦点的视图**（`FileView`），而"删谁、改成什么、
打开之后归平台"的决定在窗口。所以视图只回答"键按在哪一行"（`deleteRequested(path)` /
`renameRequested(path)` / `openRequested(path)` 三个信号沿既有中继链上达），窗口决定
做什么——与 chip 点击（`tagActivated`）同一条缝。文件夹永远不发 `openRequested`：它的
打开是选中本身（单击即开列，8b 起的形状），双击文件夹没有新事实。

### 删除的确认与改名的对话框

删除是**硬删除**（无回收站），所以两段式：第一下在状态行写明"press Delete again to
remove X - this cannot be undone"，第二下才删；换了路径重新武装，Esc 解除。确认住在
状态行而不是弹窗，与 17d 的两段式同款——`QMessageBox` 会是又一张没吃主题的表面。

改名是一次移动（保目录换名字，"永不覆盖"是 OK 按钮撞上的规则），所以打开一个一行
输入的小对话框（`RenameDialog`，`pageName` 主题、公开的 `applyName` 供无头驱动），
成功后**进撤销栈**——改名就是移动，栈里本就有它的位置。

### 两个自伤的坑，都是"路径的语义"这一课

1. `QDir(path).absoluteFilePath(name)`——`QDir` 构造自**文件**路径时把文件当目录，拼出
   `probe.txt\renamed-probe.txt`，os error 3。正确形状是 `QFileInfo(path).dir()`（父目录）。
   对话框同病同治。这一课是 pathkey 折叠那笔账的镜像：路径的成分语义（文件还是目录）
   不在类型里，只能靠命名和纪律。
2. **步骤的顺序**：打开会把文件交给平台（本机拉起记事本），而 `ShellExecute` 内部泵事件
   循环——会让后续定时器提前开火（清理步清了 `m_probe`，断言读到的是清理后的世界）。
   修法是把慢操作放到步骤最后，并把步骤挪到 drive pick 之前（它的视图指针贯穿整步，
   链在中途被清就是 use-after-free——日志在 drive pick 后戛然而止的那次就是这么来的）。

## 3. 解决方式

- `apps/fam-shell/src/fileview.{h,cpp}`：`keyPressEvent`（Delete/F2/Enter）与
  `mouseDoubleClickEvent`（文件行→`openRequested`；文件夹不发，单击已开），三个信号沿
  列链/窗格中继上达。
- `apps/fam-shell/src/renamedialog.{h,cpp}`（新）：一行输入 + Rename/Cancel，
  `applyName` 走 `pathOp`；本地拒绝空名与"名字没变"，其余是传输原语的句子。
- `apps/fam-shell/src/mainwindow.{h,cpp}`：`deleteRequested` 两段式（武装/执行/换路径
  重武装/Esc 解除）、`renameRequested` 开对话框并在成功后入撤销栈、`openRequested` 走
  `QDesktopServices`（返回 false 说"没有关联认领它"）。
- self-check：`fileCommandsForSelfCheck`——**真的键事件发给视图**（选中探针行、
  `setFocus`），顺序为改名（去→回，磁盘断言）→ 打开（日志记录平台交接）→ 删除（武装
  断言读 `m_deleteArmed`/`m_pendingDelete`，删除断言读状态行的 "removed" 句子——文件
  存在性会被同一步的删除污染，句子不会）。时间线上该步在 drive pick 之前，探针清理
  再让位。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（fresh 树，light 与 dark 两遍一致）：

```
open ...\tree\fam-self-check-probe.txt: handed to the platform
self-check file commands on fam-self-check-probe.txt: delete: armed=1 removed-sentence=1
rename: applied=1 old-gone=1 back=1 restored=1 error=[] open: sent=1
```

`applied=1 old-gone=1 back=1 restored=1`——改名去、旧名消失、改回、文件回来，两次移动
两次撤销条目；`armed=1 removed-sentence=1`——第一下武装（状态行的句子在）、第二下删除
（"removed"句子在）。

**核心侧**：38 个测试二进制全绿；clippy/fmt 干净。**壳侧**：light/dark 退出 0，对比度
18 对全过；abi-smoke 连跑两遍 ok；model-smoke ok。

### 三处自记

1. **`QDir` 构造自文件路径**的成分错误（见第 2 节）——错误句子（os error 3）一步读出。
2. **步骤顺序与 ShellExecute 的内部泵**：open 放最后的决定是在第一次试跑（日志在
   drive pick 后截断、探针被清理步提前删掉）之后做的——事件循环内部的泵会让"还没到
   时间的定时器"提前跑，这一课 20 号账记过一次（6100 的 drive pick），这次是同一枚
   硬币的背面。
3. **本机 .txt 无关联**：`openUrl` 对探针答 false——本机的 .txt 关联在这台机器上指向
   不可用的处理。句子照实说（"no application is associated with"），这本身是 open 句子
   的验证；平台上真实打开的那次（"handed to the platform"）拉起了记事本，是 mutate 模式
   在开发机上运行的一次可见副作用，记档。

### 代价与残留

- RenameDialog 约 120 行、视图键盘约 60 行、窗口三处理器约 90 行。
- **删除无回收站**：`remove_path` 是硬删除；回收站语义（`SHFileOperation` 的
  `FOF_ALLOWUNDO`）是另一条原语，agent 的 `fs_delete` 与人撞的仍是同一句"按名拒绝"。
  严重度：低（两段式确认挡在前面）。
- **打开的平台半段不可无头断言**：`openUrl` 的返回与日志行是证据，平台做了什么不在
  观察范围内。严重度：低。
- 重命名对话框的 F2→exec 路径无头驱动不了（操作公开可直调，同 16 的先例）。
