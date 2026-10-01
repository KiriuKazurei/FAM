# 回收站收下整个目录（增量 29）

## 1. 描述问题

27 号账的残留："回收站的目录支持依赖 Qt 的 Windows 实现（本步只实测了文件；目录进 bin
未单独断言）"。人的 Delete 若对目录失效或递归硬删，都与期待相反。

## 2. 分析问题

`QFile::moveToTrash` 在 Windows 走 IFileOperation 的回收站路径，目录与文件同权——不需要
自写 `SHFileOperation` 包装，也不需要 agent 那条"非空目录按名拒绝"的守卫：bin 是可恢复
的，整目录进 bin 正是每个人对 Delete 的期待；agent 的 `fs_delete` 硬删除契约原样（风险
不对称的分歧已在 27 注释里）。

## 3. 解决方式

self-check 的键盘步骤追加目录轮：步骤内新建 `fam-self-check-dir`（含 inner.txt）→ 等
reconcile 上行 → 真两下 Delete → 断言目录与内文件都从树里消失；`deleteRequested` 的
qInfo 现成对记录 bin 内地址（`recycle bin: X -> Y`）。

## 4. 效果评估

**证据**（light/dark 各一遍一致）：

```
recycle bin: ...\tree\fam-self-check-dir -> C:\$Recycle.Bin\S-1-5-21-...\$R267AGC
self-check dir trash: armed=1 gone=1 (inner file gone with it)
```

cargo 38 个二进制全绿；clippy/fmt 干净；self-check 其余断言原样。**自记一处**：目录行
经 watch + reconcile 异步上屏，步骤内加了两秒有界泵等（与传输结算同一形状）；第一版
忘了 `QElapsedTimer` 的 const 限定，编译器拦下。**残留**：bin 满或拒绝时的平台行为
（moveToTrash 返回 false 的句子已有），静默降级为硬删除不做。
