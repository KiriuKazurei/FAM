# 属性列表头：Name/Size/Type 标签条（增量 34）

## 1. 描述问题

用户要求文件列表像资源管理器的详细信息视图——每个属性（大小、类型、标签）有列头。
当前 RowDelegate 已经画了这些字段（右对齐），但没有列头告诉人"这一列是什么"。

## 2. 分析问题

每列加一条薄属性标签条（Name 左对齐、Size/Type 右对齐），插在路径/排序头与文件列表
之间。标签位置与 delegate 的右对齐字段大致匹配——字号用 `fontSM`，色用 `textSecondary`，
背景用 `fillAlter`，底部 `borderSecondary` 分隔线。

Tags 不设列标签：标签带的位置随文件动态变化（`+N` 折叠、让位），固定标签头会与实际
位置错位。chip 本身就是标签。

## 3. 解决方式

`ColumnChain::addColumn` 里内联创建 `propHeader`（QWidget + QHBoxLayout，Name 左对齐 +
Size/Type 右对齐 fixed width），objectName `columnProp` / `propLabel`，theme QSS 规则
（fillAlter 背景、textSecondary 色、borderSecondary 底线）。

## 4. 效果评估

self-check 全断言为 1；38 个二进制全绿；clippy/fmt 干净。**残留**：属性头与 delegate
的字段对齐是近似的（delegate 的动态布局让字段位置随列宽变化）；标签列无固定头。
