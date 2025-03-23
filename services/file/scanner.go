package file

import (
	"context"
	"fmt"
	"log"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"time"

	config "FAM/global_config" // 配置文件

	"github.com/fsnotify/fsnotify"
)

// var workerPool = make(chan struct{}, 100) // 线程池
// 原固定100改为配置读取
var workerPool chan struct{}

func InitWorkerPool(cfg *config.Config) {
	workerPool = make(chan struct{}, cfg.WorkerPoolSize)
}

// // 支持的媒体格式列表
//
//	var mediaExts = map[string]bool{
//	    ".png":true, ".jpg":true, ".raw":true,
//	    ".mov":true, ".mp4":true, ".gif":true, ".avi":true,
//	}
//
// 原硬编码改为从配置加载
var mediaExts map[string]bool

func InitMediaExtensions(cfg *config.Config) {
	mediaExts = make(map[string]bool)
	for _, ext := range cfg.MediaExtensions {
		mediaExts[strings.ToLower(ext)] = true
	}
}

// 视频格式判断
var videoExts = map[string]bool{
	".mov": true, ".mp4": true, ".avi": true,
}

/*
MediaFile 表示一个媒体文件的元数据结构
字段说明:

	Path     文件完整路径
	Size     文件大小(字节)
	Modified 最后修改时间(Unix时间戳)
	IsVideo  是否为视频文件标识
*/
type MediaFile struct {
	Path     string
	Size     int64
	Modified int64
	IsVideo  bool
}

// 异常处理
func safeScan(path string) (result []MediaFile, err error) {
	defer func() {
		if r := recover(); r != nil { // 修复变量定义
			err = fmt.Errorf("scan panic: %v", r)
		}
	}()

	ctx, cancel := context.WithTimeout(context.Background(),
		time.Duration(config.ScanTimeout)*time.Second)
	defer cancel()

	resultChan := make(chan []MediaFile)
	go func() {
		res, err := scanDir(path)
		if err != nil {
			log.Printf("扫描目录 %s 失败: %v", path, err)
		}
		resultChan <- res
	}()

	select {
	case <-ctx.Done():
		return nil, ctx.Err()
	case res := <-resultChan:
		return res, nil
	}
}

//

/*
scanDir 扫描指定目录并收集媒体文件信息
参数:

	path - 要扫描的目录路径

返回值:

	[]MediaFile - 找到的媒体文件集合
	error      - 扫描过程中发生的错误（如果有）

实现说明:

	使用并发方式遍历目录树，通过文件扩展名过滤媒体文件
	使用sync.WaitGroup进行goroutine同步，sync.Mutex保证切片并发安全
*/
func scanDir(path string) ([]MediaFile, error) {
	var files []MediaFile
	var mu sync.Mutex
	var wg sync.WaitGroup

	// 遍历目录树时触发的回调函数
	err := filepath.Walk(path, func(p string, info os.FileInfo, err error) error {

		// 错误处理
		if err != nil {
			return fmt.Errorf("访问路径 %q 失败： %v", p, err)
		}
		// 目录故障排查
		if info.IsDir() {
			pClean := path.Clean(p) // 标准化当前路径  TODO
			// 检查是否在排除列表中
			if Config.excludedDirs[pClean] {
				return filepath.SkipDir // TODO
			}
			// 进一步检查父目录是否被排除（如需递归排除）
		}

		if !info.IsDir() && mediaExts[filepath.Ext(p)] {
			wg.Add(1)
			workerPool <- struct{}{}
			go func() {
				defer func() { <-workerPool }()
				defer wg.Done()

				// 并发处理每个匹配的媒体文件
				file := MediaFile{
					Path:     p,
					Size:     info.Size(),
					Modified: info.ModTime().Unix(),
					IsVideo:  isVideoExt(filepath.Ext(p)),
				}

				// 安全地将文件对象添加到切片
				mu.Lock()
				files = append(files, file)
				mu.Unlock()
			}()
		}
		return nil
	})

	wg.Wait()
	return files, err
}

/*
isVideoExt 检查文件扩展名是否为视频类型
参数:

	ext - 文件扩展名（包含点符号，如".mp4"）

返回值:

	bool - true表示为视频文件扩展名
*/
func isVideoExt(ext string) bool {
	_, ok := videoExts[ext]
	return ok
}

/*
initWatcher 初始化文件系统监控并与Qt组件集成
参数:

	qtWatcher - Qt文件监控组件的实例指针

实现说明:

	使用fsnotify创建文件监控器，持续监听文件系统事件
	当检测到媒体文件变更时，通过Qt组件通知上层应用
	监控过程在无限循环中持续运行，需配合goroutine使用
*/
func initWatcher(qtWatcher *QtFileWatcher) {
	watcher, _ := fsnotify.NewWatcher()
	if err := watcher.Add(config.MediaDir); err != nil {
		log.Printf("添加监控路径失败： %v", err)
	}
	defer watcher.Close()

	// 事件处理主循环
	for {
		select {
		case event := <-watcher.Events:
			// 仅处理媒体文件相关事件
			if mediaExts[filepath.Ext(event.Name)] {
				qtWatcher.Notify(event) // 将事件转发给Qt组件
			}
		case err := <-watcher.Errors:
			log.Println("watcher error:", err)
		}
	}
}
