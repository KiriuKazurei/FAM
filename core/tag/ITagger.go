package tag

// Tagger 接口定义标签操作契约
type Tagger interface {
	AddTags(filePath string, tags []string) (string, error)
	// 可扩展其他方法（如DeleteTags、ListTags）
	DeleteTags(filePath string, tags []string) (string, error)
	ListTags(filePath string) ([]string, error)
}
