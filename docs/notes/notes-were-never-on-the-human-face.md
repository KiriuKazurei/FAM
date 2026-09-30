# 备注从未在人这一面存在过（增量 18）

## 1. 描述问题

用增量 16 的那把尺子去量"库里有、人这面没有"，量出来的是备注（notes）——而且比标签
当时更彻底：

- `notes` 表在 schema v2 就有了（增量 5 时代），`set_note`/`note`/`clear_note` 在
  `crates/fam-svc-store/src/notes.rs` 里语义完备：8 KiB 上限、空体拒绝、随 `files` 行级联；
- agent 那一面在增量 12 就接上了：`note_get`/`note_set`/`note_clear` 三个 MCP 工具，
  凭据、门、审计齐全；
- **ABI 上 0 个入口**（`fam_note_*` 不存在，这一点与标签不同——`fam_tag_list` 好歹一直
  在 ABI 上，备注连过界的路都没铺）；bridge 0 处；界面 0 处。

合起来一句话：**agent 能给文件写备注，人永远看不见**——和"标签写进去之后再也没出现"
（16 号账）是同一句话，只是写的人换到了另一面。

## 2. 分析问题

### 缺的是三段线，不是设计

store 的语义已经把界面能做什么定死了：

- **空体拒绝而非清除**。`set_note` 对空白体返回 `EmptyNote`，理由写在 store 里："a tool
  that deletes when it was handed nothing is a tool that loses a note over one dropped
  argument"。所以对话框的 Delete 手势必须是显式的 `Clear`，而 Save 空文本要在**本地**
  用 store 的同一句话拒绝——界面上说 store 的话，与 16 让"超长标签的拒绝"原话抵达
  界面是同一条规矩。
- **字节上限 8 KiB，按 UTF-8 数**。编辑器在 tooltip 里说清楚上限，本地先量再发——
  9216 字节的拒绝句是本地拼的，数字与 store 会算的一致。
- **`clear_note` 返回"是否真有"**。"清掉了"与"本来就没有"是两个事实，对话框把第二次
  Clear 说成 "there was no note to clear" 而不是假装又清了一次。
- **备注不可搜索是特性不是缺陷**。正文从不进索引，`note.get` 按确切路径是唯一的读回路
  ——"agent 看见过什么"可以只从审计回答。界面不提供任何按内容找备注的路，这一轮不做
  任何搜索入口。

### 表面放在哪：自己的对话框，不是标签对话框的又一页

备注与标签在 store 里是两张表，界面上也是两个对话框：标签对话框按名字列（一个名字两个
来源合并显示），备注对话框是一段自由文本——两者要的控件、操作、确认方式都不同。共用一
个"详情对话框"是把两种生命周期塞进一个关闭按钮，这次不做。工具栏上 `Note…` 挨着
`Tags…`，选中行的语义（没选行时回落到当前列的文件夹）与 `Tags…` 完全一致。

主题是 16 的教训直接应用：对话框 objectName 上 `page`，编辑器是 `QTextEdit`——sheet 的
QLineEdit 规则够不到它，所以有自己的 `QTextEdit#noteEditor` 规则（同 token：容器底、
`border` 发丝线、焦点 `primary`）。文字/容器的对比度对已在受门槛表里，无新对。

## 3. 解决方式

- `crates/fam-svc-audit/src/record.rs`：`Op::NoteGet`/`NoteSet`/`NoteClear`
  （`note.get`/`note.set`/`note.clear`，Ask 族——一次人动作一条线，不归拍子）。
- `crates/fam-ffi/src/lib.rs`：`fam_note_get` / `fam_note_set` / `fam_note_clear`
  （`#[no_mangle]` 25→28；`note.get` 答体与时间，`{"body":null}` 表示没有）；
  `fam-ffi/tests/ffi.rs` 一条过线测试钉住三件事——没有时答 null 不是错、过线往返、
  **空写被拒且原备注还在**。
- `apps/fam-shell`：`famcore.h` 三个声明；`FamBridge::noteGet/noteSet/noteClear`；
  `notedialog.{h,cpp}`（新）——编辑器 + Save/Clear/Close，操作全是公开方法
  （`saveNote`/`clearNote`/`reload`/`editorText`/`setEditorText`/`storedNote`/`changed`），
  模态之外无头可驱动的缝与标签对话框同一条；`MainWindow::noteSelectedFile()` 槽 +
  `Note…` 按钮。
- `apps/fam-cli`：`fam note get|set|clear`——三条命令与 agent 的三个工具答同一套句子
  （CLI 此前也没有 note，这次补齐与人手面的对称）。
- self-check：`notesForSelfCheck()`（空起 → 写 → 读回 → **空写被拒且原文还在** →
  超限被拒 → 清 → 再清如实报没有 → 终态空）与 `grabNoteDialogForSelfCheck()`
  （带内容的对话框截图，`x.note.png`，light/dark 各一）。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（fresh 树，light 与 dark 两遍逐字一致）：

```
self-check notes on fam-self-check-probe.txt: started-empty=1 save=1 stored=1 blank-refused=1 ("a note must say something; use Clear to remove one") over-limit-refused=1 ("a note on fam-self-check-probe.txt would be 9216 bytes; the limit is 8192") clear=1 clear-again=0 final-empty=1
```

- `blank-refused=1` 后面跟着存储里的原文仍在（`clear=1` 之前那段 `stored=1` 读回的是
  写入内容；空写发生在其后，拒的是本地句子）——拒绝不是清除的化装；
- `over-limit-refused=1 (9216 … 8192)`——本地算的 UTF-8 字节数与 store 的墙一致；
- `clear-again=0`——第二次 Clear 如实说"没有可清的"，这是 0 的意思，不是失败；
- 截图两遍看过：light/dark 下编辑器、Save 主色、"last written …" 行都与各自窗口同底色。

**核心侧**：`cargo test --workspace` 38 个二进制全绿（新增 ffi 过线一条、CLI 一条）；
clippy `-D warnings` 干净；fmt 干净。

**壳侧**：light/dark 两遍退出 0，`icons 21/21/21 pending=0`、`ipc retries=0`，对比度
"受门槛的 16 对里 0 对不过"，17d 的四步标签断言原样全绿；`abi-smoke` 连跑两遍 `-> ok`，
`model-smoke` ok。`famcore.dll` 的导出表 25→28（`fam_note_*` 三条），`FAM_ABI_VERSION`
仍为 2——加法不闸旧行布局。

### 自记一处

`clear-again=0` 的读法容易读反：这一行是**期望为 0** 的断言（"第二次清除报告没有"），
不是失败计数。日志行是给人读的账，0 在这里的意思写在 note 里而不是靠猜。

### 代价

- 三个 ABI 入口、两个源文件（`notedialog.{h,cpp}` 约 190 行）、一次模态交互（与标签
  对话框同款）。
- 写一条备注是一次 `record_file` upsert + 一次 upsert，未单独计时（同 store 写路径，
  标签的地板族已覆盖同类 SQL 形状）。
- 打开对话框是一次同步 `note.get`。

### 残留风险与局限（这次没解决什么、为什么）

1. **行上不显示"有备注"的标记**。哪些文件带备注看不出来，要一个个点开。需要批量读
   （`notes_for_paths`）+ 行画法上的一个记号 + 缓存与拍子——是 17b 那一整套的第二遍，
   属于独立的增量。严重度：中。
2. **备注不可搜索**（按设计，两者都不是）：内容不进索引，没有按内容找备注的路。
   什么时候做，要先回答"搜出来的备注按什么显示"。严重度：低（有意）。
3. **`fam note set --body` 在命令行上写多行文本不便**（`--body` 一个参数）；
   从文件读入（`--file`）是自然的补全。严重度：低。
4. **键盘路径未测**：`QTextEdit` 里 Return 是换行而不是保存，保存的快捷键（Ctrl+S）尚
   未绑定——没有绑定就没有可断言的键盘路径，这条留给绑定那天。严重度：低。
