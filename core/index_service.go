// core/index_service.go
package core

import (
    "database/sql"
    "log"
    "time"

    _ "github.com/mattn/go-sqlite3"
)

// IndexService 提供文件索引的数据库管理服务
// 用于维护文件元信息的持久化存储和查询
type IndexService struct {
    db *sql.DB
}

// NewIndexService 创建并初始化索引服务实例
// 参数:
//   dbPath - SQLite数据库文件路径
// 返回值:
//   *IndexService - 初始化后的服务实例
//   error - 数据库连接或建表失败时返回错误
func NewIndexService(dbPath string) (*IndexService, error) {
    db, err := sql.Open("sqlite3", dbPath)
    if err != nil {
        return nil, err
    }

    // 创建文件索引表
    // 初始化数据库表结构，包含文件路径、大小、修改时间和媒体类型标记
    // 使用UNIQUE约束确保文件路径唯一性，AUTOINCREMENT实现自增主键
    createTableStmt := `
        CREATE TABLE IF NOT EXISTS files (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            size INTEGER NOT NULL,
            modified INTEGER NOT NULL,
            is_video INTEGER NOT NULL
        );
    `
    _, err = db.Exec(createTableStmt)
    if err != nil {
        db.Close()
        return nil, err
    }

    return &IndexService{db: db}, nil
}

// AddFile 添加/更新文件记录到索引数据库
// 参数:
//   file - 要添加的媒体文件对象，包含路径、大小、修改时间和视频类型标记
// 返回值:
//   error - 数据库操作失败时返回错误
func (s *IndexService) AddFile(file MediaFile) error {
    stmt := `
        INSERT INTO files (path, size, modified, is_video)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(path) DO UPDATE SET
        size=excluded.size,
        modified=excluded.modified,
        is_video=excluded.is_video;
    `
    // 使用UPSERT操作保证路径唯一性，冲突时自动更新字段
    _, err := s.db.Exec(stmt, file.Path, file.Size, file.Modified, boolToInt(file.IsVideo))
    return err
}

// Close 关闭数据库连接
// 返回值:
//   error - 数据库关闭操作失败时返回错误
func (s *IndexService) Close() error {
    return s.db.Close()
}

// boolToInt 将布尔值转换为SQLite支持的整型值
// 参数:
//   b - 要转换的布尔值
// 返回值:
//   int - true返回1，false返回0
func boolToInt(b bool) int {
    if b {
        return 1
    }
    return 0
}