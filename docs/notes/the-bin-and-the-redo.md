# 删除进回收站，撤销有了另一半（增量 27/28）

## 1. 描述问题

两个增量各自账上的最后一条残留：

- **23/24 的"删除无回收站"**：键盘删除走 `remove_path` 硬删除——一个人按 Delete 丢掉的
  文件找不回来，与每个人对 Delete 的期待相反。状态行的两段式确认挡住了误按，但挡不住
  "删完才想起还要"。
- **25 的"无重做"**：Ctrl+Z 之后想反悔（"我撤销错了"），只能把同样的传输再做一遍——
  而撤销的逆信息本就在栈里。

## 2. 分析问题

### 回收站：一句 QDesktopServices 的距离

Qt 6.11 的 `QFile::moveToTrash(fileName, &pathInTrash)`（`supportsMoveToTrash()` 可问）
在 Windows 上走回收站，文件与目录通吃——不需要自写 `SHFileOperation` 包装。设计上
**只改人的键盘删除，不动 agent 的 `fs_delete`**：人的 Delete 进回收站（可恢复），agent
的硬删除契约原样（显式、按名拒绝、无隐性副本）——"同一句话"的原则让位于两边风险
不对称的现实，分歧写进代码注释与账。

两个断言层面的细节：句子里报的是**人删的那个路径**，不是回收站给的内部分配地址
（断言按原名找，第一版找了 bin 内地址，`removed-sentence=0` 一步读出）；打开的成功
也要写状态行（此前成功无声，无头断言无处落脚）。

### 重做：对称栈，前进传输

撤销栈的另一半： undone 的条目进 `m_redoable`，Ctrl+Shift+Z / Ctrl+Y（`QKeySequence::Redo`
两者都匹配）重跑**前进传输**——移动回退的重做就是再移过去，复制删除的重做就是从源
重新复制（源在撤销期间被改过的话，重做带的是**当前**字节——与人手工重新复制一遍
携带的相同）。完成条目回 `m_undoable`，失败/取消回 `m_redoable`；**完成分支绝不推
新条目**，否则 Ctrl+Z 撤销撤销、Ctrl+Shift+Z 重做重做，重做语义从后门漏进来。一次
新的 drop **截断重做半边**（线性历史的老规矩：回到过去的世界里行动，那些被撤销的
未来就没了），撤销半边不动。

## 3. 解决方式

- `apps/fam-shell/src/mainwindow.{h,cpp}`：`m_redoable` 栈 + `redoLastTransfer()`；
  删除改 `QFile::moveToTrash`（句式两处更新：武装句说"进回收站"，完成句报原路径）；
  `PendingTransfer` 加 `redo` 标志与 undo 并列，poll 的三个分支按两标志分岔；新 drop
  清空 `m_redoable`；keyPressEvent 加 `QKeySequence::Redo`。
- self-check：删除断言改读 "recycle bin" 句子 + 磁盘消失；undo 步扩展 redo 回合——
  两发真 Ctrl+Shift+Z 后磁盘回到两次 drop 刚完成的世界（`move=1 copy=1`）。

## 4. 效果评估

**证据**（fresh 树，light/dark 两遍一致）：

```
self-check file commands on ...: rename: applied=1 old-gone=1 back=1 restored=1 error=[]
open: sent=1 status=[opened ... with the platform] delete: armed=1 removed-sentence=1 gone=1
self-check undo: copy-undone=1 (origin=1 copy=0) move-undone=1 (origin=0 dest=1)
    redo: move=1 (origin=1 dest=0) copy=1 (both=1)
```

`move=1 copy=1` 的四个磁盘断言合起来才是 undo/redo 对称的证据：撤销后文件在回退端、
重做后文件回到前进端、副本重新出现在两端。删除的 `removed-sentence=1 gone=1`——句子
与磁盘同说一件事。

**核心侧**：38 个测试二进制全绿；clippy/fmt 干净。**壳侧**：light/dark 退出 0，对比度
18 对全过；abi-smoke 连跑两遍 ok；model-smoke ok。

**两处自记**：第一版删除句报了 bin 内地址，按原名断言的读数 `removed-sentence=0`
一步定位——句子该用人删的名字；步骤的删除轮放在改名轮之后时，改名引发的两次
reconcile 会让第二次按键的行路径不稳定（重武装分支）——每轮前重选行是结构性修法。

**代价**：两个栈的内存（会话内条目数）；新 drop 清空 redo 半边（线性历史的标准代价）。

**残留**：回收站的目录支持依赖 Qt 的 Windows 实现（本步只实测了文件；目录进 bin 未
单独断言）；撤销/重做的语义只覆盖传输（标签备注的删除仍靠确认挡）。
