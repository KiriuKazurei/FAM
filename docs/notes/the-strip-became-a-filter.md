# 筛选栏成了筛选器：三态 chip、作用域词表、右键排除（增量 31）

## 1. 描述问题

用户要求的筛选栏还差三件事：chip 只显示全库词表（"我在哪"不影响"我看什么"）；点选
只有"含"一个方向（"排除"要打字）；以及 chip 的排除视觉与包含视觉无法区分。

## 2. 分析问题

### 作用域：词表跟目录走

`fam_tag_vocabulary` 答全库；浏览页要的是"**这个目录**里出现的标签"。加
`vocabulary_within(within)`（按子树前缀过滤的 GROUP BY），一条 SQL 与全库版共享形状。
strip 的 `refresh(query, dir)` 拿非空 dir 时走作用域读，空则回落全库——与搜索框的
scope 语义一致（this folder / every root）。strip 在浏览页目录变化时随拍子刷新
（`pollChanges` 比较 deepestPath）。

### 三态：属性 + QSS 状态选择器

chip 用动态属性 `chipState` ∈ {""、include、exclude} 表达状态，样式表按
`[chipState="exclude"]` 出一套**错误色 + 删除线**的规则——排除读作动词（"这些不算"），
不能读作坏掉的 chip。错误色三 token（errorText/errorBg/errorBorder）按 Ant Design 5
的 colorError 家族入 theme，对比度两对新对入表（受门槛 19 对）。

排除色第一版取 red-7 #d4380d——`排除文字/排除底 4.40:1 不过`，换 red-8 #c5221f 后
浅 5.30 / 深 6.64 双双过线。**对比度报告就是测试**：颜色是设计决定，读数是它的验收。

## 3. 解决方式

`fam-svc-store` 的 `vocabulary_within`；ffi `fam_tag_vocabulary_within`（33→34，
`tag.fetch` 心跳族）；bridge `tagVocabularyWithin`；`TagFilterBar` 的 State 枚举 +
`applyChipState`/`stateOf`（属性写 + unpolish/polish 重绘）+ 左键 include 右键 exclude
的 connected 语义 + `applyQuery` 三态推导；theme 的 error 三 token + chip exclude QSS
规则 + 对比对。

## 4. 效果评估

light/dark 全断言为 1（对比度 19 对 0 不过：浅 5.30 / 深 6.64）；cargo 38 个二进制全绿；
clippy/fmt 干净；abi-smoke ok；model-smoke ok。**残留**：include/exclude 的可视对（含/
除）在排除态的 hover 上只描边变色——文字保持删除线，这个可以再看。
