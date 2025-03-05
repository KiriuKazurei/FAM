// config/config.go
package config

var (
    ScanTimeout = 30    // 默认扫描超时时间(秒)
    MediaDir    = "/监控目录" // 默认监控路径
)

type Config struct {
    // 已有配置
    ScanTimeout     int      `json:"scanTimeout"`     // 扫描超时时间(秒)
    MediaDir        string   `json:"mediaDir"`        // 监控目录路径
    
    // 新增配置项
    WorkerPoolSize  int      `json:"workerPoolSize"`  // 并发工作协程数
    MediaExtensions []string `json:"mediaExtensions"` // 支持的媒体格式
    VideoExtensions []string `json:"videoExtensions"` // 视频格式列表
    ExcludeDirs     []string `json:"excludeDirs"`     // 要排除的目录
    WatchInterval   int      `json:"watchInterval"`   // 文件监控轮询间隔(秒)
    DBPath          string   `json:"dbPath"`          // 数据库文件路径
}