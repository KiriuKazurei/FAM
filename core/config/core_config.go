package core_config

import (
	"encoding/json"
	"io/ioutil"
	"log"
)

type Config struct {
	DatabasePath string `json:"database_path"` // 新增数据库路径字段
	FilesDir     string `json:"files_dir"`
	TagsDir      string `json:"tags_dir"`
	LogsDir      string `json:"logs_dir"`
	ConfigDir    string `json:"config_dir"`
	TemplatesDir string `json:"templates_dir"`
}

var configData Config

func init() {
	configFilePath := "config.json"
	data, err := ioutil.ReadFile(configFilePath)
	if err != nil {
		log.Fatalf("Failed to read config file: %v", err)
	}
	if err := json.Unmarshal(data, &configData); err != nil {
		log.Fatalf("Failed to parse config file: %v", err)
	}
}

func Get(path string, defaultValue string) string {
	switch path {
	case "database.path":
		return configData.DatabasePath // 从配置中读取
	case "files_dir":
		return configData.FilesDir
	case "tags_dir":
		return configData.TagsDir
	case "logs_dir":
		return configData.LogsDir
	case "config_dir":
		return configData.ConfigDir
	case "templates_dir":
		return configData.TemplatesDir
	default:
		return defaultValue // 使用默认值
	}
}
