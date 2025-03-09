package tag

// AddTags 为指定的文件路径添加标签。
// 参数:
// - filePath: 图片文件的路径 (string 类型)。
// - tags: 需要添加的标签列表 ([]string 类型)。
// 返回值:
// - string: 操作成功的提示信息。
// - error: 如果发生错误，则返回非 nil 的 error 对象；否则返回 nil。
// 功能描述:
// 该函数目前是一个占位实现，实际的标签添加逻辑尚未实现。
// 在未来的开发中，可以扩展此函数以支持真实的标签写入功能（例如通过 EXIF 工具或数据库存储）。

// Tagger 接口定义标签操作契约
type Tagger interface {
	AddTags(filePath string, tags []string) (string, error)
	// 可扩展其他方法（如DeleteTags、ListTags）
	DeleteTags(filePath string, tags []string) (string, error)
	ListTags(filePath string) ([]string, error)
}
