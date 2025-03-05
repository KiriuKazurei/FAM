core/
├── config/               # 配置管理模块
│   ├── settings.json     # 默认配置文件
│   ├── env_loader.ts     # 环境变量加载器
│   └── validator.ts      # 配置校验逻辑
├── hardware/             # 硬件检测模块
│   ├── detector.cpp      # 硬件能力检测实现
│   ├── detector.h        # 检测接口声明
│   └── thresholds.json   # 硬件阈值配置
├── logging/              # 日志系统
│   ├── logger.ts         # 日志核心实现
│   ├── formatter.ts      # 日志格式化器
│   └── transports/       # 日志输出目标
│       ├── file.ts       # 文件日志
│       └── console.ts    # 控制台日志
├── ipc/                  # 进程通信模块
│   ├── bridge.ts         # Electron与Qt通信桥
│   ├── shared_memory.cpp # 共享内存实现
│   └── protocol.json     # 通信协议定义
├── cache/                # 缓存管理
│   ├── lru_cache.ts      # LRU缓存实现
│   ├── disk_cache.ts     # 磁盘缓存封装
│   └── policies/         # 缓存策略
│       ├── memory.ts     # 内存策略
│       └── hybrid.ts     # 混合策略
└── utils/                # 工具类集合
    ├── string_helper.ts  # 字符串处理工具
    ├── file_utils.ts     # 文件操作工具
    └── error_handler.ts  # 统一错误处理器
