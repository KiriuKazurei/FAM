package file

import (
	"fmt"

	"github.com/fsnotify/fsnotify"
)

type QtFileWatcher struct {
	eventChannel chan fsnotify.Event
	stopped      bool // 新增字段用于标记是否停止
}

func NewQtFileWatcher(bufferSize int) *QtFileWatcher {
	return &QtFileWatcher{
		eventChannel: make(chan fsnotify.Event, bufferSize),
		stopped:      false,
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
	// 实际实现需替换以下代码
	thumbnail := q.generateThumbnail(path)
	q.updateUI(thumbnail)
}

func (q *QtFileWatcher) generateThumbnail(path string) any {
	// 实现图像处理逻辑（如调用CGO）
	return nil // 替换为实际返回值
}

func (q *QtFileWatcher) updateUI(thumbnail any) {
	// 通过IPC或CGO通知Qt更新UI
}

// Stop 停止事件监听
func (q *QtFileWatcher) Stop() {
	q.stopped = true
	close(q.eventChannel)
}
