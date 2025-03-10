services/
├── file/               # 文件管理域
│   ├── scanner         # 文件扫描（集成现有file_scanner）
│   ├── watcher         # 文件监控（集成fsnotify）
│   └── metadata        # 元数据解析（EXIF/RAW处理）
├── tag/                # 标签管理域
│   ├── storage         # 标签存储
│   ├── search          # 标签搜索引擎
│   └── priority        # 标签优先级管理
├── ai/                 # AI处理域
│   ├── loader          # 模型动态加载
│   ├── local_infer     # 本地推理
│   └── cache           # 结果缓存
├── version/            # 版本控制域
│   ├── snapshot        # 快照生成
│   └── diff            # 差异对比
└── gateway/            # 统一接口层
    ├── rest            # REST适配器
    ├── rpc             # gRPC服务
    └── event           # 事件发布