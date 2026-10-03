# 齿轮打开了标签的账本（增量 32）

## 1. 描述问题

用户要求的最后一块：标签管理页——查看所有标签、编辑管理、当前目录优先。筛选栏的
齿轮按钮是入口。

## 2. 分析问题

### 管理页是标签系统三个功能的收口

增量 30 给了 OR/排除的检索语法；31 给了筛选栏三态与作用域词表。管理页收口的是**按
名字的编辑**：改名（30 的 merge 在目标缺席时即改名）、合并（同一操作，目标存在时即
合并）、删除（按名全库删——新 store 操作 `delete_tags(name)`，一条 DELETE 带 validate，
ffi `fam_tag_delete` 33→34）。这些操作一直在后端，管理页是把它们**送到人手边**的表面。

### 当前目录优先：两个读，一个列表

当前目录词表（31 的 `vocabulary_within`）先列出，库级词表补上其余（已在作用域内的不
重复列）。列表条目是**显示字符串**（`name · N — in this directory`），操作用的**原始
名字**另存一份（`m_rawNames`）——选中返回原始名，显示返回显示串。这个分离在第一版
缺失：`selectedName()` 返回显示串传给 store，merge 静默失败，`renamed=0` 一步读出。

## 3. 解决方式

`tagmanagerdialog.{h,cpp}`（新）——列表 + 底部输入行 + Rename/merge + Delete（两段式）
+ Close；齿轮按钮（`tagGear`，32px 固定，strip 滚动区外）打开它；self-check 的
`tagManagerForSelfCheck` 驱动公开操作。

## 4. 效果评估

light/dark 全断言为 1（对比度 19 对 0 不过）；38 个二进制全绿；clippy/fmt 干净；
abi-smoke ok；model-smoke ok。**自记**：显示串与原始名的分离是第一个版本的缺陷——
选中返回了 `click-me · 1 — in this directory` 传给 merge，静默零行，`renamed=0` 一步
读出。**残留**：删除是硬删（从所有文件移除名字），与 chip 的排除（可逆的查询状态）
不同层；管理页的重命名/合并没有撤销（30 的 merge_tags 本身就是逆操作：反向 merge
即回）。
