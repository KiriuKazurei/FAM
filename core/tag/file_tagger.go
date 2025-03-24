package tag

import (
	"FAM/core/core_config"
	"fmt"
	"regexp"
	"time"

	"gorm.io/driver/sqlite"

	_ "gorm.io/driver/sqlite" // 确保 SQLite 驱动已导入
	"gorm.io/gorm"            // 确保已导入 GORM
)

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

// 标签存储模型（优化存储能力）
type TagEntry struct {
	ID        int       `gorm:"primaryKey"`
	FilePath  string    `gorm:"index;not null;type:text"`   // 强制使用TEXT类型
	Tag       string    `gorm:"type:varchar(255);not null"` // 增加到255字符
	Source    string    `gorm:"default:'manual'"`
	CreatedAt time.Time `gorm:"autoCreateTime"`
}

// 数据库连接单例
var db *gorm.DB

// 在file_tagger.go的init函数中修改为：
func init() {
	// 从配置加载数据库路径（需对接core/config模块）
	dbPath := core_config.Get("database.path", "tags.db") // 使用修正后的Get方法
	db, _ = gorm.Open(sqlite.Open(dbPath), &gorm.Config{})
	db.Exec("PRAGMA encoding = 'UTF-8';")
	db.AutoMigrate(&TagEntry{})
}

// AddTags 实现
func AddTags(filePath string, tags []string) (string, error) {
	tx := db.Begin()
	defer func() {
		if r := recover(); r != nil {
			tx.Rollback()
		}
	}()

	for _, tag := range tags {
		// 验证标签格式（可选）
		if !isValidTag(tag) {
			tx.Rollback()
			return "", fmt.Errorf("invalid tag format: %s", tag)
		}

		// 创建标签记录
		entry := TagEntry{
			FilePath: filePath,
			Tag:      tag,
			Source:   "manual",
		}
		if err := tx.Create(&entry).Error; err != nil {
			tx.Rollback()
			return "", err
		}
	}

	tx.Commit()
	return fmt.Sprintf("Added %d tags to %s", len(tags), filePath), nil
}

// DeleteTags 根据文件路径删除指定标签（或全部标签）
// 参数:
// - filePath: 需要删除标签的文件路径 (string)
// - tags: 需要删除的标签列表 ([]string)，留空则删除所有标签
// 返回值:
// - string: 操作结果信息
// - error: 错误信息（如果有的话）
func DeleteTags(filePath string, tags []string) (string, error) {
	var result *gorm.DB
	var err error

	// 根据参数选择删除条件
	if len(tags) > 0 {
		// 删除指定标签
		result = db.Where("file_path = ? AND tag IN (?)", filePath, tags).Delete(&TagEntry{})
	} else {
		// 删除所有标签（该文件的所有记录）
		result = db.Where("file_path = ?", filePath).Delete(&TagEntry{})
	}

	if err = result.Error; err != nil {
		return "", err
	}

	return fmt.Sprintf("Deleted %d tags for %s", result.RowsAffected, filePath), nil
}

// ListTags 根据文件路径获取所有标签
func ListTags(filePath string) ([]string, error) {
	var tags []string

	// 使用Pluck提取所有Tag字段
	if err := db.Model(&TagEntry{}).
		Where("file_path = ?", filePath).
		Pluck("Tag", &tags).
		Error; err != nil {
		return nil, fmt.Errorf("查询标签失败: %w", err)
	}

	return tags, nil
}

// 优化后的标签格式验证函数（支持中文及常用符号）
func isValidTag(tag string) bool {
	// 允许中文、字母、数字、空格及常用标点（如：、。！？）
	return len(tag) <= 64 &&
		regexp.MustCompile(`^[a-zA-Z0-9\u4e00-\u9fa5 ！？，。、]+$`).MatchString(tag)
}

//// 可选的标签净化处理（在存储前执行）
//func sanitizeTag(tag string) string {
//	// 移除连续空格并两端修剪
//	return strings.Join(
//		strings.FieldsFunc(tag, func(r rune) bool {
//			return !unicode.IsSpace(r)
//		}),
//		" ",
//	)
//}
