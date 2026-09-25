# FAM 界面样机 · Ant Design 5 样式

这是**设计基准，不是产品**。它把资源管理器主窗口按 Ant Design 5 的设计体系画了一遍：驱动器栏、
两列 Miller 目录、工具栏、行绘制、空态与错误态、亮/暗两套。

它和 `apps/fam-shell` 一样，都编译 `apps/fam-ui` 里那**唯一一份**设计系统（token → QSS 与那张手绘的
文件行），所以"改一处、另一处还是旧的"这种漂移在这里不可能发生。`apps/fam-ui` 只链 `Qt6::Widgets`，
不链 `famcore`、不依赖任何 Rust crate——参考物不能依赖被参考的东西，这条边界照旧守着。

**2026-09-25 之前这里写着"原项目的界面保持独立，哪天真要采纳那是一次单独的改动"。**
那一天用户改口"保留 Ant Design 样式版本，接下来的开发均基于这个版本"，采纳已经发生，
账在 `docs/notes/shell-wears-the-ant-design-baseline.md`。这个目标现在的作用有两个：
**基准**（产品窗口应该长这样）与**截图工具**（`--shot` 出图、stdout 打实测对比度表）。
产品功能不再长在这里。

## 跑起来

```bash
export PATH="/d/Applications/Qt/6.11/Tools/mingw1310_64/bin:/d/Applications/Qt/6.11/Tools/CMake_64/bin:/d/Applications/Qt/6.11/Tools/Ninja/bin:$PATH"
cmake -S apps/fam-design -B apps/fam-design/build -G Ninja \
  -DCMAKE_CXX_COMPILER=g++.exe -DCMAKE_PREFIX_PATH=D:/Applications/Qt/6.11/6.11.2/mingw_64
cmake --build apps/fam-design/build

# 截图模式（渲染 → 存图 → 退出），顺手把实测对比度表打到 stdout
apps/fam-design/build/fam-design.exe --theme light --dir <某个真文件夹> --shot out.png
apps/fam-design/build/fam-design.exe --theme dark
```

窗口里每个控件都是真的：搜索框过滤当前列，两个下拉分别按类型过滤和改排序，左列点文件夹就出右列，
"打开文件夹…"是真的文件对话框，右上角那颗按钮在亮暗之间切换（换的是 token 表，不是两张样式表）。
列的是真目录，不是示例数据——名字、大小、日期都是磁盘上的。

## 设计体系在哪

`apps/fam-ui/src/theme.{h,cpp}` 是唯一的颜色与尺寸来源（两个界面都从这里取）：

- `Tokens` 用 Ant Design 自己的名字（`colorText` / `colorBgContainer` / `borderRadius` /
  `controlHeight` / `paddingXS` …），亮暗两套在同一个构造函数里按同一套算法派生。
- Qt 样式表没有变量，所以 `buildSheet()` 把 `{{token}}` 一次性替换进样式表；
  组件的形态靠 object name 选（`Theme::primaryButtonName()` 等一处列全），
  调用点不会自己编一个样式表里没有的名字。
- 样式表管不到的地方（行是自己画的）取同一份 `Tokens`，所以不会出现按钮一个蓝、行另一个蓝。

### 三处刻意不等于 Ant Design 默认值

`contrastReport()` 每次启动都把这个主题**实际画在屏幕上的**每一对文字/背景算一遍 WCAG 比值。
照 antd 原值算出来有两对不过正文线，所以改了；改完的数字是实测的，不是估的：

| 位置 | antd 默认 | 这里 | 为什么 |
| --- | --- | --- | --- |
| 实心按钮底 | `#1677ff` | 亮 `#0958d9` / 暗 `#4096ff`（配深色字） | 白字压 `#1677ff` 只有 **4.10:1** |
| 作为文字的蓝 | `#1677ff` / hover `#4096ff` | `accentText`：亮 `#0958d9`、暗 `#4096ff` | `#4096ff` 当 14px 正文是 **2.99:1** |
| 滚动条滑块 | 很淡 | 提到三级文字色 | 滑块是"看得见才能用"的控件，3.0:1 那条线要过 |

两处**保留** antd 原值并让报告点名：输入框边框 `#d9d9d9`（1.41:1）与禁用文字（1.83:1）。
前者是"要不要让表单明显变重"的取舍，后者 WCAG 1.4.3 本来就免除禁用件——两者都算进报告、
标 `免除`，不做成"没测"。

## 已知不做

- 这里没有 shell 图标（那要 `shell32`，产品壳有）。行里的 16px 槽位**永远占着**，画不出图标时画一个
  单色线稿，所以图标到位前后名字不会横移。产品壳里同一个线稿不是占位装饰，它是"图标还在工作线程上"
  的那一瞬间的形状——那层是异步的，所以每一行都先从这里开始。
- 没有监视、分页、标签、轮询——那些是行为不是长相。
- `QLabel` 不省略号，列标题按标称列宽预先裁（`elideTitle`），列比标称宽时名字整段显示。
- 焦点环：antd 用 2px box-shadow，Qt 样式表没有 box-shadow，这里退化成 1px 边框加宽。
  把边框从 1px 撑到 2px 会让文字横移 1px，比没有光环更糟。
