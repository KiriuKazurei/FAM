# 遗留规格存档（legacy specs）

本文件是 P3 删除 Go / Java / Electron / C++ 四套旧栈**之前**的规格提取。这些数值和语义目前只存在于代码里，删除后本文件是唯一出处。

代码本体的恢复锚点有两个，**用途不同，都要记住**：

- `archive/pre-rust-migration`（= commit `57db887`）—— 旧栈最后一次**已提交**的状态。
- commit `84342b5` —— 迁移前的**完整**工作区快照，含当时未提交、未跟踪的内容。**取回任何东西优先用它。**

```
git show 84342b5:<路径>          # 推荐
git ls-tree -r --name-only 84342b5 | grep <关键词>
```

> 说明：`archive/pre-rust-migration` 只覆盖已提交内容。曾有 13 个文件游离在其外（5 个仅存在于索引的 `src/main/java/com/fam/config/*.java`、`ipc-handlers/aiHandlers.ts`、`.idea/inspectionProfiles/Project_Default.xml`，以及 6 个从未进过 git 的未跟踪文件），另有 5 个已跟踪文件的未提交改动（含本文件 §5 引用的 `Design/后端结构树Java` 55 行）。这批内容已由 `84342b5` 全部收编，P3 的删除操作因此可完整回退。

范围说明：本项目已转为**通用资源管理器**，全量名称索引与搜索交由 **Everything SDK** 承担，图片/缩略图/RAW/AI 推理**已划出范围**（由另一个项目承担）。因此 `core/imageutil/*`、`Design/代码辅助器.md` 的 ONNX 与 RAW 章节、`Design/sqlDesign.md:153-247` 的 AI 置信度人工审核流程 **不予提取**，直接删除。

---

## 1. 标签数据模型（唯一仍适用的业务资产）

来源：`core/tag/new_filetagger.go:16-22`

GORM 结构体 `TagEntry`，GORM 默认表名 ⇒ 实际表为 **`tag_entries`**：

| 列 | 类型 | 约束 | 语义 |
|---|---|---|---|
| `id` | int | PRIMARY KEY | 自增 |
| `file_path` | text | NOT NULL, **INDEX** | 文件路径（见下方「身份缺陷」） |
| `tag` | varchar(255) | NOT NULL | 标签内容 |
| `source` | — | DEFAULT `'manual'` | 标签来源，实测取值约定：`manual` / `AI` |
| `created_at` | time.Time | autoCreateTime | 创建时间 |

初始化时执行 `PRAGMA encoding = 'UTF-8';`（`new_filetagger.go:35`）—— 该 PRAGMA 在 SQLite 中并非运行时可设置项，属于**无效语句**，重写时不必照搬。

数据库路径：`core_config.Get("database.path", "tags.db")`（`:29`），默认文件名 **`tags.db`**。源码自带 `TODO: 要指向正确地址`，即该路径从未真正解析成功。

### 1.1 CRUD 语义（必须逐条保持的行为契约）

| 操作 | 签名与行为 | 出处 |
|---|---|---|
| AddTags | `(filePath, tags[], source)`；**显式事务**，逐个校验标签，任一非法即整体 `Rollback`；返回 `"为 %s 添加了 %d 个标签"` | `:52-81` |
| DeleteTags | `(filePath, tags[], source)`；`tags` 为空 ⇒ 删该文件**全部**标签；`source` 为空 ⇒ 不限来源；返回 `RowsAffected` | `:96-116` |
| ListTags | `(filePath, source)`；只 Pluck 标签列；`source` 为空 ⇒ 全部 | `:130-145` |
| UpdateTag | `(filePath, oldTag, newTag, newSource)`；新值为空表示该字段不改；**`RowsAffected == 0` 时视为错误**并回滚，报「未找到标签」 | `:161-195` |
| SearchTags | `(tag, source, searchDir)`；`searchDir` 为空则取配置 `files_dir`，默认 `"./"` | `:210-242` |

**已知的两处行为缺陷，重写时必须显式决策而不是照搬：**

1. **无唯一约束** —— `tag_entries` 上没有 `(file_path, tag)` 唯一索引，同一文件重复添加同一标签会产生多行（`AddTags` 直接 `Create`，无查重）。
2. **SearchTags 的目录过滤在应用层** —— 先用 `tag` 查全表把所有行读进内存，再在 Go 里用 `strings.HasPrefix` 筛目录（`:231-239`）。文件多时是内存与延迟地雷。规范化做法是把目录前缀条件下推到 SQL（`file_path GLOB ?` 或引入 `dir_id` 外键）。
   其中路径比较前对双方做 `filepath.Clean`，并给前缀补一个分隔符：`filepath.Join(cleanSearchDir, "")`（`:227-228`）—— 这是为了避免 `/foo/bar` 前缀误匹配 `/foo/baz`，重写时这个语义要保留。

### 1.2 标签合法性规则

来源：`new_filetagger.go:246-249`

```
len(tag) <= 64  &&  tag 匹配 ^[a-zA-Z0-9\u4e00-\u9fa5 ！？，。、]+$
```

- 允许：ASCII 字母、数字、汉字、**空格**，以及全角标点 `！ ？ ， 、 。`
- 其余一律拒绝（含 `-` `_` `.` `/` `#`、半角标点、emoji）
- ⚠️ **`len()` 是字节数不是字符数**：Go 的 `len(string)` 返回 UTF-8 字节长度，汉字 3 字节 ⇒ 中文标签实际约 **21 个汉字**封顶，而非 64。Rust 侧若改用 `chars().count()` 会静默放宽限制，需明确选哪一种。

### 1.3 身份模型（本文件最重要的迁移决策）

旧实现把标签**绑在路径字符串上**。新架构已确认：Everything SDK 1.4.1 **不提供任何稳定标识符**（无 `GetResultFileFRN` / `GetResultPIDL`，只返回路径字符串），因此重命名后标签必然失联。

迁移方案：新增 `file_identity(volume_serial, frn)` 唯一表，用 `GetFileInformationByHandle` 取 `BY_HANDLE_FILE_INFORMATION` 的卷序列号 + 文件索引号作为 FRN（该 API 不要求管理员权限），标签改绑 `stable_id`，路径退化为可更新属性列。重命名后按 FRN 反查找回并只更新路径。

---

## 2. 遍历与监控参数（降级为「兜底 walk」参考）

Everything 只索引**本地固定 NTFS 卷**；exFAT/FAT/网络盘在 1.4.1 上不被索引，因此 `fam-svc-walk` 兜底路径仍然必须存在。以下参数是它的历史取值。

来源：`services/file/scanner.go`、`global_config/global_config.go`

| 参数 | 值 | 出处 |
|---|---|---|
| 扫描超时 | **30 秒**（全局变量默认值） | `global_config.go:10` |
| worker pool 容量 | **10**（实际配置值），原为硬编码 100 后改为读配置 | `scanner.go:18-24`、`global_config/config.json` |
| 并发模型 | `filepath.Walk` **串行**遍历目录树，每命中一个文件起 goroutine，`workerPool` 信号量限流，`sync.Mutex` 保护结果切片 | `scanner.go:111-159` |
| 监控目录 | `"/监控目录"`（占位值，从未真正存在） | `global_config.go:11`、`config.json` |
| 排除目录（配置） | `["/temp", "/cache", "/backup"]`；代码默认值另为 `["/tmp", "/logs"]` | `config.json`、`global_config.go:32` |
| 媒体扩展名（配置） | `[".pdf", ".docx", ".mp3"]`；代码默认值另为 `[".jpg", ".png", ".mp4", ".avi"]` | `config.json`、`global_config.go:33` |
| 视频扩展名（**硬编码**） | `.mov` `.mp4` `.avi` | `scanner.go:44-46` |
| MediaFile 字段 | `Path string`、`Size int64`、`Modified int64`(Unix 秒)、`IsVideo bool` | `scanner.go:57-62` |
| 事件过滤 | 只把扩展名命中的 fsnotify 事件转给上层（`Create`/`Write`） | `scanner.go:200`、`Qt_file_watcher.go:52-57` |

**⚠️ 这份配置本身就自相矛盾**（配置说 pdf/docx/mp3、代码默认说 jpg/png/mp4/avi，且二者都不含图像全集）。所以**不要照搬任何一份白名单** —— 新架构是「展示所有文件类型」，扩展名白名单这个概念本身应当废除，改为按类型分段渲染。

**已确认的实现缺陷（重写时避免复现）：**

1. 大小写：`InitMediaExtensions` 存表时做了 `strings.ToLower(ext)`（`scanner.go:39`），但匹配时 `mediaExts[filepath.Ext(p)]` **没有 ToLower**（`:133`）⇒ 大写扩展名 `.JPG` 永远匹配不到。
2. 排除失效：排除判定用 `filepath.Clean` 后的**精确字符串**相等（`:127`），而配置里是 `"/temp"` 这类绝对路径 ⇒ 实际文件路径 `D:\...\temp` 永远不相等，**排除目录功能事实上无效**。且注释里已自认「进一步检查父目录是否被排除（如需递归排除）」未实现。
3. nil channel 死锁：`workerPool` 只有在调用过 `InitWorkerPool` 后才非 nil，否则 `workerPool <- struct{}{}`（`:135`）**永久阻塞**且无超时。
4. 监控非递归：`watcher.Add(config.MediaDir)` 只注册了单层目录。
5. 视频扩展名无视配置：`videoExts` 是硬编码 map，配置项 `videoExtensions` 定义了却从未被使用。

---

## 3. 配置系统（三套并存，必须合并为一套）

### 3.1 `core/config/core_config.go`（被 `core/tag` 使用）

字段：`database_path`、`files_dir`、`tags_dir`、`logs_dir`、`config_dir`、`templates_dir`

实际 `core/config/config.json` 只有三项，且后两项是**占位字面量**：

```json
{ "database_path": "tags.db", "files_dir": "...", "logs_dir": "..." }
```

缺陷：
- `Get()` 的 key 与 JSON 字段名不一致 —— JSON 是 `database_path`，但查询键写成 `"database.path"`（`core_config.go:33`），其余键用下划线。两种风格混用。
- 读不到文件 / 解析失败即 `log.Fatalf`（`:24,27`）—— 在 `init()` 里，意味着**配置缺失直接终止进程**，无法降级。
- 使用已废弃的 `io/ioutil`。

### 3.2 `global_config/global_config.go`（被 `services/file` 使用）

字段：`scanTimeout`、`mediaDir`、`workerPoolSize`、`mediaExtensions`、`videoExtensions`、`excludeDirs`、`watchInterval`、`dbPath`

缺陷：
- 与 3.1 **读同一个文件名 `config.json`**（都从进程工作目录相对读取），但 schema 完全不兼容 —— 任一方加载对方的文件都会失败。
- `DefaultConfig` 定义了默认值，但 `LoadConfig()` 从不与它合并（`:37-49` 直接 decode 到零值结构体）⇒ **JSON 中缺失的字段得到的是 Go 零值（0 / nil / ""），而不是 DefaultConfig 的值**。这是「写了默认值却不生效」的陷阱。
- `LoadAndInit` 只把 `excludeDirs` 回填到全局 `ExcludedDirs`，`ScanTimeout` / `MediaDir` 全局变量**不会**被 JSON 覆盖 ⇒ 改了配置不生效。

### 3.3 `core/config/{env_loader,validator}.ts`

孤立的 TypeScript 配置代码，位置错置在 Go 包目录内，且各自 import 了不存在的模块（`./settings`、`./error_handler`），无任何构建系统会编译它们。**其中记录的配置项命名仍值得带走**（`validator.ts`）：`ai_enabled`、`force_enable_ai`、`memory_limit_mb`（范围 100–8192）—— 内存上限这一项对资源管理器的「低配降策」仍然适用。

---

## 4. 事件 → UI 回推链路（去掉缩略图环节后仍适用）

来源：`services/file/Qt_file_watcher.go`

```
fsnotify 事件 → 扩展名过滤 → 有界 channel（bufferSize 由构造参数给定）
   → Notify() 非阻塞投递：channel 满则**丢弃事件并打印**「通道满，丢弃事件」（:36-38）
   → 独立 goroutine StartQtWatcher 串行消费（:42-46）
   → 每条事件再 go 一个 processFile（:52-57）→ 异步取结果 → 回调 UI
```

**这是唯一一处真正的 Qt 集成尝试**，但它调用的 `C.update_ui_thumbnail` 在整个仓库中**没有任何声明或实现**（悬空符号），同文件的 `generateThumbnail`（`:67`）还调用了一个不存在的方法 `ThumbnailGenerator.Generate` —— 该文件从未编译通过过。

可迁移的是**语义骨架**，不是代码：有界队列 + 满即丢弃（而非阻塞）+ 单消费者串行化 + 回推 UI。新架构里的对应形态是 Rust channel → `QAbstractListModel` 的行变更信号，且必须按 Dolphin 的做法**批量冲刷插入**（新条目先进 pending 队列再合并提交）而非每条一个 `beginInsertRows`，否则慢盘/网络盘上界面必然冻结。

---

## 5. 旧文档有效性对照

| 文档 | 状态 | 说明 |
|---|---|---|
| `Design/sqlDesign.md` | **部分适用，需改写** | 标签模型的查询思路（AND/OR 集合查询、中间表、审核流程概念）可参考；但 SQL 是 **PostgreSQL 专有**（`SERIAL`/`INTEGER[]`/`GIN`/`@>`/`&&`），已定 SQLite ⇒ 需改为关联表。`:153-247` 的 AI 置信度人工审核流程**出范围，作废** |
| `Design/代码辅助器.md` | **部分适用** | 硬件降配与内存分层加载的思路对资源管理器仍有用；RAW 处理方案对比、ONNX 模型矩阵、Electron↔Qt 共享内存三节**作废** |
| `Design/struct.md` | **过时** | Go 侧 `services/` 目录树，随旧栈一并失效 |
| `Design/coreStruct.markdown` | **过时** | 规划的 `core/` 含 `hardware/`、`cache/`、`logging/`、`utils/` 四个模块，实际**从未存在**（`detector.cpp`、`detector.h`、`thresholds.json`、`lru_cache.ts`、`disk_cache.ts`、`error_handler.ts` 在 tag 中 0 命中）。`core/ipc/` 确实存在但只有 0 字节的 `shared_memory.cpp` 占位。`core/logging/` 不在 tag 中——工作区的 `logger.ts` 是**未跟踪文件，从未进过 git**，且为 0 字节。整份文档是意图而非现实 |
| `Design/frontend.md` | **过时** | Electron 目录树；且实际目录有拼写错误（`compents/`、`preeload-scripts/`、`baes.css`） |
| `Design/后端结构树Java` | **过时** | Java 包树（`com.fileauto.manage`），与实际代码（`com.fam.manage`）也不一致。⚠️ 该文件当前有**未提交的工作区改动**（55 行），归档时注意 |
| `README.md` | **定位已过时** | 「利用 AI 自动对图片增加标签」的描述已随范围变更失效 |

---

## 6. 提取动作的完整性核对

| 旧资产 | 是否已提取规格 | 去向 |
|---|---|---|
| 标签表结构与 CRUD | ✅ 本文件 §1 | `fam-svc-store` |
| 标签合法性规则 | ✅ §1.2 | 同上 |
| 遍历并发参数 | ✅ §2 | `fam-svc-walk` |
| 配置项清单与陷阱 | ✅ §3 | `fam-core` |
| 事件回推链路 | ✅ §4 | `fam-sched` |
| 缩略图生成 / 缓存池 | ❌ 不提取 | 出范围（另一项目） |
| RAW / libraw 解析 | ❌ 不提取 | 出范围 |
| AI 推理与置信度审核 | ❌ 不提取 | 出范围 |
| CAS 快照与 diff（`services/version/`） | ❌ 不提取 | 版本控制暂不在新版范围内；需要时从 tag 取回 |
| REST/gRPC 网关（`services/gateway/`） | ❌ 不提取 | 单进程架构不需要网关层；且 `rpc.go` 因缺 `*.pb.go` 从未编译通过 |
| 文件索引表（`services/index_service.go`） | ❌ 不提取 | 该职责整体移交 Everything |
