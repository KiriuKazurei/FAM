# 对账把读者的那一行搬走了：一次"改了"被报成"走了又来一行"

增量 14d。缺陷不是 14d 写出来的，但 14d 是第一个**会往屏幕上那个文件夹里写东西**的功能，
于是它第一次被踩到。

## 1. 描述问题

`FamListModel::reconcile()` 把核心重读回来的表与屏幕上那一窗行做差，只宣布真正不同的部分。
差的做法是：按位置找公共前缀、公共后缀，中间那段当作"走了 `gone` 行、来了 `arrived` 行"，
各发一对 `beginRemoveRows` / `beginInsertRows`。

前缀与后缀的判据是 `sameRow()` —— 名字、路径、大小、时间、kind、attrs、是否目录**全等**。
于是一个只是变了的行（同一份文件、同一个文件夹，大小或 mtime 换了）落不进前缀，
被划进中间那段，报成"这一行没了，新的一行出现了"。

被报成"没了"的那一行，如果正好是视图当前的那一条，`QItemSelectionModel` 会把当前索引
挪到**下一行**。而 `ColumnChain::onCurrentChanged()` 对当前索引变化的唯一解释是"有人点了它"，
于是它照着新那一行开一列。

现象（第一次跑 `--self-check-mutate`，`apps/fam-shell/src/columnchain.cpp:411` 那一行是我为了定位刚加的）：

```
info: self-check set up the transfer: left depth 1 -> 2 on ...\tree\2024-合同, right on ...\tree
info: self-check moved by drop from ...\tree\2024-合同 to ...\tree: left depth=2, right depth=1, ...
info: column C:\...\tree: current row 1 (was 0, 4 listed), photos is a folder
info: self-check copied by drop from ...\tree to ...\tree\photos: left depth=3, right depth=1, ...
```

第三步没人点过任何东西：`--self-check-mutate` 的时间线里那时只有轮询在跑。
左栏从"停在 `2024-合同`"变成"停在 `photos`"，链深从 2 变 3，
于是下一步的拖放目标也跟着从计划中的 `2024-合同` 变成 `photos`。

同一份代码在修复后：

```
info: self-check moved by drop from ...\tree\2024-合同 to ...\tree: left depth=2, right depth=1, ...
info: column C:\...\tree reconciled in 1 pair(s) + 1 modified row(s), 5 row(s), selection kept
info: self-check copied by drop from ...\tree to ...\tree\2024-合同: left depth=2, right depth=1, ...
```

`current row` 那一行不再出现，链深保持 2，落点回到计划里的那个文件夹。

## 2. 分析问题

### 为什么这是错的，而不只是"多花了一次信号"

`beginRemoveRows` 与 `dataChanged` 不是同一句话的两种写法。前者是**行的身份发生了变化**：
视图会按 Qt 的规则给当前选择找一个替代者，被删掉的那一行的下一行就成了新的当前行。
后者是**同一行换了内容**：视图只重画，不动选择。

把"改了"报成"走了又来"，代价不是性能，是**读者的位置被换掉了**。而在 Miller 列里，
当前行变了就等于导航变了 —— 这一列右边凭空多出一列，谁都没点过。

危害的方向要说清楚：它不是"看起来怪"，它会改变后续每一次用户操作的作用对象。
`deepestPath()` 同时是拖放目标、`this folder` 的搜索作用域、以及 `Tag selected` 的落点。
上面那段日志里，拖放目标就是被它改掉的 —— 一次"只是文件夹长大了"的重读，
把下一步要写的目录换成了另一个目录。

### 影响面（实测计数）

`apps/fam-shell/tools/model_smoke.cpp` 把模型接在一个真 `QListView` 上，
连上 `rowsAboutToBeRemoved` / `rowsAboutToBeInserted` / `dataChanged`，
把当前索引放到指定行，重读一次，然后看信号与当前行落点。
**修复前**（把两个扫描的判据退回 `sameRow`）：

```
  grown folder + new file: no removal        FAILED  pairs=2/1 removals=1[0,0] inserts=1 changes=0[-1,-1] modified=0 rows=5 current=2 on photos
  grown folder alone: no pairs               FAILED  pairs=2/0 removals=1[0,0] inserts=1 changes=0[-1,-1] modified=0 rows=4 current=1 on photos
  deleted file: one removal                  ok      pairs=1/1 removals=1[1,1] inserts=0 changes=0[-1,-1] modified=0 rows=3 current=0 on 2024-contract
  renamed file: removal + insertion          ok      pairs=2/2 removals=1[2,2] inserts=1 changes=0[-1,-1] modified=0 rows=4 current=0 on 2024-contract
  two rows grew side by side: one range      FAILED  pairs=2/0 removals=1[1,2] inserts=1 changes=0[-1,-1] modified=0 rows=4 current=0 on 2024-contract
  reader on the changed row, new row above   FAILED  pairs=2/1 removals=1[0,1] inserts=1 changes=0[-1,-1] modified=0 rows=5 current=3 on README.txt
model-smoke: FAILED                            (MODEL_SMOKE=1)
```

四条红，两条绿。两条绿的正是**阴性对照**：真的删除与真的改名，修复前后都各花一对，
说明这次改动没有把删除走的通道吞掉。

`grown folder alone` 是最小复现：只有一行的 size 变了，别的什么都没动，
读者的当前行从 0 落到 1（`current=1 on photos`）。

**修复后**六条全绿（`MODEL_SMOKE=0`），同一批 case 的数字：

```
  grown folder + new file: no removal        ok  pairs=1/1 removals=0[-1,-1] inserts=1 changes=1[0,0] modified=1 rows=5 current=0 on 2024-contract
  grown folder alone: no pairs               ok  pairs=0/0 removals=0[-1,-1] inserts=0 changes=1[0,0] modified=1 rows=4 current=0 on 2024-contract
  two rows grew side by side: one range      ok  pairs=0/0 removals=0[-1,-1] inserts=0 changes=1[1,2] modified=2 rows=4 current=0 on 2024-contract
  reader on the changed row, new row above   ok  pairs=1/1 removals=0[-1,-1] inserts=1 changes=1[2,2] modified=1 rows=5 current=2 on photos
```

第二行是这次的核心数字：**一对信号都不花的重读**（`pairs=0/0`），
而它以前是两对。第四行说明"当前行号会变"与"当前行会丢"是两回事：
上面插进来一行，读者的行号从 1 变成 2，**他站的还是 `photos`** —— 这是对的；
以前他会站到 `README.txt` 上。

### 排除掉的候选

- **在 `onCurrentChanged` 里判断"这次移动是不是程序干的"**：`currentChanged` 不带原因，
  只能靠"我自己有没有调 `setCurrentIndex`"的旗标，而这里的移动是 `QItemSelectionModel`
  内部做的，没人调过。放弃：修不了因，只能压住果。
- **`reconcile()` 前后保存并还原当前索引**（按行号，或按 `pathAt()` 找回来）：
  能把读者放回去，但那一窗行仍然被"删了又加"，视图要重新布局；
  而且按行号找回会踩到同一行的重排，按路径找回要在 78k 行的目录里扫窗口外的行。
  放弃：它在治症状，代价落在真正的病灶之外。
- **把 `sameRow` 里去掉 `size` 与 `modifiedMs`**：能让文件夹长大不再算差异，
  但屏幕上就会留着一个旧的大小 —— 那是 8a-1 明确不要的（"一个只是被改过的文件不是同一行"）。
  放弃：用一个假陈述换掉一个真问题。
- **只给目录行开特例**（目录的 size/mtime 不参与比较）：目录的大小本来就是聚合值，
  但"这一行的时间变了"在按时间排序的列里必须重画。放弃：同一类错误换个马甲。

## 3. 解决方式

把两个问题分开问。扫描对齐用的是**身份**，宣布时用的是**内容**。

`apps/fam-shell/src/famlistmodel.cpp:293` 新增判据：

```cpp
bool FamListModel::sameItem(const FamViewRow &left, const FamViewRow &right) {
    return left.name == right.name && left.path == right.path;
}
```

前缀与后缀扫描改成按 `sameItem` 对齐，扫过的每一行再问一次"内容一样吗"，
不一样的位置记进 `modified`（`famlistmodel.cpp:337`）：

```cpp
    int prefix = 0;
    QVector<int> modified;
    while (prefix < before.size() && prefix < after.size() &&
           sameItem(before.at(prefix), after.at(prefix))) {
        if (!sameRow(before.at(prefix), after.at(prefix))) {
            modified.append(prefix);
        }
        ++prefix;
    }
```

后缀那一圈同理，记的是它在 `after` 里的位置。中间段的 `gone` / `arrived` 算术不变，
所以真的删除、真的改名照旧走成对信号。

宣布放在表已经换好之后（`famlistmodel.cpp:408`）：

```cpp
    std::sort(modified.begin(), modified.end());
    for (int at = 0; at < modified.size();) {
        int run = 1;
        while (at + run < modified.size() && modified.at(at + run) == modified.at(at) + run) {
            ++run;
        }
        emit dataChanged(index(modified.at(at)), index(modified.at(at) + run - 1));
        at += run;
    }
    outcome.modified = modified.size();
```

顺序是有意的：`dataChanged` 会立刻让视图回来取数据，所以表必须先换完再发。
相邻的位置合成一段，30 个文件同时长大是 1 个信号而不是 30 个。

`Reconciled` 多一个字段（`famlistmodel.h:85`），轮询那行日志跟着说两句
（`columnchain.cpp:288`）：

```
column %s reconciled in %d pair(s) + %d modified row(s), %u row(s), %s
```

`pairs` 与 `modified` 是两个不同的开销，不合并成一个数：前者是视图要重新布局的行段，
后者只是要重画的行。

`columnchain.cpp:411` 那条 `current row %d (was %d, %d listed)` 是**永久**留下的插桩。
理由写在注释里：链形状每一次变化都是这一行的选择造成的，
"读者自己往下走了一行"与"一次重读把当前索引重新落在了别的东西上"事后看起来一样，
只有 `(was N)` 能分开它们。第一次跑就靠这一行定位到本缺陷。

## 4. 解决方式效果评估

### 正面证据

- `model-smoke`（新工具，`apps/fam-shell/tools/model_smoke.cpp`，CMake 目标同名）六条全绿，
  `MODEL_SMOKE=0`；其中两条是阴性对照（真删除、真改名各花一对，修复前后一样）。
- **A/B 已做**：把两个扫描的判据从 `sameItem` 退回 `sameRow`，重编译，
  `MODEL_SMOKE=1` 且四条红（上面第 2 节贴的就是那一次的输出）；
  改回来后 `md5sum -c` 对 `apps/fam-shell/src/famlistmodel.cpp` 返回 `OK`
  （`e83c541ce8519523ca00993bc63f8171`），重编译，`MODEL_SMOKE=0`。
- 壳的无头闸门两遍都过：`--self-check-mutate`（light）退出 0，
  日志里 `current row` 只剩两条合法的（都写着 `was -1`，即由 `selectFirstFolder` 与
  `descendForSelfCheck` 主动设的），拖放三步的 `left depth` 全程 2；
  dark 一遍退出 0。图标 `extracted=18 destroyed=18 cached=18 pending=0`，
  `ipc ... retries=0 total=107`。
- 核心未改：`cargo test --workspace` 499 passed / 0 failed / 2 ignored，
  `cargo clippy --workspace --all-targets -- -D warnings` 干净，`cargo fmt --all -- --check` 干净，
  `abi-smoke` 五条 `insert=1 cut=1 move=1 copy=1 refuse-overwrite=1 -> ok`。
- 对账通道仍然在工作：同一次跑里 `column ...\tree cut 1 position(s) in 1 pair(s), 4 row(s) left`
  照旧出现（那是剪裁通道，不是这次改的差分段）。

### 代价

- **每行的比较多了两次字符串比较**：同身份的行现在要先过 `sameItem`（name、path）再过 `sameRow`
  （7 项，前两项与 `sameItem` 重复）。上限是每列每次 resync ≤128 行 × 2 次字符串比较
  （一窗就是 `m_viewport = 128`）。**未实测**：这段没有 bench 面，
  而 `model-smoke` 的计时会被进程启动噪声吃掉。
- `Reconciled` 多一个字段、日志句子变长，`docs/notes/watched-folders-never-showed-new-files.md:99`
  那句 `reconciled in 1 pair(s) ... selection kept` 从此在新代码里可能是 `0 pair(s) + 1 modified row(s)`。
  那篇 note 的数字是当时的真实观测，不改它；改的是它旁边那句结论能撑多远（见下）。
- 多一个需要维护的不变式：`modified` 里的位置是 `after` 坐标，
  发信号前必须先把 `m_rows` 换成 `after`。顺序写反不会崩，只会重画出旧内容 —— 属于"错了不容易发现"那一类。

### 收回一句话

`8a-1` 那篇里 `selection kept` 想说的是"没重建整屏"，代码也确实只保证了这一件事
（`Reconciled::rebuilt == false`）。但这句话被日志读成了"读者的位置保住了"，
而它当时并不成立：视图没被重建，当前索引照样会被一次"删除+插入"搬走。
**"没重建"与"位置没动"是两件事**，从此由两个字段分别作证：`pairs`（动了行段）与 `modified`（只重画）。

### 残留风险与局限（这次不修什么）

1. **改名仍然会把读者的行搬走**：改名是身份变了，`sameItem` 判不相等，照旧走成对信号，
   当前行如果被删掉仍会挪到下一行。没有诚实的"同一行"可保 —— 那一行已经不是那份文件了。
   严重度：低（改名在打开的目录里比改大小少见得多）。归属：真按身份保选择时再说。
2. **身份匹配是按位置的**：一次重读同时改了内容和排序时，
   同一位置可能对着不同身份的行，扫描当场停住，退回成对信号 —— 与修复前行为一致，
   不是新洞，但也意味着这次修的不是"任意修改都不动选择"，而是"原位修改不动选择"。未实测发生率。
3. **`onCurrentChanged` 仍然把任何当前索引变化当作一次点击**。这次移除了最常见的那个原因，
   假设本身没动。更强的做法是区分程序性移动（发信号方带旗标），没做；
   现在它至少会在日志里留下 `(was N)` 与 `N listed`，复发时看得见而不是静默。
   归属：列链的导航语义。
4. **拖放落点仍取"该列当前显示的文件夹"，不取光标下那一行**：落在文件夹行上不会改目标。
   与用户确认过的形状一致（"从一列拖到另一列"），列进 not-fixed 清单见增量 14 的账。
