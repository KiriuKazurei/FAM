package tag

import (
	"fmt"
	"gorm.io/driver/sqlite"
	"regexp"
	"time"

	"core/config"

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

// 标签存储模型（需在初始化时创建表结构）
type TagEntry struct {
	ID        int       `gorm:"primaryKey"`
	FilePath  string    `gorm:"index;not null"` // 文件路径
	Tag       string    `gorm:"not null"`       // 标签内容
	Source    string    `gorm:"default:'manual'"`
	CreatedAt time.Time `gorm:"autoCreateTime"`
}

// 数据库连接单例
var db *gorm.DB

func init() {
	// 从配置加载数据库路径（需对接core/config模块）
	dbPath := core_config.Get("database.path", "tags.db") // 调整为实际包名和方法
	db, _ = gorm.Open(sqlite.Open(dbPath), &gorm.Config{})
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

// 辅助函数：标签格式验证
func isValidTag(tag string) bool {
	// 示例规则：不允许特殊字符且长度不超过32字符
	return len(tag) <= 32 && !regexp.MustCompile(`[^a-zA-Z0-9_\- ]`).MatchString(tag)
}
