package core_config

import (
	"encoding/json"
	"io/ioutil"
	"log"
)

type Config struct {
	// The path to the directory where the files are stored
	FilesDir string `json:"files_dir"`
	// The path to the directory where the tags are stored
	TagsDir string `json:"tags_dir"`
	// The path to the directory where the logs are stored
	LogsDir string `json:"logs_dir"`
	// The path to the directory where the config is stored
	ConfigDir string `json:"config_dir"`
	// The path to the directory where the templates are stored
	TemplatesDir string `json:"templates_dir"`
}

var configData Config

func init() {
	// 假设配置文件路径为 "config.json"
	configFilePath := "config.json"
	data, err := ioutil.ReadFile(configFilePath)
	if err != nil {
		log.Fatalf("Failed to read config file: %v", err)
	}
	if err := json.Unmarshal(data, &configData); err != nil {
		log.Fatalf("Failed to parse config file: %v", err)
	}
}

func Get(path string, databasePath string) string {
	switch path {
	case "database.path":
		return databasePath
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
		return ""
	}
}