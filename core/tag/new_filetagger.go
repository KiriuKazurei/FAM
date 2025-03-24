package tag

import (
	"FAM/core/core_config"
	"fmt"
	"path/filepath"
	"regexp"
	"strings"
	"time"

	"gorm.io/driver/sqlite"
	"gorm.io/gorm"
)

// TagEntry 存储标签的结构体，包含文件路径、标签内容、来源和时间信息
type TagEntry struct {
	ID        int       `gorm:"primaryKey"`
	FilePath  string    `gorm:"index;not null;type:text"`   // 文件路径，使用索引加速查询
	Tag       string    `gorm:"type:varchar(255);not null"` // 标签内容，最大长度255字符
	Source    string    `gorm:"default:'manual'"`           // 标签来源（手动添加/自动/AI等）
	CreatedAt time.Time `gorm:"autoCreateTime"`             // 自动记录创建时间
}

// 数据库连接单例
var db *gorm.DB

// 初始化数据库连接
func init() {
	dbPath := core_config.Get("database.path", "tags.db") // TODO: 要指向正确地址
	var err error
	db, err = gorm.Open(sqlite.Open(dbPath), &gorm.Config{})
	if err != nil {
		panic("failed to connect database")
	}
	db.Exec("PRAGMA encoding = 'UTF-8';")
	db.AutoMigrate(&TagEntry{})
}

// AddTags 添加标签到指定文件
// 参数:
//   - filePath: 文件路径
//   - tags: 标签列表
//   - source: 标签来源（如 "manual", "AI"）
//
// 返回:
//
//	成功提示和错误信息
//
// 说明:
//
//	支持指定标签来源，便于区分人工添加和AI生成的标签
func AddTags(filePath string, tags []string, source string) (string, error) {
	tx := db.Begin()
	defer func() {
		if r := recover(); r != nil {
			tx.Rollback()
		}
	}()

	for _, tag := range tags {
		// 验证标签格式（如包含中文、字母、数字及常用符号）
		if !isValidTag(tag) {
			tx.Rollback()
			return "", fmt.Errorf("无效标签格式: %s", tag)
		}

		// 创建标签记录，指定来源
		entry := TagEntry{
			FilePath: filePath,
			Tag:      tag,
			Source:   source, // 新增参数，支持来源区分
		}
		if err := tx.Create(&entry).Error; err != nil {
			tx.Rollback()
			return "", err
		}
	}

	tx.Commit()
	return fmt.Sprintf("为 %s 添加了 %d 个标签", filePath, len(tags)), nil
}

// DeleteTags 删除指定文件的标签
// 参数:
//   - filePath: 文件路径
//   - tags: 需要删除的标签列表（空则删除全部）
//   - source: 限制删除特定来源的标签（空则不限）
//
// 返回:
//
//	删除结果和错误信息
//
// 说明:
//
//	支持按来源过滤删除，例如删除AI生成的标签
func DeleteTags(filePath string, tags []string, source string) (string, error) {
	var result *gorm.DB
	var err error

	// 构建查询条件
	query := db.Where("file_path = ?", filePath)
	if len(tags) > 0 {
		query = query.Where("tag IN (?)", tags)
	}
	if source != "" {
		query = query.Where("source = ?", source)
	}

	// 执行删除
	result = query.Delete(&TagEntry{})
	if err = result.Error; err != nil {
		return "", err
	}

	return fmt.Sprintf("从 %s 删除了 %d 个标签", filePath, result.RowsAffected), nil
}

// ListTags 获取指定文件的标签列表
// 参数:
//   - filePath: 文件路径
//   - source: 限制显示特定来源的标签（空则全部）
//
// 返回:
//
//	标签列表和错误信息
//
// 说明:
//
//	支持按来源过滤，例如仅查看人工添加的标签
func ListTags(filePath string, source string) ([]string, error) {
	var tags []string

	// 构建查询条件
	query := db.Model(&TagEntry{}).Where("file_path = ?", filePath)
	if source != "" {
		query = query.Where("source = ?", source)
	}

	// 提取标签内容
	if err := query.Pluck("Tag", &tags).Error; err != nil {
		return nil, fmt.Errorf("查询失败: %w", err)
	}

	return tags, nil
}

// UpdateTag 更新指定文件的标签属性
// 参数:
//   - filePath: 文件路径
//   - oldTag: 需要更新的旧标签名
//   - newTag: 新标签名（空则不修改）
//   - newSource: 新来源（空则不修改）
//
// 返回:
//
//	更新结果和错误信息
//
// 说明:
//
//	支持修改标签名称或来源，例如将标签来源从"manual"改为"AI"
func UpdateTag(filePath string, oldTag string, newTag string, newSource string) (string, error) {
	tx := db.Begin()
	defer func() {
		if r := recover(); r != nil {
			tx.Rollback()
		}
	}()

	// 构建更新条件
	updates := make(map[string]interface{})
	if newTag != "" {
		updates["Tag"] = newTag
	}
	if newSource != "" {
		updates["Source"] = newSource
	}

	// 执行更新
	result := tx.Model(&TagEntry{}).
		Where("file_path = ? AND Tag = ?", filePath, oldTag).
		Updates(updates)

	if err := result.Error; err != nil {
		tx.Rollback()
		return "", err
	}

	if result.RowsAffected == 0 {
		tx.Rollback()
		return "", fmt.Errorf("未找到标签 %s", oldTag)
	}

	tx.Commit()
	return fmt.Sprintf("更新 %s 的 %d 个标签", filePath, result.RowsAffected), nil
}

// SearchTags 按标签和来源搜索文件路径
// 参数:
//   - tag: 需要搜索的标签
//   - source: 限制来源（空则不限）
//   - searchDir: 搜索的目录路径（空则使用默认）
//
// 返回:
//
//	文件路径列表和错误信息
//
// 说明:
//
//	支持按标签和来源搜索文件，例如查找AI生成的特定标签文件
func SearchTags(tag string, source string, searchDir string) ([]string, error) {
	if searchDir == "" {
		searchDir = core_config.Get("files_dir", "./")
	}

	// 构建查询条件
	query := db.Where("tag = ?", tag)
	if source != "" {
		query = query.Where("source = ?", source)
	}

	var entries []TagEntry
	if err := query.Find(&entries).Error; err != nil {
		return nil, err
	}

	// 筛选属于指定目录的文件
	cleanSearchDir := filepath.Clean(searchDir)
	cleanSearchDirWithSlash := filepath.Join(cleanSearchDir, "")

	var fileList []string
	for _, entry := range entries {
		filePath := entry.FilePath
		cleanPath := filepath.Clean(filePath)

		// 检查文件路径是否在 searchDir 下
		if strings.HasPrefix(cleanPath, cleanSearchDirWithSlash) {
			fileList = append(fileList, filePath)
		}
	}

	return fileList, nil
}

// isValidTag 验证标签格式
// 允许中文、字母、数字及常用符号（如：、。！？）
func isValidTag(tag string) bool {
	return len(tag) <= 64 &&
		regexp.MustCompile(`^[a-zA-Z0-9\u4e00-\u9fa5 ！？，。、]+$`).MatchString(tag)
}
