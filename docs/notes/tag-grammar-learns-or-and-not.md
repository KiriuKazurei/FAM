# 标签检索语法学了 OR 与排除（增量 30）

## 1. 描述问题

`tag:` 语法只会 AND：多个 `tag:` 词与逗号并列都是"必须全带"。三种真实问题问不出：
"带 a **或** b 的文件"（找照片，jpg 与 raw 都算）；"**排除**某个标签"（找没归档的）；
以及组合（"带 invoice 但**没**带 2026"）。`fam find --all` 的 ANY/ALL 旗标倒是表达过
前两种，但它只活在 CLI，检索语法里没有。

## 2. 分析问题

### 语法：`,` 是 AND、`||` 是 OR、`!` 是排除，全部半角

分隔符选半角 ASCII（`,` `||` `!`），**全角 `，｜｜！` 是标签名的普通字符**——中文标签库
里全角标点本来就出现在名字里（"季度汇总-2024Q1（初稿）"），把它们当语法会把合法标签
变成打不出的词。词法上这就是"只认精确的 ASCII 序列"，与 store 的 BINARY 精确匹配同一
哲学。两种排除拼写都收：`tag:!a`（值内前导）与 `!tag:a`（词首），因为人手两种都会打。

优先级：**逗号绑得比 `||` 紧**——`tag:a,b||c` = (a AND b) OR c。这是 SQL 分组表达的自然
形状（组内成员聚在同一个 CASE 里），也是数据库用户的习惯（AND 优先于 OR）。

### SQL：三段各归其位

- **required（AND）**：进 `HAVING COUNT(DISTINCT CASE WHEN t.tag IN (req) THEN t.tag END) = n`
  ——"每个名字都带着"是按文件去重后的计数断言；
- **OR 组**：每组一条
  `SUM(CASE WHEN t.tag IN (组) THEN 1 ELSE 0 END) >= 1`，组与组之间 HAVING 内 AND——
  "每组至少带一个"，`tag:a||b tag:c||d` = (a||b) AND (c||d)；
- **excluded**：`f.path_norm NOT IN (SELECT path_norm FROM tags WHERE tag IN (...))` ——
  排除是**文件的属性**而非某条标签行的属性，所以过滤文件集而不是挂在 join 条件上；
  未打标签的文件本来就不在候选集里，不会被排除句意外捞进来。

`match_all` 旗标删除：required 天然是 ALL，ANY 由 OR 组表达——原来"同一个布尔管两种
语义"的设计在新语法下没有存在理由。`fam find --all` 保留：`--all` 映射 required，
默认映射单成员 OR 组。

### 排除优先，且空集语义

排除优先于一切包含：带 required 的文件若同时带 excluded 名字即出局。**排除单独成询**
合法——`tag:!a` = "全部带标签的文件中，排除带 a 的"（未打标签的文件本不在候选集，
不会被意外捞入）——`validate` 从"必须有 tags 或 groups"放宽为"三者皆空才拒"。

## 3. 解决方式

- `crates/fam-core/src/search.rs`：`tag_groups`/`tags_not` 字段、值内解析
  （`||` 分组 → 逗号分成员 → `!` 前导排除）、`TAG_NOT_PREFIX`/`TAG_OR`/`TAG_AND` 常量、
  `names_tags`/`is_empty` 覆盖三集合；六条解析测试（含全角不分裂、空成员丢弃）。
- `crates/fam-svc-store`：模型同步三字段（`match_all` 删除）、`build_tag_query`
  （SQL 组装收成纯函数，wanted = required ∪ 组员，NOT IN 子查询，per-group HAVING）、
  validate 放宽到三者非空；测试重写为五段断言（ALL、OR、NOT、required+NOT、
  required+OR 组）。
- 调用方：TagSearch 后端透传（`tag_groups`/`tags_not`）、CLI find 的旗标映射、
  bench 去 `.match_all()`、MCP `fs_search` 的 schema 描述写明语法。

## 4. 效果评估

**正面证据**：cargo 38 个测试二进制全绿（fam-core 六条新解析测试、store 的五段组合
断言）；clippy `-D warnings` 干净；fmt 干净。语法流经 parser → SearchRequest →
TagSearch → store SQL 一条线，MCP 的 `fs_search` 与壳的搜索框自动同得（同一解析器）。

**三处自记**：第一版把单组逗号成员并进 `tags` 平列表，`tag:a,b||c` 会被错写成
`a AND b AND c`——OR 组必须整体建模，测试当场拦下；`wanted` 的借用与 args 的克隆
纠缠出生命周期错，改为 owned；`QDir` 那课的镜像——`next + 1` 对 `&mut usize` 不成立，
`*next + 1` 才是。

**代价**：SQL 组装从线性拼接变为 clauses/having 两段收集（约 70 行纯函数）；`match_all`
删除是一次小破坏（CLI 旗标语义重映射，文档已写）。

**残留**：`||` 与 `,` 不可转义（标签名含字面 `||` 者只能靠筛选栏的 chip 点选）；排除
不参与 OR 组内部（`tag:a||!b` 的 `!b` 落入全局排除——与"排除优先"的语义一致，但不是
"(a) OR (NOT b)"）；OR 组内不支持嵌套。严重度均低。
