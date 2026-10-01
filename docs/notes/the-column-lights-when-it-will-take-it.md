# 这一列会收下它：拖放的悬停反馈（增量 22）

## 1. 描述问题

14 号账的最后一个"中"：

> 拖拽悬停时列上没有任何高亮反馈：`setDropIndicatorShown(false)` 关掉了那条"插在中间"的线
> （语义不对），但没有换成"这一列会收下它"的替代提示。人只能凭落下之后的结果判断目标对
> 不对。归属：主题层，一条 `:hover` 之外的规则。

## 2. 分析问题

### 高亮的是列，不是行

14 立过"位置不决定语义"：落点是**该列显示的文件夹**，落在行上与落在空白处一个样。所以
反馈的形状跟着语义走——一整列的边框亮起，而不是行间的插入线（那句话说"落在两行之间"，
正是 14 关掉它的原因）。`FileView` 在 `dragEnter`/`dragMove`/`dragLeave`/`drop` 四个事件
里维护一个 `dropHover` 动态属性，样式表按属性匹配：

- **enter/move 各答一次**——`dragMove` 本来就要逐次回答（不答则平台画禁止光标），属性
  跟着答案走，一帧不多、一帧不少；
- **leave/drop 清除**——"会收下"只对悬停中的那一次拖成立，拖走了或落下了就该灭；
  外来载荷（`text/uri-list`）答 ignore 的同时属性也是 false，与"列收不收"同一步判断。
- 属性变化要 `unpolish`/`polish` 才会重解析样式表——Qt 的惯例，不写就没有重绘。

对比度按非文字 3:1 **受门槛**处理：这个描边承载信息（"这列会收下"），与 `输入框边框`
那种装饰性发丝线不同属。实测浅 4.10、深 4.49，双双过线——`primary` 直接用即可，
不需要换 `accentText`。

## 3. 解决方式

- `apps/fam-shell/src/fileview.{h,cpp}`：`setDropHover(bool)`（属性 + unpolish/polish +
  update）；`dragEnter`/`dragMove` 按答案置位、新增 `dragLeaveEvent` 重写清除、`dropEvent`
  开头清除（落下即拖结束，接受与否都不再悬停）。
- `apps/fam-ui/src/theme.cpp`：`QListView#fileList[dropHover="true"] { border: 1px solid
  {{primary}}; border-radius: {{radiusLG}}px; }`；对比对"拖放描边 / 容器"（受门槛 3.0）。
- self-check：`dropHoverForSelfCheck`——对最深的列发**真的 `QDragEnterEvent`**（载荷是
  拖文件，撤销步把它送回了哪端就从哪端拿），断言属性为真、**当场抓帧**（`x.drophover.png`，
  这一状态只存在于一次拖的中间，事后无法摆拍），再发真 `QDragLeaveEvent` 断言属性为假。
  时间线尾部让位（清理 10500→10800，抓帧 11000→11300）。

## 4. 解决方式效果评估

### 正面证据

**无头那一步的原文**（fresh 树，light 与 dark 两遍一致）：

```
self-check drop hover: entered=1 lit=1 shot=...\shot-light.drophover.png
self-check drop hover left: lit=0
```

两张截图看过：悬停中的那列边框亮起品牌蓝、其余列不变，light/dark 下与各自窗口同底色。

**核心侧**：`cargo test --workspace` 38 个二进制全绿；clippy 与 fmt 干净。
**壳侧**：light/dark 退出 0，对比度"受门槛的 18 对里 0 对不过"（新对：浅 4.10 / 深 4.49）；
`abi-smoke` 连跑两遍 `-> ok`；`model-smoke` ok。

### 两处自记

1. **第一版步骤找错了载荷**：拖文件被 20 的撤销步送回了子目录，而步骤只看根路径——
   警告"no probe file to carry"一步读出病因。修法是按传输步骤记下的两端找（撤销把它
   送回哪端就从哪端拿）。
2. **步骤签名第一版引用了不存在的取路径助手**，在构建前就被发现——修正为与其他抓帧
   步骤同款（路径由时间线传入）。

### 代价与残留

- 四个事件各一次属性写 + 两次样式重解析（只在状态翻转时发生——`setDropHover` 对同值
  直接返回）。
- **14 号账的三个"中"至此清零**；低级残留全部有案：悬停只回答"收不收"，不回答
  "移动还是复制"（那由 Ctrl 与光标说，光标是平台画的）；结果列表是来源不是目标，
  永不亮起。
