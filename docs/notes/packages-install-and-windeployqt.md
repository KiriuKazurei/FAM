# 装成一个别人真能双击的文件夹（增量 15）

## 1. 描述问题

README 里挂着这么一句（两份都在，中英各一处）：

> **目前既没有 `install()` 目标也没有 windeployqt 步骤**，这是把原型交给别人试之前还差的一件事

具体缺什么，动手前量的：

- `apps/fam-shell/CMakeLists.txt`、`apps/fam-design/CMakeLists.txt`、`apps/fam-ui/CMakeLists.txt`
  三个工程里 `install(` 出现 **0 次**。`cmake --install <build>` 只会打印
  `CMake Error ... file cannot create directory`（因为没有规则，也没有前缀）。
- 运行期依赖全靠一条被写进 README 的 PATH：Qt 的 bin、MinGW 的 bin、放着 `famcore.dll` 的
  GNU 产物目录，三份同时在。这条 PATH 是**这台开发机的状态**，不是程序的一部分。
- 树里这时已经有第三个 CMake 工程（`apps/fam-ui`，增量 13 为解决主题漂移而设）。
  README 那句"还差 windeployqt"是在只有两个工程的时候写的；打包要面对的新事实是三个。

## 2. 分析问题

### 为什么必须问 windeployqt，而不是自己抄文件

可执行文件的导入表能告诉链接器需要什么，告诉不了 Qt 运行时要什么。两处抄不到：

- **平台插件**。`platforms/qwindows.dll` 不是被 `Qt6Gui.dll` 静态导入的，是 Qt 启动时按目录约定
  去加载的；缺它就没有 GUI，而一个 `WIN32` 子系统的应用**没有控制台可以把那句话喊出来**。
- **调试/发布后缀与 ICU**。MinGW 版 Qt 的 DLL 命名、以及 `Qt6Core.dll` 对 ICU 的依赖方式，
  随 Qt 的版本和配置变。手抄一份清单，就是买一张"哪天 Qt 升级了这里静默烂掉"的期票。

所以这一步交给 `windeployqt`，本增量只负责把它接在 `cmake --install` 的正确位置上。

### 三个工程，两个要发的程序

`fam-ui` 是 `STATIC` 库，它的目标文件已经在可执行文件里面了。给它写安装规则会装出一个
没人链接的 `.a`——那是把"第三个工程"这个结构事实错当成"第三个交付物"。
所以它没有安装规则，而这条理由写在了它出现的地方（模块头的注释里），不是只写在这篇账里。

两个控制台目标 `abi-smoke` / `model-smoke` 同样**不安装**：它们是读这份源码才跑得动的闸门
（`abi-smoke` 要一个配置文件和一个树指），不是产品。

### 为什么打包规则放在 `apps/fam-ui/cmake`

两个 Qt 面都要部署。写两遍的话，第一遍改了 flags 第二遍不会跟着改，而漂移的表现形式是
"设计基准能跑，产品壳装完起不来"——那正好是增量 13 花了一整篇账要避免的那类错误，
只是换到了打包这一层。所以放进已经存在的"唯一一份"里，两边各 `include()` 一次。

`fam_deploy_qt(TARGET fam-shell DLL famcore)` 与 `fam_deploy_qt(TARGET fam-design)` 的差别
是**目标**的差别（一个带核心一个不带），不是**步骤**的差别，所以它留在调用点，没留在模块里。

### 一条 PATH 假设，被自己的实验否掉

模块最初还多钉了一个 `--qtpaths <绝对路径>`，理由是"windeployqt 否则要靠 PATH 找它的 Qt"。
听起来正是这个增量要消灭的那类失败，所以当时就加了。

实测把它摘掉：`PATH` 只留 `C:\Windows\system32;C:\Windows`，直接按绝对路径调 windeployqt——
它照样从**自己所在目录**解析出 Qt 并成功部署（退出 0）。假设不成立，那行连同它的理由一起删了。
留下的是 `find_program(... HINTS "${Qt6_DIR}/../../../bin")`，这条的依据不是实验而是
`find_program` 的搜索顺序（HINTS 先于 `PATH`），它保证用的是 CMake 解析到的那份 Qt；
这台机器上只装了一份 Qt，所以这条**没有被实测区分开**，标注为按文档成立。

### 一个不存在的命令行开关

第一版写的 `--no-translator` 在 Qt 6.11.2 里不存在，装到一半炸出来：
`windeployqt failed (exit 1): Unknown option 'no-translator'`。
真名是 `--no-translations`。这条本身不值一记，值一记的是**它没被 README 或文档骗过去**：
报错来自一次真实安装，而不是来自"我以为 windeployqt 有这些开关"。

### 唯一一处知道能省、但故意不省的东西

装完的壳里 `opengl32sw.dll` 占 19.7MB（总 125MB 的 16%），`--no-opengl-sw` 就能去掉。
不做的理由不是懒：它保护的是"这台机器没有可用的 OpenGL 驱动"那种环境，
而**这台开发机造不出那个条件**。在这台机器上删掉它再跑一遍成功，只能证明
"有驱动时不需要它"——那恰恰不是风险所在。所以留着，并在这里点名它有多贵。

同理，`D3Dcompiler_47.dll`（4.0MB）留着。Qt Widgets 的栅格绘制路径大概用不到它，
但"大概"在这里的代价是别人机器上一个空白窗口。

## 3. 解决方式

### `apps/fam-ui/cmake/FamDeployQt.cmake`（新，105 行：44 行命令、56 行注释、5 行空行）

```cmake
function(fam_deploy_qt)
    cmake_parse_arguments(PARSE_ARGV 0 FAMD "" "TARGET" "DLL")
    ...
    install(TARGETS ${FAMD_TARGET} RUNTIME DESTINATION .)
    foreach(dependency IN LISTS FAMD_DLL)
        if(TARGET ${dependency})
            install(IMPORTED_RUNTIME_ARTIFACTS ${dependency} DESTINATION .)
        else()
            install(FILES ${dependency} DESTINATION .)
        endif()
    endforeach()
    fam_find_windeployqt(FAM_WINDEPLOYQT)
    install(CODE "... execute_process(COMMAND windeployqt --compiler-runtime
              --no-translations --no-quick-import <installed exe> ...)")
endfunction()
```

四个点值得单独说：

- **`DLL` 收的是 imported target，不是路径**。`install(IMPORTED_RUNTIME_ARTIFACTS famcore ...)`
  装的就是 `-DFAMCORE_DLL=` 指过去的那一份，也就是链接时用的那一份 —— 一个过期的缓存项
  装不出"另一个 DLL"，而写死路径的规则可以。
- **全部平铺在 `.`**，不 `bin/` + `lib/`：这是一个人要整份拷走的 Windows 程序目录，
  而加载器不搜子目录，分文件夹只是把失败挪后一步。
- **`install(CODE)` 排在同一次安装的后段**，所以 windeployqt 检查的是一个已经落在磁盘上、
  导入表可查的二进制；并且它失败就 `FATAL_ERROR`——静默跳过部署会产出一个
  "看起来完整、跑不起来"的文件夹。
- **默认前缀**：`CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT` 时改指 `<build>/install`，
  否则一次普通的 `cmake --install` 会去撞 `C:\Program Files`。

### 两个工程各接一行

`apps/fam-shell/CMakeLists.txt` 末尾：

```cmake
include(${CMAKE_CURRENT_SOURCE_DIR}/../fam-ui/cmake/FamDeployQt.cmake)
fam_deploy_qt(TARGET fam-shell DLL famcore)
```

`apps/fam-design/CMakeLists.txt` 末尾同形，少了 `DLL famcore`。

### 验收方式写进了两份 README

```cmd
set PATH=C:\Windows\system32;C:\Windows
dist\fam-shell\fam-shell.exe --config <配置.toml> --watch <目录> --self-check 截图.png
```

## 4. 解决方式效果评估

### 正面证据

**同一条件下的一成一败**，`PATH` 只有 `C:\Windows\system32;C:\Windows`，
两个面各测一次，装好的与 build 目录里那个是**同一份构建产物**：

| 跑的东西 | 退出码 | 结果 |
| --- | --- | --- |
| `dist/fam-shell/fam-shell.exe` | `0` | 截图与 `.log` 都写出来 |
| `apps/fam-shell/build/fam-shell.exe` | `-1073741515`（`0xC0000135`） | **连日志文件都没有**（`ls` 确认不存在） |
| `dist/fam-design/fam-design.exe` | `0` | 对比度表照常打到 stdout，截图写出 |
| `apps/fam-design/build/fam-design.exe` | `-1073741515` | 同上，进程没起来 |

`0xC0000135` 是加载器找不到 DLL，发生在 `main` 之前——所以"build 目录那个跑不起来"
不是配置问题，正是这条 PATH 一直在替所有人兜底的那件事。

**装好的那份跑的是完整闸门，不是"能开窗口"**。`--self-check-mutate` 从 `dist/` 里、清空 PATH
跑完，数字与 build 目录里那一遍**逐项相同**：

```
门槛：正文 4.5:1，大字与非文字 3.0:1；受门槛的 13 对里 0 对不过
icons: extracted=18 destroyed=18 cached=18 pending=0
ipc: headers=10 pages=19 polls=78 retries=0 total=107
```

三条落放证据（移动 / Ctrl 复制 / 同目录 no-op）也都在，`info: drop ` 计数 3。

**全新配置的 build 目录里，`cmake --install` 不带 `--prefix` 也能落对地方**：
临时配一份 `fam-design`，安装输出
`Installing: .../prefixtest/install/fam-design.exe`，缓存里
`CMAKE_INSTALL_PREFIX:PATH=.../prefixtest/install`。

**重复安装是幂等的**：第二次跑输出 `Up-to-date:` 并仍然部署成功，耗时 1.15 秒
（实测，同机）。

**核心侧一行未动**：`cargo test --workspace` 499 passed / 0 failed / 2 ignored、
clippy 干净、fmt 干净、`abi-smoke` 五条 `-> ok`、`model-smoke` 六条 `MODEL_SMOKE=0`。

### 代价

- **交付物的体积**（Debug 产物，实测）：壳 **125MB / 23 个文件**，样机 **61MB / 22 个文件**。
  大头依次是 `famcore.dll` 51.2MB、`opengl32sw.dll` 19.7MB、`fam-shell.exe` 13.8MB、
  `Qt6Gui.dll` 11.0MB、`Qt6Core.dll` 10.8MB。差的 64MB 就是那份 debug 的核心。
- **多一个文件、多一条命令**：`FamDeployQt.cmake`（105 行，其中 44 行是命令）与两个 `include()`。
  安装步骤本身成为失败点——windeployqt 非零退出现在会让 `cmake --install` 直接失败，
  这是要的，但它意味着**打包多了一条会红的链路**。
- **配置期多两个 `find_program(... REQUIRED)`**：拿不到 windeployqt 就配置不过。
  对非 Windows 或裁剪过的 Qt 安装，这是一道新门槛。

### 收回一句话

README 里"**目前既没有 `install()` 目标也没有 windeployqt 步骤**"（中英各一处）已不成立，
两处都换成了实际命令与那条清空 PATH 的验收方式。

### 残留风险与局限（这次没解决什么、为什么）

1. **装的是 Debug 产物**。`famcore.dll` 51.2MB 里绝大部分是可去掉的调试信息，
   `CMAKE_BUILD_TYPE=Debug` 也决定了 Qt 侧的选择。不做的理由：Release 核心会动到
   bench 地板的口径（那些数字全是 debug 构建下测的），换构建类型是一次单独的、
   要重测地板的改动。严重度：**中**（体积直接翻倍以上）。归属：地板重录那一次一起做。
2. **`opengl32sw.dll` 19.7MB 与 `D3Dcompiler_47.dll` 4.0MB 仍在包里**。理由见第 2 节：
   它们保护的条件这台机器造不出来。严重度：低（16% 体积）。归属：拿到一台
   无 GPU 驱动的机器（或虚拟机）再裁。
3. **老 build 目录的默认前缀仍然是 `C:\Program Files`**。`CMAKE_INSTALL_PREFIX_INITIALIZED_TO_DEFAULT`
   只在**第一次配置**时为真；本仓库这两个 build 目录是改动之前配的，缓存里那条还在，
   所以不带 `--prefix` 安装会撞权限错误。没有写启发式去猜"这个前缀是不是 CMake 自己塞的"，
   因为 CMake 的默认值随生成器与位数变（这里是 `Program Files (x86)`），猜错更糟。
   绕法两条，都写进了 README：给 `--prefix`，或重新配置。严重度：低（报错可读，且新配置不受影响）。
4. **没有安装包**：产出是一个文件夹，没有安装器、没有卸载、没有开始菜单项、
   没有把 `FAM_MCP_TOKEN` 之类的环境约定接进去。严重度：中（"交给别人试"到"发给别人装"之间还差这一步）。
   归属：真正要发布的时候。
5. **可执行文件没有版本资源，也没有图标**（仓库里 `.rc` 文件 0 个，
   `VS_VERSION_INFO` 在 exe 里搜不到）。于是资源管理器里它没有版本号、没有产品名、
   任务栏上是 Qt 的默认图标。`QApplication::setApplicationVersion("0.1.0")` 是 Qt 内部的，
   不进 PE 资源。严重度：低（不影响运行，影响的是"这像不像一个成品"）。
6. **`fam-cli` 与 `fam-mcp` 不在这条路上**。它们是 Rust 控制台产物，不需要 Qt，
   也就没有 windeployqt 这一步；但"发一个能用的 MCP 服务器"还要说清 `FAM_MCP_TOKEN` 由谁注入。
   严重度：中（agent 那一面目前只能从 cargo 目标目录跑）。归属：与打包同一层，另开一次。
7. **windeployqt 只在 Windows 上存在**，所以这条安装规则在别的平台上会停在
   `find_program(... REQUIRED)`。这个仓库目前只面向 Windows，没有为此做条件分支——
   做了就是没有验证过的代码。严重度：低。归属：真要跨平台时。
