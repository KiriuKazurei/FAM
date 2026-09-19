# 遗留规格存档（legacy specs）

本文件是删除 Go / Java / Electron / C++ 四套旧栈**之前**的规格提取。这些数值和语义只存在于代码里，删除后本文件是唯一出处。

## 0. 恢复锚点（先读这一节）

| 锚点 | 指向 | 用途 |
|---|---|---|
| `archive/pre-rust-migration` | `57db887` | 旧栈的**上一个已提交**状态。**不是**引用基准 |
| `84342b5` | 迁移前完整工作区快照 | **本文件所有行号以它为基准** |

两者相差 81 个文件、+525/−835 行，其中包含 10 个从未提交过的 Java 文件，以及 `core/imageutil/thumbnail.go`、`services/file/Qt_file_watcher.go`、`src/.../version/diff/DiffAnalyzer.java`、`Design/后端结构树Java`、`.vscode/settings.json` 这 5 个文件的未提交改动。

```bash
git show 84342b5:<路径>          # 取回单个文件
git ls-tree -r --name-only 84342b5 | grep <关键词>
```

⚠️ 若误用 `archive/pre-rust-migration` 取回，会**静默丢掉**上述未提交工作 —— 本文件正文中所有 `文件:行号` 均指 `84342b5` 版本。

## 范围与不做提取清单

本项目已转为**通用资源管理器**：全量名称索引与搜索交由 **Everything SDK** 承担；图片/缩略图/RAW/AI 推理**已划出范围**（由另一项目承担）。

**不予提取**（需要时从 `84342b5` 取回）：`core/imageutil/*`（缩略图并发池、libraw 解析）、`Design/代码辅助器.md` 的 ONNX 与 RAW 章节、`Design/sqlDesign.md` 的 AI 置信度人工审核章节、`services/version/*`（CAS 快照与 diff）、`services/gateway/*`（单进程架构不需要网关层）。

---

## 1. 标签数据模型

### 1.0 可信度前提（重要）

`core/tag` **整个包从未编译通过**：`new_filetagger.go:4` 写的是 `import "FAM/core/core_config"`，而实际目录是 `core/config`（`core_config` 只是包名，不是路径）。实测报 `package FAM/core/core_config is not in std`。

同时全仓**没有任何 `main` 包**（唯一的 `func main` 在 `services/tag/storage.go:125`，位于 `package tag` 内，是永不执行的示例）。两个 `config.json` 也都按相对路径读取，且仓库根不存在 `config.json` —— 即 `core_config` 的 `init()` 会在任何导入它的二进制启动瞬间 `log.Fatalf`。

**结论：下面所有 CRUD 契约都是「未运行过的代码意图」，不是「验证过的行为」。** 重写时应把它们当作需求描述，逐条重新设计并测试，尤其 §1.3 那 4 条已知缺陷。

### 1.1 表结构（`core/tag/new_filetagger.go:16-22`）

GORM 结构体 `TagEntry`，无 `TableName()` 覆写 ⇒ 实际表名 **`tag_entries`**：

| 列 | 类型 | 约束 | 语义 |
|---|---|---|---|
| `id` | int | PRIMARY KEY | 自增 |
| `file_path` | text | NOT NULL, `index`（**非 uniqueIndex**） | 文件路径 |
| `tag` | varchar(255) | NOT NULL | 标签内容 |
| `source` | — | `default:'manual'` | 来源标识 |
| `created_at` | time.Time | autoCreateTime | 创建时间 |

关于 `source`：
- 取值约定为 `manual` / `AI`，但**代码完全不校验**，任意字符串甚至空串都能写入。
- GORM 对带 `default` 标签的**零值字段不写入** ⇒ 传空串实际落库为 `'manual'`。这个隐式行为要显式决策。

`PRAGMA encoding='UTF-8'`（`:35`）在 SQLite 中不是运行时可设置项，属无效语句，不必照搬。
数据库路径取自 `core_config.Get("database.path", "tags.db")`（`:29`），源码自带 `TODO: 要指向正确地址`。

### 1.2 CRUD 行为契约

| 操作 | 行为 | 出处 |
|---|---|---|
| AddTags | `(filePath, tags[], source)`；显式事务；逐个校验，任一非法**整体回滚**；消息 `"为 %s 添加了 %d 个标签"` | `:52-81` |
| DeleteTags | `(filePath, tags[], source)`；`tags` 空 ⇒ 删该文件全部；`source` 空 ⇒ 不限来源；返回 `RowsAffected` | `:96-116` |
| ListTags | `(filePath, source)`；只取标签列 | `:130-145` |
| UpdateTag | `(filePath, oldTag, newTag, newSource)`；新值为空表示不改该字段；**`RowsAffected==0` 视为错误**并回滚 | `:161-195` |
| SearchTags | `(tag, source, searchDir)`；`searchDir` 空则用配置 `files_dir`，默认 `"./"` | `:210-242` |

目录前缀过滤的两个细节要保留语义：比较前双方都做 `filepath.Clean`，且给前缀补分隔符 `filepath.Join(cleanSearchDir, "")`（`:227-228`），以避免 `/foo/bar` 误匹配 `/foo/baz`。

### 1.3 已知缺陷（重写时必须决策，勿照搬）

1. **无唯一约束** —— `(file_path, tag)` 上没有唯一索引，`AddTags` 直接 `Create` 无查重 ⇒ 重复添加同一标签会产生多行。
   仓库里**已有正确范式可抄**：`services/index_service.go:45` 的 `path TEXT NOT NULL UNIQUE` 配 `:72` 的 `ON CONFLICT(path) DO UPDATE SET`，是全仓唯一的幂等写入实现。
2. **目录过滤未下推** —— `tag = ?` 与 `source = ?` 已下推（`:216-219`），但目录前缀是取回整行后在应用层用 `strings.HasPrefix` 筛（`:231-239`），且 `Find(&entries)` 拉了整行却只用 `FilePath` 一列。
3. **中文标签长度陷阱** —— `isValidTag` 是 `len(tag) <= 64` 且 `len()` 返回 **UTF-8 字节数**，汉字 3 字节 ⇒ 实际约 **21 个汉字**封顶（22 字即 66B 超限），而非 64。Rust 侧若改用 `chars().count()` 会静默放宽限制，须明确选哪种。
4. **合法字符集** —— `^[a-zA-Z0-9一-龥 ！？，。、]+$`（`:248`）。允许：ASCII 字母数字、汉字、**空格**、全角 `！？，、。`；拒绝：`-` `_` `.` `/` `#`、半角标点、emoji。
5. **接口与实现脱节** —— `core/tag/ITagger.go:15-21` 声明的签名与实际函数不一致（`AddTags` 少 `source` 参数、`SearchTags` 不返回 `error`），包内无任何类型实现该接口。它是「原本想要的 API 契约」的唯一出处，重写时值得参考而非照抄现状。

### 1.4 第二套标签模型（容易漏掉，含关键语义）

`services/tag/` 是与 `core/tag` **并存的另一套**标签设计，数据模型不同：

- `storage.go:12-17`：`Tag{ID, ImageID, TagName, Priority}`，裸 `sql.DB` + `image_id` 外键（`core/tag` 用 `file_path`，这套用 `image_id`），另有 `:20` 起的 `tags` 表。**同一文件里还有一个 `func main()`**（`:125`）属自测死代码。
- `priority.go:9-13`：`PriorityManager` 维护 `map[filepath]map[tag]int`，即「每文件每标签一个整数优先级」。
- `search.go:36-54` `GetTags`：**先输出 manual 标签、再输出非 manual 标签**（按 `isManual` 分两轮遍历）—— 这是「人工标签优先于自动/AI 标签」的排序语义唯一成文处。`SearchByTags`（`:57-81`）为命中即返回式检索。

新架构里标签仍需「来源 + 优先级 + 展示排序」三件事，**这套语义不在 `core/tag` 里**，因此必须记录。

---

## 2. 遍历与监控参数

Everything 只索引**本地固定 NTFS 卷**，exFAT/FAT/网络盘在 1.4.1 上不被索引 ⇒ `fam-svc-walk` 兜底路径必须存在。以下是它的历史取值。

| 参数 | 值 | 出处 |
|---|---|---|
| 扫描超时 | 30 秒 | `global_config.go:10` |
| worker pool 容量 | 10（原硬编码 100 后改配置读取） | `scanner.go:18-24`、`global_config/config.json` |
| 并发模型 | `filepath.Walk` **串行**遍历，命中即起 goroutine，信号量限流，`Mutex` 保护结果切片 | `scanner.go:111-159` |
| 监控目录 | `"/监控目录"`（占位，该目录不存在） | `global_config.go:11`、`config.json` |
| 排除目录 | 配置 `["/temp","/cache","/backup"]`；代码默认 `["/tmp","/logs"]`；全局 `ExcludedDirs` 初始为**空 map** | `config.json`、`global_config.go:13,32` |
| 媒体扩展名 | 配置 `[".pdf",".docx",".mp3"]`；代码默认 `[".jpg",".png",".mp4",".avi"]` —— **两份互相矛盾且都不含图像全集** | `config.json`、`global_config.go:33` |
| 视频扩展名 | `.mov` `.mp4` `.avi` **硬编码**，配置项 `videoExtensions` 从未被读取 | `scanner.go:44-46` |
| MediaFile 字段 | `Path string`、`Size int64`、`Modified int64`(Unix 秒)、`IsVideo bool` | `scanner.go:57-62` |

### 2.1 两条并存的监控链路

`services/file/` 下有两套同职责实现，行为不同，**都要知道**：

| 链路 | 生产端过滤 | 消费端过滤 |
|---|---|---|
| `scanner.go:188 initWatcher` | `mediaExts[filepath.Ext(name)]` 精确 map 查找（`:200`） | — | 
| `watcher.go:30 FileWatcher.Start` | `isMediaFile()` 用 `strings.EqualFold` 逐个比对（`:67-79`）⇒ **大小写无关** | — |

`initWatcher` 无任何调用点，属遗留。

### 2.2 已确认缺陷

1. **`scanDir` 事实上恒返回空** —— `InitWorkerPool` 与 `InitMediaExtensions` 全仓**零调用点**，故 `workerPool` 与 `mediaExts` 永远是 `nil`。`nil` map 取值恒为 false ⇒ 匹配不到任何文件；即使匹配到，`workerPool <- struct{}{}`（`:135`）对 `nil` channel 发送会**永久阻塞**。这不是「忘记初始化才出问题」，而是**该路径必然不可用**。
2. **大小写：仅 scanner 链路受影响** —— `InitMediaExtensions` 存表时 `ToLower`（`:39`），但 `scanner.go:133`、`:200` 与 `isVideoExt`（`:145,171-174`）匹配时**未 ToLower** ⇒ 大写扩展名匹配不到。注意 `watcher.go:74` 用 `EqualFold` 是正确写法，两条链路行为不一致。
3. **排除目录双重失配** —— 键由 `path.Clean` 生成（`global_config.go:59`，正斜杠语义），比较值由 `filepath.Clean` 生成（`scanner.go:125`，Windows 反斜杠），且配置写的是 `"/temp"` 绝对路径、实际路径是 `D:\...\temp` ⇒ 永不相等。递归排除未实现（`:130` 注释自认）。**排除功能事实上无效。**
4. **超时是假的** —— `safeScan` 的 `resultChan` 无缓冲（`:76`），超时分支返回后 `:82` 的发送**永久阻塞**；而 `filepath.Walk` 不接收 `context` ⇒ 超时只是让调用方拿到错误，遍历和 goroutine 继续跑。兜底 walk 必须重写成可真正中断的形式。
5. **配置读取在事件热路径上** —— `watcher.go:69` 每收到一个文件事件就调一次 `config.LoadAndInit()`，失败即 `log.Fatal(err)`（`:71`）⇒ **一次配置损坏会让进程直接退出**。
6. **错误与资源** —— `scanner.go:189` 丢弃 `fsnotify.NewWatcher()` 返回的 error；`:193` 在无限循环内 `defer watcher.Close()`（永不执行）。
7. **定义了但从未读取的配置项** —— `watchInterval`、`dbPath`、`videoExtensions`（`global_config.go:24-27`）。

> ⚠️ 这份配置自身矛盾，**不要照搬任何一份扩展名白名单**。新目标是「展示所有文件类型」，白名单概念应当废除，改为按类型分段渲染。

---

## 3. 配置系统（三套并存，必须合并为一套）

### 3.1 `core/config/core_config.go`（被 `core/tag` 使用）

字段：`database_path`、`files_dir`、`tags_dir`、`logs_dir`、`config_dir`、`templates_dir`。
实际 `core/config/config.json` 只有三项，后两项是**占位字面量**：

```json
{ "database_path": "tags.db", "files_dir": "...", "logs_dir": "..." }
```

缺陷：`Get()` 的 key 与 JSON 字段名风格不一致（JSON `database_path` ↔ 查询键 `"database.path"`，其余用下划线，`core_config.go:33`）；`init()` 中读不到或解析失败即 `log.Fatalf`（`:24,27`）⇒ 配置缺失**直接终止进程**，无降级；使用已废弃的 `io/ioutil`。
另有 `core/config/settings.json`，内容竟是 Go 语句 `package config`，属误建文件。

### 3.2 `global_config/global_config.go`（被 `services/file` 使用）

字段：`scanTimeout`、`mediaDir`、`workerPoolSize`、`mediaExtensions`、`videoExtensions`、`excludeDirs`、`watchInterval`、`dbPath`。

缺陷：
- 与 3.1 **读同一个文件名 `config.json`**（都相对进程 CWD），schema 完全不兼容。
- `DefaultConfig` 定义了默认值但 `LoadConfig()` 从不与它合并（`:37-49` 直接 decode 到零值结构体）⇒ **JSON 缺字段得到 Go 零值（0/nil/""）而非默认值**。`DefaultConfig` 全仓仅 1 处定义、0 处引用。
- `LoadAndInit` 只回填全局 `ExcludedDirs`，**不回填** `ScanTimeout` / `MediaDir` 全局变量（`:51-63`）⇒ 这两项改配置不生效（代码里生效的一直是 `:10-11` 的字面量）。

### 3.3 `core/config/{env_loader,validator}.ts`

孤立的 TypeScript 配置代码，错置在 Go 包目录内，各自 import 了不存在的模块（`./settings`、`./error_handler`），无任何构建系统会编译它。**仍值得带走的只有配置项命名**（`validator.ts:10-12`）：`ai_enabled`、`force_enable_ai`、`memory_limit_mb`（**100–8192**）。内存上限一项对资源管理器的低配降策仍适用。

### 3.4 其他

驱动层也并存两套 SQLite：`gorm.io/driver/sqlite` 与 `mattn/go-sqlite3`（后者需 cgo）—— 与 Rust 侧 `rusqlite`/`sqlx` 均需 `bundled` 才免于系统库这一事实相关。

---

## 4. 事件 → UI 回推链路

来源：`services/file/Qt_file_watcher.go`

```
fsnotify 事件 → 扩展名过滤 → 有界 channel（bufferSize 由构造参数给定）
   → Notify() 非阻塞投递：满则丢弃并打印「通道满，丢弃事件」（:36-38）
   → 独立 goroutine StartQtWatcher 串行消费（:42-46）
   → 每条事件再 go 一个 processFile（:50-57）→ 异步取结果 → 回调 UI
```

可迁移的是**语义骨架**，不是代码：有界队列 + 满即丢弃（不阻塞）+ 单消费者串行化 + 回推 UI。新架构对应形态为 Rust channel → `QAbstractListModel` 行变更信号，且必须**批量合并插入**（新条目先进 pending 队列再统一提交）而非每条一次 `beginInsertRows`，否则慢盘/网络盘上界面必然冻结。

### 4.1 该文件的不可用证据

它调用的 `C.update_ui_thumbnail` **全仓仅此一处出现**，无任何声明或实现；文件顶部 `:3` 是裸 `import "C"`（无 cgo 前言），故连 `C.CString` / `C.free` 都未声明。同文件 `generateThumbnail`（`:67`）还调用了不存在的方法 `ThumbnailGenerator.Generate`（该类型只有 `GetThumbnailAsync`/`worker`/`getCachePath`/`isCacheValid`/`CleanupCache`）。**该文件从未编译通过。**

### 4.2 ⚠️ 旧栈从来没有「删除 → UI 移除」通路

消费端 `HandleEvent`（`:50-51`）的 switch **只处理 `Create` 与 `Write`**；生产端只按扩展名过滤、会把 `Remove`/`Rename` 一并投递进来。结果是：**文件被删除或改名后，事件被静默丢弃，UI 永不更新。**

对通用资源管理器而言这是功能缺失而非缺陷 —— 新实现的 `fam-sched` 必须显式覆盖 Create / Write / Remove / Rename 四类语义，其中 Rename 在 `ReadDirectoryChangesW` 层面本身就是易失信息。

---

## 5. 旧文档有效性对照

| 文档 | 状态 | 说明 |
|---|---|---|
| `docs/sqlDesign.md` | **部分适用，需改写** | 标签集合查询思路（AND/OR、中间表）可参考；但 SQL 是 **PostgreSQL 专有**（`SERIAL`/`INTEGER[]`/`GIN`/`@>`/`&&`），已定 SQLite ⇒ 须改为关联表。**AI 专属章节作废，实际范围比预想大**：`:139-287`（字段扩展、AI 生成主 Tag 流程、待审核低置信度查询、人工审核三接口、性能优化、风险控制）与 `:288-406`（Python 服务 HTTP 即时处理 + 消息队列存量批处理 + 共享库设计）整块均属图片/AI 范畴 |
| `docs/代码辅助器.md` | **部分适用** | 硬件降配与内存分层加载思路对资源管理器有用；RAW 处理方案对比、ONNX 模型矩阵、Electron↔Qt 共享内存三节作废 |
| `docs/history/struct.md` | **过时** | Go 侧 `services/` 目录树，随旧栈失效 |
| `docs/history/coreStruct.markdown` | **部分过时** | 规划的 `hardware/`、`cache/`、`utils/` 三模块确实从未存在；但 `core/ipc/shared_memory.cpp`（0 字节占位）、`core/config/settings.json` 真实存在，`core/logging/logger.ts` 存在于工作区但从未进过 git 且为 0 字节。整体是意图而非现实 |
| `docs/history/frontend.md` | **过时** | Electron 目录树；实际目录另有拼写错误（`compents/`、`preeload-scripts/`、`baes.css`） |
| `docs/history/后端结构树Java` | **过时** | 描述 `com.fileauto.manage` 包树，与实际代码（`com.fam.manage`）也不一致。该文件的 55 行改动已随 `84342b5` 提交入库 |
| `README.md` | **定位已过时** | 「利用 AI 自动对图片增加标签」随范围变更失效 |

---

## 6. 提取完整性核对

| 旧资产 | 提取规格 | 去向 |
|---|---|---|
| 标签表结构与 CRUD（`core/tag`） | ✅ §1.1-1.3 | `fam-svc-store` |
| 第二套标签模型 + 优先级 + manual 优先排序（`services/tag`） | ✅ §1.4 | `fam-svc-store` / `fam-sched` |
| 幂等 upsert 范式（`services/index_service.go`） | ✅ §1.3-1 | 同上 |
| 遍历与监控参数、两条链路、7 项缺陷 | ✅ §2 | `fam-svc-walk` |
| 事件回推骨架 + 删除通路缺失 | ✅ §4 | `fam-sched` |
| 三套配置字段与陷阱 | ✅ §3 | `fam-core` |
| 缩略图 / RAW / libraw / AI 推理 / 置信度审核 | ❌ 出范围 | 另一项目 |
| CAS 快照与 diff（`services/version`） | ❌ 不在新版范围 | 需要时从 `84342b5` 取回 |
| REST / gRPC 网关（`services/gateway`） | ❌ 单进程不需要 | `rpc.go` 因缺 `*.pb.go` 从未编译通过 |
| 文件索引表（`services/index_service.go` 表结构） | ❌ 职责移交 Everything | 仅其上 §1.3-1 的写法保留 |
| Java 全部（`src/`） | ❌ 不提取 | 无对应；`GoBasedDiffAnalyzer` 所 load 的 `GoHashAnalyzer` 库在 Go 侧零命中 |

---

## 修订记录

- 初版后经一次独立合规审查，判定 FAIL，修出 4 类问题：① 引用基准错标成旧 tag（实为 `84342b5`，两者差 81 文件）；② §1 未披露 `core/tag` 从未编译通过；③ `services/tag/` 整套第二标签模型与优先级语义遗漏；④ 缺陷描述有 3 处过度泛化（`workerPool` 是必然 nil 而非偶发、大小写问题仅限 scanner 链路、排除目录是 `path.Clean`/`filepath.Clean` 双重失配），另有 4 项缺陷（假超时、热路径 `log.Fatal`、`Remove`/`Rename` 无通路、`Define-never-read` 配置）完全未记。
