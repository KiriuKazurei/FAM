package file

import (
	"log"
	"path/filepath"
	"strings"

	config "FAM/global_config"

	"github.com/fsnotify/fsnotify"
)

// FileWatcher 结构体用于文件监控
type FileWatcher struct {
	qtWatcher *QtFileWatcher // 假设 QtFileWatcher 是一个已经定义好的结构体
}

// NewFileWatcher 创建并初始化文件监控实例
// @param qtWatcher: Qt文件监控组件的实例引用，用于事件通知
// @return *FileWatcher: 初始化完成的文件监控器实例
func NewFileWatcher(qtWatcher *QtFileWatcher) *FileWatcher {
	return &FileWatcher{
		qtWatcher: qtWatcher,
	}
}

// Start 启动文件监控流程
// 初始化文件监视器并持续监听文件系统事件，当检测到媒体文件变动时通知Qt组件
// @return error: 返回初始化监视器或添加监控路径时可能产生的错误
func (fw *FileWatcher) Start() error {
	watcher, err := fsnotify.NewWatcher()
	if err != nil {
		return err
	}
	defer watcher.Close()

	// 添加配置文件中指定的媒体目录监控
	err = watcher.Add(config.MediaDir)
	if err != nil {
		log.Printf("添加监控路径失败: %v", err)
		return err
	}

	// 主事件处理循环：持续监听文件事件和错误通道
	for {
		select {
		case event, ok := <-watcher.Events:
			if !ok {
				return nil
			}
			// 过滤并处理媒体文件相关事件
			if isMediaFile(event.Name) {
				fw.qtWatcher.Notify(event) // 将有效事件转发给Qt界面组件
			}
		case err, ok := <-watcher.Errors:
			if !ok {
				return nil
			}
			log.Printf("watcher error: %v", err)
		}
	}
}

// isMediaFile 检查给定路径是否为支持的媒体文件类型
// @param path: 需要检测的完整文件路径
// @return bool: true表示是媒体文件，false表示不支持的类型
func isMediaFile(path string) bool {
	ext := filepath.Ext(path) // 保留原始扩展名变量
	configs, err := config.LoadAndInit()
	if err != nil {
		log.Fatal(err)
	}
	for _, extConfig := range configs.MediaExtensions {
		if strings.EqualFold(ext, extConfig) {
			return true
		}
	}
	return false
}
