package tag

import (
	"database/sql"
	"fmt"
	"log"

	_ "github.com/mattn/go-sqlite3" // SQLite驱动
)

// Tag 定义标签结构
type Tag struct {
	ID       int    `json:"id"`
	ImageID  int    `json:"image_id"`
	TagName  string `json:"tag_name"`
	Priority int    `json:"priority"`
}

// Storage 提供标签存储接口
type Storage struct {
	db *sql.DB
}

// NewStorage 创建新的标签存储实例
func NewStorage(dbPath string) (*Storage, error) {
	db, err := sql.Open("sqlite3", dbPath)
	if err != nil {
		return nil, err
	}

	// 创建标签表
	createTableSQL := `
	CREATE TABLE IF NOT EXISTS tags (
		id INTEGER PRIMARY KEY AUTOINCREMENT,
		image_id INTEGER NOT NULL,
		tag_name TEXT NOT NULL,
		priority INTEGER NOT NULL,
		FOREIGN KEY (image_id) REFERENCES images(id)
	);
	`
	_, err = db.Exec(createTableSQL)
	if err != nil {
		db.Close()
		return nil, err
	}

	return &Storage{db: db}, nil
}

// Close 关闭数据库连接
func (s *Storage) Close() error {
	return s.db.Close()
}

// AddTag 添加新标签
func (s *Storage) AddTag(tag Tag) (int64, error) {
	result, err := s.db.Exec(
		"INSERT INTO tags (image_id, tag_name, priority) VALUES (?, ?, ?)",
		tag.ImageID, tag.TagName, tag.Priority,
	)
	if err != nil {
		return 0, err
	}
	return result.LastInsertId()
}

// GetTagsByImageID 获取指定图片的所有标签
func (s *Storage) GetTagsByImageID(imageID int) ([]Tag, error) {
	rows, err := s.db.Query("SELECT id, image_id, tag_name, priority FROM tags WHERE image_id = ?", imageID)
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var tags []Tag
	for rows.Next() {
		var tag Tag
		err := rows.Scan(&tag.ID, &tag.ImageID, &tag.TagName, &tag.Priority)
		if err != nil {
			return nil, err
		}
		tags = append(tags, tag)
	}

	return tags, nil
}

// UpdateTag 更新标签信息
func (s *Storage) UpdateTag(tag Tag) error {
	_, err := s.db.Exec(
		"UPDATE tags SET tag_name = ?, priority = ? WHERE id = ?",
		tag.TagName, tag.Priority, tag.ID,
	)
	return err
}

// DeleteTag 删除标签
func (s *Storage) DeleteTag(tagID int) error {
	_, err := s.db.Exec("DELETE FROM tags WHERE id = ?", tagID)
	return err
}

// GetAllTags 获取所有标签
func (s *Storage) GetAllTags() ([]Tag, error) {
	rows, err := s.db.Query("SELECT id, image_id, tag_name, priority FROM tags")
	if err != nil {
		return nil, err
	}
	defer rows.Close()

	var tags []Tag
	for rows.Next() {
		var tag Tag
		err := rows.Scan(&tag.ID, &tag.ImageID, &tag.TagName, &tag.Priority)
		if err != nil {
			return nil, err
		}
		tags = append(tags, tag)
	}

	return tags, nil
}

// 示例用法
func main() {
	storage, err := NewStorage("tags.db")
	if err != nil {
		log.Fatalf("无法创建存储实例: %v", err)
	}
	defer storage.Close()

	// 添加标签
	newTag := Tag{
		ImageID:  1,
		TagName:  "风景",
		Priority: 1,
	}
	id, err := storage.AddTag(newTag)
	if err != nil {
		log.Fatalf("添加标签失败: %v", err)
	}
	fmt.Printf("新标签ID: %d\n", id)

	// 获取标签
	tags, err := storage.GetTagsByImageID(1)
	if err != nil {
		log.Fatalf("获取标签失败: %v", err)
	}
	fmt.Printf("标签: %+v\n", tags)
}