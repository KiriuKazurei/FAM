frontend/
├── main-process/               # 主进程代码
│   ├── main.ts                 # Electron主进程入口
│   ├── ipc-handlers/           # IPC通信处理器
│   │   ├── fileHandlers.ts     # 文件相关IPC处理
│   │   ├── tagHandlers.ts      # 标签相关IPC处理
│   │   └── aiHandlers.ts       # AI功能相关IPC处理
│   └── utils/                  # 工具函数
│       ├── hardwareDetector.ts # 硬件能力检测
│       └── configManager.ts    # 配置管理
│
├── renderer-process/           # 渲染进程代码
│   ├── assets/                 # 静态资源
│   │   ├── icons/              # 图标资源
│   │   └── styles/             # 全局样式
│   │       ├── base.css        # 基础样式
│   │       └── theme.css       # 主题样式
│   ├── components/             # 可复用组件
│   │   ├── FileExplorer.tsx    # 文件浏览器组件
│   │   ├── ThumbnailGrid.tsx   # 缩略图网格组件
│   │   ├── TagEditor.tsx       # 标签编辑器组件
│   │   └── SettingsPanel.tsx   # 设置面板组件
│   ├── pages/                  # 页面级组件
│   │   ├── HomePage.tsx        # 主页面
│   │   ├── FileDetailPage.tsx  # 文件详情页
│   │   └── SettingsPage.tsx    # 设置页面
│   ├── App.tsx                 # 应用根组件
│   ├── index.tsx               # 渲染进程入口
│   └── router.ts               # 路由配置
│
├── preload-scripts/            # 预加载脚本
│   ├── preload.ts              # 预加载脚本入口
│   └── bridge.ts               # 主进程与渲染进程通信桥接
│
├── electron-builder.config.js  # 打包配置
└── package.json                # 项目依赖与脚本