# 标签在人这一面看得见、删得掉（增量 16）

## 1. 描述问题

用户的话是"先把功能完善了再考虑打包的事情"。动手前先量功能面上到底缺什么，量出来的第一条：

- `fam_tag_list` 与 `fam_tag_remove` **在 ABI 上**（`crates/fam-ffi/src/lib.rs` 那 22 个
  `#[no_mangle]` 里），**在 bridge 上**（`apps/fam-shell/src/bridge.cpp:106` 与 `:114`），
  **在界面里调用者为 0**。
- `fam_find`（按标签搜）同样：ABI 有、`bridge.cpp:125` 包好了、界面里一次都没调。
- 界面碰标签只有一处：`mainwindow.cpp` 里 `tagAdd`。旧形状是
  `QInputDialog::getText("Add tag", "tag for <path>")` → 写一个词 → 状态行 `tagged X`。

合起来是一句不好听的话：**标签写进去之后，在这个产品里再也没有任何地方出现，也没有任何地方能去掉**。
`tagList` 返回的数组、`tagRemove` 的删除、`find` 的检索，人这一面一个都用不上；
能用上的只有 agent 那一面（`tags_get` / `tags_set` / `tags_remove` / 没有 find-by-tag 工具，但有 `fs_search`）。

还有一个附带事实，是量出来才发现的：`remove_tags(path, names, source)`
（`crates/fam-svc-store/src/store.rs:176`）拼的 SQL 是——

```rust
let mut sql = String::from("DELETE FROM tags WHERE path_norm = ?1");
if let Some(source) = source { ... }        // source 为 None 就没有这一句
if !tags.is_empty() { ... " AND tag IN (?)" ... }   // 名单为空也没有这一句
```

`FamBridge::tagRemove` 传的正是 `source = nullptr`。于是：

1. 删一个名字会**同时**删掉 manual 与 auto 两条同名记录；
2. 名单为空时，这条语句是 `DELETE FROM tags WHERE path_norm = ?1` —— **删掉这个文件的全部标签**。

第 2 条决定了新界面不能有的行为：什么都没选，绝不能被翻译成"删空的名单"发出去。

## 2. 分析问题

### 三块候选，按"线已经铺到哪一步"排，这次只做第一块

用户选的是"标签在人这一面接通"。接通包含三件事，成本完全不同：

| 要做的事 | 缺什么 | 这次 |
| --- | --- | --- |
| 选中项的标签能看、能加、能删 | 什么都不缺，只缺界面调用者 | **做** |
| 标签画在每一行上 | 缺一个批量 ABI 入口 + 每列一份标签缓存 | 不做，见下 |
| 按标签搜 | 缺一个答案面（不是缺线） | 不做，见下 |

**行上显示标签，先量了代价再否决。** `crates/fam-bench/src/measure.rs:195` 与 `:209` 两条新 case，
同一个 store、同样 128 行窗口（128 = 壳的 `FamListModel::m_viewport`，不是 harness 自己的 `VIEWPORT` 60）、
每行两条标签，`errors` 字段在这里当自检用——两边都必须真把 256 条标签取回来才算数：

```
tags_per_row   p50 10150us  p95 14437us  min  7510us  entries 128  errors 0  58671ns/entry
tags_batch     p50  1437us  p95  2582us  min  1194us  entries 128  errors 0   9328ns/entry
```

6.3 倍。但**否决它的不是这个倍数**，是增量 7 定下的那条规矩：一次绘制不许去查任何东西
（图标层为此专门做了"扩展名→系统图像列表、绝不开路径"）。
每行一次查询是 7.5ms 落在绘制路径上，改成批量是 1.2ms 落在绘制路径上——**两个都不该在那条路径上**。
所以那一半真正需要的东西是：一个批量 ABI 入口，加一份**每列的标签缓存**，在 250ms 那一拍里填，
绘制只读表。那是一次独立的改动，不是把调用点换个写法。

**按标签搜缺的不是线，是答案面。** `find` 返回的是 `[{path, tags:[…]}]` —— 没有 size、没有 mtime、
没有 kind，甚至不知道这条是不是目录。而 `FamListModel` 的行是 `FamViewRow{name, path, size,
modifiedMs, kind, directory, attrs}`：硬塞进去，`size = 0` 会让每个被标签过的文件在行上印 `0 B`，
`kind` 只能填一个词表里不存在的位置。增量 13 刚把"每个文件夹都印 0 B"当成缺陷修掉，
这次不能自己写回去。所以它要自己的表面（一份"路径 + 它的标签"的列表），那是另一次设计。

### 显示形状是被 wire 语义定的，不是审美

因为删一个名字会连带删掉所有来源的同名记录，列表**按名字合并**、把来源写在名字后面
（`tagdialog.cpp:27` 的 `groupByName`）：

```
contract  (manual, auto)
```

如果按 (tag, source) 一行一条列出来，就等于承诺了一个不存在的控制——"这一行只删 manual 那个"。
`removeTag` 因此收的是**名字**，不是行号。

### 为什么对话框的操作是公开方法

`TagDialog` 是模态的（`exec()`），而模态对话框正是"没有手的运行"唯一驱动不了的东西。
所以三个操作 `addTag` / `removeTag` / `tagNames` 是公开方法，`reload()` 也是，
槽函数只做"从控件读参数、把错误写进提示标签"这一层薄壳。
这样无头那一步（`mainwindow.cpp:700`）能走**同一份代码**，而不是走一条为测试另搭的旁路。

顺带解决了一个更实际的问题：`removeTag(name)` 自己带空名单守卫，
所以"没选行"这个状态在**操作层**就被挡住了，不依赖有没有人选了行。

### 一个新表面漏掉了主题

第一张对话框截图（`--theme light` 那一遍）：对话框底色是**深色**，列表是白的，
标题文字是灰的——一个浅色的主窗口开出一个不属于任何主题的对话框。
原因很普通：样式表按 objectName 匹配，而 `QDialog` 没有 objectName，
于是它落回 Fusion 的调色板，也就是**桌面的**配色方案。

`tagdialog.cpp:60` 加了 `setObjectName(Theme::pageName())`。
两遍截图（light、dark）现在都和各自的窗口一致。

这条值得留着的原因是它的**发现方式**：同一次运行里所有关于线的断言都是绿的。
计数、IPC、对比度表、标签往返——没有一个能看出样式表没生效。
只有把那张图打开看了一眼才知道。

## 3. 解决方式

### `apps/fam-shell/src/tagdialog.{h,cpp}`（新）

- `TagDialog(FamBridge *, path, parent)`：标题栏路径、`QListWidget` 一行一个标签名、
  输入框 + `Add`（`primaryButton`）+ `Remove selected`、`Close`。
- `QStringList tagNames() const`（`:99`）：上一次 `reload()` 读到的名字，**与控件无关**——
  没显示过的对话框答得和显示过的一样。
- `bool addTag(tag, error*)`：自己先挡空串，其余交给 store 的句子
  （过长、首尾空格这些是 store 的规则，界面上看到的是同一句话，与 agent 收到的那条一致）。
- `bool removeTag(name, error*)`（`:159`）：`name.trimmed().isEmpty()` 直接拒绝并给一句
  "pick a tag to remove first"——**不发**那个会清空全部标签的调用。
- `reload()`：`tagList` → `groupByName` → 同时填 `m_names`（操作用的）与 `m_display`（给人看的）。

### 接进窗口

`mainwindow.cpp:525`：`Tag selected` 按钮改名 `Tags…`（带 tooltip 说明它管的是选中行**或那一列的文件夹**），
槽里换成开对话框；关掉之后如果 `changed()`，状态行报的是**结束状态**（`N tag(s) on <文件>`）
而不是"tagged X"——因为一次会话里可能加了也可能删了。

### 无头两步

- `tagsForSelfCheck()`（`:700`）：在探针文件上跑一遍读→加→读回→**试删空**→删→读，
  外加一次故意超长的标签。
- `grabTagDialogForSelfCheck(path)`（`:735`）：先播一个标签进去、`show()` + `processEvents()`
  让布局真的算过、抓图、再把播进去的删掉。
  截图命名成主截图的同级 `x.tags.png`，不是 `x.png.tags.png`。

### 两条 bench case

`crates/fam-bench/src/measure.rs:195/:209` + 共用的 `measure_tags(rounds, rows, per_row)`
（一个函数两种形状，保证两个数出自同一份数据）。地板写进 `bench/baseline.json`。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（`--self-check-mutate`，light 那一遍）：

```
self-check tags on fam-self-check-probe.txt: started=[] add=1 listed=[self-check] remove-nothing=0 still=[self-check]
self-check tags: over-long tag refused=1 ("invalid tag "xxxx…": tag longer than 64 characters") remove=1 final=[]
self-check wrote the tag dialog to …\drop-light.tags.png: seeded=1 listed=1 saved
```

- `listed=[self-check]` —— 标签**读得回来**了，这是"看不见"那一半被修好的地方；
- `remove-nothing=0` 之后 `still=[self-check]` —— 拒绝**且**标签还在。只看返回值不算证据，
  清空发生在返回之后也一样是 0；证明它没发出去的是后面那份名单；
- 超长标签被拒的是 store 的原话，不是界面自己编的；
- `final=[]` —— 回到开始的状态，这一步不留残留标签。

**两张截图都看过**：light 与 dark 下对话框都与各自窗口同底色，
`self-check-shot  (manual)` 一行可读，`Add` 是主色，输入框占位文案在两级灰下都够对比。

**核心侧**：`cargo test --workspace` → `passed=499 failed=0 ignored=2`；
`cargo clippy --workspace --all-targets -- -D warnings` 干净；`cargo fmt --all -- --check` 干净
（第一版我新写的 bench 代码不合格，`cargo fmt` 改的就是那一处）。

**壳侧**：light（带 mutate）与 dark 两遍退出 0；
`icons: extracted=18 destroyed=18 cached=18 pending=0`；
`ipc: headers=11 pages=19 polls=78 retries=0 total=108`（headers 从 10 涨到 11，就是那一次 `tagList`）；
对比度两遍都是"受门槛的 13 对里 0 对不过"；14 那三条落放证据原样还在。
`abi-smoke` 五条 `-> ok`，`model-smoke` 六条 `MODEL_SMOKE=0`。

**新 case 的地板**（实测，本机 debug 构建）：`tags_per_row` min 7510µs、`tags_batch` min 1194µs。

### 三处自记

1. **完整 bench 闸门这次没重跑**（跑到一半被中断）。理由与限度都写清楚：这次 Rust 侧只改
   `fam-bench`（两条新 case + 一条 `fam-svc-store` 依赖），没有触碰任何被测代码路径；
   已有九条地板一个都没重录。所以"没跑"丢的是**再次确认环境没变**这一层，不是"回归被放过"。
2. **地板写了三次才落**。第一次 `--write` 抓到的是忙时的数（min 8048µs），
   而几分钟前同一份代码观察到 7215µs；连写三次落在 7510µs。
   这条 case 的机器散布实测在 ±10% 上下，**地板比最好观测宽了 4%**——
   一个太宽的地板什么都不闸，这一点记在这里而不是假装它不存在。
3. **一个"读代码就能说"的结论我没有直接写**，而是去量了：
   标签删干净之后 `files` 表那行还在。CLI 两次 `stats`：
   `tag add` 之后 `files: 2 / tags: 1`，`tag remove` 之后 `files: 2 / tags: 0`。
   归因清楚（1→2 是我加的那条路径，2→2 是它留下的）。

### 代价

- 多两个源文件（`tagdialog.{h,cpp}`，186 行）与一次模态交互。
  **这是这个壳第一次有模态对话框**，也因此第一次出现"无头闸门不能靠点鼠标走"的界面——
  解法是把操作做成可直调的方法，而不是给测试开后门。
- 打开对话框是一次同步的 `tagList`（本机一次调用，未单独计时）。
- `Tags…` 按钮在没有任何列打开时也能点，此时它管的是**当前目录**（`selectedPath()` 的既有回落语义），
  标题栏会写清是哪个路径。

### 残留风险与局限（这次没解决什么、为什么）

1. **标签仍然不画在行上**——一列里哪些文件被标过、标了几个，看不出来。
   需要批量 ABI 入口 + 每列缓存，代价已量（见第 2 节）。严重度：**中**（这是"接通"剩下的那一半）。
   归属：下一次做标签可见性时一起。
2. **按标签搜没有答案面**——`find` 在 ABI 与 bridge 上都就绪，界面仍无调用者。
   缺的是"路径 + 标签"这种行的显示形状，不是缺线。严重度：**中**。归属：与 1 同一次或其后。
3. **`files` 表的一行永不回收**（实测）。给一个路径打过标签再全删，`files` 行留着；
   每次无头跑也留一行。它让"再打一次标签"便宜，代价是库随**曾经被标注过的路径数**增长。
   严重度：低。归属：store 的清理策略（要先回答"路径没了算不算删掉它的备注与标签"）。
4. **改标签名做不到**：`rename_tag`（`store.rs:227`）在 store 里有，**不在 ABI 上**，
   所以人和 agent 都改不了名，只能删了再加。严重度：低。归属：与 1 同一次 ABI 追加。
5. **删标签没有确认也没有撤销**。一次一个名字、界面上看得见名字，且 `auto` 来源会被连带删掉这件事
   在行上写着——但点错就是点错了。严重度：低。归属：撤销那一层（14 的账里它是"中"）。
6. **`auto` 来源在壳里无法产生**（没有索引器写标签的路径），所以"一个名字两个来源"这一行
   只被**读**的逻辑覆盖，没被真实数据验证过。要验得先有一条 auto 标签。
   严重度：低（覆盖缺口，不是已知错误）。
7. **对话框的键盘路径没测**：Enter 绑到 `add`，但没验证过 Tab 序、默认按钮与 Esc。
   没测就不写结论。严重度：低。归属：下一次碰这个对话框时补一步无头断言。
