# 事件只给一个名字，不给"它是谁"

- 增量：4（`fam-sched` 目录分组）
- 提交：本次提交（前序：`a09ac32` 词汇表上收与三骨架、`683f1fd` 懒目录缓存）
- 涉及：`crates/fam-sched/src/{pipeline,batch}.rs`、`crates/fam-sched/tests/watch.rs`、
  依赖 `notify-8.2.0/src/windows.rs`

## 1. 描述问题

缓存按目录持有一张一级表，所以一条事件要能回答两个问题才能落到表上：

1. 它属于**哪张表**（父目录是谁）；
2. 那个名字是**目录还是文件**（决定图标、能不能展开、以及删掉一个名字时要不要连带丢掉子表）。

监视器给的是：一个路径、一个动作。就这些。`notify-8.2.0/src/windows.rs` 的实际映射：

| 位置 | 事实 |
| --- | --- |
| `:388-398` | 每条记录只带一个名字，路径是 `request.data.dir.join(name)` 现拼的 |
| `:438-447` | `FILE_ACTION_ADDED → CreateKind::Any`、`FILE_ACTION_REMOVED → RemoveKind::Any` |
| `:448-452` | `FILE_ACTION_MODIFIED → ModifyKind::Any`（数据改、属性改、写时间改，全都并成一种） |
| `:425-437` | 改名是两条独立记录：`RENAMED_OLD_NAME → Modify(Name(From))`、`RENAMED_NEW_NAME → Modify(Name(To))`，**没有任何关联标识** |

两个缺口都在这里：`Any` 系意味着 `Create(File)` 与 `Create(Folder)` 无从区分；两条改名记录之间
没有 cookie，谁跟谁一对只能靠猜。旧实现的消费者根本不切 `Remove`/`Modify(Name)`，所以这两个缺口
在旧栈里没暴露过——它是直接丢删除与改名（§4.2）。

## 2. 分析问题

**"属于哪张表"这件事，事件里没有，但我们手里有。** 监视是以目录为单位挂的，
`Event.paths[0]` 必然以当初那个 `dir` 开头。于是有两条路：

- **落表时再算父目录**：每次打补丁都要 `PathBuf::parent()` + 一次 `PathKey::of`（UTF-16 折叠）+
  把名字重新编码成 `Vec<u16>` 才能进 arena。
- **在翻译处一次算清**：`translate()` 本来就拿着完整路径，顺手把 `dir`（`PathKey`）与
  `name`（原样 UTF-16 单元）算出来带走。

选后者。理由不只是省一次重算：`fam-svc-index` 的表本来就是 UTF-16 arena，
带一个 `PathBuf` 过去意味着每个补丁都要重走一遍"字符串 → 身份"的路，而那条路正是
[pathkey-identity-fold.md](pathkey-identity-fold.md) 花了一整篇钉死的东西。**在同一处把身份算出来，
就不给第二处算错的机会。**

**改名不要配对。** 三条候选：

1. *相邻配对*：把 `From` 与紧随其后的 `To` 合成一条改名。**否决**——队列是有限且**会丢**的
   （`pipeline.rs:138-147`），`From` 到手、`To` 被丢是常态路径而不是异常路径，那样就会凭空
   制造一次"文件消失了"的删除。
2. *只报 `To`*：漏掉旧名，表里留一条僵尸行。**否决**。
3. *两条各报各的*：`From → Removed`、`To → Created`。**采用**。每一侧单独看都是真话；
   同名路径在一个窗口里先删后建，`merge()` 已经把它读成"更新"
   （`batch.rs:131-138`），跨名改名正好是表需要的两条补丁。代价是"这是一次移动"这个信息
   在管道里丢了（见 §4 残留）。

**`Target` 不许猜。** 给 `Unknown` 挑一个默认值（比如 `File`）在功能上"能用"，但错的代价不对称：
把目录当文件，图标错、不能展开、删它时不会连带丢子表；反过来把文件当目录，会给它挂一个永远
不会有内容的监视。所以类型里保留 `Unknown`，并且 `target_of()` 仍然认真匹配
`Create(Folder)`/`Remove(Folder)` 等具体种类——**如果哪天换个会报的后端，就照单全收**，
不把"Windows 后端不报"这件事写成全局假设（测试
`a_backend_that_names_the_target_is_believed`）。谁来消解 `Unknown`：消费者用手里的表
（这一行本来就有 `ItemFlags::DIRECTORY`），或者一次针对性的属性查询。

**没有父表的那种路径要单独交代。** 盘根（`D:\`）的父目录按 `PathKey::parent()` 是它自己，
它不属于任何一张一级表。`FileChange::with_target` 用 `filter(|p| p != &key)` 把它归成
`dir = None`（`batch.rs:81`），`by_directory()` 明确跳过而不是硬塞给某个目录当补丁。

## 3. 解决方式

`crates/fam-sched/src/batch.rs:59-69`：

```rust
pub struct FileChange {
    pub path: PathBuf,          // v1 ABI 与 CLI 仍按完整路径显示
    pub change: Change,
    pub dir: Option<PathKey>,   // 要补丁的那张表；盘根为 None
    pub name: Vec<u16>,         // 原样 UTF-16，直接进 arena
    pub target: Target,         // Directory | File | Unknown
}
```

`pipeline.rs:170-176` 的 `target_of()` 只把后端**明确**报出的种类当真，其余一律 `Unknown`；
`translate()`（`:185-215`）把 `Create/Remove/Modify(Name)/Modify(_)` 映射成四种 `Change`，
`RenameMode::Both` 那种"一条事件两个路径"的后端也按前删后建展开。

`Batch::by_directory()`（`batch.rs:169-203`）把窗口重分布成"每张表一次"：
`DirDelta{dir, rows, rescan}`，并按 `PathKey` 定序（同一批每次出来顺序一致，UI 才能省掉排序）。
合并仍在 `merge()` 里按路径发生，分组只是搬运，`by_directory()` 不再二次合并。

## 4. 解决方式效果评估

**正面证据。** `crates/fam-sched/src/batch.rs`：
`a_change_carries_the_directory_and_the_verbatim_name`（父目录身份 + 原样 UTF-16 名字）、
`a_drive_root_has_no_parent_table`（盘根为 `None`）、
`a_later_observation_can_resolve_the_target`（`Unknown` 可被后来的明确观测补上，且不会反过来被
`Unknown` 冲掉）、`a_batch_groups_rows_by_the_directory_they_belong_to`；
`crates/fam-sched/src/pipeline.rs`：`rename_becomes_removal_plus_creation`、
`a_backend_that_names_the_target_is_believed`、`every_semantic_of_spec_four_has_a_mapping`；
端到端：`tests/watch.rs::batch_rows_arrive_grouped_by_the_directory_they_belong_to`
与 `a_hundred_and_twenty_single_level_watches_all_report`（120 个单层监视并发上报，逐个不缺席）。

**代价。**

- 每条行信号带三份名字表示：`PathBuf` + `Vec<u16>` + `PathKey`。一个 30 字符的名字约 60 字节 ×2
  （UTF-16 与 key）外加 `PathBuf` 本体，即**每条约 200 字节、三次分配**。折算的宽松上界：
  一次 8000 文件的真实突发（1624 create/s）里 8143 条全部通过 `translate()` 且队列为 65536
  时 `dropped=0`（见姊妹篇），说明单条成本远低于 615µs 的到达间隔——这个界很松，
  只够说明"翻译不是瓶颈"，不是一幅单价图。
- `Target::Unknown` 把问题推给了消费者。这是有意的，但增量 5 必须真的处理它：
  若届时"表里没有这个名"又不去查属性，就会给新建的文件夹挂上文件图标。
- `by_directory()` 每次调用重新分组（不缓存），一次批量一遍 `HashMap` + 一次排序。窗口最多
  `max_batch = 256` 条，未单独计时。

**残留风险与局限（没解决什么）。**

1. **"这是一次移动"这个信息永久丢失。** 表侧因此做不出"行从 A 位挪到 B 位"的动画，只能
   删一行加一行。不解决的理由：配对需要跨事件的关联标识，后端没给；在会丢的队列上猜相邻，
   错一次的代价是伪造一次删除。
2. **数据改 / 属性改 / 写时间改仍是同一种 `Modified`**（`windows.rs:448-452` 就并成一条）。
   对本次用途够用（三种都可能改变行上显示的大小或时间），但若将来要做"只有属性变了就只刷一格"，
   需要绕过 notify 自己读原始 `Action`。
3. Windows 事件里 `Target` **恒为 `Unknown`**，本增量没有任何一处把它就地解出来——
   解它的活儿在增量 5 的表侧，那里才拿得住"这一行原本是什么"。
4. `dir` 与 `name` 与 `path` 三者可以互相推出，代码里没有断言这一点（构造处一次算全）。
   若增量 5 出现手工构造的 `FileChange`，需要考虑把构造收窄成唯一入口。
