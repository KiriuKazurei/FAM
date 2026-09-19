package file

import "C"
import (
	"fmt"
	"unsafe"

	"github.com/fsnotify/fsnotify"

	imageutil "FAM/core/imageutil"
)

type QtFileWatcher struct {
	eventChannel chan fsnotify.Event
	stopped      bool // 新增字段用于标记是否停止
	thumbnailGen *imageutil.ThumbnailGenerator
}

// 构造函数需要接收配置参数
func NewQtFileWatcher(bufferSize int, config *imageutil.ThumbnailConfig) *QtFileWatcher {
	gen := imageutil.NewThumbnailGenerator(config)
	return &QtFileWatcher{
		eventChannel: make(chan fsnotify.Event, bufferSize),
		stopped:      false,
		thumbnailGen: gen,
	}
}

// Notify 将事件转发给Qt组件
func (q *QtFileWatcher) Notify(event fsnotify.Event) {
	if q.stopped {
		return // 已停止则不处理事件
	}
	select {
	case q.eventChannel <- event:
	default:
		fmt.Printf("通道满，丢弃事件: %s\n", event.Name)
	}
}

// StartQtWatcher 启动事件处理
func (q *QtFileWatcher) StartQtWatcher() {
	for event := range q.eventChannel {
		q.HandleEvent(event)
	}
}

// HandleEvent 处理具体事件逻辑
func (q *QtFileWatcher) HandleEvent(event fsnotify.Event) {
	switch event.Op {
	case fsnotify.Create, fsnotify.Write:
		go q.processFile(event.Name)
	}
}

// processFile 触发缩略图生成和UI更新
func (q *QtFileWatcher) processFile(path string) {
	resultChan := q.thumbnailGen.GetThumbnailAsync(path)
	result := <-resultChan // 等待异步结果（需确保在goroutine中执行）
	if result.Err != nil {
		fmt.Printf("生成缩略图失败: %v\n", result.Err)
		return
	}
	q.updateUI(result.ThumbPath) // 传递缓存路径给UI
}

func (q *QtFileWatcher) generateThumbnail(path string) any {
	// 通过ThumbnailGenerator生成缩略图（假设已实现）
	thumbPath, err := q.thumbnailGen.Generate(path)
	if err != nil {
		fmt.Printf("生成缩略图失败: %v\n", err)
		return nil // 或返回错误对象
	}
	return thumbPath // 返回缩略图路径
}

//	func (q *QtFileWatcher) updateUI(thumbnail any) {
//		// 通过IPC或CGO通知Qt更新UI
//	}
func (q *QtFileWatcher) updateUI(thumbPath string) {
	cPath := C.CString(thumbPath)
	defer C.free(unsafe.Pointer(cPath))

	// 调用Qt的C接口更新UI
	C.update_ui_thumbnail(cPath)
}

// Stop 停止事件监听
func (q *QtFileWatcher) Stop() {
	q.stopped = true
	close(q.eventChannel)
}
