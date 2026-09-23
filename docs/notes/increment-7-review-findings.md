# 增量 7 之后的复查：修了四处、驳回零条、七处确认不修

- 增量：7 复查（图标层落地后按缺陷重读一遍，不改计划）
- 提交：`fix(shell)` + 本文件
- 涉及：`apps/fam-shell/{src/iconcache.{h,cpp}, src/mainwindow.cpp, src/main.cpp}`

## 1. 描述问题

做法与增量 6 那次一样两条并行：自己逐行读新层的每一条返回路径（谁拿了 `HICON`、谁在什么线程上
碰了 Qt 的 GUI 对象、锁在哪里闭、有没有声明了没人接的线），同时派一个只读 agent 独立审同一批文件，
问题按四类问：**线程与 COM 正确性 / 资源泄漏 / 接线（写了没接、接错对象）/ 渲染**。
纪律照旧：**agent 报的每一条都要读到那一行才算数**。

结果：**四处确认并当场修**，**驳回零条**（agent 在三个类别里报"none"，我自己读下来同意），
**七处确认不修**（写清代价与归属）。另外 agent 提了一条**不是缺陷但是约束**的东西，记在最后。

## 2. 分析问题

### 2.1 修掉的四处

1. **第二层的键没有上限，请求表和图标表会随目录长大**（我自己读出来的，不是 agent 报的）。
   第一层的键是扩展名，全机也就几十种；**第二层的键是一行一个 `path`**，而几万子目录的文件夹正是
   这个壳的设计目标（78k 行）。原实现把上限压在 `m_icons` 上（512），但 `m_requested` 才是先长起来的
   那个：每个未命中的键都进它，且它没有上限。真跑到 512 之前，`m_requested` 已经装了同样多的键，
   而每个解析成功的键还额外持有一份 `QIcon`——**每份是一个设备侧 pixmap**，几万个就是几十 MB。
   改法：上限压到**请求那一侧**（`iconFor` 里 4096），撞到就把两张表一起清空。清空只花一次视口的
   重解析，长下去赔的是整个会话。
2. **`resolveByExtension` 没检查 `SHGetFileInfoW` 的返回值**（agent 报的，读到 `iconcache.cpp:66-68`
   确认）。隔壁 `resolveByPath` 查了（`:97-99`），这里没查——不对称本身就是信号。后果不是崩：失败时
   `info.iIcon` 保持 0，而 `GetIcon(0)` **会成功**，返回图像列表里的第一张图，于是"一个 shell 认不出来的
   名字"被缓存成一张**错的** glyph，且因为 `iconFor` 命中就不再重试，错到进程结束。修法：返回 0 就
   `return {}`——留占位符，不留假答案。
3. **一段注释在说代码没做的事，而且说的正是这次实测否掉的那件事**（agent 报的）。
   `scrollForSelfCheck` 上面留着一段旧注释写"Four stops…processEvents 让每个停点真的绘制"，
   而下面的循环是九停点、并且**"processEvents 就够"这个主张是这次实测被推翻的**（只 processEvents
   时九停点折叠成一次绘制，4 个键；加 `repaint()` 才是 9 个键）。留着它比没有注释更坏：它教下一个人
   用那个不管用的写法。删掉，只留与循环一致的那一段。
4. **闸门自己有个洞：`extracted == destroyed` 在有活没干完时是合法的不等**。
   计数是 `++extracted` 在取到 `HICON` 之后、`++destroyed` 在 `DestroyIcon` 之后（同一函数里，中间
   无提前返回），所以**静止时**必然相等；但自检是在风暴之后固定 1 秒读的，没有"worker 空闲"的证据，
   于是 `extracted > destroyed` 可能只是撞上了一次正在解析——这条读数撑不起"无泄漏"这个结论。
   修法：`IconWorker::pending()`（`m_lock` 改 `mutable` 以保持访问器 const），自检多打一个
   `pending=0`。现在的日志是 `extracted=9 destroyed=9 cached=9 pending=0`——**四个数一起才构成闸门**。

### 2.2 agent 报"none"、我自己也同意的三类

- **线程与 COM**：`IconWorker` 是 `IconCache` 的**值成员**，`IconCache` 在 GUI 线程构造，所以
  worker 对象的线程亲和在 GUI 线程、`run()` 在自己的线程——`emit resolved(…)` 因此是**队列连接**，
  `store()`（造 `QPixmap`/`QIcon`）落在 GUI 线程。`qRegisterMetaType` 在 `start()` 之前。
  锁在 `run()` 里于 emit 之前就已闭。`QImage` 是唯一跨边界的像素容器。
- **泄漏**：两条拿 `HICON` 的路径都在下一行销毁；`imageFromIcon` 里 `dib == nullptr || bits == nullptr`
  那个提前返回不可能漏 HBITMAP（`CreateDIBSection` 不会返回一个位为空的句柄）；`run()` 只有一个出口，
  三个 `Release()` 与 `CoUninitialize()` 都在它之后。
- **接线**：`extracted/destroyed/cached/pending` 都有消费者（自检日志）；`setIconCache` 只被调一次，
  没有重复连接；`abi-smoke` 只编 `bridge.cpp`，新字段是 POD、新文件不在它的源列表里，
  新链接库只加在 `fam-shell` 上——这一点我另外用真跑确认：`abi-smoke` 退出 0。

## 3. 解决方式

| # | 落点 | 做法 |
| --- | --- | --- |
| 1 | `iconcache.cpp` `iconFor` | 上限从 `m_icons`(512) 移到 `m_requested`(4096)，撞到清空两张表；`store()` 里那段 512 删掉 |
| 2 | `iconcache.cpp` `resolveByExtension` | `SHGetFileInfoW(...) == 0` 直接 `return {}`，理由写在注释里 |
| 3 | `mainwindow.cpp` `scrollForSelfCheck` | 删掉与九停点矛盾、且主张已被实测否掉的旧注释块 |
| 4 | `iconcache.{h,cpp}`、`main.cpp` | `IconWorker::pending()` + `IconCache::pending()`，自检日志加 `pending=%d` |

## 4. 解决方式效果评估

**正面证据（真机）。**

- 三道 `--self-check` 全部 `pending=0` 且提取=销毁：126 行树 `extracted=9 destroyed=9 cached=9`，
  搜索 11 行 `1/1/1`，3 行树 `1/1/1`。截图核对过图标仍在（`.txt` 文档 glyph、`.png` 图像 glyph、
  目录文件夹 glyph），说明第 2 处加的返回值检查没把真答案挡掉。
- Rust 侧这一笔零改动：337 条测试、clippy `--all-targets` 零告警、fmt 干净、
  `cargo check -p fam-svc-index --target aarch64-unknown-linux-musl` 通过。
- 顺带记一笔**这次差点踩的坑**：我一开始把 musl 检查扩到 `-p fam-ffi`，它失败在
  `libsqlite3-sys` 的 build script（没有 aarch64-musl 的 C 编译器），**不是代码问题**；
  文档化的跨平台检查从来只有 `fam-svc-index`，别把工具链的限制读成缺陷。

**确认存在、这次不修的（按严重度排，标归属）。**

1. **关闭可能阻塞**：`~IconCache` 里 `m_worker.wait()` 会等第二次层的一次真实路径解析跑完；
   网络命名空间上 `SHGetFileInfoW` 可以很久。今天 `--watch` 的都是本地盘，代价看不到。
   归属：真要把网络路径列成一等公民时处理（增量 8 之外）。
2. **`SizeHintRole` 的约束（agent 提的，我核对成立但今天不是缺陷）**：`QListView` 在
   `setUniformItemSizes(true)` 下按一行量所有行，今天占位符与真图标都是 16px 逻辑尺寸，
   替换不改行高。**增量 8 一旦用 `SHIL_EXTRALARGE`(256)/48px 大图标，行会被裁**，必须同时上
   `SizeHintRole` 或改 `iconSize`。写在这里是为了让它别被当成图标层"已经做完"。
3. **第二层的三条正向分支没有样本**：`.lnk` 要 COM 才造得出、`.library-ms` 同理、
   `desktop.ini` 自定义图标的目录我没建；跑过的只有"探针说没有 → 退回第一层"这条负路径，
   以及 `.url`（纯文本 INI）作为真实路径分支的替身。叠图标（`SHGFI_ADDOVERLAYS`）没有样本
   （本机没有共享/链接目标）。
4. **驱动器根那条键几乎打不到**：目录列举里不会出现名为 `C:\` 的行，它只在增量 8 的驱动器侧栏
   里才成为活路径。留着是因为计划点名了它，删掉更诚实的替代是等侧栏一起做——**归属增量 8**。
5. **4096 上限的清空路径没测过**，也不打算为测它造一万个子目录。
6. **图标解析延迟没有单独数字**：风暴落在 1.5s→2.5s 窗口里，抓图时刻只证明"解析完了"。
   要数字得给 worker 加计时，留给真有人嫌慢的时候。
7. **顺带重读了上一笔（6 复查欠账 #1，`FLAG_LOSS` 按视图基线）**，撞出一个我原本说反了的地方：
   `refresh` 把一个"在读开始之后、读完成之前打开"的视图的基线**压低**到读开始时的计数，于是它可能
   为一次**发生在它打开之前**的丢事件报警。方向是**多报**而不是漏报（漏报才是那只闩的病），
   且下一次成功重读就纠正，所以不修；记在这里免得下次把它当 bug 找。
